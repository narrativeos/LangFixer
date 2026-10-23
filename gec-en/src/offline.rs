use async_trait::async_trait;

use gec_core::detector::{DetectorError, GECDetector};
use shared_model::Error;

use crate::rules;

/// 离线英文检测器：不依赖任何外部服务
pub struct OfflineEnglishDetector {
    lexicon: rules::Lexicon,
}

impl OfflineEnglishDetector {
    pub fn new(lexicon: rules::Lexicon) -> Self {
        Self { lexicon }
    }
}

#[async_trait]
impl GECDetector for OfflineEnglishDetector {
    fn language(&self) -> &str {
        "en"
    }

    async fn detect(&self, text: &str) -> Result<Vec<Error>, DetectorError> {
        Ok(rules::detect_local(text, &self.lexicon))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn offline_detects_article_error() {
        let detector = OfflineEnglishDetector::new(rules::Lexicon::default());
        let errors = detector.detect("I bought a apple.").await.unwrap();
        assert!(
            errors
                .iter()
                .any(|e| e.original == "a" && e.correction.as_deref() == Some("an"))
        );
    }
}
