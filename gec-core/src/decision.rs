//! 决策模型 GEC 引擎：vegaml 决策模型服务的 HTTP 客户端
//!
//! 三层检测级联（规则 → 决策模型 → LLM）中的第二层：
//! - **规则引擎**（各语言检测器）：以最快速度决定最显式的错误，
//!   调整词表 / 规则立即生效，但无法覆盖所有情况；
//! - **决策模型**（本模块，vegaml）：处理相对模糊的情况
//!   （「是否有语病 + 主要错误类型」），支持零样本（engine 模式：
//!   校准 + conformal + abstain）与微调（TTT fit / adapter，见 `decision-model/`）；
//! - **LLM**（`langfixer-llm`）：讲解层 / 预留检测引擎。
//!
//! 协议（vegaml 服务，见 `decision-model/server.py`）：
//! `POST {endpoint}/decide`，请求 `{"text": ..., "lang": ...}`，
//! 响应 `{"has_error": bool, "has_error_prob": 0.87, "error_type": "missing",
//!          "abstain": false, "prediction_set": ["true"], "readout": "engine",
//!          "errors": [{error_type, confidence, span, original, correction?, message?}]?}`；
//! `GET {endpoint}/health` → `{"status": "ok", "mode": ..., "versions": {lang: 版本号}}`
//! （模型版本：每次重训练自增 `v1, v2, ...`，见 `decision-model/vega_finetune.py`；
//! 本侧经 [`DecisionGecEngine::model_version`] 查询并短 TTL 缓存，供历史复用门槛比对）。
//!
//! 映射规则：
//! - 服务返回 span 级 `errors`（微调后服务可细化）→ 直接映射为 [`Error`]；
//!   注意：服务返回的 `span` 是**字符偏移**（Python 语义），本侧经
//!   [`engine::char_span_to_bytes`] 换算为字节偏移（CJK 文本两者相差约 3 倍，
//!   不换算会导致 `merge_errors` 的 span 重叠去重对中文误判）；
//! - 否则 `has_error` 且非弃权且 `has_error_prob >= abstain_threshold`
//!   → 合成句级错误（span = 全文，`error_type` 按 vegaml 类型映射，
//!   `confidence` = 校准后的 `has_error_prob`）；
//! - 模型弃权（`abstain`）或概率低于阈值 → 不产出错误（模糊情况宁缺毋滥，
//!   避免误报；后续由 LLM 讲解层兜底）。
//!
//! 环境变量：`GEC_DEC_ENDPOINT`（默认 `http://127.0.0.1:4804`）/
//! `GEC_DEC_TIMEOUT_SECS`（默认 10）/ `GEC_DEC_MODEL`（默认 `vegaml-0.8b`）/
//! `GEC_DEC_ABSTAIN_THRESHOLD`（默认 0.5）/ `GEC_DEC_ENABLED`
//! （决策层开关：**默认关闭**（P0.5：L3 特征模型是唯一的 pre-LLM 门控，
//! 决策模型与 L3 同为句级二分类门控且 L3 严格更强，两者同开时决策模型是冗余层；
//! 见仓库根 `EVOLUTION.md` §1）；显式 `GEC_DEC_ENABLED=1`/`true` 开启（opt-in））。
//! 运行时设置（设置界面 / 设置文件）中的 `decision` 字段优先于环境变量。

use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::circuit::CircuitBreaker;
use crate::detector::DetectorError;
use crate::engine::{GecEngine, GecEngineKind, char_span_to_bytes};
use shared_model::{Error, error_types, sources};

fn default_endpoint() -> String {
    "http://127.0.0.1:4804".to_string()
}
fn default_timeout_secs() -> u64 {
    10
}
fn default_model() -> String {
    "vegaml-0.8b".to_string()
}
fn default_abstain_threshold() -> f64 {
    0.5
}

