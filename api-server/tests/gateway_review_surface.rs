//! 评审身份可达面（`middleware::auth::review_denied`）的 HTTP 层验收。
//!
//! 立场：参赛密钥是**共享**身份，改写共享规则库 / 重训模型 / 起长跑评测都不属于
//! 评测通道（11-12 走查实测这些端点原先对评审密钥全开）。可达面是 fail-closed
//! 白名单，白名单之外的端点必须在**鉴权门**就被挡住，而不是等业务 handler。
//!
//! 本测试的 app 只挂 auth + admin 路由（不起 AppState，业务路由不存在），
//! 因此两种结局可以直接区分：
//! - **放行** → 进到路由 → 404（该路由在本 app 里没有）；
//! - **拒绝** → 门直接回 403 + 说明文案（压根不进路由）。
//! 所以断言「200/404 = 放行」「403 = 拒绝」，不用真起业务面。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use api_server::{auth::GatewayConfig, middleware, routes};
use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use serde_json::Value;
use tower::ServiceExt;

const ADMIN: &str = "lf-admin-token-for-surface-test-0123456789ab";

fn temp_db(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "lf-gw-surface-{name}-{}.db",
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

/// 用评审密钥发一个请求，返回状态码。
async fn as_reviewer(app: &Router, method: &str, path: &str, token: &str) -> StatusCode {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{}"))
        .unwrap();
    app.clone().oneshot(req).await.unwrap().status()
}

/// 超管走同一路径必须始终放行（白名单只约束评审身份）。
async fn as_admin(app: &Router, method: &str, path: &str) -> StatusCode {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {ADMIN}"))
        .body(Body::empty())
        .unwrap();
    app.clone().oneshot(req).await.unwrap().status()
}

#[tokio::test]
async fn reviewer_surface_is_allowlist_not_per_endpoint_deny() {
    let db = temp_db("surface");
    let cfg = gateway(&db);
    let app = app(&cfg);

    // 签发评审密钥（超管路径本就在白名单外，但超管不受白名单约束）
    let req = Request::builder()
        .method(Method::POST)
        .uri("/admin/tokens")
        .header(header::AUTHORIZATION, format!("Bearer {ADMIN}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"label":"评审甲","quota_chars":0}"#))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap_or_default(),
    )
    .unwrap();
    let token = body["token"].as_str().unwrap().to_string();

    // 1) 评测通道面放行：进到路由 = 404（本测试 app 未挂业务路由）
    for (m, p) in [
        ("POST", "/gec/detect"),
        ("POST", "/gec/zh/detect"),
        ("POST", "/gec/analyze"),
        ("POST", "/gec/explain"),
        ("POST", "/gec/explain/stream"),
        ("GET", "/gec/settings"),
        ("GET", "/gec/languages"),
        ("GET", "/gec/history"),
        ("GET", "/gec/history/7"),
        ("GET", "/gec/sessions/s1/mistakes"),
        ("POST", "/gec/sessions"),
        ("DELETE", "/gec/sessions/s1"),
        ("GET", "/gec/eval/layers"),
        ("POST", "/mcp"),
        ("GET", "/auth/whoami"),
    ] {
        let st: StatusCode = as_reviewer(&app, m, p, &token).await;
        assert_ne!(
            st,
            StatusCode::FORBIDDEN,
            "{m} {p} 属评测通道，不该被门挡住"
        );
    }
    // 语言段数校验：/gec/tables/zh/try 不是 /gec/:lang/detect，不得被顺带放行
    let try_st: StatusCode = as_reviewer(&app, "POST", "/gec/tables/zh/try", &token).await;
    assert_eq!(try_st, StatusCode::FORBIDDEN, "决策表试跑属运营面");

    // 2) 改写共享状态 / 重训 / 长跑 / 运营解锁：一律 403，且带说明文案
    for (m, p) in [
        ("POST", "/gec/l3/retrain"),
        ("POST", "/gec/l3/collect"),
        ("POST", "/gec/train/l3/retrain"),
        ("POST", "/gec/train/lexicon/apply"),
        ("POST", "/gec/agent/run"),
        ("POST", "/gec/eval/run"),
        ("POST", "/gec/reflect/run"),
        ("POST", "/gec/round/evaluate"),
        ("POST", "/gec/llm-feedback"),
        ("POST", "/gec/feedback"),
        ("POST", "/gec/feedback/action"),
        ("POST", "/gec/settings/test"),
        ("POST", "/gec/ops/unlock"),
        ("POST", "/gec/custom-words"),
        ("GET", "/gec/rules"),
        ("GET", "/gec/stats?days=7"),
        ("GET", "/gec/knowledge/sources"),
        ("PUT", "/gec/settings"),
        ("GET", "/gec/l3/status"),
    ] {
        let req = Request::builder()
            .method(m)
            .uri(p)
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{}"))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{m} {p} 必须 403");
        let body: Value = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap_or_default(),
        )
        .unwrap();
        let msg = body["error"].as_str().unwrap_or_default();
        assert!(
            msg.contains("评测通道"),
            "{m} {p} 的 403 需带评测通道说明文案，实得 {msg}"
        );
    }

    // 3) 超管不受白名单影响（同路径不得 403；本 app 里表现为 404 而不是 403）
    for (m, p) in [("POST", "/gec/l3/retrain"), ("POST", "/gec/ops/unlock")] {
        let st: StatusCode = as_admin(&app, m, p).await;
        assert_ne!(st, StatusCode::FORBIDDEN, "超管不该被白名单挡住");
    }

    drop(app);
    drop(cfg);
    let _ = std::fs::remove_file(&db);
}
