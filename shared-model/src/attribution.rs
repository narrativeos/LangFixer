use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::knowledge::KnowledgeSourceRef;
use crate::session::SessionMeta;

/// 归因请求上下文
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttributionContext {
    pub lang: String,
    pub error_type: String,
    pub confidence: f64,
    /// 该会话历史犯错次数（用于新手/老手判定）
    pub session_error_count: u32,
    pub session_meta: Option<SessionMeta>,
    /// 具体语法知识点标识（可选）。
    /// 由检测器 / LLM 细粒度分析提供，用于命中决策表中的细粒度规则
    /// （`grammarPoint` 输入）；缺省时仅按 `errorType` 命中该类别的兜底规则。
    #[serde(default)]
    pub grammar_point: Option<String>,
}

/// 归因结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttributionResult {
    /// 归因标签，见 [`attribution_labels`]
    pub attribution: String,
    /// 韦纳三维度
    pub weiner: WeinerDimensions,
    /// 补救措施
    pub remedial: Vec<String>,
    /// 决策追踪
    pub trace: AttributionTrace,
    /// CEFR 难度等级（A1-C2；决策表未提供时为 "unknown"）
    #[serde(default = "default_cefr")]
    pub cefr: String,
    /// 知识点来源（旧版决策表 `source` 输出，如 "第一讲 冠词 · 一.1 定冠词的使用"；
    /// 新表不再内嵌来源，改用下方 [`KnowledgeSourceRef`] 列表；保留字段以兼容历史数据）
    #[serde(default)]
    pub source: String,
    /// 知识源引用列表（由知识库按 `grammar_point` 富化：同一规则/知识点可被多个知识源背书；
    /// `title` 由知识库从源注册表解析回填）
    #[serde(default)]
    pub sources: Vec<KnowledgeSourceRef>,
    /// 典型"错误→改正"实例（决策表 `examples` 输出，或知识库按知识点富化；未提供时为空）
    #[serde(default)]
    pub examples: Vec<String>,
}

/// 韦纳三维度：内外源 × 稳定性 × 可控性
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeinerDimensions {
    /// internal / external
    pub locus: String,
    /// stable / unstable
    pub stability: String,
    /// controllable / uncontrollable
    pub controllability: String,
}

/// CEFR 缺省值（决策表未提供 cefrLevel 输出时）
fn default_cefr() -> String {
    "unknown".to_string()
}

impl WeinerDimensions {
    pub fn new(
        locus: impl Into<String>,
        stability: impl Into<String>,
        controllability: impl Into<String>,
    ) -> Self {
        Self {
            locus: locus.into(),
            stability: stability.into(),
            controllability: controllability.into(),
        }
    }
}

/// 决策追踪信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttributionTrace {
    pub matched_rule: String,
    pub decision_table: String,
}

/// 检测 + 归因一站式结果中的单条错误
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyzedError {
    pub error: Error,
    pub attribution: Option<AttributionResult>,
}

/// 归因标签常量
pub mod attribution_labels {
    pub const KNOWLEDGE_GAP: &str = "knowledge_gap";
    pub const HABIT_SOLIDIFIED: &str = "habit_solidified";
    pub const RULE_MISSING: &str = "rule_missing";
    pub const RULE_FORGOTTEN: &str = "rule_forgotten";
    pub const COLLOCATION_WEAK: &str = "collocation_weak";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_matches_spec_example() {
        // 规格文档 §5.3 示例
        let json = r#"{
            "attribution": "habit_solidified",
            "weiner": {
                "locus": "internal",
                "stability": "stable",
                "controllability": "controllable"
            },
            "remedial": [
                "强化训练：对比'报复'与'抱负'的语义差异",
                "错题本：加入高频错别字清单",
                "间隔重复：3天后再次测试"
            ],
            "trace": {
                "matched_rule": "spelling_high_conf_old_hand",
                "decision_table": "zh_gec_attribution.json"
            },
            "cefr": "A2"
        }"#;
        let result: AttributionResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.attribution, attribution_labels::HABIT_SOLIDIFIED);
        assert_eq!(result.weiner.locus, "internal");
        assert_eq!(result.remedial.len(), 3);
        assert_eq!(result.trace.matched_rule, "spelling_high_conf_old_hand");
        assert_eq!(result.cefr, "A2");
    }

    #[test]
    fn cefr_defaults_to_unknown() {
        // 旧版决策表（无 cefrLevel 输出）反序列化时 cefr 缺省为 "unknown"
        let json = r#"{
            "attribution": "knowledge_gap",
            "weiner": { "locus": "internal", "stability": "unstable", "controllability": "controllable" },
            "remedial": [],
            "trace": { "matched_rule": "default_knowledge_gap", "decision_table": "zh_gec_attribution.json" }
        }"#;
        let result: AttributionResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.cefr, "unknown");
    }
}
