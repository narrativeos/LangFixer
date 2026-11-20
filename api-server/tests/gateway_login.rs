//! 单机形态登录门（用户名/密码）的 HTTP 层验收。
//!
//! 要守住的语义：
//! 1. 默认（不设 LANGFIXER_LOGIN）单机形态业务端点要求登录：匿名 401 + `login_required`；
//!    `/auth/*` 探测与 `/gec/deps` 体检保持公开；
//! 2. 内建 admin 账号密码登录（默认密码 LangFixer@2026）换会话 cookie，此后业务端点放行；
//! 3. `POST /auth/password` 改密需已验证超管：先落库再换内存，旧密码即废；
//! 4. 超管 Bearer 与密码登录等价（同为 admin 域会话）；
//! 5. `LANGFIXER_LOGIN=0` 回到「零变化」直通：匿名照旧全开放。

use std::collections::HashMap;

use api_server::{
    auth::{DEFAULT_ADMIN_PASSWORD, GatewayConfig},
    middleware, routes,
    state::AppState,
};
use axum::{Router, body::Body, http::Request, http::header};
use serde_json::json;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

/// 进程环境串行守卫：本文件按用例增删 LANGFIXER_*，并行测试会互踩。
static ENV_LOCK: std::sync::OnceLock<Arc<Mutex<()>>> = std::sync::OnceLock::new();

struct EnvGuard {
    // 持有到用例结束，锁才能真正串行整个用例
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

/// 装配「登录门形态」的完整 app：产品路由 + 登录路由 + 鉴权门。
/// dotenv 由用例给（进程环境已被守卫清空，map 即最终来源）。
async fn app(dotenv: &[(&str, &str)]) -> Router {
    let tables = temp_path("login-tables", "");
    std::fs::create_dir_all(&tables).unwrap();
    let settings = temp_path("login-settings", "json");
    let db = temp_path("login-db", "db");
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
    let cfg = Arc::new(GatewayConfig::build(&map, &temp_path("login-auth", "db")).unwrap());
    routes::router(state)
        .merge(routes::auth::router(cfg.clone()))
        .layer(axum::middleware::from_fn_with_state(
            cfg,
            middleware::auth::gate,
        ))
}

async fn get_json(
    app: &Router,
    path: &str,
    cookie: Option<&str>,
) -> (axum::http::StatusCode, serde_json::Value) {
    let mut req = Request::builder().uri(path).method("GET");
    if let Some(c) = cookie {
        req = req.header(header::COOKIE, c);
    }
    let resp = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

async fn post_json(
    app: &Router,
    path: &str,
    body: serde_json::Value,
    cookie: Option<&str>,
) -> (axum::http::StatusCode, serde_json::Value, Option<String>) {
    let mut req = Request::builder()
        .uri(path)
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(c) = cookie {
        req = req.header(header::COOKIE, c);
    }
    let resp = app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(';').next().unwrap_or_default().trim().to_string());
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json, cookie)
}

async fn login(
    app: &Router,
    username: &str,
    password: &str,
) -> (axum::http::StatusCode, Option<String>) {
    let (status, _json, cookie) = post_json(
        app,
        "/auth/session",
        json!({ "username": username, "password": password }),
        None,
    )
    .await;
    (status, cookie)
}

