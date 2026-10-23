//! LLM 自进化 agent（基于 yoagent 循环）
//!
//! 「自进化」的执行层：用户拒绝某条错误后，agent 可自主完成一串**低风险**动作
//! （加 exclude、L3 反例、知识库条目等），对**高风险**动作（禁用规则、删除条目等）
//! 只产出提案列表，由人确认后执行。
//!
//! 架构（对应 smolagents `ToolCallingAgent` 模式，循环由 yoagent 实现）：
//! - [`ManagedTool`]：工具定义（name / description / JSON Schema / 授权层级 / 执行器）
//! - [`TierGate`]：`ToolMiddleware`，层级判定的唯一执行点——Confirm 级一律 Deny，
//!   拒绝原因作为 tool result 回给模型（模型据此把动作记入最终答案的 proposals 段），
//!   循环继续；Deny 不消耗额外模型轮次
//! - [`run_agent`]：构建 yoagent `Agent`，跑完循环，把事件流汇总成 [`AgentRunResult`]
//!
//! 授权层级：
//! - [`ActionTier::Autonomous`]——可逆 / 增量操作，agent 直接执行
//! - [`ActionTier::Confirm`]——破坏性操作，gate 拦截转提案，用户点击执行
//!
//! 循环健壮性（流中断、工具 panic、死循环、限额、成本核算）由 yoagent 承担；
//! 本模块只负责工具面、授权策略与结果汇总。

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use yoagent::context::ExecutionLimits;
use yoagent::provider::ModelConfig;
use yoagent::{
    Agent, AgentEvent, AgentMessage, AgentTool, Content, Message, StopReason, ToolCallRequest,
    ToolContext, ToolDecision, ToolError, ToolMiddleware, ToolResult,
};

use crate::llm::LlmDetectConfig;

// ---------------------------------------------------------------------------
// 授权层级
// ---------------------------------------------------------------------------

/// 工具授权层级。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionTier {
    /// 可逆 / 增量操作：agent 直接执行（excludes、L3 反例、知识库等）
    Autonomous,
    /// 破坏性操作：gate 拦截转提案，用户点击执行（禁用规则、删除条目等）
    Confirm,
}

/// gate 拒绝消息的稳定前缀：事件汇总据此区分「工具失败」与「层级拦截」。
pub const DENY_PREFIX: &str = "[confirm-required]";

// ---------------------------------------------------------------------------
// 工具
// ---------------------------------------------------------------------------

/// 工具执行器：执行真实动作，返回简短的人读结果文本。
///
/// 由宿主（api-server / 桌面端）实现，闭包捕获 lexicon-store 等依赖。
#[async_trait]
pub trait AgentToolExecutor: Send + Sync {
    /// 执行动作。`Ok(文本)` 回给模型作为 tool result；`Err` 同样回给模型
    /// （模型可换策略），但不终止循环。
    async fn execute(&self, params: Value) -> Result<String, String>;
}

/// 受管工具：定义 + 层级 + 执行器。
pub struct ManagedTool {
    pub name: String,
    pub description: String,
    /// 参数 JSON Schema（`{"type":"object","properties":{...}}`）
    pub parameters: Value,
    pub tier: ActionTier,
    pub executor: Arc<dyn AgentToolExecutor>,
}

impl ManagedTool {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: Value,
        tier: ActionTier,
        executor: Arc<dyn AgentToolExecutor>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
            tier,
            executor,
        }
    }
}

#[async_trait]
impl AgentTool for ManagedTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn label(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn parameters_schema(&self) -> Value {
        self.parameters.clone()
    }
    async fn execute(&self, params: Value, _ctx: ToolContext) -> Result<ToolResult, ToolError> {
        // 防御性归一化：真实 provider（OpenAI 兼容 / mock）在 finalize 阶段已把
        // LLM 的 arguments 解析为对象（截断/坏 JSON 在 middleware 之前被拒）；
        // 此处兜底「字符串参数」形态，保证宿主执行器永远拿到对象。
        let params = match &params {
            Value::String(raw) => {
                serde_json::from_str::<Value>(raw).unwrap_or(Value::Object(serde_json::Map::new()))
            }
            other => other.clone(),
        };
        match self.executor.execute(params).await {
            Ok(text) => Ok(ToolResult {
                content: vec![Content::Text { text }],
                details: Value::Null,
            }),
            Err(e) => Err(ToolError::Failed(e)),
        }
    }
}

