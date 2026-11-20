//! 显式轮换超级管理员密钥的 HTTP 层验收（`POST /admin/admin-token/rotate`）。
//!
//! 要守住的语义：**换钥匙是一个动作**，不是重启 / 改文件的副作用——
//! 1. 必须带 `{"confirm":"ROTATE"}`，否则 400（不接受隐式请求）；
//! 2. 密钥来自 `gw_meta`（安装生成）→ 200，新值立刻可用、旧值与旧超管会话立刻失效，
//!    重启复用新值；评审密钥与其会话不受影响；
//! 3. 密钥来自 `LANGFIXER_ADMIN_TOKEN` → **409**，库里不得被改写（否则出现
//!    「gw_meta 换了一把但 env 那把仍然赢」的分裂）；
//! 4. 评审密钥不得进入管理面（403）；
//! 5. `whoami` 的来源 / 指纹只回给已验证的超管，匿名探测不得泄露。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use api_server::{auth::GatewayConfig, middleware, routes};
use axum::http::{Request, StatusCode, header};
use axum::{Router, body::Body};
use serde_json::{Value, json};
use tower::ServiceExt;

/// 进程环境变量锁：`lookup` 是「进程环境优先」，本文件要精确控制
/// `LANGFIXER_ADMIN_TOKEN` 在与不在，必须串行。
static ENV_LOCK: Mutex<()> = Mutex::new(());

struct EnvGuard(Option<std::sync::MutexGuard<'static, ()>>);

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if self.0.is_some() {
            unsafe {
                std::env::remove_var("LANGFIXER_ADMIN_TOKEN");
                std::env::remove_var("LANGFIXER_GATEWAY");
            }
        }
    }
}

/// 设（或不设）进程环境里的超管密钥并返回串行守卫。
fn env_admin_token(value: Option<&str>) -> EnvGuard {
    let guard = match ENV_LOCK.lock() {
        Ok(g) => Some(g),
        Err(e) => Some(e.into_inner()),
    };
    unsafe {
        match value {
            Some(v) => std::env::set_var("LANGFIXER_ADMIN_TOKEN", v),
            None => std::env::remove_var("LANGFIXER_ADMIN_TOKEN"),
        }
    }
    EnvGuard(guard)
}

fn temp_db(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "lf-gw-rotate-{name}-{}.db",
        uuid::Uuid::new_v4().simple()
    ));
    let _ = std::fs::remove_file(&p);
    p
}

