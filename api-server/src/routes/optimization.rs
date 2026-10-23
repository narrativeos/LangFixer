//! 优化中心（进化中心 P3）：层级贡献测量 + 优化轮次
//!
//! - `POST /gec/eval/contributions` — **层级贡献测量**（消融 Δ 常态化）：
//!   从测评服务拉取报告列表，按（benchmark, split, arm, mode）取最新组，
//!   各消融臂各拉一份完整报告 → 层级贡献表（每层边际贡献 = Fβ 全开 − 该层关闭）
//! - `POST /gec/round/evaluate` — **自动评估**：反馈增量（上次轮次之后）+
//!   飞轮池规模 + 上次重训练时间 + 测评证据 → 阈值判定 →
//!   提案轮次（`proposed`）或「无需行动」（含各指标离阈值的差距）
//! - `GET /gec/rounds` / `GET /gec/round/:id` — 轮次列表 / 详情
//! - `POST /gec/round/:id/confirm` / `reject` — **人工确认门**
//!   （`proposed` → `confirmed` / `rejected`；P2 作业自动确认，轮次必须人工过门）
//! - `POST /gec/round/:id/execute` — 执行轮次动作（`confirmed` → `running` →
//!   `completed` / `failed`）：可自动动作（l3_retrain / lexicon_condition / llm_data）
//!   各派生一个 P2 训练作业走完整状态机；`rule_tune` 标记 `human_required`
//!   （人工在规则面板操作，不阻断轮次完成）

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::routes::ApiError;
use crate::routes::training_center::ActionError;
use crate::state::AppState;

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// 测评服务地址（`GEC_EVAL_ENDPOINT` 覆盖；缺省 127.0.0.1:4807）
fn eval_endpoint() -> String {
    std::env::var("GEC_EVAL_ENDPOINT")
        .map(|s| s.trim_end_matches('/').to_string())
        .unwrap_or_else(|_| "http://127.0.0.1:4807".to_string())
}

fn err_response(status: StatusCode, msg: String) -> Response {
    (status, Json(serde_json::json!({ "error": msg }))).into_response()
}

/// RFC3339 → SQLite `datetime('now')` 格式（`YYYY-MM-DD HH:MM:SS`）
///
/// `feedback_log.created_at` 用 `datetime('now')` 写入，而轮次 / 作业的时间戳
/// 是 RFC3339——字符串比较要求同格式，故增量窗口边界在此归一化。
fn sqlite_dt(iso: &str) -> String {
    match iso.split_once('T') {
        Some((d, t)) => format!("{} {}", d, &t[..t.len().min(8)]),
        None => iso.to_string(),
    }
}

/// 飞轮确认池规模（`l3_data/<lang>/confirmed.jsonl` 非空行数）
fn pool_size(lang: &str) -> i64 {
    let path = crate::routes::l3::l3_data_dir()
        .join(lang)
        .join("confirmed.jsonl");
    std::fs::read_to_string(path)
        .map(|c| c.lines().filter(|l| !l.trim().is_empty()).count() as i64)
        .unwrap_or(0)
}

// ─────────────────────────────────────────────────────────────────────────────
// 层级贡献测量（消融 Δ 常态化）
// ─────────────────────────────────────────────────────────────────────────────

/// POST /gec/eval/contributions 请求（全部可选；缺省 = 取最新报告组）
#[derive(Deserialize)]
pub struct ContributionsRequest {
    #[serde(default)]
    pub benchmark: Option<String>,
    #[serde(default)]
    pub split: Option<String>,
    #[serde(default)]
    pub arm: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
}

/// 报告组内最新 `ts`（字符串序）
fn group_ts(v: &Vec<&serde_json::Value>) -> String {
    v.iter()
        .filter_map(|r| r.get("ts").and_then(|x| x.as_str()).map(|s| s.to_string()))
        .max()
        .unwrap_or_default()
}

