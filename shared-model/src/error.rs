use serde::{Deserialize, Serialize};

/// 修正建议（按 score 降序排列）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Suggestion {
    /// 建议文本
    pub text: String,
    /// 置信度 0.0 ~ 1.0
    pub score: f64,
}

impl Suggestion {
    pub fn new(text: impl Into<String>, score: f64) -> Self {
        Self {
            text: text.into(),
            score,
        }
    }
}

/// 统一的语法错误结构（所有语言检测器的输出）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Error {
    /// 语言代码（zh / en / ja ...）
    pub lang: String,
    /// 错误类型，见 [`error_types`]
    pub error_type: String,
    /// 置信度 0.0 ~ 1.0
    pub confidence: f64,
    /// 错误在原文中的字节偏移区间 [start, end)（Rust 字符串字节下标）
    pub span: [usize; 2],
    /// 原文片段
    pub original: String,
    /// 建议修正（第一建议的便捷访问，与 `suggestions[0]` 一致）
    pub correction: Option<String>,
    /// 人类可读的错误说明
    pub message: String,
    /// 规则分类（spelling / grammar / typography / style ...），
    /// 由词表 `rules` 元数据层提供；未匹配到规则时为 None
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// 规则唯一标识（词表 `rules` 数组中的 `id`），
    /// 用于 API 层按规则过滤/开关；未匹配到规则时为 None
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_id: Option<String>,
    /// 多个候选修正（按 score 降序）；
    /// 仅第一建议同步到 `correction` 字段
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<Suggestion>,
    /// W3C ITS 2.0 问题类型（misspelling / grammar / punctuation / untranslated / inconsistent / other）；
    /// 用于标准化输出，与内部 `error_type` 互补
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_type: Option<String>,
    /// 错误来源引擎（`rule` / `statistical` / `decision` / `llm`，见 [`sources`]）；
    /// 三级响应中 LLM 兜底层产出的错误标记为 `llm`，前端据此展示「LLM 兜底」标签
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// 除 [`source`](Error::source)（最终产出层）外，**其他层也独立命中过同一错误**
    /// 的来源列表（合并去重时落选层的来源记入此处）。
    /// 多层共识（如 `rule` + `llm` 双重确认）是更强的可信信号，前端据此展示
    /// 「多层共识」徽章。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub also_found_by: Vec<String>,
}

/// 错误来源引擎常量（[`Error::source`]）
pub mod sources {
    /// 规则引擎（各语言检测器）
    pub const RULE: &str = "rule";
    /// 统计引擎（Harper / HuggingFace 序列模型等外部服务）
    pub const STATISTICAL: &str = "statistical";
    /// 决策模型引擎（vegaml）
    pub const DECISION: &str = "decision";
    /// L3 特征模型（轻量特征分类器 + 机械修正；LLM 收缩到纯长尾后由它承担主力）
    pub const FEATURE: &str = "feature";
    /// LLM 引擎（三级响应兜底层）
    pub const LLM: &str = "llm";
    /// 知识召回层（②.5 句级向量召回 + 模糊命中）
    pub const KNOWLEDGE: &str = "knowledge";
}

impl Error {
    pub fn new(
        lang: impl Into<String>,
        error_type: impl Into<String>,
        confidence: f64,
        span: [usize; 2],
        original: impl Into<String>,
        correction: Option<String>,
        message: impl Into<String>,
    ) -> Self {
        let suggestions = correction
            .as_ref()
            .map(|c| vec![Suggestion::new(c.clone(), confidence)])
            .unwrap_or_default();
        Self {
            lang: lang.into(),
            error_type: error_type.into(),
            confidence,
            span,
            original: original.into(),
            correction,
            message: message.into(),
            category: None,
            rule_id: None,
            suggestions,
            issue_type: None,
            source: None,
            also_found_by: Vec::new(),
        }
    }

