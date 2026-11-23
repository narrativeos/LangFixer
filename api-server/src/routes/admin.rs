//! 管理面（仅超管令牌 Bearer 可达，由 `middleware::auth::gate` 强制）：
//! 签发/吊销评审令牌、用户名账号（密钥身份的口令别名，供 web 登录）、
//! 查看活跃会话、显式轮换超级管理员密钥。
//! 超管密钥来源见 `auth::AdminTokenSource`：env 给了 `LANGFIXER_ADMIN_TOKEN` 就以它为准
//! （不落库），否则用安装时生成并存在 `gw_meta.admin_token` 的那一枚。

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::auth::{
    GatewayConfig, RotateError, gen_temp_password, hash_password, normalize_username,
};
use crate::routes::ApiError;

pub fn router(cfg: Arc<GatewayConfig>) -> Router {
    Router::new()
        .route("/admin/tokens", get(list_tokens).post(issue_token))
        .route("/admin/tokens/:id/revoke", post(revoke_token))
        .route("/admin/tokens/:id", delete(delete_token))
        .route("/admin/users", get(list_users).post(create_user))
        .route("/admin/users/:username/reset", post(reset_user))
        .route("/admin/users/:username", delete(delete_user))
        .route("/admin/stats", get(stats))
        .route("/admin/admin-token/rotate", post(rotate_admin_token))
        .with_state(cfg)
}

#[derive(Deserialize)]
struct IssueReq {
    label: String,
    #[serde(default)]
    quota_chars: i64,
    /// 可选：同时为这枚密钥创建用户名账号（web 口令登录用）。
    /// 给了 username 未给 password 时服务端生成临时口令，首次登录后应改。
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    password: Option<String>,
}

#[derive(Serialize)]
struct IssuedResp {
    id: String,
    /// 明文仅此一次响应出现；库中只存 SHA-256 摘要
    token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    username: Option<String>,
    /// 仅服务端代生成时回明文（也只出现这一次）；管理员自备口令不回显
    #[serde(skip_serializing_if = "Option::is_none")]
    password: Option<String>,
}

async fn issue_token(
    State(cfg): State<Arc<GatewayConfig>>,
    Json(body): Json<IssueReq>,
) -> Result<(StatusCode, Json<IssuedResp>), ApiError> {
    if body.label.trim().is_empty() {
        return Err(ApiError::BadRequest("label 不能为空".into()));
    }
    // 先校验用户名再签发：避免「密钥已发、账号建不了」的半途状态
    let uname = match body.username.as_deref() {
        Some(raw) => Some(normalize_username(raw).ok_or_else(|| {
            ApiError::BadRequest("用户名需为 2–32 位小写字母/数字/._-（且不能是 admin）".into())
        })?),
        None => None,
    };
    let issued = cfg
        .store
        .issue(body.label.trim(), body.quota_chars)
        .map_err(ApiError::Internal)?;
    let mut resp = IssuedResp {
        id: issued.id.clone(),
        token: issued.token.clone(),
        username: None,
        password: None,
    };
    if let Some(uname) = uname {
        // 代生成的临时口令：明文只在这一次响应里回，库里落带盐摘要
        let temp = gen_temp_password();
        let (pw, generated) = match body.password.as_deref() {
            Some(p) => {
                if p.chars().count() < 8 {
                    let _ = cfg.store.remove(&issued.id);
                    return Err(ApiError::BadRequest("初始密码至少 8 位".into()));
                }
                (hash_password(p), None)
            }
            None => (hash_password(&temp), Some(temp)),
        };
        if let Err(e) = cfg.store.create_user(&issued.id, &uname, &pw, true) {
            // 账号冲突：回滚刚签发的密钥，不留幽灵身份
            let _ = cfg.store.remove(&issued.id);
            return Err(ApiError::Conflict(e));
        }
        resp.username = Some(uname);
        resp.password = generated;
    }
    Ok((StatusCode::CREATED, Json(resp)))
}

async fn list_tokens(
    State(cfg): State<Arc<GatewayConfig>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // 一条 SQL 带出用量（gw_usage 日账），界面据此显示「已用 / 配额」
    let tokens = cfg.store.list_with_usage().map_err(ApiError::Internal)?;
    Ok(Json(serde_json::json!({ "tokens": tokens })))
}