/// 网关形态配置（Cookie 明文，便于本机断言）。
fn gateway(db: &std::path::Path) -> Arc<GatewayConfig> {
    let mut dotenv = HashMap::new();
    dotenv.insert("LANGFIXER_GATEWAY".to_string(), "1".to_string());
    dotenv.insert("LANGFIXER_COOKIE_SECURE".to_string(), "0".to_string());
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

/// 发一个请求，返回（状态码，JSON 体，Set-Cookie 里的会话 id）。
async fn call(
    app: &Router,
    method: &str,
    path: &str,
    bearer: Option<&str>,
    cookie: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value, Option<String>) {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(t) = bearer {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    if let Some(sid) = cookie {
        builder = builder.header(header::COOKIE, format!("lf_session={sid}"));
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
    let set_cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .and_then(|v| v.strip_prefix("lf_session="))
        .map(str::to_string);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap_or_default();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json, set_cookie)
}

/// 免注册登录：一枚密钥换一枚会话 cookie。
async fn login(app: &Router, token: &str) -> String {
    let (status, body, sid) = call(
        app,
        "POST",
        "/auth/session",
        None,
        None,
        Some(json!({ "token": token })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    sid.expect("登录应下发 lf_session cookie")
}

fn sha8(token: &str) -> String {
    use sha2::{Digest as _, Sha256};
    Sha256::digest(token.as_bytes())[..4]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// 生成来源：不带 confirm 一律 400；带 confirm 换钥匙后旧值 / 旧超管会话即刻失效，
/// 评审密钥不受影响，重启复用新值。
#[tokio::test]
async fn rotate_generated_mode_swaps_key_and_drops_admin_sessions() {
    let _env = env_admin_token(None);
    let db = temp_db("generated");
    let cfg = gateway(&db);
    let app = app(&cfg);
    let old = cfg
        .store
        .meta_get(api_server::auth::META_ADMIN_TOKEN)
        .unwrap()
        .expect("安装时应生成超管密钥");
    // 顺带一枚评审密钥：轮换不得波及它
    let issued = cfg.store.issue("评审甲", 0).unwrap();

    // 用旧密钥登录，拿到超管会话（轮换后这条会话必须作废）
    let sid = login(&app, &old).await;
    let (status, body, _) = call(
        &app,
        "POST",
        "/admin/admin-token/rotate",
        None,
        Some(&sid),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "空 body 不算确认: {body}");
    let (status, body, _) = call(
        &app,
        "POST",
        "/admin/admin-token/rotate",
        None,
        Some(&sid),
        Some(json!({"confirm": "rotate"})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "确认字面量大小写敏感: {body}"
    );
    // 上面两次都没换成钥匙
    assert!(cfg.admin.matches(&old));

    let (status, body, _) = call(
        &app,
        "POST",
        "/admin/admin-token/rotate",
        None,
        Some(&sid),
        Some(json!({"confirm": "ROTATE"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let fresh = body["token"]
        .as_str()
        .expect("新明文只回这一次")
        .to_string();
    assert!(fresh.starts_with("lf_") && fresh.len() == 43, "{fresh}");
    assert_eq!(body["source"], "generated");
    assert_eq!(body["fingerprint"], sha8(&fresh));
    assert_ne!(fresh, old);

    // 落库的是新值；内存摘要同一把
    assert_eq!(
        cfg.store
            .meta_get(api_server::auth::META_ADMIN_TOKEN)
            .unwrap()
            .as_deref(),
        Some(fresh.as_str())
    );
    assert!(cfg.admin.matches(&fresh) && !cfg.admin.matches(&old));

    // 旧密钥登录 401；新密钥登录 200
    let (status, _, _) = call(
        &app,
        "POST",
        "/auth/session",
        None,
        None,
        Some(json!({"token": old})),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "旧钥匙应即刻失效");
    let _ = login(&app, &fresh).await;

    // 旧超管会话作废（cookie 是旧钥匙换来的，不能成为旧钥匙的后门）
    let (_, who, _) = call(&app, "GET", "/auth/whoami", None, Some(&sid), None).await;
    assert_eq!(who["authenticated"], false, "轮换后旧超管会话应失效: {who}");

    // 评审密钥不受影响
    let (status, _, _) = call(
        &app,
        "POST",
        "/auth/session",
        None,
        None,
        Some(json!({"token": issued.token})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "评审密钥不应被轮换波及");

    // 重启复用新值（不漂移）
    let again = gateway(&db);
    assert!(again.admin.matches(&fresh) && !again.admin.matches(&old));
    assert_eq!(
        again.admin_source,
        api_server::auth::AdminTokenSource::Generated
    );
    assert_eq!(
        again.admin.fingerprint().as_deref(),
        Some(sha8(&fresh).as_str())
    );

    let _ = std::fs::remove_file(&db);
}

/// env 来源：轮换一律 409，库里那枚不得被改写。
#[tokio::test]
async fn rotate_refused_when_key_comes_from_env() {
    let env_token = "k".repeat(64);
    let _env = env_admin_token(Some(&env_token));
    let db = temp_db("env-mode");
    let cfg = gateway(&db);
    let app = app(&cfg);

    let (status, body, _) = call(
        &app,
        "POST",
        "/admin/admin-token/rotate",
        Some(&env_token),
        None,
        Some(json!({"confirm": "ROTATE"})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "env 持有的钥匙应拒绝轮换: {body}"
    );
    let err = body["error"].as_str().unwrap_or_default();
    assert!(
        err.contains("LANGFIXER_ADMIN_TOKEN"),
        "409 要说明这把钥匙归谁: {err}"
    );
    // 分裂状态没出现：库里没被写进新值，env 那把仍然赢
    assert_eq!(
        cfg.store
            .meta_get(api_server::auth::META_ADMIN_TOKEN)
            .unwrap(),
        None,
        "env 模式不得写 gw_meta.admin_token"
    );
    assert!(cfg.admin.matches(&env_token));

    // whoami 如实回来源（已验证超管才回）
    let (_, who, _) = call(&app, "GET", "/auth/whoami", Some(&env_token), None, None).await;
    assert_eq!(who["admin"], true);
    assert_eq!(who["admin_token_source"], "env");
    assert_eq!(who["admin_token_fp"], sha8(&env_token));

    let _ = std::fs::remove_file(&db);
}

/// 评审密钥进不了管理面（403），未登录探测不回来源 / 指纹。
#[tokio::test]
async fn reviewer_cannot_rotate_and_anonymous_sees_no_key_origin() {
    let _env = env_admin_token(None);
    let db = temp_db("forbidden");
    let cfg = gateway(&db);
    let app = app(&cfg);
    let issued = cfg.store.issue("评审乙", 0).unwrap();

    let (status, _, _) = call(
        &app,
        "POST",
        "/admin/admin-token/rotate",
        Some(&issued.token),
        None,
        Some(json!({"confirm": "ROTATE"})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "评审密钥不得进管理面");
    // 密钥原样
    let stored = cfg
        .store
        .meta_get(api_server::auth::META_ADMIN_TOKEN)
        .unwrap()
        .unwrap();
    assert!(cfg.admin.matches(&stored));

    // 匿名 whoami：200 + authenticated=false，且不带来源 / 指纹字段
    let (status, who, _) = call(&app, "GET", "/auth/whoami", None, None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(who["authenticated"], false);
    assert!(
        who.get("admin_token_source").is_none() && who.get("admin_token_fp").is_none(),
        "匿名不得看到密钥来源/指纹: {who}"
    );

    let _ = std::fs::remove_file(&db);
}
