//! 管理面（仅超管令牌 Bearer 可达，由 `middleware::auth::gate` 强制）：
//! 签发/吊销评审令牌与查看活跃会话。超管凭证只来自 `.env`/进程环境，不落库。

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::auth::GatewayConfig;
use crate::routes::ApiError;

pub fn router(cfg: Arc<GatewayConfig>) -> Router {
    Router::new()
        .route("/admin/tokens", get(list_tokens).post(issue_token))
        .route("/admin/tokens/:id/revoke", post(revoke_token))
        .route("/admin/tokens/:id", delete(delete_token))
        .route("/admin/stats", get(stats))
        .with_state(cfg)
}

#[derive(Deserialize)]
struct IssueReq {
    label: String,
    #[serde(default)]
    quota_chars: i64,
}

#[derive(Serialize)]
struct IssuedResp {
    id: String,
    /// 明文仅此一次响应出现；库中只存 SHA-256 摘要
    token: String,
}

async fn issue_token(
    State(cfg): State<Arc<GatewayConfig>>,
    Json(body): Json<IssueReq>,
) -> Result<(StatusCode, Json<IssuedResp>), ApiError> {
    if body.label.trim().is_empty() {
        return Err(ApiError::BadRequest("label 不能为空".into()));
    }
    let issued = cfg
        .store
        .issue(body.label.trim(), body.quota_chars)
        .map_err(ApiError::Internal)?;
    Ok((
        StatusCode::CREATED,
        Json(IssuedResp {
            id: issued.id,
            token: issued.token,
        }),
    ))
}

async fn list_tokens(
    State(cfg): State<Arc<GatewayConfig>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // 一条 SQL 带出用量（gw_usage 日账），界面据此显示「已用 / 配额」
    let tokens = cfg.store.list_with_usage().map_err(ApiError::Internal)?;
    Ok(Json(serde_json::json!({ "tokens": tokens })))
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
    Ok(Json(serde_json::json!({
        "tokens_total": tokens.len(),
        "tokens_active": tokens.iter().filter(|r| r.3.is_none()).count(),
        "active_sessions": sessions,
        // 用量口径与清理 timer 同源：账只保留 LANGFIXER_RETENTION_DAYS 天（默认 7）
        "chars_today": today,
        "chars_retained": retained,
        "retention_days": std::env::var("LANGFIXER_RETENTION_DAYS")
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(7),
    })))
}
