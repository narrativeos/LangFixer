use async_trait::async_trait;

use gec_core::detector::{DetectorError, GECDetector};
use shared_model::Error;

use crate::rules;

/// 离线中文检测器：不依赖任何外部服务，仅使用本地词典 + 标点规则
pub struct OfflineChineseDetector {
    lexicon: rules::Lexicon,
}

impl OfflineChineseDetector {
    pub fn new(lexicon: rules::Lexicon) -> Self {
        Self { lexicon }
    }
}

#[async_trait]
impl GECDetector for OfflineChineseDetector {
    fn language(&self) -> &str {
        "zh"
    }

    async fn detect(&self, text: &str) -> Result<Vec<Error>, DetectorError> {
        Ok(rules::detect_local(text, &self.lexicon))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn offline_detects_homophones() {
        let detector = OfflineChineseDetector::new(rules::Lexicon::default());
        let errors = detector.detect("他時常迟到。").await.unwrap();
        assert!(
            errors
                .iter()
                .any(|e| e.original == "時" && e.correction.as_deref() == Some("时"))
        );
    }
}
