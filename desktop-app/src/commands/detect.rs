use std::sync::Arc;

use gec_core::targeted_llm_review;

use shared_model::{
    AnalyzedError, AttributionContext, Error, coarse_error_type, derive_issue_type,
};

use super::anomaly::auto_adopt_anomalies_cmd;
use super::core::{APP_VERSION, Core};
use super::l3::l3_collect_sample;
/// 检测文本中的语法错误（支持可选 category 过滤 + 规则开关 + 自定义词白名单 + 三级响应级别 + 漏报强制兜底，与 Web 端一致）
/// 检测响应（Tauri 命令返回；与 API `DetectResponse` 同构）
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct DetectResult {
    pub errors: Vec<Error>,
    /// 决策模型是否弃权
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub decision_abstained: bool,
    /// 各阶段计时（规则 / NLP（含预热）/ 决策 / L3 / 召回 / LLM / 总耗时）
    #[serde(default)]
    pub timings: gec_core::DetectTimings,
    /// 本次结果来自历史复用（同文本 + 参数一致 + 无 LLM 的近期记录）
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub from_history: bool,
    /// LLM 兜底层是否实际执行且未检出错误（「LLM 复查确认干净」）
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub llm_confirmed_clean: bool,
    /// 层级贡献摘要：本次检测实际参与的每层状态与检出数（前端「检测路径」）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<gec_core::LayerReport>,
    /// 结果质量自描述（full/partial/degraded）；历史回放时为 None（出自既往 full 运行）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<gec_core::QualityReport>,
    /// 切块账本（模式/块数/覆盖率）；回放时无
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunks: Option<gec_core::ChunkStats>,
    /// 句级账本（总句数/送审句数）；回放时无
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sentences: Option<gec_core::SentenceStats>,
}

/// 检测 + 归因响应（Tauri 命令返回）
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct AnalyzeResult {
    pub lang: String,
    pub errors: Vec<AnalyzedError>,
    pub session_id: String,
    /// 决策模型是否弃权
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub decision_abstained: bool,
    /// 各阶段计时（规则 / NLP（含预热）/ 决策 / L3 / 召回 / LLM / 总耗时）
    #[serde(default)]
    pub timings: gec_core::DetectTimings,
    /// 本次结果来自历史复用（同文本 + 参数一致 + 无 LLM 的近期记录）
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub from_history: bool,
    /// LLM 兜底层是否实际执行且未检出错误（「LLM 复查确认干净」）
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub llm_confirmed_clean: bool,
    /// 层级贡献摘要：本次检测实际参与的每层状态与检出数（前端「检测路径」）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<gec_core::LayerReport>,
    /// 结果质量自描述（full/partial/degraded）；历史回放时为 None（出自既往 full 运行）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<gec_core::QualityReport>,
    /// 切块账本（模式/块数/覆盖率）；回放时无
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunks: Option<gec_core::ChunkStats>,
    /// 句级账本（总句数/送审句数）；回放时无
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sentences: Option<gec_core::SentenceStats>,
}

/// 历史复用（重复检测）：查同文本历史，最近一条**参数一致且不含 LLM**
/// 的记录直接反序列化其结果（跳过引擎；调用方不写新历史）。
///
/// 门槛（[`shared_model::history_reusable`]）：
/// - 记录携带可解析的 `params_json`（旧记录不可复用）；
/// - 存储文本与当前文本一致（`text_hash` 命中后的精确校验，防哈希碰撞）；
/// - 参数逐项一致（level / force_llm / category / 决策层 / L3 / LLM 配置 /
///   **决策模型版本号** / **规则侧版本指纹** / **切块器版本**——规则侧、模型侧
///   或切块器任何变化都会使历史失效）；
/// - 记录为 level 1 非强制运行，且结果中无 `source == "llm"` 的错误。
async fn try_reuse_history(
    core: &Core,
    lang: &str,
    text: &str,
    category: &Option<String>,
    level: u8,
) -> Option<gec_core::DetectOutcome> {
    let hash = shared_model::text_hash(text);
    let candidates = core.detect_history.latest_for_text(lang, &hash, 10).ok()?;
    if candidates.is_empty() {
        return None;
    }
    let current = core
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
    // 层级摘要回放（与 api-server try_reuse_history 同构：前端「检测路径」可回放）
    let layers = h
        .layers_json
        .as_deref()
        .and_then(|j| serde_json::from_str::<Vec<gec_core::LayerReport>>(j).ok())
        .unwrap_or_default();
    Some(gec_core::DetectOutcome {
        errors,
        layers,
        ..Default::default()
    })
}

