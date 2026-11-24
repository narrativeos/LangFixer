use std::sync::Arc;

use gec_core::LlmDetectConfig;

use shared_model::{KnowledgePoint, KnowledgeSource};

use super::core::Core;
/// 知识源注册表（知识库「知识库」页展示；目录缺失时返回空列表）
#[tauri::command]
pub fn knowledge_sources(core: tauri::State<'_, Arc<Core>>) -> Vec<KnowledgeSource> {
    core.knowledge.sources()
}

/// 知识点列表（可按知识源过滤；1:N 关联）
#[tauri::command]
pub fn knowledge_points(
    core: tauri::State<'_, Arc<Core>>,
    lang: String,
    source_id: Option<String>,
) -> Vec<KnowledgePoint> {
    core.knowledge.points(&lang, source_id.as_deref())
}

// ---------------------------------------------------------------------------
// 知识源导入管线（一键导入：正文分块 → 后台逐块 agent 挖掘 → 草稿区 → 人工一键发布；
// 与 API 端 /gec/knowledge/ingest* 同构）
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
pub struct IngestReq {
    pub source_id: String,
    pub title: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub edition: String,
    pub lang: String,
    #[serde(default)]
    pub license_scope: String,
    pub text: String,
}

/// 桌面端默认块大小（与 API 端一致）
const INGEST_CHUNK_CHARS: usize = 6000;

/// 跑一块的 agent 抽取；返回 `None` = 成功，`Some(错误文本)` = 失败（记入会话）
async fn run_ingest_chunk(
    core: &Arc<Core>,
    lang: &str,
    source_id: &str,
    index: usize,
    total: usize,
    chunk_body: &str,
) -> Option<String> {
    let service = core.llm.read().unwrap().clone();
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
    let ctx = crate::agent_tools::AgentToolCtx::new(core, lang);
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
        gec_core::agent_timeout(),
        gec_core::run_agent(&config, &template.system, &input, tools),
    )
    .await
    {
        Ok(Ok(_result)) => None,
        Ok(Err(e)) => Some(format!("块 {index} agent 运行失败: {e}")),
        Err(_) => Some(format!("块 {index} agent 运行超时")),
    }
}

