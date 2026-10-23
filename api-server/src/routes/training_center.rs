//! 训练中心（进化中心 P2）：消费反思建议 → 执行训练动作 → 作业记录
//!
//! - `POST /gec/train/pool` — 领域池构建：模型层反馈（adopt=error / reject=clean）
//!   + 飞轮确认样本 + 增强器干净句 → 互补单侧分配去重 → 作业（completed）
//! - `POST /gec/train/llm-data` — LLM 训练数据构建：LLM 层 adopt 反馈（带修正）
//!   → SFT JSONL（作业 result 携带，可导出外部训练）
//! - `POST /gec/train/l3/retrain` — L3 重训练：转发 feature-model 飞轮
//!   （合并确认样本 → 重训练 → dev 评测 → 热切换）
//! - `POST /gec/train/lexicon/apply` — 词表条件化入库：条件化排除条目
//!   （`append_rule_exclude`；入库前先过测评消融——`check_ablation=true` 时
//!   拉取测评证据附入作业 result，测评服务不可达时降级为直接入库）
//! - `POST /gec/train/suggestion/:id/run` — **P1 → P2 消费入口**：读反思建议 →
//!   `plan_from_suggestion` 分派到对应训练动作 → 成功后建议标记 `consumed`
//!   （`rule_tune` 是人工操作，返回 400）
//! - `GET /gec/train/jobs` / `GET /gec/train/job/:id` — 作业列表 / 详情
//!
//! 闭环定位见 `EVOLUTION.md`：测评发现弱层 → 反思聚成建议 → 训练消费 →
//! 测评 A/B 验证（「训练后」A/B 是 P2 验收标准）。

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use serde::{Deserialize, Serialize};

use crate::routes::ApiError;
use crate::state::AppState;

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// 测评服务地址（`GEC_EVAL_ENDPOINT` 覆盖；缺省 127.0.0.1:4807；
/// 与 `routes::eval` / `routes::reflection_center` 同语义）
fn eval_endpoint() -> String {
    std::env::var("GEC_EVAL_ENDPOINT")
        .map(|s| s.trim_end_matches('/').to_string())
        .unwrap_or_else(|_| "http://127.0.0.1:4807".to_string())
}

/// 作业生命周期：`proposed` → `confirmed` → `running` → `completed` / `failed`
///
/// 本地作业（pool / llm-data / lexicon）同步执行，一次调用走完状态机
/// （系统自动确认——P3 优化轮次才引入人工确认门）；
/// 返回作业 id（存储失败时 0）。
fn complete_job(
    state: &Arc<AppState>,
    kind: &str,
    lang: &str,
    source_suggestion: Option<i64>,
    payload: &serde_json::Value,
    result: Result<serde_json::Value, String>,
) -> i64 {
    let t0 = now();
    let id = match state
        .training_center
        .insert_job(kind, lang, source_suggestion, payload, &t0)
    {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!(kind, error = %e, "训练作业落库失败");
            return 0;
        }
    };
    let _ = state
        .training_center
        .transition(id, "confirmed", None, &now());
    let _ = state
        .training_center
        .transition(id, "running", None, &now());
    let (to, res) = match result {
        Ok(v) => ("completed", v),
        Err(e) => ("failed", serde_json::json!({ "error": e })),
    };
    let _ = state.training_center.transition(id, to, Some(&res), &now());
    id
}

/// POST /gec/train/pool 请求
#[derive(Deserialize)]
pub struct PoolBuildRequest {
    pub lang: String,
    /// 增强器：干净句（用户自有语料——基准集句子禁止入池，防污染）
    #[serde(default)]
    pub clean_sentences: Vec<String>,
}

/// POST /gec/train/pool 响应
#[derive(Serialize)]
pub struct PoolBuildResponse {
    pub job_id: i64,
    pub pool: gec_core::DomainPool,
}

