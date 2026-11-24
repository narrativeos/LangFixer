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
    ADMIN_TOKEN_ID, ADMIN_USERNAME, AuthContext, DEFAULT_ADMIN_PASSWORD, GatewayConfig,
    InviteOutcome, META_OWNER_CLAIMED, RequestScope, SESSION_TTL_SECS, hash_password,
    normalize_username, verify_password_hash,
};
use crate::routes::ApiError;

pub fn router(cfg: Arc<GatewayConfig>) -> Router {
    Router::new()
        .route("/auth/session", post(create_session))
        .route("/auth/logout", post(logout))
        .route("/auth/whoami", axum::routing::get(whoami))
        .route("/auth/password", post(change_password))
        .route("/auth/credentials", post(set_credentials))
        .route("/auth/admin/init", post(init_admin))
        .route("/auth/invite/activate", post(invite_activate))
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

#[derive(Deserialize)]
struct CredentialsReq {
    /// 首次创建时必填；已有账号时忽略（用户名不可自改，删除由管理面做）
    #[serde(default)]
    username: Option<String>,
    /// 已有账号改密必填；首次创建时忽略
    #[serde(default)]
    old_password: Option<String>,
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
    let (tid, label, admin, must_change) = if let Some(token) = body.token.as_deref() {
        if cfg.admin.matches(token) {
            (
                ADMIN_TOKEN_ID.to_string(),
                "管理员".to_string(),
                true,
                false,
            )
        } else if let Some(info) = cfg.store.verify_token(token).map_err(ApiError::Internal)? {
            (info.id, info.label, false, false)
        } else {
            note_login_failure(&cfg, &ip);
            return Err(ApiError::Unauthorized("令牌无效或已吊销".into()));
        }
    } else if body.password.is_some() {
        // 用户名密码登录：先比内建 admin（超管），再查密钥用户名账号（网关形态）。
        // 两条路都只核验、不注册；失败同样计入限流。
        let uname = body
            .username
            .as_deref()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        let pw = body.password.as_deref().unwrap_or("");
        let user = cfg.store.get_user(&uname).map_err(ApiError::Internal)?;
        if cfg.verify_password(body.username.as_deref().unwrap_or(""), pw) {
            (
                ADMIN_TOKEN_ID.to_string(),
                "管理员".to_string(),
                true,
                false,
            )
        } else if let Some((tid, stored, mc, dead)) = &user {
            if !dead && verify_password_hash(stored, pw) {
                (tid.clone(), uname.clone(), false, *mc)
            } else {
                note_login_failure(&cfg, &ip);
                return Err(ApiError::Unauthorized("用户名或密码错误".into()));
            }
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
    let mut body = serde_json::json!({
        "token_id": if admin { serde_json::Value::Null } else { serde_json::json!(tid) },
        "label": label,
        "admin": admin,
    });
    if must_change {
        body["must_change"] = serde_json::json!(true);
    }
    Ok((
        StatusCode::OK,
        [(
            SET_COOKIE,
            format!(
                "lf_session={sid}; Path=/; HttpOnly; SameSite=Lax; Max-Age={SESSION_TTL_SECS}{secure}"
            ),
        )],
        Json(body),
    )
        .into_response())
}

#[derive(Deserialize)]
struct InitReq {
    /// 安装时打印的激活密钥（超管密钥）；只用于验证安装者身份，不落库
    token: String,
    password: String,
    #[serde(default)]
    username: Option<String>,
}

#[derive(Deserialize)]
struct ActivateReq {
    code: String,
    username: String,
    password: String,
}

/// 带机器可读错误码的 409：前端按 `error` 字段分支（already_initialized /
/// invite_used / username_taken …），`message` 仅供展示。
fn conflict(code: &str, msg: &str) -> axum::response::Response {
    (
        StatusCode::CONFLICT,
        Json(serde_json::json!({ "error": code, "message": msg })),
    )
        .into_response()
}

/// 首屏初始化：把内置 admin 从「无口令」提升到自建口令。
/// 安装者凭安装时打印的超管密钥证明身份；已初始化则 409，
/// 旧库（历史上种过默认口令的）在 `admin_initialized()` 里也算已初始化。
async fn init_admin(
    State(cfg): State<Arc<GatewayConfig>>,
    peer: Option<ConnectInfo<std::net::SocketAddr>>,
    headers: HeaderMap,
    Json(body): Json<InitReq>,
) -> Result<axum::response::Response, ApiError> {
    let ip = client_ip(&headers, peer.as_ref().map(|c| &c.0));
    if login_locked(&cfg, &ip) {
        return Err(ApiError::TooManyRequests(
            "初始化尝试过于频繁，请 60 秒后重试".into(),
        ));
    }
    if cfg.admin_initialized() {
        return Ok(conflict(
            "already_initialized",
            "管理员账号已初始化，请直接登录；忘记口令用服务器上的 env 密钥重置",
        ));
    }
    if let Some(u) = body.username.as_deref().map(str::trim)
        && !u.is_empty()
        && u != ADMIN_USERNAME
    {
        return Err(ApiError::BadRequest(
            "首个管理员账号的用户名须为 admin".into(),
        ));
    }
    if body.password.chars().count() < 8 {
        return Err(ApiError::BadRequest("密码至少 8 位".into()));
    }
    if body.password == DEFAULT_ADMIN_PASSWORD {
        return Err(ApiError::BadRequest(
            "请设置自己的密码，不要用默认口令".into(),
        ));
    }
    if !cfg.admin.matches(&body.token) {
        note_login_failure(&cfg, &ip);
        return Err(ApiError::Unauthorized("激活密钥无效".into()));
    }
    let sid = cfg
        .init_admin_password(&body.password)
        .map_err(ApiError::Internal)?;
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
        Json(serde_json::json!({ "ok": true, "username": ADMIN_USERNAME })),
    )
        .into_response())
}

/// 邀请码激活（匿名可达）：一次性邀请码 + 自定用户名口令 → 新账号会话。
/// 失败路径把已占用的码放回（除码本身无效/过期/已用），不让用户
/// 因用户名冲突丢掉重试机会。
async fn invite_activate(
    State(cfg): State<Arc<GatewayConfig>>,
    peer: Option<ConnectInfo<std::net::SocketAddr>>,
    headers: HeaderMap,
    Json(body): Json<ActivateReq>,
) -> Result<axum::response::Response, ApiError> {
    let ip = client_ip(&headers, peer.as_ref().map(|c| &c.0));
    if login_locked(&cfg, &ip) {
        return Err(ApiError::TooManyRequests(
            "激活尝试过于频繁，请 60 秒后重试".into(),
        ));
    }
    let uname = normalize_username(&body.username).ok_or_else(|| {
        ApiError::BadRequest("用户名需为 2–32 位小写字母/数字/._-（且不能是 admin）".into())
    })?;
    if body.password.chars().count() < 8 {
        return Err(ApiError::BadRequest("密码至少 8 位".into()));
    }
    if body.password == DEFAULT_ADMIN_PASSWORD {
        return Err(ApiError::BadRequest(
            "请设置自己的密码，不要用默认口令".into(),
        ));
    }
    if cfg
        .store
        .get_user(&uname)
        .map_err(ApiError::Internal)?
        .is_some()
    {
        return Ok(conflict("username_taken", "该用户名已被占用，换一个再试"));
    }
    let invite = match cfg
        .store
        .consume_invite(&body.code)
        .map_err(ApiError::Internal)?
    {
        InviteOutcome::Consumed(inv) => inv,
        InviteOutcome::Invalid => {
            // 乱猜码同样计入限流，挡住脚本扫码
            note_login_failure(&cfg, &ip);
            return Ok(conflict("invite_invalid", "邀请码无效或已被撤销"));
        }
        InviteOutcome::Expired => {
            return Ok(conflict(
                "invite_expired",
                "邀请码已过期，请向管理员索取新的",
            ));
        }
        InviteOutcome::Used => {
            return Ok(conflict(
                "invite_used",
                "邀请码已被使用，一枚码只能注册一个账号",
            ));
        }
    };
    let issued = cfg
        .store
        .issue(&format!("邀请注册 · {uname}"), invite.quota_chars)
        .map_err(ApiError::Internal)?;
    if let Err(e) = cfg
        .store
        .create_user(&issued.id, &uname, &hash_password(&body.password), false)
    {
        // 并发用户名冲突（预检后被抢先）：销毁刚发的密钥、把码放回，可换名重试
        let _ = cfg.store.remove(&issued.id);
        let _ = cfg.store.undo_invite_consume(&invite.id);
        return Ok(conflict(
            "username_taken",
            &format!("{e}；邀请码已放回，可换用户名重试"),
        ));
    }
    let _ = cfg.store.note_invite_used_by(&invite.id, &uname);
    let sid = cfg
        .store
        .create_session(&issued.id)
        .map_err(ApiError::Internal)?;
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
        Json(serde_json::json!({ "ok": true, "username": uname, "token_id": issued.id })),
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
        // 门禁页据此分支「初始化 admin 账号」还是「登录」；匿名请求也要回，
        // 首屏没有登录态也要能探测到这一步是否需要做。
        "admin_initialized": cfg.admin_initialized(),
    });
    // 评审身份带出用户名账号状态（是否已设口令 / 是否临时口令待改），
    // 门禁页与「我的账号」面板据此提示；超管不查（用 /auth/password 体系）。
    if let Some(tid) = scope.token_id.as_deref()
        && let Ok(Some((username, must_change))) = cfg.store.user_for_token(tid)
    {
        body["username"] = serde_json::json!(username);
        body["must_change"] = serde_json::json!(must_change);
    }
    // 密钥来源与指纹**只给已验证的超管**：whoami 未登录也回 200（门禁页要探测），
    // 匿名请求一律不回，避免把「这台实例活的是哪把」的信息白送出去。
    // 指纹是 SHA-256 前 8 位，用于核对钥匙，不足以据此认证。
    if ctx.admin && ctx.authed {
        body["admin_token_source"] = serde_json::json!(cfg.admin_source);
        body["admin_token_fp"] = serde_json::json!(cfg.admin.fingerprint());
    }
    Json(body)
}

