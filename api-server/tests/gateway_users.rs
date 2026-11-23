//! 用户名账号（密钥身份的口令别名）的 HTTP 层验收。
//!
//! 要守住的语义：
//! 1. 用户名是**同一枚密钥**的登录别名：web 口令登录与密钥登录是同一个身份
//!    （token_id 一致），MCP / 配额 / 历史都按密钥归属；
//! 2. 吊销密钥 → 口令登录与在途会话一并失效（孤立账号行不是登录后门）；
//! 3. 删除口令别名只删别名：密钥本身保留，MCP 仍可用；
//! 4. 自助设口令无注册流程：首次创建（用户名必填、全局唯一，重名 409），
//!    改密验原口令；自设 must_change=false，管理面代设 / 重置 must_change=true；
//! 5. 超管不走自助端点（400），走 /admin 管理面。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use api_server::{auth::GatewayConfig, middleware, routes};
use axum::http::{Request, StatusCode, header};
use axum::{Router, body::Body};
use serde_json::{Value, json};
use tower::ServiceExt;

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

fn env_admin_token() -> EnvGuard {
    let guard = match ENV_LOCK.lock() {
        Ok(g) => Some(g),
        Err(e) => Some(e.into_inner()),
    };
    unsafe { std::env::remove_var("LANGFIXER_ADMIN_TOKEN") }
    EnvGuard(guard)
}

fn temp_db(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "lf-gw-users-{name}-{}.db",
        uuid::Uuid::new_v4().simple()
    ));
    let _ = std::fs::remove_file(&p);
    p
}

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

/// 密钥直登换会话 cookie。
async fn login_token(app: &Router, token: &str) -> String {
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
    sid.expect("应下发 lf_session cookie")
}

/// 用户名口令登录；断言可给期望状态码，回响应体与新 cookie。
async fn login_password(
    app: &Router,
    username: &str,
    password: &str,
    expect: StatusCode,
) -> (Value, Option<String>) {
    let (status, body, sid) = call(
        app,
        "POST",
        "/auth/session",
        None,
        None,
        Some(json!({ "username": username, "password": password })),
    )
    .await;
    assert_eq!(status, expect, "登录 {username}: {body}");
    (body, sid)
}