/// 阶段事件 sink：把引擎阶段事件转成 Tauri 应用事件 `detect:stage`（前端分阶段进度条）；
/// 按 progress_id 过滤（前端仅消费本次检测的事件）。未传 progress_id 时不建 sink（零开销）。
fn stage_sink(app: &tauri::AppHandle, progress_id: Option<&str>) -> Option<gec_core::StageSink> {
    let pid = progress_id.filter(|s| !s.is_empty())?;
    let app = app.clone();
    let pid = pid.to_string();
    Some(Arc::new(move |ev: gec_core::StageEvent| {
        use tauri::Emitter;
        let _ = app.emit(
            "detect:stage",
            serde_json::json!({ "progress_id": pid, "event": ev }),
        );
    }))
}

/// 过滤已禁用规则 + 自定义词白名单（与 api-server `apply_suppression_filters` 同构：
/// 使「放弃」反馈（白名单止血）与「停用规则」建议在重检测时一致生效）
fn apply_suppression_filters(core: &Arc<Core>, lang: &str, outcome: &mut gec_core::DetectOutcome) {
    // 过滤已禁用的规则（开关状态存于 SQLite 词表存储；未入库的规则 id 视为启用；
    // 批量查询：一次读规则表 + 内存匹配，而非逐条点查（N+1））
    let rule_ids: Vec<&str> = outcome
        .errors
        .iter()
        .filter_map(|e| e.rule_id.as_deref())
        .collect();
    let disabled: std::collections::HashSet<String> = core
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
        .retain(|e| !core.custom_words.is_whitelisted(lang, &e.original));
}

/// L3 飞轮：干净句自动采集为负样本（与 api-server `collect_clean_sentences` 同构：
/// 原文句子汇入特征模型训练集 `l3_data/<lang>/confirmed.jsonl`，`source=llm_clean`，
/// 按 文本+标签 去重；非阻断，失败仅告警）
const MAX_CLEAN_PER_DETECT: usize = 50;

fn collect_clean_sentences(core: &Core, lang: &str, text: &str) {
    if !matches!(lang, "en" | "zh") {
        return;
    }
    // 切句统一走 gec-core 切块器（与 api-server / 切块同一 splitter 口径，不再两套规则）
    let sentences: Vec<String> = gec_core::docchunk::sentence_spans(text)
        .into_iter()
        .filter(|s| gec_core::docchunk::is_sentence(text, s))
        .map(|s| text[s[0]..s[1]].trim().to_string())
        .filter(|s| s.chars().count() >= 4)
        .collect();
    let mut count = 0usize;
    for sentence in sentences {
        let sample = serde_json::json!({
            "text": sentence,
            "label": 0,
            "error_type": "",
            "source": "llm_clean",
            "ts": chrono::Utc::now().to_rfc3339(),
        });
        if l3_collect_sample(&core.l3_data, lang, &sample).is_some() {
            count += 1;
        }
        if count >= MAX_CLEAN_PER_DETECT {
            break;
        }
    }
    if count > 0 {
        tracing::info!(lang, count, "L3 飞轮：干净句已汇入训练集");
    }
}

/// 统一管线入参（对齐 api-server `PipelineSpec`：桌面端无网关形态 / 进程级 LRU，
/// 其余口径同源）
struct PipelineSpecLite<'a> {
    lang: &'a str,
    text: &'a str,
    category: &'a Option<String>,
    level: u8,
    force_llm: bool,
    refresh: bool,
    sink: Option<gec_core::StageSink>,
}

/// 管线产出：检测结果 + 复用标记 + 参数快照（仅实际检测时产出，
/// 语义同 api-server `PipelineResult`）
struct PipelineOut {
    outcome: gec_core::DetectOutcome,
    from_history: bool,
    params: Option<shared_model::DetectParams>,
}

