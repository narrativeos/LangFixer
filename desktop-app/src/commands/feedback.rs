use std::path::PathBuf;
use std::sync::Arc;

use shared_model::{SessionRecord, error_types};

use super::agent::run_agent_cmd;
use super::anomaly::anomaly_adopt_cmd;
use super::core::Core;
use super::l3::{collect_feedback_assets_cmd, l3_collect_feedback, l3_collect_sample};
/// 记录会话犯错历史
#[tauri::command]
pub fn record_error(
    core: tauri::State<'_, Arc<Core>>,
    session_id: String,
    lang: String,
    error_type: String,
) -> SessionRecord {
    let now = chrono::Utc::now().to_rfc3339();
    core.sessions
        .record_error(&session_id, &lang, &error_type, &now)
}

/// LLM 兜底结果反馈（采纳/拒绝）
///
/// "adopt" 尝试构建规则：
/// - 构建成功（store 入库 + 写回词表文件 + 重建检测器）→ 返回规则信息（默认停用），
///   用户在检测页快捷操作「启用 / 停用」（开关实时生效，无需再重建）；
/// - 构建失败（入库错误 / 该语言无词表文件 → 规则永远无法被检测器加载）
///   → 返回知识候选（不落库），UI 提示「沉淀为知识」并测试召回效果，
///   用户选择「保留」（写入知识存储，②.5 召回）或「放弃」。
#[tauri::command]
pub async fn llm_feedback(
    core: tauri::State<'_, Arc<Core>>,
    lang: String,
    error: shared_model::Error,
    action: String,
) -> Result<serde_json::Value, String> {
    llm_feedback_inner(&core, &lang, &error, &action).await
}