/// 试用者自服务：给当前密钥身份设置（首次）或更换用户名口令，无注册流程。
/// 用户名是同一枚密钥身份的**口令别名**：配额、历史、MCP 仍按密钥归属，
/// 吊销密钥后用户名登录一并失效。超管不走这里（用 `/auth/password`）。
async fn set_credentials(
    State(cfg): State<Arc<GatewayConfig>>,
    Extension(ctx): Extension<AuthContext>,
    Extension(scope): Extension<RequestScope>,
    Json(body): Json<CredentialsReq>,
) -> Result<impl IntoResponse, ApiError> {
    if !ctx.authed {
        return Err(ApiError::Unauthorized("需要登录态（密钥或会话）".into()));
    }
    if ctx.admin {
        return Err(ApiError::BadRequest(
            "超级管理员请用 /auth/password 改管理员密码；用户账号在管理面（密钥分发）创建".into(),
        ));
    }
    let tid = scope
        .token_id
        .clone()
        .ok_or_else(|| ApiError::BadRequest("当前身份没有可设口令的密钥".into()))?;
    // 按字符计：与超管改密同口径，三个汉字不得冒充「至少 8 位」
    if body.new_password.chars().count() < 8 {
        return Err(ApiError::BadRequest("新密码至少 8 位".into()));
    }
    if body.new_password == DEFAULT_ADMIN_PASSWORD {
        return Err(ApiError::BadRequest(
            "不能沿用默认密码（公开文档里有，等于没有口令）".into(),
        ));
    }
    match cfg.store.user_for_token(&tid).map_err(ApiError::Internal)? {
        // 首次创建：用户名必填且全局唯一
        None => {
            let uname = body
                .username
                .as_deref()
                .and_then(normalize_username)
                .ok_or_else(|| {
                    ApiError::BadRequest(
                        "用户名需为 2–32 位小写字母/数字/._-（且不能是 admin）".into(),
                    )
                })?;
            cfg.store
                .create_user(&tid, &uname, &hash_password(&body.new_password), false)
                .map_err(ApiError::Conflict)?;
            Ok((
                StatusCode::OK,
                Json(serde_json::json!({ "ok": true, "username": uname, "must_change": false })),
            )
                .into_response())
        }
        // 改密：验原口令；update 会清掉该身份全部在途会话（旧口令外泄的
        // 那批 cookie 不能幸存），当场重发新会话让当前操作不用重新登录。
        Some((uname, _)) => {
            let stored = cfg
                .store
                .get_user(&uname)
                .map_err(ApiError::Internal)?
                .ok_or_else(|| ApiError::Internal("账号行丢失".into()))?;
            if !verify_password_hash(&stored.1, body.old_password.as_deref().unwrap_or("")) {
                return Err(ApiError::Unauthorized("原密码错误".into()));
            }
            let hit = cfg
                .store
                .update_user_password(&uname, &hash_password(&body.new_password), false)
                .map_err(ApiError::Internal)?;
            if !hit {
                return Err(ApiError::NotFound("账号不存在".into()));
            }
            let sid = cfg.store.create_session(&tid).map_err(ApiError::Internal)?;
            let secure = if cfg.cookie_secure { "; Secure" } else { "" };
            Ok((
                StatusCode::OK,
                [
                    (
                        SET_COOKIE,
                        format!(
                            "lf_session={sid}; Path=/; HttpOnly; SameSite=Lax; Max-Age={SESSION_TTL_SECS}{secure}"
                        ),
                    ),
                ],
                Json(serde_json::json!({ "ok": true, "username": uname, "must_change": false })),
            )
                .into_response())
        }
    }
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
