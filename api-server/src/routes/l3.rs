//! L3 特征模型训练飞轮（三期闭环）：确认样本汇入 → 积累提醒 → 重训练 → 热切换
//!
//! - **采集**（`POST /gec/l3/collect`）：前端把「确认采纳的 LLM 检测结果」
//!   （正样本）/「LLM 复查后确认干净的句子」（负样本）汇入
//!   `feature-model/l3_data/<lang>/confirmed.jsonl`（按文本+标签去重，
//!   上限 5000 条防膨胀）；
//! - **提醒**（`GET /gec/l3/status`）：样本计数 + 上次重训练时间（读
//!   feature-model 的 `retrain_meta.json`）+ 当前模型 F1（转发 feature-model
//!   `GET /l3/status`）；`retrain_ready` = 新正样本 ≥ 10（或累计 ≥ 30）
//!   且距上次重训练 ≥ 7 天（从未训练过则不限时间）——前端设置界面据此
//!   展示「可重训练」提示；
//! - **重训练**（`POST /gec/l3/retrain`）：转发 feature-model `POST /l3-retrain`
//!   （合并样本 → 重训练 → dev 评测 → F1 未显著退化则热切换模型），
//!   成功后记录本次重训练时间。
//!
//! 全程非阻断：feature-model 服务不可达时 status 降级（`service_available=false`）、
//! retrain 返回 502，不影响检测主链路。与二期 anomaly 蒸馏同源的自学习思路：
//! 特征模型随使用变强，LLM 长尾持续收缩。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Json, Query, State};
use serde::{Deserialize, Serialize};

use crate::routes::ApiError;
use crate::state::AppState;

