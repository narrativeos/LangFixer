//! 知识源导入管线 API（一键导入：正文分块 → agent 逐块挖掘 → 草稿区 → 人工一键发布）
//!
//! - `POST /gec/knowledge/ingest` — 开启导入：注册知识源 + 分块 + 后台逐块 agent 挖掘
//! - `GET /gec/knowledge/ingest` — 全部导入会话摘要
//! - `GET /gec/knowledge/ingest/:source_id` — 会话详情（进度 / 草稿 / 错误）
//! - `GET /gec/knowledge/ingest/:source_id/plan` — 发布预览（归并计划：新点 / 背书 / 已覆盖）
//! - `POST /gec/knowledge/ingest/:source_id/publish` — 一键发布（热更新知识库；token 鉴权）
//! - `POST /gec/knowledge/ingest/:source_id/discard` — 放弃会话（丢弃草稿）
//!
//! 闭环与约束（与 `attribution_engine::ingest` 核心一致）：
//! - agent 逐块抽取只进**草稿区**，发布前不影响运行时归因；
//! - 发布 = 确定性归并（既有点 1:N 背书 / 新点造点）+ 知识库热更新（原子替换）；
//! - 发布与知识库 PUT 共用 `GEC_TABLES_UPDATE_TOKEN` 鉴权（IP 边界一致）。

use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use serde::{Deserialize, Serialize};

use attribution_engine::ingest::{ApplyReport, DraftPoint, MergePlan};
use gec_core::{LlmDetectConfig, agent_timeout, run_agent};
use shared_model::KnowledgeSource;

use crate::agent_tools::AgentToolCtx;
use crate::state::AppState;

use super::ApiError;

/// 默认块大小（字符）：单块 = 一次 agent 抽取任务
const DEFAULT_CHUNK_CHARS: usize = 6000;

#[derive(Deserialize)]
pub struct IngestStartRequest {
    pub source_id: String,
    pub title: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub edition: String,
    pub lang: String,
    /// internal_test / licensed / public
    #[serde(default)]
    pub license_scope: String,
    /// 书本正文（纯文本 / Markdown）
    pub text: String,
    #[serde(default)]
    pub max_chunk_chars: Option<usize>,
}

#[derive(Serialize)]
pub struct IngestStartResponse {
    pub source_id: String,
    pub chunks_total: usize,
}

/// POST /gec/knowledge/ingest — 一键导入开始
pub async fn start_ingest(
    State(state): State<Arc<AppState>>,
    Json(req): Json<IngestStartRequest>,
) -> Result<Json<IngestStartResponse>, ApiError> {
    if req.source_id.trim().is_empty() || req.title.trim().is_empty() {
        return Err(ApiError::BadRequest("source_id / title 不能为空".into()));
    }
    if req.text.trim().len() < 100 {
        return Err(ApiError::BadRequest("正文过短（< 100 字符）".into()));
    }
    // LLM + 该语言 ingest 模板必须可用（挖掘由内置 agent 执行）
    let service = state.llm_service();
    if !service.config.enabled() {
        return Err(ApiError::BadRequest(
            "LLM 未配置：知识源挖掘由内置 agent 执行，请先在设置中配置 LLM".into(),
        ));
    }
    if service
        .templates
        .for_lang_cefr(&req.lang, "ingest")
        .is_none()
    {
        return Err(ApiError::BadRequest(format!(
            "语言 {} 缺少 ingest 提示词模板（prompts/{{lang}}_ingest_prompt.json）",
            req.lang
        )));
    }
    let source = KnowledgeSource {
        source_id: req.source_id.trim().to_string(),
        title: req.title.trim().to_string(),
        author: req.author,
        edition: req.edition,
        lang: req.lang.clone(),
        license_scope: if req.license_scope.is_empty() {
            "internal_test".to_string()
        } else {
            req.license_scope
        },
        point_count: 0,
    };
    let chunks = attribution_engine::ingest::chunk_text(
        &req.text,
        req.max_chunk_chars.unwrap_or(DEFAULT_CHUNK_CHARS),
    );
    if chunks.is_empty() {
        return Err(ApiError::BadRequest("正文分块结果为空".into()));
    }
    let summary = state
        .ingest
        .open_session(source, chunks.len())
        .map_err(ApiError::BadRequest)?;
    tracing::info!(
        source_id = %summary.source_id,
        chunks = chunks.len(),
        "知识源导入会话已开启（后台逐块 agent 挖掘）"
    );

    // 后台逐块挖掘（顺序执行：草稿区共享，避免并行 agent 相互覆盖既有认知）
    let chunks_total = chunks.len();
    spawn_mining(&state, &req.lang, &summary.source_id, chunks);
    Ok(Json(IngestStartResponse {
        source_id: summary.source_id,
        chunks_total,
    }))
}

