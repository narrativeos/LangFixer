//! 首屏 admin 初始化 + 邀请码注册的 HTTP 层验收。
//!
//! 要守住的语义：
//! 1. 新装机（网关形态）管理员口令**未初始化**：whoami 回 `admin_initialized=false`，
//!    业务面照旧要凭证；首屏用安装时打印的超管密钥一次性设置 admin 口令，
//!    成功后当场拿到会话 cookie，重复初始化 409 `already_initialized`；
//! 2. 初始化后的口令随库持久（重启重建仍认），旧装机（曾种默认口令的）不走 init；
//! 3. 邀请码：管理面生成（明文只回一次）→ 匿名激活页自定用户名口令 →
//!    新账号 = 新密钥 + gw_users 别名，cookie 直接可用；
//! 4. 一次有效：重放 409 `invite_used`；重名 409 `username_taken` 且码放回；
//!    过期 409 `invite_expired`；撤销后 409 `invite_invalid`；
//! 5. 409 错误体必须是 `{error: 机器码, message}` 形状（前端按 error 分支）。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use api_server::auth::{DEFAULT_ADMIN_PASSWORD, GatewayConfig, META_ADMIN_TOKEN};
use api_server::{middleware, routes};
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
        "lf-gw-init-invite-{name}-{}.db",
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

