use std::sync::Arc;

use super::core::Core;
/// 反思中心：生成反思报告（反馈聚合 + 测评证据 → 报告 + 建议队列落库）
#[tauri::command]
pub async fn run_reflect(
    core: tauri::State<'_, Arc<Core>>,
    body: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lang = body
        .get("lang")
        .and_then(|v| v.as_str())
        .unwrap_or("zh")
        .to_string();
    let include_eval = body
        .get("include_eval")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let eval_report_id = body
        .get("eval_report_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let stats: Vec<gec_core::FeedbackStatIn> = core
        .feedback_log
        .stats(&lang)
        .map_err(|e| format!("反馈统计查询失败: {e}"))?
        .into_iter()
        .map(|s| gec_core::FeedbackStatIn {
            source: s.source,
            rule_id: s.rule_id,
            adopted: s.adopted,
            rejected: s.rejected,
            last_at: s.last_at,
        })
        .collect();
    let recent: Vec<gec_core::FeedbackEntryIn> = core
        .feedback_log
        .list(Some(&lang), 50)
        .map_err(|e| format!("反馈明细查询失败: {e}"))?
        .into_iter()
        .map(|e| gec_core::FeedbackEntryIn {
            source: e.source,
            rule_id: e.rule_id,
            error_type: e.error_type,
            original: e.original,
            correction: e.correction,
            action: e.action,
            reason: e.reason,
            created_at: e.created_at,
        })
        .collect();

    // 测评证据（可选；服务不可达 / 无匹配报告 → 降级为纯反馈数据）
    let mut eval = if include_eval {
        let endpoint = std::env::var("GEC_EVAL_ENDPOINT")
            .map(|s| s.trim_end_matches('/').to_string())
            .unwrap_or_else(|_| "http://127.0.0.1:4807".to_string());
        gec_core::fetch_eval_evidence(&endpoint, &lang, eval_report_id.as_deref()).await
    } else {
        None
    };
    if include_eval && eval.is_none() {
        eval = Some(gec_core::EvalEvidence::default());
    }

    let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    // L3 重训练状态（「重训无效」检查）：与 api-server 反思路由同源
    let l3_retrain = {
        let mut st = gec_core::reflection_center::L3RetrainStateIn::default();
        if let Ok(jobs) = core
            .training_center
            .list_jobs(Some(&lang), Some("completed"), 50)
            && let Some(job) = jobs
                .into_iter()
                .find(|j| j.kind == gec_core::job_kind::L3_RETRAIN)
        {
            st.last_job_report_id = job
                .payload
                .get("report_id")
                .and_then(|v| v.as_str())
                .map(String::from);
            st.f1_before = job.payload.get("gate_f1").and_then(|v| v.as_f64());
        }
        st.f1_after = eval.as_ref().and_then(|e| {
            e.layers
                .iter()
                .find(|l| l.name == "feature")
                .and_then(|l| l.gate_f1)
        });
        st
    };
    let mut report = gec_core::build_report(&lang, &created_at, &stats, &recent, eval, &l3_retrain);
    if include_eval
        && report
            .eval_evidence
            .as_ref()
            .is_some_and(|e| e.report_id.is_empty())
    {
        report
            .findings
            .push("测评证据不可用（测评服务未运行或无匹配报告）——本报告仅基于反馈数据".to_string());
    }
    // 已停用规则不再「建议停用」（反馈是历史数据；规则已停用，建议无意义）
    if let Ok(defs) = core.lexicon_store.rule_defs(&lang) {
        let disabled: std::collections::HashSet<&str> = defs
            .iter()
            .filter(|d| !d.enabled)
            .map(|d| d.id.as_str())
            .collect();
        if !disabled.is_empty() {
            let before = report.suggestions.len();
            report.suggestions.retain(|s| {
                !(s.kind == gec_core::reflection_center::suggestion_kinds::RULE_TUNE
                    && disabled.contains(s.target.as_str()))
            });
            let dropped = before - report.suggestions.len();
            if dropped > 0 {
                report.findings.push(format!(
                    "已剔除 {dropped} 条「停用规则」建议——目标规则当前已停用（反馈为历史数据）"
                ));
            }
        }
    }

    let report_json = serde_json::to_string(&report).map_err(|e| e.to_string())?;
    let seeds: Vec<lexicon_store::SuggestionSeed> = report
        .suggestions
        .iter()
        .map(|s| lexicon_store::SuggestionSeed {
            kind: s.kind.clone(),
            target: s.target.clone(),
            summary: s.summary.clone(),
            evidence: s.evidence.clone(),
            payload: s.payload.clone(),
            priority: s.priority,
        })
        .collect();
    let (id, suggestion_ids) = core
        .reflection_center
        .save_report(&lang, &created_at, &report_json, &seeds)
        .map_err(|e| format!("反思报告保存失败: {e}"))?;
    Ok(serde_json::json!({
        "id": id,
        "report": report,
        "suggestion_ids": suggestion_ids,
    }))
}

/// 反思中心：报告历史（新 → 旧）
#[tauri::command]
pub fn list_reflect_reports(
    core: tauri::State<'_, Arc<Core>>,
    lang: Option<String>,
    limit: Option<usize>,
) -> Result<serde_json::Value, String> {
    let reports = core
        .reflection_center
        .list_reports(lang.as_deref(), limit.unwrap_or(20))
        .map_err(|e| format!("反思报告列表查询失败: {e}"))?;
    Ok(serde_json::json!({ "reports": reports }))
}

/// 反思中心：报告详情
#[tauri::command]
pub fn get_reflect_report(
    core: tauri::State<'_, Arc<Core>>,
    id: i64,
) -> Result<serde_json::Value, String> {
    let full = core
        .reflection_center
        .get_report(id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("反思报告 #{id} 不存在"))?;
    serde_json::to_value(full).map_err(|e| e.to_string())
}

/// 反思中心：建议队列（按状态 / 语言过滤）
#[tauri::command]
pub fn list_reflect_suggestions(
    core: tauri::State<'_, Arc<Core>>,
    status: Option<String>,
    lang: Option<String>,
    limit: Option<usize>,
) -> Result<serde_json::Value, String> {
    let suggestions = core
        .reflection_center
        .list_suggestions(status.as_deref(), lang.as_deref(), limit.unwrap_or(50))
        .map_err(|e| format!("建议队列查询失败: {e}"))?;
    Ok(serde_json::json!({ "suggestions": suggestions }))
}

/// 反思中心：更新建议状态（P2 消费入口：pending / applied / consumed / dismissed）
#[tauri::command]
pub fn update_reflect_suggestion(
    core: tauri::State<'_, Arc<Core>>,
    id: i64,
    status: String,
) -> Result<serde_json::Value, String> {
    if !lexicon_store::reflection_center::suggestion_status::ALL.contains(&status.as_str()) {
        return Err(format!(
            "无效状态: {status}（应为 {}）",
            lexicon_store::reflection_center::suggestion_status::ALL.join(" / ")
        ));
    }
    if !core
        .reflection_center
        .update_suggestion_status(id, &status)
        .map_err(|e| e.to_string())?
    {
        return Err(format!("建议 #{id} 不存在"));
    }
    Ok(serde_json::json!({ "id": id, "status": status }))
}

// ─────────────────────────────────────────────────────────────────────────────
// 训练中心（进化中心 P2；镜像 api-server `/gec/train/*`）
// ─────────────────────────────────────────────────────────────────────────────
