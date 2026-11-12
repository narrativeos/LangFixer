//! 管理面（仅超管令牌 Bearer 可达，由 `middleware::auth::gate` 强制）：
//! 签发/吊销评审令牌、查看活跃会话、显式轮换超级管理员密钥。
//! 超管密钥来源见 `auth::AdminTokenSource`：env 给了 `LANGFIXER_ADMIN_TOKEN` 就以它为准
//! （不落库），否则用安装时生成并存在 `gw_meta.admin_token` 的那一枚。

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::auth::{GatewayConfig, RotateError};
use crate::routes::ApiError;

pub fn router(cfg: Arc<GatewayConfig>) -> Router {
    Router::new()
        .route("/admin/tokens", get(list_tokens).post(issue_token))
        .route("/admin/tokens/:id/revoke", post(revoke_token))
        .route("/admin/tokens/:id", delete(delete_token))
        .route("/admin/stats", get(stats))
        .route("/admin/admin-token/rotate", post(rotate_admin_token))
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
