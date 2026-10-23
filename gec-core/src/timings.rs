//! 检测各阶段计时
//!
//! 每次检测（[`super::engine::DetectOutcome::timings`]）记录各阶段的 wall time：
//! - ① 规则检测（含 NLP 服务调用 + 本地规则匹配）；
//! - ② 决策模型；③-F L3 特征模型（与 ① 并行，总耗时 ≈ 各层 max）；
//! - ②.5 知识向量召回；③ LLM 兜底（按需触发）。
//!
//! **NLP 预热**：NLP 服务（HanLP / Sudachi）冷启动时，检测器**首次** NLP 调用
//! 包含模型加载时间。该次调用单独记入 [`DetectTimings::nlp_warmup_ms`]，
//! 便于从总耗时中区分「预热」与「真实检测逻辑」耗时；后续调用该字段为 `None`。

/// 一次 NLP 服务调用信息（语言检测器在 [`super::detector::GECDetector::last_nlp_call`]
/// 中报告最近一次调用）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NlpCall {
    /// NLP 服务调用耗时（毫秒，HTTP 请求全程）
    pub ms: u64,
    /// 是否为检测器构建 / 配置重建后的**首次** NLP 调用（含冷启动预热）
    pub cold_start: bool,
}

/// 一次检测的各阶段计时（毫秒）
///
/// 全部字段 `0` / `None` 表示该阶段未执行（或本次未实际跑检测，如缓存命中）。
/// 注意 ① / ② / ③-F 并行执行，各阶段之和 **不等于** [`Self::total_ms`]
/// （total 为整条检测链的 wall time）。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DetectTimings {
    /// ① 规则检测（NLP 服务调用 + 本地规则匹配）
    pub rule_ms: u64,
    /// ① 中 NLP 服务调用耗时（`None` = 纯本地检测，无 NLP 调用）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nlp_ms: Option<u64>,
    /// NLP 预热（冷启动）耗时：仅进程启动 / 配置重建后的首次 NLP 调用非空
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nlp_warmup_ms: Option<u64>,
    /// ② 决策模型
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision_ms: Option<u64>,
    /// ③-F L3 特征模型（仅 level 3 且非 force_llm）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub l3_ms: Option<u64>,
    /// ②.5 知识向量召回（嵌入服务）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recall_ms: Option<u64>,
    /// ③ LLM 兜底（level 2 弃权触发 / level 3 / force_llm）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_ms: Option<u64>,
    /// 本次检测总耗时（检测链入口到返回的 wall time）
    pub total_ms: u64,
}

impl DetectTimings {
    /// 是否包含任何有效计时（缓存命中等未实际检测的场景为 false）
    pub fn is_empty(&self) -> bool {
        self.total_ms == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timings_roundtrip() {
        let t = DetectTimings {
            rule_ms: 120,
            nlp_ms: Some(100),
            nlp_warmup_ms: Some(80),
            decision_ms: Some(90),
            l3_ms: None,
            recall_ms: Some(15),
            llm_ms: None,
            total_ms: 130,
        };
        let json = serde_json::to_string(&t).unwrap();
        // None 字段不序列化
        assert!(!json.contains("l3_ms"));
        assert!(!json.contains("llm_ms"));
        let back: DetectTimings = serde_json::from_str(&json).unwrap();
        assert_eq!(back, t);
    }

    #[test]
    fn default_is_empty() {
        assert!(DetectTimings::default().is_empty());
    }
}
