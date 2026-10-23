//! L3 特征模型 GEC 引擎：feature-model 服务的 HTTP 客户端
//!
//! 三级响应 level 3 的主力层（三期「规模化」）：在 LLM 之前运行
//! 轻量特征模型（`feature-model/`，默认 `http://127.0.0.1:4806`）：
//! - **句级特征分类**（has_error + 错误类型，gold 数据训练）；
//! - **模式化 span 定位 + 机械修正**（的/地/得、时态、冠词等可机械修正的类型）；
//! - **高置信判定直接定案**（有错 → 产出错误项；明确无错 → 放行），
//!   LLM 收缩到**纯长尾**：仅灰区弃权 / 服务不可用时才调用。
//!
//! 协议（feature-model/server.py）：
//! `POST {endpoint}/l3-detect`，请求 `{"text": ..., "lang": ...}`，
//! 响应 `{"has_error": bool, "has_error_prob": 0.9, "error_type": "word_order",
//!         "abstain": false,
//!         "unreviewed_spans": [[28,35]],
//!         "errors": [{error_type, confidence, span, original, correction?, message?}]}`。
//!
//! 映射规则（与决策层同构）：
//! - 服务返回 `errors`（span 级）→ 直接映射为 [`Error`]（source = `feature`），非弃权；
//!   注意：服务返回的 `span` 是**字符偏移**（Python 语义），本侧经
//!   [`crate::engine::char_span_to_bytes`] 换算为字节偏移（CJK 文本两者相差约 3 倍，
//!   不换算会导致 `merge_errors` 的 span 重叠去重对中文误判）；
//! - 无 `errors` 且（显式 `abstain` 或概率落在灰区 `[GRAY_ZONE_LOW, abstain_threshold)`）
//!   → 弃权（长尾，交由 LLM 兜底）；
//! - 无 `errors` 且概率低于灰区下界 → 明确无错（不触发 LLM）；
//! - **部分定案契约**（`abstain=false` 且 `unreviewed_spans` 非空）：服务端多数决
//!   接受了部分定案，`unreviewed_spans` 为灰区句 span（字符偏移，本侧换算为字节）——
//!   不算整体弃权，引擎侧对这些句做**定向 LLM 复查**而非全文兜底。
//!
//! 环境变量：`GEC_L3_ENDPOINT`（默认 `http://127.0.0.1:4806`）/
//! `GEC_L3_TIMEOUT_SECS`（默认 10）/ `GEC_L3_MODEL`（默认 `feature-0.1`）/
//! `GEC_L3_ABSTAIN_THRESHOLD`（默认 0.5）/ `GEC_L3_ENABLED`
//! （特征模型开关：**默认开启**；显式 `GEC_L3_ENABLED=0`/`false` 关闭，
//! level 3 回退「无条件 LLM」的旧行为）。
//! 运行时设置（设置界面 / 设置文件）中的 `l3` 字段优先于环境变量。

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::circuit::CircuitBreaker;
use crate::detector::DetectorError;
use crate::engine::char_span_to_bytes;
use shared_model::{Error, error_types, sources};

/// 飞轮数据目录缺省值（未设置 `GEC_L3_DATA_DIR` 环境变量时）
///
/// - 开发模式（CWD 下有 `feature-model/` 服务目录，即从仓库根运行）：
///   用仓库内 `feature-model/l3_data` —— 与 feature-model 服务的缺省读取目录一致，
///   采集与重训天然对齐；
/// - 打包应用（CWD 不是仓库根，如 Tauri 打包后）：回退到稳定的用户级目录
///   `~/.langfixer/l3_data`，避免飞轮数据写到随启动位置变化的 CWD；
///   Python 服务侧可用 `L3_DATA_DIR` 环境变量对齐到同一目录。
pub fn default_l3_data_dir() -> PathBuf {
    if Path::new("feature-model").is_dir() {
        return PathBuf::from("feature-model/l3_data");
    }
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        return Path::new(&home).join(".langfixer").join("l3_data");
    }
    PathBuf::from("feature-model/l3_data")
}