/// 飞轮数据目录：`GEC_L3_DATA_DIR` 环境变量覆盖；缺省见
/// [`gec_core::default_l3_data_dir`]（开发 = 仓库内 `feature-model/l3_data`，
/// 打包应用 = `~/.langfixer/l3_data`）
pub fn l3_data_dir() -> PathBuf {
    std::env::var("GEC_L3_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| gec_core::default_l3_data_dir())
}

/// 单语言样本文件上限（防无限膨胀；超出后保留最新 N 条）
const MAX_SAMPLES: usize = 5000;
/// 提醒阈值：上次重训练后样本 ≥ 该值 → `retrain_ready`
const RETRAIN_READY_POS: usize = 10;
/// 提醒阈值：从未重训练过时，累计样本 ≥ 该值即可
const RETRAIN_READY_FIRST: usize = 30;
/// 距上次重训练至少 7 天才再次提醒（从未训练过不限时间）
const RETRAIN_COOLDOWN: i64 = 7 * 24 * 3600;

/// 采集一条飞轮样本（前端「采纳」LLM 错误 / 检测无错误时的干净句自动汇入）
#[derive(Deserialize)]
pub struct L3CollectRequest {
    pub lang: String,
    /// 句子文本（正样本 = 错误句原文；负样本 = 确认干净的句子）
    pub text: String,
    /// 1 = 正样本（确认错误）；0 = 负样本（确认干净）
    #[serde(default = "default_label")]
    pub label: u8,
    /// 错误类型（正样本；负样本可缺省）
    #[serde(default)]
    pub error_type: Option<String>,
    /// 来源：`llm_adopt`（用户确认采纳）/ `llm_clean`（LLM 复查后确认干净）
    #[serde(default)]
    pub source: Option<String>,
}

fn default_label() -> u8 {
    1
}

/// 采集结果
#[derive(Serialize)]
pub struct L3CollectResponse {
    /// 是否实际写入（false = 与已有样本重复）
    pub added: bool,
    /// 该语言当前样本总数
    pub total: usize,
}

/// 飞轮状态（设置界面「训练飞轮」卡片）
#[derive(Serialize)]
pub struct L3StatusResponse {
    /// 该语言样本总数（正 + 负）
    pub samples: usize,
    /// 采纳平行语料句对数（`adopted.jsonl`；训练资产 C）
    pub adopted: usize,
    /// 负偏好条数（`rejected.jsonl`；偏好/DPO 材料）
    pub rejected: usize,
    /// 上次重训练时间（ISO 8601；从未训练过为 null）
    pub last_retrain_at: Option<String>,
    /// 是否建议重训练（样本积累够 + 冷却期已过）
    pub retrain_ready: bool,
    /// feature-model 服务是否可达
    pub service_available: bool,
    /// 当前模型名（服务不可达时 null）
    pub model: Option<String>,
    /// 当前模型 dev F1（服务不可达 / 无 dev 集时 null）
    pub f1: Option<f64>,
    /// 实际解析的飞轮数据目录（采集写入处；与 Python 服务读取目录不一致时
    /// 重训看不到样本——可用 `GEC_L3_DATA_DIR` / `L3_DATA_DIR` 对齐）
    pub data_dir: String,
}

/// 重训练结果（feature-model `POST /l3-retrain` 透传 + 本地元数据）
#[derive(Serialize)]
pub struct L3RetrainResponse {
    /// 模型是否已热切换（false = F1 显著退化，保留旧模型）
    pub swapped: bool,
    /// 未切换时的原因
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// 重训练前 dev F1
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before_f1: Option<f64>,
    /// 重训练后 dev F1
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_f1: Option<f64>,
    /// 样本统计 `{confirmed, base, total}`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub samples: Option<serde_json::Value>,
    /// 本次重训练时间（ISO 8601，本地记录）
    pub retrain_at: String,
}

/// 重训练请求体
#[derive(Deserialize, Serialize)]
pub struct L3RetrainRequest {
    pub lang: String,
}

/// 追加一条样本到 `l3_data/<lang>/confirmed.jsonl`（按 文本+标签 去重，上限截断）
///
/// 返回 `Some(总数)` = 写入成功；`None` = 与已有样本重复（no-op）或写入失败。
pub fn collect_sample(dir: &Path, lang: &str, sample: &serde_json::Value) -> Option<usize> {
    append_jsonl(
        dir,
        lang,
        "confirmed.jsonl",
        sample,
        MAX_SAMPLES,
        &sample_key,
    )
}

/// 追加一条 JSONL 记录到 `l3_data/<lang>/<file>`（按 `key_of` 去重，超 `max` 截断保留最新）
fn append_jsonl(
    dir: &Path,
    lang: &str,
    file: &str,
    sample: &serde_json::Value,
    max: usize,
    key_of: &dyn Fn(&serde_json::Value) -> String,
) -> Option<usize> {
    let lang_dir = dir.join(lang);
    if let Err(e) = std::fs::create_dir_all(&lang_dir) {
        tracing::warn!(lang, error = %e, "L3 飞轮：创建样本目录失败");
        return None;
    }
    let path = lang_dir.join(file);
    let mut lines: Vec<String> = match std::fs::read_to_string(&path) {
        Ok(s) => s
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.to_string())
            .collect(),
        Err(_) => Vec::new(),
    };
    let key = key_of(sample);
    let dup = lines.iter().any(|l| {
        serde_json::from_str::<serde_json::Value>(l)
            .map(|v| key_of(&v) == key)
            .unwrap_or(false)
    });
    if dup {
        return None;
    }
    lines.push(serde_json::to_string(sample).unwrap_or_default());
    if lines.len() > max {
        let drop = lines.len() - max;
        lines.drain(0..drop);
    }
    if let Err(e) = std::fs::write(&path, format!("\n{}", lines.join("\n"))) {
        tracing::warn!(lang, error = %e, "L3 飞轮：样本写入失败");
        return None;
    }
    Some(lines.len())
}

