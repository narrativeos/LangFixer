use std::path::PathBuf;
use std::sync::Arc;

use gec_core::{LlmDetectConfig, LlmGecEngine};
use gec_zh::anomaly_distill::{
    GoldSet, anomaly_rule_id, append_pattern_to_lexicon, distill_pattern, evaluate_candidate,
};

use shared_model::{Error, error_types};

use super::core::Core;
use super::util::llm_breaker;
/// anomaly（AI 生成文本污染）采纳分支：模式蒸馏（二期自学习飞轮；
/// 与 api-server `anomaly_adopt` 同逻辑，Tauri 命令返回 JSON）
pub(crate) async fn anomaly_adopt_cmd(
    core: &Core,
    lang: &str,
    error: &Error,
) -> Result<serde_json::Value, String> {
    let wrong = &error.original;
    let settings = core.settings.read().unwrap().clone();
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
    if let Err(e) = core
        .lexicon_store
        .backfill_rule_from_file(&lexicons_dir, lang, &rule_id)
    {
        tracing::warn!(
            error = %e,
            rule = %rule_id,
            "从词表文件回填规则定义失败（继续默认流程）"
        );
    }

    // ② 金集（缺失 → 降级知识分支）
    let gold_path = GoldSet::default_path();
    let gold = GoldSet::load(&gold_path).map_err(|e| {
        tracing::warn!(path = %gold_path.display(), error = %e, "anomaly 金集加载失败");
        format!("金集缺失，无法评估蒸馏模式（{e}）")
    })?;

    // ③ LLM 教师蒸馏（未配置 / 缺模板 / 失败 → 降级）
    //    guard 不可跨 await（非 Send）——先在锁内克隆 LLM 引擎与模板，再立即释放
    let (llm, template) = {
        let service = core.llm.read().unwrap();
        let t = service
            .templates
            .for_lang_cefr(lang, "anomaly_distill")
            .cloned();
        let llm = if service.config.enabled() {
            t.as_ref().map(|t| {
                LlmGecEngine::with_breaker(
                    lang,
                    LlmDetectConfig::from_parts(
                        service.config.base_url.clone(),
                        service.config.api_key.clone(),
                        service.config.model.clone(),
                        service.config.timeout_secs,
                        t.system.clone(),
                        t.user.clone(),
                    ),
                    llm_breaker(),
                )
            })
        } else {
            None
        };
        (llm, t)
    };
    let (pattern, gate) = match (llm, template) {
        (Some(llm), Some(t)) => {
            match distill_pattern(&llm, &t.system, &t.user, wrong, &existing, &gold).await {
                Ok(p) => {
                    let g = evaluate_candidate(&p, &gold, wrong);
                    (p, g)
                }
                Err(e) => {
                    return Ok(anomaly_knowledge_fallback_cmd(
                        wrong,
                        &error.error_type,
                        &rule_id,
                        None,
                        &format!("蒸馏失败：{e}"),
                    ));
                }
            }
        }
        (None, Some(_)) => {
            return Ok(anomaly_knowledge_fallback_cmd(
                wrong,
                &error.error_type,
                &rule_id,
                None,
                "LLM 服务未配置（请在设置界面配置 LLM 参数）",
            ));
        }
        (_, None) => {
            return Ok(anomaly_knowledge_fallback_cmd(
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
        let created = core
            .lexicon_store
            .upsert_example(lang, &rule_id, wrong, "", "llm_distill")
            .map_err(|e| e.to_string())?;
        core.lexicon_store
            .sync_rules_to_file(&lexicons_dir, lang)
            .map_err(|e| format!("词表文件写回失败: {e}"))?;
        append_pattern_to_lexicon(&lexicons_dir, lang, &rule_id, &pattern)
            .map_err(|e| format!("模式写回词表失败: {e}"))?;
        core.rebuild_router(&settings);
        tracing::info!(
            lang,
            rule = %rule_id,
            pattern = %pattern.pattern,
            "anomaly 蒸馏模式已沉淀：写回词表文件，检测器已重建"
        );
        return Ok(serde_json::json!({
            "action": "adopt",
            "via": "anomaly",
            "message": format!(
                "已确认污染：蒸馏新模式「{}」并沉淀到规则 {rule_id}（金集零误报），后续检测将自动命中",
                pattern.note
            ),
            "anomaly": {
                "pattern": pattern,
                "accepted": true,
                "gate": gate,
                "rule_id": rule_id,
                "created": created,
            },
        }));
    }

    // ⑤ 门槛不过：实例仍入库留痕，降级知识分支
    let _ = core
        .lexicon_store
        .upsert_example(lang, &rule_id, wrong, "", "llm_distill");
    let _ = core.lexicon_store.sync_rules_to_file(&lexicons_dir, lang);
    tracing::info!(
        lang,
        rule = %rule_id,
        reason = %gate.reason,
        "anomaly 蒸馏模式未过金集门槛，降级知识分支"
    );
    Ok(anomaly_knowledge_fallback_cmd(
        wrong,
        &error.error_type,
        &rule_id,
        Some((pattern, gate)),
        "蒸馏模式未过金集门槛",
    ))
}

/// anomaly 降级知识分支（Tauri 命令 JSON 形态）
fn anomaly_knowledge_fallback_cmd(
    wrong: &str,
    error_type: &str,
    rule_id: &str,
    distill: Option<(
        gec_zh::rules::AnomalyPattern,
        gec_zh::anomaly_distill::GateReport,
    )>,
    reason: &str,
) -> serde_json::Value {
    let (pattern, gate) = match distill {
        Some((p, g)) => (Some(p), Some(g)),
        None => (None, None),
    };
    serde_json::json!({
        "action": "adopt",
        "via": "anomaly",
        "message": format!(
            "模式蒸馏未沉淀（{reason}），已降级为知识候选；可选择「保留」沉淀到向量知识库"
        ),
        "knowledge": {
            "text": wrong,
            "error_type": error_type,
            "correction": "",
        },
        "anomaly": {
            "pattern": pattern,
            "gate": gate,
            "accepted": false,
            "rule_id": rule_id,
            "reason": reason,
        },
    })
}

/// anomaly 自动采纳（二期飞轮 L1；与 api-server `auto_adopt_anomalies` 同逻辑）：
/// LLM 兜底检出的污染句，置信度 ≥ 阈值时无需用户确认即沉淀到学习知识库
/// （source `llm_adopt`，②.5 向量召回）。全程非阻断。
const ANOMALY_AUTO_ADOPT_CONF: f64 = 0.9;

/// 沉淀粒度守卫（与 api-server `routes/detect.rs` 同名常量/函数保持同步）：
/// 异常沉淀必须是单句，且不过短（子串误伤）/不过长（段级条目在句级召回中永不命中）。
const ANOMALY_DEPOSIT_MIN_CHARS: usize = 12;

const ANOMALY_DEPOSIT_MAX_CHARS: usize = 150;

fn deposit_granularity_ok(original: &str) -> bool {
    gec_core::knowledge_recall::split_sentences(original).len() == 1
        && (ANOMALY_DEPOSIT_MIN_CHARS..=ANOMALY_DEPOSIT_MAX_CHARS)
            .contains(&original.trim().chars().count())
}

pub(crate) async fn auto_adopt_anomalies_cmd(core: &Arc<Core>, lang: &str, errors: &[Error]) {
    let candidates: Vec<&Error> = errors
        .iter()
        .filter(|e| {
            e.error_type == error_types::ANOMALY
                && e.source.as_deref() == Some(shared_model::sources::LLM)
                && e.confidence >= ANOMALY_AUTO_ADOPT_CONF
                && deposit_granularity_ok(&e.original)
        })
        .collect();
    if candidates.is_empty() {
        return;
    }
    let existing = match core.learned_knowledge.list(lang) {
        Ok(items) => items,
        Err(e) => {
            tracing::warn!(error = %e, "anomaly 自动采纳：知识库读取失败，跳过");
            return;
        }
    };
    // guard 不可跨 await（非 Send）——先克隆 Arc 再立即释放
    let engine = {
        let guard = core.embedding_engine.read().unwrap();
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
        match core.learned_knowledge.insert(
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

// ─── L3 特征模型训练飞轮（三期闭环；与 api-server /gec/l3/* 同构） ──────────
//
// - 采集：前端「采纳」LLM 错误（正样本）→ l3_collect；
// - 提醒：l3_status 返回样本数 + 上次重训练时间 + 当前模型 F1（转发
//   feature-model GET /l3/status）+ retrain_ready 建议；
// - 重训练：l3_retrain 转发 feature-model POST /l3-retrain（合并样本 →
//   重训练 → dev 评测 → F1 未显著退化则热切换）。
