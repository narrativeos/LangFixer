use std::sync::Arc;

use axum::extract::{Extension, Json, Path, Query, State};
use gec_core::{DetectOutcome, DetectorError};
use serde::Deserialize;
use shared_model::{Error, derive_issue_type, error_types, sources};

use crate::APP_VERSION;
use crate::auth::Tenant;
use crate::cache::cache_key;
use crate::routes::ApiError;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct DetectRequest {
    pub lang: String,
    pub text: String,
    /// 三级响应级别：1 = 规则（+ 决策模型，若启用）；
    /// 2 = 1 级 + 决策模型明确弃权时 LLM 兜底；
    /// 3 = 1 级 + 无条件强制全文 LLM（「漏报」兜底）。
    /// 缺省 1；level 2/3 触发 LLM 但未配置 LLM 时返回 503
    #[serde(default)]
    pub level: Option<u8>,
    /// 「漏报」强制兜底：无视 level 触发条件，规则 + 决策模型完成后
    /// 无条件对全文跑一次 LLM 兜底（未配置 LLM 时返回 503）
    #[serde(default)]
    pub force_llm: bool,
    /// 忽略缓存绕过：跳过进程内 LRU 与历史复用，强制实检并用新结果回写
    /// 缓存 / 历史（规则 / 模型变更后怀疑命中旧结果时的显式纠正措施）
    #[serde(default)]
    pub refresh: bool,
}

/// 检测响应（错误列表 + 决策模型弃权信号 + 各阶段计时 + 历史复用标记）
#[derive(serde::Serialize)]
pub struct DetectResponse {
    pub errors: Vec<Error>,
    /// 决策模型是否弃权（仅决策层启用时有意义；`false` = 未启用或明确判断）
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub decision_abstained: bool,
    /// 各阶段计时（规则 / NLP（含预热）/ 决策 / L3 / 召回 / LLM / 总耗时）；
    /// 缓存命中等未实际检测时不返回
    #[serde(skip_serializing_if = "gec_core::DetectTimings::is_empty")]
    pub timings: gec_core::DetectTimings,
    /// 本次结果来自历史复用（同文本 + 参数一致 + 无 LLM 的近期记录；
    /// 前端可展示「来自历史」提示）
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub from_history: bool,
    /// LLM 兜底层是否实际执行且未检出错误（「LLM 复查确认干净」）；
    /// 前端据此区分「仅规则层未查出」与「LLM 复查确认无错误」两种「没有错误」
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub llm_confirmed_clean: bool,
    /// 层级贡献摘要：本次检测实际参与的每层状态与检出数（前端「检测路径」）；
    /// 缓存命中 / 历史复用等未实际检测路径不返回（历史复用时回放记录中的 layers）
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<gec_core::LayerReport>,
    /// 结果质量自描述（full/partial/degraded + 覆盖率 + 降级原因）；
    /// 缓存命中 / 历史复用回放时无（回放结果来自既往 full 运行）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality: Option<gec_core::QualityReport>,
    /// 切块账本（模式 / 块数 / 完成与失败数 / 覆盖率）；回放时无
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunks: Option<gec_core::ChunkStats>,
    /// 句级账本（总句数 / 送审句数）；回放时无
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sentences: Option<gec_core::SentenceStats>,
    /// 检测时版本向量（规则指纹 / 决策模型 / 切块器）；仅实际检测时返回，
    /// 缓存命中 / 历史复用回放时无
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<serde_json::Value>,
}

#[derive(Deserialize)]
pub struct DetectQuery {
    /// 可选：仅返回指定分类的错误（如 "grammar" / "spelling" / "typography"）
    #[serde(default)]
    pub category: Option<String>,
}

