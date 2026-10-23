//! 检测结果统一反馈（采纳 / 放弃）+ 反思流程
//!
//! 覆盖**全部来源层**（rule / knowledge / decision / feature / statistical / llm）：
//!
//! - **采纳**（adopt；各分支均写反馈日志留痕，作为反馈历史 / 反思统计的数据基础）：
//!   - `llm` → 复用 `llm_feedback` 采纳分支（规则蒸馏 / 知识候选 / anomaly 模式蒸馏）；
//!   - `rule`（非 anomaly）→ 确认规则正确：反馈留痕 + 示例补强（有 correction 时）；
//!   - `rule`（anomaly）→ anomaly 模式蒸馏分支（与 llm 相同）；
//!   - `decision` / `feature` / `statistical` → 蒸馏为规则（与 llm 采纳同路径，
//!     规则 id 前缀 `MODEL`）+ L3 飞轮正样本；
//!   - `knowledge` → 确认知识条目正确：反馈留痕。
//!   采纳路径在 upsert 前会先按词表文件回填目标规则定义
//!   （`backfill_rule_from_file`：建库后才加入文件、播种未覆盖的规则）——
//!   防止 `upsert_example` 的「默认停用」自动创建把文件里原本启用的规则关掉。
//! - **放弃**（reject）：
//!   - `original` 加入自定义词白名单（后续检测不再报告）；
//!   - 写入反馈日志（反思流程的数据基础）；
//!   - 执行**反思流程**（[`gec_core::reflect_rejection`]）：按来源层诊断
//!     偏差来源（规则误报率 / 知识条目误召回 / 模型层误判），返回报告 +
//!     可执行建议（停用规则 / 删示例 / 删知识 / L3 负样本 / 重训练）；
//!   - 模型层来源自动汇入 L3 飞轮**负样本**（非阻断）。
//!
//! 配套端点：
//! - `POST /gec/feedback/action`：执行反思建议（disable_rule / exclude_word /
//!   remove_example / remove_knowledge / l3_negative）；
//! - `POST /gec/feedback/reflect-llm`：LLM 深度反思（`{lang}_reflect` 模板，
//!   自然语言诊断；与确定性反思分离，避免阻塞放弃操作）；
//! - `GET /gec/feedback/history`：反馈历史（按规则/层聚合统计 + 最近明细，
//!   设置界面「反馈历史」面板）。

use std::sync::Arc;

use axum::extract::{Json, Query, State};
use serde::{Deserialize, Serialize};
use shared_model::{Error, error_types, sources};

use crate::routes::ApiError;
use crate::state::AppState;

/// 统一反馈请求
#[derive(Deserialize)]
pub struct FeedbackRequest {
    pub lang: String,
    /// 检测结果（来源层由 `source` 字段决定；缺失视为规则层）
    pub error: Error,
    /// 操作：`"adopt"`（采纳）/ `"reject"`（放弃）
    pub action: String,
    /// 可选的放弃原因（记入反思报告）
    #[serde(default)]
    pub reason: Option<String>,
    /// 检测时的原文全文（采纳沉淀资产 C 用；旧客户端可缺省）
    #[serde(default)]
    pub src_text: Option<String>,
    /// 用户累积采纳本次修正后的全文（前端本地应用修正得到；
    /// adopt 时与 `src_text` 一起切句沉淀 `adopted.jsonl`）
    #[serde(default)]
    pub corrected_text: Option<String>,
}

/// 统一反馈响应
#[derive(Serialize)]
pub struct FeedbackResponse {
    pub action: String,
    pub message: String,
    /// adopt：实际走的路径（`"rule"` / `"knowledge"` / `"anomaly"` / `"confirm"`）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    /// adopt：规则信息（`{id, wrong, right, enabled, created}`）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule: Option<serde_json::Value>,
    /// adopt：知识候选（`{text, error_type, correction}`）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub knowledge: Option<serde_json::Value>,
    /// adopt：anomaly 蒸馏结果
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anomaly: Option<serde_json::Value>,
    /// reject：白名单词 ID
    #[serde(skip_serializing_if = "Option::is_none")]
    pub word_id: Option<i64>,
    /// reject：反思报告（诊断 + 证据 + 可执行建议）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reflection: Option<gec_core::ReflectionReport>,
    /// LLM 深度反思是否可用（前端可进一步调 `/gec/feedback/reflect-llm`）
    #[serde(default)]
    pub llm_reflect_ready: bool,
    /// reject：自进化 agent 结果（LLM 已配置时；自主动作已执行 + 高风险提案待确认）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<gec_core::AgentRunResult>,
}

