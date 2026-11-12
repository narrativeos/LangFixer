//! 免注册登录面：一个评审令牌换一枚短时效会话 cookie，无注册、无找回密码、无用户资料。

use std::sync::Arc;

use axum::extract::State;
use axum::http::header::{HeaderMap, SET_COOKIE};
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;

use crate::auth::{GatewayConfig, SESSION_TTL_SECS};
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
    if !cfg.enabled {
        return Ok(StatusCode::NO_CONTENT.into_response()); // 单机形态无需会话
    }
    let info = cfg
        .store
        .verify_token(&body.token)
        .map_err(ApiError::Internal)?
        .ok_or_else(|| ApiError::Unauthorized("令牌无效或已吊销".into()))?;
    let sid = cfg
        .store
        .create_session(&info.id)
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
        Json(serde_json::json!({ "token_id": info.id, "label": info.label })),
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
    req: axum::extract::Request,
) -> impl IntoResponse {
    let scope = req.extensions().get::<crate::auth::RequestScope>().cloned();
    let is_admin = scope.as_ref().is_some_and(|s| s.is_admin);
    let token_id = scope.and_then(|s| s.token_id);
    Json(serde_json::json!({
        "gateway": cfg.enabled,
        "admin": is_admin,
        "token_id": token_id,
    }))
}
