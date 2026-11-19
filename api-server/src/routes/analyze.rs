use std::sync::Arc;

use axum::extract::{Extension, Json, State};
use shared_model::{AnalyzedError, AttributionContext, coarse_error_type};

use crate::APP_VERSION;
use crate::auth::Tenant;
use crate::routes::ApiError;
use crate::routes::detect::{PipelineResult, PipelineSpec, run_pipeline};
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
    /// 忽略历史复用，强制实检并回写历史（语义同 [`super::detect::DetectRequest::refresh`]）
    #[serde(default)]
    pub refresh: bool,
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
    /// 结果质量自描述（full/partial/degraded + 覆盖率 + 降级原因）；
    /// 历史复用回放时无（回放结果来自既往 full 运行）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<gec_core::QualityReport>,
    /// 切块账本（模式 / 块数 / 完成与失败数 / 覆盖率）；回放时无
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunks: Option<gec_core::ChunkStats>,
    /// 句级账本（总句数 / 送审句数）；回放时无
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sentences: Option<gec_core::SentenceStats>,
    /// 检测时版本向量（规则指纹 / 决策模型 / 切块器）；仅实际检测时返回，
    /// 历史复用回放时无
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<serde_json::Value>,
}

/// POST /gec/analyze — 检测 + 归因一站式接口（自动持久化到历史）
pub async fn analyze(
    State(state): State<Arc<AppState>>,
    Extension(tenant): Extension<Tenant>,
    headers: axum::http::HeaderMap,
    Json(req): Json<AnalyzeRequest>,
) -> Result<Json<AnalyzeResponse>, ApiError> {
    let level = req.level.unwrap_or(1);
    if !(1..=3).contains(&level) {
        return Err(ApiError::BadRequest(format!(
            "无效响应级别: {level}（应为 1-3）"
        )));
    }
    // 网关形态：按字符数计配额（超限 429），语义与 detect 入口一致
    tenant.charge_chars(req.text.chars().count())?;

    // 执行统一由 run_pipeline 驱动（入口收敛：LRU / 历史复用 / 规则开关与
    // 白名单过滤 / category / issue_type 填充 / anomaly 采纳与 L3 后台任务
    // 全部与 detect 入口同源；persist_history=false——本路由自写带归因结果的历史）
    let PipelineResult {
        outcome,
        from_history,
        version,
        params,
        summary_id,
    } = run_pipeline(
        state.clone(),
        PipelineSpec {
            entry: "analyze",
            lang: &req.lang,
            text: &req.text,
            category: &None,
            level: Some(level),
            force_llm: req.force_llm,
            refresh: req.refresh,
            progress_id: super::detect_progress::id_from_headers(&headers),
            history_tag: None,
            persist_history: false,
        },
    )
    .await?;
    let engine = state.tables.get(&req.lang);

    // 会话 ID：请求提供则用，否则自动生成
    let session_id = req
        .session_id
        .clone()
        .unwrap_or_else(|| format!("s_{}", uuid::Uuid::new_v4().simple()));
    // 网关形态：会话归属绑定（首次即绑；他人会话 403），历史行按它做跨账号过滤
    tenant.guard_session(&session_id)?;

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
                    // zh 细粒度类型在决策表按粗族回退（§11.11 兼容，表不改）；
                    // en/ja 原样（ja 表有 word_order 命中行，不可折叠）
                    error_type: if req.lang == "zh" {
                        coarse_error_type(&error.error_type).to_string()
                    } else {
                        error.error_type.clone()
                    },
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

    // §7 质量告警数据源：归因落进 grammar_novice（规则缺失兜底）的条数回填摘要行
    if let Some(id) = summary_id {
        let fallback = analyzed
            .iter()
            .filter(|a| {
                a.attribution
                    .as_ref()
                    .is_some_and(|at| at.trace.matched_rule == "grammar_novice")
            })
            .count();
        if let Err(e) = state.request_summary.set_fallback(id, fallback) {
            tracing::warn!(error = %e, "摘要行兜底归因回填失败（不影响返回结果）");
        }
    }

    // 自动持久化（含归因结果 + 各阶段计时 + 参数快照 + 文本哈希；后台写入，不阻塞检测响应）
    // 历史复用不写新记录（避免短时间反复检测刷出重复历史）；缓存资格：仅
    // quality=full 可写历史（partial/degraded 只回传不落库；参数快照 / 版本向量
    // 直接复用 run_pipeline 执行时产出，与 LRU / 历史复用键同一口径；
    // LRU 命中回放 quality=None → 照常落库，出自既往 full 运行）
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
        let params_json = params.as_ref().and_then(|p| serde_json::to_string(p).ok());
        let text_hash = shared_model::text_hash(&text);
        let corrected_text = shared_model::apply_corrections(
            &text,
            &analyzed.iter().map(|a| a.error.clone()).collect::<Vec<_>>(),
        );
        let layers_json = (!outcome.layers.is_empty())
            .then(|| serde_json::to_string(&outcome.layers).unwrap_or_else(|_| "[]".to_string()));
        if outcome.quality.as_ref().map_or(true, |q| q.cacheable()) {
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
        quality: outcome.quality,
        chunks: outcome.chunks,
        sentences: outcome.sentences,
        version,
    }))
}
