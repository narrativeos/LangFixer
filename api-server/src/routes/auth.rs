//! 免注册登录面：一枚令牌（评审或超管）换一枚短时效会话 cookie，
//! 无注册、无找回密码、无用户资料。产品界面在验证通过前不展示（前端门禁页）。

use std::sync::Arc;

use axum::extract::{ConnectInfo, Extension, State};
use axum::http::header::{HeaderMap, SET_COOKIE};
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;

use crate::auth::{
    ADMIN_TOKEN_ID, AuthContext, GatewayConfig, META_OWNER_CLAIMED, RequestScope, SESSION_TTL_SECS,
};
use crate::routes::ApiError;

pub fn router(cfg: Arc<GatewayConfig>) -> Router {
    Router::new()
        .route("/auth/session", post(create_session))
        .route("/auth/logout", post(logout))
        .route("/auth/whoami", axum::routing::get(whoami))
        .route("/auth/password", post(change_password))
        .with_state(cfg)
}

#[derive(Deserialize)]
struct SessionReq {
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    password: Option<String>,
}

#[derive(Deserialize)]
struct PasswordReq {
    old_password: String,
    new_password: String,
}

async fn create_session(
    State(cfg): State<Arc<GatewayConfig>>,
    peer: Option<ConnectInfo<std::net::SocketAddr>>,
    headers: HeaderMap,
    Json(body): Json<SessionReq>,
) -> Result<impl IntoResponse, ApiError> {
    // 登录失败限流（按来源 IP）：连续失败封 60s，挡住脚本暴破；成功登录清零。
    let ip = client_ip(&headers, peer.as_ref().map(|c| &c.0));
    if login_locked(&cfg, &ip) {
        return Err(ApiError::TooManyRequests(
            "登录尝试过于频繁，请 60 秒后重试".into(),
        ));
    }
    let (tid, label, admin) = if let Some(token) = body.token.as_deref() {
        if cfg.admin.matches(token) {
            (ADMIN_TOKEN_ID.to_string(), "管理员".to_string(), true)
        } else if let Some(info) = cfg.store.verify_token(token).map_err(ApiError::Internal)? {
            (info.id, info.label, false)
        } else {
            note_login_failure(&cfg, &ip);
            return Err(ApiError::Unauthorized("令牌无效或已吊销".into()));
        }
    } else if body.password.is_some() {
        // 用户名密码登录：只认内建的 admin 账号（无注册、无多用户）
        if cfg.verify_password(
            body.username.as_deref().unwrap_or(""),
            body.password.as_deref().unwrap_or(""),
        ) {
            (ADMIN_TOKEN_ID.to_string(), "管理员".to_string(), true)
        } else {
            note_login_failure(&cfg, &ip);
            return Err(ApiError::Unauthorized("用户名或密码错误".into()));
        }
    } else {
        return Err(ApiError::BadRequest(
            "需要提供 token 或 username/password".into(),
        ));
    };
    // 单机形态没有评审令牌体系：只有超管密钥能建会话
    if !cfg.enabled && !admin {
        return Err(ApiError::Unauthorized(
            "单机形态仅支持超级管理员密钥".into(),
        ));
    }
    // 单机形态首次超管登录 = owner 激活：设置写入自此要求登录态（此前保持零变化）
    if admin && !cfg.enabled {
        cfg.store
            .meta_set(META_OWNER_CLAIMED, "1")
            .map_err(ApiError::Internal)?;
    }
    let sid = cfg.store.create_session(&tid).map_err(ApiError::Internal)?;
    // 认证成功：清除该 IP 的失败计数，误触封锁不惩罚正常用户
    note_login_success(&cfg, &ip);
    let secure = if cfg.cookie_secure { "; Secure" } else { "" };
    Ok((
        StatusCode::OK,
        [(
            SET_COOKIE,
            format!(
                "lf_session={sid}; Path=/; HttpOnly; SameSite=Lax; Max-Age={SESSION_TTL_SECS}{secure}"
            ),
        )],
        Json(serde_json::json!({
            "token_id": if admin { serde_json::Value::Null } else { serde_json::json!(tid) },
            "label": label,
            "admin": admin,
        })),
    )
        .into_response())
}

/// 登录限流参数：连续 5 次失败封 60s（内存态，重启即清可接受）。
const LOGIN_FAIL_LIMIT: u32 = 5;
const LOGIN_LOCKOUT_SECS: u64 = 60;

