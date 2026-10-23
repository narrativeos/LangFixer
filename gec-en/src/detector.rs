use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use serde::Deserialize;

use gec_core::detector::{DetectorError, GECDetector};
use gec_core::timings::NlpCall;
use shared_model::Error;

use crate::config::DetectorConfig;
use crate::offline::OfflineEnglishDetector;
use crate::rules;

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn lexicon_json() -> String {
        let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        p.push("../lexicons/en_gec_lexicon.json");
        std::fs::read_to_string(&p).unwrap()
    }

    #[tokio::test]
    async fn custom_lexicon_is_respected() {
        // 自定义词表目录：扩充一个内置词表没有的过去式
        let dir = std::env::temp_dir().join("langfixer_en_lexicon_test");
        std::fs::create_dir_all(&dir).unwrap();
        let content = lexicon_json().replace(
            "\"past_tenses\": [",
            "\"past_tenses\":[[\"swim\", \"swam\"],",
        );
        std::fs::write(dir.join("en_gec_lexicon.json"), content).unwrap();

        let cfg = DetectorConfig {
            endpoint: String::new(),
            timeout: std::time::Duration::from_secs(1),
            offline_only: true,
            lexicons_dir: dir.clone(),
        };
        let detector = EnglishDetector::new(cfg);
        let errors = detector
            .detect("Yesterday I swim in the lake.")
            .await
            .unwrap();
        assert!(
            errors
                .iter()
                .any(|e| e.error_type == shared_model::error_types::TENSE
                    && e.original == "swim"
                    && e.correction.as_deref() == Some("swam"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// 英文检测器（HanLP 后端 + 离线兜底）
///
/// - 主路径：`POST {endpoint}/analyze`（HanLP 英文分词 + POS）→ Rust 侧 GEC 规则
/// - 兜底：远端不可达时降级为纯本地规则检测
/// - 词表：`lexicons/en_gec_lexicon.json` 外置（缺失时用内置默认）
pub struct EnglishDetector {
    endpoint: String,
    client: reqwest::Client,
    fallback: OfflineEnglishDetector,
    lexicon: rules::Lexicon,
    offline_only: bool,
    /// NLP 服务是否已完成首次成功调用（冷启动预热完成）
    nlp_warmed: AtomicBool,
    /// 最近一次 NLP 服务调用（`None` = 未调用，如纯本地检测）
    nlp_last: Mutex<Option<NlpCall>>,
}

impl EnglishDetector {
    pub fn new(config: DetectorConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .build()
            .unwrap_or_default();
        let lexicon = rules::Lexicon::load(&config.lexicons_dir);
        Self {
            endpoint: config.endpoint,
            client,
            fallback: OfflineEnglishDetector::new(lexicon.clone()),
            lexicon,
            offline_only: config.offline_only,
            nlp_warmed: AtomicBool::new(false),
            nlp_last: Mutex::new(None),
        }
    }
}

/// HanLP 服务 `/analyze` 响应（只取需要的字段）
#[derive(Deserialize)]
struct AnalyzeResponse {
    #[serde(default)]
    content: AnalyzeContent,
}

#[derive(Deserialize, Default)]
struct AnalyzeContent {
    #[serde(default)]
    tokens: Vec<rules::Token>,
}

#[async_trait]
impl GECDetector for EnglishDetector {
    fn language(&self) -> &str {
        "en"
    }

    async fn detect(&self, text: &str) -> Result<Vec<Error>, DetectorError> {
        if self.offline_only {
            return self.fallback.detect(text).await;
        }
        match self.analyze(text).await {
            Ok(tokens) => {
                // POS 规则 + 本地规则取并集（按 span 去重），按起始偏移排序
                let mut errors = rules::detect_with_tokens(text, &tokens, &self.lexicon);
                let local = rules::detect_local(text, &self.lexicon);
                for e in local {
                    if !errors.iter().any(|x| x.span == e.span) {
                        errors.push(e);
                    }
                }
                errors.sort_by_key(|e| e.span[0]);
                Ok(errors)
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

impl EnglishDetector {
    /// 调用 HanLP `/analyze` 并记录本次 NLP 调用计时
    ///
    /// 检测器构建 / 配置重建后的**首次**调用包含 NLP 服务冷启动（模型加载）时间，
    /// 标记 `cold_start = true`（成功调用后才视为预热完成；失败不置位）。
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
            .json(&serde_json::json!({ "text": text, "language": "en" }))
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
