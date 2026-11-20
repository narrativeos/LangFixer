//! §7 可观测性只读端点：告警（integrity / quality 两路）与按天聚合看板数据
//!
//! 数据源 = `request_summary` 账本（每次 detect/analyze 一行，见
//! [`crate::request_summary`]）。纯读、无副作用；不鉴权扩展，跟随其余 GET 路由。

use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};

use crate::routes::ApiError;
use crate::state::AppState;

/// GET /gec/observability/alerts — 24h 窗口两路告警
pub async fn alerts(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state
        .request_summary
        .alerts()
        .map(Json)
        .map_err(|e| ApiError::Internal(format!("告警查询失败: {e}")))
}

#[derive(serde::Deserialize)]
pub struct SummaryQuery {
    /// 聚合窗口（天，默认 14，钳制 1..=90）
    #[serde(default)]
    pub days: Option<i64>,
}

/// GET /gec/observability/summary?days=14 — day × 规则指纹 聚合时间序列
pub async fn summary(
    State(state): State<Arc<AppState>>,
    Query(q): Query<SummaryQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let days = q.days.unwrap_or(14).clamp(1, 90);
    state
        .request_summary
        .daily_summary(days)
        .map(Json)
        .map_err(|e| ApiError::Internal(format!("摘要聚合失败: {e}")))
}
