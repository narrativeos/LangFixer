//! 把分析器检测到的英文错误映射到知识库的具体知识点（`grammar_point`）。
//!
//! 分析器产出 14 类错误（见 [`shared_model::error_types`] 英文部分），覆盖 25 类
//! 错误分类中词表/正则可达的类型：
//! - `article`（a↔an 不定冠词）/ `agreement`（主谓一致 -s）/ `tense`（过去时）/ `preposition`（介词替换）
//! - `adjective`（比较级/最高级）/ `adverb`（形副同形）/ `noun`（可数/不可数）
//! - `pronoun`（反身代词）/ `verb`（及物/不及物+介词）
//! - `infinitive`（不定式作宾语）/ `gerund`（动名词作宾语）
//! - `subjunctive`（虚拟语气）/ `negation`（双重否定）
//!
//! 每类映射到对应错误类型的具体知识点（点 key 为知识库的
//! `{errorType}.{序号}` 对齐），供决策表命中细粒度规则。无法确定时返回 `None`，
//! 调用方据此不设置 `grammar_point`，落到该讲兜底规则。

/// 介词 → 介词知识点序号（`preposition.N`，内置示例源介词单元）
fn prep_point(prep: &str) -> Option<usize> {
    Some(match prep {
        "about" => 13,
        "above" => 14,
        "after" => 15,
        "against" => 16,
        "at" => 17,
        "before" => 18,
        "beyond" => 19,
        "but" => 20,
        "by" => 21,
        "for" => 22,
        "from" => 23,
        "in" => 24,
        "into" => 25,
        "of" => 26,
        "on" => 27,
        "out of" => 28,
        "to" => 29,
        "under" => 30,
        "with" => 31,
        _ => return None,
    })
}

/// 依 `(error_type, original, correction)` 推断具体知识点 key（如 `"article.3"`）。
///
/// 静态兜底映射（分析器可达的 14 类 → 内置示例源对应知识点序号；
/// 数据驱动归点见 `attribution_engine::knowledge::classify_point_en`，
/// 知识库改错对命中优先于本映射）：
/// - `article`：分析器只报 a↔an，映射到 二.1 不定冠词（`article.3`）
/// - `tense`：过去时 → `tense.8`
/// - `agreement`：主谓一致 → `agreement.1`（代表点）
/// - `preposition`：按目标介词映射到 `preposition.N`；目标非表内介词时返回 `None`
/// - `adjective`：比较级/最高级误用 → `adjective.22`
/// - `adverb`：形副同形 → `adverb.11`
/// - `noun`：可数/不可数误用 → `noun.16`
/// - `pronoun`：反身代词缺失 → `pronoun.9`
/// - `verb`：及物/不及物+介词 → `verb.8`
/// - `infinitive`：不定式作动词宾语 → `infinitive.4`
/// - `gerund`：动名词作动词宾语 → `gerund.4`
/// - `subjunctive`：wish 后虚拟语气 → `subjunctive.8`
/// - `negation`：双重否定 → `negation.3`
pub fn classify_point(error_type: &str, original: &str, correction: &str) -> Option<String> {
    let o = original.trim().to_lowercase();
    let c = correction.trim().to_lowercase();
    match error_type {
        "article" => {
            if (o == "a" && c == "an") || (o == "an" && c == "a") {
                Some("article.3".to_string())
            } else {
                None
            }
        }
        "tense" => Some("tense.8".to_string()),
        "agreement" => Some("agreement.1".to_string()),
        "preposition" => prep_point(&c).map(|i| format!("preposition.{}", i)),
        "adjective" => Some("adjective.22".to_string()),
        "adverb" => Some("adverb.11".to_string()),
        "noun" => Some("noun.16".to_string()),
        "pronoun" => Some("pronoun.9".to_string()),
        "verb" => Some("verb.8".to_string()),
        "infinitive" => Some("infinitive.4".to_string()),
        "gerund" => Some("gerund.4".to_string()),
        "subjunctive" => Some("subjunctive.8".to_string()),
        "negation" => Some("negation.3".to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn article_a_to_an() {
        assert_eq!(
            classify_point("article", "a", "an").as_deref(),
            Some("article.3")
        );
    }

    #[test]
    fn article_an_to_a() {
        assert_eq!(
            classify_point("article", "an", "a").as_deref(),
            Some("article.3")
        );
    }

    #[test]
    fn tense_present_to_past() {
        assert_eq!(
            classify_point("tense", "go", "went").as_deref(),
            Some("tense.8")
        );
    }

    #[test]
    fn agreement_third_person() {
        assert_eq!(
            classify_point("agreement", "He play", "He plays").as_deref(),
            Some("agreement.1")
        );
    }

    #[test]
    fn preposition_on_to_in() {
        assert_eq!(
            classify_point("preposition", "on", "in").as_deref(),
            Some("preposition.24")
        );
    }

    #[test]
    fn preposition_unmapped_target() {
        assert_eq!(classify_point("preposition", "on", "across"), None);
    }

    #[test]
    fn non_en_type_returns_none() {
        assert_eq!(classify_point("spelling", "报复", "抱负"), None);
    }

    // ── 扩展错误类型 → 细点映射 ──

    #[test]
    fn adjective_maps_to_comparative() {
        assert_eq!(
            classify_point("adjective", "more better", "better").as_deref(),
            Some("adjective.22")
        );
    }

    #[test]
    fn adverb_maps_to_adj_adv() {
        assert_eq!(
            classify_point("adverb", "quiet", "quietly").as_deref(),
            Some("adverb.11")
        );
    }

    #[test]
    fn noun_maps_to_countable() {
        assert_eq!(
            classify_point("noun", "informations", "information").as_deref(),
            Some("noun.16")
        );
    }

    #[test]
    fn pronoun_maps_to_reflexive() {
        assert_eq!(
            classify_point("pronoun", "him", "himself").as_deref(),
            Some("pronoun.9")
        );
    }

    #[test]
    fn verb_maps_to_transitive() {
        assert_eq!(
            classify_point("verb", "discuss about", "discuss").as_deref(),
            Some("verb.8")
        );
    }

    #[test]
    fn infinitive_maps_to_verb_object() {
        assert_eq!(
            classify_point("infinitive", "want playing", "want to play").as_deref(),
            Some("infinitive.4")
        );
    }

    #[test]
    fn gerund_maps_to_verb_object() {
        assert_eq!(
            classify_point("gerund", "enjoy to play", "enjoy playing").as_deref(),
            Some("gerund.4")
        );
    }

    #[test]
    fn subjunctive_maps_to_wish() {
        assert_eq!(
            classify_point("subjunctive", "was", "were").as_deref(),
            Some("subjunctive.8")
        );
    }

    #[test]
    fn negation_maps_to_double_negative() {
        assert_eq!(
            classify_point("negation", "nothing", "anything").as_deref(),
            Some("negation.3")
        );
    }
}