/// 决策模型（vegaml）服务配置
///
/// 可 JSON 序列化（`#[serde(default)]` 逐字段缺省），供运行时设置持久化；
/// 设置文件 > 环境变量 > 内置默认。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionConfig {
    /// 决策模型服务地址，如 `http://127.0.0.1:4804`
    #[serde(default = "default_endpoint")]
    pub endpoint: String,
    /// 请求超时（秒）
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    /// 模型名（日志 / 元数据；微调后为 checkpoint 名）
    #[serde(default = "default_model")]
    pub model: String,
    /// 弃权阈值：句级 `has_error_prob` 低于该值时不产出错误（conformal 校准后概率）
    #[serde(default = "default_abstain_threshold")]
    pub abstain_threshold: f64,
    /// 决策层开关：**默认 `false`**（P0.5：L3 特征模型是唯一的 pre-LLM 门控，
    /// 决策模型是 opt-in 层——设置界面 / 设置文件 / `GEC_DEC_ENABLED=1` 显式开启后
    /// 在 `rule` 引擎组合中挂载（规则为主 + 决策合并））；
    /// 旧版设置文件缺失该字段时按关闭处理
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

impl Default for DecisionConfig {
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

/// 决策层默认**关闭**（P0.5：L3 特征模型是唯一的 pre-LLM 门控；opt-in 见
/// `GEC_DEC_ENABLED` / 设置界面）
fn default_enabled() -> bool {
    false
}

impl DecisionConfig {
    /// 决策层是否启用（**默认 false**；设置界面开关 / `GEC_DEC_ENABLED` 可显式覆盖）
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// 从环境变量构建（设置文件缺失时的默认来源）：
    /// `GEC_DEC_ENDPOINT` / `GEC_DEC_TIMEOUT_SECS` / `GEC_DEC_MODEL` /
    /// `GEC_DEC_ABSTAIN_THRESHOLD` / `GEC_DEC_ENABLED`
    ///
    /// 默认**关闭**（P0.5：L3 是唯一的 pre-LLM 门控）；
    /// 显式 `GEC_DEC_ENABLED=1`/`true` 时开启（opt-in）。
    pub fn from_env() -> Self {
        let base = Self::default();
        let endpoint = std::env::var("GEC_DEC_ENDPOINT").unwrap_or(base.endpoint);
        let timeout_secs = std::env::var("GEC_DEC_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(base.timeout_secs);
        let model = std::env::var("GEC_DEC_MODEL").unwrap_or(base.model);
        let abstain_threshold = std::env::var("GEC_DEC_ABSTAIN_THRESHOLD")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(base.abstain_threshold);
        // 默认关闭；仅当 GEC_DEC_ENABLED 显式取 1/true 时开启（opt-in）
        let enabled = match std::env::var("GEC_DEC_ENABLED") {
            Ok(v) => v == "1" || v.eq_ignore_ascii_case("true"),
            Err(_) => false,
        };
        Self {
            endpoint,
            timeout_secs,
            model,
            abstain_threshold,
            enabled,
        }
    }
}

/// vegaml 决策模型 GEC 引擎
pub struct DecisionGecEngine {
    lang: String,
    client: reqwest::Client,
    config: DecisionConfig,
    /// 熔断器：服务宕机连续失败后跳闸，冷却期内请求立即失败（毫秒级），
    /// 避免每次调用都等满超时
    breaker: CircuitBreaker,
    /// 模型版本缓存（`GET /health` 的 `versions[lang]`；版本只在服务重启时变化，
    /// 但「仅重启服务」不会重建检测器，故缓存带 TTL（[`Self::version_ttl`]）到期重查，
    /// 防版本过期。外层 `None` = 尚未查询；内层 = (查询时刻, 结果（`None` = 版本未知）)
    version_cache: std::sync::Mutex<Option<(std::time::Instant, Option<String>)>>,
    /// 版本缓存 TTL（默认 60s；测试可缩短）
    version_ttl: std::time::Duration,
}

impl DecisionGecEngine {
    pub fn new(lang: impl Into<String>, config: DecisionConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs.max(1)))
            .build()
            .unwrap_or_default();
        Self {
            lang: lang.into(),
            client,
            config,
            breaker: CircuitBreaker::default(),
            version_cache: std::sync::Mutex::new(None),
            version_ttl: std::time::Duration::from_secs(60),
        }
    }

