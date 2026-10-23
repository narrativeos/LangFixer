use async_trait::async_trait;

use gec_core::detector::{DetectorError, GECDetector};
use shared_model::Error;

use crate::rules;

/// 离线日文检测器：不依赖任何外部服务
pub struct OfflineJapaneseDetector {
    lexicon: rules::Lexicon,
}

impl OfflineJapaneseDetector {
    pub fn new(lexicon: rules::Lexicon) -> Self {
        Self { lexicon }
    }
}

#[async_trait]
impl GECDetector for OfflineJapaneseDetector {
    fn language(&self) -> &str {
        "ja"
    }

    async fn detect(&self, text: &str) -> Result<Vec<Error>, DetectorError> {
        Ok(rules::detect_local(text, &self.lexicon))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn offline_detects_particle() {
        let detector = OfflineJapaneseDetector::new(rules::Lexicon::default());
        let errors = detector.detect("私は毎日本読む。").await.unwrap();
        assert!(
            errors
                .iter()
                .any(|e| e.original == "本読む" && e.correction.as_deref() == Some("本を読む"))
        );
    }
}
