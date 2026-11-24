use std::sync::Arc;

use super::core::Core;
use super::eval::eval_endpoint;
use super::l3::l3_retrain_forward;
use super::training::l3_pool_size;
use super::util::sqlite_dt;
/// 测评中心：层级贡献测量（消融 Δ 常态化）
///
/// 拉取测评服务报告列表 → 按（benchmark, split, arm, mode）取最新组 →
/// 各消融臂各拉一份完整报告 → 层级贡献表（每层边际贡献 = Fβ 全开 − 该层关闭）。
/// 报告过滤字段匹配：`want` 为 null 或缺失 = 不过滤（该字段恒匹配）
pub(crate) fn report_field_matches(
    r: &serde_json::Value,
    field: &str,
    want: Option<&serde_json::Value>,
) -> bool {
    match want {
        Some(w) if w.is_null() => true,
        Some(w) => w
            .as_str()
            .and_then(|w| r.get(field).and_then(|v| v.as_str()).map(|s| s == w))
            .unwrap_or(false),
        None => true,
    }
}

#[tauri::command]
pub async fn eval_contributions(body: serde_json::Value) -> Result<serde_json::Value, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("HTTP 客户端构建失败: {e}"))?;
    let resp = client
        .get(format!("{}/eval/reports", eval_endpoint()))
        .send()
        .await
        .map_err(|e| {
            format!("测评服务未运行（{e}；启动：cd datasets && python eval_server.py）")
        })?;
    if !resp.status().is_success() {
        return Err(format!("测评服务 /eval/reports 返回 {}", resp.status()));
    }
    let list: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("测评服务响应解析失败: {e}"))?;
    let reports = list
        .get("reports")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "测评服务响应结构不符".to_string())?;

    // 过滤 + 分组（benchmark, split, arm, mode）
    let matched: Vec<&serde_json::Value> = reports
        .iter()
        .filter(|r| {
            [
                ("benchmark", body.get("benchmark")),
                ("split", body.get("split")),
                ("arm", body.get("arm")),
                ("mode", body.get("mode")),
            ]
            .iter()
            .all(|(field, want)| report_field_matches(r, field, *want))
        })
        .collect();
    if matched.is_empty() {
        return Err("无匹配测评报告：请先在「测评」页发起一次评测（默认参数即可）；若填写了 benchmark / split / arm / mode 过滤条件，请使其与已跑评测的参数一致（留空 = 不过滤，取最新组）".to_string());
    }
    let mut groups: std::collections::HashMap<
        (String, String, String, String),
        Vec<&serde_json::Value>,
    > = std::collections::HashMap::new();
    for r in &matched {
        let key = (
            r.get("benchmark")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            r.get("split")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            r.get("arm")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            r.get("mode")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        );
        groups.entry(key).or_default().push(r);
    }
    let group_ts = |v: &Vec<&serde_json::Value>| {
        v.iter()
            .filter_map(|r| r.get("ts").and_then(|x| x.as_str()).map(|s| s.to_string()))
            .max()
            .unwrap_or_default()
    };
    let (group_key, group) = groups
        .iter()
        .max_by_key(|g| group_ts(g.1))
        .expect("groups 非空");

    // 各消融规格取组内最新一份 → 拉完整报告
    let specs: [Option<gec_core::AblationSpec>; 5] = [
        None,
        Some(gec_core::AblationSpec {
            l3_off: true,
            ..Default::default()
        }),
        Some(gec_core::AblationSpec {
            decision_off: true,
            ..Default::default()
        }),
        Some(gec_core::AblationSpec {
            knowledge_off: true,
            ..Default::default()
        }),
        Some(gec_core::AblationSpec {
            l3_off: true,
            decision_off: true,
            ..Default::default()
        }),
    ];
    let mut summaries = Vec::new();
    for spec in specs {
        let entry = group
            .iter()
            .filter(|r| {
                gec_core::AblationSpec::from_report_value(
                    r.get("ablation").unwrap_or(&serde_json::Value::Null),
                ) == spec
            })
            .max_by(|a, b| {
                a.get("ts")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .cmp(b.get("ts").and_then(|v| v.as_str()).unwrap_or(""))
            })
            .cloned();
        let Some(entry) = entry else { continue };
        let id = entry.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let full: serde_json::Value = match client
            .get(format!("{}/eval/report/{}", eval_endpoint(), id))
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => match r.json().await {
                Ok(v) => v,
                Err(_) => continue,
            },
            _ => continue,
        };
        if let Some(s) = gec_core::parse_report_summary(&full) {
            summaries.push(s);
        }
    }
    let table = gec_core::measure_contributions(&summaries);
    Ok(serde_json::json!({
        "group": {
            "benchmark": group_key.0,
            "split": group_key.1,
            "arm": group_key.2,
            "mode": group_key.3,
        },
        "table": table,
    }))
}

