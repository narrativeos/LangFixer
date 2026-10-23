//! LLM 兜底结果反馈（采纳 / 拒绝）
//!
//! - **采纳**（adopt）：
//!   - 普通错误：尝试构建规则——
//!     - 构建成功（store 入库 + 写回词表文件 + 重建检测器）→ 返回规则信息
//!       （默认停用），用户在检测页快捷操作「启用 / 停用」（开关实时生效）；
//!     - 构建失败（入库错误 / 该语言无词表文件 → 规则无法被检测器加载）
//!       → 返回知识候选（不落库），UI 提示「沉淀为知识」并测试召回效果，
//!       用户选择「保留」（写入知识存储，②.5 召回）或「放弃」。
//!   - **anomaly**（AI 生成文本污染）：无 correction（删除属内容决策）；
//!     走**模式蒸馏**（二期飞轮）——LLM 教师从确认实例 + 现有模式 + 金集
//!     归纳新 `AnomalyPattern`，经确定性门槛（命中实例 + 金集负例零误报）
//!     后写回词表（检测器重建后即时生效）；门槛不过 / LLM 不可用 →
//!     降级知识分支（②.5 向量实例召回，用户选择保留/放弃）。
//! - **拒绝**（reject）：将错误的 `original` 文本加入自定义词白名单，
//!   后续检测不再报告该词。

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Json, State};
use gec_core::{LlmDetectConfig, LlmGecEngine};
use gec_zh::anomaly_distill::{
    GoldSet, anomaly_rule_id, append_pattern_to_lexicon, distill_pattern, evaluate_candidate,
};
use serde::Deserialize;
use shared_model::{Error, error_types};

use crate::routes::ApiError;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct LlmFeedbackRequest {
    pub lang: String,
    /// LLM 检出的错误（至少含 original + correction + error_type）
    pub error: Error,
    /// 操作类型：`"adopt"`（采纳）或 `"reject"`（拒绝，加入白名单）
    pub action: String,
}

#[derive(serde::Serialize)]
pub struct LlmFeedbackResponse {
    pub action: String,
    pub message: String,
    /// 采纳路径：`"rule"`（规则构建成功）/ `"knowledge"`（规则构建失败，知识候选）
    /// / `"anomaly"`（anomaly 蒸馏分支：`anomaly.accepted` 区分模式沉淀 / 知识降级）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    /// 规则分支：规则信息（`{id, wrong, right, enabled, created}`）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule: Option<serde_json::Value>,
    /// 知识分支：知识候选（`{text, error_type, correction}`；未落库，
    /// 由 UI 展示 + 召回测试后用户选择「保留 / 放弃」）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub knowledge: Option<serde_json::Value>,
    /// anomaly 分支：蒸馏结果（`{pattern, accepted, gate?, rule_id, reason?}`；
    /// `accepted=false` 时同时携带 `knowledge` 候选供用户选择）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anomaly: Option<serde_json::Value>,
    /// 拒绝时：白名单词 ID
    #[serde(skip_serializing_if = "Option::is_none")]
    pub word_id: Option<i64>,
}

/// POST /gec/llm-feedback — 对 LLM 兜底检出的错误进行采纳/拒绝
pub async fn llm_feedback(
    State(state): State<Arc<AppState>>,
    Json(req): Json<LlmFeedbackRequest>,
) -> Result<Json<LlmFeedbackResponse>, ApiError> {
    match req.action.as_str() {
        "adopt" => {
            // anomaly（AI 污染）无 correction：走模式蒸馏分支（二期飞轮）
            if req.error.error_type == error_types::ANOMALY {
                return Ok(Json(anomaly_adopt(&state, &req.lang, &req.error).await?));
            }
            Ok(Json(
                adopt_as_rule(&state, &req.lang, &req.error, "LLM").await?,
            ))
        }
        "reject" => {
            let word = req.error.original.clone();
            let cw = state
                .custom_words
                .add(&req.lang, &word, "LLM 兜底结果被用户拒绝")
                .map_err(|e| ApiError::Internal(format!("白名单写入失败: {e}")))?;
            Ok(Json(LlmFeedbackResponse {
                action: "reject".to_string(),
                message: format!("已拒绝：「{word}」加入白名单，后续检测将忽略该词"),
                via: None,
                rule: None,
                knowledge: None,
                anomaly: None,
                word_id: Some(cw.id),
            }))
        }
        other => Err(ApiError::BadRequest(format!(
            "无效操作: {other}（应为 \"adopt\" 或 \"reject\"）"
        ))),
    }
}