/// POST /gec/eval/contributions — 层级贡献表
///
/// 测评服务不可达 → 503；无匹配报告 → 400。
pub async fn contributions(
    State(_state): State<Arc<AppState>>,
    Json(req): Json<ContributionsRequest>,
) -> Response {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
    {
        Ok(c) => c,
        Err(e) => return err_response(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let resp = match client
        .get(format!("{}/eval/reports", eval_endpoint()))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return err_response(
                StatusCode::SERVICE_UNAVAILABLE,
                format!(
                    "测评服务未运行（{}；启动：cd datasets && python eval_server.py）",
                    e
                ),
            );
        }
    };
    if !resp.status().is_success() {
        return err_response(
            StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
            format!("测评服务 /eval/reports 返回 {}", resp.status()),
        );
    }
    let list: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => return err_response(StatusCode::BAD_GATEWAY, e.to_string()),
    };
    let reports = match list.get("reports").and_then(|v| v.as_array()) {
        Some(a) => a,
        None => return err_response(StatusCode::BAD_GATEWAY, "测评服务响应结构不符".into()),
    };

    // 过滤
    let matched: Vec<&serde_json::Value> = reports
        .iter()
        .filter(|r| {
            let matches = |field: &str, want: &Option<String>| match want {
                Some(w) => r
                    .get(field)
                    .and_then(|v| v.as_str())
                    .map(|s| s == w.as_str())
                    .unwrap_or(false),
                None => true,
            };
            matches("benchmark", &req.benchmark)
                && matches("split", &req.split)
                && matches("arm", &req.arm)
                && matches("mode", &req.mode)
        })
        .collect();
    if matched.is_empty() {
        return err_response(
            StatusCode::BAD_REQUEST,
            "无匹配测评报告：请先在「测评」页发起一次评测（默认参数即可）；若填写了 benchmark / split / arm / mode 过滤条件，请使其与已跑评测的参数一致（留空 = 不过滤，取最新组）"
                .to_string(),
        );
    }

    // 按（benchmark, split, arm, mode）分组，取最新组
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
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "group": {
                "benchmark": group_key.0,
                "split": group_key.1,
                "arm": group_key.2,
                "mode": group_key.3,
            },
            "table": table,
        })),
    )
        .into_response()
}

// ─────────────────────────────────────────────────────────────────────────────
// 优化轮次（自动评估 → 人工确认门 → 执行）
// ─────────────────────────────────────────────────────────────────────────────

/// POST /gec/round/evaluate 请求
#[derive(Deserialize)]
pub struct EvaluateRequest {
    pub lang: String,
    /// 显式指定测评报告 id（`None` = 该语言最近一份匹配报告）
    #[serde(default)]
    pub report_id: Option<String>,
}

/// POST /gec/round/evaluate — 自动评估 → 提案轮次（或「无需行动」）
///
/// 增量窗口 = 上次轮次（任意状态）之后；反馈统计 / 明细按窗口过滤，
/// 飞轮池规模与测评证据为绝对值。
pub async fn evaluate(
    State(state): State<Arc<AppState>>,
    Json(req): Json<EvaluateRequest>,
) -> Response {
    let lang = req.lang.trim();
    if lang.is_empty() {
        return err_response(StatusCode::BAD_REQUEST, "lang 不能为空".into());
    }
    // 增量窗口边界：上次轮次的创建时间
    let rounds = match state.optimization_round.list_rounds(Some(lang), None, 10) {
        Ok(r) => r,
        Err(e) => {
            return err_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("轮次存储读取失败: {e}"),
            );
        }
    };
    let since = rounds.first().map(|r| sqlite_dt(&r.created_at));

    let stats = match state.feedback_log.stats_since(lang, since.as_deref()) {
        Ok(s) => s,
        Err(e) => {
            return err_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("反馈统计失败: {e}"),
            );
        }
    };
    let entries = match state
        .feedback_log
        .list_since(Some(lang), since.as_deref(), 500)
    {
        Ok(l) => l,
        Err(e) => {
            return err_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("反馈明细读取失败: {e}"),
            );
        }
    };
    // 上次 L3 重训练时间（任意状态的最新作业）
    let jobs = match state.training_center.list_jobs(Some(lang), None, 100) {
        Ok(j) => j,
        Err(e) => {
            return err_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("训练作业读取失败: {e}"),
            );
        }
    };
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
        pool_size: pool_size(lang),
        last_retrain_at,
        // 测评证据（服务不可达 / 无报告 → None，纯反馈判定）
        eval_evidence: gec_core::fetch_eval_evidence(
            &eval_endpoint(),
            lang,
            req.report_id.as_deref(),
        )
        .await,
    };

    match gec_core::plan_round(&input) {
        None => (
            StatusCode::OK,
            Json(serde_json::json!({
                "action": "none",
                "reasons": gec_core::no_action_reasons(&input),
            })),
        )
            .into_response(),
        Some(plan) => {
            let id = match state.optimization_round.insert_round(
                lang,
                &plan.reason,
                &serde_json::to_value(&plan.evidence).unwrap_or_default(),
                &serde_json::to_value(&plan.actions).unwrap_or_default(),
                &now(),
            ) {
                Ok(id) => id,
                Err(e) => {
                    return err_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("轮次落库失败: {e}"),
                    );
                }
            };
            let row = match state.optimization_round.get_round(id) {
                Ok(Some(r)) => r,
                _ => {
                    return err_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "轮次落库后读取失败".into(),
                    );
                }
            };
            (
                StatusCode::CREATED,
                Json(serde_json::json!({
                    "action": "round_proposed",
                    "round": row,
                })),
            )
                .into_response()
        }
    }
}

