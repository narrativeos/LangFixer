//! MCP 面（Model Context Protocol，streamable-HTTP 无状态形态）
//!
//! `POST /mcp` 承载 JSON-RPC 2.0 单请求/响应（`initialize` / `tools/list` /
//! `tools/call` / `ping`），不开 SSE、不开会话——评测与联调场景一个 POST 走全链路。
//! 身份与配额和 web 面共用同一道 gate 与同一套 token（`docs/GATEWAY_AUTH.md`）。
//!
//! 档位纪律（与 web 侧 Confirm gate 同源叙事）：
//! - 单机形态：全部工具直接执行（本地就是用户自己的机器）；
//! - 网关形态：只读工具直接执行；写档工具（Autonomous 写与 Confirm）一律
//!   **返回提案不执行**——评审令牌共享同一份产品状态，写没有账号隔离，
//!   执行权留给拿着产品界面的用户点「采纳」的那一刻。

use std::sync::Arc;

use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use gec_core::ManagedTool;
use serde_json::{Value, json};

use crate::APP_VERSION;
use crate::agent_tools::AgentToolCtx;
use crate::auth::Tenant;
use crate::state::AppState;

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/mcp", post(handle).get(usage))
        .with_state(state)
}

/// GET 提示：本面只走 POST JSON-RPC（省得评审把 URL 贴进浏览器时一头雾水）
async fn usage() -> impl IntoResponse {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        Json(json!({
            "usage": "POST /mcp（JSON-RPC 2.0：initialize / tools/list / tools/call / ping）"
        })),
    )
}

/// 只读白名单：网关形态下仅此集合（外加 proofread / explain 两条只读管线）直接执行。
const READ_ONLY: &[&str] = &[
    "list_rules",
    "get_rule",
    "search_knowledge",
    "feedback_stats",
    "proofread",
    "explain",
];

type Rpc = (StatusCode, Json<Value>);

fn ok(id: Value, result: Value) -> Rpc {
    (
        StatusCode::OK,
        Json(json!({"jsonrpc": "2.0", "id": id, "result": result})),
    )
}

fn rpc_error(id: Value, code: i64, message: &str) -> Rpc {
    (
        StatusCode::OK,
        Json(json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})),
    )
}

/// 工具结果信封（MCP tools/call result）
fn tool_text(text: impl Into<String>) -> Value {
    json!({"content": [{"type": "text", "text": text.into()}]})
}

fn tool_error(text: impl Into<String>) -> Value {
    json!({"content": [{"type": "text", "text": text.into()}], "isError": true})
}

async fn handle(
    State(state): State<Arc<AppState>>,
    Extension(tenant): Extension<Tenant>,
    Json(req): Json<Value>,
) -> Rpc {
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let is_notification = req.get("id").is_none();
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or(json!({}));

    if is_notification {
        // notifications/initialized 等：确认即收
        return (StatusCode::ACCEPTED, Json(json!({})));
    }

    match method {
        "initialize" => ok(id, init_result(&params)),
        "ping" => ok(id, json!({})),
        "tools/list" => ok(id, json!({"tools": tool_specs(&state)})),
        "tools/call" => call_tool(&state, &tenant, id, params).await,
        other => rpc_error(id, -32601, &format!("method not found: {other}")),
    }
}

/// initialize 应答（协议版本回显客户端请求值；纯函数便于测试）
fn init_result(params: &Value) -> Value {
    let pv = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or("2025-03-26");
    json!({
        "protocolVersion": pv,
        "capabilities": {"tools": {}},
        "serverInfo": {"name": "langfixer", "version": APP_VERSION},
    })
}

/// 提案信封：网关形态下写档工具不执行，返回与 web 侧 Confirm gate 同源的结构
fn proposal_envelope(name: &str, args: Value) -> Value {
    json!({
        "content": [{"type": "text", "text": format!(
            "[confirm-required] {name} 为写档操作，评测通道仅返回提案，不执行。"
        )}],
        "structuredContent": {
            "executed": false,
            "proposal": {"tool": name, "arguments": args},
        },
    })
}

// ────────────────────────────── 工具目录 ──────────────────────────────

fn managed(state: &Arc<AppState>) -> Vec<ManagedTool> {
    // 目录按 zh 构建；tools/call 时按参数 lang 重建执行器作用域
    AgentToolCtx::new(state, "zh").build_tools()
}

fn tool_specs(state: &Arc<AppState>) -> Vec<Value> {
    let mut specs = vec![
        json!({
            "name": "proofread",
            "description": "对文本做语法/拼写纠错检测，返回错误列表（位置/类型/建议修正）。审校能力主入口。",
            "inputSchema": {"type": "object", "properties": {
                "text": {"type": "string"},
                "lang": {"type": "string", "description": "zh / en / ja，默认 zh"},
                "level": {"type": "integer", "description": "1=规则层（默认，毫秒级）；2=L3特征模型门控触发LLM；3=无条件LLM兜底（错别字类需 3）"}
            }, "required": ["text"]}
        }),
        json!({
            "name": "explain",
            "description": "对文本中的第一处错误生成个性化讲解与练习（需 LLM 配置）。",
            "inputSchema": {"type": "object", "properties": {
                "text": {"type": "string"},
                "lang": {"type": "string", "description": "zh / en / ja，默认 zh"}
            }, "required": ["text"]}
        }),
    ];
    specs.extend(managed(state).iter().map(|t| {
        json!({
            "name": t.name,
            "description": if crate::auth::gateway_mode() && !READ_ONLY.contains(&t.name.as_str()) {
                format!("{}（评测通道：调用时返回提案不执行）", t.description)
            } else { t.description.clone() },
            "inputSchema": t.parameters.clone(),
        })
    }));
    specs
}