/// LLM 兜底结果反馈核心逻辑（`llm_feedback` 命令与 `error_feedback` 共用）
async fn llm_feedback_inner(
    core: &Core,
    lang: &str,
    error: &shared_model::Error,
    action: &str,
) -> Result<serde_json::Value, String> {
    match action {
        "adopt" => {
            // anomaly（AI 污染）无 correction：走模式蒸馏分支（二期飞轮）
            if error.error_type == error_types::ANOMALY {
                return anomaly_adopt_cmd(core, lang, error).await;
            }
            let rule_id = error
                .rule_id
                .clone()
                .unwrap_or_else(|| format!("LLM_{}", error.error_type.to_uppercase()));
            let wrong = &error.original;
            let right = error
                .correction
                .as_deref()
                .ok_or_else(|| "采纳操作需要 correction 字段（修正文本）".to_string())?;

            let settings = core.settings.read().unwrap().clone();
            let lexicons_dir = PathBuf::from(&settings.lexicons_dir);

            // ⓪ 目标规则在词表文件中但不在 store（建库后才加入文件，播种只在库空时执行）：
            //    先按文件回填定义（保留文件 enabled / category / priority / description），
            //    防止 `upsert_example` 自动创建「默认停用」影子定义、
            //    随后 `sync_rules_to_file` 把停用状态写回文件关掉原本启用的规则
            if let Err(e) =
                core.lexicon_store
                    .backfill_rule_from_file(&lexicons_dir, lang, &rule_id)
            {
                tracing::warn!(
                    error = %e,
                    rule_id = %rule_id,
                    "从词表文件回填规则定义失败（继续默认流程）"
                );
            }

            // ① 尝试构建规则：store 入库（真正的新规则默认停用，可在规则面板启用）
            let (rule_ok, created) =
                match core
                    .lexicon_store
                    .upsert_example(lang, &rule_id, wrong, right, "llm_distill")
                {
                    Ok(created) => (true, created),
                    Err(e) => {
                        tracing::warn!(error = %e, rule_id = %rule_id, "规则入库失败");
                        (false, false)
                    }
                };

            // ② 规则能否生效的前提：写回词表文件（检测器只在构造时加载词表）
            let file_exists = lexicons_dir
                .join(format!("{lang}_gec_lexicon.json"))
                .exists();

            if rule_ok && file_exists {
                match core.lexicon_store.sync_rules_to_file(&lexicons_dir, lang) {
                    Ok(outcome) => {
                        // ③ 重建检测器：新规则进入检测词表（新规则默认停用；
                        //    文件既有规则保持原启用状态；启用后即时生效）
                        core.rebuild_router(&settings);
                        tracing::info!(
                            rule = %rule_id,
                            created = created,
                            updated = ?outcome.updated_rule,
                            "采纳：规则已写回词表文件，检测器已重建"
                        );
                        let enabled = core
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
                            format!(
                                "该示例已存在于规则 {rule_id}（默认停用，可在本页或规则面板启用）"
                            )
                        };
                        return Ok(serde_json::json!({
                            "action": "adopt",
                            "via": "rule",
                            "message": message,
                            "rule": {
                                "id": rule_id,
                                "wrong": wrong,
                                "right": right,
                                "enabled": enabled,
                                "created": created,
                            },
                        }));
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
            Ok(serde_json::json!({
                "action": "adopt",
                "via": "knowledge",
                "message": format!("规则构建失败（{reason}），建议沉淀为知识"),
                "knowledge": {
                    "text": wrong,
                    "error_type": error.error_type,
                    "correction": right,
                },
            }))
        }
        "reject" => {
            let word = error.original.clone();
            let cw = core
                .custom_words
                .add(lang, &word, "LLM 兜底结果被用户拒绝")
                .map_err(|e| format!("白名单写入失败: {e}"))?;
            Ok(serde_json::json!({
                "action": "reject",
                "message": format!("已拒绝：「{word}」加入白名单"),
                "word_id": cw.id,
            }))
        }
        other => Err(format!("无效操作: {other}（应为 \"adopt\" 或 \"reject\"）")),
    }
}

/// 反馈日志写入（失败仅告警，不阻断反馈主流程）
fn log_feedback_cmd(
    core: &Core,
    lang: &str,
    source: &str,
    error: &shared_model::Error,
    action: &str,
    reason: &Option<String>,
) {
    let rule_id = error.rule_id.as_deref();
    if let Err(e) = core.feedback_log.insert(
        lang,
        source,
        rule_id,
        &error.error_type,
        &error.original,
        error.correction.as_deref().unwrap_or(""),
        action,
        reason.as_deref().unwrap_or(""),
    ) {
        tracing::warn!(error = %e, "反馈日志写入失败（不阻断）");
    }
}

/// 模型层（decision / feature / statistical）采纳：蒸馏为规则（失败时降级/// 降级知识候选）
async fn adopt_as_rule_cmd(
    core: &Core,
    lang: &str,
    error: &shared_model::Error,
    rule_id_prefix: &str,
) -> Result<serde_json::Value, String> {
    let rule_id = error
        .rule_id
        .clone()
        .unwrap_or_else(|| format!("{rule_id_prefix}_{}", error.error_type.to_uppercase()));
    let wrong = &error.original;
    let right = error
        .correction
        .as_deref()
        .ok_or_else(|| "采纳操作需要 correction 字段（修正文本）".to_string())?;
    let settings = core.settings.read().unwrap().clone();
    let lexicons_dir = std::path::PathBuf::from(&settings.lexicons_dir);
    // ⓪ 目标规则在词表文件中但不在 store（建库后才加入文件，播种只在库空时执行）：
    //    先按文件回填定义（保留文件 enabled / category / priority / description），
    //    防止 `upsert_example` 自动创建「默认停用」影子定义、
    //    随后 `sync_rules_to_file` 把停用状态写回文件关掉原本启用的规则
    if let Err(e) = core
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
        match core
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
        match core.lexicon_store.sync_rules_to_file(&lexicons_dir, lang) {
            Ok(outcome) => {
                core.rebuild_router(&settings);
                tracing::info!(
                    rule = %rule_id,
                    created = created,
                    updated = ?outcome.updated_rule,
                    "采纳：规则已写回词表文件，检测器已重建"
                );
                let enabled = core
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
                            "已采纳：「{wrong}」→「{right}」编译为规则 {rule_id}（默认停用，可在规则面板启用）"
                        )
                    }
                } else if enabled {
                    format!("该示例已存在于规则 {rule_id}（规则保持启用，可在规则面板切换开关）")
                } else {
                    format!("该示例已存在于规则 {rule_id}（默认停用，可在规则面板启用）")
                };
                return Ok(serde_json::json!({
                    "action": "adopt",
                    "message": message,
                    "via": "rule",
                    "rule": {
                        "id": rule_id,
                        "wrong": wrong,
                        "right": right,
                        "enabled": enabled,
                        "created": created,
                    },
                }));
            }
            Err(e) => {
                tracing::warn!(error = %e, "规则写回词表文件失败，转知识分支");
            }
        }
    }
    // ④ 知识分支：规则失败 → 知识候选（UI 展示 + 召回测试后用户选择保留/放弃）
    let reason: String = if !rule_ok {
        "规则入库失败".to_string()
    } else if !file_exists {
        format!("该语言无词表文件（{lang}），规则无法被检测器加载")
    } else {
        "规则写回词表文件失败".to_string()
    };
    Ok(serde_json::json!({
        "action": "adopt",
        "message": format!("规则构建失败（{reason}），建议沉淀为知识"),
        "via": "knowledge",
        "knowledge": {
            "text": wrong,
            "error_type": error.error_type,
            "correction": right,
        },
    }))
}