#[tokio::test]
async fn deps_health_get_public_but_process_spawn_posts_require_login() {
    let _g = env_guard();
    let app = app(&[]).await;
    // 体检快照保持公开（保活探测用）
    let (status, _) = get_json(&app, "/gec/deps", None).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    // 同前缀下的 POST 会拉起本机进程，不得匿名可达
    let (status, body, _) =
        post_json(&app, "/gec/deps/start", json!({ "id": "decision" }), None).await;
    assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED, "{body}");
    let (status, _, _) = post_json(&app, "/gec/deps/recheck", json!({}), None).await;
    assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn login_bruteforce_locks_after_five_failures() {
    let _g = env_guard();
    let app = app(&[]).await;
    for i in 0..5 {
        let (status, _) = login(&app, "admin", &format!("wrong-{i}")).await;
        assert_eq!(
            status,
            axum::http::StatusCode::UNAUTHORIZED,
            "第 {i} 次失败应仍回 401"
        );
    }
    // 封锁窗口内：正确密码也拒（否则限流形同虚设）
    let (status, _) = login(&app, "admin", DEFAULT_ADMIN_PASSWORD).await;
    assert_eq!(status, axum::http::StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn change_password_rotates_caller_session() {
    let _g = env_guard();
    let app = app(&[]).await;
    let (_, old_cookie) = login(&app, "admin", DEFAULT_ADMIN_PASSWORD).await;
    let old_cookie = old_cookie.expect("登录必须下发 lf_session cookie");
    // 改密当场重发新会话（旧会话被 rotate 连坐作废）
    let (status, _body, new_cookie) = post_json(
        &app,
        "/auth/password",
        json!({ "old_password": DEFAULT_ADMIN_PASSWORD, "new_password": "FreshPass@12345" }),
        Some(&old_cookie),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    let new_cookie = new_cookie.expect("改密响应必须重发 lf_session");
    // 旧 cookie 即刻失效（旧密码泄露后的改密才真正止血）
    let (status, _) = get_json(&app, "/gec/tables", Some(&old_cookie)).await;
    assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED);
    // 调用者自己的会话无缝存活，不用重新登录
    let (status, _) = get_json(&app, "/gec/tables", Some(&new_cookie)).await;
    assert_eq!(status, axum::http::StatusCode::OK);
}

#[tokio::test]
async fn change_password_rejects_default_and_short() {
    let _g = env_guard();
    let app = app(&[]).await;
    let (_, cookie) = login(&app, "admin", DEFAULT_ADMIN_PASSWORD).await;
    let cookie = cookie.expect("登录必须下发 lf_session cookie");
    // 默认密码本身拒用（它写在文档里，等于没有口令）
    let (status, _, _) = post_json(
        &app,
        "/auth/password",
        json!({ "old_password": DEFAULT_ADMIN_PASSWORD, "new_password": DEFAULT_ADMIN_PASSWORD }),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
    // 长度按字符计：三个汉字（9 字节）不再冒充「至少 8 位」
    let (status, _, _) = post_json(
        &app,
        "/auth/password",
        json!({ "old_password": DEFAULT_ADMIN_PASSWORD, "new_password": "密码口" }),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
    // 拒绝性改密后旧密码仍可用（没改成就什么都没变）
    let (status, _) = login(&app, "admin", DEFAULT_ADMIN_PASSWORD).await;
    assert_eq!(status, axum::http::StatusCode::OK);
}

#[tokio::test]
async fn default_login_gate_blocks_business_but_keeps_probe_public() {
    let _g = env_guard();
    let app = app(&[]).await;
    // 匿名业务端点：401 + login_required（前端据此回门禁页）
    let (status, body) = get_json(&app, "/gec/tables", None).await;
    assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED);
    assert_eq!(body["login_required"], true);
    // whoami 永远可达，且如实报告「要锁 + 可密码登录」
    let (status, who) = get_json(&app, "/auth/whoami", None).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(who["locked"], true);
    assert_eq!(who["password_login"], true);
    assert_eq!(who["authenticated"], false);
    // 依赖体检面不得被登录拦住（重启保活的探查看它）
    let (status, _) = get_json(&app, "/gec/deps", None).await;
    assert_eq!(status, axum::http::StatusCode::OK);
}

#[tokio::test]
async fn password_login_round_trip() {
    let _g = env_guard();
    let app = app(&[]).await;
    // 错密码 / 错用户名都拒
    let (status, _) = login(&app, "admin", "wrong-password").await;
    assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED);
    let (status, _) = login(&app, "root", DEFAULT_ADMIN_PASSWORD).await;
    assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED);
    // 默认账号密码登录：admin / LangFixer@2026 → 会话 cookie，业务端点放行
    let (status, cookie) = login(&app, "admin", DEFAULT_ADMIN_PASSWORD).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    let cookie = cookie.expect("登录必须下发 lf_session cookie");
    assert!(cookie.starts_with("lf_session="));
    let (status, _) = get_json(&app, "/gec/tables", Some(&cookie)).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    // 登录后 whoami 已验证且是超管域
    let (_, who) = get_json(&app, "/auth/whoami", Some(&cookie)).await;
    assert_eq!(who["authenticated"], true);
    assert_eq!(who["admin"], true);
}

#[tokio::test]
async fn change_password_invalidates_old() {
    let _g = env_guard();
    let app = app(&[]).await;
    let (_, cookie) = login(&app, "admin", DEFAULT_ADMIN_PASSWORD).await;
    let cookie = cookie.expect("登录必须下发 lf_session cookie");
    // 未登录不得改密
    let (status, _, _) = post_json(
        &app,
        "/auth/password",
        json!({ "old_password": DEFAULT_ADMIN_PASSWORD, "new_password": "FreshPass@12345" }),
        None,
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED);
    // 已验证超管：旧密码核验通过才换
    let (status, body, _) = post_json(
        &app,
        "/auth/password",
        json!({ "old_password": DEFAULT_ADMIN_PASSWORD, "new_password": "FreshPass@12345" }),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    // 旧密码即废，新密码可用
    let (status, _) = login(&app, "admin", DEFAULT_ADMIN_PASSWORD).await;
    assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED);
    let (status, _) = login(&app, "admin", "FreshPass@12345").await;
    assert_eq!(status, axum::http::StatusCode::OK);
}

#[tokio::test]
async fn admin_token_bearer_passes_gate() {
    let _g = env_guard();
    let token = "k".repeat(40);
    let app = app(&[("LANGFIXER_ADMIN_TOKEN", &token)]).await;
    // Bearer 在 gate 内直接核验，不需要额外装配
    let req = Request::builder()
        .uri("/gec/tables")
        .method("GET")
        .header(
            header::AUTHORIZATION,
            header::HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        )
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::OK);
}

#[tokio::test]
async fn login_gate_can_be_disabled_for_zero_change() {
    let _g = env_guard();
    let app = app(&[("LANGFIXER_LOGIN", "0")]).await;
    // 显式关闭：回到接入鉴权前的直通（匿名不拒）
    let (status, _) = get_json(&app, "/gec/tables", None).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    let (_, who) = get_json(&app, "/auth/whoami", None).await;
    assert_eq!(who["locked"], false);
}
