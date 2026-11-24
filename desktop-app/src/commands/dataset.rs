use std::path::PathBuf;
use std::sync::Arc;

use super::core::Core;
use super::util::{pack_atomic_write, tables_dir_of};
/// 资源包盘点（删除前预览）
#[tauri::command]
pub fn dataset_summary(
    core: tauri::State<'_, Arc<Core>>,
    source_id: String,
) -> Result<attribution_engine::PackSummary, String> {
    let src = core
        .knowledge
        .sources()
        .into_iter()
        .find(|s| s.source_id == source_id)
        .ok_or_else(|| format!("知识源不存在: {source_id}"))?;
    let staging = core.ingest.session(&source_id);
    let tables: Vec<(String, serde_json::Value)> = core
        .tables
        .list()
        .into_iter()
        .filter_map(|m| {
            core.tables
                .get_raw(&m.lang)
                .and_then(|raw| serde_json::from_str(&raw).ok())
                .map(|jdm| (m.lang, jdm))
        })
        .collect();
    let dir = tables_dir_of(&core);
    let aux = (
        dir.join(format!("{}_points.json", src.lang)).exists(),
        dir.join(format!("{}_point_index.json", src.lang)).exists(),
    );
    attribution_engine::summarize(&core.knowledge, staging.as_ref(), &tables, aux, &source_id)
}