/// 样本去重键：`文本|标签`
fn sample_key(v: &serde_json::Value) -> String {
    format!(
        "{}|{}",
        v.get("text").and_then(|v| v.as_str()).unwrap_or(""),
        v.get("label").and_then(|v| v.as_u64()).unwrap_or(1)
    )
}

/// 读取某语言样本总数（文件缺失 = 0）
pub fn count_samples(dir: &Path, lang: &str) -> usize {
    let path = dir.join(lang).join("confirmed.jsonl");
    match std::fs::read_to_string(&path) {
        Ok(s) => s.lines().filter(|l| !l.trim().is_empty()).count(),
        Err(_) => 0,
    }
}

// ── 采纳 / 放弃沉淀资产（adopted / rejected 平行语料）──────────────────────

/// 采纳平行语料与负偏好池上限（长期训练资产，大于 confirmed 的 5000）
const MAX_ADOPTED: usize = 20000;
/// 句对编辑密度上限：超过视为「用户改写」而非「纠错」，打 weak 标记
const WEAK_EDIT_RATIO: f64 = 0.4;

/// 采纳/放弃沉淀开关：**默认开**（自用产品定位）；多租户部署显式
/// `GEC_L3_ADOPT_COLLECT=0`（或 `false`）关闭（隐私边界，见 README）。
pub fn adopt_collect_enabled() -> bool {
    match std::env::var("GEC_L3_ADOPT_COLLECT") {
        Ok(v) => v != "0" && !v.eq_ignore_ascii_case("false"),
        Err(_) => true,
    }
}

/// 简易切句（与 detect.rs 干净句采集同构：。！？；.!? + 换行，≥4 字）
fn split_sentences(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for c in text.chars() {
        cur.push(c);
        if matches!(c, '。' | '！' | '？' | '；' | '.' | '!' | '?' | '\n') {
            let s = cur.trim().to_string();
            if s.chars().count() >= 4 {
                out.push(s);
            }
            cur.clear();
        }
    }
    let tail = cur.trim().to_string();
    if tail.chars().count() >= 4 {
        out.push(tail);
    }
    out
}

