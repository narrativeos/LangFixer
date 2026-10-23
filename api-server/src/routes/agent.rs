//! 自进化 agent 路由
//!
//! - `POST /gec/agent/run`：手动触发一次 agent run（给定任务描述）
//! - [`run_rejection_agent`]：`/gec/feedback` reject 分支的自进化入口——
//!   从拒绝上下文构建任务，LLM 自主完成低风险处置并产出高风险提案
//!
//! agent 不可用（LLM 未配置 / 该语言无 agent 模板）时返回 `agent_available: false`，
//! 调用方行为与未引入 agent 前完全一致（确定性路径不受影响）。

use std::sync::Arc;

use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};

use gec_core::{
    ActionTier, AgentRunResult, LlmDetectConfig, agent_timeout, build_rejection_task, run_agent,
};
use shared_model::Error as DetectedError;

use crate::agent_tools::AgentToolCtx;
use crate::state::AppState;

use super::ApiError;

#[derive(Deserialize)]
pub struct AgentRunRequest {
    pub lang: String,
    /// 任务描述（自然语言）
    pub task: String,
    /// 关联错误（审计与上下文用）
    #[serde(default)]
    pub error: Option<DetectedError>,
}

#[derive(Serialize)]
pub struct AgentRunResponse {
    /// agent 是否可用（LLM 已配置 + 该语言有 agent 模板）
    pub agent_available: bool,
    /// 运行结果（None = 不可用 / 运行失败）
    pub result: Option<AgentRunResult>,
    /// 失败原因（result 为 None 且 agent_available 为 true 时）
    pub error: Option<String>,
}

/// 运行一次 agent：构建工具集 + agent 模板 + LLM 配置，跑完循环并汇总。
/// 返回 `(agent_available, result, error)`。
pub async fn run_agent_once(
    state: &Arc<AppState>,
    lang: &str,
    task: &str,
    error: Option<&DetectedError>,
) -> (bool, Option<AgentRunResult>, Option<String>) {
    let service = state.llm_service();
    let Some(template) = service.templates.for_lang_cefr(lang, "agent") else {
        return (false, None, None);
    };
    if !service.config.enabled() {
        return (false, None, None);
    }
    let ctx = AgentToolCtx::new(state, lang);
    let tools = ctx.build_tools();
    let input = template.render(&[("task", task)]);
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
        Ok(Ok(result)) => {
            audit_agent_result(state, lang, &result, error);
            (true, Some(result), None)
        }
        Ok(Err(e)) => (true, None, Some(e.to_string())),
        Err(_) => (true, None, Some("agent 运行超时".into())),
    }
}

/// 审计：把 agent 执行 / 提案的动作记入反馈日志（`source="agent"`）
fn audit_agent_result(
    state: &Arc<AppState>,
    lang: &str,
    result: &AgentRunResult,
    error: Option<&DetectedError>,
) {
    let rule_id = error.and_then(|e| e.rule_id.as_deref());
    let error_type = error.map(|e| e.error_type.clone()).unwrap_or_default();
    let original = error.map(|e| e.original.clone()).unwrap_or_default();
    let correction = error.and_then(|e| e.correction.clone()).unwrap_or_default();
    for a in &result.executed {
        let _ = state.feedback_log.insert(
            lang,
            "agent",
            rule_id,
            &error_type,
            &original,
            &correction,
            &format!("agent:{a_tool}", a_tool = a.tool),
            &a.params.to_string(),
        );
    }
    for p in &result.proposals {
        let _ = state.feedback_log.insert(
            lang,
            "agent",
            rule_id,
            &error_type,
            &original,
            &correction,
            &format!("agent_proposal:{p_tool}", p_tool = p.tool),
            &p.params.to_string(),
        );
    }
}

