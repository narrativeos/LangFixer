//! 自进化 agent 集成测试（`/gec/agent/run` + `/gec/feedback` reject 分支）
//!
//! 覆盖：
//! - reject + LLM 已配置：agent 自主动作真实落盘（exclude 写入词表）+
//!   Confirm 动作被 gate 拦截转提案（规则保持启用）
//! - reject + LLM 未配置：响应与引入前一致（agent 为 null）
//! - `/gec/agent/run` + LLM 未配置：agent_available = false

use std::sync::Arc;
use std::sync::Mutex;

use api_server::{middleware, routes, state::AppState};

/// 环境变量互斥锁（与 tests/feedback_api.rs 相同的并行隔离约定）
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn env_guard() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn lexicons_abs() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../lexicons")
}

fn prompts_abs() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../prompts")
}

/// 把仓库词表目录复制到临时目录（agent 的 exclude 写入不污染仓库）
fn lexicons_temp_copy() -> std::path::PathBuf {
    let tmp = std::env::temp_dir().join(format!(
        "langfixer_agent_lex_{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    for entry in std::fs::read_dir(lexicons_abs()).unwrap() {
        let entry = entry.unwrap();
        if entry.path().is_file() {
            std::fs::copy(entry.path(), tmp.join(entry.path().file_name().unwrap())).unwrap();
        }
    }
    tmp
}

/// 构建 server（调用方须持 env 锁）；`llm_base_url` 为 Some 时配置 LLM 指向该地址
async fn build_server(llm_base_url: Option<&str>) -> (String, std::path::PathBuf) {
    let tables_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../decision-tables");
    let lexicons = lexicons_temp_copy();
    unsafe {
        std::env::set_var(
            "GEC_SETTINGS_FILE",
            std::env::temp_dir()
                .join(format!(
                    "langfixer_ag_{}.json",
                    uuid::Uuid::new_v4().simple()
                ))
                .display()
                .to_string(),
        );
        std::env::set_var(
            "GEC_L3_DATA_DIR",
            std::env::temp_dir()
                .join(format!("langfixer_ag_l3_{}", uuid::Uuid::new_v4().simple()))
                .display()
                .to_string(),
        );
        match llm_base_url {
            Some(url) => {
                std::env::set_var("LLM_BASE_URL", url);
                std::env::set_var("LLM_API_KEY", "test-key");
                std::env::set_var("LLM_MODEL", "test-model");
                std::env::set_var("LLM_PROMPTS_DIR", prompts_abs().display().to_string());
            }
            None => {
                std::env::remove_var("LLM_API_KEY");
                std::env::remove_var("LLM_BASE_URL");
                std::env::remove_var("LLM_MODEL");
                std::env::remove_var("LLM_PROMPTS_DIR");
            }
        }
        std::env::remove_var("GEC_ENGINE_ZH");
        std::env::remove_var("GEC_ENGINE_EN");
        std::env::remove_var("GEC_ENGINE_JA");
        std::env::remove_var("GEC_STAT_ENDPOINT");
        std::env::remove_var("GEC_OFFLINE");
        std::env::set_var("HANLP_ENDPOINT", "http://127.0.0.1:8000");
        std::env::remove_var("GEC_TABLES_DIR");
        std::env::set_var("GEC_LEXICONS_DIR", lexicons.display().to_string());
        std::env::remove_var("GEC_TABLES_UPDATE_TOKEN");
        std::env::remove_var("CORS_ORIGINS");
    }
    let db_file =
        std::env::temp_dir().join(format!("langfixer_ag_{}.db", uuid::Uuid::new_v4().simple()));
    let state = Arc::new(AppState::from_env(Some(&tables_dir), Some(&db_file)));
    let app = routes::router(state).layer(middleware::cors::cors_layer());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, app).into_future());
    (format!("http://{addr}"), lexicons)
}

fn client() -> reqwest::Client {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(reqwest::header::CONNECTION, "close".parse().unwrap());
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .default_headers(headers)
        .build()
        .unwrap()
}

