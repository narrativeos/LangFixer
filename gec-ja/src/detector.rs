use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use serde::Deserialize;

use gec_core::detector::{DetectorError, GECDetector};
use gec_core::timings::NlpCall;
use shared_model::Error;

use crate::config::DetectorConfig;
use crate::offline::OfflineJapaneseDetector;
use crate::rules;

/// 日文检测器（Sudachi 后端 + 离线兜底）
///
/// - 主路径：`POST {endpoint}/parse`（Sudachi 分词 + 词性）→ Rust 侧 GEC 规则
/// - 兜底：远端不可达时降级为纯本地规则检测
/// - 词表：`lexicons/ja_gec_lexicon.json` 外置（缺失时用内置默认）
pub struct JapaneseDetector {
    endpoint: String,
    client: reqwest::Client,
    fallback: OfflineJapaneseDetector,
    lexicon: rules::Lexicon,
    offline_only: bool,
    /// NLP 服务是否已完成首次成功调用（冷启动预热完成）
    nlp_warmed: AtomicBool,
    /// 最近一次 NLP 服务调用（`None` = 未调用，如纯本地检测）
    nlp_last: Mutex<Option<NlpCall>>,
}

impl JapaneseDetector {
    pub fn new(config: DetectorConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .build()
            .unwrap_or_default();
        let lexicon = rules::Lexicon::load(&config.lexicons_dir);
        Self {
            endpoint: config.endpoint,
            client,
            fallback: OfflineJapaneseDetector::new(lexicon.clone()),
            lexicon,
            offline_only: config.offline_only,
            nlp_warmed: AtomicBool::new(false),
            nlp_last: Mutex::new(None),
        }
    }
}

/// Sudachi 分词响应（兼容多种常见返回形态）
#[derive(Deserialize)]
struct ParseResponse {
    #[serde(default)]
    tokens: Vec<JaToken>,
    #[serde(default)]
    result: Option<ParseResult>,
}

#[derive(Deserialize)]
struct ParseResult {
    #[serde(default)]
    tokens: Vec<JaToken>,
}

#[derive(Deserialize)]
struct JaToken {
    #[serde(alias = "surface", alias = "word")]
    text: String,
    #[serde(default)]
    features: Vec<String>,
}

#[async_trait]
impl GECDetector for JapaneseDetector {
    fn language(&self) -> &str {
        "ja"
    }

    async fn detect(&self, text: &str) -> Result<Vec<Error>, DetectorError> {
        if self.offline_only {
            return self.fallback.detect(text).await;
        }
        match self.parse(text).await {
            Ok(tokens) => Ok(rules::detect_with_tokens(text, &tokens, &self.lexicon)),
            Err(e) => {
                tracing::warn!(error = %e, "Sudachi 服务不可用，降级为离线检测");
                self.fallback.detect(text).await
            }
        }
    }

    fn last_nlp_call(&self) -> Option<NlpCall> {
        *self.nlp_last.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl JapaneseDetector {
    /// 调用 Sudachi `/parse` 并记录本次 NLP 调用计时
    ///
    /// 检测器构建 / 配置重建后的**首次**调用包含 NLP 服务冷启动（模型加载）时间，
    /// 标记 `cold_start = true`（成功调用后才视为预热完成；失败不置位）。
    async fn parse(&self, text: &str) -> Result<Vec<rules::Token>, DetectorError> {
        let t = std::time::Instant::now();
        let result = self.parse_inner(text).await;
        let ms = t.elapsed().as_millis() as u64;
        let cold_start = !self.nlp_warmed.load(Ordering::Relaxed);
        if result.is_ok() {
            self.nlp_warmed.store(true, Ordering::Relaxed);
        }
        *self.nlp_last.lock().unwrap_or_else(|e| e.into_inner()) = Some(NlpCall { ms, cold_start });
        result
    }

    async fn parse_inner(&self, text: &str) -> Result<Vec<rules::Token>, DetectorError> {
        let url = format!("{}/parse", self.endpoint.trim_end_matches('/'));
        let resp = self
            .client
            .post(&url)
            .json(&serde_json::json!({ "text": text }))
            .send()
            .await
            .map_err(|e| DetectorError::ServiceUnavailable(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(DetectorError::ServiceUnavailable(format!(
                "Sudachi 服务返回 HTTP {}",
                resp.status()
            )));
        }

        let body: ParseResponse = resp
            .json()
            .await
            .map_err(|e| DetectorError::ServiceUnavailable(e.to_string()))?;

        let raw = body
            .tokens
            .into_iter()
            .chain(body.result.map(|r| r.tokens).into_iter().flatten());
        let mut offset = 0usize;
        let tokens: Vec<rules::Token> = raw
            .filter(|t| !t.text.is_empty())
            .map(|t| {
                // span 为字节偏移（与 shared_model::Error 契约一致）
                let len = t.text.len();
                let span = [offset, offset + len];
                offset += len;
                rules::Token {
                    text: t.text,
                    pos: t.features.first().cloned().unwrap_or_default(),
                    span,
                }
            })
            .collect();
        Ok(tokens)
    }
}