/// 来源 IP：反代头优先（部署形态常经 Caddy），否则 socket 对端；
/// 集成测试 oneshot 无 ConnectInfo 时落 `unknown`（单桶，不影响用例）。
fn client_ip(headers: &HeaderMap, peer: Option<&std::net::SocketAddr>) -> String {
    if let Some(first) = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        return first.to_string();
    }
    if let Some(rip) = headers
        .get("x-real-ip")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        return rip.to_string();
    }
    peer.map(|p| p.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn login_locked(cfg: &GatewayConfig, ip: &str) -> bool {
    let map = cfg.login_throttle.lock().unwrap_or_else(|e| e.into_inner());
    map.get(ip).is_some_and(|(count, since)| {
        *count >= LOGIN_FAIL_LIMIT && since.elapsed().as_secs() < LOGIN_LOCKOUT_SECS
    })
}

fn note_login_failure(cfg: &GatewayConfig, ip: &str) {
    let mut map = cfg.login_throttle.lock().unwrap_or_else(|e| e.into_inner());
    let entry = map
        .entry(ip.to_string())
        .or_insert((0, std::time::Instant::now()));
    if entry.1.elapsed().as_secs() >= LOGIN_LOCKOUT_SECS {
        // 封锁窗口已过：重新起算，不把陈旧计数直接叠回封锁
        *entry = (1, std::time::Instant::now());
    } else {
        entry.0 += 1;
        if entry.0 >= LOGIN_FAIL_LIMIT {
            entry.1 = std::time::Instant::now();
        }
    }
}

fn note_login_success(cfg: &GatewayConfig, ip: &str) {
    cfg.login_throttle
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(ip);
}

async fn logout(
    State(cfg): State<Arc<GatewayConfig>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(sid) = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.split(';')
                .map(str::trim)
                .find_map(|c| c.strip_prefix("lf_session="))
        })
    {
        cfg.store.end_session(sid).map_err(ApiError::Internal)?;
    }
    Ok((
        StatusCode::OK,
        [(SET_COOKIE, "lf_session=; Path=/; Max-Age=0")],
    ))
}

async fn whoami(
    State(cfg): State<Arc<GatewayConfig>>,
    Extension(ctx): Extension<AuthContext>,
    Extension(scope): Extension<RequestScope>,
) -> impl IntoResponse {
    // locked = 产品界面是否需要验证：网关形态恒真；单机形态看登录门是否开启
    // （默认开启；LANGFIXER_LOGIN=0 时退回旧语义——owner 激活后才锁）
    let mut locked = cfg.enabled;
    if !locked {
        locked = cfg.login_gate
            || cfg
                .store
                .meta_get(crate::auth::META_OWNER_CLAIMED)
                .ok()
                .flatten()
                .is_some();
    }
    let mut body = serde_json::json!({
        "gateway": ctx.gateway,
        "admin": ctx.admin,
        "token_id": scope.token_id,
        "authenticated": ctx.authed,
        "locked": locked,
        "password_login": cfg.password.is_set(),
    });
    // 密钥来源与指纹**只给已验证的超管**：whoami 未登录也回 200（门禁页要探测），
    // 匿名请求一律不回，避免把「这台实例活的是哪把」的信息白送出去。
    // 指纹是 SHA-256 前 8 位，用于核对钥匙，不足以据此认证。
    if ctx.admin && ctx.authed {
        body["admin_token_source"] = serde_json::json!(cfg.admin_source);
        body["admin_token_fp"] = serde_json::json!(cfg.admin.fingerprint());
    }
    Json(body)
}

/// 改管理员密码：需已验证的管理员身份（会话或超管 Bearer）。
/// 默认密码写在文档与启动日志里，不改密等于没有口令——所以这个端点与登录面同批落地。
async fn change_password(
    State(cfg): State<Arc<GatewayConfig>>,
    Extension(ctx): Extension<AuthContext>,
    Json(body): Json<PasswordReq>,
) -> Result<impl IntoResponse, ApiError> {
    if !(ctx.authed && ctx.admin) {
        return Err(ApiError::Unauthorized("需要管理员登录态".into()));
    }
    if !cfg.password.matches(&body.old_password) {
        return Err(ApiError::Unauthorized("原密码错误".into()));
    }
    // 按字符计：字节数会让三个汉字（9 字节）冒充「至少 8 位」
    if body.new_password.chars().count() < 8 {
        return Err(ApiError::BadRequest("新密码至少 8 位".into()));
    }
    // 默认密码写在文档里，「改成它」等于没改——直接拒
    if body.new_password == crate::auth::DEFAULT_ADMIN_PASSWORD {
        return Err(ApiError::BadRequest(
            "不能沿用默认密码（公开文档里有，等于没有口令）".into(),
        ));
    }
    // rotate 会清掉全部超管会话（含本请求的）；给调用者当场重发新会话，
    // 旧 cookie 即刻作废而当前操作不用重新登录。
    cfg.rotate_password(&body.new_password)
        .map_err(ApiError::Internal)?;
    let sid = cfg
        .store
        .create_session(ADMIN_TOKEN_ID)
        .map_err(ApiError::Internal)?;
    let secure = if cfg.cookie_secure { "; Secure" } else { "" };
    Ok((
        StatusCode::OK,
        [(
            SET_COOKIE,
            format!(
                "lf_session={sid}; Path=/; HttpOnly; SameSite=Lax; Max-Age={SESSION_TTL_SECS}{secure}"
            ),
        )],
        Json(serde_json::json!({ "ok": true })),
    ))
}