fn default_endpoint() -> String {
    "http://127.0.0.1:4806".to_string()
}
fn default_timeout_secs() -> u64 {
    10
}
fn default_model() -> String {
    "feature-0.1".to_string()
}
fn default_abstain_threshold() -> f64 {
    0.5
}
/// 特征模型层默认**启用**（level 3 先跑特征模型；opt-out 见 `GEC_L3_ENABLED` / 设置界面）
fn default_enabled() -> bool {
    true
}

/// L3 特征模型服务配置
///
/// 可 JSON 序列化（`#[serde(default)]` 逐字段缺省），供运行时设置持久化；
/// 设置文件 > 环境变量 > 内置默认。
#[derive(Debug, Clone, PartialEq, serde::Serialize, Deserialize)]
pub struct L3FeatureConfig {
    /// 特征模型服务地址，如 `http://127.0.0.1:4806`
    #[serde(default = "default_endpoint")]
    pub endpoint: String,
    /// 请求超时（秒）
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    /// 模型名（日志 / 元数据；微调后为 checkpoint 名）
    #[serde(default = "default_model")]
    pub model: String,
    /// 弃权阈值：句级 `has_error_prob` 落在 `[GRAY_ZONE_LOW, abstain_threshold)` 视为弃权
    #[serde(default = "default_abstain_threshold")]
    pub abstain_threshold: f64,
    /// 特征模型层开关：**默认 `true`**（level 3 先跑特征模型，LLM 收缩到长尾）；
    /// 设置界面 / 设置文件 / `GEC_L3_ENABLED=0` 可显式关闭（回退无条件 LLM）
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

impl Default for L3FeatureConfig {
    fn default() -> Self {
        Self {
            endpoint: default_endpoint(),
            timeout_secs: default_timeout_secs(),
            model: default_model(),
            abstain_threshold: default_abstain_threshold(),
            enabled: default_enabled(),
        }
    }
}

impl L3FeatureConfig {
    /// 特征模型层是否启用（**默认 true**；设置界面开关 / `GEC_L3_ENABLED` 可显式覆盖）
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// 从环境变量构建（设置文件缺失时的默认来源）：
    /// `GEC_L3_ENDPOINT` / `GEC_L3_TIMEOUT_SECS` / `GEC_L3_MODEL` /
    /// `GEC_L3_ABSTAIN_THRESHOLD` / `GEC_L3_ENABLED`
    pub fn from_env() -> Self {
        let mut cfg = Self::default();
        if let Some(v) = std::env::var("GEC_L3_ENDPOINT")
            .ok()
            .filter(|s| !s.is_empty())
        {
            cfg.endpoint = v;
        }
        if let Some(v) = std::env::var("GEC_L3_TIMEOUT_SECS").ok() {
            if let Ok(t) = v.parse::<u64>() {
                cfg.timeout_secs = t.max(1);
            }
        }
        if let Some(v) = std::env::var("GEC_L3_MODEL").ok().filter(|s| !s.is_empty()) {
            cfg.model = v;
        }
        if let Some(v) = std::env::var("GEC_L3_ABSTAIN_THRESHOLD").ok() {
            if let Ok(t) = v.parse::<f64>() {
                cfg.abstain_threshold = t.clamp(0.0, 1.0);
            }
        }
        if let Some(v) = std::env::var("GEC_L3_ENABLED").ok() {
            cfg.enabled = match v.to_lowercase().as_str() {
                "0" | "false" | "off" | "no" => false,
                _ => true,
            };
        }
        cfg
    }
}

/// L3 特征模型引擎：feature-model 服务的 HTTP 客户端
pub struct L3FeatureEngine {
    lang: String,
    client: reqwest::Client,
    config: L3FeatureConfig,
    /// 熔断器：服务宕机连续失败后跳闸，冷却期内请求立即失败（毫秒级），
    /// 避免每次调用都等满超时
    breaker: CircuitBreaker,
}

impl L3FeatureEngine {
    pub fn new(lang: impl Into<String>, config: L3FeatureConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs.max(1)))
            .build()
            .unwrap_or_default();
        Self {
            lang: lang.into(),
            client,
            config,
            breaker: CircuitBreaker::default(),
        }
    }

    /// 从环境变量（`GEC_L3_*`）构建
    pub fn from_env(lang: impl Into<String>) -> Self {
        Self::new(lang, L3FeatureConfig::from_env())
    }

    /// 调用特征模型服务，返回错误列表 + 弃权标记
    ///
    /// 弃权判定（level 3 的 LLM 长尾触发信号）：
    /// - 显式 `abstain` 标志（服务方按契约输出）；
    /// - 或：未产出错误且 `has_error_prob` 落在灰区
    ///   `[GRAY_ZONE_LOW, abstain_threshold)`（不输出 abstain 字段的旧服务兜底判定）。
    /// 概率低于灰区下界视为「明确无错」，不触发 LLM。
    pub async fn detect(&self, text: &str) -> Result<L3Outcome, DetectorError> {
        // 熔断器：服务宕机跳闸后冷却期内立即失败（毫秒级），
        // 避免每次调用都等满超时（调用方按既有回退路径处理）
        if !self.breaker.allow_request() {
            return Err(DetectorError::ServiceUnavailable(
                "L3 特征模型服务已熔断（连续失败），暂跳过；冷却到期后自动探测恢复".into(),
            ));
        }
        let url = format!("{}/l3-detect", self.config.endpoint.trim_end_matches('/'));
        let resp = self
            .client
            .post(&url)
            .json(&serde_json::json!({ "text": text, "lang": self.lang }))
            .send()
            .await
            .map_err(|e| {
                self.breaker.on_failure();
                DetectorError::ServiceUnavailable(e.to_string())
            })?;

        if !resp.status().is_success() {
            self.breaker.on_failure();
            return Err(DetectorError::ServiceUnavailable(format!(
                "L3 特征模型服务返回 HTTP {}",
                resp.status()
            )));
        }

        let body: L3DetectResponse = resp.json().await.map_err(|e| {
            self.breaker.on_failure();
            DetectorError::ServiceUnavailable(e.to_string())
        })?;
        self.breaker.on_success();

        // 部分定案契约：abstain=false 且带灰区句 span → 不算整体弃权
        // （引擎对 unreviewed_spans 做定向 LLM 复查）；span 字符偏移 → 字节偏移
        let partial = !body.abstain && !body.unreviewed_spans.is_empty();
        let unreviewed: Vec<[usize; 2]> = body
            .unreviewed_spans
            .iter()
            .map(|s| char_span_to_bytes(text, *s))
            .collect();

        // 1. 服务返回 span 级错误 → 直接映射（明确判断，非弃权）；
        //    服务 span 为字符偏移（Python 语义），换算为字节偏移后入 `Error.span`
        if !body.errors.is_empty() {
            let errors = body
                .errors
                .into_iter()
                .map(|e| {
                    let mut err = Error::new(
                        &self.lang,
                        map_error_type(&e.error_type),
                        e.confidence,
                        char_span_to_bytes(text, e.span),
                        e.original,
                        e.correction,
                        if e.message.is_empty() {
                            "特征模型检出错误"
                        } else {
                            &e.message
                        },
                    );
                    err.source = Some(sources::FEATURE.to_string());
                    err
                })
                .collect();
            return Ok(L3Outcome {
                errors,
                abstained: false,
                unreviewed_spans: unreviewed,
            });
        }

        // 2. 无错误：弃权（显式标志 / 概率灰区）或明确无错；
        //    部分定案（带 unreviewed_spans）不视为整体弃权
        if body.abstain || !body.has_error {
            let abstained = !partial
                && (body.abstain
                    || (body.has_error_prob >= GRAY_ZONE_LOW
                        && body.has_error_prob < self.config.abstain_threshold));
            return Ok(L3Outcome {
                errors: Vec::new(),
                abstained,
                unreviewed_spans: unreviewed,
            });
        }
        // has_error=true 但无 span 级错误且概率低于阈值 → 灰区弃权
        let prob = if body.has_error_prob > 0.0 {
            body.has_error_prob
        } else {
            0.5
        };
        if prob < self.config.abstain_threshold {
            let abstained = !partial && (body.abstain || prob >= GRAY_ZONE_LOW);
            return Ok(L3Outcome {
                errors: Vec::new(),
                abstained,
                unreviewed_spans: unreviewed,
            });
        }
        // 概率达标但服务未给出 span 级错误 → 合成句级错误
        let model_type = body
            .error_type
            .clone()
            .unwrap_or_else(|| "grammar".to_string());
        let mut err = Error::new(
            &self.lang,
            map_error_type(&model_type),
            prob,
            [0, text.len()],
            text.to_string(),
            None,
            sentence_message(&model_type),
        );
        err.source = Some(sources::FEATURE.to_string());
        Ok(L3Outcome {
            errors: vec![err],
            abstained: false,
            unreviewed_spans: unreviewed,
        })
    }
}

