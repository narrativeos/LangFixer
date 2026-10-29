//! 多模态审校 API（`MULTIMODAL.md` §8 / §9）——**薄传输层**
//!
//! - `POST /gec/media/probes` — 三家协作工具只读探测（版本 / 路径 / 能力 / 降级原因）
//! - `POST /gec/media/inspect?filename=` — 载体能力探测（尺寸 / 文字层 / 可编辑性）
//! - `POST /gec/media/jobs?filename=` — 建 job 并跑全链路（副本隔离，**不改原件**）
//! - `GET /gec/media/jobs` / `GET /gec/media/jobs/:id` — 列表 / 状态轮询
//! - `GET /gec/media/jobs/:id/preview` — 扁平预览字节
//! - `POST /gec/media/jobs/:id/apply` — 回写（默认 Overlay）
//! - `POST /gec/media/report` — 勾选后的结果导出成正误表（JSON / Markdown / HTML / docx）
//! - `GET /gec/media/reports/:id?format=json|md|html|docx` — 取导出产物
//!
//! 链路本身在 `media_flow`（api-server 与桌面端共用同一份实现）；本文件只做三件事：
//! 解析 HTTP 请求、把 [`FlowError`] 映射成状态码、把服务端检测级联（`run_detection`：
//! 缓存 / 历史复用 / 规则开关 / 白名单 / 三级响应 / layers 全部照旧）作为
//! [`CarrierDetector`] 注入编排层。

use std::sync::Arc;

use async_trait::async_trait;
use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use media_flow::{
    CarrierDetector, DetectResult, FlowCtx, FlowError, JobOptions, MediaJob, ReportRequest, flow,
};

use crate::routes::ApiError;
use crate::routes::detect::run_detection;
use crate::state::AppState;

/// 编排层错误 → HTTP 状态码（口径与原 `media_err` 一致）
fn flow_err(e: FlowError) -> ApiError {
    match e {
        FlowError::BadRequest(m) => ApiError::BadRequest(m),
        FlowError::NotFound(m) => ApiError::NotFound(m),
        FlowError::Unavailable(m) => ApiError::ServiceUnavailable(m),
        FlowError::Upstream(m) => ApiError::Upstream(m),
        FlowError::Internal(m) => ApiError::Internal(m),
    }
}

/// 服务端检测级联的载体接缝：载体链路要的只是「判定 + 层状态」，
/// 其余（缓存 / 历史 / 白名单 / 三级响应）全部沿用 `/gec/detect` 的实现
struct ServerDetector {
    state: Arc<AppState>,
}

#[async_trait]
impl CarrierDetector for ServerDetector {
    async fn detect(
        &self,
        lang: &str,
        text: &str,
        level: Option<u8>,
        force_llm: bool,
        category: Option<&str>,
    ) -> Result<DetectResult, FlowError> {
        let category = category.map(|c| c.to_string());
        let (outcome, _) =
            run_detection(self.state.clone(), lang, text, &category, level, force_llm)
                .await
                .map_err(flow_err_from_api)?;
        Ok(DetectResult {
            errors: outcome.errors,
            layers: outcome
                .layers
                .iter()
                .map(|l| serde_json::to_value(l).unwrap_or_default())
                .collect(),
        })
    }

    fn llm_ready(&self, lang: &str) -> bool {
        self.state.llm_detect_engine(lang).is_some()
    }
}

/// `run_detection` 的错误进编排层（`ApiError` 与 `FlowError` 的语义一一对应）
fn flow_err_from_api(e: ApiError) -> FlowError {
    match e {
        ApiError::BadRequest(m) => FlowError::BadRequest(m),
        ApiError::NotFound(m) => FlowError::NotFound(m),
        ApiError::Unauthorized(m) => FlowError::BadRequest(m),
        ApiError::Upstream(m) => FlowError::Upstream(m),
        ApiError::ServiceUnavailable(m) => FlowError::Unavailable(m),
        ApiError::Internal(m) => FlowError::Internal(m),
    }
}

/// 每次请求构造编排上下文（设置即时生效：读当前设置，不缓存）
fn flow_ctx(state: &Arc<AppState>) -> FlowCtx {
    let settings = state.settings.read().unwrap();
    FlowCtx {
        store: state.media_jobs.clone(),
        media: settings.media.clone(),
        llm: settings.llm.clone(),
        prompts: state.llm_templates.read().unwrap().clone(),
    }
}

#[derive(Deserialize)]
pub struct InspectQuery {
    pub filename: String,
}

