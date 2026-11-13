//! 文档级审校作业（10 万字+ 文档）：请求变作业，块级 checkpoint / 断点续跑
//!
//! 同步 `/gec/detect` 在文档规模下不成立（上下文上限 / 分钟级延迟 / 失败放大，
//! 见 doc-detect-flow 设计）；本路由把检测变成后台作业：
//! - `POST /gec/doc-jobs`：创建 + 切块落库 + 后台并行执行，立即返回 `job_id`；
//! - `GET /gec/doc-jobs/:id`：状态 + 进度 + 深度复查覆盖率；
//! - `GET /gec/doc-jobs/:id/results`：聚合错误（文档字节坐标）；
//! - `POST /gec/doc-jobs/:id/cancel`：取消（未开跑块保持 pending）；
//! - `POST /gec/doc-jobs/:id/retry`：failed/parse_failed 块重置重跑（续跑）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use axum::extract::Path as AxPath;
use axum::extract::{Query, State};
use axum::Json;

use crate::routes::ApiError;
use crate::state::AppState;

#[derive(serde::Deserialize)]
pub struct CreateReq {
    pub lang: String,
    pub text: String,
    /// 目标块大小（字符，默认 `GEC_LLM_CHUNK_TARGET` > 1500）
    #[serde(default)]
    pub chunk_target: Option<usize>,
}

fn internal(e: rusqlite::Error) -> ApiError {
    ApiError::Internal(format!("文档作业存储错误: {e}"))
}

/// 后台执行（spawn）：取消位入 map，完成后移除
fn spawn_run(
    state: &AppState,
    id: &str,
    llm: Arc<gec_core::LlmGecEngine>,
) -> Result<Arc<AtomicBool>, ApiError> {
    let cancel = Arc::new(AtomicBool::new(false));
    state
        .doc_job_cancel
        .lock()
        .unwrap()
        .insert(id.to_string(), cancel.clone());
    let store = state.doc_jobs.clone();
    let job_id = id.to_string();
    let cancel_bg = cancel.clone();
    let cleanup = state.doc_job_cancel.clone();
    let cleanup_id = job_id.clone();
    tokio::spawn(async move {
        let r = gec_core::run_doc_job(
            store,
            llm,
            &job_id,
            gec_core::docjob::default_concurrency(),
            cancel_bg,
        )
        .await;
        if let Err(e) = r {
            tracing::warn!(job_id, error = %e, "文档作业执行失败");
        }
        cleanup.lock().unwrap().remove(&cleanup_id);
    });
    Ok(cancel)
}

pub async fn create(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if req.text.is_empty() {
        return Err(ApiError::BadRequest("text 不能为空".into()));
    }
    let llm = state
        .llm_detect_engine(&req.lang)
        .ok_or_else(|| ApiError::ServiceUnavailable("文档级审校需要 LLM，请先在设置中配置".into()))?;
    let target = req
        .chunk_target
        .unwrap_or_else(gec_core::docjob::default_chunk_target);
    let id = uuid::Uuid::new_v4().to_string();
    let chunks = state
        .doc_jobs
        .create_job(&id, &req.lang, &req.text, None, target)
        .map_err(internal)?;
    spawn_run(&state, &id, llm)?;
    Ok(Json(serde_json::json!({
        "job_id": id,
        "chunks": chunks,
        "status": "running",
    })))
}


#[derive(serde::Deserialize)]
pub struct ListReq {
    #[serde(default = "default_limit")]
    pub limit: usize,
}

fn default_limit() -> usize {
    50
}

pub async fn list(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ListReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let rows = state.doc_jobs.list_jobs(q.limit.min(200)).map_err(internal)?;
    let jobs: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|(m, p)| {
            serde_json::json!({
                "id": m.id, "lang": m.lang, "status": m.status,
                "created_at": m.created_at,
                "progress": p, "coverage": p.coverage(),
            })
        })
        .collect();
    Ok(Json(serde_json::json!({ "jobs": jobs })))
}

pub async fn get(
    State(state): State<Arc<AppState>>,
    AxPath(id): AxPath<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let job = state
        .doc_jobs
        .job(&id)
        .map_err(internal)?
        .ok_or_else(|| ApiError::NotFound(format!("作业不存在: {id}")))?;
    let progress = state.doc_jobs.progress(&id).map_err(internal)?;
    Ok(Json(serde_json::json!({
        "id": job.id,
        "lang": job.lang,
        "status": job.status,
        "created_at": job.created_at,
        "text_chars": job.text.chars().count(),
        "chunk_target": job.chunk_target,
        "progress": progress,
        "coverage": progress.coverage(),
        "cancelling": state
            .doc_job_cancel
            .lock()
            .unwrap()
            .get(&id)
            .is_some_and(|f| f.load(Ordering::Relaxed)),
    })))
}

pub async fn results(
    State(state): State<Arc<AppState>>,
    AxPath(id): AxPath<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if state.doc_jobs.job(&id).map_err(internal)?.is_none() {
        return Err(ApiError::NotFound(format!("作业不存在: {id}")));
    }
    let progress = state.doc_jobs.progress(&id).map_err(internal)?;
    let errors = state.doc_jobs.aggregated_errors(&id).map_err(internal)?;
    let chunks: Vec<serde_json::Value> = state
        .doc_jobs
        .chunks(&id)
        .map_err(internal)?
        .into_iter()
        .map(|c| {
            serde_json::json!({
                "index": c.index, "owner": c.owner, "status": c.status, "error": c.error,
            })
        })
        .collect();
    Ok(Json(serde_json::json!({
        "errors": errors,
        "progress": progress,
        "coverage": progress.coverage(),
        // 覆盖率不满时如实标注：部分块未复查，结果不完整
        "complete": progress.pending == 0 && progress.failed == 0 && progress.parse_failed == 0,
        "chunks": chunks,
    })))
}

pub async fn cancel(
    State(state): State<Arc<AppState>>,
    AxPath(id): AxPath<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let job = state
        .doc_jobs
        .job(&id)
        .map_err(internal)?
        .ok_or_else(|| ApiError::NotFound(format!("作业不存在: {id}")))?;
    if let Some(flag) = state.doc_job_cancel.lock().unwrap().get(&id) {
        flag.store(true, Ordering::Relaxed);
    } else if !matches!(job.status.as_str(), "created" | "cancelled") {
        return Err(ApiError::BadRequest(format!(
            "作业已处于终态: {}",
            job.status
        )));
    }
    Ok(Json(serde_json::json!({ "ok": true, "cancelling": true })))
}

/// 失败块一键重跑（reset failed/parse_failed → pending 后续跑）
pub async fn retry(
    State(state): State<Arc<AppState>>,
    AxPath(id): AxPath<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let job = state
        .doc_jobs
        .job(&id)
        .map_err(internal)?
        .ok_or_else(|| ApiError::NotFound(format!("作业不存在: {id}")))?;
    if state.doc_job_cancel.lock().unwrap().contains_key(&id) {
        return Err(ApiError::BadRequest("作业正在运行".into()));
    }
    let llm = state
        .llm_detect_engine(&job.lang)
        .ok_or_else(|| ApiError::ServiceUnavailable("文档级审校需要 LLM，请先在设置中配置".into()))?;
    let reset = state.doc_jobs.reset_failed_chunks(&id).map_err(internal)?;
    spawn_run(&state, &id, llm)?;
    Ok(Json(serde_json::json!({ "ok": true, "reset_chunks": reset })))
}
