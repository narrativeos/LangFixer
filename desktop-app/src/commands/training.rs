use std::sync::Arc;

use super::core::Core;
use super::l3::l3_retrain_forward;
use super::util::now_iso;
/// 作业生命周期：`proposed` → `confirmed` → `running` → `completed` / `failed`
fn training_complete_job(
    core: &Core,
    kind: &str,
    lang: &str,
    source_suggestion: Option<i64>,
    payload: &serde_json::Value,
    result: Result<serde_json::Value, String>,
) -> i64 {
    let id =
        match core
            .training_center
            .insert_job(kind, lang, source_suggestion, payload, &now_iso())
        {
            Ok(id) => id,
            Err(e) => {
                tracing::warn!(kind, error = %e, "训练作业落库失败");
                return 0;
            }
        };
    let _ = core
        .training_center
        .transition(id, "confirmed", None, &now_iso());
    let _ = core
        .training_center
        .transition(id, "running", None, &now_iso());
    let (to, res) = match result {
        Ok(v) => ("completed", v),
        Err(e) => ("failed", serde_json::json!({ "error": e })),
    };
    let _ = core
        .training_center
        .transition(id, to, Some(&res), &now_iso());
    id
}

/// 读取飞轮确认样本 → `PoolSample`（label 1=error / 0=clean）
fn load_flywheel_samples(core: &Core, lang: &str) -> Vec<gec_core::PoolSample> {
    let path = core.l3_data.join(lang).join("confirmed.jsonl");
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).ok()?;
            let sentence = match v.get("text").and_then(|t| t.as_str()) {
                Some(s) if !s.trim().is_empty() => s.to_string(),
                _ => return None,
            };
            let label = v.get("label")?.as_u64().unwrap_or(1);
            Some(gec_core::PoolSample {
                sentence,
                label: if label == 1 { "error" } else { "clean" }.to_string(),
                error_type: v
                    .get("error_type")
                    .and_then(|e| e.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string()),
                correction: None,
                source: "flywheel".to_string(),
            })
        })
        .collect()
}

/// 训练中心：领域池构建（模型层反馈 + 飞轮确认样本 + 增强器干净句）
#[tauri::command]
pub async fn train_build_pool(
    core: tauri::State<'_, Arc<Core>>,
    body: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lang = body
        .get("lang")
        .and_then(|v| v.as_str())
        .unwrap_or("zh")
        .to_string();
    if !matches!(lang.as_str(), "en" | "zh") {
        return Err(format!("不支持的语言: {lang}"));
    }
    let clean_sentences: Vec<String> = body
        .get("clean_sentences")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let entries: Vec<gec_core::FeedbackEntryIn> = core
        .feedback_log
        .list(Some(&lang), 500)
        .map_err(|e| format!("反馈明细查询失败: {e}"))?
        .into_iter()
        .filter(|f| gec_core::is_model_source(&f.source))
        .map(|f| gec_core::FeedbackEntryIn {
            source: f.source,
            rule_id: f.rule_id,
            error_type: f.error_type,
            original: f.original,
            correction: f.correction,
            action: f.action,
            reason: f.reason,
            created_at: f.created_at,
        })
        .collect();
    let flywheel = load_flywheel_samples(&core, &lang);
    let input = gec_core::PoolInput {
        lang: lang.clone(),
        feedback: entries,
        flywheel,
        clean_sentences: clean_sentences.clone(),
    };
    let pool = gec_core::build_domain_pool(&input);
    let payload = serde_json::json!({
        "lang": lang,
        "feedback": input.feedback.len(),
        "flywheel": input.flywheel.len(),
        "augment": clean_sentences.len(),
    });
    let job_id = training_complete_job(
        &core,
        gec_core::job_kind::DOMAIN_POOL,
        &lang,
        None,
        &payload,
        Ok(serde_json::to_value(&pool).unwrap_or(serde_json::Value::Null)),
    );
    Ok(serde_json::json!({ "job_id": job_id, "pool": pool }))
}