/// 组装反思上下文：按来源层从各存储定位具体对象
fn build_reflection_context_cmd<'a>(
    core: &Core,
    lang: &str,
    error: &'a shared_model::Error,
    reason: Option<&'a str>,
) -> gec_core::ReflectionContext<'a> {
    let source = gec_core::normalize_source(error);
    let mut rule_def: Option<gec_core::RuleDefInfo> = None;
    let mut rule_feedback: Option<(i64, i64)> = None;
    let mut matched_example: Option<gec_core::ExampleInfo> = None;
    let mut knowledge_item: Option<gec_core::KnowledgeInfo> = None;
    if source == shared_model::sources::RULE {
        if let Some(rule_id) = &error.rule_id {
            if let Some(def) = core.lexicon_store.rule_defs_by_id(rule_id).ok().flatten() {
                let count = core
                    .lexicon_store
                    .list_rule_examples(lang, rule_id, 1, 1)
                    .map(|l| l.total)
                    .unwrap_or(0);
                rule_def = Some(gec_core::RuleDefInfo {
                    id: def.id,
                    category: def.category,
                    description: def.description,
                    enabled: def.enabled,
                    example_count: count,
                });
            }
            rule_feedback = core.feedback_log.stats_by_rule_distinct(lang, rule_id).ok(); // 已含本次（log_feedback_cmd 先于本函数执行）；按 original 去重
            if let Ok(list) = core.lexicon_store.list_rule_examples(lang, rule_id, 1, 200) {
                matched_example = list
                    .examples
                    .into_iter()
                    .find(|ex| ex.wrong.trim() == error.original.trim())
                    .map(|ex| gec_core::ExampleInfo {
                        wrong: ex.wrong,
                        right: ex.right,
                        source: ex.source,
                    });
            }
        }
    } else if source == shared_model::sources::KNOWLEDGE
        && let Ok(items) = core.learned_knowledge.list(lang)
    {
        // ① 精确：条目文本 == original
        if let Some(it) = items
            .iter()
            .find(|it| it.text.trim() == error.original.trim())
        {
            knowledge_item = Some(gec_core::KnowledgeInfo {
                id: it.id,
                text: it.text.clone(),
                error_type: it.error_type.clone(),
                correction: it.correction.clone(),
                source: it.source.clone(),
            });
        } else {
            // ② 模糊：bigram 相似度最高者
            let mut best: Option<(i64, f64)> = None;
            for it in &items {
                let sim = gec_core::fuzzy_similarity(&it.text, &error.original);
                if sim >= gec_core::knowledge_recall::FUZZY_THRESHOLD
                    && best.as_ref().is_none_or(|(_, b)| sim > *b)
                {
                    best = Some((it.id, sim));
                }
            }
            if let Some((id, _)) = best {
                knowledge_item =
                    items
                        .iter()
                        .find(|it| it.id == id)
                        .map(|it| gec_core::KnowledgeInfo {
                            id: it.id,
                            text: it.text.clone(),
                            error_type: it.error_type.clone(),
                            correction: it.correction.clone(),
                            source: it.source.clone(),
                        });
            }
        }
    }
    gec_core::ReflectionContext {
        error,
        reason,
        rule_def,
        rule_feedback,
        matched_example,
        knowledge_item,
    }
}