/// 采纳（非 anomaly）→ 规则蒸馏：把错误编译为规则（失败时降级知识候选）
///
/// `rule_id_prefix`：错误无 `rule_id` 时使用的规则 id 前缀
/// （`LLM` = LLM 兜底层；`MODEL` = decision / feature / statistical 模型层）。
/// 真正的新规则默认停用，可在规则面板启用；**已存在于词表文件**的规则（建库后
/// 才入文件、播种未覆盖）先按文件状态回填定义，采纳不改变其启用状态。
pub async fn adopt_as_rule(
    state: &AppState,
    lang: &str,
    error: &Error,
    rule_id_prefix: &str,
) -> Result<LlmFeedbackResponse, ApiError> {
    let rule_id = error
        .rule_id
        .clone()
        .unwrap_or_else(|| format!("{rule_id_prefix}_{}", error.error_type.to_uppercase()));
    let wrong = &error.original;
    let right = error.correction.as_deref().ok_or_else(|| {
        ApiError::BadRequest("采纳操作需要 correction 字段（修正文本）".to_string())
    })?;

    let settings = state.settings.read().unwrap().clone();
    let lexicons_dir = std::path::PathBuf::from(&settings.lexicons_dir);

    // ⓪ 目标规则在词表文件中但不在 store（建库后才加入文件，播种只在库空时执行）：
    //    先按文件回填定义（保留文件 enabled / category / priority / description），
    //    防止 `upsert_example` 自动创建「默认停用」影子定义、
    //    随后 `sync_rules_to_file` 把停用状态写回文件关掉原本启用的规则
    if let Err(e) = state
        .lexicon_store
        .backfill_rule_from_file(&lexicons_dir, lang, &rule_id)
    {
        tracing::warn!(
            error = %e,
            rule_id = %rule_id,
            "从词表文件回填规则定义失败（继续默认流程）"
        );
    }

    // ① 优先：编译为规则（真正的新规则默认停用，可在规则面板启用）
    let (rule_ok, created) =
        match state
            .lexicon_store
            .upsert_example(lang, &rule_id, wrong, right, "llm_distill")
        {
            Ok(created) => (true, created),
            Err(e) => {
                tracing::warn!(error = %e, rule_id = %rule_id, "规则入库失败，fallback 到知识存储");
                (false, false)
            }
        };

    // ② 规则能否生效的前提：写回词表文件（检测器只在构造时加载词表）
    let file_exists = lexicons_dir
        .join(format!("{lang}_gec_lexicon.json"))
        .exists();

    if rule_ok && file_exists {
        match state.lexicon_store.sync_rules_to_file(&lexicons_dir, lang) {
            Ok(outcome) => {
                // ③ 重建检测器：新规则进入检测词表（新规则默认停用；
                //    文件既有规则保持原启用状态；启用后即时生效）
                state.rebuild_router(&settings);
                tracing::info!(
                    rule = %rule_id,
                    created = created,
                    updated = ?outcome.updated_rule,
                    "采纳：规则已写回词表文件，检测器已重建"
                );
                let enabled = state
                    .lexicon_store
                    .get_rule(lang, &rule_id)
                    .ok()
                    .flatten()
                    .map(|d| d.enabled)
                    .unwrap_or(false);
                let message = if created {
                    if enabled {
                        format!(
                            "已采纳：「{wrong}」→「{right}」补入规则 {rule_id}（规则保持启用，后续同类错误将继续报告）"
                        )
                    } else {
                        format!(
                            "已采纳：「{wrong}」→「{right}」编译为规则 {rule_id}（默认停用，可在本页或规则面板启用）"
                        )
                    }
                } else if enabled {
                    format!(
                        "该示例已存在于规则 {rule_id}（规则保持启用，可在本页或规则面板切换开关）"
                    )
                } else {
                    format!("该示例已存在于规则 {rule_id}（默认停用，可在本页或规则面板启用）")
                };
                return Ok(LlmFeedbackResponse {
                    action: "adopt".to_string(),
                    message,
                    via: Some("rule".to_string()),
                    rule: Some(serde_json::json!({
                        "id": rule_id,
                        "wrong": wrong,
                        "right": right,
                        "enabled": enabled,
                        "created": created,
                    })),
                    knowledge: None,
                    anomaly: None,
                    word_id: None,
                });
            }
            Err(e) => {
                tracing::warn!(error = %e, "规则写回词表文件失败，转知识分支");
            }
        }
    }

    // ④ 知识分支：规则失败（入库错误 / 无词表文件 / 写回失败）
    //    返回知识候选（不落库）——由 UI 展示内容 + 测试召回效果后，
    //    用户选择「保留」（写入知识存储，②.5 召回）或「放弃」
    let reason: String = if !rule_ok {
        "规则入库失败".to_string()
    } else if !file_exists {
        format!("该语言无词表文件（{lang}），规则无法被检测器加载")
    } else {
        "规则写回词表文件失败".to_string()
    };
    Ok(LlmFeedbackResponse {
        action: "adopt".to_string(),
        message: format!("规则构建失败（{reason}），建议沉淀为知识"),
        via: Some("knowledge".to_string()),
        rule: None,
        knowledge: Some(serde_json::json!({
            "text": wrong,
            "error_type": error.error_type,
            "correction": right,
        })),
        anomaly: None,
        word_id: None,
    })
}

