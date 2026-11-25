//! 单机形态管理面 fail-closed 的 HTTP 层验收（2026-11 安全修复）。
//!
//! 要守住的语义：
//! 1. `LANGFIXER_LOGIN=0`「零变化」直台下，业务端点照旧匿名开放，但 `/admin/*`
//!    必须带凭证：匿名一律 401 + `login_required`，超管 Bearer 放行；
//! 2. 默认登录门形态下匿名 `/admin/*` 同样 401（回归钉死，防直通规则改动带偏）；
//! 3. 匿名签发密钥（`POST /admin/tokens`）在两种形态下都不得落库。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use api_server::{auth::GatewayConfig, middleware, routes, state::AppState};
use axum::{
    Router,
    body::Body,
    http::{Request, header},
};
use serde_json::json;
use tower::ServiceExt;

const ADMIN: &str = "lf-admin-token-for-admin-gate-test-0123456789ab";

/// 进程环境串行守卫：`lookup` 进程环境优先，必须清干净再装配。
static ENV_LOCK: std::sync::OnceLock<Arc<Mutex<()>>> = std::sync::OnceLock::new();

struct EnvGuard {
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        unsafe {
            for k in [
                "LANGFIXER_GATEWAY",
                "LANGFIXER_LOGIN",
                "LANGFIXER_ADMIN_TOKEN",
            ] {
                std::env::remove_var(k);
            }
        }
    }
}

fn env_guard() -> EnvGuard {
    let lock = ENV_LOCK.get_or_init(|| Arc::new(Mutex::new(())));
    let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        for k in [
            "LANGFIXER_GATEWAY",
            "LANGFIXER_LOGIN",
            "LANGFIXER_ADMIN_TOKEN",
        ] {
            std::env::remove_var(k);
        }
    }
    EnvGuard { _guard }
}

fn temp_path(prefix: &str, ext: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir().join(format!("lf-{prefix}-{}", uuid::Uuid::new_v4().simple()));
    if !ext.is_empty() {
        p.set_extension(ext);
    }
    let _ = std::fs::remove_file(&p);
    p
}

/// 装配带管理面的单机 app：产品路由 + 登录路由 + 管理路由 + 鉴权门。
/// 与 `main.rs` 的装配一致（mcp 面与本用例无关，不挂）。
async fn app(dotenv: &[(&str, &str)]) -> Router {
    let tables = temp_path("ag-tables", "");
    std::fs::create_dir_all(&tables).unwrap();
    let settings = temp_path("ag-settings", "json");
    let db = temp_path("ag-db", "db");
    unsafe {
        std::env::set_var("GEC_SETTINGS_FILE", &settings);
        std::env::remove_var("GEC_LEXICONS_DIR");
    }
    let state = Arc::new(AppState::from_env(Some(&tables), Some(&db)));
    let mut map = HashMap::new();
    map.insert("LANGFIXER_COOKIE_SECURE".to_string(), "0".to_string());
    for (k, v) in dotenv {
        map.insert((*k).to_string(), (*v).to_string());
    }
    let cfg = Arc::new(GatewayConfig::build(&map, &temp_path("ag-auth", "db")).unwrap());
    routes::router(state)
        .merge(routes::auth::router(cfg.clone()))
        .merge(routes::admin::router(cfg.clone()))
        .layer(axum::middleware::from_fn_with_state(
            cfg,
            middleware::auth::gate,
        ))
}

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    bearer: Option<&str>,
    body: Option<serde_json::Value>,
) -> (axum::http::StatusCode, serde_json::Value) {
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
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

#[tokio::test]
async fn admin_surface_stays_locked_under_zero_change_passthrough() {
    let _g = env_guard();
    let app = app(&[("LANGFIXER_LOGIN", "0"), ("LANGFIXER_ADMIN_TOKEN", ADMIN)]).await;

    // 业务端点直通语义不回归（这正是 LANGFIXER_LOGIN=0 的既有契约）
    let (status, _) = call(&app, "GET", "/gec/tables", None, None).await;
    assert_eq!(status, axum::http::StatusCode::OK);

    // 但管理面不因直通敞开：匿名概览/签发一律 401 + login_required
    let (status, body) = call(&app, "GET", "/admin/stats", None, None).await;
    assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED);
    assert_eq!(body["login_required"], true);

    let (status, _) = call(
        &app,
        "POST",
        "/admin/tokens",
        None,
        Some(json!({"label": "attacker", "quota_chars": 0})),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED);

    // 超管凭证照常放行：签发、列表、概览
    let (status, issued) = call(
        &app,
        "POST",
        "/admin/tokens",
        Some(ADMIN),
        Some(json!({"label": "onedesk", "quota_chars": 0})),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::CREATED);
    assert!(
        issued["token"]
            .as_str()
            .is_some_and(|t| t.starts_with("lf_"))
    );

    let (status, _) = call(&app, "GET", "/admin/stats", Some(ADMIN), None).await;
    assert_eq!(status, axum::http::StatusCode::OK);
}

#[tokio::test]
async fn admin_surface_locked_under_default_login_gate() {
    let _g = env_guard();
    let app = app(&[]).await;

    // 登录门形态：匿名业务端点与管理端点都 401；/auth/* 探测保持公开
    let (status, body) = call(&app, "GET", "/admin/stats", None, None).await;
    assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED);
    assert_eq!(body["login_required"], true);

    let (status, body) = call(&app, "GET", "/gec/tables", None, None).await;
    assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED);
    assert_eq!(body["login_required"], true);

    let (status, _) = call(&app, "GET", "/auth/whoami", None, None).await;
    assert_eq!(status, axum::http::StatusCode::OK);
}