/// 训练中心：LLM 训练数据构建（LLM 层 adopt 反馈带修正 → SFT JSONL）
#[tauri::command]
pub async fn train_build_llm_data(
    core: tauri::State<'_, Arc<Core>>,
    body: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lang = body
        .get("lang")
        .and_then(|v| v.as_str())
        .unwrap_or("zh")
        .to_string();
    if !matches!(lang.as_str(), "en" | "zh") {
        return Err(format!("不支持的语言: {lang}"));
    }
    let arm = body
        .get("arm")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let entries: Vec<gec_core::FeedbackEntryIn> = core
        .feedback_log
        .list(Some(&lang), 500)
        .map_err(|e| format!("反馈明细查询失败: {e}"))?
        .into_iter()
        .filter(|f| f.source == "llm")
        .map(|f| gec_core::FeedbackEntryIn {
            source: f.source,
            rule_id: f.rule_id,
            error_type: f.error_type,
            original: f.original,
            correction: f.correction,
            action: f.action,
            reason: f.reason,
            created_at: f.created_at,
        })
        .collect();
    let data = gec_core::build_llm_training_data(&lang, &arm, &entries);
    let payload = serde_json::json!({ "lang": lang, "arm": arm });
    let job_id = training_complete_job(
        &core,
        gec_core::job_kind::LLM_DATA,
        &lang,
        None,
        &payload,
        Ok(serde_json::json!({
            "n_samples": data.samples.len(),
            "jsonl": data.jsonl,
        })),
    );
    Ok(serde_json::json!({ "job_id": job_id, "data": data }))
}

/// 训练中心：L3 重训练（转发 feature-model 飞轮；与 `l3_retrain` 同源）
#[tauri::command]
pub async fn train_run_l3_retrain(
    core: tauri::State<'_, Arc<Core>>,
    body: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lang = body
        .get("lang")
        .and_then(|v| v.as_str())
        .unwrap_or("zh")
        .to_string();
    if !matches!(lang.as_str(), "en" | "zh") {
        return Err(format!("不支持的语言: {lang}"));
    }
    let payload = serde_json::json!({ "lang": lang });
    let result: Result<serde_json::Value, String> = l3_retrain_forward(&core, &lang).await;
    let job_id = training_complete_job(
        &core,
        gec_core::job_kind::L3_RETRAIN,
        &lang,
        None,
        &payload,
        result.clone(),
    );
    match result {
        Ok(v) => Ok(serde_json::json!({ "job_id": job_id, "result": v })),
        Err(e) => Err(e),
    }
}

