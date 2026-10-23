use std::sync::Arc;

use axum::extract::{Path, State};
use axum::Json;
use shared_model::{StudentProfile, StudentRecord};

use crate::state::AppState;

/// GET /student/:id/profile — 获取用户画像
pub async fn profile(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Json<StudentProfile> {
    Json(state.students.get_or_create(&id).profile)
}

#[derive(serde::Deserialize)]
pub struct RecordRequest {
    pub lang: String,
    pub error_type: String,
    pub level: Option<String>,
    pub native_language: Option<String>,
}

/// POST /student/:id/record — 记录用户犯错历史
pub async fn record(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<RecordRequest>,
) -> Json<StudentRecord> {
    let now = chrono::Utc::now().to_rfc3339();
    Json(state.students.record_error(
        &id,
        req.level,
        req.native_language,
        &req.lang,
        &req.error_type,
        &now,
    ))
}