// ---------------------------------------------------------------------------
// 层级 gate（ToolMiddleware）
// ---------------------------------------------------------------------------

/// 层级判定的唯一执行点。
///
/// - Autonomous（或未知工具名）→ `Allow`
/// - Confirm → `Deny`：原因回给模型（模型把动作记入最终答案的 proposals 段），
///   循环继续；同时记入内部 `denied` 列表，供 [`AgentRunResult::proposals`]
pub struct TierGate {
    tiers: HashMap<String, ActionTier>,
    /// 提案存储共享在 Arc 后：`with_tool_middleware` 按值拿走 middleware 实例，
    /// 结果汇总方持另一个共享同一存储的实例读取
    denied: Arc<Mutex<Vec<ProposedAction>>>,
}

impl TierGate {
    pub fn new(tiers: HashMap<String, ActionTier>) -> Self {
        Self {
            tiers,
            denied: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// 与既有 gate 共享提案存储（`run_agent` 内部使用）。
    pub fn with_shared_denied(
        tiers: HashMap<String, ActionTier>,
        denied: Arc<Mutex<Vec<ProposedAction>>>,
    ) -> Self {
        Self { tiers, denied }
    }

    /// 取出本次 run 被拦截的提案（run 结束后调用）。
    pub async fn take_denied(&self) -> Vec<ProposedAction> {
        std::mem::take(&mut *self.denied.lock().await)
    }
}

#[async_trait::async_trait]
impl ToolMiddleware for TierGate {
    async fn before_tool(&self, call: &ToolCallRequest<'_>) -> ToolDecision {
        match self
            .tiers
            .get(call.tool_name)
            .copied()
            .unwrap_or(ActionTier::Autonomous)
        {
            ActionTier::Autonomous => ToolDecision::Allow,
            ActionTier::Confirm => {
                // 给模型的指令：把该动作原样记入最终答案的 proposals 段
                let instruction = format!(
                    "Action `{}` requires user confirmation and was NOT executed. \
                     Record it verbatim in the `proposals` section of your final answer \
                     (tool name and full arguments), then continue or finish.",
                    call.tool_name
                );
                // 提案的 reason 是**用户可见**文案（前端确认对话框展示）；
                // 上面的模型指令只走 tool result（Deny 原因），不进提案。
                let action = ProposedAction {
                    tool: call.tool_name.to_string(),
                    params: call.args.clone(),
                    reason: "高风险动作：agent 建议执行此操作，需用户确认后生效".to_string(),
                };
                self.denied.lock().await.push(action);
                ToolDecision::Deny(format!("{DENY_PREFIX} {instruction}"))
            }
        }
    }
}
// ---------------------------------------------------------------------------
// 结果
// ---------------------------------------------------------------------------

/// agent 实际执行的一个自主动作。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutedAction {
    pub tool: String,
    pub params: Value,
    /// 执行器返回的结果文本
    pub result: String,
}

/// agent 提案的高风险动作（gate 拦截，待用户确认）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposedAction {
    pub tool: String,
    pub params: Value,
    pub reason: String,
}

/// agent 执行轨迹的一步（UI 展示与审计）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub struct TraceStep {
    pub kind: TraceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub detail: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceKind {
    /// 模型发起工具调用
    ToolCall,
    /// 工具执行完成（含失败）
    ToolResult,
    /// 工具调用被 gate 拦截（转提案）
    ToolDenied,
    /// 模型文本输出轮
    ModelTurn,
    /// run 结束
    Ended,
}

/// 一次 agent run 的汇总结果。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentRunResult {
    /// 模型最终总结文本（展示给用户；失败时为空）
    pub summary: String,
    /// 实际执行的自主动作
    pub executed: Vec<ExecutedAction>,
    /// agent 提案的高风险动作（待用户确认）
    pub proposals: Vec<ProposedAction>,
    /// 执行轨迹
    pub trace: Vec<TraceStep>,
    /// 是否正常结束（false = 模型错误 / 限额 / 取消）
    pub completed: bool,
}

/// agent 运行错误。
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("agent run did not complete: {0}")]
    Incomplete(String),
}