/// 检测结果统一反馈（采纳 / 放弃）+ 反思流程（全部来源层）
///
/// 镜像 api-server `POST /gec/feedback`：
/// - adopt：llm → 复用现有 LLM 反馈采纳分支（anomaly 蒸馏 / 规则蒸馏 / 知识候选）；
///   rule（非 anomaly）→ 确认规则正确（留痕 + 示例补强）；
///   模型层（decision/feature/statistical）→ 蒸馏为规则 + L3 正样本；
///   knowledge → 确认知识条目正确（留痕）；
/// - reject：白名单止血 + 反馈留痕 + 反思报告（诊断 + 可执行建议）
///   + 模型层自动汇入 L3 负样本。
#[tauri::command]
pub async fn error_feedback(
    core: tauri::State<'_, Arc<Core>>,
    lang: String,
    error: shared_model::Error,
    action: String,
    reason: Option<String>,
    ctx: Option<serde_json::Value>,
    session_id: Option<String>,
) -> Result<serde_json::Value, String> {
    let mut res = error_feedback_body(
        core.clone(),
        lang.clone(),
        error.clone(),
        action.clone(),
        reason.clone(),
    )
    .await?;
    // 错题本（非阻断）：采纳成功且带会话时写入该会话错题本（Phase 2 学习扩展）；
    // adopt_count = 此类错误累计采纳次数（含本次，D2 闭环价值感）
    if action == "adopt"
        && let Some(sid) = session_id.as_deref()
    {
        let sentence = ctx
            .as_ref()
            .and_then(|v| v.get("src_text"))
            .and_then(|v| v.as_str())
            .map(|src| shared_model::sentence_around(src, error.span, &error.original))
            .unwrap_or_else(|| error.original.clone());
        let adopt_count = core.sessions.add_mistake(
            sid,
            &lang,
            &error.error_type,
            &error.original,
            error.correction.as_deref(),
            &sentence,
        );
        if let Some(obj) = res.as_object_mut() {
            obj.insert("adopt_count".to_string(), serde_json::json!(adopt_count));
        }
    }
    // 资产沉淀（非阻断）：adopt → adopted.jsonl 句对；reject → rejected.jsonl 负偏好
    collect_feedback_assets_cmd(core.inner(), &lang, &error, &action, &reason, ctx.as_ref());
    Ok(res)
}