/// GET /gec/rounds 查询参数
#[derive(Deserialize)]
pub struct RoundsQuery {
    #[serde(default)]
    pub lang: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// GET /gec/rounds — 轮次列表（新 → 旧）
pub async fn rounds(
    State(state): State<Arc<AppState>>,
    Query(q): Query<RoundsQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let rows = state
        .optimization_round
        .list_rounds(
            q.lang.as_deref(),
            q.status.as_deref(),
            q.limit.unwrap_or(50).clamp(1, 200),
        )
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(serde_json::json!({ "rounds": rows })))
}

/// GET /gec/round/:id — 轮次详情
pub async fn round(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let row = state
        .optimization_round
        .get_round(id)
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::NotFound(format!("轮次 #{id} 不存在")))?;
    Ok(Json(serde_json::json!({ "round": row })))
}

/// POST /gec/round/:id/confirm — 人工确认门（`proposed` → `confirmed`）
pub async fn confirm_round(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let row = state
        .optimization_round
        .get_round(id)
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::NotFound(format!("轮次 #{id} 不存在")))?;
    if row.status != "proposed" {
        return Err(ApiError::BadRequest(format!(
            "轮次 #{id} 当前状态为 {0}，只有 proposed 可确认",
            row.status
        )));
    }
    if !state
        .optimization_round
        .transition(id, "confirmed", None, &now())
        .map_err(|e| ApiError::Internal(e.to_string()))?
    {
        return Err(ApiError::Internal(format!("轮次 #{id} 状态迁移失败")));
    }
    let row = state
        .optimization_round
        .get_round(id)
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::NotFound(format!("轮次 #{id} 不存在")))?;
    Ok(Json(serde_json::json!({ "round": row })))
}

/// POST /gec/round/:id/reject — 人工拒绝（`proposed` → `rejected`）
pub async fn reject_round(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let row = state
        .optimization_round
        .get_round(id)
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::NotFound(format!("轮次 #{id} 不存在")))?;
    if row.status != "proposed" {
        return Err(ApiError::BadRequest(format!(
            "轮次 #{id} 当前状态为 {0}，只有 proposed 可拒绝",
            row.status
        )));
    }
    if !state
        .optimization_round
        .transition(id, "rejected", None, &now())
        .map_err(|e| ApiError::Internal(e.to_string()))?
    {
        return Err(ApiError::Internal(format!("轮次 #{id} 状态迁移失败")));
    }
    let row = state
        .optimization_round
        .get_round(id)
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::NotFound(format!("轮次 #{id} 不存在")))?;
    Ok(Json(serde_json::json!({ "round": row })))
}