/// 优化轮次：自动评估 → 提案轮次（或「无需行动」）
///
/// 增量窗口 = 上次轮次（任意状态）之后；反馈统计 / 明细按窗口过滤，
/// 飞轮池规模与测评证据为绝对值。
#[tauri::command]
pub async fn round_evaluate(
    core: tauri::State<'_, Arc<Core>>,
    body: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lang = body
        .get("lang")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "lang 缺失或为空".to_string())?;
    let report_id = body
        .get("report_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // 增量窗口边界：上次轮次的创建时间
    let rounds = core
        .optimization_round
        .list_rounds(Some(lang), None, 10)
        .map_err(|e| format!("轮次存储读取失败: {e}"))?;
    let since = rounds.first().map(|r| sqlite_dt(&r.created_at));

    let stats = core
        .feedback_log
        .stats_since(lang, since.as_deref())
        .map_err(|e| format!("反馈统计失败: {e}"))?;
    let entries = core
        .feedback_log
        .list_since(Some(lang), since.as_deref(), 500)
        .map_err(|e| format!("反馈明细读取失败: {e}"))?;
    // 上次 L3 重训练时间（任意状态的最新作业）
    let jobs = core
        .training_center
        .list_jobs(Some(lang), None, 100)
        .map_err(|e| format!("训练作业读取失败: {e}"))?;
    let last_retrain_at = jobs
        .iter()
        .filter(|j| j.kind == gec_core::job_kind::L3_RETRAIN)
        .map(|j| j.updated_at.clone())
        .max();

    let input = gec_core::RoundInput {
        lang: lang.to_string(),
        feedback_stats: stats
            .into_iter()
            .map(|s| gec_core::FeedbackStatIn {
                source: s.source,
                rule_id: s.rule_id,
                adopted: s.adopted,
                rejected: s.rejected,
                last_at: s.last_at,
            })
            .collect(),
        feedback_entries: entries
            .into_iter()
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
            .collect(),
        pool_size: l3_pool_size(&core, lang),
        last_retrain_at,
        // 测评证据（服务不可达 / 无报告 → None，纯反馈判定）
        eval_evidence: gec_core::fetch_eval_evidence(&eval_endpoint(), lang, report_id.as_deref())
            .await,
    };

    match gec_core::plan_round(&input) {
        None => Ok(serde_json::json!({
            "action": "none",
            "reasons": gec_core::no_action_reasons(&input),
        })),
        Some(plan) => {
            let id = core
                .optimization_round
                .insert_round(
                    lang,
                    &plan.reason,
                    &serde_json::to_value(&plan.evidence).unwrap_or_default(),
                    &serde_json::to_value(&plan.actions).unwrap_or_default(),
                    &chrono::Utc::now().to_rfc3339(),
                )
                .map_err(|e| format!("轮次落库失败: {e}"))?;
            let row = core
                .optimization_round
                .get_round(id)
                .map_err(|e| format!("轮次读取失败: {e}"))?
                .ok_or_else(|| "轮次落库后读取失败".to_string())?;
            Ok(serde_json::json!({
                "action": "round_proposed",
                "round": row,
            }))
        }
    }
}

/// 优化轮次：列表（新 → 旧；lang / status 可选过滤）
#[tauri::command]
pub async fn round_list(
    core: tauri::State<'_, Arc<Core>>,
    lang: Option<String>,
    status: Option<String>,
    limit: Option<usize>,
) -> Result<serde_json::Value, String> {
    let limit = limit.unwrap_or(50).min(200);
    let rows = core
        .optimization_round
        .list_rounds(lang.as_deref(), status.as_deref(), limit)
        .map_err(|e| format!("轮次列表查询失败: {e}"))?;
    Ok(serde_json::json!({ "rounds": rows }))
}

/// 优化轮次：详情
#[tauri::command]
pub async fn round_get(
    core: tauri::State<'_, Arc<Core>>,
    id: i64,
) -> Result<serde_json::Value, String> {
    let row = core
        .optimization_round
        .get_round(id)
        .map_err(|e| format!("轮次查询失败: {e}"))?
        .ok_or_else(|| format!("轮次 #{id} 不存在"))?;
    Ok(serde_json::json!({ "round": row }))
}