#[derive(Deserialize)]
pub struct JobQuery {
    pub filename: String,
    /// 回写模式偏好（缺省取设置里的 apply_mode，默认 overlay）
    #[serde(default)]
    pub apply_mode: Option<String>,
    /// 检测语言（缺省 zh）
    #[serde(default)]
    pub lang: Option<String>,
    /// 三级响应级别（语义同 /gec/detect）
    #[serde(default)]
    pub level: Option<u8>,
    /// 强制 LLM 兜底（语义同 /gec/detect）
    #[serde(default)]
    pub force_llm: Option<bool>,
    /// 仅返回指定分类（语义同 /gec/detect）
    #[serde(default)]
    pub category: Option<String>,
    /// 媒体模式：`simple`（零 CLI：段落级定位 + 只读文档视图）/ `advanced`（CLI 读结构层并回写）。
    /// 缺省取设置里的 `media.mode`（默认 advanced）；不可用时后端会给出原因与退路，不会静默换档
    #[serde(default)]
    pub mode: Option<String>,
}

/// POST /gec/media/probes — 三家工具探测（只读，不写文件）
pub async fn probes(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(flow::probes(&flow_ctx(&state)).await)
}

/// POST /gec/media/inspect?filename= — 载体探测（不落 job，用于上传前预判）
pub async fn inspect(
    State(state): State<Arc<AppState>>,
    Query(q): Query<InspectQuery>,
    body: Bytes,
) -> Result<Json<serde_json::Value>, ApiError> {
    flow::inspect_carrier(&flow_ctx(&state), &q.filename, &body)
        .await
        .map(Json)
        .map_err(flow_err)
}

/// POST /gec/media/jobs?filename= — 建 job 并跑全链路（异步，返回 job 供轮询）
pub async fn create_job(
    State(state): State<Arc<AppState>>,
    Query(q): Query<JobQuery>,
    body: Bytes,
) -> Result<Json<MediaJob>, ApiError> {
    let ctx = flow_ctx(&state);
    let detector = Arc::new(ServerDetector {
        state: state.clone(),
    });
    let opts = JobOptions {
        apply_mode: q.apply_mode,
        lang: q.lang,
        level: q.level,
        force_llm: q.force_llm,
        category: q.category,
        mode: q.mode,
    };
    flow::create_job(&ctx, detector, &q.filename, &body, opts)
        .map(Json)
        .map_err(flow_err)
}

/// GET /gec/media/jobs — job 列表（创建时间倒序）
pub async fn jobs(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(serde_json::json!(state.media_jobs.read().unwrap().list()))
}

/// GET /gec/media/jobs/:id — 状态轮询
pub async fn job(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<MediaJob>, ApiError> {
    state
        .media_jobs
        .read()
        .unwrap()
        .get(&id)
        .cloned()
        .map(Json)
        .ok_or_else(|| ApiError::NotFound(format!("job 不存在: {id}")))
}

/// GET /gec/media/jobs/:id/preview — 预览字节（无 result.png 时回退副本原件）
pub async fn preview(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let (path, ctype) = flow::preview_file(&flow_ctx(&state), &id).map_err(flow_err)?;
    let bytes = std::fs::read(&path).map_err(|e| ApiError::NotFound(format!("预览不可用：{e}")))?;
    Ok(([(axum::http::header::CONTENT_TYPE, ctype)], bytes).into_response())
}

#[derive(Deserialize)]
pub struct ApplyBody {
    /// 覆盖 job 的回写模式（UI 切换后重应用）
    #[serde(default)]
    pub apply_mode: Option<String>,
}

/// POST /gec/media/jobs/:id/apply — 回写（幂等：重跑先清历史 `mark-*` 图层）
pub async fn apply(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<ApplyBody>,
) -> Result<Json<MediaJob>, ApiError> {
    flow::apply_job(&flow_ctx(&state), &id, body.apply_mode.as_deref())
        .await
        .map(Json)
        .map_err(flow_err)
}

/// POST /gec/media/report — 把多个载体的推演结果汇成一张正误表（与专家同构）
pub async fn report(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ReportRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    flow::build_report(&flow_ctx(&state), body)
        .await
        .map(Json)
        .map_err(flow_err)
}

#[derive(Deserialize)]
pub struct ReportQuery {
    #[serde(default)]
    pub format: Option<String>,
}

/// GET /gec/media/reports/:id?format=json|md|html|docx
pub async fn report_file(
    State(state): State<Arc<AppState>>,
    Path(rid): Path<String>,
    Query(q): Query<ReportQuery>,
) -> Result<Response, ApiError> {
    let (path, ctype) = flow::report_file_path(
        &flow_ctx(&state),
        &rid,
        q.format.as_deref().unwrap_or("json"),
    )
    .map_err(flow_err)?;
    let bytes =
        std::fs::read(&path).map_err(|e| ApiError::NotFound(format!("报告产物不可用：{e}")))?;
    Ok(([(axum::http::header::CONTENT_TYPE, ctype)], bytes).into_response())
}