/// 特征模型判断结果：错误列表 + 弃权标记（level 3 的 LLM 长尾触发信号）
#[derive(Debug, Clone, Default)]
pub struct L3Outcome {
    /// 特征模型产出的错误（span 级或句级合成）
    pub errors: Vec<Error>,
    /// 特征模型弃权（显式 abstain 标志，或概率灰区且未产出错误；
    /// 部分定案契约——`abstain=false` 且带 `unreviewed_spans`——不算弃权）
    pub abstained: bool,
    /// 部分定案时服务端报告的灰区句 span（**字节偏移**，已换算）：
    /// 引擎侧对这些 span 做定向 LLM 复查，而非全文兜底
    pub unreviewed_spans: Vec<[usize; 2]>,
}

/// 弃权灰区下界：与决策层一致（`[0.25, abstain_threshold)` 视为弃权）
const GRAY_ZONE_LOW: f64 = 0.25;

/// feature-model 服务 `/l3-detect` 响应
#[derive(Debug, Deserialize)]
struct L3DetectResponse {
    #[serde(default)]
    has_error: bool,
    #[serde(default)]
    has_error_prob: f64,
    #[serde(default)]
    error_type: Option<String>,
    #[serde(default)]
    abstain: bool,
    #[serde(default)]
    errors: Vec<L3SpanError>,
    /// 部分定案时的灰区句 span（字符偏移，Python 语义）
    #[serde(default)]
    unreviewed_spans: Vec<[usize; 2]>,
}

