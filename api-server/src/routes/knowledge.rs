//! 知识库 API（知识源注册表 + 知识点集合；与决策表分离）
//!
//! - `GET /gec/knowledge/sources` — 知识源注册表（含关联知识点数统计）
//! - `GET /gec/knowledge/points?lang=&source_id=` — 知识点列表（可按知识源过滤）
//! - `PUT /gec/knowledge/sources` — 热更新知识源注册表（校验失败保留旧数据）
//! - `PUT /gec/knowledge/points/:lang` — 热更新某语言知识点集合
//!
//! 知识内容涉及知识产权，不随产品内置发布：目录缺失（`GEC_KNOWLEDGE_DIR`）
//! 时列表为空、归因结果优雅降级为纯表级结果。
//! PUT 与决策表热更新共用 `GEC_TABLES_UPDATE_TOKEN` 鉴权（`X-Tables-Update-Token`）。

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use serde::Deserialize;
use shared_model::{KnowledgePoint, KnowledgeSource};

use crate::routes::ApiError;
use crate::state::AppState;

/// GET /gec/knowledge/sources — 知识源注册表
pub async fn list_sources(State(state): State<Arc<AppState>>) -> Json<Vec<KnowledgeSource>> {
    Json(state.knowledge.sources())
}

/// GET /gec/knowledge/points?lang=&source_id= — 知识点列表（可按知识源过滤）
pub async fn list_points(
    State(state): State<Arc<AppState>>,
    Query(query): Query<PointsQuery>,
) -> Json<Vec<KnowledgePoint>> {
    Json(
        state
            .knowledge
            .points(&query.lang, query.source_id.as_deref()),
    )
}

#[derive(Deserialize)]
pub struct PointsQuery {
    pub lang: String,
    #[serde(default)]
    pub source_id: Option<String>,
}

/// PUT /gec/knowledge/sources — 热更新知识源注册表（body 为数组或 JSON 字符串）
pub async fn update_sources(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<Vec<KnowledgeSource>>, ApiError> {
    check_token(&state, &headers)?;
    let json = match body {
        serde_json::Value::String(s) => s,
        other => other.to_string(),
    };
    let sources = state
        .knowledge
        .update_sources(&json)
        .map_err(ApiError::BadRequest)?;
    persist_knowledge(&state);
    Ok(Json(sources))
}

/// PUT /gec/knowledge/points/:lang — 热更新某语言知识点集合（body 为数组或 JSON 字符串）
pub async fn update_points(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(lang): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<PointsUpdateResponse>, ApiError> {
    check_token(&state, &headers)?;
    let json = match body {
        serde_json::Value::String(s) => s,
        other => other.to_string(),
    };
    let count = state
        .knowledge
        .update_points(&lang, &json)
        .map_err(ApiError::BadRequest)?;
    persist_knowledge(&state);
    Ok(Json(PointsUpdateResponse { lang, count }))
}

/// 热更新后写回知识库目录（`{GEC_KNOWLEDGE_DIR}/sources.json` + `points/{lang}.json`）。
/// 写回失败不回滚内存热更新（当次进程内已生效），但明确告警：重启后将丢失。
pub(crate) fn persist_knowledge(state: &AppState) {
    if let Err(e) = state.knowledge.save_to_dir(&state.knowledge_dir) {
        tracing::warn!(error = %e, "知识库写回失败（内存已更新，重启后将丢失）");
    }
}

#[derive(serde::Serialize)]
pub struct PointsUpdateResponse {
    pub lang: String,
    pub count: usize,
}

pub(crate) fn check_token(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    if let Some(expected) = &state.tables_update_token {
        let provided = headers
            .get("x-tables-update-token")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if provided != expected.as_str() {
            return Err(ApiError::Unauthorized(
                "更新知识库需提供有效的 X-Tables-Update-Token".into(),
            ));
        }
    }
    Ok(())
}