/// 打包导出资源包 → zip 压缩包写盘（单条目 pack.json，deflate 压缩）。
/// `path` = 前端原生「另存为」对话框选定的保存地址（桌面端弹出保存对话框）；
/// 缺省写 `{知识库目录}/resource-packs/{source_id}-resource-pack.zip`。
/// 返回保存地址与组成统计。
#[tauri::command]
pub fn dataset_export(
    core: tauri::State<'_, Arc<Core>>,
    source_id: String,
    path: Option<String>,
) -> Result<serde_json::Value, String> {
    let src = core
        .knowledge
        .sources()
        .into_iter()
        .find(|s| s.source_id == source_id)
        .ok_or_else(|| format!("知识源不存在: {source_id}"))?;
    let staging = core.ingest.session(&source_id);
    let mut pack = attribution_engine::export_pack(&core.knowledge, staging.as_ref(), &source_id)?;
    // 决策表组件：凡存在该源标记行的语言都携带整表快照（与 clean 的跨语言
    // 摘除范围对称，保证「导出 → 删除 → 导入」完整往返）
    for meta in core.tables.list() {
        let Some(raw) = core.tables.get_raw(&meta.lang) else {
            continue;
        };
        if let Ok(jdm) = serde_json::from_str::<serde_json::Value>(&raw)
            && attribution_engine::tagged_rule_count(&jdm, &source_id) > 0
        {
            pack.components
                .decision_tables
                .push(attribution_engine::PackDecisionTable {
                    lang: meta.lang.clone(),
                    jdm,
                });
        }
    }
    let dir = tables_dir_of(&core);
    // aux 组件（源原始提取数据）：条目级按源过滤（entry.source_id 归属；
    // 同语言多源时其他源条目不混入包，旧格式整文件判给本源并注入标记）
    for (aux_path, slot) in [
        (
            dir.join(format!("{}_points.json", src.lang)),
            &mut pack.components.aux_points,
        ),
        (
            dir.join(format!("{}_point_index.json", src.lang)),
            &mut pack.components.aux_index,
        ),
    ] {
        if aux_path.exists() {
            let content = std::fs::read_to_string(&aux_path).map_err(|e| e.to_string())?;
            let aux: serde_json::Value =
                serde_json::from_str(&content).map_err(|e| e.to_string())?;
            let (owned, _) = attribution_engine::aux_split_source(&aux, &source_id);
            if !attribution_engine::aux_is_empty(&owned) {
                *slot = Some(owned);
            }
        }
    }
    pack.checksum = attribution_engine::dataset::pack_checksum(&pack.source, &pack.components);

    let zip = attribution_engine::dataset::pack_to_zip(&pack)?;
    // 保存位置：「另存为」选定路径优先；缺省写默认导出目录
    let file_path = match path {
        Some(p) => {
            let p = PathBuf::from(p);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("创建保存目录失败 {}: {e}", parent.display()))?;
            }
            p
        }
        None => {
            let packs_dir = core.knowledge_dir.join("resource-packs");
            std::fs::create_dir_all(&packs_dir).map_err(|e| format!("创建导出目录失败: {e}"))?;
            let mut name = format!("{source_id}-resource-pack.zip");
            // 删除前自动备份不覆盖既有导出：同名时加时间戳
            if packs_dir.join(&name).exists() {
                name = format!(
                    "{source_id}-resource-pack-{}.zip",
                    chrono::Utc::now().format("%Y%m%d%H%M%S")
                );
            }
            packs_dir.join(name)
        }
    };
    // 原子写：临时文件 + rename（大 zip 导出中途失败不留半截文件）
    let tmp = file_path.with_extension(format!("zip.tmp.{}", uuid::Uuid::new_v4().simple()));
    std::fs::write(&tmp, &zip).map_err(|e| format!("写入失败 {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &file_path)
        .map_err(|e| format!("替换失败 {}: {e}", file_path.display()))?;
    Ok(serde_json::json!({
        "file_path": file_path.display().to_string(),
        "filename": file_path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
        "size_bytes": zip.len(),
        "points": pack.components.knowledge_points.len(),
        "has_table": !pack.components.decision_tables.is_empty(),
        "has_aux": pack.components.aux_points.is_some(),
        "has_staging": pack.components.staging.is_some(),
    }))
}

/// 导入资源包（校验 → 知识库热更新 → 决策表按 _meta 合并 → 决策数据写回 → 草稿恢复）
#[tauri::command]
pub fn dataset_import(
    core: tauri::State<'_, Arc<Core>>,
    pack: attribution_engine::ResourcePack,
) -> Result<serde_json::Value, String> {
    apply_pack(core.inner(), pack)
}

/// 导入资源包（字节自适应：zip 压缩包 / 裸 JSON——web-ui 文件选择器直接上传字节）
#[tauri::command]
pub fn dataset_import_bytes(
    core: tauri::State<'_, Arc<Core>>,
    data: Vec<u8>,
) -> Result<serde_json::Value, String> {
    let pack = attribution_engine::dataset::pack_from_bytes(&data)?;
    apply_pack(core.inner(), pack)
}

fn apply_pack(
    core: &Arc<Core>,
    pack: attribution_engine::ResourcePack,
) -> Result<serde_json::Value, String> {
    attribution_engine::validate_pack(&pack)?;
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
        let (json, installed, replaced, added) = match core.tables.get_raw(&dt.lang) {
            None => (
                serde_json::to_string(&dt.jdm).map_err(|e| e.to_string())?,
                true,
                0,
                0,
            ),
            Some(raw) => {
                let mut jdm: serde_json::Value =
                    serde_json::from_str(&raw).map_err(|e| e.to_string())?;
                let (r, a) = attribution_engine::merge_tagged_rows(
                    &mut jdm,
                    &attribution_engine::dataset::table_rows(&dt.jdm),
                    &source_id,
                );
                (
                    serde_json::to_string(&jdm).map_err(|e| e.to_string())?,
                    false,
                    r,
                    a,
                )
            }
        };
        attribution_engine::AttributionEngine::validate(&json)
            .map_err(|e| format!("决策表合并结果校验未通过（{}），整体拒收: {e}", dt.lang))?;
        updates.push(TableUpdate {
            lang: dt.lang.clone(),
            json,
            installed,
            replaced,
            added,
        });
    }

    let knowledge = attribution_engine::import_pack(&core.knowledge, &pack)?;
    core.knowledge.save_to_dir(&core.knowledge_dir)?;

    let mut table_installed = false;
    let mut rules_replaced = 0usize;
    let mut rules_added = 0usize;
    for u in updates {
        core.tables
            .update(&u.lang, &u.json)
            .map_err(|e| e.to_string())?;
        pack_atomic_write(
            &tables_dir_of(core).join(format!("{}_gec_attribution.json", u.lang)),
            &u.json,
        )?;
        table_installed |= u.installed;
        rules_replaced += u.replaced;
        rules_added += u.added;
    }

    // 决策数据（aux 文件）：条目级合并——该源条目以资源包为权威，
    // 其他源条目原样保留（同语言多源不互相覆盖）
    let dir = tables_dir_of(core);
    let mut aux_written = 0usize;
    for (value, name) in [
        (&pack.components.aux_points, "points"),
        (&pack.components.aux_index, "point_index"),
    ] {
        if let Some(v) = value {
            let path = dir.join(format!("{}_{}.json", pack.source.lang, name));
            let merged = if path.exists() {
                let content = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
                let current: serde_json::Value =
                    serde_json::from_str(&content).map_err(|e| e.to_string())?;
                attribution_engine::aux_merge_source(&current, v, &source_id)
            } else {
                v.clone()
            };
            let json = serde_json::to_string_pretty(&merged).map_err(|e| e.to_string())?;
            pack_atomic_write(&path, &json)?;
            aux_written += 1;
        }
    }

    // staging 草稿会话：仅当无同 ID 会话时恢复（不打断进行中的挖掘；
    // 检查 + 插入同锁完成，杜绝与并发 ingest 启动的 TOCTOU）
    let mut staging_restored = false;
    if let Some(session) = &pack.components.staging
        && core.ingest.put_session_if_absent(session.clone())
    {
        std::fs::create_dir_all(&core.ingest_dir).map_err(|e| e.to_string())?;
        pack_atomic_write(
            &core.ingest_dir.join(format!("{source_id}.json")),
            &serde_json::to_string_pretty(session).map_err(|e| e.to_string())?,
        )?;
        staging_restored = true;
    }
    Ok(serde_json::json!({
        "knowledge": knowledge,
        "table_installed": table_installed,
        "rules_replaced": rules_replaced,
        "rules_added": rules_added,
        "aux_written": aux_written,
        "staging_restored": staging_restored,
    }))
}

