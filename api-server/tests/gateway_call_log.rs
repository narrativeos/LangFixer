//! 调用级日志（`gw_call_log`）的 HTTP 层验收。
//!
//! 要守住的语义：
//! 1. 放行与拒绝都记一行，被拒的 401/403 不能静默蒸发；
//! 2. 核验成功的评审令牌即使被拒也记本人——越权尝试不得洗成匿名；
//! 3. 匿名与超管的行 token_id 为 NULL，计入实例合计、不计入任何令牌行；
//! 4. endpoint 只存路径：带 query（可能含用户文本）也不得把查询串落进日志；
//! 5. `/admin/stats` 与 `/admin/tokens` 的调用口径与该表一致。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use api_server::{auth::GatewayConfig, middleware, routes};
use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

const ADMIN: &str = "lf-admin-token-for-call-log-test-0123456789";

fn temp_db(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "lf-gw-calllog-{name}-{}.db",
        uuid::Uuid::new_v4().simple()
    ));
    let _ = std::fs::remove_file(&p);
    p
}

fn gateway(db: &std::path::Path) -> Arc<GatewayConfig> {
    let mut dotenv = HashMap::new();
    dotenv.insert("LANGFIXER_GATEWAY".to_string(), "1".to_string());
    dotenv.insert("LANGFIXER_COOKIE_SECURE".to_string(), "0".to_string());
    dotenv.insert("LANGFIXER_ADMIN_TOKEN".to_string(), ADMIN.to_string());
    Arc::new(GatewayConfig::build(&dotenv, db).unwrap())
}

fn app(cfg: &Arc<GatewayConfig>) -> Router {
    routes::auth::router(cfg.clone())
        .merge(routes::admin::router(cfg.clone()))
        .layer(axum::middleware::from_fn_with_state(
            cfg.clone(),
            middleware::auth::gate,
        ))
}

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    bearer: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(t) = bearer {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    let req = match body {
        Some(v) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap_or_default();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// 日志行的 (token_id, endpoint, status)，按写入序。
fn log_rows(db: &std::path::Path) -> Vec<(Option<String>, String, i64)> {
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.prepare("SELECT token_id, endpoint, status FROM gw_call_log ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

#[tokio::test]
async fn call_log_records_passes_denials_and_privacy() {
    let db = temp_db("full");
    let cfg = gateway(&db);
    let app = app(&cfg);

    // 签发一枚评审密钥（超管调用，行记 NULL 身份）
    let (st, issued) = call(
        &app,
        "POST",
        "/admin/tokens",
        Some(ADMIN),
        Some(json!({"label": "评审甲", "quota_chars": 0})),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let token = issued["token"].as_str().unwrap().to_string();
    let tid = issued["id"].as_str().unwrap().to_string();

    // 匿名 whoami 门禁探测放行
    let (st, _) = call(&app, "GET", "/auth/whoami", None, None).await;
    assert_eq!(st, StatusCode::OK);
    // 匿名业务端点 401——拒绝也要有行
    let (st, _) = call(
        &app,
        "POST",
        "/gec/detect",
        None,
        Some(json!({"lang": "zh", "text": "测试"})),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    // 评审密钥越权进管理面 403——记本人
    let (st, _) = call(&app, "GET", "/admin/tokens", Some(&token), None).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    // 评审密钥正常调用，query 带「像用户文本」的参数——日志只落路径
    let (st, _) = call(
        &app,
        "GET",
        "/auth/whoami?text=%E4%BD%A0%E5%A5%BD",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    let rows = log_rows(&db);
    assert_eq!(
        rows,
        vec![
            (None, "/admin/tokens".into(), 201),
            (None, "/auth/whoami".into(), 200),
            (None, "/gec/detect".into(), 401),
            (Some(tid.clone()), "/admin/tokens".into(), 403),
            (Some(tid.clone()), "/auth/whoami".into(), 200),
        ],
        "放行/拒绝/越权逐行对齐，query 不落日志"
    );

    // 合计：此前完成的 5 行。进行中的请求**完成后才入账**（latency 要等响应成形），
    // 所以 stats 查询不含它自己——读到的永远是「此前所有调用」。
    let (st, stats) = call(&app, "GET", "/admin/stats", Some(ADMIN), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(stats["calls_retained"].as_i64(), Some(5));
    assert_eq!(stats["calls_today"].as_i64(), Some(5));

    // 逐令牌读法与合计同源（/admin/tokens 这次调用记 NULL 身份，不改评审行计数）
    let (st, list) = call(&app, "GET", "/admin/tokens", Some(ADMIN), None).await;
    assert_eq!(st, StatusCode::OK);
    let row = list["tokens"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == json!(tid))
        .unwrap();
    assert_eq!(row["calls_today"].as_i64(), Some(2), "403 + 200 两次调用");
    assert_eq!(row["calls_retained"].as_i64(), Some(2));

    drop(app);
    drop(cfg);
    let _ = std::fs::remove_file(&db);
}