/// 字符编辑密度（1 - LCS/max_len）；超长句跳过精确计算（不标 weak）
fn char_diff_ratio(a: &str, b: &str) -> f64 {
    let (x, y): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let (n, m) = (x.len(), y.len());
    let mx = n.max(m);
    if mx == 0 {
        return 0.0;
    }
    if mx > 400 {
        return (mx - n.min(m)) as f64 / mx as f64;
    }
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if x[i] == y[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    1.0 - dp[0][0] as f64 / mx as f64
}

/// 沉淀一对「用户采纳」的纠正文本：切句对齐 → 句级 `src→dst` 对写
/// `l3_data/<lang>/adopted.jsonl`（去重键 src|dst）。
///
/// `src` = 检测原文（前端送检文本），`dst` = 用户累积采纳修正后的全文。
/// 切句数一致时逐句配对（仅保留有变化的句对）；否则整体作一对兜底。
/// 编辑密度 > [`WEAK_EDIT_RATIO`] 打 `weak` 标记（多半是用户改写，
/// 默认训练池排除，见 feature-model prepare 工具）。
///
/// 返回新增句对数（0 = 全部重复或无变化）。
pub fn collect_adopted(dir: &Path, lang: &str, src: &str, dst: &str, source: &str) -> usize {
    let src_s = split_sentences(src);
    let dst_s = split_sentences(dst);
    let pairs: Vec<(String, String)> = if !src_s.is_empty() && src_s.len() == dst_s.len() {
        src_s
            .into_iter()
            .zip(dst_s)
            .filter(|(a, b)| a != b)
            .collect()
    } else {
        let (a, b) = (src.trim(), dst.trim());
        if a != b {
            vec![(a.to_string(), b.to_string())]
        } else {
            Vec::new()
        }
    };
    let key_of = |v: &serde_json::Value| -> String {
        format!(
            "{}|{}",
            v.get("src").and_then(|v| v.as_str()).unwrap_or(""),
            v.get("dst").and_then(|v| v.as_str()).unwrap_or("")
        )
    };
    let mut added = 0usize;
    for (s, d) in pairs {
        let sample = serde_json::json!({
            "src": s,
            "dst": d,
            "weak": char_diff_ratio(&s, &d) > WEAK_EDIT_RATIO,
            "source": source,
            "ts": chrono::Utc::now().to_rfc3339(),
        });
        if append_jsonl(dir, lang, "adopted.jsonl", &sample, MAX_ADOPTED, &key_of).is_some() {
            added += 1;
        }
    }
    added
}

/// 沉淀一条「用户放弃」的负偏好：`l3_data/<lang>/rejected.jsonl`
/// （片段级：被放弃的原文片段 + 被拒绝的修正；未来偏好/DPO 训练材料）。
/// 去重键 fragment|rejected_correction。
pub fn collect_rejected(
    dir: &Path,
    lang: &str,
    error: &shared_model::Error,
    reason: Option<&str>,
) -> bool {
    let sample = serde_json::json!({
        "fragment": error.original,
        "rejected_correction": error.correction,
        "error_type": error.error_type,
        "source_layer": error.source,
        "reason": reason.unwrap_or(""),
        "ts": chrono::Utc::now().to_rfc3339(),
    });
    let key_of = |v: &serde_json::Value| -> String {
        format!(
            "{}|{}",
            v.get("fragment").and_then(|v| v.as_str()).unwrap_or(""),
            v.get("rejected_correction")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
        )
    };
    append_jsonl(dir, lang, "rejected.jsonl", &sample, MAX_ADOPTED, &key_of).is_some()
}

/// 读取某语言 JSONL 资产计数（文件缺失 = 0）
pub fn count_jsonl(dir: &Path, lang: &str, file: &str) -> usize {
    let path = dir.join(lang).join(file);
    match std::fs::read_to_string(&path) {
        Ok(s) => s.lines().filter(|l| !l.trim().is_empty()).count(),
        Err(_) => 0,
    }
}

/// 读取上次重训练时间（feature-model 写的 `retrain_meta.json`）
pub fn last_retrain_at(dir: &Path, lang: &str) -> Option<String> {
    let s = std::fs::read_to_string(dir.join("retrain_meta.json")).ok()?;
    let meta = serde_json::from_str::<serde_json::Value>(&s).ok()?;
    meta.get(lang)?
        .get("ts")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// 判断是否建议重训练（样本积累够 + 冷却期）
pub fn retrain_ready(total: usize, last_at: Option<&str>) -> bool {
    let enough = match last_at {
        Some(_) => total >= RETRAIN_READY_POS,
        None => total >= RETRAIN_READY_FIRST,
    };
    if !enough {
        return false;
    }
    match last_at {
        None => true,
        Some(ts) => {
            // RFC 3339（`+08:00`）优先；兼容无冒号时区（`+0800`）的旧元数据
            let secs = chrono::DateTime::parse_from_rfc3339(ts)
                .or_else(|_| chrono::DateTime::parse_from_str(ts, "%Y-%m-%dT%H:%M:%S%z"))
                .map(|d| d.timestamp())
                .unwrap_or(0);
            chrono::Utc::now().timestamp() - secs >= RETRAIN_COOLDOWN
        }
    }
}

/// POST /gec/l3/collect — 采集一条飞轮样本（采纳 / 干净句）
pub async fn collect(
    State(state): State<Arc<AppState>>,
    Json(req): Json<L3CollectRequest>,
) -> Result<Json<L3CollectResponse>, ApiError> {
    let _ = &state; // 数据目录取环境变量（GEC_L3_DATA_DIR），保留 state 供后续扩展
    if !matches!(req.lang.as_str(), "en" | "zh") {
        return Err(ApiError::BadRequest(format!("不支持的语言: {}", req.lang)));
    }
    if req.text.trim().is_empty() {
        return Err(ApiError::BadRequest("text 不能为空".to_string()));
    }
    let sample = serde_json::json!({
        "text": req.text,
        "label": req.label,
        "error_type": req.error_type.unwrap_or_default(),
        "source": req.source.unwrap_or_else(|| {
            if req.label == 1 {
                "llm_adopt".to_string()
            } else {
                "llm_clean".to_string()
            }
        }),
        "ts": chrono::Utc::now().to_rfc3339(),
    });
    let dir = l3_data_dir();
    match collect_sample(&dir, &req.lang, &sample) {
        Some(total) => {
            tracing::info!(lang = %req.lang, label = req.label, total, "L3 飞轮：样本已汇入");
            Ok(Json(L3CollectResponse { added: true, total }))
        }
        None => Ok(Json(L3CollectResponse {
            added: false,
            total: count_samples(&dir, &req.lang),
        })),
    }
}

/// GET /gec/l3/status?lang= — 飞轮状态（样本数 + 上次重训练 + 当前模型 F1）
pub async fn status(
    State(state): State<Arc<AppState>>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<L3StatusResponse>, ApiError> {
    let lang = q.get("lang").cloned().unwrap_or_else(|| "zh".to_string());
    if !matches!(lang.as_str(), "en" | "zh") {
        return Err(ApiError::BadRequest(format!("不支持的语言: {lang}")));
    }
    let dir = l3_data_dir();
    let total = count_samples(&dir, &lang);
    let last = last_retrain_at(&dir, &lang);
    let ready = retrain_ready(total, last.as_deref());
    // 记录实际解析的数据目录：与 feature-model 服务读取目录不一致时重训看不到样本
    tracing::info!(data_dir = %dir.display(), "L3 飞轮数据目录（解析结果）");

    // feature-model 服务状态（不可达时降级：本地数据仍可用）
    let cfg = state.settings.read().unwrap().l3.clone();
    let (service_available, model, f1) = match reqwest::Client::new()
        .get(format!("{}/l3/status", cfg.endpoint.trim_end_matches('/')))
        .timeout(Duration::from_secs(5))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => match resp.json::<serde_json::Value>().await {
            Ok(v) => {
                let m = v.get("models").and_then(|m| m.get(&lang));
                (
                    true,
                    m.and_then(|m| m.get("model"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    m.and_then(|m| m.get("f1")).and_then(|v| v.as_f64()),
                )
            }
            Err(_) => (true, None, None),
        },
        _ => (false, None, None),
    };

    Ok(Json(L3StatusResponse {
        samples: total,
        adopted: count_jsonl(&dir, &lang, "adopted.jsonl"),
        rejected: count_jsonl(&dir, &lang, "rejected.jsonl"),
        last_retrain_at: last,
        retrain_ready: ready,
        service_available,
        model,
        f1,
        data_dir: dir.to_string_lossy().into_owned(),
    }))
}

/// 转发 feature-model 重训练（合并样本 → 重训练 → 热切换）→ 响应 JSON
///
/// 训练中心（`routes::training_center`）的 `l3_retrain` 作业与
/// `POST /gec/l3/retrain` 共用本函数。
pub async fn do_retrain(state: &Arc<AppState>, lang: &str) -> Result<serde_json::Value, ApiError> {
    let cfg = state.settings.read().unwrap().l3.clone();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(cfg.timeout_secs.max(120)))
        .build()
        .map_err(|e| ApiError::Internal(format!("HTTP 客户端构建失败: {e}")))?;
    let req = L3RetrainRequest {
        lang: lang.to_string(),
    };
    let resp = client
        .post(format!("{}/l3-retrain", cfg.endpoint.trim_end_matches('/')))
        .json(&req)
        .send()
        .await
        .map_err(|e| ApiError::Upstream(format!("特征模型服务不可达（{e}）")))?;
    let (code, body) = (resp.status(), resp.json::<serde_json::Value>().await);
    let body = match body {
        Ok(b) => b,
        Err(e) => return Err(ApiError::Upstream(format!("特征模型响应解析失败: {e}"))),
    };
    if !code.is_success() {
        let msg = body
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("重训练失败");
        return Err(ApiError::Upstream(msg.to_string()));
    }
    Ok(body)
}

/// 记录本次重训练时间（feature-model 侧也会写 retrain_meta.json，双写以本地为准）
pub(crate) fn record_retrain_meta_public(lang: &str) -> String {
    let dir = l3_data_dir();
    let retrain_at = chrono::Utc::now().to_rfc3339();
    let meta_path = dir.join("retrain_meta.json");
    let existing = std::fs::read_to_string(&meta_path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
    let mut meta = existing.unwrap_or_else(serde_json::Value::default);
    if !meta.is_object() {
        meta = serde_json::Map::new().into();
    }
    meta[lang] = serde_json::json!({
        "ts": retrain_at,
        "confirmed": count_samples(&dir, lang),
        "source": "api-server",
    });
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(&meta_path, meta.to_string());
    retrain_at
}

/// POST /gec/l3/retrain — 转发 feature-model 重训练（合并样本 → 重训练 → 热切换）
pub async fn retrain(
    State(state): State<Arc<AppState>>,
    Json(req): Json<L3RetrainRequest>,
) -> Result<Json<L3RetrainResponse>, ApiError> {
    if !matches!(req.lang.as_str(), "en" | "zh") {
        return Err(ApiError::BadRequest(format!("不支持的语言: {}", req.lang)));
    }
    let body = do_retrain(&state, &req.lang).await?;
    let retrain_at = record_retrain_meta_public(&req.lang);

    Ok(Json(L3RetrainResponse {
        swapped: body
            .get("swapped")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        reason: body
            .get("reason")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        before_f1: body.get("before_f1").and_then(|v| v.as_f64()),
        after_f1: body.get("after_f1").and_then(|v| v.as_f64()),
        samples: body.get("samples").cloned(),
        retrain_at,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("l3_flywheel_{tag}_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        d
    }

    fn sample(text: &str, label: u8) -> serde_json::Value {
        serde_json::json!({ "text": text, "label": label, "error_type": "grammar", "source": "llm_adopt", "ts": "t" })
    }

    #[test]
    fn collect_appends_and_dedups() {
        let dir = tmp_dir("dedup");
        let a = collect_sample(&dir, "zh", &sample("他昨天吃了昨天。", 1));
        assert_eq!(a, Some(1));
        // 相同文本+标签 → 去重 no-op
        assert_eq!(
            collect_sample(&dir, "zh", &sample("他昨天吃了昨天。", 1)),
            None
        );
        // 相同文本不同标签 → 追加
        assert_eq!(
            collect_sample(&dir, "zh", &sample("他昨天吃了昨天。", 0)),
            Some(2)
        );
        assert_eq!(count_samples(&dir, "zh"), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn collect_caps_at_max() {
        let dir = tmp_dir("cap");
        for i in 0..MAX_SAMPLES + 5 {
            let _ = collect_sample(&dir, "en", &sample(&format!("sentence {i}"), 1));
        }
        assert_eq!(count_samples(&dir, "en"), MAX_SAMPLES);
        // 截断保留最新：最早 5 条被丢弃
        let content = std::fs::read_to_string(dir.join("en").join("confirmed.jsonl")).unwrap();
        assert!(!content.contains("sentence 0\""));
        assert!(content.contains(&format!("sentence {MAX_SAMPLES}\"")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn retrain_ready_thresholds() {
        // 从未训练：需累计 ≥ 30
        assert!(!retrain_ready(10, None));
        assert!(retrain_ready(30, None));
        // 训练过：样本 ≥ 10 且冷却期 7 天
        let old = (chrono::Utc::now() - chrono::Duration::days(8)).to_rfc3339();
        assert!(retrain_ready(10, Some(&old)));
        assert!(!retrain_ready(9, Some(&old)));
        let recent = (chrono::Utc::now() - chrono::Duration::days(1)).to_rfc3339();
        assert!(!retrain_ready(50, Some(&recent)));
        // 兼容无冒号时区（`+0800`）的旧元数据
        let old_no_colon = (chrono::Utc::now() - chrono::Duration::days(8))
            .format("%Y-%m-%dT%H:%M:%S%z")
            .to_string();
        assert!(retrain_ready(10, Some(&old_no_colon)));
        let recent_no_colon = (chrono::Utc::now() - chrono::Duration::days(1))
            .format("%Y-%m-%dT%H:%M:%S%z")
            .to_string();
        assert!(!retrain_ready(50, Some(&recent_no_colon)));
    }

    #[test]
    fn collect_adopted_pairs_and_weak_flag() {
        let dir = tmp_dir("adopted");
        // 两句、只改第二句 → 只沉淀有变化的句对
        let added = collect_adopted(
            &dir,
            "zh",
            "今天天气很好。他昨天报复很大。",
            "今天天气很好。他昨天抱负很大。",
            "adopt",
        );
        assert_eq!(added, 1);
        let content = std::fs::read_to_string(dir.join("zh").join("adopted.jsonl")).unwrap();
        assert!(content.contains("他昨天报复很大"));
        assert!(content.contains("他昨天抱负很大"));
        assert!(!content.contains("今天天气很好")); // 无变化的句不沉淀
        // 重复采纳 → 去重
        assert_eq!(
            collect_adopted(
                &dir,
                "zh",
                "今天天气很好。他昨天报复很大。",
                "今天天气很好。他昨天抱负很大。",
                "adopt",
            ),
            0
        );
        // 整句重写 → weak 标记
        collect_adopted(
            &dir,
            "zh",
            "一二三四五六七八。",
            "完全不同的另一句话内容。",
            "adopt",
        );
        let content = std::fs::read_to_string(dir.join("zh").join("adopted.jsonl")).unwrap();
        assert!(content.contains("\"weak\":true"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn collect_adopted_fallback_whole_text_on_sentence_mismatch() {
        let dir = tmp_dir("adopted_mismatch");
        // 切句数不一致（用户在改写中）→ 整体一对兜底
        let added = collect_adopted(&dir, "zh", "短句一。短句二。", "短句一改了。", "adopt");
        assert_eq!(added, 1);
        let content = std::fs::read_to_string(dir.join("zh").join("adopted.jsonl")).unwrap();
        assert!(content.contains("短句二"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn collect_rejected_writes_negative_pref() {
        let dir = tmp_dir("rejected");
        let err = shared_model::Error::new(
            "zh",
            "grammar",
            0.8,
            [0, 6],
            "他报复",
            Some("他抱负".to_string()),
            "t",
        );
        assert!(collect_rejected(&dir, "zh", &err, Some("语义不对")));
        // 去重
        assert!(!collect_rejected(&dir, "zh", &err, Some("语义不对")));
        assert_eq!(count_jsonl(&dir, "zh", "rejected.jsonl"), 1);
        let content = std::fs::read_to_string(dir.join("zh").join("rejected.jsonl")).unwrap();
        assert!(content.contains("他报复"));
        assert!(content.contains("语义不对"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn char_diff_ratio_basics() {
        assert!((char_diff_ratio("abcdef", "abcdef") - 0.0).abs() < 1e-9);
        // 两字之差（7 字句）：LCS=5，密度 = 1-5/7 ≈ 0.286
        let r = char_diff_ratio("他昨天报复很大", "他昨天抱负很大");
        assert!(r > 0.2 && r < 0.35);
        // 完全不同：接近 1
        assert!(char_diff_ratio("一二三四五六", "abcdefghij") > 0.9);
    }
}