/// POST /gec/feedback — 检测结果统一采纳 / 放弃（全部来源层）
pub async fn feedback(
    State(state): State<Arc<AppState>>,
    Json(req): Json<FeedbackRequest>,
) -> Result<Json<FeedbackResponse>, ApiError> {
    match req.action.as_str() {
        "adopt" => {
            let resp = adopt(&state, &req).await?;
            collect_feedback_assets(&req, "adopt");
            Ok(Json(resp))
        }
        "reject" => {
            let resp = reject(&state, &req).await?;
            collect_feedback_assets(&req, "reject");
            Ok(Json(resp))
        }
        other => Err(ApiError::BadRequest(format!(
            "无效操作: {other}（应为 \"adopt\" 或 \"reject\"）"
        ))),
    }
}

/// 采纳 / 放弃沉淀资产（非阻断：失败仅告警）
///
/// - adopt + `src_text`/`corrected_text` 齐备 → 切句对齐沉淀 `adopted.jsonl`
///   （训练资产 C：用户确认的 src→dst 平行句对）；
/// - reject → 片段级负偏好沉淀 `rejected.jsonl`（偏好/DPO 材料）。
///
/// 开关 [`crate::routes::l3::adopt_collect_enabled`]（默认开，多租户用
/// `GEC_L3_ADOPT_COLLECT=0` 关闭）。仅 en/zh（L3 数据目录按语言组织）。
fn collect_feedback_assets(req: &FeedbackRequest, action: &str) {
    if !crate::routes::l3::adopt_collect_enabled() || !matches!(req.lang.as_str(), "en" | "zh") {
        return;
    }
    let dir = crate::routes::l3::l3_data_dir();
    match action {
        "adopt" => {
            if let (Some(src), Some(dst)) = (req.src_text.as_deref(), req.corrected_text.as_deref())
            {
                if !src.trim().is_empty() && !dst.trim().is_empty() {
                    let added =
                        crate::routes::l3::collect_adopted(&dir, &req.lang, src, dst, "adopt");
                    if added > 0 {
                        tracing::info!(lang = %req.lang, added, "沉淀：采纳平行句对已汇入 adopted.jsonl");
                    }
                }
            }
        }
        "reject" => {
            if crate::routes::l3::collect_rejected(
                &dir,
                &req.lang,
                &req.error,
                req.reason.as_deref(),
            ) {
                tracing::info!(lang = %req.lang, "沉淀：放弃负偏好已汇入 rejected.jsonl");
            }
        }
        _ => {}
    }
}