/// 桌面端唯一执行入口（与 api-server `run_pipeline` 执行段同构，命令层只透传参数）：
/// level 1 历史复用 → 引擎级联 → 规则开关 / 白名单 / category 过滤 → issue_type 填充
/// → anomaly 采纳与 L3 干净句后台任务；参数快照仅实际检测时计算。
async fn run_pipeline(core: &Arc<Core>, spec: PipelineSpecLite<'_>) -> Result<PipelineOut, String> {
    let PipelineSpecLite {
        lang,
        text,
        category,
        level,
        force_llm,
        refresh,
        sink,
    } = spec;
    // level 1 = 常规检测；level 2/3 或 force_llm = 完整级联（LLM 兜底按需/强制触发）
    // 统一走 detect_level：各阶段计时（含 NLP 预热）随结果返回并持久化
    let (mut outcome, from_history) = if level <= 1 && !force_llm {
        // 历史复用：同文本 + 参数一致 + 无 LLM 的最近记录直接复用（跳过引擎；refresh 跳过复用）
        let reused = if refresh {
            None
        } else {
            try_reuse_history(core, lang, text, category, 1).await
        };
        if let Some(reused) = reused {
            (reused, true)
        } else {
            // 读锁内仅克隆引擎 Arc（锁立即释放），避免 guard 跨 await（非 Send）
            let engine = core
                .router
                .read()
                .unwrap()
                .get(lang)
                .cloned()
                .ok_or_else(|| format!("语言 {lang} 无检测器"))?;
            (
                engine
                    .detect_level_traced(text, 1, false, None, sink)
                    .await
                    .map_err(|e| e.to_string())?,
                false,
            )
        }
    } else {
        let engine = core
            .router
            .read()
            .unwrap()
            .get(lang)
            .cloned()
            .ok_or_else(|| format!("语言 {lang} 无检测器"))?;
        let llm = core.llm_detect_engine(lang);
        (
            engine
                .detect_level_traced(text, level, force_llm, llm.as_deref(), sink)
                .await
                // 审计 F2 后引擎对 LLM 未配置不再报错（层报 skipped），无需变体分流
                .map_err(|e| e.to_string())?,
            false,
        )
    };
    // 过滤已禁用规则 + 自定义词白名单（与 analyze 入口共用同一过滤）
    apply_suppression_filters(core, lang, &mut outcome);
    // 按 category 过滤
    if let Some(cat) = category {
        outcome
            .errors
            .retain(|e| e.category.as_deref() == Some(cat.as_str()));
    }
    // 填充 W3C ITS 2.0 issue_type
    for e in &mut outcome.errors {
        if e.issue_type.is_none() {
            e.issue_type = Some(derive_issue_type(&e.error_type).to_string());
        }
    }
    // anomaly 自动采纳（二期飞轮 L1）：LLM 高置信命中自动沉淀到向量知识库；
    // 转后台任务（逐候选 embed 调用 + 知识库读写不阻塞检测响应）
    {
        let core = core.clone();
        let lang = lang.to_string();
        let errors = outcome.errors.clone();
        tokio::spawn(async move {
            auto_adopt_anomalies_cmd(&core, &lang, &errors).await;
        });
    }
    // L3 飞轮（三期闭环）：仅当 LLM 实际复查确认干净且最终无错误时采集原句作负样本
    //（level 1「未检出」可能是漏报，不采集——与 api-server 同口径）；文件 I/O 转后台
    if outcome.llm_confirmed_clean && outcome.errors.is_empty() {
        let core = core.clone();
        let lang = lang.to_string();
        let text = text.to_string();
        tokio::spawn(async move {
            collect_clean_sentences(&core, &lang, &text);
        });
    }
    // 参数快照：实际检测即算（历史复用回放不写新记录，无需快照）
    let params = if !from_history {
        Some(
            core.current_detect_params(lang, level, force_llm, category)
                .await,
        )
    } else {
        None
    };
    Ok(PipelineOut {
        outcome,
        from_history,
        params,
    })
}

