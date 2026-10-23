use std::sync::Arc;

use axum::extract::{Json, State};
use axum::response::IntoResponse;
use futures_util::StreamExt;

use crate::routes::ApiError;
use crate::state::AppState;

#[derive(serde::Deserialize)]
pub struct ExplainRequest {
    /// 语言（zh / en / ja）
    pub lang: String,
    /// 错误类型（如 spelling / grammar / article）
    pub error_type: String,
    /// 归因标签（如 knowledge_gap / carelessness）
    pub attribution: Option<String>,
    /// CEFR 水平（A1-C2；缺省 A1）
    pub cefr: Option<String>,
    /// 会话 ID（可选；提供时注入其母语与历史错误分布）
    pub session_id: Option<String>,
    /// 会话历史犯错次数（可选；提供 session_id 时以档案为准）
    pub session_error_count: Option<u32>,
    /// 讲解目标语言（可选；如 "English"；缺省时跟随检测语言）
    pub target_language: Option<String>,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct ExplainResponse {
    /// 个性化讲解文本
    pub explanation: String,
    /// 个性化练习（2-3 题）
    pub exercises: String,
    /// 使用的模型
    pub model: String,
    /// 提示词模板版本
    pub template_version: String,
    /// 使用的提示词模板键（如 zh_a2）
    pub template: String,
}

/// 将 LLM 输出按 `【练习】` 标记切分为（讲解, 练习）
pub fn split_explanation(full: &str) -> (String, String) {
    match full.split_once("【练习】") {
        Some((ex, exx)) => (ex.trim().to_string(), exx.trim().to_string()),
        None => (full.to_string(), String::new()),
    }
}

/// 从会话档案取画像上下文（母语 + 历史错误类型分布 + 总犯错次数）
fn session_context(
    state: &AppState,
    session_id: Option<&str>,
) -> (Option<String>, Option<String>, u32) {
    match session_id {
        Some(id) => {
            let record = state.sessions.get_or_create(id, "zh");
            let distribution = if record.error_type_counts.is_empty() {
                None
            } else {
                let mut items: Vec<(String, u32)> = record
                    .error_type_counts
                    .iter()
                    .map(|(k, v)| (k.clone(), *v))
                    .collect();
                items.sort_by(|a, b| b.1.cmp(&a.1));
                Some(
                    items
                        .iter()
                        .map(|(k, v)| format!("{k}: {v}"))
                        .collect::<Vec<_>>()
                        .join(", "),
                )
            };
            let native = record.meta.lang.clone();
            (Some(native), distribution, record.total_errors())
        }
        None => (None, None, 0),
    }
}

/// POST /gec/explain — LLM 个性化讲解（非流式；外部 API 调用的默认模式）
///
/// 按 `lang` + `cefr` 选择提示词模板，注入归因上下文与用户画像后调用 LLM。
/// 未配置 LLM（环境变量 `LLM_API_KEY` 或设置界面）时返回 503（客户端应回退到静态补救建议）。
pub async fn explain(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ExplainRequest>,
) -> Result<Json<ExplainResponse>, ApiError> {
    let llm = state.llm_service();
    if !llm.is_available() {
        return Err(ApiError::ServiceUnavailable(
            "LLM 服务未启用（请设置 LLM_API_KEY 或在「设置」界面配置 LLM 参数）".into(),
        ));
    }
    let cefr = req.cefr.unwrap_or_else(|| "A1".into());
    let attribution = req.attribution.clone().unwrap_or_else(|| "unknown".into());
    let (native_language, distribution, history_from_store) =
        session_context(&state, req.session_id.as_deref());
    let history = req
        .session_error_count
        .or(Some(history_from_store))
        .unwrap_or(0);

    let template_key = llm
        .templates
        .for_lang_cefr(&req.lang, &cefr)
        .map(|t| format!("{}_{}", t.lang, t.cefr))
        .or_else(|| Some(format!("{}_generic", req.lang)))
        .ok_or_else(|| ApiError::NotFound(format!("语言 {} 无匹配提示词模板", req.lang)))?;

    let full = llm
        .explain(
            &req.lang,
            &req.error_type,
            &attribution,
            &cefr,
            history,
            native_language.as_deref(),
            distribution.as_deref(),
            req.target_language.as_deref(),
        )
        .await
        .map_err(|e| ApiError::Upstream(e.to_string()))?;
    let (explanation, exercises) = split_explanation(&full);

    let template_version = llm
        .templates
        .get(&template_key)
        .map(|t| t.version.clone())
        .unwrap_or_default();

    Ok(Json(ExplainResponse {
        explanation,
        exercises,
        model: llm.config.model.clone(),
        template_version,
        template: template_key,
    }))
}

/// POST /gec/explain/stream — LLM 个性化讲解（SSE 流式；前端请求的默认模式）
///
/// 事件序列：`meta`（模型/模板/版本）→ 若干 `token`（讲解 + 练习逐段）→ `done`（完整结果）。
/// 未配置 LLM（环境变量 `LLM_API_KEY` 或设置界面）时返回 503（客户端应回退到静态补救建议）。
pub async fn explain_stream(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ExplainRequest>,
) -> Result<axum::response::Response, ApiError> {
    let llm = state.llm_service();
    if !llm.is_available() {
        return Err(ApiError::ServiceUnavailable(
            "LLM 服务未启用（请设置 LLM_API_KEY 或在「设置」界面配置 LLM 参数）".into(),
        ));
    }
    let cefr = req.cefr.unwrap_or_else(|| "A1".into());
    let attribution = req.attribution.clone().unwrap_or_else(|| "unknown".into());
    let (native_language, distribution, history_from_store) =
        session_context(&state, req.session_id.as_deref());
    let history = req
        .session_error_count
        .or(Some(history_from_store))
        .unwrap_or(0);

    let (template, stream) = llm
        .stream_explain(
            &req.lang,
            &req.error_type,
            &attribution,
            &cefr,
            history,
            native_language.as_deref(),
            distribution.as_deref(),
            req.target_language.as_deref(),
        )
        .await
        .map_err(|e| ApiError::Upstream(e.to_string()))?;

    let meta = serde_json::json!({
        "model": llm.config.model,
        "template": format!("{}_{}", template.lang, template.cefr),
        "template_version": template.version,
    });
    let meta_event = axum::response::sse::Event::default()
        .event("meta")
        .json_data(meta)
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    // 累积全文，供 done 事件切分讲解/练习
    let full = Arc::new(tokio::sync::Mutex::new(String::new()));
    let token_events = stream.filter_map({
        let full = full.clone();
        move |item| {
            let full = full.clone();
            async move {
                match item {
                    Ok(Some(token)) => {
                        full.lock().await.push_str(&token);
                        Some(
                            axum::response::sse::Event::default()
                                .event("token")
                                .json_data(serde_json::json!({ "token": token }))
                                .map_err(|e| ApiError::Internal(e.to_string())),
                        )
                    }
                    Ok(None) => None,
                    Err(e) => Some(Err(ApiError::Upstream(e.to_string()))),
                }
            }
        }
    });
    let done_event = futures_util::stream::once({
        let full = full.clone();
        async move {
            let full = full.lock().await.clone();
            let (explanation, exercises) = split_explanation(&full);
            axum::response::sse::Event::default()
                .event("done")
                .json_data(
                    serde_json::json!({ "explanation": explanation, "exercises": exercises }),
                )
                .map_err(|e| ApiError::Internal(e.to_string()))
        }
    });

    let sse = futures_util::stream::once(async move { Ok::<_, ApiError>(meta_event) })
        .chain(token_events)
        .chain(done_event);
    Ok(axum::response::sse::Sse::new(sse)
        .keep_alive(axum::response::sse::KeepAlive::default())
        .into_response())
}