/// 采纳分支分发（按来源层）
async fn adopt(state: &AppState, req: &FeedbackRequest) -> Result<FeedbackResponse, ApiError> {
    let source = gec_core::normalize_source(&req.error).to_string();
    let llm_reflect_ready = state.is_llm_available();
    match source.as_str() {
        // LLM 兜底层：复用现有采纳分支（anomaly → 模式蒸馏；其余 → 规则蒸馏 / 知识候选）
        sources::LLM => {
            if req.error.error_type == error_types::ANOMALY {
                let r = crate::routes::llm_feedback::anomaly_adopt(state, &req.lang, &req.error)
                    .await?;
                // 采纳留痕（anomaly 分支提前返回，须在此记录；失败请求不落日志）
                log_feedback(state, &req.lang, &source, &req.error, "adopt", &req.reason);
                return Ok(FeedbackResponse {
                    action: "adopt".to_string(),
                    message: r.message,
                    via: r.via,
                    rule: r.rule,
                    knowledge: r.knowledge,
                    anomaly: r.anomaly,
                    word_id: None,
                    reflection: None,
                    llm_reflect_ready,
                    agent: None,
                });
            }
            let r = crate::routes::llm_feedback::adopt_as_rule(state, &req.lang, &req.error, "LLM")
                .await?;
            // 采纳留痕（成功后记录；缺 correction 等失败请求不落日志）
            log_feedback(state, &req.lang, &source, &req.error, "adopt", &req.reason);
            // L3 飞轮正样本（确认错误句；非阻断）
            collect_l3(&req.lang, &req.error, 1, "llm_adopt");
            Ok(FeedbackResponse {
                action: "adopt".to_string(),
                message: r.message,
                via: r.via,
                rule: r.rule,
                knowledge: r.knowledge,
                anomaly: r.anomaly,
                word_id: None,
                reflection: None,
                llm_reflect_ready,
                agent: None,
            })
        }
        // 规则层：anomaly → 模式蒸馏；其余 → 确认规则正确（留痕 + 示例补强）
        sources::RULE => {
            if req.error.error_type == error_types::ANOMALY {
                let r = crate::routes::llm_feedback::anomaly_adopt(state, &req.lang, &req.error)
                    .await?;
                // 采纳留痕（anomaly 分支提前返回，须在此记录；失败请求不落日志）
                log_feedback(state, &req.lang, &source, &req.error, "adopt", &req.reason);
                return Ok(FeedbackResponse {
                    action: "adopt".to_string(),
                    message: r.message,
                    via: r.via,
                    rule: r.rule,
                    knowledge: r.knowledge,
                    anomaly: r.anomaly,
                    word_id: None,
                    reflection: None,
                    llm_reflect_ready,
                    agent: None,
                });
            }
            confirm_rule_adopt(state, req)
        }
        // 模型层（decision / feature / statistical）：蒸馏为规则 + L3 正样本
        sources::DECISION | sources::FEATURE | sources::STATISTICAL => {
            let r =
                crate::routes::llm_feedback::adopt_as_rule(state, &req.lang, &req.error, "MODEL")
                    .await?;
            // 采纳留痕（成功后记录；失败请求不落日志）
            log_feedback(state, &req.lang, &source, &req.error, "adopt", &req.reason);
            collect_l3(&req.lang, &req.error, 1, "model_adopt");
            Ok(FeedbackResponse {
                action: "adopt".to_string(),
                message: r.message,
                via: r.via,
                rule: r.rule,
                knowledge: r.knowledge,
                anomaly: r.anomaly,
                word_id: None,
                reflection: None,
                llm_reflect_ready,
                agent: None,
            })
        }
        // 知识层：确认知识条目正确（留痕）
        sources::KNOWLEDGE => {
            log_feedback(state, &req.lang, &source, &req.error, "adopt", &req.reason);
            Ok(FeedbackResponse {
                action: "adopt".to_string(),
                message: format!(
                    "已采纳：确认知识条目「{}」为真实错误（后续同类错误将继续报告）",
                    req.error.original
                ),
                via: Some("confirm".to_string()),
                rule: None,
                knowledge: None,
                anomaly: None,
                word_id: None,
                reflection: None,
                llm_reflect_ready,
                agent: None,
            })
        }
        _ => Err(ApiError::BadRequest(format!("未知来源层: {source}"))),
    }
}

/// 放弃分支：白名单止血 + 反馈留痕 + 反思诊断（+ 模型层 L3 负样本 + 自进化 agent）
async fn reject(
    state: &Arc<AppState>,
    req: &FeedbackRequest,
) -> Result<FeedbackResponse, ApiError> {
    let source = gec_core::normalize_source(&req.error).to_string();

    // ① 白名单：original 不再报告（所有来源层通用止血手段）
    let word = req.error.original.clone();
    let cw = state
        .custom_words
        .add(&req.lang, &word, "检测结果被用户放弃")
        .map_err(|e| ApiError::Internal(format!("白名单写入失败: {e}")))?;

    // ② 反馈留痕（反思流程的数据基础：规则误报率统计 / 反馈历史面板）
    log_feedback(state, &req.lang, &source, &req.error, "reject", &req.reason);

    // ③ 反思：按来源层诊断偏差来源，产出报告 + 可执行建议
    let ctx = build_reflection_context(state, &req.lang, &req.error, req.reason.as_deref());
    let report = gec_core::reflect_rejection(&ctx);

    // ④ 模型层偏差：自动汇入 L3 飞轮负样本（非阻断；en/zh 才有 L3 模型）
    if matches!(
        source.as_str(),
        sources::DECISION | sources::FEATURE | sources::STATISTICAL
    ) {
        collect_l3(&req.lang, &req.error, 0, "reject_model");
    }

    // ⑤ 自进化 agent：LLM 已配置时自主完成低风险处置并产出高风险提案
    //    （同步 + 超时保护；失败/未配置 → agent: None，行为与引入前一致）
    let (agent_available, agent_result, agent_error) =
        crate::routes::agent::run_rejection_agent(state, &req.lang, &req.error, &report.diagnosis)
            .await;
    if agent_available && agent_result.is_none() {
        tracing::warn!(
            error = %agent_error.unwrap_or_default(),
            "自进化 agent 运行失败（已回退确定性路径）"
        );
    }

    tracing::info!(
        lang = %req.lang,
        source = %source,
        subject = %report.subject,
        "检测结果被放弃：反思完成"
    );
    Ok(FeedbackResponse {
        action: "reject".to_string(),
        message: format!(
            "已放弃：「{word}」加入白名单；偏差诊断：{}",
            report.diagnosis
        ),
        via: None,
        rule: None,
        knowledge: None,
        anomaly: None,
        word_id: Some(cw.id),
        reflection: Some(report),
        llm_reflect_ready: state.is_llm_available(),
        agent: agent_result,
    })
}

