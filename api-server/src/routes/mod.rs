pub mod agent;
pub mod analyze;
pub mod attribute;
pub mod custom_words;
pub mod detect;
pub mod eval;
pub mod explain;
pub mod feedback;
pub mod history;
pub mod knowledge;
pub mod knowledge_dataset;
pub mod knowledge_ingest;
pub mod l3;
pub mod languages;
pub mod learned_knowledge;
pub mod llm_feedback;
pub mod media;
pub mod optimization;
pub mod reflection_center;
pub mod rules;
pub mod session;
pub mod settings;
pub mod tables;
pub mod training_center;

use std::sync::Arc;

use axum::Router;
use axum::response::IntoResponse;

use crate::state::AppState;

/// API 统一错误
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    #[error("upstream error: {0}")]
    Upstream(String),
    #[error("service unavailable: {0}")]
    ServiceUnavailable(String),
    #[error("internal error: {0}")]
    Internal(String),
}

impl ApiError {
    fn status(&self) -> http::StatusCode {
        match self {
            ApiError::BadRequest(_) => http::StatusCode::BAD_REQUEST,
            ApiError::NotFound(_) => http::StatusCode::NOT_FOUND,
            ApiError::Unauthorized(_) => http::StatusCode::UNAUTHORIZED,
            ApiError::Upstream(_) => http::StatusCode::BAD_GATEWAY,
            ApiError::ServiceUnavailable(_) => http::StatusCode::SERVICE_UNAVAILABLE,
            ApiError::Internal(_) => http::StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let status = self.status();
        let body = serde_json::json!({ "error": self.to_string() });
        (status, axum::Json(body)).into_response()
    }
}
impl From<gec_core::DetectorError> for ApiError {
    fn from(err: gec_core::DetectorError) -> Self {
        match err {
            gec_core::DetectorError::UnsupportedLanguage(lang) => {
                ApiError::NotFound(format!("不支持的语言: {lang}"))
            }
            gec_core::DetectorError::ServiceUnavailable(msg) => ApiError::Upstream(msg),
            gec_core::DetectorError::DetectionFailed(msg) => ApiError::Internal(msg),
            // LLM 兜底层触发但未配置 LLM → 503（前端提示去设置界面配置）
            gec_core::DetectorError::LlmNotConfigured(msg) => ApiError::ServiceUnavailable(msg),
        }
    }
}

impl From<attribution_engine::AttributionError> for ApiError {
    fn from(err: attribution_engine::AttributionError) -> Self {
        match err {
            attribution_engine::AttributionError::LoadFailed(msg) => ApiError::BadRequest(msg),
            attribution_engine::AttributionError::EvaluationFailed(msg) => ApiError::Internal(msg),
        }
    }
}

/// 构建路由
pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/gec/detect", axum::routing::post(detect::detect))
        // ── 多模态审校（MULTIMODAL.md §8）：上传载体可达数十 MB，单独放宽 body limit ──
        .nest(
            "/gec/media",
            Router::new()
                .route("/probes", axum::routing::post(media::probes))
                .route("/inspect", axum::routing::post(media::inspect))
                .route(
                    "/jobs",
                    axum::routing::post(media::create_job).get(media::jobs),
                )
                .route("/jobs/:id", axum::routing::get(media::job))
                .route("/jobs/:id/preview", axum::routing::get(media::preview))
                .route("/jobs/:id/apply", axum::routing::post(media::apply))
                .route("/report", axum::routing::post(media::report))
                .route("/reports/:id", axum::routing::get(media::report_file))
                .layer(axum::extract::DefaultBodyLimit::max(64 * 1024 * 1024)),
        )
        .route("/gec/eval/run", axum::routing::post(eval::run))
        .route("/gec/eval/job/:id", axum::routing::get(eval::job))
        .route("/gec/eval/reports", axum::routing::get(eval::reports))
        .route("/gec/eval/report/:id", axum::routing::get(eval::report))
        .route(
            "/gec/eval/contributions",
            axum::routing::post(optimization::contributions),
        )
        .route(
            "/gec/round/evaluate",
            axum::routing::post(optimization::evaluate),
        )
        .route("/gec/rounds", axum::routing::get(optimization::rounds))
        .route("/gec/round/:id", axum::routing::get(optimization::round))
        .route(
            "/gec/round/:id/confirm",
            axum::routing::post(optimization::confirm_round),
        )
        .route(
            "/gec/round/:id/reject",
            axum::routing::post(optimization::reject_round),
        )
        .route(
            "/gec/round/:id/execute",
            axum::routing::post(optimization::execute_round),
        )
        .route(
            "/gec/round/:id/report",
            axum::routing::get(optimization::round_report),
        )
        .route("/gec/attribute", axum::routing::post(attribute::attribute))
        .route("/gec/analyze", axum::routing::post(analyze::analyze))
        .route("/gec/explain", axum::routing::post(explain::explain))
        .route(
            "/gec/explain/stream",
            axum::routing::post(explain::explain_stream),
        )
        .route("/gec/tables", axum::routing::get(tables::list))
        .route(
            "/gec/tables/:lang",
            axum::routing::get(tables::get).put(tables::update),
        )
        .route(
            "/gec/tables/:lang/validate",
            axum::routing::post(tables::validate),
        )
        .route("/gec/tables/:lang/try", axum::routing::post(tables::try_))
        .route(
            "/gec/tables/:lang/versions",
            axum::routing::get(tables::versions),
        )
        .route(
            "/gec/tables/:lang/rollback",
            axum::routing::post(tables::rollback),
        )
        .route(
            "/gec/knowledge/sources",
            axum::routing::get(knowledge::list_sources).put(knowledge::update_sources),
        )
        .route(
            "/gec/knowledge/points",
            axum::routing::get(knowledge::list_points),
        )
        .route(
            "/gec/knowledge/points/:lang",
            axum::routing::put(knowledge::update_points),
        )
        .route(
            "/gec/knowledge/ingest",
            axum::routing::get(knowledge_ingest::list_ingests).post(knowledge_ingest::start_ingest),
        )
        .route(
            "/gec/knowledge/ingest/:source_id",
            axum::routing::get(knowledge_ingest::get_ingest),
        )
        .route(
            "/gec/knowledge/ingest/:source_id/plan",
            axum::routing::get(knowledge_ingest::get_ingest_plan),
        )
        .route(
            "/gec/knowledge/ingest/:source_id/drafts",
            axum::routing::get(knowledge_ingest::get_ingest_drafts),
        )
        .route(
            "/gec/knowledge/ingest/:source_id/publish",
            axum::routing::post(knowledge_ingest::publish_ingest),
        )
        .route(
            "/gec/knowledge/ingest/:source_id/discard",
            axum::routing::post(knowledge_ingest::discard_ingest),
        )
        .route(
            "/gec/knowledge/ingest/:source_id/derive-rules",
            axum::routing::post(knowledge_ingest::derive_rules),
        )
        .route(
            "/gec/knowledge/ingest/:source_id/rules-plan",
            axum::routing::get(knowledge_ingest::get_ingest_rules_plan),
        )
        .route(
            "/gec/knowledge/dataset/:source_id",
            axum::routing::get(knowledge_dataset::summary),
        )
        .route(
            "/gec/knowledge/dataset/:source_id/export",
            axum::routing::post(knowledge_dataset::export),
        )
        // 资源包导入请求体 = zip 压缩包 / 完整资源包 JSON（含 aux 决策数据），
        // 书本上传文件（epub/docx/pdf）同量级，均远超 axum 默认 2MB 上限。
        // Router::layer 只作用于「此前已注册」的全部路由，直接挂在主链上会把
        // 放宽泄漏给所有接口 → 大请求体路由单独装进子 Router 放宽（64MB）后
        // merge，其余路由维持默认上限
        .merge(
            Router::new()
                .route(
                    "/gec/knowledge/dataset/import",
                    axum::routing::post(knowledge_dataset::import),
                )
                .route(
                    "/gec/knowledge/parse-preview",
                    axum::routing::post(knowledge_ingest::parse_preview),
                )
                .route(
                    "/gec/knowledge/ingest/upload",
                    axum::routing::post(knowledge_ingest::upload_ingest),
                )
                .layer(axum::extract::DefaultBodyLimit::max(64 * 1024 * 1024)),
        )
        .route(
            "/gec/knowledge/dataset/:source_id/clean",
            axum::routing::post(knowledge_dataset::clean),
        )
        .route("/gec/rules", axum::routing::get(rules::list_rules))
        .route(
            "/gec/rules/:lang/toggles",
            axum::routing::put(rules::put_toggles),
        )
        .route(
            "/gec/rules/:id/examples",
            axum::routing::get(rules::list_examples),
        )
        .route("/gec/languages", axum::routing::get(languages::list))
        .route(
            "/gec/rules/:id/toggle",
            axum::routing::put(rules::toggle_rule),
        )
        .route(
            "/gec/:lang/detect",
            axum::routing::post(detect::detect_lang),
        )
        .route(
            "/gec/custom-words",
            axum::routing::get(custom_words::list_words).post(custom_words::add_word),
        )
        .route(
            "/gec/custom-words/:id",
            axum::routing::delete(custom_words::remove_word),
        )
        .route(
            "/gec/llm-feedback",
            axum::routing::post(llm_feedback::llm_feedback),
        )
        .route("/gec/feedback", axum::routing::post(feedback::feedback))
        .route("/gec/agent/run", axum::routing::post(agent::agent_run))
        .route(
            "/gec/agent/confirm",
            axum::routing::post(agent::agent_confirm),
        )
        .route(
            "/gec/feedback/action",
            axum::routing::post(feedback::feedback_action),
        )
        .route(
            "/gec/feedback/reflect-llm",
            axum::routing::post(feedback::reflect_llm),
        )
        .route(
            "/gec/feedback/history",
            axum::routing::get(feedback::feedback_history),
        )
        .route("/student/:id/profile", axum::routing::get(session::get))
        .route("/student/:id/record", axum::routing::post(session::record))
        .route(
            "/gec/sessions",
            axum::routing::post(session::create).delete(session::remove_all),
        )
        .route(
            "/gec/sessions/:id",
            axum::routing::get(session::get).delete(session::remove),
        )
        .route(
            "/gec/sessions/:id/record",
            axum::routing::post(session::record),
        )
        .route(
            "/gec/sessions/:id/analysis",
            axum::routing::get(session::analysis),
        )
        .route(
            "/gec/sessions/:id/archive",
            axum::routing::put(session::archive),
        )
        .route(
            "/gec/sessions/:id/restore",
            axum::routing::put(session::restore),
        )
        .route(
            "/gec/history",
            axum::routing::get(history::list).delete(history::clear),
        )
        .route(
            "/gec/history/:id",
            axum::routing::get(history::get).delete(history::remove),
        )
        .route(
            "/gec/settings",
            axum::routing::get(settings::get).put(settings::put),
        )
        .route("/gec/settings/test", axum::routing::post(settings::test))
        .route(
            "/gec/learned-knowledge",
            axum::routing::get(learned_knowledge::list).post(learned_knowledge::add),
        )
        .route(
            "/gec/learned-knowledge/:id",
            axum::routing::delete(learned_knowledge::remove),
        )
        .route(
            "/gec/knowledge/recall-test",
            axum::routing::post(learned_knowledge::recall_test),
        )
        .route("/gec/l3/collect", axum::routing::post(l3::collect))
        .route("/gec/l3/status", axum::routing::get(l3::status))
        .route("/gec/l3/retrain", axum::routing::post(l3::retrain))
        .route(
            "/gec/reflect/run",
            axum::routing::post(reflection_center::run),
        )
        .route(
            "/gec/reflect/reports",
            axum::routing::get(reflection_center::reports),
        )
        .route(
            "/gec/reflect/report/:id",
            axum::routing::get(reflection_center::report),
        )
        .route(
            "/gec/reflect/suggestions",
            axum::routing::get(reflection_center::suggestions),
        )
        .route(
            "/gec/reflect/suggestion/:id",
            axum::routing::put(reflection_center::update_suggestion),
        )
        .route(
            "/gec/train/pool",
            axum::routing::post(training_center::build_pool),
        )
        .route(
            "/gec/train/llm-data",
            axum::routing::post(training_center::build_llm_data),
        )
        .route(
            "/gec/train/l3/retrain",
            axum::routing::post(training_center::retrain_l3),
        )
        .route(
            "/gec/train/lexicon/apply",
            axum::routing::post(training_center::apply_lexicon),
        )
        .route(
            "/gec/train/suggestion/:id/run",
            axum::routing::post(training_center::run_suggestion),
        )
        .route("/gec/train/jobs", axum::routing::get(training_center::jobs))
        .route(
            "/gec/train/job/:id",
            axum::routing::get(training_center::job),
        )
        .with_state(state)
}