async fn call_tool(state: &Arc<AppState>, tenant: &Tenant, id: Value, params: Value) -> Rpc {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    let lang = args
        .get("lang")
        .and_then(Value::as_str)
        .unwrap_or("zh")
        .to_string();

    match name {
        "proofread" | "explain" => {
            let Some(text) = args.get("text").and_then(Value::as_str) else {
                return ok(id, tool_error("arguments.text 必填"));
            };
            // 与 web 面同一配额账（评审令牌按字符计量；超限 429 语义以工具错误呈现）
            if let Err(e) = tenant.charge_chars(text.chars().count()) {
                return ok(id, tool_error(e.to_string()));
            }
            let r = if name == "proofread" {
                let level = args.get("level").and_then(Value::as_i64).unwrap_or(1) as u8;
                proofread(state.clone(), &lang, text, level).await
            } else {
                explain_text(state.clone(), &lang, text).await
            };
            match r {
                Ok(out) => ok(id, out),
                Err(e) => ok(id, tool_error(e)),
            }
        }
        _ => {
            let tools = AgentToolCtx::new(state, &lang).build_tools();
            let Some(tool) = tools.iter().find(|t| t.name == name) else {
                return rpc_error(id, -32602, &format!("unknown tool: {name}"));
            };
            // 网关形态：只读外一律提案（写不隔离共享状态；执行权在产品界面）
            if crate::auth::gateway_mode() && !READ_ONLY.contains(&name) {
                return ok(id, proposal_envelope(name, args));
            }
            match tool.executor.execute(args).await {
                Ok(text) => ok(id, json!({"content": [{"type": "text", "text": text}]})),
                Err(e) => ok(id, tool_error(e)),
            }
        }
    }
}

async fn proofread(
    state: Arc<AppState>,
    lang: &str,
    text: &str,
    level: u8,
) -> Result<Value, String> {
    let (outcome, _from_history) = gec(state, lang, text, level).await?;
    let errors = serde_json::to_value(&outcome.errors).map_err(|e| e.to_string())?;
    Ok(json!({
        "content": [{"type": "text", "text": format!("检测完成：{} 处错误", outcome.errors.len())}],
        "structuredContent": {"errors": errors, "count": outcome.errors.len()},
    }))
}

async fn gec(
    state: Arc<AppState>,
    lang: &str,
    text: &str,
    level: u8,
) -> Result<(gec_core::DetectOutcome, bool), String> {
    super::detect::run_detection(state, lang, text, &None, Some(level), false)
        .await
        .map_err(|e| e.to_string())
}

/// explain：检测 → 对第一处错误生成讲解（无错误则直接说明）
/// 规则层无检出且 LLM 可用时升 level 3 复检（错别字类只有 LLM 兜底能发现）
async fn explain_text(state: Arc<AppState>, lang: &str, text: &str) -> Result<Value, String> {
    let (mut outcome, _) = gec(state.clone(), lang, text, 1).await?;
    if outcome.errors.is_empty() && state.llm_service().is_available() {
        outcome = gec(state.clone(), lang, text, 3).await?.0;
    }
    let Some(first) = outcome.errors.first() else {
        return Ok(tool_text("未检出错误，无需讲解。"));
    };
    let req = super::explain::ExplainRequest {
        lang: lang.to_string(),
        error_type: first.error_type.clone(),
        attribution: None,
        cefr: None,
        session_id: None,
        session_error_count: None,
        target_language: None,
    };
    match super::explain::explain(State(state), Json(req)).await {
        Ok(Json(resp)) => Ok(json!({
            "content": [{"type": "text", "text": resp.explanation}, {"type": "text", "text": resp.exercises}],
            "structuredContent": {"error_type": first.error_type, "explanation": resp.explanation, "exercises": resp.exercises},
        })),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_echoes_protocol_version() {
        let r = init_result(&json!({"protocolVersion": "2024-11-05"}));
        assert_eq!(r["protocolVersion"], "2024-11-05");
        assert_eq!(r["serverInfo"]["name"], "langfixer");
        assert_eq!(r["capabilities"]["tools"], json!({}));
        // 缺省协议版本
        assert_eq!(init_result(&json!({}))["protocolVersion"], "2025-03-26");
    }

    #[test]
    fn proposal_envelope_carries_tool_and_args() {
        let p = proposal_envelope("disable_rule", json!({"rule_id": "zh-x-1"}));
        assert_eq!(p["structuredContent"]["executed"], false);
        assert_eq!(p["structuredContent"]["proposal"]["tool"], "disable_rule");
        assert_eq!(
            p["structuredContent"]["proposal"]["arguments"]["rule_id"],
            "zh-x-1"
        );
        assert!(
            p["content"][0]["text"]
                .as_str()
                .unwrap()
                .starts_with("[confirm-required]")
        );
    }

    #[test]
    fn read_only_whitelist_excludes_write_tools() {
        for t in [
            "list_rules",
            "get_rule",
            "search_knowledge",
            "feedback_stats",
            "proofread",
            "explain",
        ] {
            assert!(READ_ONLY.contains(&t), "{t} 应为只读白名单");
        }
        // 写档不得混入白名单（agent_tools.rs 的 add_/remove_/disable_/enable_ 全集）
        for t in [
            "add_exclude_word",
            "remove_exclude_word",
            "add_l3_negative",
            "add_knowledge",
            "disable_rule",
            "enable_rule",
            "remove_example",
            "remove_knowledge",
        ] {
            assert!(!READ_ONLY.contains(&t), "{t} 是写档，不应在白名单");
        }
    }
}