/// 后台逐块挖掘任务（JSON 正文与文件上传两条导入路径共用）
fn spawn_mining(
    state: &Arc<AppState>,
    lang: &str,
    source_id: &str,
    chunks: Vec<attribution_engine::ingest::TextChunk>,
) {
    let state = state.clone();
    let lang = lang.to_string();
    let source_id = source_id.to_string();
    tokio::spawn(async move {
        let total = chunks.len();
        for chunk in chunks {
            // 会话被外部放弃/熔断 → 停止挖掘
            if !state.ingest.is_active(&source_id) {
                break;
            }
            // 单块失败重试一次（LLM 抖动）；连续失败达阈值由 mark_chunk_done 熔断
            let mut err =
                run_ingest_chunk(&state, &lang, &source_id, chunk.index, total, &chunk.text).await;
            if err.is_some() && state.ingest.is_active(&source_id) {
                err = run_ingest_chunk(&state, &lang, &source_id, chunk.index, total, &chunk.text)
                    .await
                    .map(|e| format!("{e}（重试后仍失败）"));
            }
            let active = state
                .ingest
                .mark_chunk_done(&source_id, err)
                .unwrap_or(false);
            // 每块后持久化草稿（重启可恢复；发布/放弃时清理）
            let _ = state.ingest.save_to_dir(&state.ingest_dir);
            if !active {
                break;
            }
        }
        tracing::info!(
            source_id = %source_id,
            chunks = total,
            "知识源逐块挖掘完成（等待人工审核发布）"
        );
    });
}

/// 跑一块的 agent 抽取；返回 `None` = 成功，`Some(错误文本)` = 失败（记入会话）
async fn run_ingest_chunk(
    state: &Arc<AppState>,
    lang: &str,
    source_id: &str,
    index: usize,
    total: usize,
    chunk_body: &str,
) -> Option<String> {
    let service = state.llm_service();
    let Some(template) = service.templates.for_lang_cefr(lang, "ingest") else {
        return Some(format!("语言 {lang} 缺少 ingest 模板"));
    };
    let idx = index.to_string();
    let tot = total.to_string();
    let input = template.render(&[
        ("source_id", source_id),
        ("chunk_index", &idx),
        ("total_chunks", &tot),
        ("chunk", chunk_body),
    ]);
    let ctx = AgentToolCtx::new(state, lang);
    let tools = ctx.build_tools();
    let config = LlmDetectConfig::from_parts(
        service.config.base_url.clone(),
        service.config.api_key.clone(),
        service.config.model.clone(),
        service.config.timeout_secs,
        template.system.clone(),
        input.clone(),
    );
    match tokio::time::timeout(
        agent_timeout(),
        run_agent(&config, &template.system, &input, tools),
    )
    .await
    {
        Ok(Ok(_result)) => None,
        Ok(Err(e)) => Some(format!("块 {index} agent 运行失败: {e}")),
        Err(_) => Some(format!("块 {index} agent 运行超时")),
    }
}