// ── 用户名账号（密钥身份的口令别名）：创建/重置只影响 web 登录，
// MCP、配额、历史仍按密钥归属；吊销密钥后口令登录一并失效。

#[derive(Serialize)]
struct UserRow {
    username: String,
    token_id: String,
    must_change: bool,
    revoked: bool,
    created_at: i64,
}

async fn list_users(State(cfg): State<Arc<GatewayConfig>>) -> Result<Json<Value>, ApiError> {
    let users = cfg
        .store
        .list_users()
        .map_err(ApiError::Internal)?
        .into_iter()
        .map(
            |(username, token_id, must_change, revoked, created_at)| UserRow {
                username,
                token_id,
                must_change,
                revoked,
                created_at,
            },
        )
        .collect::<Vec<_>>();
    Ok(Json(serde_json::json!({ "users": users })))
}

#[derive(Deserialize)]
struct CreateUserReq {
    token_id: String,
    username: String,
    /// 缺省时服务端生成临时口令（只回这一次）；自备口令不回显
    #[serde(default)]
    password: Option<String>,
}

#[derive(Serialize)]
struct UserCreatedResp {
    username: String,
    token_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    password: Option<String>,
}

async fn create_user(
    State(cfg): State<Arc<GatewayConfig>>,
    Json(body): Json<CreateUserReq>,
) -> Result<(StatusCode, Json<UserCreatedResp>), ApiError> {
    let uname = normalize_username(&body.username).ok_or_else(|| {
        ApiError::BadRequest("用户名需为 2–32 位小写字母/数字/._-（且不能是 admin）".into())
    })?;
    if !cfg
        .store
        .token_exists(body.token_id.trim())
        .map_err(ApiError::Internal)?
    {
        return Err(ApiError::NotFound(format!("密钥不存在: {}", body.token_id)));
    }
    let temp = gen_temp_password();
    let (pw, generated) = match body.password.as_deref() {
        Some(p) => {
            if p.chars().count() < 8 {
                return Err(ApiError::BadRequest("初始密码至少 8 位".into()));
            }
            (hash_password(p), None)
        }
        None => (hash_password(&temp), Some(temp)),
    };
    // 管理员代设的口令一律标 must_change：对方首次登录后应改成自己的
    cfg.store
        .create_user(body.token_id.trim(), &uname, &pw, true)
        .map_err(ApiError::Conflict)?;
    Ok((
        StatusCode::CREATED,
        Json(UserCreatedResp {
            username: uname,
            token_id: body.token_id.trim().to_string(),
            password: generated,
        }),
    ))
}

#[derive(Deserialize)]
struct ResetReq {
    #[serde(default)]
    password: Option<String>,
}

/// 重置口令：新临时口令只回这一次，该密钥在途会话即刻作废，
/// 首次登录后 must_change 提示改密。
async fn reset_user(
    State(cfg): State<Arc<GatewayConfig>>,
    Path(username): Path<String>,
    body: Option<Json<ResetReq>>,
) -> Result<Json<Value>, ApiError> {
    let uname = username.trim().to_ascii_lowercase();
    if cfg
        .store
        .get_user(&uname)
        .map_err(ApiError::Internal)?
        .is_none()
    {
        return Err(ApiError::NotFound(format!("账号不存在: {uname}")));
    }
    let temp = gen_temp_password();
    let (pw, generated) = match body.and_then(|Json(b)| b.password) {
        Some(p) => {
            if p.chars().count() < 8 {
                return Err(ApiError::BadRequest("新密码至少 8 位".into()));
            }
            (hash_password(&p), None)
        }
        None => (hash_password(&temp), Some(temp)),
    };
    // update 连带清该身份在途会话（旧口令换的 cookie 不能幸存）
    let hit = cfg
        .store
        .update_user_password(&uname, &pw, true)
        .map_err(ApiError::Internal)?;
    if !hit {
        return Err(ApiError::NotFound(format!("账号不存在: {uname}")));
    }
    Ok(Json(serde_json::json!({
        "ok": true,
        "username": uname,
        "password": generated,
        "must_change": true,
    })))
}