    /// 从环境变量（`GEC_DEC_*`）构建
    pub fn from_env(lang: impl Into<String>) -> Self {
        Self::new(lang, DecisionConfig::from_env())
    }

    /// 决策模型版本号（服务 `GET /health` 的 `versions[lang]`；每次重训练自增）。
    ///
    /// 版本只在服务重启（重新加载模型）时变化，而「仅重启服务」不会重建检测器
    /// （引擎随之不重建），故结果按引擎缓存、TTL（默认 60s）到期重查。
    /// 服务不可用 / 该语言未加载模型（启发式回退）/ 旧服务无 `versions` 字段
    /// → `None`（版本未知；历史复用时与携带版本的历史不匹配，保守不复用）。
    pub async fn model_version(&self) -> Option<String> {
        // 快路径：缓存未过期（锁不跨 await）
        if let Some((t, v)) = self.version_cache.lock().unwrap().clone() {
            if t.elapsed() < self.version_ttl {
                return v;
            }
        }
        match self.fetch_model_version().await {
            Ok(v) => {
                *self.version_cache.lock().unwrap() = Some((std::time::Instant::now(), v.clone()));
                v
            }
            // 查询失败（服务不可用）：版本未知 → 保守 `None`；
            // 也不返回旧缓存（决策层当前未参与检测，旧版本会误导复用判定）
            Err(()) => None,
        }
    }

    /// 查询服务 `/health`：`Ok(None)` = 查询成功但版本未知
    /// （该语言无模型 / 旧服务无 `versions` 字段）；`Err(())` = 查询失败
    /// （服务不可用 / 响应不可解析）
    async fn fetch_model_version(&self) -> Result<Option<String>, ()> {
        let url = format!("{}/health", self.config.endpoint.trim_end_matches('/'));
        let resp = match self.client.get(&url).send().await {
            Ok(r) => r,
            Err(_) => return Err(()),
        };
        if !resp.status().is_success() {
            return Err(());
        }
        let body: serde_json::Value = match resp.json().await {
            Ok(b) => b,
            Err(_) => return Err(()),
        };
        let v = body
            .get("versions")
            .and_then(|v| v.get(&self.lang))
            .and_then(|v| v.as_str());
        Ok(match v {
            Some(v) if !v.is_empty() => Some(v.to_string()),
            _ => None,
        })
    }

    /// 调用决策模型服务，返回错误列表 + 弃权标记
    ///
    /// 弃权判定（三级响应 level 2 的触发信号）：
    /// - 显式 `abstain` 标志（服务方按契约输出）；
    /// - 或：未产出错误且 `has_error_prob` 落在灰区
    ///   `[GRAY_ZONE_LOW, abstain_threshold)`（不输出 abstain 字段的旧服务的兜底判定）。
    /// 概率低于灰区下界视为「明确无错」，不触发兜底。
    pub async fn decide(&self, text: &str) -> Result<DecideOutcome, DetectorError> {
        // 熔断器：服务宕机跳闸后冷却期内立即失败（毫秒级），
        // 避免每次调用都等满超时（调用方按既有回退路径处理）
        if !self.breaker.allow_request() {
            return Err(DetectorError::ServiceUnavailable(
                "决策模型服务已熔断（连续失败），暂跳过；冷却到期后自动探测恢复".into(),
            ));
        }
        let url = format!("{}/decide", self.config.endpoint.trim_end_matches('/'));
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
                "决策模型服务返回 HTTP {}",
                resp.status()
            )));
        }

        let body: DecideResponse = resp.json().await.map_err(|e| {
            self.breaker.on_failure();
            DetectorError::ServiceUnavailable(e.to_string())
        })?;
        self.breaker.on_success();

        // 1. 服务返回 span 级错误（微调后细化）→ 直接映射（明确判断，非弃权）；
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
                            "决策模型检出错误"
                        } else {
                            &e.message
                        },
                    );
                    err.source = Some(sources::DECISION.to_string());
                    err
                })
                .collect();
            return Ok(DecideOutcome {
                errors,
                abstained: false,
            });
        }

        // 2. 句级判断：弃权或无错 → 不产出错误（模糊情况宁缺毋滥）
        if body.abstain || !body.has_error {
            let abstained = body.abstain
                || (body.has_error_prob >= GRAY_ZONE_LOW
                    && body.has_error_prob < self.config.abstain_threshold);
            return Ok(DecideOutcome {
                errors: Vec::new(),
                abstained,
            });
        }
        let prob = if body.has_error_prob > 0.0 {
            body.has_error_prob
        } else {
            0.5
        };
        if prob < self.config.abstain_threshold {
            // has_error=true 但概率低于阈值 → 不产出错误；灰区内视为弃权
            let abstained = body.abstain || prob >= GRAY_ZONE_LOW;
            return Ok(DecideOutcome {
                errors: Vec::new(),
                abstained,
            });
        }
        let vegaml_type = body
            .error_type
            .clone()
            .unwrap_or_else(|| "grammar".to_string());
        let mut err = Error::new(
            &self.lang,
            map_error_type(&vegaml_type),
            prob,
            [0, text.len()],
            text.to_string(),
            None,
            sentence_message(&vegaml_type),
        );
        err.source = Some(sources::DECISION.to_string());
        Ok(DecideOutcome {
            errors: vec![err],
            abstained: false,
        })
    }
}

