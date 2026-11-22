//! ③ 双判判定日志查看（`GET /gec/dual-judge`）
//!
//! 双判开关开启后每次 ③ 检测产生一行（主臂 × 影子臂判定 + 对照 verdict）；
//! 本端点提供 verdict 聚合计数与最近明细，供运营台 / 测评中心观察臂间差异。

use std::sync::Arc;

use axum::extract::{Json, Query, State};
use lexicon_store::dual_judge_log::DualJudgeLog;
use serde::Serialize;

use crate::routes::ApiError;
use crate::state::AppState;

#[derive(Serialize)]
pub struct DualJudgeResponse {
    /// verdict → 计数
    pub counts: Vec<(String, i64)>,
    /// 最近明细（新在前）
    pub recent: Vec<DualJudgeLog>,
}

/// GET /gec/dual-judge?limit= — 双判判定日志
pub async fn recent(
    State(state): State<Arc<AppState>>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<DualJudgeResponse>, ApiError> {
    let limit = q
        .get("limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(50);
    let counts = state
        .dual_judge
        .verdict_counts()
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let recent = state
        .dual_judge
        .list(limit)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(DualJudgeResponse { counts, recent }))
}
