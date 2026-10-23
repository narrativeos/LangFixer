//! 知识源资源包 API（整理盘点 / 打包导出 / 导入 / 删除）
//!
//! 一个知识源（如内置示例源 `src-sample-en`）= 一个资源包单元，
//! 组件 = 知识数据（注册表条目 + 知识点 + 未发布草稿）+ 归因规则
//! （决策表中 `_meta` 标记该源的规则行）+ 决策数据
//! （`decision-tables/{lang}_points.json` / `{lang}_point_index.json`）。
//!
//! - `GET  /gec/knowledge/dataset/:source_id` — 盘点（点数/规则数/aux/staging 清单；
//!   删除前预览）
//! - `POST /gec/knowledge/dataset/:source_id/export` — 打包导出（`?format=zip` 返回
//!   zip 压缩包附件（单条目 pack.json，deflate），浏览器直接下载保存；
//!   缺省返回资源包 JSON）
//! - `POST /gec/knowledge/dataset/import` — 导入资源包（请求体自适应：zip 压缩包或
//!   裸 JSON；校验通过后：知识库热更新 + 决策表按 `_meta` 合并 + aux 文件写回 +
//!   staging 草稿恢复；token 鉴权）
//! - `POST /gec/knowledge/dataset/:source_id/clean` — 删除资源包（知识库 +
//!   决策表该源规则行 + aux 文件条目级摘除该源条目（摘空删文件）+ staging；
//!   token 鉴权；`?keep_orphan_points=1` 保留失去全部背书的点）
//!
//! 鉴权与知识库 PUT 共用 `GEC_TABLES_UPDATE_TOKEN`（`X-Tables-Update-Token`）。
//! 与导入管线（`knowledge_ingest`）的 IP 守卫不同：资源包导入/删除是**显式的
//! 管理员级资源单元生命周期操作**，按设计会写回 `decision-tables/`（原子写 +
//! 决策表版本历史留痕，可回滚）；导入管线仍绝不触碰决策表。

use std::path::PathBuf;
use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::Value;

use attribution_engine::AttributionEngine;
use attribution_engine::dataset::{
    self, PackCleanReport, PackDecisionTable, PackImportReport, PackSummary,
};

use crate::routes::ApiError;
use crate::state::AppState;

use super::knowledge::{check_token, persist_knowledge};

// ---------------------------------------------------------------------------
// 辅助：路径 / 文件原子写 / 快照收集
// ---------------------------------------------------------------------------

/// 某语言的 aux 数据文件路径（源原始提取数据，非 JDM 表、运行时不加载）
fn aux_points_path(state: &AppState, lang: &str) -> PathBuf {
    state
        .tables_dir
        .read()
        .unwrap()
        .join(format!("{lang}_points.json"))
}

fn aux_index_path(state: &AppState, lang: &str) -> PathBuf {
    state
        .tables_dir
        .read()
        .unwrap()
        .join(format!("{lang}_point_index.json"))
}

/// 决策表文件路径（与 TableStore::load_from_dir 命名约定一致）
pub(crate) fn table_file_path(state: &AppState, lang: &str) -> PathBuf {
    state
        .tables_dir
        .read()
        .unwrap()
        .join(format!("{lang}_gec_attribution.json"))
}

/// 原子写（临时文件 + rename；崩溃不留半截文件；临时名带随机后缀，
/// 并发写同一目标不互相踩临时文件）
pub(crate) fn atomic_write(path: &PathBuf, content: &str) -> Result<(), ApiError> {
    let tmp = path.with_extension(format!("json.tmp.{}", uuid::Uuid::new_v4().simple()));
    std::fs::write(&tmp, content)
        .map_err(|e| ApiError::Internal(format!("写入失败 {}: {e}", tmp.display())))?;
    std::fs::rename(&tmp, path)
        .map_err(|e| ApiError::Internal(format!("替换失败 {}: {e}", path.display())))?;
    Ok(())
}