/// 只删口令别名：密钥本身保留（MCP 仍可用）。
async fn delete_user(
    State(cfg): State<Arc<GatewayConfig>>,
    Path(username): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let hit = cfg
        .store
        .delete_user(username.trim().to_ascii_lowercase().as_str())
        .map_err(ApiError::Internal)?;
    if !hit {
        return Err(ApiError::NotFound(format!("账号不存在: {username}")));
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn revoke_token(
    State(cfg): State<Arc<GatewayConfig>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let hit = cfg.store.revoke(&id).map_err(ApiError::Internal)?;
    if !hit {
        return Err(ApiError::NotFound(format!("令牌不存在或已吊销: {id}")));
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn delete_token(
    State(cfg): State<Arc<GatewayConfig>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let hit = cfg.store.remove(&id).map_err(ApiError::Internal)?;
    if !hit {
        return Err(ApiError::NotFound(format!("令牌不存在: {id}")));
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn stats(State(cfg): State<Arc<GatewayConfig>>) -> Result<Json<serde_json::Value>, ApiError> {
    let sessions = cfg.store.active_sessions().map_err(ApiError::Internal)?;
    let tokens = cfg.store.list().map_err(ApiError::Internal)?;
    let (today, retained) = cfg.store.usage_totals().map_err(ApiError::Internal)?;
    // 调用级合计：含被拒请求（401/403/429 也是有人敲过门），口径与 gw_call_log 同源
    let (calls_retained, calls_today) = cfg.store.call_totals().map_err(ApiError::Internal)?;
    Ok(Json(serde_json::json!({
        "tokens_total": tokens.len(),
        "tokens_active": tokens.iter().filter(|r| r.3.is_none()).count(),
        "active_sessions": sessions,
        // 用量口径与清理 timer 同源：账只保留 LANGFIXER_RETENTION_DAYS 天（默认 7）
        "chars_today": today,
        "chars_retained": retained,
        "calls_today": calls_today,
        "calls_retained": calls_retained,
        "retention_days": std::env::var("LANGFIXER_RETENTION_DAYS")
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(7),
    })))
}

#[derive(Deserialize)]
struct RotateReq {
    /// 确认字面量，必须为 `"ROTATE"`。缺失或不同一律 400：换钥匙不接受隐式请求
    /// （空 body、`{}`、`{"confirm":true}` 都不算确认）。
    #[serde(default)]
    confirm: Option<String>,
}

/// `POST /admin/admin-token/rotate` — **显式**轮换超级管理员密钥。
///
/// 换钥匙是一个动作，不是重启或改文件的副作用：旧密钥与其会话在本次调用返回时
/// 即刻失效，新明文只出现在这一次响应里（`gw_meta` 存新值明文以便重启复用，
/// 日志只记指纹）。密钥来自 `LANGFIXER_ADMIN_TOKEN` 时回 **409** 并说明改 env 的流程——
/// 不做「库里换了一把但 env 那把仍然赢」的分裂状态。
async fn rotate_admin_token(
    State(cfg): State<Arc<GatewayConfig>>,
    body: Option<Json<RotateReq>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let confirm = body.and_then(|Json(b)| b.confirm).unwrap_or_default();
    if confirm != "ROTATE" {
        return Err(ApiError::BadRequest(
            "轮换超级管理员密钥需要显式确认：body 必须为 {\"confirm\":\"ROTATE\"}".into(),
        ));
    }
    let next = cfg.rotate_admin_token().map_err(|e| match e {
        RotateError::EnvOwned => ApiError::Conflict(e.user_message()),
        RotateError::Store(_) => ApiError::Internal(e.user_message()),
    })?;
    Ok(Json(serde_json::json!({
        // 明文只在这一次响应里出现
        "token": next,
        "fingerprint": cfg.admin.fingerprint(),
        "source": cfg.admin_source,
        "note": "旧超级管理员密钥及其会话已即刻失效；评审密钥与其会话不受影响。新值不再回显，请立即妥善保存。",
    })))
}