/// 训练中心：词表条件化入库（条件化排除条目；入库前先过测评消融）
#[tauri::command]
pub async fn train_apply_lexicon(
    core: tauri::State<'_, Arc<Core>>,
    body: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lang = body
        .get("lang")
        .and_then(|v| v.as_str())
        .unwrap_or("zh")
        .to_string();
    if !matches!(lang.as_str(), "en" | "zh") {
        return Err(format!("不支持的语言: {lang}"));
    }
    let suggestion_id = body.get("suggestion_id").and_then(|v| v.as_i64());
    let check_ablation = body
        .get("check_ablation")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // 建议 payload 补全 rule_id / words
    let (mut rule_id, mut words) = (
        body.get("rule_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        body.get("words")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|w| w.as_str().map(|s| s.to_string()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
    );
    if let Some(sid) = suggestion_id
        && let Some(row) = core
            .reflection_center
            .get_suggestion(sid)
            .map_err(|e| format!("建议查询失败: {e}"))?
    {
        if rule_id.is_none() {
            rule_id = row
                .payload
                .get("rule_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
        }
        if words.is_empty() {
            words = row
                .payload
                .get("originals")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|w| w.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
        }
    }
    let rule_id = rule_id.ok_or_else(|| "rule_id 缺失（且建议 payload 无 rule_id）".to_string())?;
    if words.is_empty() {
        return Err("无可入库的排除文本".to_string());
    }
    // 测评消融（可选；不可达 / 无报告 → 降级为直接入库）
    let eval_evidence = if check_ablation {
        let endpoint = std::env::var("GEC_EVAL_ENDPOINT")
            .map(|s| s.trim_end_matches('/').to_string())
            .unwrap_or_else(|_| "http://127.0.0.1:4807".to_string());
        gec_core::fetch_eval_evidence(&endpoint, &lang, None).await
    } else {
        None
    };
    // 入库
    let lexicons_dir = std::path::PathBuf::from(core.settings.read().unwrap().lexicons_dir.clone());
    let mut applied = Vec::new();
    let mut skipped = 0;
    for w in &words {
        match lexicon_store::append_rule_exclude(&lexicons_dir, &lang, &rule_id, w) {
            Ok(true) => applied.push(serde_json::json!({ "rule_id": rule_id, "word": w })),
            Ok(false) => skipped += 1,
            Err(e) => return Err(format!("排除条目入库失败（{w}）: {e}")),
        }
    }
    let payload = serde_json::json!({
        "lang": lang,
        "rule_id": rule_id,
        "words": words,
    });
    let result = serde_json::json!({
        "applied": applied,
        "skipped": skipped,
        "eval_evidence": eval_evidence,
    });
    let job_id = training_complete_job(
        &core,
        gec_core::job_kind::LEXICON_CONDITION,
        &lang,
        suggestion_id,
        &payload,
        Ok(result),
    );
    let suggestion_consumed = match suggestion_id {
        Some(sid) if job_id > 0 => core
            .reflection_center
            .update_suggestion_status(sid, "consumed")
            .map_err(|e| format!("建议状态更新失败: {e}"))?,
        _ => false,
    };
    Ok(serde_json::json!({
        "job_id": job_id,
        "applied": applied,
        "skipped": skipped,
        "eval_evidence": eval_evidence,
        "suggestion_consumed": suggestion_consumed,
    }))
}

/// 训练中心：**P1 → P2 消费入口**——反思建议 → 分派训练动作 → 建议 consumed
///
/// - `l3_sample` → L3 重训练（feature-model 不可达 → 作业 failed，建议保持 pending）
/// - `lexicon_condition` → 词表条件化入库
/// - `llm_arm` → LLM 训练数据构建
/// - `rule_tune` → 错误（人工操作，不产生训练作业）
#[tauri::command]
pub async fn train_run_suggestion(
    core: tauri::State<'_, Arc<Core>>,
    body: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let id = body
        .get("suggestion_id")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| "suggestion_id 缺失".to_string())?;
    let row = core
        .reflection_center
        .get_suggestion(id)
        .map_err(|e| format!("建议查询失败: {e}"))?
        .ok_or_else(|| format!("建议 #{id} 不存在"))?;
    let suggestion = gec_core::reflection_center::ReflectionSuggestion {
        kind: row.kind.clone(),
        target: row.target.clone(),
        summary: row.summary.clone(),
        evidence: row.evidence.clone(),
        payload: row.payload.clone(),
        priority: row.priority as u8,
    };
    let plan = gec_core::plan_from_suggestion(&suggestion)
        .ok_or_else(|| format!("建议类型 {} 不是训练作业（人工操作）", row.kind))?;
    let lang = plan.lang.clone();

    let (job_kind, result): (&str, Result<serde_json::Value, String>) = match plan.kind.as_str() {
        gec_core::job_kind::L3_RETRAIN => {
            let r: Result<serde_json::Value, String> = l3_retrain_forward(&core, &lang).await;
            (gec_core::job_kind::L3_RETRAIN, r)
        }
        gec_core::job_kind::LEXICON_CONDITION => {
            let rule_id = plan
                .payload
                .get("rule_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "建议 payload 缺 rule_id".to_string())?;
            let words = plan
                .payload
                .get("originals")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|w| w.as_str().map(|s| s.to_string()))
                        .collect::<Vec<_>>()
                })
                .ok_or_else(|| "建议 payload 缺 originals".to_string())?;
            if words.is_empty() {
                return Err("建议无可入库的排除文本".to_string());
            }
            let lexicons_dir =
                std::path::PathBuf::from(core.settings.read().unwrap().lexicons_dir.clone());
            let mut applied = Vec::new();
            let mut skipped = 0;
            for w in &words {
                match lexicon_store::append_rule_exclude(&lexicons_dir, &lang, rule_id, w) {
                    Ok(true) => applied.push(serde_json::json!({ "rule_id": rule_id, "word": w })),
                    Ok(false) => skipped += 1,
                    Err(e) => return Err(format!("排除条目入库失败（{w}）: {e}")),
                }
            }
            (
                gec_core::job_kind::LEXICON_CONDITION,
                Ok(serde_json::json!({ "applied": applied, "skipped": skipped })),
            )
        }
        gec_core::job_kind::LLM_DATA => {
            let entries: Vec<gec_core::FeedbackEntryIn> = core
                .feedback_log
                .list(Some(&lang), 500)
                .map_err(|e| format!("反馈明细查询失败: {e}"))?
                .into_iter()
                .filter(|f| f.source == "llm")
                .map(|f| gec_core::FeedbackEntryIn {
                    source: f.source,
                    rule_id: f.rule_id,
                    error_type: f.error_type,
                    original: f.original,
                    correction: f.correction,
                    action: f.action,
                    reason: f.reason,
                    created_at: f.created_at,
                })
                .collect();
            let data = gec_core::build_llm_training_data(
                &lang,
                plan.payload
                    .get("current_arm")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
                &entries,
            );
            (
                gec_core::job_kind::LLM_DATA,
                Ok(serde_json::json!({
                    "n_samples": data.samples.len(),
                    "jsonl": data.jsonl,
                })),
            )
        }
        _ => return Err(format!("未知作业类型: {}", plan.kind)),
    };

    let job_id = training_complete_job(
        &core,
        job_kind,
        &lang,
        Some(id),
        &plan.payload,
        result.clone(),
    );
    match result {
        // 失败：作业已标 failed，建议保持 pending（可重试）
        Err(e) => Err(e),
        Ok(v) => {
            let suggestion_consumed = if job_id > 0 {
                core.reflection_center
                    .update_suggestion_status(id, "consumed")
                    .map_err(|e| format!("建议状态更新失败: {e}"))?
            } else {
                false
            };
            Ok(serde_json::json!({
                "job_id": job_id,
                "job_kind": job_kind,
                "result": v,
                "suggestion_consumed": suggestion_consumed,
            }))
        }
    }
}