/// 领域池构建：模型层反馈 + 飞轮确认样本 + 增强器
pub async fn build_pool(
    State(state): State<Arc<AppState>>,
    Json(req): Json<PoolBuildRequest>,
) -> Result<Json<PoolBuildResponse>, ApiError> {
    if !matches!(req.lang.as_str(), "en" | "zh") {
        return Err(ApiError::BadRequest(format!("不支持的语言: {}", req.lang)));
    }
    // 1. 模型层反馈明细 → 纯数据输入
    let entries: Vec<gec_core::FeedbackEntryIn> = state
        .feedback_log
        .list(Some(&req.lang), 500)
        .map_err(|e| ApiError::Internal(e.to_string()))?
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
    // 2. 飞轮确认样本（l3_data/<lang>/confirmed.jsonl：text + label 1/0）
    let flywheel = load_flywheel_samples(&req.lang);
    let input = gec_core::PoolInput {
        lang: req.lang.clone(),
        feedback: entries,
        flywheel,
        clean_sentences: req.clean_sentences.clone(),
    };
    // 3. 构建 + 落作业
    let pool = gec_core::build_domain_pool(&input);
    let payload = serde_json::json!({
        "lang": req.lang,
        "feedback": input.feedback.len(),
        "flywheel": input.flywheel.len(),
        "augment": input.clean_sentences.len(),
    });
    let result = serde_json::to_value(&pool).unwrap_or(serde_json::Value::Null);
    let job_id = complete_job(
        &state,
        gec_core::job_kind::DOMAIN_POOL,
        &req.lang,
        None,
        &payload,
        Ok(result),
    );
    Ok(Json(PoolBuildResponse { job_id, pool }))
}

