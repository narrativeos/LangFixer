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

    /// 网关**排队 / 过载**（429 / 5xx）：服务是好的，只是没排到。
    ///
    /// 与 [`Self::Request`] 分开，调用方才能按网关给的 `retry_after_secs` 等，
    /// 而不是拿固定退避表猜排队窗口（实测共享网关跑完一次长转录后有几十秒到两分钟的
    /// 503 窗口，短退避只会连续撞墙）。
    #[error("LLM 服务繁忙（HTTP {status}）")]
    Queued {
        status: u16,
        retry_after_secs: Option<u64>,
    },

    /// 未找到匹配的提示词模板
    #[error("语言 {0} 无匹配提示词模板")]
    TemplateNotFound(String),
}

impl LlmError {
    /// 是否**瞬时**故障（值得退避重试）：5xx / 429 / 传输层错误 / 超时。
    ///
    /// 立场：未配置、模板缺失、4xx（除 429）都是「要人修」的问题，重试只是拖慢失败；
    /// 而共享网关的 503 抖一下就让整批结果报废，那是把可用结果丢掉。
    pub fn is_transient(&self) -> bool {
        match self {
            // 排队 / 过载：等就完了，不该判死
            LlmError::Queued { .. } => true,
            LlmError::Request(s) => [
                "HTTP 5",
                "HTTP 429",
                "error sending request",
                "operation timed out",
                "connection closed",
            ]
            .iter()
            .any(|k| s.contains(k)),
            _ => false,
        }
    }

    /// 网关要求的等待秒数（`Retry-After`，拿不到就 `None`）
    ///
    /// 有值时调用方应取 `max(退避表, 该值)`：网关知道自己排到哪，猜的退避只会短。
    pub fn retry_after_secs(&self) -> Option<u64> {
        match self {
            LlmError::Queued {
                retry_after_secs, ..
            } => *retry_after_secs,
            _ => None,
        }
    }

    /// 是否「排队」而不是「坏了」：只用于把告警写准（`HTTP 503` 与「网关排队」是两回事）
    pub fn is_queued(&self) -> bool {
        matches!(self, LlmError::Queued { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_only_covers_retryable_failures() {
        for s in [
            "HTTP 503 Service Unavailable",
            "HTTP 500",
            "HTTP 429 Too Many Requests",
            "error sending request for url (http://x/v1/chat/completions)",
            "operation timed out",
        ] {
            assert!(
                LlmError::Request(s.to_string()).is_transient(),
                "{s} 应可重试"
            );
        }
        for s in ["HTTP 401 Unauthorized", "HTTP 404 Not Found"] {
            assert!(
                !LlmError::Request(s.to_string()).is_transient(),
                "{s} 不是瞬时故障"
            );
        }
        assert!(!LlmError::NotConfigured.is_transient());
        assert!(!LlmError::TemplateNotFound("zh".into()).is_transient());
    }
}