/// 删除资源包（知识库 + 决策表该源规则行 + 决策数据 + 草稿；不可逆）
#[tauri::command]
pub fn dataset_clean(
    core: tauri::State<'_, Arc<Core>>,
    source_id: String,
    keep_orphan_points: Option<bool>,
) -> Result<serde_json::Value, String> {
    let src = core
        .knowledge
        .sources()
        .into_iter()
        .find(|s| s.source_id == source_id)
        .ok_or_else(|| format!("知识源不存在: {source_id}"))?;

    // ⓪ 预检：各决策表摘除结果先算出并通过引擎校验，再开始任何落盘——
    //   杜绝「知识点已删、表更新中途失败」的半应用状态
    let mut table_updates: Vec<(String, String, usize)> = Vec::new(); // (lang, json, removed)
    for table in core.tables.list() {
        let Some(raw) = core.tables.get_raw(&table.lang) else {
            continue;
        };
        let mut jdm: serde_json::Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let removed = attribution_engine::remove_tagged_rows(&mut jdm, &source_id);
        if removed == 0 {
            continue;
        }
        let json = serde_json::to_string(&jdm).map_err(|e| e.to_string())?;
        attribution_engine::AttributionEngine::validate(&json)
            .map_err(|e| format!("决策表摘除结果校验未通过（{}），中止删除: {e}", table.lang))?;
        table_updates.push((table.lang, json, removed));
    }

    let knowledge = attribution_engine::clean_pack(
        &core.knowledge,
        &source_id,
        keep_orphan_points.unwrap_or(false),
    )?;
    core.knowledge.save_to_dir(&core.knowledge_dir)?;

    let mut rules_removed = 0usize;
    for (lang, json, removed) in table_updates {
        core.tables
            .update(&lang, &json)
            .map_err(|e| e.to_string())?;
        pack_atomic_write(
            &tables_dir_of(&core).join(format!("{}_gec_attribution.json", lang)),
            &json,
        )?;
        rules_removed += removed;
    }

    // 决策数据（aux 文件）：条目级摘除该源条目（entry.source_id 归属；
    // 同语言其他源条目保留）；摘空后删除文件
    let dir = tables_dir_of(&core);
    let mut aux_removed = 0usize;
    for name in ["points", "point_index"] {
        let path = dir.join(format!("{}_{}.json", src.lang, name));
        if !path.exists() {
            continue;
        }
        let content = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let aux: serde_json::Value = serde_json::from_str(&content).map_err(|e| e.to_string())?;
        let (_, rest) = attribution_engine::aux_split_source(&aux, &source_id);
        if attribution_engine::aux_is_empty(&rest) {
            std::fs::remove_file(&path).map_err(|e| format!("删除失败 {}: {e}", path.display()))?;
        } else {
            let json = serde_json::to_string_pretty(&rest).map_err(|e| e.to_string())?;
            pack_atomic_write(&path, &json)?;
        }
        aux_removed += 1;
    }

    let staging_removed = core.ingest.discard(&source_id).is_ok();
    let staged_file = core.ingest_dir.join(format!("{source_id}.json"));
    if staged_file.exists() {
        let _ = std::fs::remove_file(&staged_file);
    }
    Ok(serde_json::json!({
        "knowledge": knowledge,
        "rules_removed": rules_removed,
        "aux_removed": aux_removed,
        "staging_removed": staging_removed,
    }))
}