/// 读取飞轮确认样本 → `PoolSample`（label 1=error / 0=clean）
fn load_flywheel_samples(lang: &str) -> Vec<gec_core::PoolSample> {
    let path = crate::routes::l3::l3_data_dir()
        .join(lang)
        .join("confirmed.jsonl");
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
/// POST /gec/train/llm-data 请求
#[derive(Deserialize)]
pub struct LlmDataRequest {
    pub lang: String,
    /// 当前 LLM 臂（标注用；缺省时从 payload / 设置推断）
    #[serde(default)]
    pub arm: Option<String>,
}

/// POST /gec/train/llm-data 响应
#[derive(Serialize)]
pub struct LlmDataResponse {
    pub job_id: i64,
    pub data: gec_core::LlmTrainingData,
}

/// LLM 训练数据构建：LLM 层 adopt 反馈（带修正）→ SFT JSONL
pub async fn build_llm_data(
    State(state): State<Arc<AppState>>,
    Json(req): Json<LlmDataRequest>,
) -> Result<Json<LlmDataResponse>, ApiError> {
    if !matches!(req.lang.as_str(), "en" | "zh") {
        return Err(ApiError::BadRequest(format!("不支持的语言: {}", req.lang)));
    }
    let entries: Vec<gec_core::FeedbackEntryIn> = state
        .feedback_log
        .list(Some(&req.lang), 500)
        .map_err(|e| ApiError::Internal(e.to_string()))?
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
    let data =
        gec_core::build_llm_training_data(&req.lang, req.arm.as_deref().unwrap_or(""), &entries);
    let payload = serde_json::json!({
        "lang": req.lang,
        "arm": req.arm,
    });
    let result = serde_json::json!({
        "n_samples": data.samples.len(),
        "jsonl": data.jsonl,
    });
    let job_id = complete_job(
        &state,
        gec_core::job_kind::LLM_DATA,
        &req.lang,
        None,
        &payload,
        Ok(result),
    );
    Ok(Json(LlmDataResponse { job_id, data }))
}

/// POST /gec/train/l3/retrain 请求
#[derive(Deserialize)]
pub struct L3RetrainRequest {
    pub lang: String,
}

/// POST /gec/train/l3/retrain 响应
#[derive(Serialize)]
pub struct L3RetrainResponse {
    pub job_id: i64,
    /// feature-model 响应（swapped / reason / before_f1 / after_f1 / samples）
    pub result: serde_json::Value,
}

/// L3 重训练：转发 feature-model 飞轮（与 `POST /gec/l3/retrain` 同源）
pub async fn retrain_l3(
    State(state): State<Arc<AppState>>,
    Json(req): Json<L3RetrainRequest>,
) -> Result<Json<L3RetrainResponse>, ApiError> {
    if !matches!(req.lang.as_str(), "en" | "zh") {
        return Err(ApiError::BadRequest(format!("不支持的语言: {}", req.lang)));
    }
    let payload = serde_json::json!({ "lang": req.lang });
    let result: Result<serde_json::Value, String> =
        crate::routes::l3::do_retrain(&state, &req.lang)
            .await
            .map(|body| {
                crate::routes::l3::record_retrain_meta_public(&req.lang);
                body
            })
            .map_err(|e| e.to_string());
    let job_id = complete_job(
        &state,
        gec_core::job_kind::L3_RETRAIN,
        &req.lang,
        None,
        &payload,
        result.clone(),
    );
    match result {
        Ok(body) => Ok(Json(L3RetrainResponse {
            job_id,
            result: body,
        })),
        Err(e) => Err(ApiError::Upstream(e)),
    }
}

/// POST /gec/train/lexicon/apply 请求
#[derive(Deserialize)]
pub struct LexiconApplyRequest {
    pub lang: String,
    /// 目标规则 id（缺省时从 `suggestion_id` 的 payload 推断）
    #[serde(default)]
    pub rule_id: Option<String>,
    /// 条件化排除文本（缺省时从 `suggestion_id` 的 payload.originals 取）
    #[serde(default)]
    pub words: Vec<String>,
    /// 来源反思建议 id（成功后标记 consumed）
    #[serde(default)]
    pub suggestion_id: Option<i64>,
    /// 入库前先过测评消融（测评服务不可达时降级为直接入库）
    #[serde(default)]
    pub check_ablation: bool,
}

/// POST /gec/train/lexicon/apply 响应
#[derive(Serialize)]
pub struct LexiconApplyResponse {
    pub job_id: i64,
    /// 实际入库的排除条目（rule_id → 新增 words）
    pub applied: Vec<serde_json::Value>,
    /// 已存在（幂等跳过）的条目数
    pub skipped: usize,
    /// 测评证据（check_ablation 且测评服务可达时有值）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eval_evidence: Option<gec_core::EvalEvidence>,
    /// 来源建议是否已标记 consumed
    pub suggestion_consumed: bool,
}

/// 词表条件化入库：条件化排除条目（入库前先过测评消融）
pub async fn apply_lexicon(
    State(state): State<Arc<AppState>>,
    Json(req): Json<LexiconApplyRequest>,
) -> Result<Json<LexiconApplyResponse>, ApiError> {
    if !matches!(req.lang.as_str(), "en" | "zh") {
        return Err(ApiError::BadRequest(format!("不支持的语言: {}", req.lang)));
    }
    // 建议 payload 补全 rule_id / words
    let (mut rule_id, mut words) = (req.rule_id.clone(), req.words.clone());
    if let Some(sid) = req.suggestion_id {
        if let Some(row) = state
            .reflection_center
            .get_suggestion(sid)
            .map_err(|e| ApiError::Internal(e.to_string()))?
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
    }
    let rule_id = rule_id
        .ok_or_else(|| ApiError::BadRequest("rule_id 缺失（且建议 payload 无 rule_id）".into()))?;
    if words.is_empty() {
        return Err(ApiError::BadRequest("无可入库的排除文本".into()));
    }
    // 测评消融（可选；不可达 / 无报告 → 降级为直接入库）
    let eval_evidence = if req.check_ablation {
        gec_core::fetch_eval_evidence(&eval_endpoint(), &req.lang, None).await
    } else {
        None
    };
    // 入库
    let lexicons_dir =
        std::path::PathBuf::from(state.settings.read().unwrap().lexicons_dir.clone());
    let mut applied = Vec::new();
    let mut skipped = 0;
    for w in &words {
        match lexicon_store::append_rule_exclude(&lexicons_dir, &req.lang, &rule_id, w) {
            Ok(true) => applied.push(serde_json::json!({ "rule_id": rule_id, "word": w })),
            Ok(false) => skipped += 1,
            Err(e) => return Err(ApiError::Internal(format!("排除条目入库失败（{w}）: {e}"))),
        }
    }
    let payload = serde_json::json!({
        "lang": req.lang,
        "rule_id": rule_id,
        "words": words,
    });
    let result = serde_json::json!({
        "applied": applied,
        "skipped": skipped,
        "eval_evidence": eval_evidence,
    });
    let job_id = complete_job(
        &state,
        gec_core::job_kind::LEXICON_CONDITION,
        &req.lang,
        req.suggestion_id,
        &payload,
        Ok(result),
    );
    // 建议 → consumed
    let suggestion_consumed = match req.suggestion_id {
        Some(sid) if job_id > 0 => state
            .reflection_center
            .update_suggestion_status(sid, "consumed")
            .map_err(|e| ApiError::Internal(e.to_string()))?,
        _ => false,
    };
    Ok(Json(LexiconApplyResponse {
        job_id,
        applied,
        skipped,
        eval_evidence,
        suggestion_consumed,
    }))
}

/// POST /gec/train/suggestion/:id/run 响应
#[derive(Serialize)]
pub struct RunSuggestionResponse {
    /// 派生作业 id
    pub job_id: i64,
    /// 作业类型
    pub job_kind: String,
    /// 作业结果（completed 时有值；failed 时含 error）
    pub result: serde_json::Value,
    /// 建议是否已标记 consumed（成功 = true）
    pub suggestion_consumed: bool,
}

/// 训练动作执行错误
///
/// - [`BadParam`](参数错误：payload 缺字段 → 400，不落作业)
/// - [`Execution`](执行失败：外部服务不可达 / 入库失败 → 作业标 failed)
/// - [`Unknown`](未知动作类型 → 500)
#[derive(Debug)]
pub(crate) enum ActionError {
    BadParam(String),
    Execution(String),
    Unknown(String),
}

/// 训练动作分派器（P2 建议消费与 P3 轮次执行共用）
///
/// - `l3_retrain`：转发 feature-model 飞轮（合并确认样本 → 重训练 → dev 评测 →
///   热切换）；服务不可达 → [`ActionError::Execution`]
/// - `lexicon_condition`：payload 的 `rule_id` + `originals` → 条件化排除条目入库；
///   payload 缺参 → [`ActionError::BadParam`]
/// - `llm_data`：LLM 层 adopt 反馈（带修正）→ SFT JSONL
///
/// 成功返回 `(job_kind, result)`。
pub(crate) async fn execute_training_action(
    state: &Arc<AppState>,
    kind: &str,
    payload: &serde_json::Value,
    lang: &str,
) -> Result<(String, serde_json::Value), ActionError> {
    match kind {
        gec_core::job_kind::L3_RETRAIN => {
            let r: Result<serde_json::Value, ActionError> =
                crate::routes::l3::do_retrain(state, lang)
                    .await
                    .map(|body| {
                        crate::routes::l3::record_retrain_meta_public(lang);
                        body
                    })
                    .map_err(|e| ActionError::Execution(e.to_string()));
            r.map(|v| (gec_core::job_kind::L3_RETRAIN.to_string(), v))
        }
        gec_core::job_kind::LEXICON_CONDITION => {
            let rule_id = payload
                .get("rule_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    ActionError::BadParam("lexicon_condition: payload 缺 rule_id".into())
                })?;
            let words: Vec<String> = payload
                .get("originals")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|w| w.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .ok_or_else(|| {
                    ActionError::BadParam("lexicon_condition: payload 缺 originals".into())
                })?;
            if words.is_empty() {
                return Err(ActionError::BadParam(
                    "lexicon_condition: payload 无可入库的排除文本".into(),
                ));
            }
            let lexicons_dir =
                std::path::PathBuf::from(state.settings.read().unwrap().lexicons_dir.clone());
            let mut applied = Vec::new();
            let mut skipped = 0;
            for w in &words {
                match lexicon_store::append_rule_exclude(&lexicons_dir, lang, rule_id, w) {
                    Ok(true) => applied.push(serde_json::json!({ "rule_id": rule_id, "word": w })),
                    Ok(false) => skipped += 1,
                    Err(e) => {
                        return Err(ActionError::Execution(format!(
                            "排除条目入库失败（{w}）: {e}"
                        )));
                    }
                }
            }
            Ok((
                gec_core::job_kind::LEXICON_CONDITION.to_string(),
                serde_json::json!({ "applied": applied, "skipped": skipped }),
            ))
        }
        gec_core::job_kind::LLM_DATA => {
            let entries: Vec<gec_core::FeedbackEntryIn> = state
                .feedback_log
                .list(Some(lang), 500)
                .map_err(|e| ActionError::Execution(e.to_string()))?
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
                lang,
                payload
                    .get("current_arm")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
                &entries,
            );
            Ok((
                gec_core::job_kind::LLM_DATA.to_string(),
                serde_json::json!({
                    "n_samples": data.samples.len(),
                    "jsonl": data.jsonl,
                }),
            ))
        }
        _ => Err(ActionError::Unknown(format!("未知动作类型: {kind}"))),
    }
}