/// 规则层采纳：确认规则正确（留痕 + 示例补强）
fn confirm_rule_adopt(
    state: &AppState,
    req: &FeedbackRequest,
) -> Result<FeedbackResponse, ApiError> {
    log_feedback(
        state,
        &req.lang,
        sources::RULE,
        &req.error,
        "adopt",
        &req.reason,
    );
    // 示例补强：有 correction 且该示例对不在规则中时补入（幂等）
    let mut reinforced = false;
    if let (Some(rule_id), Some(right)) = (&req.error.rule_id, req.error.correction.as_deref()) {
        if !right.trim().is_empty() {
            // 规则在词表文件中但不在 store（建库后才加入文件，播种只在库空时执行）：
            // 先按文件回填定义，防止 upsert 自动创建「默认停用」影子定义
            let lexicons_dir =
                std::path::PathBuf::from(&state.settings.read().unwrap().lexicons_dir);
            if let Err(e) =
                state
                    .lexicon_store
                    .backfill_rule_from_file(&lexicons_dir, &req.lang, rule_id)
            {
                tracing::warn!(
                    error = %e,
                    rule_id = %rule_id,
                    "从词表文件回填规则定义失败（继续默认流程）"
                );
            }
            if let Ok(true) = state.lexicon_store.upsert_example(
                &req.lang,
                rule_id,
                &req.error.original,
                right,
                "manual",
            ) {
                reinforced = true;
            }
        }
    }
    let rule_id = req
        .error
        .rule_id
        .clone()
        .unwrap_or_else(|| "（未入库规则）".to_string());
    Ok(FeedbackResponse {
        action: "adopt".to_string(),
        message: if reinforced {
            format!(
                "已采纳：确认规则 {rule_id} 正确，并补强示例「{}」→「{}」",
                req.error.original,
                req.error.correction.as_deref().unwrap_or("")
            )
        } else {
            format!("已采纳：确认规则 {rule_id} 正确（后续同类错误将继续报告）")
        },
        via: Some("confirm".to_string()),
        rule: Some(serde_json::json!({ "id": rule_id, "enabled": true })),
        knowledge: None,
        anomaly: None,
        word_id: None,
        reflection: None,
        llm_reflect_ready: state.is_llm_available(),
        agent: None,
    })
}

/// 写入反馈日志（失败仅告警，不阻断反馈主流程）
fn log_feedback(
    state: &AppState,
    lang: &str,
    source: &str,
    error: &Error,
    action: &str,
    reason: &Option<String>,
) {
    let rule_id = error.rule_id.as_deref();
    if let Err(e) = state.feedback_log.insert(
        lang,
        source,
        rule_id,
        &error.error_type,
        &error.original,
        error.correction.as_deref().unwrap_or(""),
        action,
        reason.as_deref().unwrap_or(""),
    ) {
        tracing::warn!(error = %e, "反馈日志写入失败（不阻断）");
    }
}

