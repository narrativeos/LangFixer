use async_trait::async_trait;

use shared_model::Error;

use crate::timings::NlpCall;

/// 语言检测器统一接口：新增语言只需实现该 trait 并注册到 [`crate::LanguageRouter`]。
#[async_trait]
pub trait GECDetector: Send + Sync {
    /// 语言代码（"zh" / "en" / "ja" ...）
    fn language(&self) -> &str;

    /// 检测文本中的拼写 / 语法 / 搭配错误
    async fn detect(&self, text: &str) -> Result<Vec<Error>, DetectorError>;

    /// 最近一次 `detect` 中的 NLP 服务调用信息（`None` = 纯本地检测，未调用 NLP 服务）。
    ///
    /// 依赖 NLP 服务（HanLP / Sudachi 等）的检测器覆写本方法，
    /// 报告最近一次 NLP 调用耗时与是否冷启动（首次调用含模型加载预热）。
    fn last_nlp_call(&self) -> Option<NlpCall> {
        None
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DetectorError {
    #[error("detection failed: {0}")]
    DetectionFailed(String),
    #[error("unsupported language: {0}")]
    UnsupportedLanguage(String),
    #[error("upstream NLP service unavailable: {0}")]
    ServiceUnavailable(String),
    /// LLM 兜底层（三级响应 level 2/3）触发时 LLM 未配置（无 API Key 或缺检测模板）
    #[error("LLM fallback not configured: {0}")]
    LlmNotConfigured(String),
}
