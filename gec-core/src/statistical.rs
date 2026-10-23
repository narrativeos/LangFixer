//! 统计 GEC 引擎：外部统计序列模型服务的 HTTP 客户端
//!
//! 兼容 Harper / HuggingFace transformers server 类自建服务：
//! `POST {endpoint}/gec`，请求 `{"text": ..., "lang": ...}`，
//! 响应 `{"errors": [{error_type, confidence, span, original, correction?, message?}]}`。
//!
//! 环境变量：`GEC_STAT_ENDPOINT`（服务地址）/ `GEC_STAT_TIMEOUT_SECS` / `GEC_STAT_MODEL`（模型名）。

use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;

use crate::detector::DetectorError;
use crate::engine::{GecEngine, GecEngineKind};
use shared_model::Error;

/// 统计 GEC 服务配置（环境变量驱动）
#[derive(Debug, Clone)]
pub struct StatisticalConfig {
    /// 统计服务地址，如 `http://127.0.0.1:4803`
    pub endpoint: String,
    /// 请求超时
    pub timeout: Duration,
    /// 模型名（日志 / 元数据）
    pub model: String,
}

impl StatisticalConfig {
    pub fn from_env() -> Self {
        Self {
            endpoint: std::env::var("GEC_STAT_ENDPOINT")
                .unwrap_or_else(|_| "http://127.0.0.1:4803".to_string()),
            timeout: Duration::from_secs(
                std::env::var("GEC_STAT_TIMEOUT_SECS")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(30),
            ),
            model: std::env::var("GEC_STAT_MODEL").unwrap_or_else(|_| "hf-gec".to_string()),
        }
    }
}

/// 统计 GEC 引擎（序列模型服务）
pub struct StatisticalGecEngine {
    lang: String,
    client: reqwest::Client,
    config: StatisticalConfig,
}

impl StatisticalGecEngine {
    pub fn new(lang: impl Into<String>, config: StatisticalConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .build()
            .unwrap_or_default();
        Self {
            lang: lang.into(),
            client,
            config,
        }
    }

    /// 从环境变量（`GEC_STAT_*`）构建
    pub fn from_env(lang: impl Into<String>) -> Self {
        Self::new(lang, StatisticalConfig::from_env())
    }
}

/// 统计服务 `/gec` 响应
#[derive(Deserialize)]
struct StatGecResponse {
    #[serde(default)]
    errors: Vec<StatError>,
}

#[derive(Deserialize)]
struct StatError {
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

#[async_trait]
impl GecEngine for StatisticalGecEngine {
    fn kind(&self) -> GecEngineKind {
        GecEngineKind::Statistical
    }
    fn name(&self) -> &str {
        &self.config.model
    }

    async fn detect(&self, text: &str) -> Result<Vec<Error>, DetectorError> {
        let url = format!("{}/gec", self.config.endpoint.trim_end_matches('/'));
        let resp = self
            .client
            .post(&url)
            .json(&serde_json::json!({ "text": text, "lang": self.lang }))
            .send()
            .await
            .map_err(|e| DetectorError::ServiceUnavailable(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(DetectorError::ServiceUnavailable(format!(
                "统计 GEC 服务返回 HTTP {}",
                resp.status()
            )));
        }

        let body: StatGecResponse = resp
            .json()
            .await
            .map_err(|e| DetectorError::ServiceUnavailable(e.to_string()))?;
        Ok(body
            .errors
            .into_iter()
            .map(|e| {
                Error::new(
                    &self.lang,
                    e.error_type,
                    e.confidence,
                    e.span,
                    e.original,
                    e.correction,
                    if e.message.is_empty() {
                        "统计模型检出错误"
                    } else {
                        &e.message
                    },
                )
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;

    /// 本地 mock 统计 GEC 服务
    async fn mock_stat_server() -> String {
        let app = axum::Router::new().route(
            "/gec",
            axum::routing::post(|body: axum::body::Bytes| async move {
                let req: serde_json::Value = serde_json::from_slice(&body).unwrap();
                let text = req["text"].as_str().unwrap_or("");
                (
                    axum::http::StatusCode::OK,
                    axum::Json(serde_json::json!({
                        "errors": [
                            {
                                "error_type": "grammar",
                                "confidence": 0.95,
                                "span": [0, text.chars().count().min(1)],
                                "original": "mock",
                                "correction": "fixed",
                                "message": "statistical model"
                            }
                        ]
                    })),
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

    #[tokio::test]
    async fn statistical_roundtrip() {
        let endpoint = mock_stat_server().await;
        let engine = StatisticalGecEngine::new(
            "en",
            StatisticalConfig {
                endpoint,
                timeout: Duration::from_secs(5),
                model: "mock-model".into(),
            },
        );
        assert_eq!(engine.kind(), GecEngineKind::Statistical);
        assert_eq!(engine.name(), "mock-model");
        let errors = engine.detect("I saw a apple.").await.unwrap();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].lang, "en");
        assert_eq!(errors[0].error_type, "grammar");
        assert_eq!(errors[0].confidence, 0.95);
        assert_eq!(errors[0].correction.as_deref(), Some("fixed"));
        assert_eq!(errors[0].message, "statistical model");
    }

    #[tokio::test]
    async fn statistical_unavailable_when_service_down() {
        let engine = StatisticalGecEngine::new(
            "en",
            StatisticalConfig {
                endpoint: "http://127.0.0.1:1".into(),
                timeout: Duration::from_millis(200),
                model: "dead".into(),
            },
        );
        let err = engine.detect("x").await.unwrap_err();
        assert!(matches!(err, DetectorError::ServiceUnavailable(_)));
    }
}