/// 决策模型判断结果：错误列表 + 弃权标记（三级响应 level 2 的触发信号）
#[derive(Debug, Clone, Default)]
pub struct DecideOutcome {
    /// 决策模型产出的错误（span 级或句级合成）
    pub errors: Vec<Error>,
    /// 决策模型明确弃权（显式 abstain 标志，或概率灰区且未产出错误）
    pub abstained: bool,
}

/// 弃权灰区下界：`has_error_prob` 低于该值视为「明确无错」，
/// 落在 `[0.25, abstain_threshold)` 视为弃权（模糊情况交由 LLM 兜底层）
const GRAY_ZONE_LOW: f64 = 0.25;

/// vegaml 服务 `/decide` 响应（句级判断 + 可选 span 级错误）
#[derive(Debug, Deserialize)]
struct DecideResponse {
    #[serde(default)]
    has_error: bool,
    #[serde(default)]
    has_error_prob: f64,
    #[serde(default)]
    error_type: Option<String>,
    #[serde(default)]
    abstain: bool,
    #[serde(default)]
    #[allow(dead_code)]
    prediction_set: Option<Vec<String>>,
    #[serde(default)]
    #[allow(dead_code)]
    readout: Option<String>,
    #[serde(default)]
    errors: Vec<DecisionSpanError>,
}

/// span 级错误（微调后服务可细化到片段级）
#[derive(Debug, Deserialize)]
struct DecisionSpanError {
    /// 错误类型（兼容旧服务的 `type` 字段名）
    #[serde(default, alias = "type")]
    error_type: String,
    #[serde(default = "default_confidence")]
    confidence: f64,
    span: [usize; 2],
    /// span 对应的原文（旧服务缺失时留空）
    #[serde(default)]
    original: String,
    #[serde(default)]
    correction: Option<String>,
    #[serde(default)]
    message: String,
}

fn default_confidence() -> f64 {
    0.5
}

/// vegaml 错误类型 → 统一错误类型（`shared_model::error_types`）
fn map_error_type(vegaml_type: &str) -> String {
    match vegaml_type {
        "collocation" => error_types::COLLOCATION.to_string(),
        "word_order" => error_types::WORD_ORDER.to_string(),
        // 成分残缺 / 句式杂糅 / 逻辑不通 → 统一归入 grammar
        "missing" | "structure" | "logic" | "none" => error_types::GRAMMAR.to_string(),
        other => other.to_string(),
    }
}