/// 读取 JSON 文件（不存在 = None；解析失败报错）
fn read_json(path: &PathBuf) -> Result<Option<Value>, ApiError> {
    if !path.exists() {
        return Ok(None);
    }
    let content =
        std::fs::read_to_string(path).map_err(|e| ApiError::Internal(format!("读取失败: {e}")))?;
    serde_json::from_str(&content)
        .map(Some)
        .map_err(|e| ApiError::Internal(format!("JSON 解析失败 {}: {e}", path.display())))
}

/// 全部决策表快照（lang → JDM Value；解析失败的表跳过）
fn tables_snapshot(state: &AppState) -> Vec<(String, Value)> {
    state
        .tables
        .list()
        .into_iter()
        .filter_map(|meta| {
            let raw = state.tables.get_raw(&meta.lang)?;
            serde_json::from_str(&raw).ok().map(|jdm| (meta.lang, jdm))
        })
        .collect()
}

/// 源注册表条目语言（不存在报错）
fn registry_source(state: &AppState, source_id: &str) -> Result<String, ApiError> {
    state
        .knowledge
        .sources()
        .into_iter()
        .find(|s| s.source_id == source_id)
        .map(|s| s.lang)
        .ok_or_else(|| ApiError::NotFound(format!("知识源不存在: {source_id}")))
}

// ---------------------------------------------------------------------------
// GET /gec/knowledge/dataset/:source_id — 盘点
// ---------------------------------------------------------------------------

pub async fn summary(
    State(state): State<Arc<AppState>>,
    Path(source_id): Path<String>,
) -> Result<Json<PackSummary>, ApiError> {
    let lang = registry_source(&state, &source_id)?;
    let staging = state.ingest.session(&source_id);
    let tables = tables_snapshot(&state);
    let aux = (
        aux_points_path(&state, &lang).exists(),
        aux_index_path(&state, &lang).exists(),
    );
    let summary = dataset::summarize(&state.knowledge, staging.as_ref(), &tables, aux, &source_id)
        .map_err(ApiError::BadRequest)?;
    Ok(Json(summary))
}

// ---------------------------------------------------------------------------
// POST /gec/knowledge/dataset/:source_id/export — 打包导出
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ExportQuery {
    /// "zip" = 返回 zip 压缩包（application/zip 附件下载，单条目 pack.json，
    /// deflate 压缩）；缺省 = 返回资源包 JSON
    #[serde(default)]
    pub format: Option<String>,
}

pub async fn export(
    State(state): State<Arc<AppState>>,
    Path(source_id): Path<String>,
    Query(q): Query<ExportQuery>,
) -> Result<Response, ApiError> {
    let lang = registry_source(&state, &source_id)?;
    let staging = state.ingest.session(&source_id);
    let mut pack = dataset::export_pack(&state.knowledge, staging.as_ref(), &source_id)
        .map_err(ApiError::BadRequest)?;

    // 决策表组件：凡存在该源标记行的语言都携带整表快照（与 clean 的跨语言
    // 摘除范围对称，保证「导出 → 删除 → 导入」完整往返）
    for (table_lang, jdm) in tables_snapshot(&state) {
        if dataset::tagged_rule_count(&jdm, &source_id) > 0 {
            pack.components.decision_tables.push(PackDecisionTable {
                lang: table_lang,
                jdm,
            });
        }
    }
    // aux 组件（源原始提取数据）：条目级按源过滤（entry.source_id 归属；
    // 同语言多源时其他源条目不混入包，旧格式整文件判给本源并注入标记）
    for (path, slot) in [
        (
            aux_points_path(&state, &lang),
            &mut pack.components.aux_points,
        ),
        (
            aux_index_path(&state, &lang),
            &mut pack.components.aux_index,
        ),
    ] {
        if let Some(aux) = read_json(&path)? {
            let (owned, _) = dataset::aux_split_source(&aux, &source_id);
            if !dataset::aux_is_empty(&owned) {
                *slot = Some(owned);
            }
        }
    }
    // 组件齐备后重算校验和
    pack.checksum = dataset::pack_checksum(&pack.source, &pack.components);
    tracing::info!(
        source_id = %source_id,
        points = pack.components.knowledge_points.len(),
        tables = pack.components.decision_tables.len(),
        "资源包已导出"
    );
    if q.format.as_deref() == Some("zip") {
        let zip = dataset::pack_to_zip(&pack).map_err(ApiError::Internal)?;
        let filename = format!("{source_id}-resource-pack.zip");
        let response = Response::builder()
            .header(CONTENT_TYPE, "application/zip")
            .header(
                CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            )
            .body(axum::body::Body::from(zip))
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        return Ok(response);
    }
    Ok(Json(pack).into_response())
}