/// 组装反思上下文：按来源层从各存储定位具体对象
///
/// - 规则层：规则定义 + 该规则反馈统计 + 被放弃 original 是否正是某条示例；
/// - 知识层：按文本（精确 / 模糊）在知识存储中定位条目。
fn build_reflection_context<'a>(
    state: &AppState,
    lang: &str,
    error: &'a Error,
    reason: Option<&'a str>,
) -> gec_core::ReflectionContext<'a> {
    let source = gec_core::normalize_source(error);
    let mut rule_def: Option<gec_core::RuleDefInfo> = None;
    let mut rule_feedback: Option<(i64, i64)> = None;
    let mut matched_example: Option<gec_core::ExampleInfo> = None;
    let mut knowledge_item: Option<gec_core::KnowledgeInfo> = None;
    if source == sources::RULE {
        if let Some(rule_id) = &error.rule_id {
            if let Some(def) = state.lexicon_store.rule_defs_by_id(rule_id).ok().flatten() {
                let count = state
                    .lexicon_store
                    .list_rule_examples(lang, rule_id, 1, 1)
                    .map(|l| l.total)
                    .unwrap_or(0);
                rule_def = Some(gec_core::RuleDefInfo {
                    id: def.id,
                    category: def.category,
                    description: def.description,
                    enabled: def.enabled,
                    example_count: count,
                });
            }
            rule_feedback = state
                .feedback_log
                .stats_by_rule_distinct(lang, rule_id)
                .ok(); // 已含本次（log_feedback 先于本函数执行）；按 original 去重
            matched_example = find_matched_example(state, lang, rule_id, &error.original);
        }
    } else if source == sources::KNOWLEDGE {
        knowledge_item = find_knowledge_item(state, lang, error);
    }
    gec_core::ReflectionContext {
        error,
        reason,
        rule_def,
        rule_feedback,
        matched_example,
        knowledge_item,
    }
}

/// 规则示例定位：被放弃的 original 是否正是该规则的某条示例
fn find_matched_example(
    state: &AppState,
    lang: &str,
    rule_id: &str,
    original: &str,
) -> Option<gec_core::ExampleInfo> {
    let list = state
        .lexicon_store
        .list_rule_examples(lang, rule_id, 1, 200)
        .ok()?;
    list.examples
        .into_iter()
        .find(|ex| ex.wrong.trim() == original.trim())
        .map(|ex| gec_core::ExampleInfo {
            wrong: ex.wrong,
            right: ex.right,
            source: ex.source,
        })
}

/// 知识条目定位：先精确（original 即条目文本），再模糊（bigram 相似度 ≥ 0.8）
fn find_knowledge_item(
    state: &AppState,
    lang: &str,
    error: &Error,
) -> Option<gec_core::KnowledgeInfo> {
    let items = state.learned_knowledge.list(lang).ok()?;
    // ① 精确：条目文本 == original
    if let Some(it) = items
        .iter()
        .find(|it| it.text.trim() == error.original.trim())
    {
        return Some(gec_core::KnowledgeInfo {
            id: it.id,
            text: it.text.clone(),
            error_type: it.error_type.clone(),
            correction: it.correction.clone(),
            source: it.source.clone(),
        });
    }
    // ② 模糊：条目文本与 original 的 bigram 相似度最高者
    let mut best: Option<(i64, f64)> = None;
    for it in &items {
        let sim = gec_core::fuzzy_similarity(&it.text, &error.original);
        if sim >= gec_core::knowledge_recall::FUZZY_THRESHOLD
            && best.as_ref().is_none_or(|(_, b)| sim > *b)
        {
            best = Some((it.id, sim));
        }
    }
    let (id, _) = best?;
    items
        .iter()
        .find(|it| it.id == id)
        .map(|it| gec_core::KnowledgeInfo {
            id: it.id,
            text: it.text.clone(),
            error_type: it.error_type.clone(),
            correction: it.correction.clone(),
            source: it.source.clone(),
        })
}

/// L3 飞轮样本采集（非阻断：目录不可写 / 语言不支持时静默跳过）
fn collect_l3(lang: &str, error: &Error, label: u8, source: &str) {
    if !matches!(lang, "en" | "zh") {
        return;
    }
    let dir = crate::routes::l3::l3_data_dir();
    let sample = serde_json::json!({
        "text": error.original,
        "label": label,
        "error_type": if label == 1 { error.error_type.to_string() } else { String::new() },
        "source": source,
        "ts": chrono::Utc::now().to_rfc3339(),
    });
    if let Some(total) = crate::routes::l3::collect_sample(&dir, lang, &sample) {
        tracing::info!(lang, label, total, "L3 飞轮：反馈样本已汇入");
    }
}

/// 反思建议执行请求
#[derive(Deserialize)]
pub struct FeedbackActionRequest {
    /// 建议类型：`disable_rule` / `exclude_word` / `remove_example` / `remove_knowledge` / `l3_negative`
    pub action: String,
    /// 执行参数（按 action 解释）
    #[serde(default)]
    pub payload: serde_json::Value,
}

/// 反思建议执行结果
#[derive(Serialize)]
pub struct FeedbackActionResponse {
    pub ok: bool,
    pub message: String,
}