/// span 级错误（特征模型的模式化定位 + 机械修正）
#[derive(Debug, Deserialize)]
struct L3SpanError {
    error_type: String,
    #[serde(default = "default_confidence")]
    confidence: f64,
    span: [usize; 2],
    original: String,
    #[serde(default)]
    correction: Option<String>,
    #[serde(default)]
    message: String,
}

fn default_confidence() -> f64 {
    0.5
}

/// 特征模型错误类型 → 统一错误类型（与决策层 `map_error_type` 对齐）
fn map_error_type(model_type: &str) -> String {
    match model_type {
        "collocation" => error_types::COLLOCATION.to_string(),
        "word_order" => error_types::WORD_ORDER.to_string(),
        // 成分残缺 / 句式杂糅 / 逻辑不通 → 统一归入 grammar
        "missing" | "structure" | "logic" | "none" => error_types::GRAMMAR.to_string(),
        other => other.to_string(),
    }
}

/// 句级错误的默认说明（按特征模型错误类型）
fn sentence_message(model_type: &str) -> String {
    match model_type {
        "collocation" => "特征模型检出搭配不当".to_string(),
        "word_order" => "特征模型检出语序不当".to_string(),
        "missing" => "特征模型检出成分残缺或赘余".to_string(),
        "structure" => "特征模型检出句式杂糅 / 结构混乱".to_string(),
        "logic" => "特征模型检出逻辑不通 / 语义矛盾".to_string(),
        other => format!("特征模型检出语病（{other}）"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;

    fn config(endpoint: &str) -> L3FeatureConfig {
        L3FeatureConfig {
            endpoint: endpoint.to_string(),
            ..Default::default()
        }
    }

    /// 本地 mock feature-model 服务（固定 /l3-detect 响应）
    async fn mock_l3_server(body: &'static str) -> String {
        let app = axum::Router::new().route(
            "/l3-detect",
            axum::routing::post(move |b: axum::body::Bytes| async move {
                let req: serde_json::Value = serde_json::from_slice(&b).unwrap();
                assert!(req.get("text").is_some(), "请求应携带 text");
                assert!(req.get("lang").is_some(), "请求应携带 lang");
                (
                    axum::http::StatusCode::OK,
                    axum::Json(serde_json::from_str::<serde_json::Value>(body).unwrap()),
                )
                    .into_response()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        format!("http://{addr}")
    }

    #[test]
    fn config_defaults() {
        let cfg = L3FeatureConfig::default();
        assert_eq!(cfg.endpoint, "http://127.0.0.1:4806");
        assert_eq!(cfg.timeout_secs, 10);
        assert_eq!(cfg.model, "feature-0.1");
        assert_eq!(cfg.abstain_threshold, 0.5);
        assert!(cfg.enabled());
    }

    #[test]
    fn config_old_file_missing_enabled_defaults_on() {
        // 旧版设置文件缺失 enabled 字段 → 按启用处理（serde 逐字段缺省）
        let old = r#"{"endpoint":"http://10.0.0.9:4806","timeout_secs":5,"model":"m","abstain_threshold":0.6}"#;
        let cfg: L3FeatureConfig = serde_json::from_str(old).unwrap();
        assert!(cfg.enabled());
    }

    #[test]
    fn map_error_type_alignment() {
        assert_eq!(map_error_type("collocation"), error_types::COLLOCATION);
        assert_eq!(map_error_type("word_order"), error_types::WORD_ORDER);
        assert_eq!(map_error_type("structure"), error_types::GRAMMAR);
        assert_eq!(map_error_type("logic"), error_types::GRAMMAR);
        assert_eq!(map_error_type("tense"), "tense");
    }

    #[tokio::test]
    async fn detect_span_errors_mapped() {
        let endpoint = mock_l3_server(
            r#"{"has_error":true,"has_error_prob":0.9,
               "error_type":"word_order","abstain":false,
               "errors":[{"error_type":"word_order","confidence":0.9,
                         "span":[0,11],"original":"I go school","correction":"I go to school",
                         "message":"缺少介词 to"}]}"#,
        )
        .await;
        let engine = L3FeatureEngine::new("en", config(&endpoint));
        let o = engine.detect("I go school.").await.unwrap();
        assert!(!o.abstained);
        assert_eq!(o.errors.len(), 1);
        assert_eq!(o.errors[0].source.as_deref(), Some(sources::FEATURE));
        assert_eq!(o.errors[0].error_type, error_types::WORD_ORDER);
        assert_eq!(o.errors[0].correction.as_deref(), Some("I go to school"));
        assert_eq!(o.errors[0].span, [0, 11]);
    }

    #[tokio::test]
    async fn detect_abstain_flag() {
        let endpoint = mock_l3_server(
            r#"{"has_error":false,"has_error_prob":0.4,"error_type":"","abstain":true,"errors":[]}"#,
        )
        .await;
        let engine = L3FeatureEngine::new("en", config(&endpoint));
        let o = engine.detect("clean text").await.unwrap();
        assert!(o.abstained);
        assert!(o.errors.is_empty());
    }

    #[tokio::test]
    async fn detect_confident_clean_not_abstain() {
        let endpoint = mock_l3_server(
            r#"{"has_error":false,"has_error_prob":0.05,"error_type":"","abstain":false,"errors":[]}"#,
        )
        .await;
        let engine = L3FeatureEngine::new("en", config(&endpoint));
        let o = engine.detect("clean text").await.unwrap();
        assert!(!o.abstained);
        assert!(o.errors.is_empty());
    }

    #[tokio::test]
    async fn detect_gray_zone_abstain() {
        // 概率落在 [0.25, 0.5) 灰区且无显式 abstain → 兜底弃权
        let endpoint = mock_l3_server(
            r#"{"has_error":false,"has_error_prob":0.35,"error_type":"","abstain":false,"errors":[]}"#,
        )
        .await;
        let engine = L3FeatureEngine::new("en", config(&endpoint));
        let o = engine.detect("maybe").await.unwrap();
        assert!(o.abstained);
    }

    #[tokio::test]
    async fn detect_partial_accept_unreviewed_spans() {
        // 部分定案契约：abstain=false + unreviewed_spans → 不算整体弃权，
        // span 由字符偏移换算为字节偏移（CJK 每字 3 字节）
        let endpoint = mock_l3_server(
            r#"{"has_error":false,"has_error_prob":0.30,"error_type":"",
               "abstain":false,"errors":[],
               "unreviewed_spans":[[0,5],[8,12]]}"#,
        )
        .await;
        let engine = L3FeatureEngine::new("zh", config(&endpoint));
        let o = engine.detect("他今天心情很好。我也一样。").await.unwrap();
        assert!(!o.abstained, "部分定案不应视为整体弃权");
        assert!(o.errors.is_empty());
        // 字符 [0,5) → 字节 [0,15)；字符 [8,12) → 字节 [24,36)
        assert_eq!(o.unreviewed_spans, vec![[0, 15], [24, 36]]);
    }

    #[tokio::test]
    async fn detect_full_abstain_ignores_gray_fallback_with_spans() {
        // 显式 abstain=true（整体弃权）时即使带 unreviewed_spans 也保持弃权语义
        let endpoint = mock_l3_server(
            r#"{"has_error":false,"has_error_prob":0.4,"error_type":"",
               "abstain":true,"errors":[],"unreviewed_spans":[[0,3]]}"#,
        )
        .await;
        let engine = L3FeatureEngine::new("zh", config(&endpoint));
        let o = engine.detect("他今天心情很好。").await.unwrap();
        assert!(o.abstained);
    }

    #[tokio::test]
    async fn detect_sentence_level_synthesis() {
        // has_error=true + 概率达标 + 无 span 级错误 → 合成句级错误
        let endpoint = mock_l3_server(
            r#"{"has_error":true,"has_error_prob":0.8,"error_type":"collocation","abstain":false,"errors":[]}"#,
        )
        .await;
        let engine = L3FeatureEngine::new("en", config(&endpoint));
        let o = engine.detect("a apple on tree").await.unwrap();
        assert!(!o.abstained);
        assert_eq!(o.errors.len(), 1);
        assert_eq!(o.errors[0].error_type, error_types::COLLOCATION);
        assert_eq!(o.errors[0].span, [0, "a apple on tree".len()]);
        assert!(o.errors[0].correction.is_none());
    }

    #[tokio::test]
    async fn detect_service_down_is_unavailable() {
        let engine = L3FeatureEngine::new(
            "en",
            L3FeatureConfig {
                endpoint: "http://127.0.0.1:1".to_string(),
                timeout_secs: 1,
                ..Default::default()
            },
        );
        let e = engine.detect("x").await.unwrap_err();
        assert!(matches!(e, DetectorError::ServiceUnavailable(_)));
    }
}
