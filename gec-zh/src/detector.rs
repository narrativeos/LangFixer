use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;

use gec_core::detector::{DetectorError, GECDetector};
use gec_core::timings::NlpCall;
use shared_model::Error;

use crate::config::DetectorConfig;
use crate::offline::OfflineChineseDetector;
use crate::rules;

/// 中文检测器
///
/// - 主路径：`POST {endpoint}/analyze`（HanLP v2 分词 + POS + NER）→ Rust 侧 GEC 规则
/// - 兜底：远端不可达时降级为纯本地规则检测
/// - 词表：`lexicons/zh_gec_lexicon.json` 外置（缺失时用内置默认）
pub struct ChineseDetector {
    endpoint: String,
    client: reqwest::Client,
    fallback: OfflineChineseDetector,
    lexicon: rules::Lexicon,
    offline_only: bool,
    /// NLP 服务是否已完成首次成功调用（冷启动预热完成）
    nlp_warmed: AtomicBool,
    /// 最近一次 NLP 服务调用（`None` = 未调用，如纯本地检测）
    nlp_last: Mutex<Option<NlpCall>>,
}

impl ChineseDetector {
    pub fn new(config: DetectorConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .build()
            .unwrap_or_default();
        let lexicon = rules::Lexicon::load(&config.lexicons_dir);
        Self {
            endpoint: config.endpoint,
            client,
            fallback: OfflineChineseDetector::new(lexicon.clone()),
            lexicon,
            offline_only: config.offline_only,
            nlp_warmed: AtomicBool::new(false),
            nlp_last: Mutex::new(None),
        }
    }
}

/// HanLP 服务 `/analyze` 响应（只取需要的字段）
#[derive(serde::Deserialize)]
struct AnalyzeResponse {
    #[serde(default)]
    content: AnalyzeContent,
}

#[derive(serde::Deserialize, Default)]
struct AnalyzeContent {
    #[serde(default)]
    tokens: Vec<rules::Token>,
}

#[async_trait]
impl GECDetector for ChineseDetector {
    fn language(&self) -> &str {
        "zh"
    }

    async fn detect(&self, text: &str) -> Result<Vec<Error>, DetectorError> {
        if self.offline_only {
            return self.fallback.detect(text).await;
        }
        match self.analyze(text).await {
            Ok(tokens) => {
                // HanLP `/analyze` 返回**字符偏移**（Python `len` 语义），而
                // [`Error::span`] 契约为**字节偏移**——CJK 文本两者相差约 3 倍，
                // 不换算会导致规则产出的 span 错位（高亮错位、apply_corrections
                // 在非字符边界切片 panic）。统一在入口处换算。
                let tokens = rules::tokens_to_byte_spans(text, &tokens);
                Ok(rules::detect_with_tokens(text, &tokens, &self.lexicon))
            }
            Err(e) => {
                tracing::warn!(error = %e, "HanLP 服务不可用，降级为离线检测");
                self.fallback.detect(text).await
            }
        }
    }

    fn last_nlp_call(&self) -> Option<NlpCall> {
        *self.nlp_last.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl ChineseDetector {
    /// 调用 HanLP `/analyze` 并记录本次 NLP 调用计时
    ///
    /// 检测器构建 / 配置重建后的**首次**调用包含 NLP 服务冷启动（模型加载）时间，
    /// 标记 `cold_start = true`（成功调用后才视为预热完成；失败不置位，
    /// 服务恢复后的下一次调用仍可能含冷启动）。
    async fn analyze(&self, text: &str) -> Result<Vec<rules::Token>, DetectorError> {
        let t = std::time::Instant::now();
        let result = self.analyze_inner(text).await;
        let ms = t.elapsed().as_millis() as u64;
        let cold_start = !self.nlp_warmed.load(Ordering::Relaxed);
        if result.is_ok() {
            self.nlp_warmed.store(true, Ordering::Relaxed);
        }
        *self.nlp_last.lock().unwrap_or_else(|e| e.into_inner()) = Some(NlpCall { ms, cold_start });
        result
    }

    async fn analyze_inner(&self, text: &str) -> Result<Vec<rules::Token>, DetectorError> {
        let url = format!("{}/analyze", self.endpoint.trim_end_matches('/'));
        let resp = self
            .client
            .post(&url)
            .json(&serde_json::json!({ "text": text, "language": "modern" }))
            .send()
            .await
            .map_err(|e| DetectorError::ServiceUnavailable(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(DetectorError::ServiceUnavailable(format!(
                "HanLP 服务返回 HTTP {}",
                resp.status()
            )));
        }

        let body: AnalyzeResponse = resp
            .json()
            .await
            .map_err(|e| DetectorError::ServiceUnavailable(e.to_string()))?;
        Ok(body.content.tokens)
    }
}