// ---------------------------------------------------------------------------
// POST /gec/knowledge/dataset/import — 导入资源包
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
pub struct PackImportResponse {
    pub knowledge: PackImportReport,
    /// 决策表：是否新装了整表（此前该语言无表）
    pub table_installed: bool,
    /// 合并进决策表的该源规则行（替换 / 新增）
    pub rules_replaced: usize,
    pub rules_added: usize,
    /// aux 数据文件写入数（0-2）
    pub aux_written: usize,
    /// staging 草稿会话是否已恢复（既有同 ID 会话时不覆盖）
    pub staging_restored: bool,
}

pub async fn import(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<PackImportResponse>, ApiError> {
    check_token(&state, &headers)?;
    // 请求体自适应：zip 压缩包（导出文件原样上传）或裸资源包 JSON
    let pack = dataset::pack_from_bytes(&body).map_err(ApiError::BadRequest)?;
    dataset::validate_pack(&pack).map_err(ApiError::BadRequest)?;
    let source_id = pack.source.source_id.clone();

    // ⓪ 预检：全部决策表合并结果先算出并通过引擎校验，再开始任何落盘——
    //   杜绝「知识库已写入、表更新中途失败」的半应用状态
    struct TableUpdate {
        lang: String,
        json: String,
        installed: bool,
        replaced: usize,
        added: usize,
    }
    let mut updates: Vec<TableUpdate> = Vec::new();
    for dt in &pack.components.decision_tables {
        let (json, installed, replaced, added) = match state.tables.get_raw(&dt.lang) {
            None => (
                serde_json::to_string(&dt.jdm).map_err(|e| ApiError::Internal(e.to_string()))?,
                true,
                0,
                0,
            ),
            Some(raw) => {
                let mut jdm: Value = serde_json::from_str(&raw)
                    .map_err(|e| ApiError::Internal(format!("当前决策表解析失败: {e}")))?;
                let (r, a) =
                    dataset::merge_tagged_rows(&mut jdm, &dataset::table_rows(&dt.jdm), &source_id);
                (
                    serde_json::to_string(&jdm).map_err(|e| ApiError::Internal(e.to_string()))?,
                    false,
                    r,
                    a,
                )
            }
        };
        AttributionEngine::validate(&json).map_err(|e| {
            ApiError::BadRequest(format!(
                "决策表合并结果校验未通过（{}），整体拒收: {e}",
                dt.lang
            ))
        })?;
        updates.push(TableUpdate {
            lang: dt.lang.clone(),
            json,
            installed,
            replaced,
            added,
        });
    }

    // ① 知识数据：注册表 upsert + 知识点幂等归并（既有点只追加背书不覆盖内容）
    let knowledge = dataset::import_pack(&state.knowledge, &pack).map_err(ApiError::BadRequest)?;
    persist_knowledge(&state);

    // ② 归因规则：应用预检通过的合并结果（该语言无表则装整表；有表则按 `_id`
    //   替换/插入该源标记行；TableStore.update 校验 + 原子替换；版本留痕；
    //   写回文件跨重启存活）
    let mut table_installed = false;
    let mut rules_replaced = 0usize;
    let mut rules_added = 0usize;
    for u in updates {
        let meta = state
            .tables
            .update(&u.lang, &u.json)
            .map_err(|e| ApiError::BadRequest(e.to_string()))?;
        if let Err(e) = state.table_history.save(&u.lang, meta.rule_count, &u.json) {
            tracing::warn!(lang = %u.lang, error = %e, "决策表版本存档失败（不影响导入）");
        }
        atomic_write(&table_file_path(&state, &u.lang), &u.json)?;
        table_installed |= u.installed;
        rules_replaced += u.replaced;
        rules_added += u.added;
    }

    // ③ 决策数据（aux 文件）：条目级合并——该源条目以资源包为权威，
    //   其他源条目原样保留（同语言多源不互相覆盖）
    let mut aux_written = 0usize;
    let lang = pack.source.lang.clone();
    for (value, path) in [
        (&pack.components.aux_points, aux_points_path(&state, &lang)),
        (&pack.components.aux_index, aux_index_path(&state, &lang)),
    ] {
        if let Some(v) = value {
            let merged = match read_json(&path)? {
                Some(current) => dataset::aux_merge_source(&current, v, &source_id),
                None => v.clone(),
            };
            let json = serde_json::to_string_pretty(&merged)
                .map_err(|e| ApiError::Internal(e.to_string()))?;
            atomic_write(&path, &json)?;
            aux_written += 1;
        }
    }

    // ④ staging 草稿会话：仅当无同 ID 会话时恢复（不打断进行中的挖掘；
    //   检查 + 插入同锁完成，杜绝与并发 ingest 启动的 TOCTOU）
    let mut staging_restored = false;
    if let Some(session) = &pack.components.staging {
        if state.ingest.put_session_if_absent(session.clone()) {
            let dir = state.ingest_dir.clone();
            if let Err(e) = std::fs::create_dir_all(&dir) {
                tracing::warn!(error = %e, "staging 目录创建失败（草稿仅内存态）");
            } else {
                let path = dir.join(format!("{source_id}.json"));
                let json = serde_json::to_string_pretty(session)
                    .map_err(|e| ApiError::Internal(e.to_string()))?;
                atomic_write(&PathBuf::from(&path), &json)?;
            }
            staging_restored = true;
        }
    }

    tracing::info!(
        source_id = %source_id,
        endorsed = knowledge.points_endorsed,
        created = knowledge.points_created,
        rules_replaced,
        rules_added,
        aux_written,
        "资源包已导入（知识库 + 决策表 + 决策数据）"
    );
    Ok(Json(PackImportResponse {
        knowledge,
        table_installed,
        rules_replaced,
        rules_added,
        aux_written,
        staging_restored,
    }))
}

// ---------------------------------------------------------------------------
// POST /gec/knowledge/dataset/:source_id/clean — 删除资源包
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct CleanQuery {
    /// 1/true/yes = 保留失去全部背书的孤儿知识点（默认删除）
    #[serde(default)]
    pub keep_orphan_points: Option<String>,
}

#[derive(serde::Serialize)]
pub struct PackCleanResponse {
    pub knowledge: PackCleanReport,
    /// 从决策表摘除的该源规则行数（跨语言合计）
    pub rules_removed: usize,
    /// aux 数据文件清理数（0-2：条目级摘除该源条目，摘空后删除文件）
    pub aux_removed: usize,
    /// staging 草稿会话是否已删除
    pub staging_removed: bool,
}

fn truthy(v: &Option<String>) -> bool {
    matches!(
        v.as_deref().map(|s| s.to_ascii_lowercase()).as_deref(),
        Some("1") | Some("true") | Some("yes")
    )
}

pub async fn clean(
    State(state): State<Arc<AppState>>,
    Path(source_id): Path<String>,
    Query(q): Query<CleanQuery>,
    headers: HeaderMap,
) -> Result<Json<PackCleanResponse>, ApiError> {
    check_token(&state, &headers)?;
    let lang = registry_source(&state, &source_id)?;

    // ⓪ 预检：各决策表摘除结果先算出并通过引擎校验，再开始任何落盘——
    //   杜绝「知识点已删、表更新中途失败」的半应用状态
    let mut table_updates: Vec<(String, String, usize)> = Vec::new(); // (lang, json, removed)
    for table in state.tables.list() {
        let Some(raw) = state.tables.get_raw(&table.lang) else {
            continue;
        };
        let mut jdm: Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let removed = dataset::remove_tagged_rows(&mut jdm, &source_id);
        if removed == 0 {
            continue;
        }
        let json = serde_json::to_string(&jdm).map_err(|e| ApiError::Internal(e.to_string()))?;
        AttributionEngine::validate(&json).map_err(|e| {
            ApiError::BadRequest(format!(
                "决策表摘除结果校验未通过（{}），中止删除: {e}",
                table.lang
            ))
        })?;
        table_updates.push((table.lang, json, removed));
    }

    // ① 知识数据：注册表条目 + 背书引用 + 孤儿点
    let knowledge =
        dataset::clean_pack(&state.knowledge, &source_id, truthy(&q.keep_orphan_points))
            .map_err(ApiError::BadRequest)?;
    persist_knowledge(&state);

    // ② 归因规则：应用预检通过的摘除结果（通用行保留；TableStore.update 校验 +
    //   版本留痕 + 写回文件跨重启存活）
    let mut rules_removed = 0usize;
    for (lang_t, json, removed) in table_updates {
        let meta = state
            .tables
            .update(&lang_t, &json)
            .map_err(|e| ApiError::BadRequest(e.to_string()))?;
        if let Err(e) = state.table_history.save(&lang_t, meta.rule_count, &json) {
            tracing::warn!(lang = %lang_t, error = %e, "决策表版本存档失败（不影响删除）");
        }
        atomic_write(&table_file_path(&state, &lang_t), &json)?;
        rules_removed += removed;
    }

    // ③ 决策数据（aux 文件）：条目级摘除该源条目（entry.source_id 归属；
    //   同语言其他源条目保留）；摘空后删除文件
    let mut aux_removed = 0usize;
    for path in [
        aux_points_path(&state, &lang),
        aux_index_path(&state, &lang),
    ] {
        if !path.exists() {
            continue;
        }
        let aux = read_json(&path)?.unwrap_or_else(|| Value::Object(Default::default()));
        let (_, rest) = dataset::aux_split_source(&aux, &source_id);
        if dataset::aux_is_empty(&rest) {
            std::fs::remove_file(&path)
                .map_err(|e| ApiError::Internal(format!("删除失败 {}: {e}", path.display())))?;
        } else {
            let json = serde_json::to_string_pretty(&rest)
                .map_err(|e| ApiError::Internal(e.to_string()))?;
            atomic_write(&path, &json)?;
        }
        aux_removed += 1;
    }

    // ④ staging 草稿会话：内存 + 持久化文件
    let staging_removed = state.ingest.discard(&source_id).is_ok();
    let staged_file = state.ingest_dir.join(format!("{source_id}.json"));
    if staged_file.exists() {
        let _ = std::fs::remove_file(&staged_file);
    }

    tracing::info!(
        source_id = %source_id,
        points_removed = knowledge.points_removed,
        refs_detached = knowledge.refs_detached,
        rules_removed,
        aux_removed,
        "资源包已删除（知识库 + 决策表 + 决策数据 + 草稿）"
    );
    Ok(Json(PackCleanResponse {
        knowledge,
        rules_removed,
        aux_removed,
        staging_removed,
    }))
}
