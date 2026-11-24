use std::path::Path;
use std::sync::Arc;

use super::core::Core;
/// 单语言样本文件上限（防无限膨胀；超出后保留最新 N 条）
const L3_MAX_SAMPLES: usize = 5000;

/// 提醒阈值：上次重训练后样本 ≥ 该值 → retrain_ready
const L3_RETRAIN_READY_POS: usize = 10;

/// 提醒阈值：从未重训练过时，累计样本 ≥ 该值即可
const L3_RETRAIN_READY_FIRST: usize = 30;

/// 距上次重训练至少 7 天才再次提醒（从未训练过不限时间）
const L3_RETRAIN_COOLDOWN: i64 = 7 * 24 * 3600;

/// 追加一条样本到 `l3_data/<lang>/confirmed.jsonl`（按 文本+标签 去重，上限截断）
pub(crate) fn l3_collect_sample(
    dir: &Path,
    lang: &str,
    sample: &serde_json::Value,
) -> Option<usize> {
    let lang_dir = dir.join(lang);
    if let Err(e) = std::fs::create_dir_all(&lang_dir) {
        tracing::warn!(lang, error = %e, "L3 飞轮：创建样本目录失败");
        return None;
    }
    let path = lang_dir.join("confirmed.jsonl");
    let mut lines: Vec<String> = match std::fs::read_to_string(&path) {
        Ok(s) => s
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.to_string())
            .collect(),
        Err(_) => Vec::new(),
    };
    let key = l3_sample_key(sample);
    let dup = lines.iter().any(|l| {
        serde_json::from_str::<serde_json::Value>(l)
            .map(|v| l3_sample_key(&v) == key)
            .unwrap_or(false)
    });
    if dup {
        return None;
    }
    lines.push(serde_json::to_string(sample).unwrap_or_default());
    if lines.len() > L3_MAX_SAMPLES {
        let drop = lines.len() - L3_MAX_SAMPLES;
        lines.drain(0..drop);
    }
    if let Err(e) = std::fs::write(&path, format!("\n{}", lines.join("\n"))) {
        tracing::warn!(lang, error = %e, "L3 飞轮：样本写入失败");
        return None;
    }
    Some(lines.len())
}

/// 样本去重键：`文本|标签`
fn l3_sample_key(v: &serde_json::Value) -> String {
    format!(
        "{}|{}",
        v.get("text").and_then(|v| v.as_str()).unwrap_or(""),
        v.get("label").and_then(|v| v.as_u64()).unwrap_or(1)
    )
}

/// 读取某语言样本总数（文件缺失 = 0）
fn l3_count_samples(dir: &Path, lang: &str) -> usize {
    let path = dir.join(lang).join("confirmed.jsonl");
    match std::fs::read_to_string(&path) {
        Ok(s) => s.lines().filter(|l| !l.trim().is_empty()).count(),
        Err(_) => 0,
    }
}

// ── 采纳 / 放弃沉淀资产（adopted / rejected；与 api-server routes/l3.rs 同构）──

/// 采纳平行语料与负偏好池上限（长期训练资产）
const L3_MAX_ADOPTED: usize = 20000;

/// 句对编辑密度上限：超过视为「用户改写」而非「纠错」，打 weak 标记
const L3_WEAK_EDIT_RATIO: f64 = 0.4;

/// 采纳/放弃沉淀开关：**默认开**（桌面端自用 = 本机单用户）；显式 `GEC_L3_ADOPT_COLLECT=0` 关闭
/// （api-server 形态默认相反：默认关、`=1` 开启，见 api-server `routes/l3.rs`）
fn adopt_collect_enabled() -> bool {
    match std::env::var("GEC_L3_ADOPT_COLLECT") {
        Ok(v) => v != "0" && !v.eq_ignore_ascii_case("false"),
        Err(_) => true,
    }
}

