//! 评测通道反馈 = 留痕模式的 HTTP 层验收（`routes::feedback::record_only_feedback`）。
//!
//! `POST /gec/feedback` 在评审身份下被 fail-closed 白名单放行（见
//! `middleware::auth::review_denied`），安全性靠的是 handler 自动降级：
//! - adopt 不改共享规则库（无蒸馏产物、`via == "record"`）；
//! - reject 不写共享白名单（`word_id == null`），反思报告仍可读；
//! - 错题本只进**本人会话**：带他人 session_id 即 403（`Tenant::guard_session`）；
//! - `/gec/feedback/action`、`/gec/feedback/history` 子端点仍 403。
//!
//! 与 feedback_api.rs 同口径：词表用出厂目录的临时副本（本测试不应改任何共享
//! 资产，若改了新规则，副本能兜住出厂词表）。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use api_server::{auth::GatewayConfig, middleware, routes, state::AppState};
use axum::Router;
use serde_json::{Value, json};

const ADMIN: &str = "lf-admin-token-for-feedback-test-0123456789abcd";

fn temp_path(prefix: &str, ext: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "{prefix}-{}.{}",
        uuid::Uuid::new_v4().simple(),
        ext
    ))
}

/// 出厂词表的临时副本（同 feedback_api.rs 的隔离约定）
fn lexicons_abs() -> PathBuf {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../lexicons");
    let tmp = std::env::temp_dir().join(format!(
        "langfixer_gwfb_lex_{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    for entry in std::fs::read_dir(&src).unwrap() {
        let entry = entry.unwrap();
        if entry.path().is_file() {
            std::fs::copy(entry.path(), tmp.join(entry.path().file_name().unwrap())).unwrap();
        }
    }
    tmp
}

struct Server {
    base: String,
    state: Arc<AppState>,
    reviewer: String,
    other: String,
    _files: Vec<PathBuf>,
}

/// 业务全路由 + 网关鉴权门：调用方须持进程级 env 锁（同 feedback_api 约定）
async fn build_gateway_server() -> Server {
    let tables_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../decision-tables");
    let app_db = temp_path("langfixer_gwfb_app", "db");
    let gw_db = temp_path("langfixer_gwfb_gw", "db");
    unsafe {
        std::env::set_var(
            "GEC_SETTINGS_FILE",
            temp_path("langfixer_gwfb", "json").display().to_string(),
        );
        std::env::set_var(
            "GEC_L3_DATA_DIR",
            temp_path("langfixer_gwfb_l3", "").display().to_string(),
        );
        for v in [
            "LLM_API_KEY",
            "LLM_BASE_URL",
            "LLM_MODEL",
            "LLM_PROMPTS_DIR",
            "GEC_ENGINE_ZH",
            "GEC_ENGINE_EN",
            "GEC_STAT_ENDPOINT",
            "GEC_OFFLINE",
            "GEC_TABLES_DIR",
            "GEC_TABLES_UPDATE_TOKEN",
            "CORS_ORIGINS",
        ] {
            std::env::remove_var(v);
        }
        std::env::set_var("HANLP_ENDPOINT", "http://127.0.0.1:8000");
        std::env::set_var("GEC_LEXICONS_DIR", lexicons_abs().display().to_string());
    }
    let state = Arc::new(AppState::from_env(
        Some(&tables_dir),
        Some(app_db.as_path()),
    ));
    let mut dotenv = HashMap::new();
    dotenv.insert("LANGFIXER_GATEWAY".to_string(), "1".to_string());
    dotenv.insert("LANGFIXER_COOKIE_SECURE".to_string(), "0".to_string());
    dotenv.insert("LANGFIXER_ADMIN_TOKEN".to_string(), ADMIN.to_string());
    let cfg = Arc::new(GatewayConfig::build(&dotenv, &gw_db).unwrap());
    let reviewer = cfg.store.issue("评审甲", 0).unwrap().token;
    let other = cfg.store.issue("评审乙", 0).unwrap().token;

    let app: Router = routes::router(state.clone()).layer(axum::middleware::from_fn_with_state(
        cfg.clone(),
        middleware::auth::gate,
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Server {
        base: format!("http://{addr}"),
        state,
        reviewer,
        other,
        _files: vec![app_db, gw_db],
    }
}

fn error_json(original: &str, rule_id: &str) -> Value {
    json!({
        "lang": "zh",
        "error_type": "spelling",
        "confidence": 0.9,
        "span": [0, original.len()],
        "original": original,
        "correction": format!("{original}（改）"),
        "source": "rule",
        "rule_id": rule_id,
        "message": "测试错误",
    })
}

async fn post(
    c: &reqwest::Client,
    base: &str,
    path: &str,
    token: &str,
    body: &Value,
) -> (reqwest::StatusCode, Value) {
    let resp = c
        .post(format!("{base}{path}"))
        .bearer_auth(token)
        .json(body)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let v: Value = resp.json().await.unwrap_or(Value::Null);
    (status, v)
}

fn env_guard() -> std::sync::MutexGuard<'static, ()> {
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[tokio::test]
async fn gateway_feedback_adopts_record_only_and_never_writes_rules() {
    let _env = env_guard();
    let s = build_gateway_server().await;
    let c = reqwest::Client::new();

    let (status, res) = post(
        &c,
        &s.base,
        "/gec/feedback",
        &s.reviewer,
        &json!({ "lang": "zh", "action": "adopt", "error": error_json("报复", "ZH_SPELL") }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "评测通道反馈应放行: {res}");
    assert_eq!(
        res["via"], "record",
        "评审身份必须走留痕模式，不得蒸馏: {res}"
    );
    assert!(
        res.get("rule").map(|r| r.is_null()).unwrap_or(true),
        "不得返回/创建共享规则: {res}"
    );
    assert!(res["message"].as_str().unwrap().contains("评测通道"));

    // 子端点仍钉死在门外
    let (st, body) = post(
        &c,
        &s.base,
        "/gec/feedback/action",
        &s.reviewer,
        &json!({ "action": "disable_rule", "rule_id": "ZH_SPELL" }),
    )
    .await;
    assert_eq!(st, reqwest::StatusCode::FORBIDDEN, "/action 不得随之放开");
    assert!(body["error"].as_str().unwrap_or("").contains("评测通道"));
    let resp = c
        .get(format!("{}/gec/feedback/history?lang=zh", s.base))
        .bearer_auth(&s.reviewer)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::FORBIDDEN,
        "反馈历史仍属运营面"
    );
}

#[tokio::test]
async fn gateway_feedback_rejects_without_shared_whitelist_and_agent() {
    let _env = env_guard();
    let s = build_gateway_server().await;
    let c = reqwest::Client::new();

    let original = "放弃词甲";
    let (status, res) = post(
        &c,
        &s.base,
        "/gec/feedback",
        &s.reviewer,
        &json!({
            "lang": "zh",
            "action": "reject",
            "error": error_json(original, "ZH_SPELL"),
            "reason": "这个词在上下文里是对的"
        }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "reject 留痕应成功: {res}");
    assert!(res["word_id"].is_null(), "不得写共享白名单: {res}");
    assert!(res["agent"].is_null(), "不得起自进化 agent: {res}");
    assert_eq!(
        res["reflection"]["source"], "rule",
        "只读反思报告仍应可回: {res}"
    );
    assert_eq!(
        res["llm_reflect_ready"], false,
        "reflect-llm 在评测通道不开放，不得引导前端去撞 403"
    );
    assert!(
        !s.state.custom_words.is_whitelisted("zh", original),
        "白名单落库即共享状态被改，越线"
    );
}

#[tokio::test]
async fn gateway_feedback_mistake_book_is_session_guarded() {
    let _env = env_guard();
    let s = build_gateway_server().await;
    let c = reqwest::Client::new();

    let body = |sid: &str| {
        json!({
            "lang": "zh",
            "action": "adopt",
            "error": error_json("归属词", "ZH_SPELL"),
            "session_id": sid,
            "src_text": "他抱着报复的态度工作。"
        })
    };
    // 评审甲首次使用即绑定该会话（同 detect 的归属口径）
    let (st, _) = post(
        &c,
        &s.base,
        "/gec/feedback",
        &s.reviewer,
        &body("s_fb_owner"),
    )
    .await;
    assert_eq!(st, reqwest::StatusCode::OK, "本人会话应可留痕错题");
    // 评审乙拿同一会话即 fail-closed 403
    let (st, body) = post(&c, &s.base, "/gec/feedback", &s.other, &body("s_fb_owner")).await;
    assert_eq!(st, reqwest::StatusCode::FORBIDDEN, "他人会话不得写入错题本");
    assert!(body["error"].as_str().unwrap_or("").contains("不属于"));
}