    /// 创建带规则元数据的错误（category + rule_id + 多建议）
    pub fn new_with_meta(
        lang: impl Into<String>,
        error_type: impl Into<String>,
        confidence: f64,
        span: [usize; 2],
        original: impl Into<String>,
        correction: Option<String>,
        message: impl Into<String>,
        category: Option<impl Into<String>>,
        rule_id: Option<impl Into<String>>,
        suggestions: Vec<Suggestion>,
    ) -> Self {
        Self {
            lang: lang.into(),
            error_type: error_type.into(),
            confidence,
            span,
            original: original.into(),
            correction,
            message: message.into(),
            category: category.map(|c| c.into()),
            rule_id: rule_id.map(|r| r.into()),
            suggestions,
            issue_type: None,
            source: None,
            also_found_by: Vec::new(),
        }
    }

    /// 记录「另一层也独立命中过本错误」（多层共识）。
    /// 与自身 `source` 相同、空来源或已记录过的来源会被忽略（幂等）。
    pub fn add_also_found_by(&mut self, src: &str) {
        if src.is_empty() || self.source.as_deref() == Some(src) {
            return;
        }
        if !self.also_found_by.iter().any(|s| s == src) {
            self.also_found_by.push(src.to_string());
        }
    }
}

/// 常见错误类型常量（与决策表输入对齐）。
pub mod error_types {
    // 中文
    pub const SPELLING: &str = "spelling";
    pub const GRAMMAR: &str = "grammar";
    pub const COLLOCATION: &str = "collocation";
    pub const PUNCTUATION: &str = "punctuation";
    /// 异常（AI 生成文本污染：助手回复 / 元讨论混入正文；非语言错误，不做 CEFR 归因）
    pub const ANOMALY: &str = "anomaly";
    // 英文
    pub const ARTICLE: &str = "article";
    pub const AGREEMENT: &str = "agreement";
    pub const TENSE: &str = "tense";
    pub const PREPOSITION: &str = "preposition";
    pub const MORPHOLOGY: &str = "morphology";
    pub const ADJECTIVE: &str = "adjective";
    pub const ADVERB: &str = "adverb";
    pub const NOUN: &str = "noun";
    pub const PRONOUN: &str = "pronoun";
    pub const VERB: &str = "verb";
    pub const INFINITIVE: &str = "infinitive";
    pub const GERUND: &str = "gerund";
    pub const SUBJUNCTIVE: &str = "subjunctive";
    pub const NEGATION: &str = "negation";
    // 日文
    pub const PARTICLE: &str = "particle";
    pub const CONJUGATION: &str = "conjugation";
    pub const HONORIFIC: &str = "honorific";
    pub const WORD_ORDER: &str = "word_order";
}

/// W3C ITS 2.0 标准问题类型常量。
/// 参考: https://www.w3.org/TR/its20/
pub mod issue_types {
    pub const MISSPELLING: &str = "misspelling";
    pub const GRAMMAR: &str = "grammar";
    pub const PUNCTUATION: &str = "punctuation";
    pub const UNTRANSLATED: &str = "untranslated";
    pub const INCONSISTENT: &str = "inconsistent";
    pub const OTHER: &str = "other";
}

/// 从内部 error_type 推导 W3C ITS 2.0 issue_type
pub fn derive_issue_type(error_type: &str) -> &'static str {
    match error_type {
        "spelling" | "typo" => issue_types::MISSPELLING,
        "punctuation" => issue_types::PUNCTUATION,
        "anomaly" => issue_types::OTHER,
        "grammar" | "article" | "agreement" | "tense" | "preposition" | "morphology"
        | "adjective" | "adverb" | "noun" | "verb" | "pronoun" | "infinitive" | "gerund"
        | "subjunctive" | "negation" | "collocation" | "particle" | "conjugation" | "honorific"
        | "word_order" => issue_types::GRAMMAR,
        _ => issue_types::OTHER,
    }
}