/// 一键导入开始：注册知识源 + 分块 + 后台逐块 agent 挖掘（草稿只进草稿区）
#[tauri::command]
pub async fn start_ingest(
    core: tauri::State<'_, Arc<Core>>,
    req: IngestReq,
) -> Result<serde_json::Value, String> {
    let core = core.inner().clone();
    if req.source_id.trim().is_empty() || req.title.trim().is_empty() {
        return Err("source_id / title 不能为空".into());
    }
    if req.text.trim().len() < 100 {
        return Err("正文过短（< 100 字符）".into());
    }
    let service = core.llm.read().unwrap().clone();
    if !service.config.enabled() {
        return Err("LLM 未配置：知识源挖掘由内置 agent 执行，请先在设置中配置 LLM".into());
    }
    if service
        .templates
        .for_lang_cefr(&req.lang, "ingest")
        .is_none()
    {
        return Err(format!(
            "语言 {} 缺少 ingest 提示词模板（prompts/{{lang}}_ingest_prompt.json）",
            req.lang
        ));
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
    let chunks = attribution_engine::ingest::chunk_text(&req.text, INGEST_CHUNK_CHARS);
    if chunks.is_empty() {
        return Err("正文分块结果为空".into());
    }
    let summary = core.ingest.open_session(source, chunks.len())?;
    tracing::info!(
        source_id = %summary.source_id,
        chunks = chunks.len(),
        "知识源导入会话已开启（后台逐块 agent 挖掘）"
    );
    let lang = req.lang.clone();
    let source_id = summary.source_id.clone();
    let chunks_total = chunks.len();
    tokio::spawn(async move {
        let total = chunks.len();
        for chunk in chunks {
            if !core.ingest.is_active(&source_id) {
                break;
            }
            let mut err =
                run_ingest_chunk(&core, &lang, &source_id, chunk.index, total, &chunk.text).await;
            if err.is_some() && core.ingest.is_active(&source_id) {
                err = run_ingest_chunk(&core, &lang, &source_id, chunk.index, total, &chunk.text)
                    .await
                    .map(|e| format!("{e}（重试后仍失败）"));
            }
            let active = core
                .ingest
                .mark_chunk_done(&source_id, err)
                .unwrap_or(false);
            let _ = core.ingest.save_to_dir(&core.ingest_dir);
            if !active {
                break;
            }
        }
        tracing::info!(source_id = %source_id, "知识源逐块挖掘完成（等待人工审核发布）");
    });
    Ok(serde_json::json!({
        "source_id": summary.source_id,
        "chunks_total": chunks_total,
    }))
}

/// 导入会话摘要列表
#[tauri::command]
pub fn ingest_sessions(
    core: tauri::State<'_, Arc<Core>>,
) -> Vec<attribution_engine::ingest::IngestSummary> {
    core.ingest.list()
}

/// 会话详情（进度 / 草稿 / 错误 / 状态）
#[tauri::command]
pub fn ingest_session(
    core: tauri::State<'_, Arc<Core>>,
    source_id: String,
) -> Result<attribution_engine::ingest::IngestSession, String> {
    core.ingest
        .session(&source_id)
        .ok_or_else(|| format!("导入会话不存在: {source_id}"))
}

/// 草稿明细（含逐条归并预判）
#[tauri::command]
pub fn ingest_drafts(
    core: tauri::State<'_, Arc<Core>>,
    source_id: String,
) -> Result<Vec<serde_json::Value>, String> {
    let session = core
        .ingest
        .session(&source_id)
        .ok_or_else(|| format!("导入会话不存在: {source_id}"))?;
    let drafts: Vec<attribution_engine::ingest::DraftPoint> =
        session.drafts.iter().map(|d| d.draft.clone()).collect();
    let plan = attribution_engine::ingest::plan_merge(&core.knowledge, &session.source, &drafts);
    Ok(session
        .drafts
        .iter()
        .zip(plan.actions)
        .map(|(s, a)| {
            serde_json::json!({
                "seq": s.seq,
                "staged_at": s.staged_at,
                "draft": s.draft,
                "action": a.action,
                "point_id": a.point_id,
            })
        })
        .collect())
}

/// 发布预览（归并计划，不落库）
#[tauri::command]
pub fn ingest_plan(
    core: tauri::State<'_, Arc<Core>>,
    source_id: String,
) -> Result<attribution_engine::ingest::MergePlan, String> {
    let session = core
        .ingest
        .session(&source_id)
        .ok_or_else(|| format!("导入会话不存在: {source_id}"))?;
    let drafts: Vec<attribution_engine::ingest::DraftPoint> =
        session.drafts.iter().map(|d| d.draft.clone()).collect();
    Ok(attribution_engine::ingest::plan_merge(
        &core.knowledge,
        &session.source,
        &drafts,
    ))
}

/// 人工一键发布（知识库热更新；桌面端本地操作无需 token）
#[tauri::command]
pub fn publish_ingest(
    core: tauri::State<'_, Arc<Core>>,
    source_id: String,
) -> Result<attribution_engine::ingest::ApplyReport, String> {
    crate::agent_tools::publish_session(&core, &source_id)
}

/// 放弃会话（丢弃草稿；生效知识库不受影响）
#[tauri::command]
pub fn discard_ingest(
    core: tauri::State<'_, Arc<Core>>,
    source_id: String,
) -> Result<serde_json::Value, String> {
    let dropped = core.ingest.discard(&source_id)?;
    let staged_file = core.ingest_dir.join(format!("{source_id}.json"));
    if staged_file.exists() {
        let _ = std::fs::remove_file(&staged_file);
    }
    Ok(serde_json::json!({ "source_id": source_id, "dropped_drafts": dropped }))
}

/// 解析预览（书本文件字节 → 元数据/章节/告警；不落任何状态）
#[tauri::command]
pub fn parse_preview(
    core: tauri::State<'_, Arc<Core>>,
    filename: String,
    data: Vec<u8>,
) -> Result<serde_json::Value, String> {
    let _ = core;
    if data.is_empty() {
        return Err("文件为空".into());
    }
    let doc = document_parse::parse_document(&data, &filename).map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "format": doc.format,
        "title": doc.title,
        "author": doc.author,
        "lang_hint": doc.lang_hint,
        "char_count": doc.char_count,
        "warnings": doc.warnings,
        "suggested_source_id": document_parse::suggest_source_id(&doc.title, &filename),
        "chapters": doc.chapters.iter().enumerate().map(|(i, c)| serde_json::json!({
            "index": i,
            "heading": c.heading,
            "chars": c.text.chars().count(),
        })).collect::<Vec<_>>(),
    }))
}