/// **P1 → P2 消费入口**：反思建议 → 分派训练动作 → 建议 consumed
///
/// - `l3_sample` → L3 重训练（feature-model 飞轮；服务不可达 → 作业 failed，建议保持 pending）
/// - `lexicon_condition` → 词表条件化入库（payload 的 rule_id / originals）
/// - `llm_arm` → LLM 训练数据构建
/// - `rule_tune` → 400（人工操作，不产生训练作业）
pub async fn run_suggestion(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> Result<Json<RunSuggestionResponse>, ApiError> {
    let row = state
        .reflection_center
        .get_suggestion(id)
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::NotFound(format!("建议 #{id} 不存在")))?;
    let suggestion = gec_core::reflection_center::ReflectionSuggestion {
        kind: row.kind.clone(),
        target: row.target.clone(),
        summary: row.summary.clone(),
        evidence: row.evidence.clone(),
        payload: row.payload.clone(),
        priority: row.priority as u8,
    };
    let plan = gec_core::plan_from_suggestion(&suggestion).ok_or_else(|| {
        ApiError::BadRequest(format!("建议类型 {} 不是训练作业（人工操作）", row.kind))
    })?;
    let lang = plan.lang.clone();

    // 分派执行（与 P3 轮次执行共用 execute_training_action）
    let (job_kind, result) =
        match execute_training_action(&state, &plan.kind, &plan.payload, &lang).await {
            Ok(r) => r,
            Err(ActionError::BadParam(e)) => return Err(ApiError::BadRequest(e)),
            Err(ActionError::Execution(e)) => {
                // 执行失败：作业落库标 failed，建议保持 pending（可重试）
                let _ = complete_job(
                    &state,
                    &plan.kind,
                    &lang,
                    Some(id),
                    &plan.payload,
                    Err(e.clone()),
                );
                return Err(ApiError::Upstream(e));
            }
            Err(ActionError::Unknown(e)) => return Err(ApiError::Internal(e)),
        };
    let job_id = complete_job(
        &state,
        &job_kind,
        &lang,
        Some(id),
        &plan.payload,
        Ok(result.clone()),
    );
    let suggestion_consumed = if job_id > 0 {
        state
            .reflection_center
            .update_suggestion_status(id, "consumed")
            .map_err(|e| ApiError::Internal(e.to_string()))?
    } else {
        false
    };
    Ok(Json(RunSuggestionResponse {
        job_id,
        job_kind,
        result,
        suggestion_consumed,
    }))
}

/// GET /gec/train/jobs 查询参数
#[derive(Deserialize)]
pub struct JobsQuery {
    #[serde(default)]
    pub lang: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// GET /gec/train/jobs 响应
#[derive(Serialize)]
pub struct JobsResponse {
    pub jobs: Vec<lexicon_store::TrainingJobRow>,
}

/// 作业列表（新 → 旧；lang / status 可选过滤）
pub async fn jobs(
    State(state): State<Arc<AppState>>,
    Query(q): Query<JobsQuery>,
) -> Result<Json<JobsResponse>, ApiError> {
    let limit = q.limit.unwrap_or(50).min(200);
    let jobs = state
        .training_center
        .list_jobs(q.lang.as_deref(), q.status.as_deref(), limit)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(JobsResponse { jobs }))
}

/// GET /gec/train/job/:id 响应
#[derive(Serialize)]
pub struct JobResponse {
    pub job: lexicon_store::TrainingJobRow,
}

/// 作业详情
pub async fn job(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> Result<Json<JobResponse>, ApiError> {
    let job = state
        .training_center
        .get_job(id)
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::NotFound(format!("作业 #{id} 不存在")))?;
    Ok(Json(JobResponse { job }))
}