#[tokio::test]
async fn gateway_init_then_invite_lifecycle() {
    let _g = env_admin_token();
    let db = temp_db("lifecycle");
    let cfg = gateway(&db);
    let admin_token = cfg.store.meta_get(META_ADMIN_TOKEN).unwrap().unwrap();
    let app = app(&cfg);

    // 1. 未初始化：匿名 whoami 回 admin_initialized=false（门禁页据此走「初始化」）
    let (st, who, _) = call(&app, "GET", "/auth/whoami", None, None, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(who["gateway"], json!(true));
    assert_eq!(who["admin_initialized"], json!(false));
    assert_eq!(who["password_login"], json!(false));

    // 2. 密钥不对：401，且不能留下任何口令
    let init_body = |token: &str, password: &str| json!({ "token": token, "password": password });
    let (st, _, sid) = call(
        &app,
        "POST",
        "/auth/admin/init",
        None,
        None,
        Some(init_body(&"x".repeat(64), "GoodPass123")),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    assert!(sid.is_none());
    assert!(!cfg.admin_initialized());

    // 3. 口令太短 / 等于历史默认口令：400
    let (st, _, _) = call(
        &app,
        "POST",
        "/auth/admin/init",
        None,
        None,
        Some(init_body(&admin_token, "short")),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _, _) = call(
        &app,
        "POST",
        "/auth/admin/init",
        None,
        None,
        Some(init_body(&admin_token, DEFAULT_ADMIN_PASSWORD)),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    // 4. 用户名不是 admin：400（首个管理员账号用户名固定）
    let (st, _, _) = call(
        &app,
        "POST",
        "/auth/admin/init",
        None,
        None,
        Some(json!({"token": admin_token, "password": "GoodPass123", "username": "root"})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert!(!cfg.admin_initialized());

    // 5. 成功：200 + 会话 cookie + username=admin
    const ADMIN_PW: &str = "InitPass#2026";
    let (st, body, sid) = call(
        &app,
        "POST",
        "/auth/admin/init",
        None,
        None,
        Some(init_body(&admin_token, ADMIN_PW)),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["ok"], json!(true));
    assert_eq!(body["username"], json!("admin"));
    let admin_sid = sid.expect("初始化成功应当场发会话 cookie");
    assert!(cfg.verify_password("admin", ADMIN_PW));
    assert!(cfg.admin_initialized());

    // 6. cookie 身份：超管 + 已登录 + 已初始化
    let (st, who, _) = call(&app, "GET", "/auth/whoami", None, Some(&admin_sid), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(who["admin"], json!(true));
    assert_eq!(who["authenticated"], json!(true));
    assert_eq!(who["admin_initialized"], json!(true));

    // 7. 重复初始化：409 already_initialized（错误体带机器码）
    let (st, body, _) = call(
        &app,
        "POST",
        "/auth/admin/init",
        None,
        None,
        Some(init_body(&admin_token, "AnotherPass#99")),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert_eq!(body["error"], json!("already_initialized"));
    assert!(body["message"].is_string());

    // 8. 重启重建：初始化状态与口令随库持久
    let cfg2 = gateway(&db);
    assert!(cfg2.admin_initialized());
    assert!(cfg2.verify_password("admin", ADMIN_PW));

    // 9. 管理面生成邀请码（Bearer 超管密钥）：201，明文 code 只回这一次
    let (st, inv, _) = call(
        &app,
        "POST",
        "/admin/invites",
        Some(&admin_token),
        None,
        Some(json!({"label": "给爱丽丝", "quota_chars": 0})),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let code = inv["code"].as_str().unwrap().to_string();
    assert!(code.starts_with("lfi_"), "邀请码格式 {code}");
    assert_eq!(inv["quota_chars"], json!(0));
    assert!(inv["expires_at"].as_i64().unwrap() > 0);

    // 10. 匿名激活：新用户名口令 → cookie + whoami 身份闭合
    const ALICE_PW: &str = "AlicePass#77";
    let (st, act, sid) = call(
        &app,
        "POST",
        "/auth/invite/activate",
        None,
        None,
        Some(json!({"code": code, "username": "alice", "password": ALICE_PW})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(act["username"], json!("alice"));
    let alice_sid = sid.expect("激活成功当场发会话 cookie");
    let alice_tid = act["token_id"].as_str().unwrap().to_string();
    let (st, who, _) = call(&app, "GET", "/auth/whoami", None, Some(&alice_sid), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(who["authenticated"], json!(true));
    assert_eq!(who["admin"], json!(false));
    assert_eq!(who["token_id"], json!(alice_tid));
    assert_eq!(who["username"], json!("alice"));
    // 口令登录可用（别名语义与手工建号一致）
    let (st, _, _) = call(
        &app,
        "POST",
        "/auth/session",
        None,
        None,
        Some(json!({"username": "alice", "password": ALICE_PW})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    // 11. 同一枚码重放：409 invite_used
    let (st, body, _) = call(
        &app,
        "POST",
        "/auth/invite/activate",
        None,
        None,
        Some(json!({"code": code, "username": "bob", "password": "BobPass#1234"})),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert_eq!(body["error"], json!("invite_used"));
    // 用户名没被吃掉：bob 尚不存在
    assert!(cfg.store.get_user("bob").unwrap().is_none());

    // 12. 重名：409 username_taken，且码放回可换名重试
    let (st, inv2, _) = call(
        &app,
        "POST",
        "/admin/invites",
        Some(&admin_token),
        None,
        Some(json!({})),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let code2 = inv2["code"].as_str().unwrap().to_string();
    let (st, body, _) = call(
        &app,
        "POST",
        "/auth/invite/activate",
        None,
        None,
        Some(json!({"code": code2, "username": "alice", "password": "ZoePass#1234"})),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert_eq!(body["error"], json!("username_taken"));
    // 码已放回：换用户名继续用同一枚码成功
    let (st, _, zoe_sid) = call(
        &app,
        "POST",
        "/auth/invite/activate",
        None,
        None,
        Some(json!({"code": code2, "username": "zoe", "password": "ZoePass#1234"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let zoe_sid = zoe_sid.expect("换名重试应当场发 cookie");
    let (st, who, _) = call(&app, "GET", "/auth/whoami", None, Some(&zoe_sid), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(who["username"], json!("zoe"));

    // 13. 过期：409 invite_expired（把库里的到期时间拨到过去）
    let (st, inv3, _) = call(
        &app,
        "POST",
        "/admin/invites",
        Some(&admin_token),
        None,
        Some(json!({"label": "快过期"})),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let code3 = inv3["code"].as_str().unwrap().to_string();
    let id3 = inv3["id"].as_str().unwrap().to_string();
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute(
            "UPDATE gw_invites SET expires_at = 1 WHERE id = ?1",
            [id3.as_str()],
        )
        .unwrap();
    }
    let (st, body, _) = call(
        &app,
        "POST",
        "/auth/invite/activate",
        None,
        None,
        Some(json!({"code": code3, "username": "tim", "password": "TimPass#1234"})),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert_eq!(body["error"], json!("invite_expired"));

    // 14. 撤销（DELETE）后：409 invite_invalid
    let (st, inv4, _) = call(
        &app,
        "POST",
        "/admin/invites",
        Some(&admin_token),
        None,
        Some(json!({"label": "会被撤"})),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let code4 = inv4["code"].as_str().unwrap().to_string();
    let id4 = inv4["id"].as_str().unwrap().to_string();
    let (st, _, _) = call(
        &app,
        "DELETE",
        &format!("/admin/invites/{id4}"),
        Some(&admin_token),
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, body, _) = call(
        &app,
        "POST",
        "/auth/invite/activate",
        None,
        None,
        Some(json!({"code": code4, "username": "ann", "password": "AnnPass#1234"})),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert_eq!(body["error"], json!("invite_invalid"));
    // 撤销不存在的码：404
    let (st, _, _) = call(
        &app,
        "DELETE",
        "/admin/invites/inv_nope",
        Some(&admin_token),
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // 15. 列表：已用/未用的都在，带 used_at/used_by
    let (st, list, _) = call(
        &app,
        "GET",
        "/admin/invites",
        Some(&admin_token),
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let invites = list["invites"].as_array().unwrap();
    assert!(invites.iter().any(|i| i["used_by"] == json!("alice")));
    assert!(invites.iter().any(|i| i["label"] == json!("快过期")));
}

#[tokio::test]
async fn uninitialized_invite_quota_flows_into_new_token() {
    // 带配额的邀请码：新账号密钥的配额 = 邀请码配额（charge 按密钥行，口径统一）
    let _g = env_admin_token();
    let db = temp_db("quota");
    let cfg = gateway(&db);
    let admin_token = cfg.store.meta_get(META_ADMIN_TOKEN).unwrap().unwrap();
    let app = app(&cfg);
    let (st, inv, _) = call(
        &app,
        "POST",
        "/admin/invites",
        Some(&admin_token),
        None,
        Some(json!({"label": "限 100 万字符", "quota_chars": 1_000_000})),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let code = inv["code"].as_str().unwrap().to_string();
    let (st, act, _) = call(
        &app,
        "POST",
        "/auth/invite/activate",
        None,
        None,
        Some(json!({"code": code, "username": "max", "password": "MaxPass#1234"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let tid = act["token_id"].as_str().unwrap().to_string();
    let quota: i64 = {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.query_row(
            "SELECT quota_chars FROM access_tokens WHERE id = ?1",
            [tid.as_str()],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(quota, 1_000_000);
}