#[derive(serde::Deserialize)]
pub struct IngestFileReq {
    pub filename: String,
    pub data: Vec<u8>,
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
    /// 章节索引选择（空 = 全部）
    #[serde(default)]
    pub chapters: Option<Vec<usize>>,
}

/// 一键导入开始（文件路径）：解析 → 章节选择 → 分块 → 后台逐块挖掘
#[tauri::command]
pub async fn start_ingest_file(
    core: tauri::State<'_, Arc<Core>>,
    req: IngestFileReq,
) -> Result<serde_json::Value, String> {
    let core = core.inner().clone();
    if req.source_id.trim().is_empty() {
        return Err("source_id 不能为空".into());
    }
    let service = core.llm.read().unwrap().clone();
    if !service.config.enabled() {
        return Err("LLM 未配置：知识源挖掘由内置 agent 执行，请先在设置中配置 LLM".into());
    }
    if service
        .templates
        .for_lang_cefr(&req.lang, "ingest")
        .is_none()
    {
        return Err(format!(
            "语言 {} 缺少 ingest 提示词模板（prompts/{{lang}}_ingest_prompt.json）",
            req.lang
        ));
    }
    let doc =
        document_parse::parse_document(&req.data, &req.filename).map_err(|e| e.to_string())?;
    let chapters: Vec<attribution_engine::ingest::ChapterInput> = doc
        .chapters
        .iter()
        .enumerate()
        .filter(|(i, _)| req.chapters.as_ref().is_none_or(|sel| sel.contains(i)))
        .map(|(_, c)| attribution_engine::ingest::ChapterInput {
            heading: c.heading.clone(),
            text: c.text.clone(),
        })
        .collect();
    if chapters.is_empty() {
        return Err("所选章节为空".into());
    }
    let title = if req.title.trim().is_empty() {
        doc.title.clone()
    } else {
        req.title.trim().to_string()
    };
    if title.is_empty() {
        return Err("title 不能为空（文件未携带标题时需手填）".into());
    }
    let source = KnowledgeSource {
        source_id: req.source_id.trim().to_string(),
        title,
        author: if req.author.is_empty() {
            doc.author.clone()
        } else {
            req.author
        },
        edition: req.edition,
        lang: req.lang.clone(),
        license_scope: if req.license_scope.is_empty() {
            "internal_test".to_string()
        } else {
            req.license_scope
        },
        point_count: 0,
    };
    let chunks = attribution_engine::ingest::chunk_chapters(&chapters, INGEST_CHUNK_CHARS);
    if chunks.is_empty() {
        return Err("正文分块结果为空".into());
    }
    let summary = core.ingest.open_session(source, chunks.len())?;
    tracing::info!(
        source_id = %summary.source_id,
        format = doc.format,
        chapters = chapters.len(),
        chunks = chunks.len(),
        "知识源导入会话已开启（文件 → 章节分块 → 逐块 agent 挖掘）"
    );
    let lang = req.lang.clone();
    let source_id = summary.source_id.clone();
    tokio::spawn(async move {
        let total = chunks.len();
        for chunk in chunks {
            if !core.ingest.is_active(&source_id) {
                break;
            }
            let mut err =
                run_ingest_chunk(&core, &lang, &source_id, chunk.index, total, &chunk.text).await;
            if err.is_some() && core.ingest.is_active(&source_id) {
                err = run_ingest_chunk(&core, &lang, &source_id, chunk.index, total, &chunk.text)
                    .await
                    .map(|e| format!("{e}（重试后仍失败）"));
            }
            let active = core
                .ingest
                .mark_chunk_done(&source_id, err)
                .unwrap_or(false);
            let _ = core.ingest.save_to_dir(&core.ingest_dir);
            if !active {
                break;
            }
        }
        tracing::info!(source_id = %source_id, "知识源逐块挖掘完成（等待人工审核发布）");
    });
    Ok(serde_json::json!({
        "source_id": summary.source_id,
        "chunks_total": summary.chunks_total,
    }))
}