/// reject 分支的自进化：从拒绝上下文构建任务并运行 agent。
/// 诊断文本（确定性反思报告）作为任务的一部分，帮助模型判断偏差来源。
pub async fn run_rejection_agent(
    state: &Arc<AppState>,
    lang: &str,
    error: &DetectedError,
    diagnosis: &str,
) -> (bool, Option<AgentRunResult>, Option<String>) {
    let source = error.source.as_deref().unwrap_or("未知");
    let rule = error.rule_id.as_deref().unwrap_or("（无关联规则）");
    let correction = error.correction.as_deref().unwrap_or("（无）");
    let task = build_rejection_task(
        lang,
        &error.original,
        &error.error_type,
        source,
        rule,
        correction,
        diagnosis,
    );
    run_agent_once(state, lang, &task, Some(error)).await
}

/// POST /gec/agent/confirm — 用户确认后执行 agent 提案的高风险动作
///
/// 与 gate 的对应关系：agent 循环里 Confirm 层级工具被 `TierGate` 拦截转提案
/// （`AgentRunResult.proposals`）；用户在 UI 上确认后调本端点，经**同一执行器**
/// 真实执行（确认即授权，绕过 gate）。
///
/// 安全约束：
/// - 仅接受 **Confirm 层级**工具（`disable_rule` / `enable_rule` /
///   `remove_example` / `remove_knowledge`）；Autonomous 工具在 agent 循环内
///   已直接执行，不接受二次提交（防重放）；
/// - 执行审计记入反馈日志（`source="agent_confirm"`），与 agent 自主动作
///   （`source="agent"`）区分。
#[derive(Deserialize)]
pub struct AgentConfirmRequest {
    pub lang: String,
    /// 提案的工具名（`AgentRunResult.proposals[].tool`）
    pub tool: String,
    /// 提案的完整参数（`AgentRunResult.proposals[].params`，原样回传）
    #[serde(default)]
    pub params: serde_json::Value,
    /// 关联错误（审计用，可选）
    #[serde(default)]
    pub error: Option<DetectedError>,
}

#[derive(Serialize)]
pub struct AgentConfirmResponse {
    pub ok: bool,
    pub message: String,
}

pub async fn agent_confirm(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AgentConfirmRequest>,
) -> Result<Json<AgentConfirmResponse>, ApiError> {
    if req.tool.trim().is_empty() {
        return Err(ApiError::BadRequest("tool 不能为空".into()));
    }
    let ctx = AgentToolCtx::new(&state, &req.lang);
    let tools = ctx.build_tools();
    let tool = tools
        .iter()
        .find(|t| t.name == req.tool)
        .ok_or_else(|| ApiError::BadRequest(format!("未知 agent 工具: {}", req.tool)))?;
    if tool.tier != ActionTier::Confirm {
        return Err(ApiError::BadRequest(format!(
            "工具 {} 不是 Confirm 层级（自主动作在 agent 循环内已执行，不可二次提交）",
            req.tool
        )));
    }
    let params = if req.params.is_null() {
        serde_json::Value::Object(serde_json::Map::new())
    } else {
        req.params
    };
    let result = tool
        .executor
        .execute(params.clone())
        .await
        .map_err(|e| ApiError::Internal(e))?;
    // 审计：确认执行的高风险动作记入反馈日志
    let error = req.error.as_ref();
    let rule_id = error.and_then(|e| e.rule_id.as_deref());
    let error_type = error.map(|e| e.error_type.clone()).unwrap_or_default();
    let original = error.map(|e| e.original.clone()).unwrap_or_default();
    let correction = error.and_then(|e| e.correction.clone()).unwrap_or_default();
    let _ = state.feedback_log.insert(
        &req.lang,
        "agent_confirm",
        rule_id,
        &error_type,
        &original,
        &correction,
        &format!("agent_confirm:{}", req.tool),
        &params.to_string(),
    );
    Ok(Json(AgentConfirmResponse {
        ok: true,
        message: result,
    }))
}

/// POST /gec/agent/run — 手动触发一次 agent run
pub async fn agent_run(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AgentRunRequest>,
) -> Result<Json<AgentRunResponse>, ApiError> {
    if req.task.trim().is_empty() {
        return Err(ApiError::BadRequest("task 不能为空".into()));
    }
    let (agent_available, result, error) =
        run_agent_once(&state, &req.lang, &req.task, req.error.as_ref()).await;
    Ok(Json(AgentRunResponse {
        agent_available,
        result,
        error,
    }))
}