async fn error_feedback_body(
    core: tauri::State<'_, Arc<Core>>,
    lang: String,
    error: shared_model::Error,
    action: String,
    reason: Option<String>,
) -> Result<serde_json::Value, String> {
    let source = gec_core::normalize_source(&error).to_string();
    let llm_reflect_ready = core.llm.read().unwrap().is_available();
    match action.as_str() {
        "adopt" => {
            if source == shared_model::sources::LLM {
                // 复用现有 LLM 反馈采纳分支（anomaly 蒸馏 / 规则蒸馏 / 知识候选）
                let res = llm_feedback_inner(core.inner(), &lang, &error, "adopt").await?;
                // 采纳留痕（成功后记录；失败请求不落日志）
                log_feedback_cmd(core.inner(), &lang, &source, &error, "adopt", &reason);
                l3_collect_feedback(core.inner(), &lang, &error, 1, "llm_adopt");
                return Ok(res);
            }
            if source == shared_model::sources::RULE {
                if error.error_type == error_types::ANOMALY {
                    let res = anomaly_adopt_cmd(core.inner(), &lang, &error).await?;
                    // 采纳留痕（anomaly 分支提前返回，须在此记录；失败请求不落日志）
                    log_feedback_cmd(core.inner(), &lang, &source, &error, "adopt", &reason);
                    return Ok(res);
                }
                // 确认规则正确：留痕 + 示例补强（幂等）
                log_feedback_cmd(core.inner(), &lang, &source, &error, "adopt", &reason);
                let mut reinforced = false;
                if let (Some(rule_id), Some(right)) =
                    (error.rule_id.as_deref(), error.correction.as_deref())
                    && !right.trim().is_empty()
                {
                    // 规则在词表文件中但不在 store（建库后才加入文件，播种只在库空时执行）：
                    // 先按文件回填定义，防止 upsert 自动创建「默认停用」影子定义
                    let lexicons_dir = PathBuf::from(&core.settings.read().unwrap().lexicons_dir);
                    if let Err(e) =
                        core.lexicon_store
                            .backfill_rule_from_file(&lexicons_dir, &lang, rule_id)
                    {
                        tracing::warn!(
                            error = %e,
                            rule_id = %rule_id,
                            "从词表文件回填规则定义失败（继续默认流程）"
                        );
                    }
                    if let Ok(true) = core.lexicon_store.upsert_example(
                        &lang,
                        rule_id,
                        &error.original,
                        right,
                        "manual",
                    ) {
                        reinforced = true;
                    }
                }
                let rule_id = error
                    .rule_id
                    .clone()
                    .unwrap_or_else(|| "（未入库规则）".to_string());
                let message = if reinforced {
                    format!(
                        "已采纳：确认规则 {rule_id} 正确，并补强示例「{}」→「{}」",
                        error.original,
                        error.correction.as_deref().unwrap_or("")
                    )
                } else {
                    format!("已采纳：确认规则 {rule_id} 正确（后续同类错误将继续报告）")
                };
                return Ok(serde_json::json!({
                    "action": "adopt",
                    "message": message,
                    "via": "confirm",
                    "rule": { "id": rule_id, "enabled": true },
                }));
            }
            if matches!(
                source.as_str(),
                shared_model::sources::DECISION
                    | shared_model::sources::FEATURE
                    | shared_model::sources::STATISTICAL
            ) {
                let res = adopt_as_rule_cmd(core.inner(), &lang, &error, "MODEL").await?;
                // 采纳留痕（成功后记录；失败请求不落日志）
                log_feedback_cmd(core.inner(), &lang, &source, &error, "adopt", &reason);
                l3_collect_feedback(core.inner(), &lang, &error, 1, "model_adopt");
                return Ok(res);
            }
            if source == shared_model::sources::KNOWLEDGE {
                log_feedback_cmd(core.inner(), &lang, &source, &error, "adopt", &reason);
                return Ok(serde_json::json!({
                    "action": "adopt",
                    "message": format!(
                        "已采纳：确认知识条目「{}」为真实错误（后续同类错误将继续报告）",
                        error.original
                    ),
                    "via": "confirm",
                }));
            }
            Err(format!("未知来源层: {source}"))
        }
        "reject" => {
            // ① 白名单：original 不再报告（所有来源层通用止血手段）
            let word = error.original.clone();
            let cw = core
                .custom_words
                .add(&lang, &word, "检测结果被用户放弃")
                .map_err(|e| e.to_string())?;
            // ② 反馈留痕（反思流程的数据基础）
            log_feedback_cmd(core.inner(), &lang, &source, &error, "reject", &reason);
            // ③ 反思：按来源层诊断偏差来源，产出报告 + 可执行建议
            let ctx = build_reflection_context_cmd(core.inner(), &lang, &error, reason.as_deref());
            let report = gec_core::reflect_rejection(&ctx);
            // ④ 模型层偏差：自动汇入 L3 飞轮负样本（非阻断）
            if matches!(
                source.as_str(),
                shared_model::sources::DECISION
                    | shared_model::sources::FEATURE
                    | shared_model::sources::STATISTICAL
            ) {
                l3_collect_feedback(core.inner(), &lang, &error, 0, "reject_model");
            }
            // ⑤ 自进化 agent：LLM 已配置时自主完成低风险处置并产出高风险提案
            //    （同步 + 超时保护；失败/未配置 → agent: null，行为与引入前一致）
            let (agent_available, agent_result, agent_error) = {
                let source = error.source.as_deref().unwrap_or("未知");
                let rule = error.rule_id.as_deref().unwrap_or("（无关联规则）");
                let correction = error.correction.as_deref().unwrap_or("（无）");
                let task = gec_core::build_rejection_task(
                    &lang,
                    &error.original,
                    &error.error_type,
                    source,
                    rule,
                    correction,
                    &report.diagnosis,
                );
                run_agent_cmd(core.inner(), &lang, &task, Some(&error)).await
            };
            if agent_available && agent_result.is_none() {
                tracing::warn!(
                    error = %agent_error.unwrap_or_default(),
                    "自进化 agent 运行失败（已回退确定性路径）"
                );
            }
            tracing::info!(
                lang = %lang,
                source = %source,
                subject = %report.subject,
                "检测结果被放弃：反思完成"
            );
            Ok(serde_json::json!({
                "action": "reject",
                "message": format!("已放弃：「{word}」加入白名单；偏差诊断：{}", report.diagnosis),
                "word_id": cw.id,
                "reflection": report,
                "llm_reflect_ready": llm_reflect_ready,
                "agent": agent_result,
            }))
        }
        other => Err(format!("无效操作: {other}（应为 \"adopt\" 或 \"reject\"）")),
    }
}