/// 表内既有规则行 `_id` 清单（喂给 agent，避免重复造同 ID 行）
fn existing_rule_ids(core: &Core, lang: &str) -> String {
    let Some(raw) = core.tables.get_raw(lang) else {
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

/// 跑一个 error_type 批次的规则派生 agent；`None` = 成功
async fn run_rules_batch(
    core: &Arc<Core>,
    lang: &str,
    source_id: &str,
    error_type: &str,
    points_json: &str,
    existing_rules: &str,
) -> Option<String> {
    let service = core.llm.read().unwrap().clone();
    let Some(template) = service.templates.for_lang_cefr(lang, "rules_ingest") else {
        return Some(format!("语言 {lang} 缺少 rules_ingest 模板"));
    };
    let input = template.render(&[
        ("source_id", source_id),
        ("error_type", error_type),
        ("points", points_json),
        ("existing_rules", existing_rules),
    ]);
    let ctx = crate::agent_tools::AgentToolCtx::new(core, lang);
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
        gec_core::agent_timeout(),
        gec_core::run_agent(&config, &template.system, &input, tools),
    )
    .await
    {
        Ok(Ok(_result)) => None,
        Ok(Err(e)) => Some(format!("规则派生 {error_type} agent 运行失败: {e}")),
        Err(_) => Some(format!("规则派生 {error_type} agent 运行超时")),
    }
}

/// 开启规则派生（知识点挖掘完成后、发布前；按 error_type 分批跑 agent）
#[tauri::command]
pub async fn derive_rules(
    core: tauri::State<'_, Arc<Core>>,
    source_id: String,
) -> Result<serde_json::Value, String> {
    let core = core.inner().clone();
    let session = core
        .ingest
        .session(&source_id)
        .ok_or_else(|| format!("导入会话不存在: {source_id}"))?;
    if session.drafts.is_empty() {
        return Err("会话没有知识点草稿，先完成挖掘".into());
    }
    let mut types: Vec<String> = session
        .drafts
        .iter()
        .map(|d| d.draft.error_type.clone())
        .collect();
    types.sort();
    types.dedup();
    core.ingest.begin_rules(&source_id, types.len())?;
    let service = core.llm.read().unwrap().clone();
    if !service.config.enabled() {
        return Err("LLM 未配置：规则派生由内置 agent 执行，请先在设置中配置 LLM".into());
    }
    if service
        .templates
        .for_lang_cefr(&session.source.lang, "rules_ingest")
        .is_none()
    {
        return Err(format!(
            "语言 {} 缺少 rules_ingest 提示词模板（prompts/{{lang}}_rules_ingest_prompt.json）",
            session.source.lang
        ));
    }
    let lang = session.source.lang.clone();
    let sid = source_id.clone();
    let total_batches = types.len();
    tokio::spawn(async move {
        for error_type in types {
            if !core.ingest.is_rules_active(&sid) {
                break;
            }
            let session = match core.ingest.session(&sid) {
                Some(s) => s,
                None => break,
            };
            let points: Vec<&attribution_engine::ingest::DraftPoint> = session
                .drafts
                .iter()
                .map(|d| &d.draft)
                .filter(|d| d.error_type == error_type)
                .collect();
            let points_json = serde_json::to_string(&points).unwrap_or_default();
            let existing = existing_rule_ids(&core, &lang);
            let err =
                run_rules_batch(&core, &lang, &sid, &error_type, &points_json, &existing).await;
            let active = core.ingest.mark_rule_batch_done(&sid, err).unwrap_or(false);
            let _ = core.ingest.save_to_dir(&core.ingest_dir);
            if !active {
                break;
            }
        }
        tracing::info!(source_id = %sid, "规则派生批次完成（等待人工审阅 rules-plan）");
    });
    Ok(serde_json::json!({ "source_id": source_id, "batches_total": total_batches }))
}

/// 规则归并预览（新增/替换/冲突；不落库）
#[tauri::command]
pub fn ingest_rules_plan(
    core: tauri::State<'_, Arc<Core>>,
    source_id: String,
) -> Result<Option<attribution_engine::ingest::RulePlan>, String> {
    let core = core.inner();
    let session = core
        .ingest
        .session(&source_id)
        .ok_or_else(|| format!("导入会话不存在: {source_id}"))?;
    if session.rule_drafts.is_empty() {
        return Ok(None);
    }
    let lang = session.source.lang.clone();
    let raw = core
        .tables
        .get_raw(&lang)
        .ok_or_else(|| format!("语言 {lang} 无决策表"))?;
    let drafts: Vec<attribution_engine::ingest::DraftRule> = session
        .rule_drafts
        .iter()
        .map(|d| d.draft.clone())
        .collect();
    Ok(Some(attribution_engine::ingest::plan_rules(
        &raw, &lang, &source_id, &drafts,
    )?))
}

// ─────────────────────────────────────────────────────────────────────────────
// 知识源资源包（盘点 / 打包导出 / 导入 / 删除）
// 与 api-server routes/knowledge_dataset.rs 同一语义（纯逻辑在
// attribution_engine::dataset；桌面端本地操作、无 token 鉴权）。
// ─────────────────────────────────────────────────────────────────────────────

/// list_learned_knowledge — 列出指定语言的学习知识条目
#[tauri::command]
pub fn list_learned_knowledge(
    core: tauri::State<'_, Arc<Core>>,
    lang: String,
) -> Result<Vec<shared_model::LearnedKnowledge>, String> {
    core.learned_knowledge
        .list(&lang)
        .map_err(|e| e.to_string())
}

/// add_learned_knowledge — 添加学习知识条目（自动生成嵌入向量）
#[tauri::command]
pub async fn add_learned_knowledge(
    core: tauri::State<'_, Arc<Core>>,
    lang: String,
    text: String,
    error_type: String,
    correction: String,
    source: String,
) -> Result<i64, String> {
    let embedding = {
        // guard 不可跨 await（非 Send）——先克隆 Arc 再立即释放
        let guard = core.embedding_engine.read().unwrap();
        guard.as_ref().cloned()
    };
    let embedding = match embedding.as_deref() {
        Some(engine) => engine.embed(&text).await.map_err(|e| e.to_string())?,
        None => vec![],
    };
    core.learned_knowledge
        .insert(&lang, &text, &error_type, &correction, &embedding, &source)
        .map_err(|e| e.to_string())
}

/// remove_learned_knowledge — 删除指定知识条目
#[tauri::command]
pub fn remove_learned_knowledge(
    core: tauri::State<'_, Arc<Core>>,
    id: i64,
) -> Result<bool, String> {
    core.learned_knowledge.remove(id).map_err(|e| e.to_string())
}
