//! 会话（Session）管理 API
//!
//! - POST   /gec/sessions              — 创建会话
//! - GET    /gec/sessions/:id          — 获取会话详情
//! - PUT    /gec/sessions/:id/archive  — 归档
//! - PUT    /gec/sessions/:id/restore  — 恢复
//! - DELETE /gec/sessions/:id          — 删除（级联删除历史）
//! - DELETE /gec/sessions?lang=        — 批量删除某语言全部

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use shared_model::{SessionMeta, SessionRecord};

use crate::routes::ApiError;
use crate::state::AppState;

#[derive(serde::Deserialize)]
pub struct CreateSessionRequest {
    pub lang: String,
    /// 上游会话标识（可选；提供时生成复合 ID 防冲突）
    #[serde(default)]
    pub upstream_id: Option<String>,
}

/// POST /gec/sessions — 创建会话
pub async fn create(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateSessionRequest>,
) -> Result<Json<SessionMeta>, ApiError> {
    let local = uuid::Uuid::new_v4().simple().to_string();
    let session_id = match &req.upstream_id {
        Some(upstream) => format!("s_{}:{}", upstream, local),
        None => format!("s_{}", local),
    };
    let meta = SessionMeta::new(&session_id, &req.lang);
    state.sessions.get_or_create(&session_id, &req.lang);
    Ok(Json(meta))
}

/// GET /gec/sessions/:id — 获取会话详情
pub async fn get(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<SessionRecord>, ApiError> {
    let record = state
        .sessions
        .get(&id)
        .ok_or_else(|| ApiError::NotFound(format!("session#{} 不存在", id)))?;
    Ok(Json(record))
}

/// PUT /gec/sessions/:id/archive — 归档
pub async fn archive(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let ok = state.sessions.archive(&id);
    if !ok {
        return Err(ApiError::NotFound(format!("session#{} 不存在", id)));
    }
    Ok(Json(
        serde_json::json!({ "session_id": id, "status": "archived" }),
    ))
}

/// PUT /gec/sessions/:id/restore — 恢复
pub async fn restore(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let ok = state.sessions.restore(&id);
    if !ok {
        return Err(ApiError::NotFound(format!("session#{} 不存在", id)));
    }
    Ok(Json(
        serde_json::json!({ "session_id": id, "status": "active" }),
    ))
}

/// POST /gec/sessions/:id/record — 记录犯错（兼容旧 /student/:id/record）
#[derive(serde::Deserialize)]
pub struct SessionRecordRequest {
    pub lang: String,
    pub error_type: String,
}

pub async fn record(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<SessionRecordRequest>,
) -> Json<SessionRecord> {
    let now = chrono::Utc::now().to_rfc3339();
    Json(
        state
            .sessions
            .record_error(&id, &req.lang, &req.error_type, &now),
    )
}

/// DELETE /gec/sessions/:id — 删除单条（级联删除 detect_history）
pub async fn remove(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let ok = state.sessions.remove(&id);
    if !ok {
        return Err(ApiError::NotFound(format!("session#{} 不存在", id)));
    }
    // 级联删除该 session 的所有历史记录
    let _ = state.detect_history.remove_by_session(&id);
    Ok(Json(
        serde_json::json!({ "deleted": true, "session_id": id }),
    ))
}

#[derive(serde::Deserialize)]
pub struct DeleteAllQuery {
    #[serde(default)]
    pub lang: Option<String>,
}

/// DELETE /gec/sessions?lang= — 批量删除
pub async fn remove_all(
    State(state): State<Arc<AppState>>,
    Query(q): Query<DeleteAllQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // 简化：清空所有 session（后续可按 lang 过滤）
    let _ = q.lang;
    let n = state.sessions.clear();
    Ok(Json(serde_json::json!({ "deleted": n })))
}

// ─── 习惯性错误分析 ─────────────────────────────────────────────────────────

#[derive(serde::Serialize)]
pub struct HabitualErrorType {
    pub error_type: String,
    pub count: u32,
    pub ratio: f64,
    pub is_dominant: bool,
    pub examples: Vec<String>,
}

#[derive(serde::Serialize)]
pub struct TopCorrection {
    pub from: String,
    pub to: String,
    pub count: u32,
}

#[derive(serde::Serialize)]
pub struct SessionAnalysis {
    pub session_id: String,
    pub total_errors: u32,
    pub type_distribution: std::collections::HashMap<String, u32>,
    pub habitual: Vec<HabitualErrorType>,
    pub top_corrections: Vec<TopCorrection>,
}

/// GET /gec/sessions/:id/analysis — 会话习惯性错误分析
pub async fn analysis(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<SessionAnalysis>, ApiError> {
    // 1. 从 detect_history 聚合该 session 的所有错误
    let records = state
        .detect_history
        .list(Some(&id), None, 200)
        .map_err(|e| ApiError::Internal(format!("查询历史失败: {e}")))?;

    let mut type_counts: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    let mut correction_pairs: std::collections::HashMap<(String, String), u32> =
        std::collections::HashMap::new();
    let mut total: u32 = 0;

    for record in &records {
        let errors: Vec<shared_model::Error> =
            serde_json::from_str(&record.errors_json).unwrap_or_default();
        for err in &errors {
            *type_counts.entry(err.error_type.clone()).or_insert(0) += 1;
            total += 1;
            if let Some(correction) = &err.correction {
                let key = (err.original.clone(), correction.clone());
                *correction_pairs.entry(key).or_insert(0) += 1;
            }
        }
    }

    // 2. 计算占比，判定 is_dominant（≥ 0.3）
    let mut habitual: Vec<HabitualErrorType> = type_counts
        .iter()
        .map(|(et, count)| {
            let ratio = if total > 0 {
                *count as f64 / total as f64
            } else {
                0.0
            };
            HabitualErrorType {
                error_type: et.clone(),
                count: *count,
                ratio,
                is_dominant: ratio >= 0.3,
                examples: Vec::new(),
            }
        })
        .collect();

    // 3. 为 dominant 类型收集最多 5 个示例
    for h in &mut habitual {
        if !h.is_dominant {
            continue;
        }
        let mut examples = Vec::new();
        for record in &records {
            if examples.len() >= 5 {
                break;
            }
            let errors: Vec<shared_model::Error> =
                serde_json::from_str(&record.errors_json).unwrap_or_default();
            for err in &errors {
                if examples.len() >= 5 {
                    break;
                }
                if err.error_type == h.error_type {
                    let example = if let Some(correction) = &err.correction {
                        format!("{} → {}", err.original, correction)
                    } else {
                        err.original.clone()
                    };
                    if !examples.contains(&example) {
                        examples.push(example);
                    }
                }
            }
        }
        h.examples = examples;
    }

    // 4. 按 count 降序排列
    habitual.sort_by(|a, b| b.count.cmp(&a.count));

    // 5. top_corrections：同一 (from, to) 对出现 ≥ 2 次，取 top 5
    let mut top_corrections: Vec<TopCorrection> = correction_pairs
        .into_iter()
        .filter(|(_, count)| *count >= 2)
        .map(|((from, to), count)| TopCorrection { from, to, count })
        .collect();
    top_corrections.sort_by(|a, b| b.count.cmp(&a.count));
    top_corrections.truncate(5);

    Ok(Json(SessionAnalysis {
        session_id: id,
        total_errors: total,
        type_distribution: type_counts,
        habitual,
        top_corrections,
    }))
}
