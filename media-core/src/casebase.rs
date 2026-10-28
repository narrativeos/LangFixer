//! 案例库（开卷复现）：把「已经确认过的判定」变成可复用的案例
//!
//! **诚实边界（必须写进报告与评测，不能含糊）**：
//! - 案例库命中 = **记忆**，不是能力。它能证明两件事：
//!   1. 「抽取 → 定位 → 回映射 → 导出」链路无损（同一份判定重演可 100% 复现，回归门）；
//!   2. 学习闭环可用（确认过的判定下次不再付检测成本）。
//! - 它**不能**用来宣称识别能力，因此评测必须把 **开卷 / 闭卷** 两个数分开报。
//! - 命中行一律带 `detector = "case"` 溯源，导出表与 JSON 里都能看出路。
//!
//! 匹配口径：归一化（去空白 + 全角标点转半角，见 [`gold::normalize`]）后的**精确子串**，
//! 返回 stitched text 的字节偏移——与 [`stitch::remap`] 同一坐标契约，可直接进回映射。

use serde::{Deserialize, Serialize};

use crate::gold::{self, GoldItem};

/// 案例条目最短字符数：短于此（单个标点、单个字）在案例库里必然误伤，直接跳过
pub const MIN_CASE_CHARS: usize = 2;

/// 一条案例：一段「已确认的原文片段」+ 当时的判定
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Case {
    /// 案例原文（匹配主键；通常是专家引用的那段）
    pub original: String,
    /// 当时的处理建议
    #[serde(default)]
    pub suggestion: String,
    /// 是否计入差错数
    #[serde(default)]
    pub counted: bool,
    /// 判定依据 / 备注
    #[serde(default)]
    pub note: String,
    /// 来源留痕（`gold` = 由正误表导入；`manual` = 人工录入；`confirmed` = 用户确认过）
    #[serde(default = "default_case_source")]
    pub source: String,
}

fn default_case_source() -> String {
    "manual".to_string()
}

/// 一次案例命中：案例下标 + 在载体文本中的字节区间
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaseHit {
    /// [`CaseBase::cases`] 中的下标
    pub case: usize,
    /// stitched text 的字节区间 `[start, end)`
    pub span: (usize, usize),
}

/// 案例库
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CaseBase {
    #[serde(default)]
    pub cases: Vec<Case>,
}

impl CaseBase {
    /// 由 gold 集（专家正误表）构造案例库——**开卷**：把答案本身作为案例
    ///
    /// 用途是「同一份报告 100% 复现」的回归门与学习闭环验证，**不是**能力评测；
    /// 能力评测必须用没进案例库的材料（留出集 / 第二份专家报告）。
    pub fn from_gold(gold_items: &[GoldItem]) -> Self {
        Self {
            cases: gold_items
                .iter()
                .map(|g| Case {
                    original: g.original.clone(),
                    suggestion: g.suggestion.clone(),
                    counted: g.counted,
                    note: g.note.clone(),
                    source: "gold".to_string(),
                })
                .collect(),
        }
    }

    pub fn len(&self) -> usize {
        self.cases.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cases.is_empty()
    }

    /// 在文本里找所有案例命中（归一化精确子串；同一案例多处出现全部返回）
    pub fn match_text(&self, text: &str) -> Vec<CaseHit> {
        let (chars, spans) = normalized_index(text);
        let mut hits = Vec::new();
        for (ci, case) in self.cases.iter().enumerate() {
            let needle: Vec<char> = gold::normalize(&case.original).chars().collect();
            if needle.len() < MIN_CASE_CHARS || needle.len() > chars.len() {
                continue;
            }
            let mut i = 0usize;
            while i + needle.len() <= chars.len() {
                if chars[i..i + needle.len()] == needle[..] {
                    hits.push(CaseHit {
                        case: ci,
                        span: (spans[i].0, spans[i + needle.len() - 1].1),
                    });
                    i += needle.len();
                } else {
                    i += 1;
                }
            }
        }
        hits
    }
}

/// 归一化索引：`(归一化后的字符序列, 每个归一化字符在原文中的字节区间)`
///
/// 归一化会丢空白并做全角→半角映射，因此必须保留原字节区间，
/// 命中才能换算回 stitched text 的 span（[`shared_model::Error::span`] 契约）。
fn normalized_index(text: &str) -> (Vec<char>, Vec<(usize, usize)>) {
    let mut chars = Vec::with_capacity(text.len());
    let mut spans = Vec::with_capacity(text.len());
    for (i, c) in text.char_indices() {
        if c.is_whitespace() {
            continue;
        }
        chars.push(gold::normalize_char(c));
        spans.push((i, i + c.len_utf8()));
    }
    (chars, spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case(original: &str) -> Case {
        Case {
            original: original.into(),
            suggestion: "将「报道」改为「报到」".into(),
            counted: true,
            note: "【差错】错别字".into(),
            source: "gold".into(),
        }
    }

    #[test]
    fn matches_through_whitespace_and_full_half_width() {
        let cb = CaseBase {
            cases: vec![case("分时段进入病区报道。")],
        };
        // 抽取噪声：多余空格 + 半角句号
        let text = "请 分时段进入病区报道.";
        let hits = cb.match_text(text);
        assert_eq!(hits.len(), 1, "空白与全半角差异不得影响案例命中");
        let (s, e) = hits[0].span;
        // 归一化让「。」与「.」等价，因此 span 覆盖到句末标点（原文原样返回）
        assert_eq!(&text[s..e], "分时段进入病区报道.");
    }

    #[test]
    fn too_short_cases_are_skipped_and_multiple_occurrences_all_hit() {
        let cb = CaseBase {
            cases: vec![case("："), case("微信小程序：掌上阜外医院")],
        };
        let text = "微信小程序：掌上阜外医院 与 微信小程序：掌上阜外医院";
        let hits = cb.match_text(text);
        assert_eq!(hits.len(), 2, "单字符案例跳过，长案例两处都算");
        assert!(hits.iter().all(|h| h.case == 1), "{hits:?}");
    }

    #[test]
    fn from_gold_carries_verdict_fields() {
        let rows = vec![vec![
            "1".to_string(),
            "图片1 术前签字第2项".to_string(),
            "分时段进入病区报道".to_string(),
            "将「报道」改为「报到」".to_string(),
            "计1".to_string(),
            "【差错】错别字".to_string(),
        ]];
        let gold = gold::parse_gold_rows(&rows);
        let cb = CaseBase::from_gold(&gold);
        assert_eq!(cb.len(), 1);
        assert!(cb.cases[0].counted && cb.cases[0].source == "gold");
    }
}