/// GET /gec/knowledge/ingest — 会话摘要列表
pub async fn list_ingests(
    State(state): State<Arc<AppState>>,
) -> Json<Vec<attribution_engine::ingest::IngestSummary>> {
    Json(state.ingest.list())
}

/// GET /gec/knowledge/ingest/:source_id — 会话详情（含草稿）
pub async fn get_ingest(
    State(state): State<Arc<AppState>>,
    Path(source_id): Path<String>,
) -> Result<Json<attribution_engine::ingest::IngestSession>, ApiError> {
    state
        .ingest
        .session(&source_id)
        .map(Json)
        .ok_or_else(|| ApiError::BadRequest(format!("导入会话不存在: {source_id}")))
}

/// GET /gec/knowledge/ingest/:source_id/plan — 发布预览（归并计划，不落库）
pub async fn get_ingest_plan(
    State(state): State<Arc<AppState>>,
    Path(source_id): Path<String>,
) -> Result<Json<MergePlan>, ApiError> {
    let session = state
        .ingest
        .session(&source_id)
        .ok_or_else(|| ApiError::BadRequest(format!("导入会话不存在: {source_id}")))?;
    let drafts: Vec<DraftPoint> = session.drafts.iter().map(|d| d.draft.clone()).collect();
    Ok(Json(attribution_engine::ingest::plan_merge(
        &state.knowledge,
        &session.source,
        &drafts,
    )))
}

/// 草稿审阅条目（草稿 + 归并预判）
#[derive(Serialize)]
pub struct IngestDraftItem {
    pub seq: u64,
    pub staged_at: String,
    pub draft: DraftPoint,
    /// 归并预判：`endorse` 背书既有点 / `create` 造新点 / `covered` 本源已背书
    pub action: String,
    /// 命中或即将分配的 point_id
    pub point_id: String,
}

/// GET /gec/knowledge/ingest/:source_id/drafts — 草稿明细（含逐条归并预判）
pub async fn get_ingest_drafts(
    State(state): State<Arc<AppState>>,
    Path(source_id): Path<String>,
) -> Result<Json<Vec<IngestDraftItem>>, ApiError> {
    let session = state
        .ingest
        .session(&source_id)
        .ok_or_else(|| ApiError::BadRequest(format!("导入会话不存在: {source_id}")))?;
    let drafts: Vec<DraftPoint> = session.drafts.iter().map(|d| d.draft.clone()).collect();
    let plan = attribution_engine::ingest::plan_merge(&state.knowledge, &session.source, &drafts);
    let items = session
        .drafts
        .iter()
        .zip(plan.actions)
        .map(|(s, a)| IngestDraftItem {
            seq: s.seq,
            staged_at: s.staged_at.clone(),
            draft: s.draft.clone(),
            action: a.action,
            point_id: a.point_id,
        })
        .collect();
    Ok(Json(items))
}