/// 过滤已禁用规则 + 自定义词白名单（**全部检测入口共用**：经由
/// `run_detection`（detect / detect_lang / analyze 唯一执行路径）生效——
/// 使「放弃」反馈（白名单止血）与「停用规则」建议在重检测时一致生效）
pub(crate) fn apply_suppression_filters(state: &AppState, lang: &str, outcome: &mut DetectOutcome) {
    // 过滤已禁用的规则（开关状态存于 SQLite 词表存储；
    // 未入库的规则 id 视为启用，与旧行为一致；
    // 批量查询：一次读规则表 + 内存匹配，而非逐条点查（N+1））
    let rule_ids: Vec<&str> = outcome
        .errors
        .iter()
        .filter_map(|e| e.rule_id.as_deref())
        .collect();
    let disabled: std::collections::HashSet<String> = state
        .lexicon_store
        .get_rules(lang, &rule_ids)
        .ok()
        .map(|defs| {
            defs.into_iter()
                .filter_map(|d| d.filter(|d| !d.enabled).map(|d| d.id))
                .collect()
        })
        .unwrap_or_default();
    outcome
        .errors
        .retain(|e| !e.rule_id.as_deref().is_some_and(|id| disabled.contains(id)));

    // 过滤自定义词白名单
    outcome
        .errors
        .retain(|e| !state.custom_words.is_whitelisted(lang, &e.original));
}

/// 历史复用（重复检测）：LRU 未命中时查同文本历史，最近一条**参数一致且不含 LLM**
/// 的记录直接反序列化其结果（跳过引擎；调用方不写新历史）。
///
/// 门槛（[`shared_model::history_reusable`]）：
/// - 记录携带可解析的 `params_json`（旧记录不可复用）；
/// - 存储文本与当前文本一致（`text_hash` 命中后的精确校验，防哈希碰撞）；
/// - 参数逐项一致（level / force_llm / category / 决策层 / L3 / LLM 配置 /
///   **决策模型版本号** / **规则侧版本指纹**——规则侧或模型侧任何变化都会使历史失效）；
/// - 记录为 level 1 非强制运行，且结果中无 `source == "llm"` 的错误。
pub(crate) async fn try_reuse_history(
    state: &AppState,
    lang: &str,
    text: &str,
    params: &shared_model::DetectParams,
) -> Option<DetectOutcome> {
    let hash = shared_model::text_hash(text);
    let candidates = state.detect_history.latest_for_text(lang, &hash, 10).ok()?;
    if candidates.is_empty() {
        return None;
    }
    let idx = candidates.iter().position(|c| {
        shared_model::history_reusable(
            c.params_json.as_deref().unwrap_or(""),
            &c.text,
            &c.errors_json,
            params,
            text,
        )
    })?;
    let h = &candidates[idx];
    let errors: Vec<Error> = serde_json::from_str(&h.errors_json).ok()?;
    tracing::debug!(
        history_id = h.id,
        errors = errors.len(),
        "重复检测命中历史，复用结果（跳过引擎）"
    );
    let layers = h
        .layers_json
        .as_deref()
        .and_then(|j| serde_json::from_str::<Vec<gec_core::LayerReport>>(j).ok())
        .unwrap_or_default();
    Some(DetectOutcome {
        errors,
        layers,
        ..Default::default()
    })
}