/// 管理面为密钥建账号（服务端代生成临时口令），返回临时口令。
async fn admin_create_user(app: &Router, admin: &str, token_id: &str, username: &str) -> String {
    let (status, body, _) = call(
        app,
        "POST",
        "/admin/users",
        Some(admin),
        None,
        Some(json!({ "token_id": token_id, "username": username })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["password"]
        .as_str()
        .expect("代生成口令只回这一次")
        .to_string()
}

/// 签发 + 建账号一步到位：管理面带 username 签发，密码回一次。
#[tokio::test]
async fn issue_with_username_gives_working_password_login() {
    let _env = env_admin_token();
    let db = temp_db("issue-account");
    let cfg = gateway(&db);
    let app = app(&cfg);
    let admin = cfg
        .store
        .meta_get(api_server::auth::META_ADMIN_TOKEN)
        .unwrap()
        .unwrap();

    let (status, body, _) = call(
        &app,
        "POST",
        "/admin/tokens",
        Some(&admin),
        None,
        Some(json!({"label": "评审-张老师", "quota_chars": 2000000, "username": "zhang-laoshi"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let token = body["token"].as_str().unwrap().to_string();
    let temp_pw = body["password"].as_str().expect("代生成口令").to_string();
    assert_eq!(body["username"], "zhang-laoshi");
    assert_eq!(temp_pw.chars().count(), 12);

    // 临时口令登录：身份是这枚密钥（token_id 一致），并带 must_change 提示
    let (who, sid) = login_password(&app, "zhang-laoshi", &temp_pw, StatusCode::OK).await;
    let sid = sid.expect("口令登录应下发会话");
    assert_eq!(who["admin"], false);
    assert_eq!(who["token_id"], body["id"]);
    assert_eq!(who["must_change"], true);

    // 用户名登录与密钥直登是同一个身份：whoami 带 username/must_change
    let (_, w1, _) = call(&app, "GET", "/auth/whoami", None, Some(&sid), None).await;
    assert_eq!(w1["username"], "zhang-laoshi");
    assert_eq!(w1["must_change"], true);
    let sid2 = login_token(&app, &token).await;
    let (_, w2, _) = call(&app, "GET", "/auth/whoami", None, Some(&sid2), None).await;
    assert_eq!(w1["token_id"], w2["token_id"], "口令别名必须是同一身份");

    // 错口令 401；「admin」用户名永远走超管路径而不是 gw_users
    login_password(
        &app,
        "zhang-laoshi",
        "wrong-password",
        StatusCode::UNAUTHORIZED,
    )
    .await;

    // 管理面清单能对上密钥行
    let (_, list, _) = call(&app, "GET", "/admin/users", Some(&admin), None, None).await;
    let users = list["users"].as_array().unwrap();
    assert_eq!(users.len(), 1);
    assert_eq!(users[0]["username"], "zhang-laoshi");
    assert_eq!(users[0]["token_id"], body["id"]);
    assert_eq!(users[0]["revoked"], false);

    let _ = std::fs::remove_file(&db);
}

/// 吊销密钥：口令登录与在途会话一并失效——孤立账号行不得成为登录后门。
#[tokio::test]
async fn revoke_token_kills_password_login_and_sessions() {
    let _env = env_admin_token();
    let db = temp_db("revoke");
    let cfg = gateway(&db);
    let app = app(&cfg);
    let admin = cfg
        .store
        .meta_get(api_server::auth::META_ADMIN_TOKEN)
        .unwrap()
        .unwrap();

    let issued = cfg.store.issue("评审-吊销", 0).unwrap();
    let pw = admin_create_user(&app, &admin, &issued.id, "revoked-user").await;
    let (_, sid) = login_password(&app, "revoked-user", &pw, StatusCode::OK).await;
    let sid = sid.unwrap();

    let (_, body, _) = call(
        &app,
        "POST",
        &format!("/admin/tokens/{}/revoke", issued.id),
        Some(&admin),
        None,
        Some(json!({})),
    )
    .await;
    assert_eq!(body["ok"], true);

    // 口令登录 401（dead），在途会话作废，自助端点也随之拒
    login_password(&app, "revoked-user", &pw, StatusCode::UNAUTHORIZED).await;
    let (_, who, _) = call(&app, "GET", "/auth/whoami", None, Some(&sid), None).await;
    assert_eq!(who["authenticated"], false, "{who}");
    let (status, _, _) = call(
        &app,
        "POST",
        "/auth/credentials",
        None,
        Some(&sid),
        Some(json!({"new_password": "another-secret-9"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // 密钥直登同样失效
    let (status, _, _) = call(
        &app,
        "POST",
        "/auth/session",
        None,
        None,
        Some(json!({"token": issued.token})),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let _ = std::fs::remove_file(&db);
}

/// 删除口令别名只删别名：密钥保留，MCP / 密钥直登不受影响。
#[tokio::test]
async fn delete_alias_keeps_token_working() {
    let _env = env_admin_token();
    let db = temp_db("delete-alias");
    let cfg = gateway(&db);
    let app = app(&cfg);
    let admin = cfg
        .store
        .meta_get(api_server::auth::META_ADMIN_TOKEN)
        .unwrap()
        .unwrap();

    let issued = cfg.store.issue("评审-删别名", 0).unwrap();
    let pw = admin_create_user(&app, &admin, &issued.id, "alias-user").await;
    login_password(&app, "alias-user", &pw, StatusCode::OK).await;

    let (status, _, _) = call(
        &app,
        "DELETE",
        "/admin/users/alias-user",
        Some(&admin),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // 口令没了，密钥还活着
    login_password(&app, "alias-user", &pw, StatusCode::UNAUTHORIZED).await;
    login_token(&app, &issued.token).await;
    // whoami 不再带 username
    let sid = login_token(&app, &issued.token).await;
    let (_, who, _) = call(&app, "GET", "/auth/whoami", None, Some(&sid), None).await;
    assert!(
        who.get("username").is_none(),
        "删别名后 whoami 不应带 username: {who}"
    );

    let _ = std::fs::remove_file(&db);
}

/// 自助端点：首次创建（重名 409 / 用户名非法 400 / 太短 400），
/// 改密验原口令并当场换新会话；自设 must_change=false。
#[tokio::test]
async fn self_service_create_and_change() {
    let _env = env_admin_token();
    let db = temp_db("self-service");
    let cfg = gateway(&db);
    let app = app(&cfg);
    let admin = cfg
        .store
        .meta_get(api_server::auth::META_ADMIN_TOKEN)
        .unwrap()
        .unwrap();

    // 先占一个名：zhang / li 两枚密钥
    let zhang = cfg.store.issue("评审甲", 0).unwrap();
    let li = cfg.store.issue("评审乙", 0).unwrap();
    admin_create_user(&app, &admin, &zhang.id, "zhang").await;

    // 乙密钥会话自助创建；重名 409、非法名 400、口令过短 400
    let sid = login_token(&app, &li.token).await;
    let (status, body, _) = call(
        &app,
        "POST",
        "/auth/credentials",
        None,
        Some(&sid),
        Some(json!({"username": "zhang", "new_password": "li-secret-123"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "用户名全局唯一: {body}");
    let (status, _, _) = call(
        &app,
        "POST",
        "/auth/credentials",
        None,
        Some(&sid),
        Some(json!({"username": "Bad Name!", "new_password": "li-secret-123"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "用户名要过 normalize");
    let (status, _, _) = call(
        &app,
        "POST",
        "/auth/credentials",
        None,
        Some(&sid),
        Some(json!({"username": "li", "new_password": "short"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "口令至少 8 位");

    let (status, body, _) = call(
        &app,
        "POST",
        "/auth/credentials",
        None,
        Some(&sid),
        Some(json!({"username": "  LI.User  ", "new_password": "li-secret-123"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["username"], "li.user", "入库前 normalize");
    assert_eq!(body["must_change"], false, "自设口令不催改密");

    // 自设的口令可直接登录
    let (who, _) = login_password(&app, "li.user", "li-secret-123", StatusCode::OK).await;
    assert_eq!(who["must_change"], Value::Null, "false 时不带字段");

    // 改密：原口令错 401；对的 → 当场换新会话，旧口令旧会话作废
    let sid2 = login_token(&app, &li.token).await;
    let (status, _, _) = call(
        &app,
        "POST",
        "/auth/credentials",
        None,
        Some(&sid2),
        Some(json!({"old_password": "wrong-old-1", "new_password": "li-secret-456"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, body, new_sid) = call(
        &app,
        "POST",
        "/auth/credentials",
        None,
        Some(&sid2),
        Some(json!({"old_password": "li-secret-123", "new_password": "li-secret-456"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let new_sid = new_sid.expect("改密应重发会话，不用重新登录");
    let (_, who, _) = call(&app, "GET", "/auth/whoami", None, Some(&new_sid), None).await;
    assert_eq!(who["authenticated"], true);
    let (_, old, _) = call(&app, "GET", "/auth/whoami", None, Some(&sid2), None).await;
    assert_eq!(old["authenticated"], false, "旧口令的在途会话应作废");
    login_password(&app, "li.user", "li-secret-123", StatusCode::UNAUTHORIZED).await;
    login_password(&app, "li.user", "li-secret-456", StatusCode::OK).await;

    // 管理面重置：新临时口令只回一次，must_change 重新亮起来
    let (status, body, _) = call(
        &app,
        "POST",
        "/admin/users/li.user/reset",
        Some(&admin),
        None,
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let temp = body["password"].as_str().expect("重置口令回一次");
    let (who, _) = login_password(&app, "li.user", temp, StatusCode::OK).await;
    assert_eq!(who["must_change"], true, "代重置应催改密");
    // 重置清了在途会话
    let (_, dead, _) = call(&app, "GET", "/auth/whoami", None, Some(&new_sid), None).await;
    assert_eq!(dead["authenticated"], false);

    let _ = std::fs::remove_file(&db);
}

/// 超管不走自助端点；管理面签发时自备口令不回显但可用。
#[tokio::test]
async fn admin_uses_admin_plane_not_self_service() {
    let _env = env_admin_token();
    let db = temp_db("admin-path");
    let cfg = gateway(&db);
    let app = app(&cfg);
    let admin = cfg
        .store
        .meta_get(api_server::auth::META_ADMIN_TOKEN)
        .unwrap()
        .unwrap();

    // 超管会话调自助端点 → 400（超管口令是另一套体系）
    let sid = login_token(&app, &admin).await;
    let (status, body, _) = call(
        &app,
        "POST",
        "/auth/credentials",
        None,
        Some(&sid),
        Some(json!({"username": "sneaky", "new_password": "whatever-123"})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "超管不得建用户账号: {body}"
    );

    // 签发时自备口令：不回显明文，但能登录
    let (status, body, _) = call(
        &app,
        "POST",
        "/admin/tokens",
        Some(&admin),
        None,
        Some(json!({"label": "评审丙", "username": "bing", "password": "own-password-1"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(body.get("password").is_none(), "自备口令不回显: {body}");
    let (who, _) = login_password(&app, "bing", "own-password-1", StatusCode::OK).await;
    assert_eq!(who["token_id"], body["id"]);

    // 用户名撞车 → 409，且刚签发的密钥回滚（不留幽灵身份）
    let before: Vec<_> = cfg.store.list().unwrap();
    let (status, _, _) = call(
        &app,
        "POST",
        "/admin/tokens",
        Some(&admin),
        None,
        Some(json!({"label": "撞名", "username": "bing"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        cfg.store.list().unwrap().len(),
        before.len(),
        "撞名应回滚密钥"
    );

    // 非法用户名签发：400 且不落任何行
    let (status, _, _) = call(
        &app,
        "POST",
        "/admin/tokens",
        Some(&admin),
        None,
        Some(json!({"label": "非法名", "username": "admin"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "admin 是保留名");
    assert_eq!(cfg.store.list().unwrap().len(), before.len());

    let _ = std::fs::remove_file(&db);
}
