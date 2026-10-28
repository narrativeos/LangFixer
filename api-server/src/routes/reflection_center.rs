//! 反思中心（进化中心 P1）：反馈聚合 + 测评证据 → 反思报告 → 建议队列
//!
//! - `POST /gec/reflect/run` — 生成反思报告：聚合 `feedback_log`（按来源层 /
//!   规则 id 的采纳 / 放弃统计 + 最近明细）与测评中心最近报告（层级归因 /
//!   漏斗；`include_eval=false` 或测评服务不可达时退化为纯反馈数据）→
//!   `gec_core::reflection_center::build_report` → 报告 + 建议队列落
//!   `ReflectionCenterStore`（SQLite）
//! - `GET /gec/reflect/reports` — 反思报告历史（新 → 旧）
//! - `GET /gec/reflect/report/:id` — 报告详情（完整 JSON 产出）
//! - `GET /gec/reflect/suggestions` — 建议队列（按状态 / 语言过滤）
//! - `PUT /gec/reflect/suggestion/:id` — 更新建议状态
//!   （`pending` / `applied` / `consumed` / `dismissed`；P2 训练中心消费入口）
//!
//! 建议类型（`gec_core::reflection_center::suggestion_kinds`）：
//! `lexicon_condition`（词表条件化）/ `rule_tune`（规则调参）/
//! `l3_sample`（L3 样本 / 重训练）/ `llm_arm`（LLM 换臂 / 训练数据）。
//! 请求可带 `gold`（金集闭卷证据，评测 harness 产出）：金集里**具体哪几条没抓到**
//! 会按靶子分类落成建议（规则触发了没 credited → `rule_tune`；局部可形式化 →
//! `lexicon_condition`；结构性差错 → `l3_sample`；编辑判断 → `llm_arm`），
//! 每条都带 `gold_nos` 能回到专家表条目。只补充，不改反馈侧判定。
//! 闭环定位见 `EVOLUTION.md`：测评发现弱层 → 反思聚成建议 → 训练消费 →
//! 测评 A/B 验证。

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use serde::{Deserialize, Serialize};

use crate::routes::ApiError;
use crate::state::AppState;

/// 测评服务地址（`GEC_EVAL_ENDPOINT` 覆盖；缺省 127.0.0.1:4807；
/// 与 `routes::eval` 同语义——反思中心只是其证据消费方）
fn eval_endpoint() -> String {
    std::env::var("GEC_EVAL_ENDPOINT")
        .map(|s| s.trim_end_matches('/').to_string())
        .unwrap_or_else(|_| "http://127.0.0.1:4807".to_string())
}

/// POST /gec/reflect/run 请求
#[derive(Deserialize)]
pub struct ReflectRunRequest {
    pub lang: String,
    /// 是否拉取测评证据（默认 true；测评服务不可达时静默退化为纯反馈数据）
    #[serde(default = "default_true")]
    pub include_eval: bool,
    /// 显式指定测评报告 id（缺省 = 该语言最近一份匹配基准的报告）
    #[serde(default)]
    pub eval_report_id: Option<String>,
    /// 金集闭卷证据（可选；由评测 harness 产出，如 `media_gold@…`）
    ///
    /// 与测评服务的「整体 Fβ」互补：金集给的是**具体哪几条没抓到**，
    /// 只有具体条目才能变成可执行靶子（补条件 / 喂样本 / 开 LLM）。
    /// 只补充建议，不改反馈侧判定（见 `gec_core::build_report_with_gold`）。
    #[serde(default)]
    pub gold: Option<gec_core::GoldEvidenceIn>,
}

fn default_true() -> bool {
    true
}

/// POST /gec/reflect/run 响应
#[derive(Serialize)]
pub struct ReflectRunResponse {
    /// 报告 id
    pub id: i64,
    /// 反思报告（findings + 证据 + 建议）
    pub report: gec_core::ReflectionCenterReport,
    /// 建议队列 id（与 report.suggestions 一一对应）
    pub suggestion_ids: Vec<i64>,
}