/// 执行反思建议（前端一键按钮）
///
/// `action`：`disable_rule` / `exclude_word` / `remove_example` / `remove_knowledge` / `l3_negative`
#[tauri::command]
pub fn feedback_action(
    core: tauri::State<'_, Arc<Core>>,
    action: String,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let p = &payload;
    let get = |k: &str| p.get(k).and_then(|v| v.as_str()).unwrap_or("");
    match action.as_str() {
        "disable_rule" => {
            let (lang, rule_id) = (get("lang"), get("rule_id"));
            if lang.is_empty() || rule_id.is_empty() {
                return Err("disable_rule 需要 lang + rule_id".into());
            }
            core.lexicon_store
                .toggle_rule(lang, rule_id, false)
                .map_err(|e| e.to_string())?;
            Ok(serde_json::json!({
                "ok": true,
                "message": format!("规则 {rule_id} 已停用（可在规则面板重新启用）"),
            }))
        }
        "remove_example" => {
            let (lang, rule_id, wrong, right) =
                (get("lang"), get("rule_id"), get("wrong"), get("right"));
            if lang.is_empty() || rule_id.is_empty() || wrong.is_empty() {
                return Err("remove_example 需要 lang + rule_id + wrong".into());
            }
            let deleted = core
                .lexicon_store
                .delete_example(lang, rule_id, wrong, right)
                .map_err(|e| e.to_string())?;
            Ok(serde_json::json!({
                "ok": deleted,
                "message": if deleted {
                    format!("示例「{wrong}」已从规则 {rule_id} 删除")
                } else {
                    format!("示例「{wrong}」不在规则 {rule_id} 中（可能已被删除）")
                },
            }))
        }
        "remove_knowledge" => {
            let id = p.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
            if id <= 0 {
                return Err("remove_knowledge 需要 id".into());
            }
            let removed = core
                .learned_knowledge
                .remove(id)
                .map_err(|e| e.to_string())?;
            Ok(serde_json::json!({
                "ok": removed,
                "message": if removed {
                    format!("知识条目 #{id} 已删除（后续不再召回）")
                } else {
                    format!("知识条目 #{id} 不存在（可能已被删除）")
                },
            }))
        }
        "l3_negative" => {
            let (lang, text) = (get("lang"), get("text"));
            if lang.is_empty() || text.is_empty() {
                return Err("l3_negative 需要 lang + text".into());
            }
            if !matches!(lang, "en" | "zh") {
                return Err(format!("L3 飞轮暂不支持语言: {lang}"));
            }
            let error_type = get("error_type");
            let sample = serde_json::json!({
                "text": text,
                "label": 0,
                "error_type": error_type,
                "source": "reflect_negative",
                "ts": chrono::Utc::now().to_rfc3339(),
            });
            let added = l3_collect_sample(&core.l3_data, lang, &sample).is_some();
            Ok(serde_json::json!({
                "ok": true,
                "message": if added {
                    format!("「{text}」已汇入 L3 飞轮负样本（积累后重训练可系统性修正）")
                } else {
                    format!("「{text}」已在 L3 飞轮负样本中（去重）")
                },
            }))
        }
        // 规则排除词（payload: {lang, rule_id, word}）：写回词表 rules[].excludes
        // + 重建检测器——规则保持启用，仅该词不再触发（POS 误标等单点误报的精准对策）
        "exclude_word" => {
            let (lang, rule_id, word) = (get("lang"), get("rule_id"), get("word"));
            if lang.is_empty() || rule_id.is_empty() || word.is_empty() {
                return Err("exclude_word 需要 lang + rule_id + word".into());
            }
            let settings = core.settings.read().unwrap().clone();
            let lexicons_dir = PathBuf::from(settings.lexicons_dir.clone());
            let added = lexicon_store::append_rule_exclude(&lexicons_dir, lang, rule_id, word)
                .map_err(|e| e.to_string())?;
            if added {
                // 检测器只在构造时加载词表 → 重建即时生效（与采纳路径同一机制）
                core.rebuild_router(&settings);
            }
            Ok(serde_json::json!({
                "ok": true,
                "message": if added {
                    format!(
                        "「{word}」已加入规则 {rule_id} 排除名单（该词不再触发此规则，检测器已重建）"
                    )
                } else {
                    format!("「{word}」已在规则 {rule_id} 排除名单中")
                },
            }))
        }
        other => Err(format!("未知操作: {other}")),
    }
}

