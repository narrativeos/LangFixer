//! 会话（Session）管理 API
//!
//! - POST   /gec/sessions              — 创建会话
//! - GET    /gec/sessions/:id          — 获取会话详情
//! - PUT    /gec/sessions/:id/profile  — 更新学习者画像（母语 / 自评 CEFR）
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

/// GET /gec/sessions/:id/mistakes — 错题本列表（Phase 2 学习扩展；
/// 用户采纳确认的错误，同类型 + 同原文 + 同修正聚合，按最近出现倒序）
pub async fn mistakes(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Json<Vec<shared_model::Mistake>> {
    Json(state.sessions.list_mistakes(&id))
}

/// GET /gec/sessions/:id/review — 到期复习队列（Phase 2 间隔复习；
/// 未复习过或已到下次复习时间的错题，已掌握不进入）
pub async fn review_queue(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Json<Vec<shared_model::Mistake>> {
    Json(state.sessions.list_review_queue(&id))
}

#[derive(serde::Deserialize)]
pub struct ReviewRequest {
    /// FSRS 四键评分：again / hard / good / easy（优先于 passed）
    #[serde(default)]
    pub rating: Option<String>,
    /// 旧二键契约（记住 / 没记住）：映射为 good / again，保持兼容
    #[serde(default)]
    pub passed: Option<bool>,
}

impl ReviewRequest {
    /// 解析评分：rating 优先；无 rating 时用 passed 映射；两者皆缺 → 400
    fn resolve(&self) -> Result<shared_model::session_store::ReviewRating, ApiError> {
        if let Some(r) = &self.rating {
            return shared_model::session_store::ReviewRating::parse(r)
                .ok_or_else(|| ApiError::BadRequest(format!("无效 rating: {r}")));
        }
        match self.passed {
            Some(p) => Ok(shared_model::session_store::ReviewRating::from_passed(p)),
            None => Err(ApiError::BadRequest("需要 rating 或 passed".into())),
        }
    }
}

/// POST /gec/sessions/:id/mistakes/:mid/review — 记录一次复习结果
/// （FSRS 四键间隔复习调度：间隔按记忆状态与评分个性化递增，到期 ≥30 天已掌握；again → 次日再练）
pub async fn review_mistake(
    State(state): State<Arc<AppState>>,
    Path((id, mid)): Path<(String, i64)>,
    Json(req): Json<ReviewRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let rating = req.resolve()?;
    let ok = state.sessions.record_review(&id, mid, rating);
    if !ok {
        return Err(ApiError::NotFound(format!(
            "mistake#{mid}（session#{id}）复习记录写入失败"
        )));
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// POST /gec/sessions/:id/mistakes/:mid/review/undo — 撤销最近一次复习
/// （还原复习前记忆状态；每卡仅一次，时间窗由 UI 侧把关）
pub async fn undo_review_mistake(
    State(state): State<Arc<AppState>>,
    Path((id, mid)): Path<(String, i64)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if !state.sessions.undo_review(&id, mid) {
        return Err(ApiError::NotFound(format!(
            "mistake#{mid}（session#{id}）无可撤销的复习"
        )));
    }
    Ok(Json(serde_json::json!({ "ok": true })))
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

/// POST /gec/sessions/:id/record — 记录犯错
#[derive(serde::Deserialize)]
pub struct SessionRecordRequest {
    pub lang: String,
    pub error_type: String,
}

/// PUT /gec/sessions/:id/profile — 更新学习者画像（母语 / 自评 CEFR）
///
/// 部分更新：None / 空串字段保持原值；会话不存在时按请求 lang 建档。
/// 画像注入 LLM 讲解 prompt（母语 + 错误分布）并作为讲解模板 CEFR 缺省档位。
#[derive(serde::Deserialize)]
pub struct SessionProfileRequest {
    /// 请求会话的检测语言（建档时用；已有会话不改 meta.lang）
    #[serde(default)]
    pub lang: Option<String>,
    #[serde(default)]
    pub native_language: Option<String>,
    #[serde(default)]
    pub cefr: Option<String>,
}

pub async fn update_profile(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<SessionProfileRequest>,
) -> Json<SessionRecord> {
    let lang = req.lang.unwrap_or_else(|| "zh".into());
    Json(
        state
            .sessions
            .set_profile(&id, &lang, req.native_language, req.cefr),
    )
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

// ─── 统一错误画像（Phase 3）──────────────────────────────────────────────────
//
// 学习场景的错题本（mistakes + mistake_mastery）与审校场景的作者习惯性错误
// 是同一模型的两个视图：**同一主体（会话）随时间沉淀的错误模式**。
// 投影逻辑统一在 shared_model::profile，两端（REST / 桌面命令）共用同一口径。
//
// 数据基准（basis）：
// - `confirmed`：错题本非空 → 以确认错误为准（含掌握度摘要）。误报与未采纳
//   建议不污染画像——这是相对旧实现（纯 detect_history 聚合）的语义修正。
// - `detected`：错题本为空的旧会话回退到检测记录聚合（向后兼容）。

use shared_model::profile::{
    BASIS_CONFIRMED, BASIS_DETECTED, ErrorObservation, ErrorProfile, build_error_profile,
    mastery_summary_from_mistakes, observations_from_mistakes,
};

/// GET /gec/sessions/:id/analysis — 会话统一错误画像
pub async fn analysis(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<ErrorProfile>, ApiError> {
    let mistakes = state.sessions.list_mistakes(&id);
    if !mistakes.is_empty() {
        let observations = observations_from_mistakes(&mistakes);
        let summary = mastery_summary_from_mistakes(&mistakes);
        return Ok(Json(build_error_profile(
            &id,
            BASIS_CONFIRMED,
            &observations,
            Some(summary),
        )));
    }

    // 回退：检测记录基准（旧会话无采纳动作）
    let records = state
        .detect_history
        .list(Some(&id), None, 200)
        .map_err(|e| ApiError::Internal(format!("查询历史失败: {e}")))?;
    let mut observations: Vec<ErrorObservation> = Vec::new();
    for record in &records {
        let errors: Vec<shared_model::Error> =
            serde_json::from_str(&record.errors_json).unwrap_or_default();
        for err in errors {
            observations.push(ErrorObservation {
                error_type: err.error_type,
                original: err.original,
                correction: err.correction,
                weight: 1,
            });
        }
    }
    Ok(Json(build_error_profile(
        &id,
        BASIS_DETECTED,
        &observations,
        None,
    )))
}

// ─── 全局错题本（跨会话，学习闭环功能闭合）─────────────────────────────────
//
// 错题本体仍是单一事实源（`mistakes` 表），全局视图 = 跨会话投影；记忆状态按
// 错题键（语言+类型+原文+修正）挂 `global_mastery`，跨会话共享、删会话不丢。

/// GET /gec/mistakes — 全局错题本（跨会话聚合；`sessions_seen` = 出现会话数）
pub async fn global_mistakes(
    State(state): State<Arc<AppState>>,
) -> Json<Vec<shared_model::Mistake>> {
    Json(state.sessions.list_global_mistakes())
}

/// GET /gec/review — 全局到期复习队列（跨会话；已掌握不进入）
pub async fn global_review_queue(
    State(state): State<Arc<AppState>>,
) -> Json<Vec<shared_model::Mistake>> {
    Json(state.sessions.list_global_review_queue())
}

/// POST /gec/mistakes/:gid/review — 全局错题复习结果（FSRS 四键调度挂在错题键上）
pub async fn review_global_mistake(
    State(state): State<Arc<AppState>>,
    Path(gid): Path<i64>,
    Json(req): Json<ReviewRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let rating = req.resolve()?;
    if !state.sessions.record_global_review(gid, rating) {
        return Err(ApiError::NotFound(format!(
            "global mistake#{gid} 不存在或复习记录写入失败"
        )));
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// POST /gec/mistakes/:gid/review/undo — 撤销全局错题最近一次复习
pub async fn undo_global_review(
    State(state): State<Arc<AppState>>,
    Path(gid): Path<i64>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if !state.sessions.undo_global_review(gid) {
        return Err(ApiError::NotFound(format!(
            "global mistake#{gid} 无可撤销的复习"
        )));
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// GET /gec/analysis — 全局统一错误画像（确认错误基准，跨会话；
/// 不回退 detected——全局层面未采纳建议不应污染画像）
pub async fn global_analysis(State(state): State<Arc<AppState>>) -> Json<ErrorProfile> {
    let mistakes = state.sessions.list_global_mistakes();
    let observations = observations_from_mistakes(&mistakes);
    let summary = mastery_summary_from_mistakes(&mistakes);
    Json(build_error_profile(
        "global",
        BASIS_CONFIRMED,
        &observations,
        Some(summary),
    ))
}