/// 发布后反哺：新点改错对 → `lexicon_add` 建议队列（人工采纳才生效，进向量召回层）。
/// 幂等：同 (lang, wrong, right) 已有 pending 建议则跳过；失败不阻断发布（仅告警）
fn stage_lexicon_add_suggestions(
    state: &AppState,
    source_id: &str,
    plan: &MergePlan,
) -> Result<usize, String> {
    use gec_core::reflection_center::suggestion_kinds::LEXICON_ADD;
    let lang = plan
        .new_points
        .first()
        .map(|p| p.lang.clone())
        .unwrap_or_default();
    if lang.is_empty() {
        return Ok(0);
    }
    let pending = state
        .reflection_center
        .list_suggestions(Some("pending"), Some(&lang), 500)
        .map_err(|e| e.to_string())?;
    let mut seen: std::collections::HashSet<(String, String)> = pending
        .iter()
        .filter(|s| s.kind == LEXICON_ADD)
        .map(|s| {
            (
                s.payload
                    .get("wrong")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                s.payload
                    .get("right")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            )
        })
        .collect();
    let mut seeds: Vec<lexicon_store::SuggestionSeed> = Vec::new();
    for p in &plan.new_points {
        for c in &p.corrections {
            let key = (c.wrong.trim().to_lowercase(), c.right.trim().to_lowercase());
            if key.0.is_empty() || key.1.is_empty() || seen.contains(&key) {
                continue;
            }
            seen.insert(key);
            seeds.push(lexicon_store::SuggestionSeed {
                kind: LEXICON_ADD.into(),
                target: p.point_id.clone(),
                summary: format!("改错对：{} → {}（知识源 {source_id}）", c.wrong, c.right),
                evidence: vec![format!("知识源 {source_id} 发布新点 {}", p.point_id)],
                payload: serde_json::json!({
                    "lang": p.lang,
                    "point_id": p.point_id,
                    "error_type": p.error_type,
                    "wrong": c.wrong,
                    "right": c.right,
                    "source_id": source_id,
                }),
                priority: 1,
            });
        }
    }
    if seeds.is_empty() {
        return Ok(0);
    }
    let origin = serde_json::json!({ "source": "knowledge_ingest", "source_id": source_id });
    state
        .reflection_center
        .save_report(
            &lang,
            &chrono::Utc::now().to_rfc3339(),
            &origin.to_string(),
            &seeds,
        )
        .map_err(|e| e.to_string())?;
    Ok(seeds.len())
}

/// 发布一个导入会话（agent 工具 `publish_knowledge` 确认后执行与本端点同源）
pub(crate) fn publish_session(
    state: &Arc<AppState>,
    source_id: &str,
) -> Result<ApplyReport, String> {
    let session = state
        .ingest
        .session(source_id)
        .ok_or_else(|| format!("导入会话不存在: {source_id}"))?;
    let drafts: Vec<DraftPoint> = session.drafts.iter().map(|d| d.draft.clone()).collect();
    if drafts.is_empty() {
        return Err(format!("会话 {source_id} 没有草稿可发布"));
    }
    let plan = attribution_engine::ingest::plan_merge(&state.knowledge, &session.source, &drafts);

    // ⓪ 预检：规则草稿合并结果先算出并通过引擎校验，再开始任何落盘——
    //   杜绝「知识点已发布、决策表中途失败」的半应用状态
    let rule_drafts: Vec<attribution_engine::ingest::DraftRule> = session
        .rule_drafts
        .iter()
        .map(|d| d.draft.clone())
        .collect();
    let mut table_updates: Vec<(String, String, attribution_engine::ingest::RulePlan)> = Vec::new();
    if !rule_drafts.is_empty() {
        let lang = session.source.lang.clone();
        let raw = state
            .tables
            .get_raw(&lang)
            .ok_or_else(|| format!("语言 {lang} 无决策表，无法应用派生规则"))?;
        let rule_plan =
            attribution_engine::ingest::plan_rules(&raw, &lang, source_id, &rule_drafts)
                .map_err(|e| format!("规则归并计划失败: {e}"))?;
        if rule_plan.new + rule_plan.replaced > 0 {
            let mut jdm: serde_json::Value =
                serde_json::from_str(&raw).map_err(|e| e.to_string())?;
            let rows: Vec<serde_json::Value> = rule_plan
                .rows
                .iter()
                .filter(|r| r.action != attribution_engine::ingest::RULE_CONFLICT)
                .map(|r| r.row.clone())
                .collect();
            attribution_engine::merge_tagged_rows(&mut jdm, &rows, source_id);
            let json = serde_json::to_string(&jdm).map_err(|e| e.to_string())?;
            attribution_engine::AttributionEngine::validate(&json)
                .map_err(|e| format!("决策表合并结果校验未通过（{lang}），中止发布: {e}"))?;
            table_updates.push((lang, json, rule_plan));
        }
    }

    // ① 知识点归并发布（知识库热更新）
    let mut report =
        attribution_engine::ingest::apply_plan(&state.knowledge, &session.source, &plan)?;
    // 合并成果写回知识库目录（跨重启存活；失败不阻断发布，内存已生效 + 告警）
    crate::routes::knowledge::persist_knowledge(state);
    // 反哺闭环：新点改错对 → lexicon_add 建议队列（人工采纳才生效；失败不阻断发布）
    match stage_lexicon_add_suggestions(state, &report.source_id, &plan) {
        Ok(n) if n > 0 => tracing::info!(suggestions = n, "改错对已汇入建议队列（待人工采纳）"),
        Err(e) => tracing::warn!(error = %e, "改错对建议入队失败（不影响发布）"),
        _ => {}
    }

    // ② 决策表规则应用（预检已通过；TableStore 校验 + 版本留痕 + 写回文件跨重启存活）
    for (lang_t, json, rule_plan) in &table_updates {
        let meta = state
            .tables
            .update(lang_t, json)
            .map_err(|e| format!("决策表更新失败: {e}"))?;
        if let Err(e) = state.table_history.save(lang_t, meta.rule_count, json) {
            tracing::warn!(lang = %lang_t, error = %e, "决策表版本存档失败（不影响发布）");
        }
        if let Err(e) = super::knowledge_dataset::atomic_write(
            &super::knowledge_dataset::table_file_path(state, lang_t),
            json,
        ) {
            tracing::warn!(lang = %lang_t, error = ?e, "决策表写回文件失败（内存已生效，重启后回退）");
        }
        report.rules_new = rule_plan.new;
        report.rules_replaced = rule_plan.replaced;
        report.rules_conflicts = rule_plan.conflicts;
    }

    // 发布成功 → 关闭会话并清理持久化草稿
    let _ = state.ingest.discard(source_id);
    let staged_file = state.ingest_dir.join(format!("{source_id}.json"));
    if staged_file.exists() {
        let _ = std::fs::remove_file(&staged_file);
    }
    tracing::info!(
        source_id = %report.source_id,
        new_points = report.new_points,
        endorsed = report.endorsed,
        rules_new = report.rules_new,
        rules_replaced = report.rules_replaced,
        "知识源已发布（知识库热更新 + 决策表规则派生，归因富化即时生效）"
    );
    Ok(report)
}