/// 优化轮次：人工确认门（`proposed` → `confirmed`）
#[tauri::command]
pub async fn round_confirm(
    core: tauri::State<'_, Arc<Core>>,
    id: i64,
) -> Result<serde_json::Value, String> {
    let row = core
        .optimization_round
        .get_round(id)
        .map_err(|e| format!("轮次查询失败: {e}"))?
        .ok_or_else(|| format!("轮次 #{id} 不存在"))?;
    if row.status != "proposed" {
        return Err(format!(
            "轮次 #{id} 当前状态为 {}，只有 proposed 可确认",
            row.status
        ));
    }
    if !core
        .optimization_round
        .transition(id, "confirmed", None, &chrono::Utc::now().to_rfc3339())
        .map_err(|e| format!("轮次状态迁移失败: {e}"))?
    {
        return Err(format!("轮次 #{id} 状态迁移失败"));
    }
    let row = core
        .optimization_round
        .get_round(id)
        .map_err(|e| format!("轮次查询失败: {e}"))?
        .ok_or_else(|| format!("轮次 #{id} 不存在"))?;
    Ok(serde_json::json!({ "round": row }))
}

/// 优化轮次：人工拒绝（`proposed` → `rejected`）
#[tauri::command]
pub async fn round_reject(
    core: tauri::State<'_, Arc<Core>>,
    id: i64,
) -> Result<serde_json::Value, String> {
    let row = core
        .optimization_round
        .get_round(id)
        .map_err(|e| format!("轮次查询失败: {e}"))?
        .ok_or_else(|| format!("轮次 #{id} 不存在"))?;
    if row.status != "proposed" {
        return Err(format!(
            "轮次 #{id} 当前状态为 {}，只有 proposed 可拒绝",
            row.status
        ));
    }
    if !core
        .optimization_round
        .transition(id, "rejected", None, &chrono::Utc::now().to_rfc3339())
        .map_err(|e| format!("轮次状态迁移失败: {e}"))?
    {
        return Err(format!("轮次 #{id} 状态迁移失败"));
    }
    let row = core
        .optimization_round
        .get_round(id)
        .map_err(|e| format!("轮次查询失败: {e}"))?
        .ok_or_else(|| format!("轮次 #{id} 不存在"))?;
    Ok(serde_json::json!({ "round": row }))
}