/// 执行检测 + 缓存 + 历史复用 + 过滤（已禁用规则 / 自定义词白名单 / category）+ 三级响应级别
///
/// 缓存两层（LRU / 历史）的键都携带完整参数指纹（规则侧 + 模型侧 + 层开关）：
/// 规则或模型变更后旧条目必然 miss，不会命中错误缓存；`refresh = true` 进一步
/// 跳过两层读取强制实检（显式纠正措施）。
///
/// 返回 `(结果, from_history)`：`from_history = true` 表示结果来自历史复用
/// （调用方据此跳过历史写入，并向前端返回 `from_history` 标记）。
/// 后处理（anomaly 自动采纳 / L3 飞轮）全部在**后台任务**执行，
/// 不阻塞检测响应（见各步骤注释）。
pub(crate) async fn run_detection(
    state: Arc<AppState>,
    lang: &str,
    text: &str,
    category: &Option<String>,
    level: Option<u8>,
    force_llm: bool,
    refresh: bool,
    progress_id: Option<&str>,
) -> Result<(DetectOutcome, bool), ApiError> {
    let level = level.unwrap_or(1);
    if !(1..=3).contains(&level) {
        return Err(ApiError::BadRequest(format!(
            "无效响应级别: {level}（应为 1-3）"
        )));
    }
    // 分阶段进度（前端进度条）：客户端经 X-Progress-Id 传入时登记事件条目，
    // 引擎侧由 sink 在阶段结束点推送；缓存 / 历史命中无阶段事件，直接标完成
    let sink = progress_id.map(|id| {
        super::detect_progress::begin(&state.detect_progress, id);
        super::detect_progress::sink(state.clone(), id)
    });
    let finish_progress = |id: Option<&str>| {
        if let Some(id) = id {
            super::detect_progress::finish(&state.detect_progress, id, text);
        }
    };
    // 网关形态：跳过进程级缓存与历史复用，每账号实时跑并自写历史（归属完整）
    let gw = crate::auth::gateway_mode();
    // LRU 键携带完整参数指纹（与历史复用门槛同一口径）：规则侧（词表 / 决策表 /
    // 禁用规则 / 自定义词）、模型侧（决策模型重训练）与层开关的任一变化都使旧键
    // 失效——修复旧版「键只含文本、变更后仍命中错误缓存」的缺口。
    // refresh = 显式绕过两层缓存读取，强制实检并回写。
    let params_opt = if level == 1 && !force_llm && !gw {
        Some(
            state
                .as_ref()
                .current_detect_params(lang, 1, false, category)
                .await,
        )
    } else {
        None
    };
    let cache_k = params_opt.as_ref().map(|p| cache_key(lang, text, p));
    let cached = match (&cache_k, gw || refresh) {
        (Some(k), false) => state.as_ref().cache.get(k),
        _ => None,
    };

    // 1. 基础结果（1 级：规则 + 决策模型，若启用）；
    //    level 2/3 或 force_llm 涉及 LLM 兜底（非确定性、不缓存）→ 实时跑完整级联
    let (mut outcome, from_history) = if let Some(errors) = cached {
        finish_progress(progress_id);
        (
            DetectOutcome {
                errors,
                ..Default::default()
            },
            false,
        )
    } else if level == 1 && !force_llm {
        // 历史复用：LRU 未命中 → 查同文本历史（参数一致 + 无 LLM 的最近记录；
        // refresh / 网关形态跳过）
        let reused = match &params_opt {
            Some(p) if !gw && !refresh => try_reuse_history(state.as_ref(), lang, text, p).await,
            _ => None,
        };
        if let Some(reused) = reused {
            finish_progress(progress_id);
            (reused, true)
        } else {
            // 读锁内仅克隆引擎 Arc（锁立即释放），避免 guard 跨 await（非 Send）
            let engine = state
                .as_ref()
                .router
                .read()
                .unwrap()
                .get(lang)
                .cloned()
                .ok_or_else(|| DetectorError::UnsupportedLanguage(lang.to_string()))?;
            // 统一走 detect_level：1 级结果 + 各阶段计时（含 NLP 预热）
            let outcome = engine
                .detect_level_traced(text, 1, false, None, sink)
                .await?;
            // 缓存资格：仅 quality=full 的运行可写 LRU（partial/degraded 只回传
            // 不入缓存，杜绝降级结果被缓存放大）
            if let Some(k) = cache_k {
                if outcome.quality.as_ref().map_or(true, |q| q.cacheable()) {
                    state.as_ref().cache.insert(k, outcome.errors.clone());
                }
            }
            (outcome, false)
        }
    } else {
        let engine = state
            .as_ref()
            .router
            .read()
            .unwrap()
            .get(lang)
            .cloned()
            .ok_or_else(|| DetectorError::UnsupportedLanguage(lang.to_string()))?;
        let llm = state.as_ref().llm_detect_engine(lang);
        (
            engine
                .detect_level_traced(text, level, force_llm, llm.as_deref(), sink)
                .await?,
            false,
        )
    };

    // 无事件引擎（纯规则默认配置等）的进度兜底：支持事件的引擎已自行 End / 缓存历史已 finish，均 no-op
    if let Some(id) = progress_id {
        super::detect_progress::fallback(&state.detect_progress, id, text, &outcome.timings);
    }

    // 2. + 3. 过滤已禁用的规则 + 自定义词白名单（与 analyze 路由共用同一过滤，
    //    保证「放弃」反馈 / 「停用规则」建议在所有检测入口一致生效）
    apply_suppression_filters(state.as_ref(), lang, &mut outcome);

    // 4. 按 category 过滤
    if let Some(cat) = category {
        outcome
            .errors
            .retain(|e| e.category.as_deref() == Some(cat.as_str()));
    }

    // 5. 填充 W3C ITS 2.0 issue_type
    for e in &mut outcome.errors {
        if e.issue_type.is_none() {
            e.issue_type = Some(derive_issue_type(&e.error_type).to_string());
        }
    }

    // 6. anomaly 自动采纳（二期飞轮 L1）：LLM 高置信命中自动沉淀到向量知识库
    //    （同一错误下次不再付 LLM 成本）；
    //    转后台任务——逐候选 embed 调用 + 知识库读写不阻塞检测响应
    {
        let state = state.clone();
        let lang = lang.to_string();
        let errors = outcome.errors.clone();
        tokio::spawn(async move {
            auto_adopt_anomalies(state.as_ref(), &lang, &errors).await;
        });
    }

    // 7. L3 飞轮（三期闭环）：仅当 **LLM 实际执行过且复查确认干净**
    //    （`llm_confirmed_clean`：level 2 弃权触发 / level 3 / force_llm 触发了 LLM，
    //    且 LLM 未检出错误）且最终无错误时，采集原句作为负样本入特征模型训练集。
    //    注意：level 1（无 LLM 复查）「未检出」只代表规则 / 决策模型未命中，
    //    可能是漏报——此时采集会把 LLM 盲区固化为特征模型的「干净」负样本，故不采集。
    //    文件 I/O 转后台任务，不阻塞检测响应。
    //    （历史复用路径：errors 来自 level 1 记录、`llm_confirmed_clean = false`，此处自然不采集）
    if outcome.llm_confirmed_clean && outcome.errors.is_empty() {
        let state = state.clone();
        let lang = lang.to_string();
        let text = text.to_string();
        tokio::spawn(async move {
            collect_clean_sentences(state.as_ref(), &lang, &text).await;
        });
    }

    Ok((outcome, from_history))
}