/// POST /gec/feedback/action — 执行反思建议（前端一键按钮）
pub async fn feedback_action(
    State(state): State<Arc<AppState>>,
    Json(req): Json<FeedbackActionRequest>,
) -> Result<Json<FeedbackActionResponse>, ApiError> {
    let p = &req.payload;
    let get = |k: &str| p.get(k).and_then(|v| v.as_str()).unwrap_or("");
    match req.action.as_str() {
        // 停用规则（payload: {lang, rule_id}）
        "disable_rule" => {
            let (lang, rule_id) = (get("lang"), get("rule_id"));
            if lang.is_empty() || rule_id.is_empty() {
                return Err(ApiError::BadRequest(
                    "disable_rule 需要 lang + rule_id".into(),
                ));
            }
            state
                .lexicon_store
                .toggle_rule(lang, rule_id, false)
                .map_err(|e| ApiError::Internal(e.to_string()))?;
            Ok(Json(FeedbackActionResponse {
                ok: true,
                message: format!("规则 {rule_id} 已停用（可在规则面板重新启用）"),
            }))
        }
        // 删除规则示例（payload: {lang, rule_id, wrong, right}）
        "remove_example" => {
            let (lang, rule_id, wrong, right) =
                (get("lang"), get("rule_id"), get("wrong"), get("right"));
            if lang.is_empty() || rule_id.is_empty() || wrong.is_empty() {
                return Err(ApiError::BadRequest(
                    "remove_example 需要 lang + rule_id + wrong".into(),
                ));
            }
            let deleted = state
                .lexicon_store
                .delete_example(lang, rule_id, wrong, right)
                .map_err(|e| ApiError::Internal(e.to_string()))?;
            Ok(Json(FeedbackActionResponse {
                ok: deleted,
                message: if deleted {
                    format!("示例「{wrong}」已从规则 {rule_id} 删除")
                } else {
                    format!("示例「{wrong}」不在规则 {rule_id} 中（可能已被删除）")
                },
            }))
        }
        // 删除知识条目（payload: {id, text}）
        "remove_knowledge" => {
            let id = p.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
            if id <= 0 {
                return Err(ApiError::BadRequest("remove_knowledge 需要 id".into()));
            }
            let removed = state
                .learned_knowledge
                .remove(id)
                .map_err(|e| ApiError::Internal(e.to_string()))?;
            Ok(Json(FeedbackActionResponse {
                ok: removed,
                message: if removed {
                    format!("知识条目 #{id} 已删除（后续不再召回）")
                } else {
                    format!("知识条目 #{id} 不存在（可能已被删除）")
                },
            }))
        }
        // L3 飞轮负样本（payload: {lang, text, error_type}）
        "l3_negative" => {
            let (lang, text) = (get("lang"), get("text"));
            if lang.is_empty() || text.is_empty() {
                return Err(ApiError::BadRequest("l3_negative 需要 lang + text".into()));
            }
            if !matches!(lang, "en" | "zh") {
                return Err(ApiError::BadRequest(format!("L3 飞轮暂不支持语言: {lang}")));
            }
            let error_type = get("error_type");
            let dir = crate::routes::l3::l3_data_dir();
            let sample = serde_json::json!({
                "text": text,
                "label": 0,
                "error_type": error_type,
                "source": "reflect_negative",
                "ts": chrono::Utc::now().to_rfc3339(),
            });
            let added = crate::routes::l3::collect_sample(&dir, lang, &sample).is_some();
            Ok(Json(FeedbackActionResponse {
                ok: true,
                message: if added {
                    format!("「{text}」已汇入 L3 飞轮负样本（积累后重训练可系统性修正）")
                } else {
                    format!("「{text}」已在 L3 飞轮负样本中（去重）")
                },
            }))
        }
        // 规则排除词（payload: {lang, rule_id, word}）：写回词表 rules[].excludes
        // + 重建检测器——规则保持启用，仅该词不再触发（POS 误标等单点误报的精准对策）
        "exclude_word" => {
            let (lang, rule_id, word) = (get("lang"), get("rule_id"), get("word"));
            if lang.is_empty() || rule_id.is_empty() || word.is_empty() {
                return Err(ApiError::BadRequest(
                    "exclude_word 需要 lang + rule_id + word".into(),
                ));
            }
            let settings = state.settings.read().unwrap().clone();
            let lexicons_dir = std::path::PathBuf::from(&settings.lexicons_dir);
            let added = lexicon_store::append_rule_exclude(&lexicons_dir, lang, rule_id, word)
                .map_err(|e| ApiError::Internal(e.to_string()))?;
            if added {
                // 检测器只在构造时加载词表 → 重建即时生效（与采纳路径同一机制）
                state.rebuild_router(&settings);
            }
            Ok(Json(FeedbackActionResponse {
                ok: true,
                message: if added {
                    format!(
                        "「{word}」已加入规则 {rule_id} 排除名单（该词不再触发此规则，检测器已重建）"
                    )
                } else {
                    format!("「{word}」已在规则 {rule_id} 排除名单中")
                },
            }))
        }
        other => Err(ApiError::BadRequest(format!("未知操作: {other}"))),
    }
}