/// 生成反思报告：聚合反馈 + 测评证据 → 落库 → 返回
pub async fn run(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ReflectRunRequest>,
) -> Result<Json<ReflectRunResponse>, ApiError> {
    let stats: Vec<gec_core::FeedbackStatIn> = state
        .feedback_log
        .stats(&req.lang)
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .into_iter()
        .map(|s| gec_core::FeedbackStatIn {
            source: s.source,
            rule_id: s.rule_id,
            adopted: s.adopted,
            rejected: s.rejected,
            last_at: s.last_at,
        })
        .collect();
    let recent: Vec<gec_core::FeedbackEntryIn> = state
        .feedback_log
        .list(Some(&req.lang), 50)
        .map_err(|e| ApiError::Internal(e.to_string()))?
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

    // 测评证据（可选；服务不可达 / 无匹配报告 → None，不阻断）
    let mut eval = if req.include_eval {
        gec_core::fetch_eval_evidence(&eval_endpoint(), &req.lang, req.eval_report_id.as_deref())
            .await
    } else {
        None
    };
    if req.include_eval && eval.is_none() {
        eval = Some(gec_core::EvalEvidence::default());
    }

    let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    // L3 重训练状态（「重训无效」检查）：最近一次 completed 的 l3_retrain 作业
    // （payload 含触发报告的 gate_f1 / report_id）+ 当前测评报告的 L3 门控 F1（复测值）
    let l3_retrain = {
        let mut st = gec_core::reflection_center::L3RetrainStateIn::default();
        if let Ok(jobs) = state
            .training_center
            .list_jobs(Some(&req.lang), Some("completed"), 50)
        {
            if let Some(job) = jobs
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
        }
        st.f1_after = eval.as_ref().and_then(|e| {
            e.layers
                .iter()
                .find(|l| l.name == "feature")
                .and_then(|l| l.gate_f1)
        });
        st
    };
    let mut report = gec_core::build_report_with_gold(
        &req.lang,
        &created_at,
        &stats,
        &recent,
        eval,
        &l3_retrain,
        req.gold.as_ref(),
    );
    if req.include_eval
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
    if let Ok(defs) = state.lexicon_store.rule_defs(&req.lang) {
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

    let report_json =
        serde_json::to_string(&report).map_err(|e| ApiError::Internal(e.to_string()))?;
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
    let (id, suggestion_ids) = state
        .reflection_center
        .save_report(&req.lang, &created_at, &report_json, &seeds)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(ReflectRunResponse {
        id,
        report,
        suggestion_ids,
    }))
}

/// GET /gec/reflect/reports?lang=&limit= — 反思报告历史（新 → 旧）
pub async fn reports(
    State(state): State<Arc<AppState>>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let lang = q.get("lang").cloned();
    let limit = q
        .get("limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(20);
    let reports = state
        .reflection_center
        .list_reports(lang.as_deref(), limit)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(serde_json::json!({ "reports": reports })))
}

/// GET /gec/reflect/report/:id — 报告详情
pub async fn report(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> Result<Json<lexicon_store::ReflectionReportFull>, ApiError> {
    state
        .reflection_center
        .get_report(id)
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::NotFound(format!("反思报告 #{id} 不存在")))
        .map(Json)
}

/// GET /gec/reflect/suggestions?status=&lang=&limit= — 建议队列
pub async fn suggestions(
    State(state): State<Arc<AppState>>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let status = q.get("status").cloned();
    let lang = q.get("lang").cloned();
    let limit = q
        .get("limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(50);
    let suggestions = state
        .reflection_center
        .list_suggestions(status.as_deref(), lang.as_deref(), limit)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(serde_json::json!({ "suggestions": suggestions })))
}

/// PUT /gec/reflect/suggestion/:id — 更新建议状态（P2 消费入口）
#[derive(Deserialize)]
pub struct SuggestionStatusRequest {
    pub status: String,
}

pub async fn update_suggestion(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Json(req): Json<SuggestionStatusRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if !lexicon_store::reflection_center::suggestion_status::ALL.contains(&req.status.as_str()) {
        return Err(ApiError::BadRequest(format!(
            "无效状态: {}（应为 {}）",
            req.status,
            lexicon_store::reflection_center::suggestion_status::ALL.join(" / ")
        )));
    }
    if !state
        .reflection_center
        .update_suggestion_status(id, &req.status)
        .map_err(|e| ApiError::Internal(e.to_string()))?
    {
        return Err(ApiError::NotFound(format!("建议 #{id} 不存在")));
    }
    Ok(Json(serde_json::json!({ "id": id, "status": req.status })))
}