/// 统一管线入参（[`run_pipeline`]）：执行参数 + 历史落库策略
pub(crate) struct PipelineSpec<'a> {
    /// 入口标识（§7 摘要行归属："detect" | "analyze"）
    pub entry: &'static str,
    pub lang: &'a str,
    pub text: &'a str,
    pub category: &'a Option<String>,
    pub level: Option<u8>,
    pub force_llm: bool,
    pub refresh: bool,
    pub progress_id: Option<&'a str>,
    /// 历史归属标记（detect = 网关伪会话 tag；None = 无会话归属）
    pub history_tag: Option<&'a str>,
    /// 是否由本函数回写纯检测结果历史（false = 调用方自带 persist，
    /// 如 analyze 需写入含归因结果的 `analyzed_json`）
    pub persist_history: bool,
}

/// 统一管线产出：检测结果 + 复用标记 + 版本向量 + 参数快照
pub(crate) struct PipelineResult {
    pub outcome: DetectOutcome,
    pub from_history: bool,
    /// 检测时版本向量（仅实际检测且 quality 非空时）
    pub version: Option<serde_json::Value>,
    /// 本次执行的参数快照（仅实际检测时）
    pub params: Option<shared_model::DetectParams>,
    /// 摘要行 id（§7；analyze 归因后回填 fallback_count 用；写失败为 None）
    pub summary_id: Option<i64>,
}

