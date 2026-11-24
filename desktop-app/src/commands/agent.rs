use std::sync::Arc;

use gec_core::{ActionTier, LlmDetectConfig};

use super::core::Core;
/// 运行一次自进化 agent（桌面端镜像；与 API 端 `/gec/agent/run` 同构）
///
/// 返回 `(agent_available, result, error)`：LLM 未配置 / 该语言无 agent 模板时
/// `agent_available = false`，行为与未引入 agent 前完全一致。
pub(crate) async fn run_agent_cmd(
    core: &Arc<Core>,
    lang: &str,
    task: &str,
    error: Option<&shared_model::Error>,
) -> (bool, Option<gec_core::AgentRunResult>, Option<String>) {
    let service = core.llm.read().unwrap().clone();
    let Some(template) = service.templates.for_lang_cefr(lang, "agent") else {
        return (false, None, None);
    };
    if !service.config.enabled() {
        return (false, None, None);
    }
    let ctx = crate::agent_tools::AgentToolCtx::new(core, lang);
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
        gec_core::agent_timeout(),
        gec_core::run_agent(&config, &template.system, &input, tools),
    )
    .await
    {
        Ok(Ok(result)) => {
            // 审计：执行 / 提案动作记入反馈日志（source="agent"）
            let rule_id = error.and_then(|e| e.rule_id.as_deref());
            let error_type = error.map(|e| e.error_type.clone()).unwrap_or_default();
            let original = error.map(|e| e.original.clone()).unwrap_or_default();
            let correction = error.and_then(|e| e.correction.clone()).unwrap_or_default();
            for a in &result.executed {
                let _ = core.feedback_log.insert(
                    lang,
                    "agent",
                    rule_id,
                    &error_type,
                    &original,
                    &correction,
                    &format!("agent:{}", a.tool),
                    &a.params.to_string(),
                );
            }
            for p in &result.proposals {
                let _ = core.feedback_log.insert(
                    lang,
                    "agent",
                    rule_id,
                    &error_type,
                    &original,
                    &correction,
                    &format!("agent_proposal:{}", p.tool),
                    &p.params.to_string(),
                );
            }
            (true, Some(result), None)
        }
        Ok(Err(e)) => (true, None, Some(e.to_string())),
        Err(_) => (true, None, Some("agent 运行超时".into())),
    }
}

/// POST 等价：手动触发一次 agent run（前端「让 agent 处理」入口）
#[tauri::command]
pub async fn feedback_agent(
    core: tauri::State<'_, Arc<Core>>,
    lang: String,
    task: String,
    error: Option<shared_model::Error>,
) -> Result<serde_json::Value, String> {
    if task.trim().is_empty() {
        return Err("task 不能为空".into());
    }
    let (agent_available, result, err) =
        run_agent_cmd(core.inner(), &lang, &task, error.as_ref()).await;
    Ok(serde_json::json!({
        "agent_available": agent_available,
        "result": result,
        "error": err,
    }))
}

/// POST 等价：用户确认后执行 agent 提案的高风险动作（与 API 端 `/gec/agent/confirm` 同构）
///
/// 安全约束同 API 端：仅接受 Confirm 层级工具；Autonomous 工具在 agent 循环内
/// 已直接执行，不接受二次提交（防重放）。审计记入反馈日志（`source="agent_confirm"`）。
#[tauri::command]
pub async fn confirm_agent_proposal(
    core: tauri::State<'_, Arc<Core>>,
    lang: String,
    tool: String,
    params: Option<serde_json::Value>,
    error: Option<shared_model::Error>,
) -> Result<serde_json::Value, String> {
    if tool.trim().is_empty() {
        return Err("tool 不能为空".into());
    }
    let ctx = crate::agent_tools::AgentToolCtx::new(core.inner(), &lang);
    let tools = ctx.build_tools();
    let mt = tools
        .iter()
        .find(|t| t.name == tool)
        .ok_or_else(|| format!("未知 agent 工具: {}", tool))?;
    if mt.tier != ActionTier::Confirm {
        return Err(format!(
            "工具 {} 不是 Confirm 层级（自主动作在 agent 循环内已执行，不可二次提交）",
            tool
        ));
    }
    let params = match params {
        Some(p) if !p.is_null() => p,
        _ => serde_json::Value::Object(serde_json::Map::new()),
    };
    let message = mt.executor.execute(params.clone()).await?;
    // 审计：确认执行的高风险动作记入反馈日志
    let rule_id = error.as_ref().and_then(|e| e.rule_id.as_deref());
    let error_type = error
        .as_ref()
        .map(|e| e.error_type.clone())
        .unwrap_or_default();
    let original = error
        .as_ref()
        .map(|e| e.original.clone())
        .unwrap_or_default();
    let correction = error
        .as_ref()
        .and_then(|e| e.correction.clone())
        .unwrap_or_default();
    let _ = core.feedback_log.insert(
        &lang,
        "agent_confirm",
        rule_id,
        &error_type,
        &original,
        &correction,
        &format!("agent_confirm:{}", tool),
        &params.to_string(),
    );
    Ok(serde_json::json!({ "ok": true, "message": message }))
}