/// POST /gec/round/:id/execute — 执行轮次动作（`confirmed` → `running` → 终态）
///
/// 每个可自动动作（`l3_retrain` / `lexicon_condition` / `llm_data`）派生一个
/// P2 训练作业（走完整状态机）；`rule_tune` 标记 `human_required`（不阻断完成）。
/// 任一自动动作失败 → 轮次 `failed`（各动作结果在 `result.actions`）。
pub async fn execute_round(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let row = state
        .optimization_round
        .get_round(id)
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::NotFound(format!("轮次 #{id} 不存在")))?;
    if row.status != "confirmed" {
        return Err(ApiError::BadRequest(format!(
            "轮次 #{id} 当前状态为 {0}，只有 confirmed 可执行（先过人工确认门）",
            row.status
        )));
    }
    if !state
        .optimization_round
        .transition(id, "running", None, &now())
        .map_err(|e| ApiError::Internal(e.to_string()))?
    {
        return Err(ApiError::Internal(format!("轮次 #{id} 状态迁移失败")));
    }

    let actions: Vec<gec_core::RoundAction> =
        serde_json::from_str(&row.actions.to_string()).unwrap_or_default();
    let mut action_results: Vec<serde_json::Value> = Vec::new();
    let mut any_failed = false;

    for a in &actions {
        match a.kind.as_str() {
            gec_core::job_kind::L3_RETRAIN
            | gec_core::job_kind::LEXICON_CONDITION
            | gec_core::job_kind::LLM_DATA => {
                // 派生 P2 训练作业（proposed → confirmed → running → 终态）
                let t0 = now();
                let job_id = state
                    .training_center
                    .insert_job(&a.kind, &row.lang, None, &a.payload, &t0)
                    .unwrap_or(0);
                if job_id > 0 {
                    let _ = state
                        .training_center
                        .transition(job_id, "confirmed", None, &now());
                    let _ = state
                        .training_center
                        .transition(job_id, "running", None, &now());
                }
                let r = crate::routes::training_center::execute_training_action(
                    &state, &a.kind, &a.payload, &row.lang,
                )
                .await;
                let (job_status, err) = match &r {
                    Ok((_, result)) => {
                        if job_id > 0 {
                            let _ = state.training_center.transition(
                                job_id,
                                "completed",
                                Some(result),
                                &now(),
                            );
                        }
                        ("completed".to_string(), None)
                    }
                    Err(e) => {
                        let msg = match e {
                            ActionError::BadParam(m)
                            | ActionError::Execution(m)
                            | ActionError::Unknown(m) => m.clone(),
                        };
                        if job_id > 0 {
                            let _ = state.training_center.transition(
                                job_id,
                                "failed",
                                Some(&serde_json::json!({ "error": msg })),
                                &now(),
                            );
                        }
                        ("failed".to_string(), Some(msg))
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
            "rule_tune" => {
                // 人工操作：不产生训练作业，不阻断轮次
                action_results.push(serde_json::json!({
                    "kind": "rule_tune",
                    "target": a.target,
                    "status": "human_required",
                    "note": "请在规则面板停用 / 收紧该规则",
                }));
            }
            _ => {
                any_failed = true;
                action_results.push(serde_json::json!({
                    "kind": a.kind,
                    "status": "failed",
                    "error": format!("未知动作类型: {}", a.kind),
                }));
            }
        }
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
    if !state
        .optimization_round
        .transition(id, to, Some(&result), &now())
        .map_err(|e| ApiError::Internal(e.to_string()))?
    {
        return Err(ApiError::Internal(format!("轮次 #{id} 状态迁移失败")));
    }
    let row = state
        .optimization_round
        .get_round(id)
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::NotFound(format!("轮次 #{id} 不存在")))?;
    Ok(Json(serde_json::json!({ "round": row })))
}

/// 轮次报告（聚合视图：背景 + 各动作完整作业结果 + 执行后测评证据）
///
/// `GET /gec/round/:id/report`
///
/// 任何状态都可查看：`proposed` / `confirmed` 显示计划；`completed` / `failed`
/// 附各派生作业的完整 `result`（池规模 / SFT 样本数 / 入库条目 / L3 指标）
/// 与人类可读 `summary`。执行后该语言的最新测评报告作为 `eval_after`
/// （测评服务不可达 → `null`，优雅降级）。
pub async fn round_report(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let row = state
        .optimization_round
        .get_round(id)
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::NotFound(format!("轮次 #{id} 不存在")))?;

    // 本轮派生的作业（从 result.actions 提取 job_id；未执行 → 空）
    let mut jobs: Vec<serde_json::Value> = Vec::new();
    if let Some(list) = row
        .result
        .as_ref()
        .and_then(|v| v.get("actions"))
        .and_then(|v| v.as_array())
    {
        for a in list {
            if let Some(jid) = a.get("job_id").and_then(|v| v.as_i64()) {
                if let Some(job) = state
                    .training_center
                    .get_job(jid)
                    .map_err(|e| ApiError::Internal(e.to_string()))?
                {
                    jobs.push(serde_json::to_value(job).unwrap_or_default());
                }
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
    Ok(Json(report))
}