/// 唯一执行入口（Detect Pipeline v2 P4 入口收敛）：detect / detect_lang / analyze
/// 全部经本函数驱动，消除路由层的第二份执行实现。
///
/// = `run_detection`（引擎 / 缓存 / 历史复用 / 过滤 / 三级响应 / 后台任务）
/// + 参数快照 / 版本向量 / 历史回写：
/// - `from_history = true`（历史复用回放）：不产快照、不写历史；
/// - 实际检测即产快照（LRU 命中回放 quality=None → version 为 None，但历史
///   按 `map_or(true)` 照常落库——回放结果出自既往 full 运行）；
/// - `persist_history = false`（analyze）：只产快照供调用方组历史行。
pub(crate) async fn run_pipeline(
    state: Arc<AppState>,
    spec: PipelineSpec<'_>,
) -> Result<PipelineResult, ApiError> {
    let (outcome, from_history) = match run_detection(
        state.clone(),
        spec.lang,
        spec.text,
        spec.category,
        spec.level,
        spec.force_llm,
        spec.refresh,
        spec.progress_id,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => {
            // §7 失败也要留痕：错误同样写摘要行，否则仪表盘/告警对「检测失败」
            // 完全失明。客户端错（参数/语言不支持）计 rejected，引擎/上游错计
            // failed（后者才进失败告警，用户拼错 lang 不得污染告警）。
            let status = if matches!(e, ApiError::BadRequest(_) | ApiError::NotFound(_)) {
                "rejected"
            } else {
                "failed"
            };
            let row = crate::request_summary::SummaryRow {
                entry: spec.entry,
                lang: spec.lang,
                level: spec.level.unwrap_or(1),
                quality_status: Some(status),
                coverage: None,
                unowned_chars: 0,
                dropped: Default::default(),
                degraded_json: Some(serde_json::json!([format!("engine_error: {e}")]).to_string()),
                chunks: None,
                sentences: None,
                version_json: None,
                error_count: 0,
                from_history: false,
                from_cache: false,
                total_ms: None,
            };
            if let Err(se) = state.request_summary.insert(&row) {
                tracing::warn!(error = %se, "失败请求摘要写入失败（不影响返回结果）");
            }
            return Err(e);
        }
    };

    let mut version: Option<serde_json::Value> = None;
    let mut params: Option<shared_model::DetectParams> = None;
    // 自动持久化（后台：历史写入不阻塞检测响应；含各阶段计时 + 参数快照 + 文本哈希）
    // 历史复用不写新记录（避免短时间反复检测刷出重复历史）；缓存资格：仅
    // quality=full 可写历史（partial/degraded 只回传不落库，杜绝降级结果被复用为
    // 正常结果；LRU 命中回放结果出自 full 运行，照常落库）
    if !from_history {
        let snapshot = state
            .current_detect_params(
                spec.lang,
                spec.level.unwrap_or(1),
                spec.force_llm,
                spec.category,
            )
            .await;
        // 版本向量：实际检测时的参数快照（LRU 命中回放 quality=None → 不返回）
        version = outcome.quality.as_ref().map(|_| {
            serde_json::json!({
                "rules_fingerprint": snapshot.rules_fingerprint,
                "model_version": snapshot.model_version,
                "splitter": snapshot.splitter,
            })
        });
        if spec.persist_history && outcome.quality.as_ref().map_or(true, |q| q.cacheable()) {
            let state = state.clone();
            let lang = spec.lang.to_string();
            let text = spec.text.to_string();
            let history_tag = spec.history_tag.map(|s| s.to_string());
            let errors_json =
                serde_json::to_string(&outcome.errors).unwrap_or_else(|_| "[]".to_string());
            let timings_json = (!outcome.timings.is_empty()).then(|| {
                serde_json::to_string(&outcome.timings).unwrap_or_else(|_| "{}".to_string())
            });
            let params_json = serde_json::to_string(&snapshot).ok();
            let text_hash = shared_model::text_hash(&text);
            let corrected_text = shared_model::apply_corrections(&text, &outcome.errors);
            let layers_json = (!outcome.layers.is_empty()).then(|| {
                serde_json::to_string(&outcome.layers).unwrap_or_else(|_| "[]".to_string())
            });
            tokio::spawn(async move {
                if let Err(e) = state.detect_history.insert(
                    history_tag.as_deref(),
                    &lang,
                    &text,
                    &errors_json,
                    None,
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
        params = Some(snapshot);
    }

    // §7 摘要行：每次请求留痕，**不受 cacheable / from_history 门控**——
    // partial / degraded / 回放更要留痕（回放行 quality/status 为空）。
    // 同步写入：账本不许丢，单行 INSERT 微秒级。
    let degraded_json = outcome
        .quality
        .as_ref()
        .filter(|q| !q.degraded_reasons.is_empty())
        .and_then(|q| serde_json::to_string(&q.degraded_reasons).ok());
    let summary_row = crate::request_summary::SummaryRow {
        entry: spec.entry,
        lang: spec.lang,
        level: spec.level.unwrap_or(1),
        quality_status: outcome.quality.as_ref().map(|q| q.status.as_str()),
        coverage: outcome.quality.as_ref().and_then(|q| q.coverage),
        unowned_chars: outcome
            .quality
            .as_ref()
            .map(|q| q.unowned_chars)
            .unwrap_or(0),
        dropped: outcome
            .quality
            .as_ref()
            .map(|q| q.dropped)
            .unwrap_or_default(),
        degraded_json,
        chunks: outcome.chunks.as_ref(),
        sentences: outcome.sentences.as_ref(),
        version_json: version.as_ref().map(|v| v.to_string()),
        error_count: outcome.errors.len(),
        from_history,
        from_cache: !from_history && outcome.quality.is_none(),
        total_ms: (!outcome.timings.is_empty()).then(|| outcome.timings.total_ms),
    };
    // 告警①即时面：所有权空洞 / 丢弃账本非零 = 数据静默丢失，当场最高级日志
    if let Some(q) = outcome.quality.as_ref() {
        if q.unowned_chars != 0 || !q.dropped.is_empty() {
            tracing::error!(
                unowned_chars = q.unowned_chars,
                dropped_ownership = q.dropped.ownership,
                dropped_pii_mask = q.dropped.pii_mask,
                dropped_out_of_range = q.dropped.out_of_range,
                dropped_out_of_scope = q.dropped.out_of_scope,
                "§7 完整性告警：本次请求存在数据丢失（所有权空洞或检出丢弃）"
            );
        }
    }
    let summary_id = match state.request_summary.insert(&summary_row) {
        Ok(id) => Some(id),
        Err(e) => {
            tracing::warn!(error = %e, "请求摘要写入失败（不影响返回结果）");
            None
        }
    };

    Ok(PipelineResult {
        outcome,
        from_history,
        version,
        params,
        summary_id,
    })
}

/// L3 飞轮：干净句自动采集为负样本
///
/// 检测未产出错误（含 LLM 兜底复查后确认干净）时，把原文句子采集为负样本
/// 汇入特征模型训练集（`l3_data/<lang>/confirmed.jsonl`，`source=llm_clean`，
/// 与已有样本按 文本+标签 去重）。非阻断：失败仅告警，不影响检测结果返回。
const MAX_CLEAN_PER_DETECT: usize = 50;

async fn collect_clean_sentences(state: &AppState, lang: &str, text: &str) {
    if !matches!(lang, "en" | "zh") {
        return;
    }
    // 切句统一走 gec-core 切块器（单一 splitter 口径，不再维护第二套切句规则；
    // 纯标点/纯空白片段用 is_sentence 过滤，句表/召回同口径）
    let sentences: Vec<String> = gec_core::docchunk::sentence_spans(text)
        .into_iter()
        .filter(|s| gec_core::docchunk::is_sentence(text, s))
        .map(|s| text[s[0]..s[1]].trim().to_string())
        .filter(|s| s.chars().count() >= 4)
        .collect();
    let dir = crate::routes::l3::l3_data_dir();
    let mut count = 0usize;
    for sentence in sentences {
        let sample = serde_json::json!({
            "text": sentence,
            "label": 0,
            "error_type": "",
            "source": "llm_clean",
            "ts": chrono::Utc::now().to_rfc3339(),
        });
        if crate::routes::l3::collect_sample(&dir, lang, &sample).is_some() {
            count += 1;
        }
        if count >= MAX_CLEAN_PER_DETECT {
            break;
        }
    }
    if count > 0 {
        tracing::info!(lang, count, "L3 飞轮：干净句已汇入训练集");
    }
    let _ = state;
}

/// anomaly 自动采纳：LLM 兜底检出的污染句，置信度 ≥ 阈值时
/// 无需用户确认即沉淀到学习知识库（source `llm_adopt`，②.5 向量召回）。
///
/// - 仅针对 `error_type == anomaly` 且 `source == llm` 的错误（规则层检出
///   的一期污染不重复沉淀）；
/// - 同文本去重（知识库已有则跳过）；
/// - 嵌入引擎未配置时以空向量写入（不参与向量召回，仅留痕）；
/// - 全程非阻断：失败仅告警，不影响检测结果返回。
const ANOMALY_AUTO_ADOPT_CONF: f64 = 0.9;

async fn auto_adopt_anomalies(state: &AppState, lang: &str, errors: &[Error]) {
    let candidates: Vec<&Error> = errors
        .iter()
        .filter(|e| {
            e.error_type == error_types::ANOMALY
                && e.source.as_deref() == Some(sources::LLM)
                && e.confidence >= ANOMALY_AUTO_ADOPT_CONF
        })
        .collect();
    if candidates.is_empty() {
        return;
    }
    // 同文本去重（一次检测多句相同污染 / 重复检测）
    let existing = match state.learned_knowledge.list(lang) {
        Ok(items) => items,
        Err(e) => {
            tracing::warn!(error = %e, "anomaly 自动采纳：知识库读取失败，跳过");
            return;
        }
    };
    // guard 不可跨 await（非 Send）——先克隆 Arc 再立即释放
    let engine = {
        let guard = state.embedding_engine.read().unwrap();
        guard.as_ref().cloned()
    };
    for e in candidates {
        if existing.iter().any(|k| k.text == e.original) {
            continue;
        }
        let embedding = match engine.as_deref() {
            Some(eng) => eng.embed(&e.original).await.unwrap_or_default(),
            None => Vec::new(),
        };
        match state.learned_knowledge.insert(
            lang,
            &e.original,
            &e.error_type,
            "",
            &embedding,
            "llm_adopt",
        ) {
            Ok(id) if id > 0 => tracing::info!(
                lang,
                entry = id,
                text = %e.original,
                confidence = e.confidence,
                "anomaly 自动采纳：LLM 高置信命中已沉淀到向量知识库（llm_adopt）"
            ),
            Ok(_) => tracing::debug!(
                lang,
                text = %e.original,
                "anomaly 自动采纳：重复条目（唯一约束命中，并发重复写），跳过"
            ),
            Err(err) => tracing::warn!(
                error = %err,
                "anomaly 自动采纳：知识库写入失败（不影响检测结果）"
            ),
        }
    }
}

#[derive(Deserialize)]
pub struct ClearCacheRequest {
    /// 同时清空历史复用库（SQLite `detect_history`）；缺省 false（仅清进程内 LRU）
    #[serde(default)]
    pub history: bool,
}

/// POST /gec/cache/clear — 显式清缓存纠正措施：
/// 清空进程内 LRU 检测缓存（缺省）；`history=true` 时一并清空历史复用库。
/// 规则 / 模型变更本身已使旧缓存键失效（指纹入键），本端点用于「缓存无法纠正」
/// 时的手动兜底（例：词表文件被仓外手段直接修改、或需要强制全新检测）。
pub async fn clear_cache(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ClearCacheRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let lru_cleared = state.cache.clear();
    let history_cleared = if req.history {
        state
            .detect_history
            .clear()
            .map_err(|e| ApiError::Internal(format!("清空历史失败: {e}")))?
    } else {
        0
    };
    tracing::info!(
        lru_cleared,
        history_cleared,
        "显式清缓存（POST /gec/cache/clear）"
    );
    Ok(Json(serde_json::json!({
        "lru_cleared": lru_cleared,
        "history_cleared": history_cleared,
    })))
}

/// POST /gec/detect — 检测文本中的语法错误（自动持久化到历史）
pub async fn detect(
    State(state): State<Arc<AppState>>,
    Extension(tenant): Extension<Tenant>,
    Query(q): Query<DetectQuery>,
    headers: axum::http::HeaderMap,
    Json(req): Json<DetectRequest>,
) -> Result<Json<DetectResponse>, ApiError> {
    // 网关形态：先按字符数计配额（超限 429），历史按账号伪会话归户
    tenant.charge_chars(req.text.chars().count())?;
    let gw_tag = tenant.history_tag();
    if let Some(tag) = &gw_tag {
        tenant.guard_session(tag)?;
    }
    let PipelineResult {
        outcome,
        from_history,
        version,
        ..
    } = run_pipeline(
        state,
        PipelineSpec {
            entry: "detect",
            lang: &req.lang,
            text: &req.text,
            category: &q.category,
            level: req.level,
            force_llm: req.force_llm,
            refresh: req.refresh,
            progress_id: super::detect_progress::id_from_headers(&headers),
            history_tag: gw_tag.as_deref(),
            persist_history: true,
        },
    )
    .await?;

    Ok(Json(DetectResponse {
        errors: outcome.errors,
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

// ─────────────────────────────────────────────────────────────────────────────
// 语言变体路由：/gec/zh/detect, /gec/en/detect, /gec/ja/detect
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct DetectLangRequest {
    pub text: String,
    /// 三级响应级别（1/2/3，缺省 1；语义同 [`DetectRequest::level`]）
    #[serde(default)]
    pub level: Option<u8>,
    /// 「漏报」强制兜底（语义同 [`DetectRequest::force_llm`]）
    #[serde(default)]
    pub force_llm: bool,
    /// 忽略缓存绕过（语义同 [`DetectRequest::refresh`]）
    #[serde(default)]
    pub refresh: bool,
}

/// POST /gec/:lang/detect — 语言变体检测（lang 来自路径；自动持久化）
pub async fn detect_lang(
    State(state): State<Arc<AppState>>,
    Extension(tenant): Extension<Tenant>,
    Path(lang): Path<String>,
    Query(q): Query<DetectQuery>,
    headers: axum::http::HeaderMap,
    Json(req): Json<DetectLangRequest>,
) -> Result<Json<DetectResponse>, ApiError> {
    // 网关形态：先按字符数计配额（超限 429），历史按账号伪会话归户
    tenant.charge_chars(req.text.chars().count())?;
    let gw_tag = tenant.history_tag();
    if let Some(tag) = &gw_tag {
        tenant.guard_session(tag)?;
    }
    let PipelineResult {
        outcome,
        from_history,
        version,
        ..
    } = run_pipeline(
        state,
        PipelineSpec {
            entry: "detect",
            lang: &lang,
            text: &req.text,
            category: &q.category,
            level: req.level,
            force_llm: req.force_llm,
            refresh: req.refresh,
            progress_id: super::detect_progress::id_from_headers(&headers),
            history_tag: gw_tag.as_deref(),
            persist_history: true,
        },
    )
    .await?;

    Ok(Json(DetectResponse {
        errors: outcome.errors,
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
