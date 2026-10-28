use std::sync::Arc;

use axum::extract::{Json, Path, Query, State};
use gec_core::{DetectOutcome, DetectorError};
use serde::Deserialize;
use shared_model::{Error, derive_issue_type, error_types, sources};

use crate::APP_VERSION;
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
}

#[derive(Deserialize)]
pub struct DetectQuery {
    /// 可选：仅返回指定分类的错误（如 "grammar" / "spelling" / "typography"）
    #[serde(default)]
    pub category: Option<String>,
}

/// 过滤已禁用规则 + 自定义词白名单（**全部检测入口共用**：
/// `run_detection`（detect / detect_lang）与 analyze 路由——
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
    category: &Option<String>,
    level: u8,
) -> Option<DetectOutcome> {
    let hash = shared_model::text_hash(text);
    let candidates = state.detect_history.latest_for_text(lang, &hash, 10).ok()?;
    if candidates.is_empty() {
        return None;
    }
    let current = state
        .current_detect_params(lang, level, false, category)
        .await;
    let idx = candidates.iter().position(|c| {
        shared_model::history_reusable(
            c.params_json.as_deref().unwrap_or(""),
            &c.text,
            &c.errors_json,
            &current,
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
) -> Result<(DetectOutcome, bool), ApiError> {
    let level = level.unwrap_or(1);
    if !(1..=3).contains(&level) {
        return Err(ApiError::BadRequest(format!(
            "无效响应级别: {level}（应为 1-3）"
        )));
    }
    let key = cache_key(lang, text);

    // 1. 基础结果（1 级：规则 + 决策模型，若启用）；
    //    level 2/3 或 force_llm 涉及 LLM 兜底（非确定性、不缓存）→ 实时跑完整级联
    let (mut outcome, from_history) = match (level, force_llm, state.as_ref().cache.get(&key)) {
        (1, false, Some(cached)) => (
            DetectOutcome {
                errors: cached,
                ..Default::default()
            },
            false,
        ),
        (1, false, None) => {
            // 历史复用：LRU 未命中 → 查同文本历史（参数一致 + 无 LLM 的最近记录）
            if let Some(reused) =
                try_reuse_history(state.as_ref(), lang, text, category, level).await
            {
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
                let outcome = engine.detect_level(text, 1, false, None).await?;
                state.as_ref().cache.insert(key, outcome.errors.clone());
                (outcome, false)
            }
        }
        (_, _, _) => {
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
                    .detect_level(text, level, force_llm, llm.as_deref())
                    .await?,
                false,
            )
        }
    };

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
    // 简易切句（。！？；.!? + 换行），避免引入 regex 依赖
    let mut sentences: Vec<String> = Vec::new();
    let mut cur = String::new();
    for c in text.chars() {
        cur.push(c);
        if matches!(c, '。' | '！' | '？' | '；' | '.' | '!' | '?' | '\n') {
            let s = cur.trim().to_string();
            if s.chars().count() >= 4 {
                sentences.push(s);
            }
            cur.clear();
        }
    }
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

/// POST /gec/detect — 检测文本中的语法错误（自动持久化到历史）
pub async fn detect(
    State(state): State<Arc<AppState>>,
    Query(q): Query<DetectQuery>,
    Json(req): Json<DetectRequest>,
) -> Result<Json<DetectResponse>, ApiError> {
    let (outcome, from_history) = run_detection(
        state.clone(),
        &req.lang,
        &req.text,
        &q.category,
        req.level,
        req.force_llm,
    )
    .await?;

    // 自动持久化（后台：历史写入不阻塞检测响应；含各阶段计时 + 参数快照 + 文本哈希）
    // 历史复用不写新记录（避免短时间反复检测刷出重复历史）
    if !from_history {
        let state = state.clone();
        let lang = req.lang.clone();
        let text = req.text.clone();
        let errors_json =
            serde_json::to_string(&outcome.errors).unwrap_or_else(|_| "[]".to_string());
        let timings_json = (!outcome.timings.is_empty())
            .then(|| serde_json::to_string(&outcome.timings).unwrap_or_else(|_| "{}".to_string()));
        let params = state
            .current_detect_params(&lang, req.level.unwrap_or(1), req.force_llm, &q.category)
            .await;
        let params_json = serde_json::to_string(&params).ok();
        let text_hash = shared_model::text_hash(&text);
        let corrected_text = shared_model::apply_corrections(&text, &outcome.errors);
        let layers_json = (!outcome.layers.is_empty())
            .then(|| serde_json::to_string(&outcome.layers).unwrap_or_else(|_| "[]".to_string()));
        tokio::spawn(async move {
            if let Err(e) = state.detect_history.insert(
                None,
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

    Ok(Json(DetectResponse {
        errors: outcome.errors,
        decision_abstained: outcome.decision_abstained,
        timings: outcome.timings,
        from_history,
        llm_confirmed_clean: outcome.llm_confirmed_clean,
        layers: outcome.layers,
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
}

/// POST /gec/:lang/detect — 语言变体检测（lang 来自路径；自动持久化）
pub async fn detect_lang(
    State(state): State<Arc<AppState>>,
    Path(lang): Path<String>,
    Query(q): Query<DetectQuery>,
    Json(req): Json<DetectLangRequest>,
) -> Result<Json<DetectResponse>, ApiError> {
    let (outcome, from_history) = run_detection(
        state.clone(),
        &lang,
        &req.text,
        &q.category,
        req.level,
        req.force_llm,
    )
    .await?;

    // 自动持久化（后台：历史写入不阻塞检测响应；含各阶段计时 + 参数快照 + 文本哈希）
    // 历史复用不写新记录（避免短时间反复检测刷出重复历史）
    if !from_history {
        let state = state.clone();
        let lang = lang.clone();
        let text = req.text.clone();
        let errors_json =
            serde_json::to_string(&outcome.errors).unwrap_or_else(|_| "[]".to_string());
        let timings_json = (!outcome.timings.is_empty())
            .then(|| serde_json::to_string(&outcome.timings).unwrap_or_else(|_| "{}".to_string()));
        let params = state
            .current_detect_params(&lang, req.level.unwrap_or(1), req.force_llm, &q.category)
            .await;
        let params_json = serde_json::to_string(&params).ok();
        let text_hash = shared_model::text_hash(&text);
        let corrected_text = shared_model::apply_corrections(&text, &outcome.errors);
        let layers_json = (!outcome.layers.is_empty())
            .then(|| serde_json::to_string(&outcome.layers).unwrap_or_else(|_| "[]".to_string()));
        tokio::spawn(async move {
            if let Err(e) = state.detect_history.insert(
                None,
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

    Ok(Json(DetectResponse {
        errors: outcome.errors,
        decision_abstained: outcome.decision_abstained,
        timings: outcome.timings,
        from_history,
        llm_confirmed_clean: outcome.llm_confirmed_clean,
        layers: outcome.layers,
    }))
}