/// anomaly（AI 生成文本污染）采纳分支：模式蒸馏（二期自学习飞轮）
///
/// 流程：词表取目标规则 + 现有模式 → 加载金集 → LLM 教师蒸馏候选模式 →
/// 确定性门槛（[`evaluate_candidate`]：命中实例 + 金集负例零误报）→
/// - 过门槛：实例入库（`right=""`）+ 写回词表文件 + 追加模式 + 重建检测器
///   → 返回 `via:"anomaly"`（`accepted:true`，含模式与门槛报告）；
/// - 不过门槛 / LLM 不可用 / 金集缺失：实例仍入库（留痕），降级知识分支
///   （返回知识候选，UI 让用户选择「保留 / 放弃」②.5 沉淀）。
///
/// 入库前先按词表文件回填目标规则定义（[`LexiconStore::backfill_rule_from_file`]）：
/// 建库后才加入文件的规则（播种只在库空时执行，未覆盖）不会被 `upsert_example`
/// 的「默认停用」自动创建冲掉文件里的启用状态。
pub async fn anomaly_adopt(
    state: &AppState,
    lang: &str,
    error: &Error,
) -> Result<LlmFeedbackResponse, ApiError> {
    let wrong = &error.original;
    let settings = state.settings.read().unwrap().clone();
    let lexicons_dir = PathBuf::from(&settings.lexicons_dir);
    let lex_file = lexicons_dir.join(format!("{lang}_gec_lexicon.json"));

    // ① 目标规则 + 现有模式（词表文件是模式数据的权威来源）
    let (rule_id, existing) = match std::fs::read_to_string(&lex_file)
        .ok()
        .and_then(|c| serde_json::from_str::<gec_zh::rules::Lexicon>(&c).ok())
    {
        Some(lex) => (
            anomaly_rule_id(&lex)
                .unwrap_or("ZH_ANOMALY_ASSISTANT_REPLY")
                .to_string(),
            lex.rules
                .iter()
                .filter(|r| r.category == "anomaly")
                .flat_map(|r| r.patterns.iter().cloned())
                .collect::<Vec<_>>(),
        ),
        None => {
            tracing::warn!(
                path = %lex_file.display(),
                "词表文件缺失/解析失败，蒸馏使用空模式集"
            );
            ("ZH_ANOMALY_ASSISTANT_REPLY".to_string(), Vec::new())
        }
    };

    // ①.5 目标规则在文件中但不在 store（建库后才加入文件，播种只在库空时执行）：
    //      先按文件回填定义（保留文件 enabled / category / priority / description），
    //      防止后续 upsert 自动创建「默认停用」影子定义、
    //      随后 sync_rules_to_file 把停用状态写回文件关掉原本启用的规则
    if let Err(e) = state
        .lexicon_store
        .backfill_rule_from_file(&lexicons_dir, lang, &rule_id)
    {
        tracing::warn!(
            error = %e,
            rule = %rule_id,
            "从词表文件回填规则定义失败（继续默认流程）"
        );
    }

    // ② 金集（缺失 → 跳过蒸馏，直接降级知识分支）
    let gold_path = GoldSet::default_path();
    let gold = match GoldSet::load(&gold_path) {
        Ok(g) => g,
        Err(e) => {
            tracing::warn!(
                path = %gold_path.display(),
                error = %e,
                "anomaly 金集加载失败，跳过模式蒸馏"
            );
            return Ok(anomaly_knowledge_fallback(
                wrong,
                &error.error_type,
                &rule_id,
                None,
                "金集缺失，无法评估蒸馏模式",
            ));
        }
    };

    // ③ LLM 教师蒸馏（未配置 LLM / 缺蒸馏模板 / 调用或解析失败 → 降级）
    let template = state
        .llm_templates
        .read()
        .unwrap()
        .for_lang_cefr(lang, "anomaly_distill")
        .cloned();
    let (pattern, gate) = match template {
        Some(t) if state.is_llm_available() => {
            let llm = LlmGecEngine::new(
                lang,
                LlmDetectConfig::from_parts(
                    settings.llm.base_url.clone(),
                    settings.llm.api_key.clone(),
                    settings.llm.model.clone(),
                    settings.llm.timeout_secs,
                    t.system.clone(),
                    t.user.clone(),
                ),
            );
            match distill_pattern(&llm, &t.system, &t.user, wrong, &existing, &gold).await {
                Ok(p) => {
                    let g = evaluate_candidate(&p, &gold, wrong);
                    (p, g)
                }
                Err(e) => {
                    tracing::warn!(error = %e, "anomaly 模式蒸馏失败，降级知识分支");
                    return Ok(anomaly_knowledge_fallback(
                        wrong,
                        &error.error_type,
                        &rule_id,
                        None,
                        &format!("蒸馏失败：{e}"),
                    ));
                }
            }
        }
        Some(_) => {
            return Ok(anomaly_knowledge_fallback(
                wrong,
                &error.error_type,
                &rule_id,
                None,
                "LLM 服务未配置（请在设置界面配置 LLM 参数）",
            ));
        }
        None => {
            return Ok(anomaly_knowledge_fallback(
                wrong,
                &error.error_type,
                &rule_id,
                None,
                "蒸馏模板缺失（prompts/{lang}_anomaly_distill_prompt.json）",
            ));
        }
    };

    // ④ 门槛：通过 → 写回词表 + 重建检测器；不过 → 降级知识分支
    if gate.accepted {
        let created = state
            .lexicon_store
            .upsert_example(lang, &rule_id, wrong, "", "llm_distill")
            .map_err(|e| ApiError::Internal(format!("实例入库失败: {e}")))?;
        state
            .lexicon_store
            .sync_rules_to_file(&lexicons_dir, lang)
            .map_err(|e| ApiError::Internal(format!("词表文件写回失败: {e}")))?;
        append_pattern_to_lexicon(&lexicons_dir, lang, &rule_id, &pattern)
            .map_err(|e| ApiError::Internal(format!("模式写回词表失败: {e}")))?;
        state.rebuild_router(&settings);
        tracing::info!(
            lang,
            rule = %rule_id,
            pattern = %pattern.pattern,
            "anomaly 蒸馏模式已沉淀：写回词表文件，检测器已重建"
        );
        return Ok(LlmFeedbackResponse {
            action: "adopt".to_string(),
            message: format!(
                "已确认污染：蒸馏新模式「{}」并沉淀到规则 {rule_id}（金集零误报），后续检测将自动命中",
                pattern.note
            ),
            via: Some("anomaly".to_string()),
            rule: None,
            knowledge: None,
            anomaly: Some(serde_json::json!({
                "pattern": pattern,
                "accepted": true,
                "gate": gate,
                "rule_id": rule_id,
                "created": created,
            })),
            word_id: None,
        });
    }

    // ⑤ 门槛不过：实例仍入库留痕（写回词表 examples），降级知识分支
    let _ = state
        .lexicon_store
        .upsert_example(lang, &rule_id, wrong, "", "llm_distill");
    let _ = state.lexicon_store.sync_rules_to_file(&lexicons_dir, lang);
    tracing::info!(
        lang,
        rule = %rule_id,
        reason = %gate.reason,
        "anomaly 蒸馏模式未过金集门槛，降级知识分支"
    );
    Ok(anomaly_knowledge_fallback(
        wrong,
        &error.error_type,
        &rule_id,
        Some((pattern, gate)),
        "蒸馏模式未过金集门槛",
    ))
}

