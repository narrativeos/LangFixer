//! LLM 错误类型

/// LLM 服务错误
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    /// 未配置 LLM_API_KEY
    #[error("LLM 服务未配置（请设置 LLM_API_KEY）")]
    NotConfigured,

    /// 请求/解析失败
    #[error("LLM 请求失败: {0}")]
    Request(String),

    /// 未找到匹配的提示词模板
    #[error("语言 {0} 无匹配提示词模板")]
    TemplateNotFound(String),
}