/// 把一组错误的建议修正应用到原文，返回「应用全部建议后」的确定性文本。
///
/// 数据沉淀用途：检测历史 `corrected_text` 列、采纳平行语料（资产 A/C）。
/// 规则：按 `span` 排序后从后往前替换（字节偏移，Rust 字符串语义）；
/// `correction` 为 `None` 的错误跳过（无建议不可应用）；`Some("")` = 删除；
/// 与已应用区间重叠的后续错误跳过（保留排序靠前者）。
pub fn apply_corrections(text: &str, errors: &[Error]) -> String {
    let mut items: Vec<(usize, usize, &str)> = errors
        .iter()
        .filter_map(|e| e.correction.as_deref().map(|c| (e.span[0], e.span[1], c)))
        .filter(|(a, b, _)| a < b && *b <= text.len())
        .collect();
    items.sort_by_key(|(a, b, _)| (*a, *b));
    // 前向贪心：跳过与已选区间重叠者（保留排序靠前者）
    let mut selected: Vec<(usize, usize, &str)> = Vec::new();
    let mut last_end = 0usize;
    for it in items {
        if it.0 >= last_end {
            selected.push(it);
            last_end = it.1;
        }
    }
    let mut out = text.to_string();
    for (a, b, corr) in selected.into_iter().rev() {
        out.replace_range(a..b, corr);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_serde_roundtrip() {
        let err = Error::new(
            "zh",
            error_types::SPELLING,
            0.95,
            [10, 12],
            "报复",
            Some("抱负".into()),
            "疑似同音错别字",
        );
        let json = serde_json::to_string(&err).unwrap();
        let back: Error = serde_json::from_str(&json).unwrap();
        assert_eq!(back.lang, "zh");
        assert_eq!(back.span, [10, 12]);
        assert_eq!(back.correction.as_deref(), Some("抱负"));
        // 新字段：correction 存在时 suggestions 自动填充
        assert_eq!(back.suggestions.len(), 1);
        assert_eq!(back.suggestions[0].text, "抱负");
        assert_eq!(back.suggestions[0].score, 0.95);
        // 未设元数据时 category/rule_id 为 None
        assert!(back.category.is_none());
        assert!(back.rule_id.is_none());
    }

    #[test]
    fn error_matches_spec_example() {
        // 规格文档 §5.1 示例（旧格式 JSON 反序列化兼容：新字段缺省）
        let json = r#"{
            "lang": "zh",
            "error_type": "spelling",
            "confidence": 0.95,
            "span": [10, 12],
            "original": "报复",
            "correction": "抱负",
            "message": "疑似同音错别字"
        }"#;
        let err: Error = serde_json::from_str(json).unwrap();
        assert_eq!(err.original, "报复");
        assert_eq!(err.correction.as_deref(), Some("抱负"));
        assert!(err.category.is_none());
        assert!(err.rule_id.is_none());
        assert!(err.suggestions.is_empty());
    }

    #[test]
    fn error_new_with_meta() {
        let err = Error::new_with_meta(
            "en",
            error_types::ADJECTIVE,
            0.9,
            [6, 16],
            "more better",
            Some("better".into()),
            "比较级误用",
            Some("grammar".to_string()),
            Some("EN_ADJECTIVE".to_string()),
            vec![
                Suggestion::new("better", 0.9),
                Suggestion::new("more good", 0.3),
            ],
        );
        assert_eq!(err.category.as_deref(), Some("grammar"));
        assert_eq!(err.rule_id.as_deref(), Some("EN_ADJECTIVE"));
        assert_eq!(err.suggestions.len(), 2);
        assert_eq!(err.suggestions[0].text, "better");
        assert_eq!(err.suggestions[1].score, 0.3);
        // 序列化后 category/rule_id 存在
        let json = serde_json::to_string(&err).unwrap();
        assert!(json.contains("\"category\":\"grammar\""));
        assert!(json.contains("\"rule_id\":\"EN_ADJECTIVE\""));
        assert!(json.contains("\"suggestions\""));
    }

    #[test]
    fn error_new_no_correction_no_suggestions() {
        let err = Error::new(
            "zh",
            error_types::GRAMMAR,
            0.5,
            [0, 2],
            "学习",
            None,
            "缺主语",
        );
        assert!(err.suggestions.is_empty());
        assert!(err.category.is_none());
    }

    #[test]
    fn also_found_by_serde_and_idempotent() {
        let mut err = Error::new("zh", error_types::SPELLING, 0.7, [0, 3], "a", None, "t");
        // 默认不序列化（向后兼容）
        assert_eq!(
            serde_json::to_string(&err)
                .unwrap()
                .contains("also_found_by"),
            false
        );
        err.source = Some("rule".into());
        err.add_also_found_by("rule"); // 与自身 source 相同 → 忽略
        err.add_also_found_by(""); // 空 → 忽略
        err.add_also_found_by("llm");
        err.add_also_found_by("llm"); // 幂等
        assert_eq!(err.also_found_by, vec!["llm".to_string()]);
        let json = serde_json::to_string(&err).unwrap();
        assert!(json.contains("\"also_found_by\":[\"llm\"]"));
        // 旧 JSON（无该字段）可正常反序列化
        let old = r#"{"lang":"zh","error_type":"spelling","confidence":0.7,"span":[0,3],"original":"a","message":"t"}"#;
        let back: Error = serde_json::from_str(old).unwrap();
        assert!(back.also_found_by.is_empty());
    }

    fn zh_err(text: &str, span: [usize; 2], original: &str, corr: Option<&str>) -> Error {
        let mut e = Error::new(
            "zh",
            error_types::GRAMMAR,
            0.8,
            span,
            original,
            corr.map(str::to_string),
            "t",
        );
        e.source = Some("llm".into());
        e
    }

    #[test]
    fn apply_corrections_multi_replacement_and_deletion() {
        // 「报复→抱负」与「的→删」两处独立编辑
        let text = "他报复很大。";
        let a = text.find("报复").unwrap();
        let text2 = "他的跑。";
        let b = text2.find("的").unwrap();
        // 分别验证替换与删除
        let out = apply_corrections(
            text,
            &[zh_err(text, [a, a + "报复".len()], "报复", Some("抱负"))],
        );
        assert_eq!(out, "他抱负很大。");
        let out2 = apply_corrections(text2, &[zh_err(text2, [b, b + "的".len()], "的", Some(""))]);
        assert_eq!(out2, "他跑。");
    }

    #[test]
    fn apply_corrections_skips_none_and_out_of_order() {
        let text = "他报复很大，进行修订。";
        let a = text.find("报复").unwrap();
        let c = text.find("修订").unwrap();
        let errs = vec![
            // 乱序输入：后一处先给出
            zh_err(text, [c, c + "修订".len()], "修订", Some("修正")),
            // correction=None → 跳过
            zh_err(text, [0, 1], "他", None),
            zh_err(text, [a, a + "报复".len()], "报复", Some("抱负")),
        ];
        assert_eq!(apply_corrections(text, &errs), "他抱负很大，进行修正。");
    }

    #[test]
    fn apply_corrections_overlapping_spans_keep_first() {
        let text = "他报复很大。";
        let a = text.find("报复").unwrap();
        let errs = vec![
            zh_err(text, [a, a + "报复".len()], "报复", Some("抱负")),
            // 与上一处重叠：跳过
            zh_err(text, [a + 1, a + "报复".len()], "复", Some("覆")),
        ];
        assert_eq!(apply_corrections(text, &errs), "他抱负很大。");
    }

    #[test]
    fn apply_corrections_adjacent_spans() {
        let text = "ABCD";
        let errs = vec![
            zh_err(text, [0, 2], "AB", Some("x")),
            zh_err(text, [2, 4], "CD", Some("y")),
        ];
        assert_eq!(apply_corrections(text, &errs), "xy");
    }
}