/// anomaly 降级知识分支：返回知识候选（未落库）+ 蒸馏诊断信息
///
/// UI 展示候选内容 + 召回测试后，用户选择「保留」（写入知识存储，
/// ②.5 向量召回）或「放弃」。
fn anomaly_knowledge_fallback(
    wrong: &str,
    error_type: &str,
    rule_id: &str,
    distill: Option<(
        gec_zh::rules::AnomalyPattern,
        gec_zh::anomaly_distill::GateReport,
    )>,
    reason: &str,
) -> LlmFeedbackResponse {
    let (pattern, gate) = match distill {
        Some((p, g)) => (Some(p), Some(g)),
        None => (None, None),
    };
    LlmFeedbackResponse {
        action: "adopt".to_string(),
        message: format!(
            "模式蒸馏未沉淀（{reason}），已降级为知识候选；可选择「保留」沉淀到向量知识库"
        ),
        via: Some("anomaly".to_string()),
        rule: None,
        knowledge: Some(serde_json::json!({
            "text": wrong,
            "error_type": error_type,
            "correction": "",
        })),
        anomaly: Some(serde_json::json!({
            "pattern": pattern,
            "gate": gate,
            "accepted": false,
            "rule_id": rule_id,
            "reason": reason,
        })),
        word_id: None,
    }
}
