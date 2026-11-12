//! 免注册登录面：一枚令牌（评审或超管）换一枚短时效会话 cookie，
//! 无注册、无找回密码、无用户资料。产品界面在验证通过前不展示（前端门禁页）。

use std::sync::Arc;

use axum::extract::{Extension, State};
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
        .with_state(cfg)
}

#[derive(Deserialize)]
struct SessionReq {
    token: String,
}

async fn create_session(
    State(cfg): State<Arc<GatewayConfig>>,
    Json(body): Json<SessionReq>,
) -> Result<impl IntoResponse, ApiError> {
    let (tid, label, admin) = if cfg.admin.matches(&body.token) {
        (ADMIN_TOKEN_ID.to_string(), "管理员".to_string(), true)
    } else if let Some(info) = cfg
        .store
        .verify_token(&body.token)
        .map_err(ApiError::Internal)?
    {
        (info.id, info.label, false)
    } else {
        return Err(ApiError::Unauthorized("令牌无效或已吊销".into()));
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
    // locked = 产品界面是否需要验证：网关形态恒真；单机形态看 owner 是否已激活
    // （未激活 = 刚装完的本机用户，界面照常敞开，与设置写入零变化同口径）
    let mut locked = cfg.enabled;
    if !locked {
        locked = cfg
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