#[tauri::command]
pub async fn detect(
    app: tauri::AppHandle,
    core: tauri::State<'_, Arc<Core>>,
    lang: String,
    text: String,
    category: Option<String>,
    // 三级响应级别：1 = 规则（+ 决策模型，若启用）；2 = + 决策模型明确弃权时
    // LLM 兜底；3 = + 强制 LLM（缺省 1）
    level: Option<u8>,
    // 「漏报」强制兜底：无视 level 触发条件，无条件对全文跑一次 LLM 兜底
    force_llm: bool,
    // 忽略缓存绕过：跳过历史复用强制实检并回写历史（语义同 Web 端 refresh）
    refresh: Option<bool>,
    // 分阶段进度条事件标识（前端生成；随 invoke 参数传入，事件经 detect:stage 推回）
    progress_id: Option<String>,
) -> Result<DetectResult, String> {
    let level = level.unwrap_or(1);
    if !(1..=3).contains(&level) {
        return Err(format!("无效响应级别: {level}（应为 1-3）"));
    }
    let sink = stage_sink(&app, progress_id.as_deref());
    let PipelineOut {
        outcome,
        from_history,
        params,
    } = run_pipeline(
        &core,
        PipelineSpecLite {
            lang: &lang,
            text: &text,
            category: &category,
            level,
            force_llm,
            refresh: refresh.unwrap_or(false),
            sink,
        },
    )
    .await?;
    // 自动持久化（与 api-server run_pipeline persist 块对齐：session_id=None、无归因结果；
    // 含各阶段计时 + 参数快照 + 文本哈希 + 纠正版 + 层级摘要；后台写入，不阻塞检测响应）
    // 历史复用不写新记录（避免短时间反复检测刷出重复历史）；缓存资格：
    // 仅 quality=full 落库（partial/degraded 只回传不落库，杜绝降级结果被复用，
    // 语义与 Web 端 detect 路由一致）
    if !from_history && outcome.quality.as_ref().map_or(true, |q| q.cacheable()) {
        let core = (*core).clone();
        let lang = lang.clone();
        let text = text.clone();
        let errors_json =
            serde_json::to_string(&outcome.errors).unwrap_or_else(|_| "[]".to_string());
        let timings_json = (!outcome.timings.is_empty())
            .then(|| serde_json::to_string(&outcome.timings).unwrap_or_else(|_| "{}".to_string()));
        let params_json = params.as_ref().and_then(|p| serde_json::to_string(p).ok());
        let text_hash = shared_model::text_hash(&text);
        let corrected_text = shared_model::apply_corrections(&text, &outcome.errors);
        let layers_json = (!outcome.layers.is_empty())
            .then(|| serde_json::to_string(&outcome.layers).unwrap_or_else(|_| "[]".to_string()));
        tokio::spawn(async move {
            if let Err(e) = core.detect_history.insert(
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
    Ok(DetectResult {
        errors: outcome.errors,
        decision_abstained: outcome.decision_abstained,
        timings: outcome.timings,
        from_history,
        llm_confirmed_clean: outcome.llm_confirmed_clean,
        layers: outcome.layers,
        quality: outcome.quality,
        chunks: outcome.chunks,
        sentences: outcome.sentences,
    })
}

/// 检测 + 归因一站式（支持三级响应级别 + 漏报强制兜底）
#[tauri::command]
pub async fn analyze(
    app: tauri::AppHandle,
    core: tauri::State<'_, Arc<Core>>,
    lang: String,
    text: String,
    session_id: Option<String>,
    // 三级响应级别（1/2/3，缺省 1；语义同 `detect` 的 level）
    level: Option<u8>,
    // 「漏报」强制兜底（语义同 `detect` 的 force_llm）
    force_llm: bool,
    // 忽略缓存绕过（语义同 `detect` 的 refresh）
    refresh: Option<bool>,
    // 分阶段进度条事件标识（语义同 `detect` 的 progress_id）
    progress_id: Option<String>,
) -> Result<AnalyzeResult, String> {
    let level = level.unwrap_or(1);
    if !(1..=3).contains(&level) {
        return Err(format!("无效响应级别: {level}（应为 1-3）"));
    }
    let sink = stage_sink(&app, progress_id.as_deref());
    // 统一管线（与 detect 命令及 api-server run_pipeline 同构）：历史复用 / 级联检测 /
    // 抑制过滤 / issue_type / anomaly 采纳 / L3 采集 / 参数快照
    let PipelineOut {
        outcome,
        from_history,
        params,
    } = run_pipeline(
        &core,
        PipelineSpecLite {
            lang: &lang,
            text: &text,
            category: &None,
            level,
            force_llm,
            refresh: refresh.unwrap_or(false),
            sink,
        },
    )
    .await?;
    let engine = core.tables.get(&lang);
    let (history, meta) = match &session_id {
        Some(id) => {
            let record = core.sessions.get_or_create(id, &lang);
            (record.total_errors(), Some(record.meta))
        }
        None => (0, None),
    };

    let mut out = Vec::new();
    for error in outcome.errors {
        let attribution = match &engine {
            Some(engine) => {
                // 英文错误：推断具体知识点，命中细粒度规则。
                // 数据优先：已发布知识点的改错对精确命中优先（新导入书自动参与归点），
                // 未命中回退静态映射。知识层开关关闭（测评消融臂）→ 只用静态映射。
                let knowledge_on = core.settings.read().unwrap().knowledge_enabled;
                let grammar_point = if lang == "en" {
                    let fallback = gec_en::classify_point(
                        &error.error_type,
                        &error.original,
                        error.correction.as_deref().unwrap_or(""),
                    );
                    if knowledge_on {
                        attribution_engine::knowledge::classify_point_en(
                            &core.knowledge,
                            &error.error_type,
                            &error.original,
                            error.correction.as_deref().unwrap_or(""),
                            fallback,
                        )
                    } else {
                        fallback
                    }
                } else if lang == "zh" {
                    // zh：检测命中的规则点 id 即细粒度知识点 id（与 api-server analyze 同步）
                    error.rule_id.clone()
                } else {
                    None
                };
                let ctx = AttributionContext {
                    lang: lang.clone(),
                    // zh 细粒度类型在决策表按粗族回退（§11.11 兼容，表不改）；
                    // en/ja 原样（ja 表有 word_order 命中行，不可折叠）
                    error_type: if lang == "zh" {
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
                let mut result = engine.attribute(&ctx).await.map_err(|e| e.to_string())?;
                if knowledge_on {
                    core.knowledge.enrich(&ctx, &mut result);
                }
                Some(result)
            }
            None => None,
        };
        out.push(AnalyzedError { error, attribution });
    }
    let session_id = session_id.unwrap_or_else(|| format!("s_{}", uuid::Uuid::new_v4().simple()));
    // 自动持久化（含归因结果 + 各阶段计时 + 参数快照 + 文本哈希；与 api-server analyze 路由对齐；
    // 后台写入，不阻塞检测响应）
    // 历史复用不写新记录；缓存资格：仅 quality=full 落库（同 detect 命令）
    if !from_history && outcome.quality.as_ref().map_or(true, |q| q.cacheable()) {
        let core = (*core).clone();
        let lang = lang.clone();
        let text = text.clone();
        let errors_json = serde_json::to_string(&out.iter().map(|a| &a.error).collect::<Vec<_>>())
            .unwrap_or_else(|_| "[]".to_string());
        let analyzed_json = serde_json::to_string(&out).ok();
        let timings_json = (!outcome.timings.is_empty())
            .then(|| serde_json::to_string(&outcome.timings).unwrap_or_else(|_| "{}".to_string()));
        let params_json = params.as_ref().and_then(|p| serde_json::to_string(p).ok());
        let text_hash = shared_model::text_hash(&text);
        let corrected_text = shared_model::apply_corrections(
            &text,
            &out.iter().map(|a| a.error.clone()).collect::<Vec<_>>(),
        );
        let layers_json = (!outcome.layers.is_empty())
            .then(|| serde_json::to_string(&outcome.layers).unwrap_or_else(|_| "[]".to_string()));
        let session_id = session_id.clone();
        tokio::spawn(async move {
            if let Err(e) = core.detect_history.insert(
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
    Ok(AnalyzeResult {
        lang,
        errors: out,
        session_id,
        decision_abstained: outcome.decision_abstained,
        timings: outcome.timings,
        from_history,
        llm_confirmed_clean: outcome.llm_confirmed_clean,
        layers: outcome.layers,
        quality: outcome.quality,
        chunks: outcome.chunks,
        sentences: outcome.sentences,
    })
}

/// 对全文中某一句做定向 LLM 复查（「待复查」条目的 LLM 复查按钮）。
///
/// 复用 ③-T 的 targeted_llm_review：只复查 span 字节区间，返回定位到
/// 全文坐标的具体错误（source='llm'）的 JSON。LLM 未配置时报错，
/// 前端展示原因并可重试（与 api-server /gec/recheck 同构）。
#[tauri::command]
pub async fn recheck(
    core: tauri::State<'_, Arc<Core>>,
    lang: String,
    text: String,
    span: [usize; 2],
) -> Result<String, String> {
    if span[0] >= span[1] || span[1] > text.len() {
        return Err(format!(
            "复查区间不合法: [{}, {})（全文长度 {}）",
            span[0],
            span[1],
            text.len()
        ));
    }
    let llm = core
        .llm_detect_engine(&lang)
        .ok_or_else(|| "AI 复查未配置（请先在设置中配置 LLM）".to_string())?;
    let (errors, _ledger) = targeted_llm_review(&llm, &text, &[span])
        .await
        .map_err(|e| e.to_string())?;
    serde_json::to_string(&errors).map_err(|e| e.to_string())
}

/// LLM 个性化讲解（按 lang + cefr 选择提示词模板）
///
/// 提供 `session_id` 时注入会话画像（母语 + 历史错误分布）。
/// 未配置 `LLM_API_KEY` 时返回错误，前端应回退到决策表静态补救建议。
/// 桌面端无 SSE 通道：内部走流式请求并收集全文后一次性返回
/// （非流式请求在生成完成前收不到任何数据，思考模型的长生成会被读超时中断）。
/// 返回 JSON：`{ explanation, exercises, model, template, template_version }`
#[tauri::command]
// Tauri 命令参数名即前端 invoke 契约（与 api-server 请求体字段同构），不能聚合为结构体
#[allow(clippy::too_many_arguments)]
pub async fn explain(
    core: tauri::State<'_, Arc<Core>>,
    lang: String,
    error_type: String,
    attribution: Option<String>,
    cefr: Option<String>,
    session_id: Option<String>,
    session_error_count: Option<u32>,
    target_language: Option<String>,
) -> Result<String, String> {
    let cefr_req = cefr;
    let attribution = attribution.unwrap_or_else(|| "unknown".to_string());
    // 会话画像：母语 + 自评 CEFR + 历史错误类型分布（有 session_id 时以档案为准）
    // 母语取 meta.native_language（用户自报），不是 meta.lang（检测语言）——与 api-server 同构修正
    let (native_language, profile_cefr, distribution, history_from_store) =
        match session_id.as_deref() {
            Some(id) => {
                let record = core.sessions.get_or_create(id, &lang);
                let distribution = if record.error_type_counts.is_empty() {
                    None
                } else {
                    let mut items: Vec<(String, u32)> = record
                        .error_type_counts
                        .iter()
                        .map(|(k, v)| (k.clone(), *v))
                        .collect();
                    items.sort_by_key(|a| std::cmp::Reverse(a.1));
                    Some(
                        items
                            .iter()
                            .map(|(k, v)| format!("{k}: {v}"))
                            .collect::<Vec<_>>()
                            .join(", "),
                    )
                };
                (
                    record.meta.native_language.clone(),
                    record.meta.cefr.clone(),
                    distribution,
                    record.total_errors(),
                )
            }
            None => (None, None, None, 0),
        };
    // CEFR 档位优先级：请求显式指定 > 会话画像自评 > A1
    let cefr = cefr_req
        .or(profile_cefr)
        .unwrap_or_else(|| "A1".to_string());
    let history = session_error_count
        .or(Some(history_from_store))
        .unwrap_or(0);

    let llm = core.llm.read().unwrap().clone();
    let (_template, mut stream) = llm
        .stream_explain(
            &lang,
            &error_type,
            &attribution,
            &cefr,
            history,
            native_language.as_deref(),
            distribution.as_deref(),
            target_language.as_deref(),
        )
        .await
        .map_err(|e| e.to_string())?;
    let mut full = String::new();
    use futures_util::StreamExt;
    while let Some(item) = stream.next().await {
        match item {
            Ok(Some(token)) => full.push_str(&token),
            Ok(None) => break,
            Err(e) => return Err(e.to_string()),
        }
    }
    let (explanation, exercises) = match full.split_once("【练习】") {
        Some((ex, exx)) => (ex.trim().to_string(), exx.trim().to_string()),
        None => (full, String::new()),
    };
    let template_key = llm
        .templates
        .for_lang_cefr(&lang, &cefr)
        .map(|t| format!("{}_{}", t.lang, t.cefr))
        .or_else(|| Some(format!("{lang}_generic")))
        .unwrap_or_default();
    let template_version = llm
        .templates
        .get(&template_key)
        .map(|t| t.version.clone())
        .unwrap_or_default();
    Ok(serde_json::json!({
        "explanation": explanation,
        "exercises": exercises,
        "model": llm.config.model,
        "template": template_key,
        "template_version": template_version,
    })
    .to_string())
}