/// 优化轮次：执行（`confirmed` → `running` → 终态）
///
/// 每个可自动动作（`l3_retrain` / `lexicon_condition` / `llm_data`）派生一个
/// P2 训练作业（走完整状态机）；`rule_tune` 标记 `human_required`（不阻断完成）。
/// 任一自动动作失败 → 轮次 `failed`（各动作结果在 `result.actions`）。
#[tauri::command]
pub async fn round_execute(
    core: tauri::State<'_, Arc<Core>>,
    id: i64,
) -> Result<serde_json::Value, String> {
    let row = core
        .optimization_round
        .get_round(id)
        .map_err(|e| format!("轮次查询失败: {e}"))?
        .ok_or_else(|| format!("轮次 #{id} 不存在"))?;
    if row.status != "confirmed" {
        return Err(format!(
            "轮次 #{id} 当前状态为 {}，只有 confirmed 可执行（先过人工确认门）",
            row.status
        ));
    }
    if !core
        .optimization_round
        .transition(id, "running", None, &chrono::Utc::now().to_rfc3339())
        .map_err(|e| format!("轮次状态迁移失败: {e}"))?
    {
        return Err(format!("轮次 #{id} 状态迁移失败"));
    }

    let actions: Vec<gec_core::RoundAction> =
        serde_json::from_str(&row.actions.to_string()).unwrap_or_default();
    let mut action_results: Vec<serde_json::Value> = Vec::new();
    let mut any_failed = false;

    for a in &actions {
        let result: Result<serde_json::Value, String> = match a.kind.as_str() {
            gec_core::job_kind::L3_RETRAIN => l3_retrain_forward(&core, &row.lang).await,
            gec_core::job_kind::LEXICON_CONDITION => {
                let rule_id = a
                    .payload
                    .get("rule_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "payload 缺 rule_id".to_string())?;
                let words = a
                    .payload
                    .get("originals")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|w| w.as_str().map(|s| s.to_string()))
                            .collect::<Vec<_>>()
                    })
                    .ok_or_else(|| "payload 缺 originals".to_string())?;
                if words.is_empty() {
                    return Err("payload 无可入库的排除文本".to_string());
                }
                let lexicons_dir =
                    std::path::PathBuf::from(core.settings.read().unwrap().lexicons_dir.clone());
                let mut applied = Vec::new();
                let mut skipped = 0;
                for w in &words {
                    match lexicon_store::append_rule_exclude(&lexicons_dir, &row.lang, rule_id, w) {
                        Ok(true) => {
                            applied.push(serde_json::json!({ "rule_id": rule_id, "word": w }))
                        }
                        Ok(false) => skipped += 1,
                        Err(e) => return Err(format!("排除条目入库失败（{w}）: {e}")),
                    }
                }
                Ok(serde_json::json!({ "applied": applied, "skipped": skipped }))
            }
            gec_core::job_kind::LLM_DATA => {
                let entries: Vec<gec_core::FeedbackEntryIn> = core
                    .feedback_log
                    .list(Some(&row.lang), 500)
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
                    &row.lang,
                    a.payload
                        .get("current_arm")
                        .and_then(|v| v.as_str())
                        .unwrap_or(""),
                    &entries,
                );
                Ok(serde_json::json!({
                    "n_samples": data.samples.len(),
                    "jsonl": data.jsonl,
                }))
            }
            "rule_tune" => {
                // 人工操作：不产生训练作业，不阻断轮次
                action_results.push(serde_json::json!({
                    "kind": "rule_tune",
                    "target": a.target,
                    "status": "human_required",
                    "note": "请在规则面板停用 / 收紧该规则",
                }));
                continue;
            }
            _ => return Err(format!("未知动作类型: {}", a.kind)),
        };

        // 派生 P2 训练作业（proposed → confirmed → running → 终态）
        let t0 = chrono::Utc::now().to_rfc3339();
        let job_id = core
            .training_center
            .insert_job(&a.kind, &row.lang, None, &a.payload, &t0)
            .unwrap_or(0);
        if job_id > 0 {
            let _ = core.training_center.transition(
                job_id,
                "confirmed",
                None,
                &chrono::Utc::now().to_rfc3339(),
            );
            let _ = core.training_center.transition(
                job_id,
                "running",
                None,
                &chrono::Utc::now().to_rfc3339(),
            );
        }
        let (job_status, err) = match result {
            Ok(v) => {
                if job_id > 0 {
                    let _ = core.training_center.transition(
                        job_id,
                        "completed",
                        Some(&v),
                        &chrono::Utc::now().to_rfc3339(),
                    );
                }
                ("completed".to_string(), None)
            }
            Err(e) => {
                if job_id > 0 {
                    let _ = core.training_center.transition(
                        job_id,
                        "failed",
                        Some(&serde_json::json!({ "error": e })),
                        &chrono::Utc::now().to_rfc3339(),
                    );
                }
                ("failed".to_string(), Some(e))
            }
        };
        if job_status == "failed" {
            any_failed = true;
        }
        action_results.push(serde_json::json!({
            "kind": a.kind,
            "target": a.target,
            "job_id": job_id,
            "status": job_status,
            "error": err,
        }));
    }

    let (to, result) = if any_failed {
        (
            "failed",
            serde_json::json!({ "actions": action_results, "error": "部分动作失败" }),
        )
    } else {
        (
            "completed",
            serde_json::json!({ "actions": action_results }),
        )
    };
    if !core
        .optimization_round
        .transition(id, to, Some(&result), &chrono::Utc::now().to_rfc3339())
        .map_err(|e| format!("轮次状态迁移失败: {e}"))?
    {
        return Err(format!("轮次 #{id} 状态迁移失败"));
    }
    let row = core
        .optimization_round
        .get_round(id)
        .map_err(|e| format!("轮次查询失败: {e}"))?
        .ok_or_else(|| format!("轮次 #{id} 不存在"))?;
    Ok(serde_json::json!({ "round": row }))
}

/// 优化轮次：报告（聚合视图：背景 + 各动作完整作业结果 + 执行后测评证据）
///
/// 任何状态都可查看：`proposed` / `confirmed` 显示计划；`completed` / `failed`
/// 附各派生作业的完整 `result` 与人类可读 `summary`。
#[tauri::command]
pub async fn round_report(
    core: tauri::State<'_, Arc<Core>>,
    id: i64,
) -> Result<serde_json::Value, String> {
    let row = core
        .optimization_round
        .get_round(id)
        .map_err(|e| format!("轮次查询失败: {e}"))?
        .ok_or_else(|| format!("轮次 #{id} 不存在"))?;

    // 本轮派生的作业（从 result.actions 提取 job_id；未执行 → 空）
    let mut jobs: Vec<serde_json::Value> = Vec::new();
    if let Some(list) = row
        .result
        .as_ref()
        .and_then(|v| v.get("actions"))
        .and_then(|v| v.as_array())
    {
        for a in list {
            if let Some(jid) = a.get("job_id").and_then(|v| v.as_i64())
                && let Some(job) = core
                    .training_center
                    .get_job(jid)
                    .map_err(|e| format!("作业查询失败: {e}"))?
            {
                jobs.push(serde_json::to_value(job).unwrap_or_default());
            }
        }
    }

    // 执行后测评证据（最新报告；不可达 → None）
    let eval_after = gec_core::fetch_eval_evidence(&eval_endpoint(), &row.lang, None).await;

    let report = gec_core::round_report(
        &serde_json::to_value(&row).unwrap_or_default(),
        &jobs,
        eval_after.as_ref(),
    );
    Ok(report)
}