/// 启动 mock OpenAI 兼容端点（SSE）：
/// 第一轮返回两个工具调用（add_exclude_word 自主 + disable_rule 确认），
/// 第二轮（历史含 tool 结果）返回最终文本。
async fn start_mock_llm() -> String {
    use axum::{Json, Router, http::StatusCode, routing};

    let nl = char::from(10);
    // arguments 是 JSON 字符串（真实 LLM 的 SSE 同样如此），json! 宏会自动
    // 转义内层引号；agent 侧负责把它解析回对象
    let args1 =
        serde_json::to_string(&serde_json::json!({"rule_id": "ZH_CONFUSABLE", "word": "报复"}))
            .unwrap();
    let args2 = serde_json::to_string(&serde_json::json!({"rule_id": "ZH_CONFUSABLE"})).unwrap();
    let turn1 = format!(
        "data: {c1}{n}{n}data: {c2}{n}{n}data: [DONE]{n}{n}",
        c1 = serde_json::json!({
            "choices": [{
                "delta": {
                    "role": "assistant",
                    "tool_calls": [
                        {"index": 0, "id": "call_1", "function": {"name": "add_exclude_word", "arguments": args1}},
                        {"index": 1, "id": "call_2", "function": {"name": "disable_rule", "arguments": args2}},
                    ]
                }
            }]
        }),
        c2 = serde_json::json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}]}),
        n = nl
    );
    let turn2 = format!(
        "data: {c1}{n}{n}data: {c2}{n}{n}data: [DONE]{n}{n}",
        c1 = serde_json::json!({"choices": [{"delta": {"content": "已把「报复」加入 ZH_CONFUSABLE 排除名单。"}}]}),
        c2 = serde_json::json!({"choices": [{"delta": {}, "finish_reason": "stop"}], "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}}),
        n = nl
    );

    let app = Router::new().route(
        "/v1/chat/completions",
        routing::post(move |Json(req): Json<serde_json::Value>| async move {
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
    format!("http://{addr}/v1")
}

/// reject + LLM 已配置：agent 自主动作落盘 + 高风险动作转提案（规则保持启用）
#[tokio::test]
async fn reject_with_agent_executes_and_proposes() {
    let _g = env_guard();
    let mock = start_mock_llm().await;
    let (base, lexicons) = build_server(Some(&mock)).await;
    let c = client();

    // 记录 ZH_CONFUSABLE 初始启用状态（生产词表可能已停用该规则——断言用「不变」而非硬编码 true）
    let v0: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(lexicons.join("zh_gec_lexicon.json")).unwrap(),
    )
    .unwrap();
    let initial_enabled = v0["rules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "ZH_CONFUSABLE")
        .unwrap()["enabled"]
        .clone();

    let body = serde_json::json!({
        "lang": "zh",
        "error": {
            "lang": "zh",
            "error_type": "spelling",
            "confidence": 0.9,
            "span": [0, 2],
            "original": "报复",
            "message": "测试错误",
            "correction": "抱负",
            "rule_id": "ZH_CONFUSABLE",
        },
        "action": "reject",
        "reason": "这个词在上下文中是正确的",
    });
    let resp = c
        .post(format!("{base}/gec/feedback"))
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let res: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(status, reqwest::StatusCode::OK, "reject 应成功: {}", res);

    // ① 白名单 + 反思（确定性路径不受 agent 影响）
    assert!(res["word_id"].is_number(), "应返回白名单词 id: {}", res);
    assert_eq!(res["reflection"]["subject"], "ZH_CONFUSABLE");

    // ② agent 结果：自主动作已执行
    let agent = &res["agent"];
    assert!(
        agent.is_object(),
        "LLM 已配置时 reject 响应应带 agent 结果: {}",
        res
    );
    let executed = agent["executed"].as_array().expect("executed 数组");
    let exclude_exec: Option<&serde_json::Value> =
        executed.iter().find(|a| a["tool"] == "add_exclude_word");
    assert!(
        exclude_exec.is_some(),
        "agent 应执行 add_exclude_word: {}",
        agent
    );
    let exclude_exec = exclude_exec.unwrap();
    assert_eq!(exclude_exec["params"]["word"], "报复");

    // ③ 高风险动作被 gate 拦截 → 提案（规则保持启用）
    let proposals = agent["proposals"].as_array().expect("proposals 数组");
    let disable_prop: Option<&serde_json::Value> =
        proposals.iter().find(|p| p["tool"] == "disable_rule");
    assert!(
        disable_prop.is_some(),
        "agent 应提案 disable_rule: {}",
        agent
    );
    let disable_prop = disable_prop.unwrap();
    assert_eq!(disable_prop["params"]["rule_id"], "ZH_CONFUSABLE");

    // ④ 自主动作真实落盘：词表文件 excludes 含「报复」
    let v: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(lexicons.join("zh_gec_lexicon.json")).unwrap(),
    )
    .unwrap();
    let rule = v["rules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "ZH_CONFUSABLE")
        .unwrap();
    assert!(
        rule["excludes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w == "报复"),
        "「报复」应写入 ZH_CONFUSABLE 排除名单: {}",
        rule["excludes"]
    );

    // ⑤ 提案未被执行：规则启用状态与执行前一致（经列表接口查询）
    let list_json: serde_json::Value = c
        .get(format!("{base}/gec/rules?lang=zh"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let rule = list_json["rules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "ZH_CONFUSABLE")
        .unwrap();
    assert_eq!(
        rule["enabled"], initial_enabled,
        "Confirm 动作不得自动执行（状态应与执行前一致）: {}",
        rule
    );

    let _ = std::fs::remove_dir_all(&lexicons);
}

/// 提案确认闭环：agent 提案 disable_rule → 用户 confirm → 规则真实停用 + 审计留痕；
/// 且 confirm 端点拒绝 Autonomous 工具（防重放）与未知工具
#[tokio::test]
async fn confirm_agent_proposal_executes_and_audits() {
    let _g = env_guard();
    let mock = start_mock_llm().await;
    let (base, lexicons) = build_server(Some(&mock)).await;
    let c = client();

    // ① reject 触发 agent run，拿到 disable_rule 提案
    let body = serde_json::json!({
        "lang": "zh",
        "error": {
            "lang": "zh",
            "error_type": "spelling",
            "confidence": 0.9,
            "span": [0, 2],
            "original": "报复",
            "message": "测试错误",
            "correction": "抱负",
            "rule_id": "ZH_CONFUSABLE",
        },
        "action": "reject",
        "reason": "这个词在上下文中是正确的",
    });
    let resp = c
        .post(format!("{base}/gec/feedback"))
        .json(&body)
        .send()
        .await
        .unwrap();
    let res: serde_json::Value = resp.json().await.unwrap();
    let agent = &res["agent"];
    assert!(
        agent.is_object(),
        "LLM 已配置时 reject 响应应带 agent 结果: {}",
        res
    );
    let proposals = agent["proposals"].as_array().expect("proposals 数组");
    let prop = proposals
        .iter()
        .find(|p| p["tool"] == "disable_rule")
        .expect("agent 应提案 disable_rule");

    // ② 确认提案：经同一执行器真实执行
    let confirm_body = serde_json::json!({
        "lang": "zh",
        "tool": prop["tool"],
        "params": prop["params"],
        "error": body["error"],
    });
    let resp = c
        .post(format!("{base}/gec/agent/confirm"))
        .json(&confirm_body)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let confirm_res: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "confirm 应成功: {}",
        confirm_res
    );
    assert_eq!(confirm_res["ok"], true);

    // ③ 规则真实停用（store 经列表接口可查）
    let list_json: serde_json::Value = c
        .get(format!("{base}/gec/rules?lang=zh"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let rule = list_json["rules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "ZH_CONFUSABLE")
        .unwrap();
    assert_eq!(rule["enabled"], false, "确认后的提案应停用规则: {}", rule);

    // ④ 审计：反馈历史含 agent_confirm 条目
    let hist: serde_json::Value = c
        .get(format!("{base}/gec/feedback/history?lang=zh"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        hist["recent"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["action"] == "agent_confirm:disable_rule"),
        "反馈历史应含 agent_confirm 审计条目: {}",
        hist["recent"]
    );

    // ⑤ 防重放：Autonomous 工具不接受二次提交
    let bad = serde_json::json!({"lang": "zh", "tool": "add_exclude_word", "params": {}});
    let resp = c
        .post(format!("{base}/gec/agent/confirm"))
        .json(&bad)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    // ⑥ 未知工具 → 400
    let bad = serde_json::json!({"lang": "zh", "tool": "drop_table", "params": {}});
    let resp = c
        .post(format!("{base}/gec/agent/confirm"))
        .json(&bad)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    let _ = std::fs::remove_dir_all(&lexicons);
}

/// reject + LLM 未配置：响应与引入前一致（agent 为 null）
#[tokio::test]
async fn reject_without_llm_keeps_legacy_behavior() {
    let _g = env_guard();
    let (base, lexicons) = build_server(None).await;
    let c = client();

    let body = serde_json::json!({
        "lang": "zh",
        "error": {
            "lang": "zh",
            "error_type": "spelling",
            "confidence": 0.9,
            "span": [0, 2],
            "original": "报复",
            "message": "测试错误",
            "correction": "抱负",
            "rule_id": "ZH_CONFUSABLE",
        },
        "action": "reject",
    });
    let resp = c
        .post(format!("{base}/gec/feedback"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let res: serde_json::Value = resp.json().await.unwrap();
    assert!(res["word_id"].is_number());
    assert!(res["reflection"].is_object());
    assert!(
        res["agent"].is_null(),
        "LLM 未配置时 agent 应为 null: {}",
        res
    );

    let _ = std::fs::remove_dir_all(&lexicons);
}

/// POST /gec/agent/run + LLM 未配置：agent_available = false
#[tokio::test]
async fn agent_run_unavailable_when_llm_missing() {
    let _g = env_guard();
    let (base, lexicons) = build_server(None).await;
    let c = client();

    let body = serde_json::json!({
        "lang": "zh",
        "task": "分析误报",
    });
    let resp = c
        .post(format!("{base}/gec/agent/run"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let res: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(res["agent_available"], false, "{}", res);
    assert!(res["result"].is_null());

    let _ = std::fs::remove_dir_all(&lexicons);
}