/// LLM 深度反思请求
#[derive(Deserialize)]
pub struct ReflectLlmRequest {
    pub lang: String,
    /// 被放弃的错误
    pub error: Error,
    /// 用户放弃原因（可选）
    #[serde(default)]
    pub reason: Option<String>,
}

/// LLM 深度反思结果
#[derive(Serialize)]
pub struct ReflectLlmResponse {
    /// 自然语言诊断（偏差来源 + 建议）
    pub llm_analysis: String,
    /// 使用的模型
    pub model: String,
    /// 使用的提示词模板键（如 zh_reflect）
    pub template: String,
}

/// POST /gec/feedback/reflect-llm — LLM 深度反思
///
/// 与确定性反思（`/gec/feedback` 的 reject 分支）分离：确定性报告即时返回，
/// LLM 分析是独立的可选请求（模板 `prompts/{lang}_reflect_prompt.json`），
/// 避免放弃操作被 LLM 超时阻塞。LLM 未配置 / 无模板时返回 503 / 404。
pub async fn reflect_llm(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ReflectLlmRequest>,
) -> Result<Json<ReflectLlmResponse>, ApiError> {
    let llm = state.llm_service();
    if !llm.is_available() {
        return Err(ApiError::ServiceUnavailable(
            "LLM 服务未启用（请设置 LLM_API_KEY 或在「设置」界面配置 LLM 参数）".into(),
        ));
    }
    let template = llm
        .templates
        .for_lang_cefr(&req.lang, "reflect")
        .ok_or_else(|| {
            ApiError::NotFound(format!(
                "语言 {} 无反思模板（prompts/{{lang}}_reflect_prompt.json）",
                req.lang
            ))
        })?;
    let source = gec_core::normalize_source(&req.error);
    let reason = req.reason.as_deref().unwrap_or("");
    let user = template.render(&[
        ("lang", &req.lang),
        ("source", source),
        ("error_type", &req.error.error_type),
        ("original", &req.error.original),
        (
            "correction",
            req.error.correction.as_deref().unwrap_or("（无修正）"),
        ),
        ("rule_id", req.error.rule_id.as_deref().unwrap_or("（无）")),
        (
            "reason",
            if reason.is_empty() {
                "（用户未说明）"
            } else {
                reason
            },
        ),
    ]);
    let analysis = llm
        .complete(&template.system, &user)
        .await
        .map_err(|e| ApiError::Upstream(e.to_string()))?;
    let template_key = format!("{}_reflect", req.lang);
    Ok(Json(ReflectLlmResponse {
        llm_analysis: analysis,
        model: llm.config.model,
        template: template_key,
    }))
}

/// 反馈历史响应（设置界面「反馈历史」面板）
#[derive(Serialize)]
pub struct FeedbackHistoryResponse {
    pub lang: String,
    /// 按（来源层, 规则 id）聚合的统计（拒绝数降序）
    pub stats: Vec<lexicon_store::FeedbackStats>,
    /// 最近反馈明细
    pub recent: Vec<lexicon_store::FeedbackLog>,
}

/// GET /gec/feedback/history?lang=&limit= — 反馈历史
pub async fn feedback_history(
    State(state): State<Arc<AppState>>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<FeedbackHistoryResponse>, ApiError> {
    let lang = q.get("lang").cloned().unwrap_or_else(|| "zh".to_string());
    let limit = q
        .get("limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(50);
    let stats = state
        .feedback_log
        .stats(&lang)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let recent = state
        .feedback_log
        .list(Some(&lang), limit)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(FeedbackHistoryResponse {
        lang,
        stats,
        recent,
    }))
}