/// 训练中心：作业列表（新 → 旧；lang / status 可选过滤）
#[tauri::command]
pub async fn train_list_jobs(
    core: tauri::State<'_, Arc<Core>>,
    lang: Option<String>,
    status: Option<String>,
    limit: Option<usize>,
) -> Result<serde_json::Value, String> {
    let limit = limit.unwrap_or(50).min(200);
    let jobs = core
        .training_center
        .list_jobs(lang.as_deref(), status.as_deref(), limit)
        .map_err(|e| format!("作业列表查询失败: {e}"))?;
    Ok(serde_json::json!({ "jobs": jobs }))
}

/// 训练中心：作业详情
#[tauri::command]
pub async fn train_get_job(
    core: tauri::State<'_, Arc<Core>>,
    id: i64,
) -> Result<serde_json::Value, String> {
    let job = core
        .training_center
        .get_job(id)
        .map_err(|e| format!("作业查询失败: {e}"))?
        .ok_or_else(|| format!("作业 #{id} 不存在"))?;
    Ok(serde_json::json!({ "job": job }))
}

/// 飞轮确认池规模（`l3_data/<lang>/confirmed.jsonl` 非空行数）
pub(crate) fn l3_pool_size(core: &Core, lang: &str) -> i64 {
    let path = core.l3_data.join(lang).join("confirmed.jsonl");
    std::fs::read_to_string(path)
        .map(|c| c.lines().filter(|l| !l.trim().is_empty()).count() as i64)
        .unwrap_or(0)
}