/// 句级错误的默认说明（按 vegaml 错误类型）
fn sentence_message(vegaml_type: &str) -> String {
    match vegaml_type {
        "collocation" => "决策模型检出搭配不当".to_string(),
        "word_order" => "决策模型检出语序不当".to_string(),
        "missing" => "决策模型检出成分残缺或赘余".to_string(),
        "structure" => "决策模型检出句式杂糅 / 结构混乱".to_string(),
        "logic" => "决策模型检出逻辑不通 / 语义矛盾".to_string(),
        other => format!("决策模型检出语病（{other}）"),
    }
}

#[async_trait]
impl GecEngine for DecisionGecEngine {
    fn kind(&self) -> GecEngineKind {
        GecEngineKind::Decision
    }
    fn name(&self) -> &str {
        &self.config.model
    }

    async fn detect(&self, text: &str) -> Result<Vec<Error>, DetectorError> {
        self.decide(text).await.map(|o| o.errors)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;

    /// 本地 mock 决策模型服务（按请求文本返回可配置的判断）
    async fn mock_decide_server(
        handler: std::sync::Arc<dyn Fn(&str) -> serde_json::Value + Send + Sync>,
    ) -> String {
        let app = axum::Router::new().route(
            "/decide",
            axum::routing::post(move |body: axum::body::Bytes| async move {
                let req: serde_json::Value = serde_json::from_slice(&body).unwrap();
                let text = req["text"].as_str().unwrap_or("");
                (axum::http::StatusCode::OK, axum::Json(handler(text))).into_response()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        format!("http://{addr}")
    }

    fn config(endpoint: String) -> DecisionConfig {
        DecisionConfig {
            endpoint,
            timeout_secs: 5,
            model: "mock-vegaml".into(),
            abstain_threshold: 0.5,
            enabled: true,
        }
    }

    /// 本地 mock 决策服务 `/health`（返回可配置的 `versions`；计数请求次数）
    async fn mock_health_server(
        versions: serde_json::Value,
        hits: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) -> String {
        let app = axum::Router::new().route(
            "/health",
            axum::routing::get(move || {
                let versions = versions.clone();
                async move {
                    hits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    (
                        axum::http::StatusCode::OK,
                        axum::Json(serde_json::json!({
                            "status": "ok",
                            "mode": "sklearn",
                            "versions": versions,
                        })),
                    )
                        .into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn model_version_per_lang_from_health() {
        let endpoint = mock_health_server(
            serde_json::json!({ "en": "v2", "zh": "v1" }),
            std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        )
        .await;
        let en = DecisionGecEngine::new("en", config(endpoint.clone()));
        let zh = DecisionGecEngine::new("zh", config(endpoint));
        assert_eq!(en.model_version().await.as_deref(), Some("v2"));
        assert_eq!(zh.model_version().await.as_deref(), Some("v1"));
    }

    #[tokio::test]
    async fn model_version_none_when_service_down_or_lang_missing() {
        // 服务宕机（端口无监听）→ None
        let down = DecisionGecEngine::new("zh", config("http://127.0.0.1:1".into()));
        assert_eq!(down.model_version().await, None);
        // 服务可达但该语言无模型（启发式回退）→ None
        let endpoint = mock_health_server(
            serde_json::json!({ "en": "v3" }),
            std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        )
        .await;
        let zh = DecisionGecEngine::new("zh", config(endpoint));
        assert_eq!(zh.model_version().await, None);
    }

    #[tokio::test]
    async fn model_version_cached_within_ttl() {
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let endpoint = mock_health_server(serde_json::json!({ "en": "v4" }), hits.clone()).await;
        let engine = DecisionGecEngine::new("en", config(endpoint));
        assert_eq!(engine.model_version().await.as_deref(), Some("v4"));
        assert_eq!(engine.model_version().await.as_deref(), Some("v4"));
        assert_eq!(
            hits.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "TTL 内版本只应查询一次（缓存命中）"
        );
    }

    #[tokio::test]
    async fn model_version_refetches_after_ttl_expiry() {
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let endpoint = mock_health_server(serde_json::json!({ "en": "v5" }), hits.clone()).await;
        let mut engine = DecisionGecEngine::new("en", config(endpoint));
        engine.version_ttl = std::time::Duration::from_millis(50);
        assert_eq!(engine.model_version().await.as_deref(), Some("v5"));
        assert_eq!(engine.model_version().await.as_deref(), Some("v5"));
        assert_eq!(hits.load(std::sync::atomic::Ordering::Relaxed), 1);
        // TTL 过期 → 重查（防「仅重启服务」场景下版本缓存过期）
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        assert_eq!(engine.model_version().await.as_deref(), Some("v5"));
        assert_eq!(
            hits.load(std::sync::atomic::Ordering::Relaxed),
            2,
            "TTL 过期后应重查 /health"
        );
    }

    /// 本地 mock 决策服务 `/health`：首次请求返回 `v1`，之后返回 500（模拟服务重启后宕机）
    async fn mock_flaky_health_server(
        hits: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) -> String {
        let app = axum::Router::new().route(
            "/health",
            axum::routing::get({
                let hits = hits;
                move || {
                    let hits = hits.clone();
                    async move {
                        let first = hits.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 0;
                        if first {
                            (
                                axum::http::StatusCode::OK,
                                axum::Json(serde_json::json!({
                                    "status": "ok",
                                    "mode": "sklearn",
                                    "versions": { "en": "v1" },
                                })),
                            )
                                .into_response()
                        } else {
                            (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "boom").into_response()
                        }
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn model_version_none_on_query_failure_not_stale_cache() {
        let endpoint =
            mock_flaky_health_server(std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)))
                .await;
        let mut engine = DecisionGecEngine::new("en", config(endpoint));
        engine.version_ttl = std::time::Duration::from_millis(20);
        // 首次查询成功 → v1
        assert_eq!(engine.model_version().await.as_deref(), Some("v1"));
        // TTL 过期后服务开始失败 → 保守 None（不返回过期的 v1，
        // 否则「服务宕机期间」会与携带版本的历史错误匹配）
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        assert_eq!(engine.model_version().await, None);
    }

    #[test]
    fn model_version_future_is_send() {
        fn assert_send<T: Send>(t: &T) {
            let _ = t;
        }
        let engine = DecisionGecEngine::new("en", config("http://127.0.0.1:1".into()));
        assert_send(&engine.model_version());
    }

    #[tokio::test]
    async fn span_level_errors_mapped() {
        let endpoint = mock_decide_server(std::sync::Arc::new(|text| {
            serde_json::json!({
                "has_error": true,
                "has_error_prob": 0.9,
                "error_type": "missing",
                "abstain": false,
                "errors": [
                    {
                        "error_type": "collocation",
                        "confidence": 0.85,
                        "span": [0, text.chars().count().min(2)],
                        "original": "搭配",
                        "correction": "修正",
                        "message": "搭配不当"
                    }
                ]
            })
        }))
        .await;
        let engine = DecisionGecEngine::new("zh", config(endpoint));
        assert_eq!(engine.kind(), GecEngineKind::Decision);
        assert_eq!(engine.name(), "mock-vegaml");
        let errors = engine.detect("搭配不当的句子").await.unwrap();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].lang, "zh");
        assert_eq!(errors[0].error_type, "collocation");
        assert_eq!(errors[0].confidence, 0.85);
        assert_eq!(errors[0].correction.as_deref(), Some("修正"));
        assert_eq!(errors[0].message, "搭配不当");
    }

    #[tokio::test]
    async fn sentence_level_synthesized() {
        let endpoint = mock_decide_server(std::sync::Arc::new(|_| {
            serde_json::json!({
                "has_error": true,
                "has_error_prob": 0.9,
                "error_type": "missing",
                "abstain": false,
                "errors": []
            })
        }))
        .await;
        let engine = DecisionGecEngine::new("zh", config(endpoint));
        let errors = engine.detect("通过这次活动使我学到了很多").await.unwrap();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].error_type, "grammar", "missing → grammar 映射");
        assert_eq!(errors[0].confidence, 0.9);
        assert_eq!(errors[0].span, [0, "通过这次活动使我学到了很多".len()]);
        assert!(errors[0].message.contains("成分残缺"));
    }

    #[tokio::test]
    async fn abstain_yields_no_error() {
        let endpoint = mock_decide_server(std::sync::Arc::new(|_| {
            serde_json::json!({
                "has_error": true,
                "has_error_prob": 0.9,
                "error_type": "logic",
                "abstain": true
            })
        }))
        .await;
        let engine = DecisionGecEngine::new("zh", config(endpoint));
        let errors = engine.detect("模糊句").await.unwrap();
        assert!(errors.is_empty(), "弃权时不应产出错误");
    }

    #[tokio::test]
    async fn below_threshold_yields_no_error() {
        let endpoint = mock_decide_server(std::sync::Arc::new(|_| {
            serde_json::json!({
                "has_error": true,
                "has_error_prob": 0.3,
                "error_type": "logic",
                "abstain": false
            })
        }))
        .await;
        let engine = DecisionGecEngine::new("zh", config(endpoint));
        let errors = engine.detect("模糊句").await.unwrap();
        assert!(errors.is_empty(), "低于弃权阈值不应产出错误");
    }

    #[tokio::test]
    async fn no_error_yields_no_error() {
        let endpoint = mock_decide_server(std::sync::Arc::new(|_| {
            serde_json::json!({
                "has_error": false,
                "has_error_prob": 0.1,
                "error_type": "none",
                "abstain": false
            })
        }))
        .await;
        let engine = DecisionGecEngine::new("zh", config(endpoint));
        assert!(engine.detect("正确句子").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn word_order_type_mapping() {
        let endpoint = mock_decide_server(std::sync::Arc::new(|_| {
            serde_json::json!({
                "has_error": true,
                "has_error_prob": 0.8,
                "error_type": "word_order",
                "abstain": false
            })
        }))
        .await;
        let engine = DecisionGecEngine::new("zh", config(endpoint));
        let errors = engine.detect("他的成绩不但提高了").await.unwrap();
        assert_eq!(errors[0].error_type, "word_order");
        assert!(errors[0].message.contains("语序"));
    }

    #[tokio::test]
    async fn unavailable_when_service_down() {
        let engine = DecisionGecEngine::new(
            "zh",
            DecisionConfig {
                endpoint: "http://127.0.0.1:1".into(),
                timeout_secs: 1,
                model: "dead".into(),
                abstain_threshold: 0.5,
                enabled: true,
            },
        );
        let err = engine.detect("x").await.unwrap_err();
        assert!(matches!(err, DetectorError::ServiceUnavailable(_)));
    }

    #[test]
    fn config_from_env_default_off_opt_in() {
        unsafe {
            std::env::remove_var("GEC_DEC_ENDPOINT");
            std::env::remove_var("GEC_DEC_ENABLED");
            std::env::remove_var("GEC_DEC_MODEL");
        }
        // 未显式配置 → 默认关闭（P0.5：L3 是唯一的 pre-LLM 门控）
        let cfg = DecisionConfig::from_env();
        assert!(!cfg.enabled());
        assert_eq!(cfg.endpoint, "http://127.0.0.1:4804");
        // 显式 GEC_DEC_ENABLED=1 / true → 开启（opt-in）
        unsafe { std::env::set_var("GEC_DEC_ENABLED", "1") }
        assert!(DecisionConfig::from_env().enabled());
        unsafe { std::env::set_var("GEC_DEC_ENABLED", "true") }
        assert!(DecisionConfig::from_env().enabled());
        // 显式 0 / false → 关闭
        unsafe { std::env::set_var("GEC_DEC_ENABLED", "0") }
        assert!(!DecisionConfig::from_env().enabled());
        unsafe { std::env::set_var("GEC_DEC_ENABLED", "false") }
        assert!(!DecisionConfig::from_env().enabled());
        unsafe { std::env::remove_var("GEC_DEC_ENABLED") }
        // 旧版设置文件（无 enabled 字段）→ 默认关闭
        let old = r#"{"endpoint":"http://10.0.0.9:4804","timeout_secs":5,"model":"m","abstain_threshold":0.6}"#;
        let cfg: DecisionConfig = serde_json::from_str(old).unwrap();
        assert!(!cfg.enabled);
        assert_eq!(cfg.abstain_threshold, 0.6);
        // 显式 enabled=true 的旧文件 → 尊重开启
        let on = r#"{"enabled":true}"#;
        let cfg: DecisionConfig = serde_json::from_str(on).unwrap();
        assert!(cfg.enabled);
    }

    // ---------- 弃权信号（三级响应 level 2 触发条件） ----------

    #[tokio::test]
    async fn decide_abstain_flag_reports_abstained() {
        let endpoint = mock_decide_server(std::sync::Arc::new(|_| {
            serde_json::json!({
                "has_error": false,
                "has_error_prob": 0.2,
                "error_type": "none",
                "abstain": true,
                "errors": []
            })
        }))
        .await;
        let engine = DecisionGecEngine::new("zh", config(endpoint));
        let outcome = engine.decide("模糊句子").await.unwrap();
        assert!(outcome.abstained, "显式 abstain 标志应上报弃权");
        assert!(outcome.errors.is_empty());
    }

    #[tokio::test]
    async fn decide_gray_zone_prob_reports_abstained() {
        // 无 abstain 标志（旧服务）但概率落在灰区 [0.25, 0.5) → 视为弃权
        let endpoint = mock_decide_server(std::sync::Arc::new(|_| {
            serde_json::json!({
                "has_error": false,
                "has_error_prob": 0.3,
                "error_type": "none",
                "errors": []
            })
        }))
        .await;
        let engine = DecisionGecEngine::new("zh", config(endpoint));
        let outcome = engine.decide("模糊句子").await.unwrap();
        assert!(outcome.abstained, "灰区概率应视为弃权");
    }

    #[tokio::test]
    async fn decide_confident_no_error_not_abstained() {
        // 概率低于灰区下界 → 明确无错，不触发兜底
        let endpoint = mock_decide_server(std::sync::Arc::new(|_| {
            serde_json::json!({
                "has_error": false,
                "has_error_prob": 0.1,
                "error_type": "none",
                "errors": []
            })
        }))
        .await;
        let engine = DecisionGecEngine::new("zh", config(endpoint));
        let outcome = engine.decide("正确句子").await.unwrap();
        assert!(!outcome.abstained, "明确无错不应视为弃权");
        assert!(outcome.errors.is_empty());
    }

    #[tokio::test]
    async fn decide_span_errors_not_abstained() {
        let endpoint = mock_decide_server(std::sync::Arc::new(|_| {
            serde_json::json!({
                "has_error": true,
                "has_error_prob": 0.9,
                "error_type": "missing",
                "abstain": false,
                "errors": [
                    {
                        "error_type": "collocation",
                        "confidence": 0.85,
                        "span": [0, 2],
                        "original": "搭配",
                        "correction": "修正",
                        "message": "搭配不当"
                    }
                ]
            })
        }))
        .await;
        let engine = DecisionGecEngine::new("zh", config(endpoint));
        let outcome = engine.decide("搭配不当的句子").await.unwrap();
        assert!(!outcome.abstained);
        assert_eq!(outcome.errors.len(), 1);
        assert_eq!(outcome.errors[0].source.as_deref(), Some("decision"));
    }

    #[tokio::test]
    async fn decide_sentence_error_not_abstained() {
        let endpoint = mock_decide_server(std::sync::Arc::new(|_| {
            serde_json::json!({
                "has_error": true,
                "has_error_prob": 0.9,
                "error_type": "missing",
                "abstain": false,
                "errors": []
            })
        }))
        .await;
        let engine = DecisionGecEngine::new("zh", config(endpoint));
        let outcome = engine.decide("有语病的句子").await.unwrap();
        assert!(!outcome.abstained, "明确有错不应视为弃权");
        assert_eq!(outcome.errors.len(), 1);
        assert_eq!(outcome.errors[0].source.as_deref(), Some("decision"));
    }
}