/// LLM 深度反思（`{lang}_reflect` 模板；与确定性反思分离，避免阻塞放弃操作）
#[tauri::command]
pub async fn reflect_llm(
    core: tauri::State<'_, Arc<Core>>,
    lang: String,
    error: shared_model::Error,
    reason: Option<String>,
) -> Result<serde_json::Value, String> {
    let llm = core.llm.read().unwrap().clone();
    if !llm.is_available() {
        return Err("LLM 服务未启用（请在「设置」界面配置 LLM 参数）".into());
    }
    let template = llm
        .templates
        .for_lang_cefr(&lang, "reflect")
        .ok_or_else(|| format!("语言 {lang} 无反思模板（prompts/{{lang}}_reflect_prompt.json）"))?;
    let source = gec_core::normalize_source(&error);
    let reason = reason.as_deref().unwrap_or("");
    let user = template.render(&[
        ("lang", &lang),
        ("source", source),
        ("error_type", &error.error_type),
        ("original", &error.original),
        (
            "correction",
            error.correction.as_deref().unwrap_or("（无修正）"),
        ),
        ("rule_id", error.rule_id.as_deref().unwrap_or("（无）")),
        (
            "reason",
            if reason.is_empty() {
                "（用户未说明）"
            } else {
                reason
            },
        ),
    ]);
    let analysis = llm
        .complete(&template.system, &user)
        .await
        .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "llm_analysis": analysis,
        "model": llm.config.model,
        "template": format!("{lang}_reflect"),
    }))
}

/// 反馈历史（设置界面「反馈历史」面板）
#[tauri::command]
pub fn feedback_history(
    core: tauri::State<'_, Arc<Core>>,
    lang: String,
    limit: Option<usize>,
) -> Result<serde_json::Value, String> {
    let limit = limit.unwrap_or(50);
    let stats = core.feedback_log.stats(&lang).map_err(|e| e.to_string())?;
    let recent = core
        .feedback_log
        .list(Some(&lang), limit)
        .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "lang": lang,
        "stats": stats,
        "recent": recent,
    }))
}
// ③ 重建检测器：新规则进入检测词表（默认停用

// ─────────────────────────────────────────────────────────────────────────────
// 测评中心（进化中心 P0）：Python 测评服务（datasets/eval_server.py）代理
// web-ui client.ts 桌面端走这 4 个命令（Web 端走 api-server /gec/eval/*）
// ─────────────────────────────────────────────────────────────────────────────