/// POST /gec/knowledge/ingest/:source_id/publish — 人工一键发布
pub async fn publish_ingest(
    State(state): State<Arc<AppState>>,
    Path(source_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<ApplyReport>, ApiError> {
    super::knowledge::check_token(&state, &headers)?;
    publish_session(&state, &source_id)
        .map(Json)
        .map_err(ApiError::BadRequest)
}

/// POST /gec/knowledge/ingest/:source_id/discard — 放弃会话
pub async fn discard_ingest(
    State(state): State<Arc<AppState>>,
    Path(source_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let dropped = state
        .ingest
        .discard(&source_id)
        .map_err(ApiError::BadRequest)?;
    let staged_file = state.ingest_dir.join(format!("{source_id}.json"));
    if staged_file.exists() {
        let _ = std::fs::remove_file(&staged_file);
    }
    Ok(Json(serde_json::json!({
        "source_id": source_id,
        "dropped_drafts": dropped
    })))
}

// ---------------------------------------------------------------------------
// 文档解析预览 + 文件上传导入（epub / txt / md / docx / 可解析 pdf）
// ---------------------------------------------------------------------------

/// 章节预览条目
#[derive(Serialize)]
pub struct ChapterPreview {
    pub index: usize,
    pub heading: String,
    pub chars: usize,
}

/// POST /gec/knowledge/parse-preview — 解析预览（不落任何状态；上传前确认格式/章节/告警）
#[derive(Serialize)]
pub struct ParsePreviewResponse {
    pub format: &'static str,
    pub title: String,
    pub author: String,
    pub lang_hint: String,
    pub char_count: usize,
    pub warnings: Vec<String>,
    pub suggested_source_id: String,
    pub chapters: Vec<ChapterPreview>,
}

#[derive(Deserialize)]
pub struct PreviewQuery {
    pub filename: String,
}

pub async fn parse_preview(
    State(_state): State<Arc<AppState>>,
    Query(q): Query<PreviewQuery>,
    body: Bytes,
) -> Result<Json<ParsePreviewResponse>, ApiError> {
    if body.is_empty() {
        return Err(ApiError::BadRequest("文件为空".into()));
    }
    let doc = document_parse::parse_document(&body, &q.filename)
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    Ok(Json(ParsePreviewResponse {
        format: doc.format,
        title: doc.title.clone(),
        author: doc.author,
        lang_hint: doc.lang_hint,
        char_count: doc.char_count,
        warnings: doc.warnings,
        suggested_source_id: document_parse::suggest_source_id(&doc.title, &q.filename),
        chapters: doc
            .chapters
            .iter()
            .enumerate()
            .map(|(i, c)| ChapterPreview {
                index: i,
                heading: c.heading.clone(),
                chars: c.text.chars().count(),
            })
            .collect(),
    }))
}

#[derive(Deserialize)]
pub struct UploadQuery {
    pub filename: String,
    pub source_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub edition: String,
    pub lang: String,
    #[serde(default)]
    pub license_scope: String,
    /// 章节选择：省略/"all" = 全部；否则索引列表，支持 `0,2,4-6`
    #[serde(default)]
    pub chapters: Option<String>,
    #[serde(default)]
    pub max_chunk_chars: Option<usize>,
}

/// 解析章节选择串（`0,2,4-6` / `all`）
fn parse_selection(s: Option<&str>) -> Option<Vec<usize>> {
    let s = s?.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("all") {
        return None;
    }
    let mut out = Vec::new();
    for part in s.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((a, b)) = part.split_once('-') {
            if let (Ok(a), Ok(b)) = (a.trim().parse::<usize>(), b.trim().parse::<usize>()) {
                if b >= a {
                    out.extend(a..=b);
                }
            }
        } else if let Ok(n) = part.parse::<usize>() {
            out.push(n);
        }
    }
    Some(out)
}

/// POST /gec/knowledge/ingest/upload — 上传书本文件并开启导入
pub async fn upload_ingest(
    State(state): State<Arc<AppState>>,
    Query(q): Query<UploadQuery>,
    body: Bytes,
) -> Result<Json<IngestStartResponse>, ApiError> {
    if q.source_id.trim().is_empty() {
        return Err(ApiError::BadRequest("source_id 不能为空".into()));
    }
    // LLM + ingest 模板可用性（与 JSON 导入路径一致）
    let service = state.llm_service();
    if !service.config.enabled() {
        return Err(ApiError::BadRequest(
            "LLM 未配置：知识源挖掘由内置 agent 执行，请先在设置中配置 LLM".into(),
        ));
    }
    if service.templates.for_lang_cefr(&q.lang, "ingest").is_none() {
        return Err(ApiError::BadRequest(format!(
            "语言 {} 缺少 ingest 提示词模板（prompts/{{lang}}_ingest_prompt.json）",
            q.lang
        )));
    }
    let doc = document_parse::parse_document(&body, &q.filename)
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    let selection = parse_selection(q.chapters.as_deref());
    let chapters: Vec<attribution_engine::ingest::ChapterInput> = doc
        .chapters
        .iter()
        .enumerate()
        .filter(|(i, _)| selection.as_ref().is_none_or(|sel| sel.contains(i)))
        .map(|(_, c)| attribution_engine::ingest::ChapterInput {
            heading: c.heading.clone(),
            text: c.text.clone(),
        })
        .collect();
    if chapters.is_empty() {
        return Err(ApiError::BadRequest(
            "所选章节为空（检查 chapters 参数）".into(),
        ));
    }
    let total_chars: usize = chapters.iter().map(|c| c.text.chars().count()).sum();
    if total_chars < 100 {
        return Err(ApiError::BadRequest("正文过短（< 100 字符）".into()));
    }
    let title = if q.title.trim().is_empty() {
        doc.title.clone()
    } else {
        q.title.trim().to_string()
    };
    if title.is_empty() {
        return Err(ApiError::BadRequest(
            "title 不能为空（文件未携带标题时需手填）".into(),
        ));
    }
    let source = KnowledgeSource {
        source_id: q.source_id.trim().to_string(),
        title,
        author: if q.author.is_empty() {
            doc.author.clone()
        } else {
            q.author
        },
        edition: q.edition,
        lang: q.lang.clone(),
        license_scope: if q.license_scope.is_empty() {
            "internal_test".to_string()
        } else {
            q.license_scope
        },
        point_count: 0,
    };
    let chunks = attribution_engine::ingest::chunk_chapters(
        &chapters,
        q.max_chunk_chars.unwrap_or(DEFAULT_CHUNK_CHARS),
    );
    if chunks.is_empty() {
        return Err(ApiError::BadRequest("正文分块结果为空".into()));
    }
    let summary = state
        .ingest
        .open_session(source, chunks.len())
        .map_err(ApiError::BadRequest)?;
    tracing::info!(
        source_id = %summary.source_id,
        format = doc.format,
        chapters = chapters.len(),
        chunks = chunks.len(),
        "知识源导入会话已开启（文件上传 → 章节分块 → 逐块 agent 挖掘）"
    );
    let chunks_total = chunks.len();
    spawn_mining(&state, &q.lang, &summary.source_id, chunks);
    Ok(Json(IngestStartResponse {
        source_id: summary.source_id,
        chunks_total,
    }))
}

// ---------------------------------------------------------------------------
// 归因规则派生（知识点挖掘完成后、发布前：按 error_type 分批跑 rules agent）
// ---------------------------------------------------------------------------

/// POST /gec/knowledge/ingest/:source_id/derive-rules — 开启规则派生（草稿只进会话草稿区）
pub async fn derive_rules(
    State(state): State<Arc<AppState>>,
    Path(source_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let session = state
        .ingest
        .session(&source_id)
        .ok_or_else(|| ApiError::BadRequest(format!("导入会话不存在: {source_id}")))?;
    if session.drafts.is_empty() {
        return Err(ApiError::BadRequest(
            "会话没有知识点草稿，先完成挖掘".into(),
        ));
    }
    // 按 error_type 分批（每类一次 agent 调用；类内草稿完整可见）
    let mut types: Vec<String> = session
        .drafts
        .iter()
        .map(|d| d.draft.error_type.clone())
        .collect();
    types.sort();
    types.dedup();
    state
        .ingest
        .begin_rules(&source_id, types.len())
        .map_err(ApiError::BadRequest)?;

    // LLM + rules_ingest 模板可用性
    let service = state.llm_service();
    if !service.config.enabled() {
        return Err(ApiError::BadRequest(
            "LLM 未配置：规则派生由内置 agent 执行，请先在设置中配置 LLM".into(),
        ));
    }
    if service
        .templates
        .for_lang_cefr(&session.source.lang, "rules_ingest")
        .is_none()
    {
        return Err(ApiError::BadRequest(format!(
            "语言 {} 缺少 rules_ingest 提示词模板（prompts/{{lang}}_rules_ingest_prompt.json）",
            session.source.lang
        )));
    }

    let lang = session.source.lang.clone();
    let sid = source_id.clone();
    tokio::spawn(async move {
        for error_type in types {
            if !state.ingest.is_rules_active(&sid) {
                break;
            }
            let session = match state.ingest.session(&sid) {
                Some(s) => s,
                None => break,
            };
            let points: Vec<&DraftPoint> = session
                .drafts
                .iter()
                .map(|d| &d.draft)
                .filter(|d| d.error_type == error_type)
                .collect();
            let points_json = serde_json::to_string(&points).unwrap_or_default();
            let existing = existing_rule_ids(&state, &lang);
            let err =
                run_rules_batch(&state, &lang, &sid, &error_type, &points_json, &existing).await;
            let active = state
                .ingest
                .mark_rule_batch_done(&sid, err)
                .unwrap_or(false);
            let _ = state.ingest.save_to_dir(&state.ingest_dir);
            if !active {
                break;
            }
        }
        tracing::info!(source_id = %sid, "规则派生批次完成（等待人工审阅 rules-plan）");
    });
    Ok(Json(serde_json::json!({
        "source_id": source_id,
        "batches_total": session.drafts.iter().map(|d| d.draft.error_type.clone()).collect::<std::collections::HashSet<_>>().len(),
    })))
}

/// 表内既有规则行 `_id` 清单（喂给 agent，避免重复造同 ID 行）
fn existing_rule_ids(state: &AppState, lang: &str) -> String {
    let Some(raw) = state.tables.get_raw(lang) else {
        return String::new();
    };
    let Ok(jdm) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return String::new();
    };
    attribution_engine::dataset::table_rows(&jdm)
        .iter()
        .filter_map(|r| r.get("_id").and_then(|v| v.as_str()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// 跑一个 error_type 批次的规则派生 agent
async fn run_rules_batch(
    state: &Arc<AppState>,
    lang: &str,
    source_id: &str,
    error_type: &str,
    points_json: &str,
    existing_rules: &str,
) -> Option<String> {
    let service = state.llm_service();
    let Some(template) = service.templates.for_lang_cefr(lang, "rules_ingest") else {
        return Some(format!("语言 {lang} 缺少 rules_ingest 模板"));
    };
    let input = template.render(&[
        ("source_id", source_id),
        ("error_type", error_type),
        ("points", points_json),
        ("existing_rules", existing_rules),
    ]);
    let ctx = AgentToolCtx::new(state, lang);
    let tools = ctx.build_tools();
    let config = LlmDetectConfig::from_parts(
        service.config.base_url.clone(),
        service.config.api_key.clone(),
        service.config.model.clone(),
        service.config.timeout_secs,
        template.system.clone(),
        input.clone(),
    );
    match tokio::time::timeout(
        agent_timeout(),
        run_agent(&config, &template.system, &input, tools),
    )
    .await
    {
        Ok(Ok(_result)) => None,
        Ok(Err(e)) => Some(format!("规则派生 {error_type} agent 运行失败: {e}")),
        Err(_) => Some(format!("规则派生 {error_type} agent 运行超时")),
    }
}

/// GET /gec/knowledge/ingest/:source_id/rules-plan — 规则归并预览（不落库）
pub async fn get_ingest_rules_plan(
    State(state): State<Arc<AppState>>,
    Path(source_id): Path<String>,
) -> Result<Json<Option<attribution_engine::ingest::RulePlan>>, ApiError> {
    let session = state
        .ingest
        .session(&source_id)
        .ok_or_else(|| ApiError::BadRequest(format!("导入会话不存在: {source_id}")))?;
    if session.rule_drafts.is_empty() {
        return Ok(Json(None));
    }
    let lang = session.source.lang.clone();
    let raw = state
        .tables
        .get_raw(&lang)
        .ok_or_else(|| ApiError::NotFound(format!("语言 {lang} 无决策表")))?;
    let drafts: Vec<attribution_engine::ingest::DraftRule> = session
        .rule_drafts
        .iter()
        .map(|d| d.draft.clone())
        .collect();
    let plan = attribution_engine::ingest::plan_rules(&raw, &lang, &source_id, &drafts)
        .map_err(ApiError::Internal)?;
    Ok(Json(Some(plan)))
}
