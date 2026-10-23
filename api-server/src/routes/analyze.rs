use std::sync::Arc;

use axum::extract::{Json, State};
use gec_core::{DetectOutcome, DetectorError};
use shared_model::{AnalyzedError, AttributionContext};

use crate::APP_VERSION;
use crate::routes::ApiError;
use crate::routes::detect;
use crate::state::AppState;

#[derive(serde::Deserialize)]
pub struct AnalyzeRequest {
    pub lang: String,
    pub text: String,
    /// 会话 ID（可选；提供时按其历史犯错次数做新手/老手判定）
    pub session_id: Option<String>,
    /// 三级响应级别：1 = 规则（+ 决策模型，若启用）；
    /// 2 = + 决策模型明确弃权时 LLM 兜底；3 = + 强制 LLM（缺省 1）
    #[serde(default)]
    pub level: Option<u8>,
    /// 「漏报」强制兜底：无视 level 触发条件，无条件对全文跑一次 LLM 兜底
    /// （未配置 LLM 时返回 503）
    #[serde(default)]
    pub force_llm: bool,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct AnalyzeResponse {
    pub lang: String,
    pub errors: Vec<AnalyzedError>,
    /// 实际使用的会话 ID（请求未提供时自动生成）
    pub session_id: String,
    /// 决策模型是否弃权（仅决策层启用时有意义）
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub decision_abstained: bool,
    /// 各阶段计时（规则 / NLP（含预热）/ 决策 / L3 / 召回 / LLM / 总耗时）
    #[serde(default)]
    pub timings: gec_core::DetectTimings,
    /// 本次结果来自历史复用（同文本 + 参数一致 + 无 LLM 的近期记录；
    /// 前端可展示「来自历史」提示）
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub from_history: bool,
    /// LLM 兜底层是否实际执行且未检出错误（「LLM 复查确认干净」）
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub llm_confirmed_clean: bool,
    /// 层级贡献摘要：本次检测实际参与的每层状态与检出数（前端「检测路径」）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<gec_core::LayerReport>,
}

/// POST /gec/analyze — 检测 + 归因一站式接口（自动持久化到历史）
pub async fn analyze(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AnalyzeRequest>,
) -> Result<Json<AnalyzeResponse>, ApiError> {
    let level = req.level.unwrap_or(1);
    if !(1..=3).contains(&level) {
        return Err(ApiError::BadRequest(format!(
            "无效响应级别: {level}（应为 1-3）"
        )));
    }
    // 读锁内仅克隆引擎 Arc（锁立即释放），避免 guard 跨 await（非 Send）
    let engine = state
        .router
        .read()
        .unwrap()
        .get(&req.lang)
        .cloned()
        .ok_or_else(|| DetectorError::UnsupportedLanguage(req.lang.clone()))?;
    // level 1 = 常规检测；level 2/3 或 force_llm = 完整级联（LLM 兜底按需/强制触发，不缓存）
    // 统一走 detect_level：各阶段计时（含 NLP 预热）随结果返回并持久化
    let (mut outcome, from_history): (DetectOutcome, bool) = if level <= 1 && !req.force_llm {
        // 历史复用：同文本 + 参数一致 + 无 LLM 的最近记录直接复用（跳过引擎）
        if let Some(reused) =
            detect::try_reuse_history(state.as_ref(), &req.lang, &req.text, &None, 1).await
        {
            (reused, true)
        } else {
            (engine.detect_level(&req.text, 1, false, None).await?, false)
        }
    } else {
        let llm = state.llm_detect_engine(&req.lang);
        (
            engine
                .detect_level(&req.text, level, req.force_llm, llm.as_deref())
                .await?,
            false,
        )
    };
    // 与 detect 路由一致的后处理：过滤已禁用规则 + 自定义词白名单
    // （「放弃」反馈 / 「停用规则」建议在重检测时同样生效）
    detect::apply_suppression_filters(state.as_ref(), &req.lang, &mut outcome);
    let engine = state.tables.get(&req.lang);

    // 会话 ID：请求提供则用，否则自动生成
    let session_id = req
        .session_id
        .clone()
        .unwrap_or_else(|| format!("s_{}", uuid::Uuid::new_v4().simple()));

    let (history, meta) = {
        let record = state.sessions.get_or_create(&session_id, &req.lang);
        (record.total_errors(), Some(record.meta))
    };

    let mut analyzed = Vec::with_capacity(outcome.errors.len());
    for error in outcome.errors {
        let attribution = match &engine {
            Some(engine) => {
                // 英文错误：按 (error_type, original, correction) 推断具体知识点，
                // 让决策表命中细粒度规则（如 article.3 不定冠词）。
                // 数据优先：已发布知识点的改错对精确命中优先（新导入书自动参与归点），
                // 未命中回退静态映射。知识层开关关闭（测评消融臂）→ 只用静态映射。
                let knowledge_on = state.settings.read().unwrap().knowledge_enabled;
                let grammar_point = if req.lang == "en" {
                    let fallback = gec_en::classify_point(
                        &error.error_type,
                        &error.original,
                        error.correction.as_deref().unwrap_or(""),
                    );
                    if knowledge_on {
                        attribution_engine::knowledge::classify_point_en(
                            &state.knowledge,
                            &error.error_type,
                            &error.original,
                            error.correction.as_deref().unwrap_or(""),
                            fallback,
                        )
                    } else {
                        fallback
                    }
                } else {
                    None
                };
                let ctx = AttributionContext {
                    lang: req.lang.clone(),
                    error_type: error.error_type.clone(),
                    confidence: error.confidence,
                    session_error_count: history,
                    session_meta: meta.clone(),
                    grammar_point,
                };
                // 表级归因 + 知识库富化（点级补救/CEFR/实例 + 知识源引用；知识层开关关闭时跳过）
                let mut result = engine.attribute(&ctx).await?;
                if knowledge_on {
                    state.knowledge.enrich(&ctx, &mut result);
                }
                Some(result)
            }
            None => None,
        };
        analyzed.push(AnalyzedError { error, attribution });
    }

    // 自动持久化（含归因结果 + 各阶段计时 + 参数快照 + 文本哈希；后台写入，不阻塞检测响应）
    // 历史复用不写新记录（避免短时间反复检测刷出重复历史）
    if !from_history {
        let state = state.clone();
        let lang = req.lang.clone();
        let text = req.text.clone();
        let errors_json =
            serde_json::to_string(&analyzed.iter().map(|a| &a.error).collect::<Vec<_>>())
                .unwrap_or_else(|_| "[]".to_string());
        let analyzed_json = serde_json::to_string(&analyzed).ok();
        let timings_json = (!outcome.timings.is_empty())
            .then(|| serde_json::to_string(&outcome.timings).unwrap_or_else(|_| "{}".to_string()));
        let params = state
            .current_detect_params(&lang, level, req.force_llm, &None)
            .await;
        let params_json = serde_json::to_string(&params).ok();
        let text_hash = shared_model::text_hash(&text);
        let corrected_text = shared_model::apply_corrections(
            &text,
            &analyzed.iter().map(|a| a.error.clone()).collect::<Vec<_>>(),
        );
        let layers_json = (!outcome.layers.is_empty())
            .then(|| serde_json::to_string(&outcome.layers).unwrap_or_else(|_| "[]".to_string()));
        let session_id = session_id.clone();
        tokio::spawn(async move {
            if let Err(e) = state.detect_history.insert(
                Some(&session_id),
                &lang,
                &text,
                &errors_json,
                analyzed_json.as_deref(),
                timings_json.as_deref(),
                Some(APP_VERSION),
                params_json.as_deref(),
                Some(&text_hash),
                Some(corrected_text.as_str()),
                layers_json.as_deref(),
            ) {
                tracing::warn!(error = %e, "检测历史写入失败（不影响返回结果）");
            }
        });
    }

    Ok(Json(AnalyzeResponse {
        lang: req.lang,
        errors: analyzed,
        session_id,
        decision_abstained: outcome.decision_abstained,
        timings: outcome.timings,
        from_history,
        llm_confirmed_clean: outcome.llm_confirmed_clean,
        layers: outcome.layers,
    }))
}