/// 简易切句（。！？；.!? + 换行，≥4 字）
fn l3_split_sentences(text: &str) -> Vec<String> {
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

/// 字符编辑密度（1 - LCS/max_len）；超长句用长度差近似
fn l3_char_diff_ratio(a: &str, b: &str) -> f64 {
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

/// 追加一条 JSONL 记录到 `l3_data/<lang>/<file>`（按 `key_of` 去重，超 `max` 截断保留最新）
fn l3_append_jsonl(
    dir: &Path,
    lang: &str,
    file: &str,
    sample: &serde_json::Value,
    max: usize,
    key_of: &dyn Fn(&serde_json::Value) -> String,
) -> Option<usize> {
    let lang_dir = dir.join(lang);
    if let Err(e) = std::fs::create_dir_all(&lang_dir) {
        tracing::warn!(lang, error = %e, "L3 沉淀：创建样本目录失败");
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
        tracing::warn!(lang, error = %e, "L3 沉淀：样本写入失败");
        return None;
    }
    Some(lines.len())
}

/// 采纳/放弃反馈的资产沉淀（非阻断）：
/// - adopt + `ctx.src_text`/`ctx.corrected_text` → 切句对齐写 `adopted.jsonl`（句对平行语料）
/// - reject → 片段级负偏好写 `rejected.jsonl`（偏好/DPO 材料）
pub(crate) fn collect_feedback_assets_cmd(
    core: &Core,
    lang: &str,
    error: &shared_model::Error,
    action: &str,
    reason: &Option<String>,
    ctx: Option<&serde_json::Value>,
) {
    if !adopt_collect_enabled() || !matches!(lang, "en" | "zh") {
        return;
    }
    let dir = core.l3_data.clone();
    match action {
        "adopt" => {
            let src = ctx
                .and_then(|c| c.get("src_text"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let dst = ctx
                .and_then(|c| c.get("corrected_text"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if src.trim().is_empty() || dst.trim().is_empty() {
                return;
            }
            let src_s = l3_split_sentences(src);
            let dst_s = l3_split_sentences(dst);
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
                    "weak": l3_char_diff_ratio(&s, &d) > L3_WEAK_EDIT_RATIO,
                    "source": "adopt",
                    "ts": chrono::Utc::now().to_rfc3339(),
                });
                if l3_append_jsonl(
                    &dir,
                    lang,
                    "adopted.jsonl",
                    &sample,
                    L3_MAX_ADOPTED,
                    &key_of,
                )
                .is_some()
                {
                    added += 1;
                }
            }
            if added > 0 {
                tracing::info!(lang, added, "沉淀：采纳平行句对已汇入 adopted.jsonl");
            }
        }
        "reject" => {
            let sample = serde_json::json!({
                "fragment": error.original,
                "rejected_correction": error.correction,
                "error_type": error.error_type,
                "source_layer": error.source,
                "reason": reason.clone().unwrap_or_default(),
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
            if l3_append_jsonl(
                &dir,
                lang,
                "rejected.jsonl",
                &sample,
                L3_MAX_ADOPTED,
                &key_of,
            )
            .is_some()
            {
                tracing::info!(lang, "沉淀：放弃负偏好已汇入 rejected.jsonl");
            }
        }
        _ => {}
    }
}

/// JSONL 资产计数（文件缺失 = 0）
fn l3_count_jsonl(dir: &Path, lang: &str, file: &str) -> usize {
    let path = dir.join(lang).join(file);
    match std::fs::read_to_string(&path) {
        Ok(s) => s.lines().filter(|l| !l.trim().is_empty()).count(),
        Err(_) => 0,
    }
}

/// 读取上次重训练时间（feature-model 写的 retrain_meta.json）
fn l3_last_retrain_at(dir: &Path, lang: &str) -> Option<String> {
    let s = std::fs::read_to_string(dir.join("retrain_meta.json")).ok()?;
    let meta = serde_json::from_str::<serde_json::Value>(&s).ok()?;
    meta.get(lang)?
        .get("ts")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// 是否建议重训练（样本积累够 + 冷却期）
fn l3_retrain_ready(total: usize, last_at: Option<&str>) -> bool {
    let enough = match last_at {
        Some(_) => total >= L3_RETRAIN_READY_POS,
        None => total >= L3_RETRAIN_READY_FIRST,
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
            chrono::Utc::now().timestamp() - secs >= L3_RETRAIN_COOLDOWN
        }
    }
}

/// L3 飞轮：采集一条样本（前端「采纳」LLM 错误 / 干净句自动汇入）
#[tauri::command]
pub async fn l3_collect(
    core: tauri::State<'_, Arc<Core>>,
    lang: String,
    text: String,
    label: Option<u8>,
    error_type: Option<String>,
    source: Option<String>,
) -> Result<serde_json::Value, String> {
    if !matches!(lang.as_str(), "en" | "zh") {
        return Err(format!("不支持的语言: {lang}"));
    }
    if text.trim().is_empty() {
        return Err("text 不能为空".into());
    }
    let label = label.unwrap_or(1);
    let sample = serde_json::json!({
        "text": text,
        "label": label,
        "error_type": error_type.unwrap_or_default(),
        "source": source.unwrap_or_else(|| {
            if label == 1 {
                "llm_adopt".to_string()
            } else {
                "llm_clean".to_string()
            }
        }),
        "ts": chrono::Utc::now().to_rfc3339(),
    });
    match l3_collect_sample(&core.l3_data, &lang, &sample) {
        Some(total) => Ok(serde_json::json!({ "added": true, "total": total })),
        None => Ok(serde_json::json!({
            "added": false,
            "total": l3_count_samples(&core.l3_data, &lang),
        })),
    }
}

/// L3 飞轮：状态查询（样本数 + 上次重训练 + 当前模型 F1）
#[tauri::command]
pub async fn l3_status(
    core: tauri::State<'_, Arc<Core>>,
    lang: Option<String>,
) -> Result<serde_json::Value, String> {
    let lang = lang.unwrap_or_else(|| "zh".to_string());
    if !matches!(lang.as_str(), "en" | "zh") {
        return Err(format!("不支持的语言: {lang}"));
    }
    let dir = core.l3_data.clone();
    let total = l3_count_samples(&dir, &lang);
    let last = l3_last_retrain_at(&dir, &lang);
    // 记录实际解析的数据目录：与 feature-model 服务读取目录不一致时重训看不到样本
    // （开发 = 仓库内 feature-model/l3_data；打包应用 = ~/.langfixer/l3_data；
    // Python 服务侧可用 L3_DATA_DIR 环境变量对齐）
    tracing::info!(data_dir = %dir.display(), "L3 飞轮数据目录（解析结果）");

    let cfg = core.settings.read().unwrap().l3.clone();
    let (service_available, model, f1) = match reqwest::Client::new()
        .get(format!("{}/l3/status", cfg.endpoint.trim_end_matches('/')))
        .timeout(std::time::Duration::from_secs(5))
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

    Ok(serde_json::json!({
        "samples": total,
        "adopted": l3_count_jsonl(&dir, &lang, "adopted.jsonl"),
        "rejected": l3_count_jsonl(&dir, &lang, "rejected.jsonl"),
        "last_retrain_at": last,
        "retrain_ready": l3_retrain_ready(total, last.as_deref()),
        "service_available": service_available,
        "model": model,
        "f1": f1,
        "data_dir": dir.to_string_lossy().into_owned(),
    }))
}

/// L3 飞轮：触发重训练（转发 feature-model POST /l3-retrain）
#[tauri::command]
pub async fn l3_retrain(
    core: tauri::State<'_, Arc<Core>>,
    lang: String,
) -> Result<serde_json::Value, String> {
    l3_retrain_forward(&core, &lang).await
}

/// L3 重训练转发（`l3_retrain` 命令与训练中心 `train_run_l3_retrain` 共用）
pub(crate) async fn l3_retrain_forward(
    core: &Core,
    lang: &str,
) -> Result<serde_json::Value, String> {
    if !matches!(lang, "en" | "zh") {
        return Err(format!("不支持的语言: {lang}"));
    }
    let cfg = core.settings.read().unwrap().l3.clone();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(cfg.timeout_secs.max(120)))
        .build()
        .map_err(|e| format!("HTTP 客户端构建失败: {e}"))?;
    let resp = client
        .post(format!("{}/l3-retrain", cfg.endpoint.trim_end_matches('/')))
        .json(&serde_json::json!({ "lang": lang }))
        .send()
        .await
        .map_err(|e| format!("特征模型服务不可达（{e}"))?;
    let (code, body) = (resp.status(), resp.json::<serde_json::Value>().await);
    let body = body.map_err(|e| format!("特征模型响应解析失败: {e}"))?;
    if !code.is_success() {
        let msg = body
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("重训练失败");
        return Err(msg.to_string());
    }
    // 记录本次重训练时间（feature-model 侧也会写 retrain_meta.json，双写以本地为准）
    let dir = core.l3_data.clone();
    let retrain_at = chrono::Utc::now().to_rfc3339();
    let meta_path = dir.join("retrain_meta.json");
    let existing = std::fs::read_to_string(&meta_path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
    let mut meta = existing.unwrap_or_default();
    if !meta.is_object() {
        meta = serde_json::Map::new().into();
    }
    meta[lang] = serde_json::json!({
        "ts": retrain_at,
        "confirmed": l3_count_samples(&dir, lang),
        "source": "desktop-app",
    });
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(&meta_path, meta.to_string());

    Ok(serde_json::json!({
        "swapped": body.get("swapped").and_then(|v| v.as_bool()).unwrap_or(false),
        "reason": body.get("reason").and_then(|v| v.as_str()),
        "before_f1": body.get("before_f1").and_then(|v| v.as_f64()),
        "after_f1": body.get("after_f1").and_then(|v| v.as_f64()),
        "samples": body.get("samples").cloned(),
        "retrain_at": retrain_at,
    }))
}
// ─────────────────────────────────────────────────────────────────────────────
// 统一反馈 + 反思流程（镜像 api-server `/gec/feedback*`）
// ─────────────────────────────────────────────────────────────────────────────

/// L3 飞轮样本采集（非阻断；en/zh 才有 L3 模型）
pub(crate) fn l3_collect_feedback(
    core: &Core,
    lang: &str,
    error: &shared_model::Error,
    label: u8,
    source: &str,
) {
    if !matches!(lang, "en" | "zh") {
        return;
    }
    let sample = serde_json::json!({
        "text": error.original,
        "label": label,
        "error_type": if label == 1 { error.error_type.to_string() } else { String::new() },
        "source": source,
        "ts": chrono::Utc::now().to_rfc3339(),
    });
    let _ = l3_collect_sample(&core.l3_data, lang, &sample);
}