/// agent 运行超时（秒；`GEC_AGENT_TIMEOUT_SECS` 可覆盖，API 端与桌面端共用）
pub fn agent_timeout() -> std::time::Duration {
    std::env::var("GEC_AGENT_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .map(std::time::Duration::from_secs)
        .unwrap_or(std::time::Duration::from_secs(60))
}

/// 构建 reject 分支自进化 agent 的任务描述（API 端与桌面端共用，避免双份维护）。
///
/// L3 飞轮仅支持 en/zh：其他语言的 agent 调 `add_l3_negative` 会被执行器拒绝，
/// 故任务文本对非 en/zh 语言不提该选项。
pub fn build_rejection_task(
    lang: &str,
    original: &str,
    error_type: &str,
    source: &str,
    rule: &str,
    correction: &str,
    diagnosis: &str,
) -> String {
    let l3_line = if matches!(lang, "en" | "zh") {
        "- 模型层（decision / feature / statistical）误报：可汇入 L3 负样本（add_l3_negative）；\n"
    } else {
        ""
    };
    format!(
        "用户拒绝了以下检测结果：原文片段「{original}」（错误类型: {error_type}，来源层: {source}，规则: {rule}，修正建议: {correction}）。\
         反思诊断：{diagnosis}。\n\
         请分析误报原因并采取适当行动：\n\
         - 单点误报（如某个词被规则误触发）：可把该词加入规则排除名单（add_exclude_word）；\
         先查 feedback_stats / get_rule 确认该规则的历史误报情况；\n\
         {l3_line}\
         - 规则误报率显著偏高：可提案停用该规则（disable_rule，需用户确认）；\n\
         - 发现值得沉淀的误报模式：可添加知识条目（add_knowledge）。",
    )
}

// ---------------------------------------------------------------------------
// 运行器
// ---------------------------------------------------------------------------

/// 运行一次 agent 循环至结束并汇总结果。
///
/// - `config`：LLM 端点（与检测层同源，只用 base_url / api_key / model）
/// - `system_prompt`：agent 系统提示词（模板库 `prompts/{lang}_agent_prompt.json`）
/// - `input`：本次任务描述（如「用户拒绝了以下错误：…」）
/// - `tools`：本次可用工具（gate 由它们的层级构建）
pub async fn run_agent(
    config: &LlmDetectConfig,
    system_prompt: &str,
    input: &str,
    tools: Vec<ManagedTool>,
) -> Result<AgentRunResult, AgentError> {
    let tiers: HashMap<String, ActionTier> =
        tools.iter().map(|t| (t.name.clone(), t.tier)).collect();
    let denied: Arc<Mutex<Vec<ProposedAction>>> = Arc::new(Mutex::new(Vec::new()));
    let gate = TierGate::with_shared_denied(tiers.clone(), denied);

    let model = ModelConfig::local(config.base_url.trim_end_matches('/'), &config.model);
    let mut agent = Agent::from_config(model)
        .with_api_key(&config.api_key)
        .with_system_prompt(system_prompt)
        .with_tools(
            tools
                .into_iter()
                .map(|t| -> Box<dyn AgentTool> { Box::new(t) })
                .collect(),
        )
        .with_tool_middleware(TierGate::with_shared_denied(tiers, gate.denied.clone()))
        .with_execution_limits(ExecutionLimits::default().with_max_turns(6))
        .with_temperature(0.0);

    let mut events = agent.prompt(input).await;

    let mut result = AgentRunResult::default();
    // tool_call_id -> (name, args)，ToolExecutionEnd 时取回
    let mut pending: HashMap<String, (String, Value)> = HashMap::new();
    let mut ended_ok = false;

    while let Some(event) = events.recv().await {
        match &event {
            AgentEvent::ToolExecutionStart {
                tool_call_id,
                tool_name,
                args,
            } => {
                // 归一化：把字符串形态的 arguments 解析为对象，便于
                // trace 展示与 ExecutedAction.params 记录（解析失败保留空对象）
                let args = match args {
                    Value::String(raw) => serde_json::from_str::<Value>(raw)
                        .unwrap_or_else(|_| Value::Object(serde_json::Map::new())),
                    other => other.clone(),
                };
                pending.insert(tool_call_id.clone(), (tool_name.clone(), args.clone()));
                result.trace.push(TraceStep {
                    kind: TraceKind::ToolCall,
                    tool: Some(tool_name.clone()),
                    detail: args,
                });
            }
            AgentEvent::ToolExecutionEnd {
                tool_call_id,
                tool_name,
                result: tool_result,
                is_error,
            } => {
                let text = content_text(&tool_result.content);
                let (name, params) = pending
                    .remove(tool_call_id)
                    .unwrap_or_else(|| (tool_name.clone(), Value::Null));
                // loop 会给拒绝结果加 "Tool call denied: " 前缀，故用 contains 判定
                if *is_error && text.contains(DENY_PREFIX) {
                    let reason = text
                        .split(DENY_PREFIX)
                        .last()
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    result.trace.push(TraceStep {
                        kind: TraceKind::ToolDenied,
                        tool: Some(name),
                        detail: json!({ "reason": reason }),
                    });
                } else {
                    result.trace.push(TraceStep {
                        kind: TraceKind::ToolResult,
                        tool: Some(name.clone()),
                        detail: json!({ "ok": !is_error, "text": text }),
                    });
                    if !is_error {
                        result.executed.push(ExecutedAction {
                            tool: name,
                            params,
                            result: text,
                        });
                    }
                }
            }
            AgentEvent::MessageEnd { message } => {
                if let AgentMessage::Llm(Message::Assistant {
                    content,
                    stop_reason,
                    error_message,
                    ..
                }) = message
                {
                    let text = content_text(content);
                    match stop_reason {
                        StopReason::Stop | StopReason::Length => {
                            result.trace.push(TraceStep {
                                kind: TraceKind::ModelTurn,
                                tool: None,
                                detail: json!({ "text": text }),
                            });
                            if *stop_reason == StopReason::Stop {
                                result.summary = text;
                            }
                        }
                        _ => {
                            if let Some(err) = error_message {
                                result.trace.push(TraceStep {
                                    kind: TraceKind::Ended,
                                    tool: None,
                                    detail: json!({ "error": err }),
                                });
                            }
                        }
                    }
                }
            }
            AgentEvent::AgentEnd { messages, .. } => {
                // 最后一条 assistant 消息的 stop_reason 决定 run 是否算完成
                if let Some(AgentMessage::Llm(Message::Assistant {
                    stop_reason,
                    error_message,
                    ..
                })) = messages
                    .iter()
                    .rev()
                    .find(|m| matches!(m, AgentMessage::Llm(Message::Assistant { .. })))
                {
                    ended_ok = matches!(
                        stop_reason,
                        StopReason::Stop | StopReason::Length | StopReason::ToolUse
                    ) && error_message.is_none();
                    if !ended_ok {
                        if let Some(err) = error_message {
                            result.trace.push(TraceStep {
                                kind: TraceKind::Ended,
                                tool: None,
                                detail: json!({ "error": err }),
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }

    result.proposals = gate.take_denied().await;
    result.completed = ended_ok;
    if !result.completed {
        return Err(AgentError::Incomplete(
            "agent run ended without a clean stop (model error or limit)".into(),
        ));
    }
    Ok(result)
}

/// 从 content 块提取纯文本（忽略 thinking / tool_call 块）。
fn content_text(content: &[Content]) -> String {
    content
        .iter()
        .filter_map(|c| match c {
            Content::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    use yoagent::provider::ModelConfig;
    use yoagent::provider::mock::{MockProvider, MockResponse, MockToolCall};

    /// 构造 SSE 响应体：每个 chunk 一行 `data:`，空行分隔，`[DONE]` 收尾。
    /// 换行用 `char::from(10)` 构造，避免源码里出现转义序列。
    fn sse_body(chunks: &[Value]) -> String {
        let nl = char::from(10);
        let mut out = String::new();
        for c in chunks {
            out.push_str("data: ");
            out.push_str(&c.to_string());
            out.push(nl);
            out.push(nl);
        }
        out.push_str("data: [DONE]");
        out.push(nl);
        out.push(nl);
        out
    }

    /// 记录型执行器：置位标志并返回固定文本
    struct Recorder {
        ran: Arc<AtomicBool>,
        text: &'static str,
    }
    #[async_trait::async_trait]
    impl AgentToolExecutor for Recorder {
        async fn execute(&self, _params: Value) -> Result<String, String> {
            self.ran.store(true, Ordering::SeqCst);
            Ok(self.text.to_string())
        }
    }

    fn mock_tool_call(name: &str, args: Value) -> MockToolCall {
        MockToolCall {
            name: name.to_string(),
            arguments: args,
            provider_metadata: None,
        }
    }

    /// spike 1：mock provider 驱动完整循环——
    /// Autonomous 工具被执行、Confirm 工具被 gate 拦截转提案、最终文本收尾。
    #[tokio::test]
    async fn spike_mock_loop_executes_and_gates() {
        let autonomous_ran = Arc::new(AtomicBool::new(false));
        let confirm_ran = Arc::new(AtomicBool::new(false));

        let tools = vec![
            ManagedTool::new(
                "add_exclude_word",
                "Add a word to a rule's exclude list",
                json!({"type": "object", "properties": {"word": {"type": "string"}}, "required": ["word"]}),
                ActionTier::Autonomous,
                Arc::new(Recorder {
                    ran: autonomous_ran.clone(),
                    text: "excluded",
                }),
            ),
            ManagedTool::new(
                "disable_rule",
                "Disable a rule",
                json!({"type": "object", "properties": {"rule_id": {"type": "string"}}, "required": ["rule_id"]}),
                ActionTier::Confirm,
                Arc::new(Recorder {
                    ran: confirm_ran.clone(),
                    text: "disabled",
                }),
            ),
        ];

        let tiers: HashMap<String, ActionTier> =
            tools.iter().map(|t| (t.name.clone(), t.tier)).collect();
        // 与 run_agent 同构：middleware 按值拿走一个实例，断言方持共享存储的另一个
        let denied = Arc::new(Mutex::new(Vec::new()));
        let gate = TierGate::with_shared_denied(tiers.clone(), denied);

        let provider = MockProvider::new(vec![
            MockResponse::ToolCalls(vec![mock_tool_call(
                "add_exclude_word",
                json!({"word": "靠山"}),
            )]),
            MockResponse::ToolCalls(vec![mock_tool_call(
                "disable_rule",
                json!({"rule_id": "ZH_MISSING_SUBJECT"}),
            )]),
            MockResponse::Text("已完成排除「靠山」；建议禁用 ZH_MISSING_SUBJECT".into()),
        ]);

        let mut agent = Agent::from_provider(provider, ModelConfig::mock())
            .with_system_prompt("You are a test agent.")
            .with_tools(
                tools
                    .into_iter()
                    .map(|t| -> Box<dyn AgentTool> { Box::new(t) })
                    .collect(),
            )
            .with_tool_middleware(TierGate::with_shared_denied(tiers, gate.denied.clone()))
            .with_execution_limits(ExecutionLimits::default().with_max_turns(6));

        let mut events = agent.prompt("用户拒绝了一条错误").await;
        let mut saw_end = false;
        let mut last_stop: Option<StopReason> = None;
        while let Some(ev) = events.recv().await {
            if let AgentEvent::AgentEnd { messages, .. } = &ev {
                saw_end = true;
                if let Some(AgentMessage::Llm(Message::Assistant { stop_reason, .. })) = messages
                    .iter()
                    .rev()
                    .find(|m| matches!(m, AgentMessage::Llm(Message::Assistant { .. })))
                {
                    last_stop = Some(stop_reason.clone());
                }
            }
        }

        assert!(saw_end, "run must end with AgentEnd");
        assert_eq!(last_stop, Some(StopReason::Stop));
        assert!(
            autonomous_ran.load(Ordering::SeqCst),
            "autonomous tool must run"
        );
        assert!(
            !confirm_ran.load(Ordering::SeqCst),
            "confirm tool must NOT run"
        );

        let denied = gate.take_denied().await;
        assert_eq!(denied.len(), 1);
        assert_eq!(denied[0].tool, "disable_rule");
        assert_eq!(denied[0].params["rule_id"], "ZH_MISSING_SUBJECT");
        // reason 是用户可见文案（模型指令走 tool result，不进提案）
        assert!(denied[0].reason.contains("高风险"));
    }

    /// spike 3：`run_agent` 全路径——真实 OpenAI 兼容 HTTP 端点（SSE）驱动工具循环。
    /// 验证：请求带 tools、Autonomous 执行 + Confirm 转提案、事件汇总（executed/proposals/summary）。
    #[tokio::test]
    async fn spike_run_agent_over_openai_wire() {
        use axum::{Json, Router, http::StatusCode, routing};

        // 历史里出现 role=tool 的消息 → 第二轮（最终文本）；否则 → 第一轮（工具调用）。
        // SSE 载荷用 json! 程序化构造（serde 负责转义），避免手写转义。
        let args1 = json!({"word": "靠山"}).to_string();
        let args2 = json!({"rule_id": "ZH_MISSING_SUBJECT"}).to_string();
        let turn1 = sse_body(&[
            json!({"choices": [{"delta": {"role": "assistant", "tool_calls": [
                {"index": 0, "id": "call_1", "function": {"name": "add_exclude_word", "arguments": args1}},
                {"index": 1, "id": "call_2", "function": {"name": "disable_rule", "arguments": args2}},
            ]}}]}),
            json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}]}),
        ]);
        let turn2 = sse_body(&[
            json!({"choices": [{"delta": {"content": "已排除「靠山」。"}}]}),
            json!({"choices": [{"delta": {}, "finish_reason": "stop"}], "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}}),
        ]);

        let app = Router::new().route(
            "/v1/chat/completions",
            routing::post(move |Json(req): Json<Value>| async move {
                let has_tool_result = req["messages"]
                    .as_array()
                    .map(|ms| ms.iter().any(|m| m["role"] == "tool"))
                    .unwrap_or(false);
                let body = if has_tool_result { turn2 } else { turn1 };
                (
                    StatusCode::OK,
                    [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                    body,
                )
            }),
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).into_future().await });

        let autonomous_ran = Arc::new(AtomicBool::new(false));
        let confirm_ran = Arc::new(AtomicBool::new(false));
        let tools = vec![
            ManagedTool::new(
                "add_exclude_word",
                "Add a word to a rule's exclude list",
                json!({"type": "object", "properties": {"word": {"type": "string"}}, "required": ["word"]}),
                ActionTier::Autonomous,
                Arc::new(Recorder {
                    ran: autonomous_ran.clone(),
                    text: "excluded",
                }),
            ),
            ManagedTool::new(
                "disable_rule",
                "Disable a rule",
                json!({"type": "object", "properties": {"rule_id": {"type": "string"}}, "required": ["rule_id"]}),
                ActionTier::Confirm,
                Arc::new(Recorder {
                    ran: confirm_ran.clone(),
                    text: "disabled",
                }),
            ),
        ];

        let config = LlmDetectConfig {
            base_url: format!("http://{addr}/v1"),
            api_key: "test-key".into(),
            model: "test-model".into(),
            timeout_secs: 10,
            system: String::new(),
            user_template: String::new(),
            output_format: "json".into(),
            max_tokens: 0,
        };

        let result = run_agent(&config, "你是测试 agent", "用户拒绝了一条错误", tools)
            .await
            .expect("agent run should complete");

        assert!(result.completed);
        assert_eq!(result.summary, "已排除「靠山」。");
        assert_eq!(result.executed.len(), 1);
        assert_eq!(result.executed[0].tool, "add_exclude_word");
        assert_eq!(result.executed[0].params["word"], "靠山");
        assert_eq!(result.proposals.len(), 1);
        assert_eq!(result.proposals[0].tool, "disable_rule");
        assert_eq!(result.proposals[0].params["rule_id"], "ZH_MISSING_SUBJECT");
        assert!(autonomous_ran.load(Ordering::SeqCst));
        assert!(!confirm_ran.load(Ordering::SeqCst));
        // 轨迹里应有 gate 拦截步
        assert!(result.trace.iter().any(|s| s.kind == TraceKind::ToolDenied));
    }

    /// `build_rejection_task`：L3 选项仅对 en/zh 出现（其他语言执行器会拒绝）
    #[test]
    fn rejection_task_mentions_l3_only_for_en_zh() {
        let zh = build_rejection_task("zh", "报复", "spelling", "rule", "ZH_X", "抱负", "诊断");
        assert!(zh.contains("add_l3_negative"), "zh 任务应含 L3 选项: {zh}");
        let en = build_rejection_task(
            "en", "a apple", "spelling", "decision", "EN_X", "an apple", "d",
        );
        assert!(en.contains("add_l3_negative"), "en 任务应含 L3 选项: {en}");
        let ja = build_rejection_task("ja", "苹果", "spelling", "rule", "JA_X", "林檎", "診断");
        assert!(!ja.contains("add_l3_negative"), "ja 任务不应提 L3: {ja}");
        assert!(ja.contains("disable_rule"));
        assert!(ja.contains("反思诊断：診断"));
    }

    /// spike 4：content 文本提取（只取 Text 块）。
    #[test]
    fn spike_content_text_extracts_only_text_blocks() {
        let text = content_text(&[
            Content::Text {
                text: "hello".into(),
            },
            Content::Text {
                text: " world".into(),
            },
        ]);
        assert_eq!(text, "hello world");
    }
}
