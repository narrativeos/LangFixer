use shared_model::Error;

use crate::detector::{DetectorError, GECDetector};

/// 检测流水线：封装单个检测器的执行入口
pub struct DetectionPipeline {
    pub detector: Box<dyn GECDetector>,
}

impl DetectionPipeline {
    pub fn new(detector: Box<dyn GECDetector>) -> Self {
        Self { detector }
    }

    pub async fn run(&self, text: &str) -> Result<Vec<Error>, DetectorError> {
        self.detector.detect(text).await
    }
}
