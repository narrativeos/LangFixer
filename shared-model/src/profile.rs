//! 统一错误画像模型（Error Profile）
//!
//! 学习场景的「错题本」与审校场景的「作者习惯性错误」是同一模型的两个视图：
//! **同一主体（会话）随时间沉淀的错误模式**。本模块定义统一投影：
//!
//! - 输入观测（`ErrorObservation`）：`basis = confirmed` 时来自确认错误（`mistakes`，
//!   权重 = 聚合次数）；`basis = detected` 时来自检测记录（`detect_history`，权重 1，
//!   兼容无采纳动作的旧会话）
//! - 输出（`ErrorProfile`）：类型分布 / 习惯性（主导）错误 + 示例 / 高频改错对 /
//!   掌握度摘要（仅 confirmed）
//!
//! 判定规则（两端共用，避免口径漂移）：主导 = 占比 ≥ [`DOMINANT_RATIO`]；
//! 高频改错对 = 出现 ≥ [`REPEATED_CORRECTION_MIN`] 次取前 [`TOP_CORRECTIONS_N`]；
//! 每主导类型示例最多 [`EXAMPLES_PER_TYPE`] 个。

use std::collections::HashMap;

use serde::Serialize;

use crate::session::Mistake;

/// 主导错误类型的占比阈值
pub const DOMINANT_RATIO: f64 = 0.3;
/// 「高频改错对」的最低出现次数
pub const REPEATED_CORRECTION_MIN: u32 = 2;
/// 高频改错对取前 N
pub const TOP_CORRECTIONS_N: usize = 5;
/// 每个主导类型收集的示例数
pub const EXAMPLES_PER_TYPE: usize = 5;

/// 画像数据基准：确认错误（mistakes）
pub const BASIS_CONFIRMED: &str = "confirmed";
/// 画像数据基准：检测记录（detect_history，旧会话回退）
pub const BASIS_DETECTED: &str = "detected";

/// 一条错误观测（weight = 该错误模式出现的次数）
#[derive(Debug, Clone)]
pub struct ErrorObservation {
    pub error_type: String,
    pub original: String,
    pub correction: Option<String>,
    pub weight: u32,
}

/// 习惯性（含主导判定）错误类型
#[derive(Debug, Clone, Serialize)]
pub struct HabitualErrorType {
    pub error_type: String,
    pub count: u32,
    pub ratio: f64,
    pub is_dominant: bool,
    /// 主导类型的示例（`original → correction` 去重，最多 [`EXAMPLES_PER_TYPE`] 个）
    pub examples: Vec<String>,
}

/// 高频改错对（同一 from→to 反复出现 = 典型习惯）
#[derive(Debug, Clone, Serialize)]
pub struct TopCorrection {
    pub from: String,
    pub to: String,
    pub count: u32,
}

/// 掌握度摘要（confirmed 基准下由 mistakes + mistake_mastery 带出）
#[derive(Debug, Clone, Serialize)]
pub struct MasterySummary {
    /// 确认错误条数（聚合后）
    pub confirmed: u32,
    /// 未复习过
    pub new: u32,
    /// 复习中
    pub reviewing: u32,
    /// 已掌握
    pub mastered: u32,
}

/// 统一错误画像（学习错题本与审校习惯性错误的同一投影）
#[derive(Debug, Clone, Serialize)]
pub struct ErrorProfile {
    pub session_id: String,
    /// 数据基准：`confirmed`（确认错误）/ `detected`（检测记录回退）
    pub basis: String,
    pub total_errors: u32,
    pub type_distribution: HashMap<String, u32>,
    /// 按 count 降序的全部类型（is_dominant 标记习惯性主导）
    pub habitual: Vec<HabitualErrorType>,
    pub top_corrections: Vec<TopCorrection>,
    /// 掌握度摘要（仅 confirmed 基准；detected 为 None）
    pub mastery_summary: Option<MasterySummary>,
}

/// 由错误观测构建画像投影（判定规则见模块文档）
pub fn build_error_profile(
    session_id: &str,
    basis: &str,
    observations: &[ErrorObservation],
    mastery: Option<MasterySummary>,
) -> ErrorProfile {
    let mut type_counts: HashMap<String, u32> = HashMap::new();
    let mut correction_pairs: HashMap<(String, String), u32> = HashMap::new();
    let mut total: u32 = 0;
    for o in observations {
        *type_counts.entry(o.error_type.clone()).or_insert(0) += o.weight;
        total += o.weight;
        if let Some(c) = &o.correction {
            *correction_pairs
                .entry((o.original.clone(), c.clone()))
                .or_insert(0) += o.weight;
        }
    }

    let mut habitual: Vec<HabitualErrorType> = type_counts
        .iter()
        .map(|(et, count)| HabitualErrorType {
            error_type: et.clone(),
            count: *count,
            ratio: if total > 0 {
                *count as f64 / total as f64
            } else {
                0.0
            },
            is_dominant: *count as f64 / total.max(1) as f64 >= DOMINANT_RATIO,
            examples: Vec::new(),
        })
        .collect();

    // 主导类型收集示例（去重，最多 EXAMPLES_PER_TYPE 个）
    for h in &mut habitual {
        if !h.is_dominant {
            continue;
        }
        let mut examples: Vec<String> = Vec::new();
        for o in observations {
            if examples.len() >= EXAMPLES_PER_TYPE {
                break;
            }
            if o.error_type != h.error_type {
                continue;
            }
            let example = match &o.correction {
                Some(c) => format!("{} → {}", o.original, c),
                None => o.original.clone(),
            };
            if !examples.contains(&example) {
                examples.push(example);
            }
        }
        h.examples = examples;
    }
    habitual.sort_by(|a, b| b.count.cmp(&a.count).then(a.error_type.cmp(&b.error_type)));

    let mut top_corrections: Vec<TopCorrection> = correction_pairs
        .into_iter()
        .filter(|(_, count)| *count >= REPEATED_CORRECTION_MIN)
        .map(|((from, to), count)| TopCorrection { from, to, count })
        .collect();
    top_corrections.sort_by(|a, b| b.count.cmp(&a.count).then(a.from.cmp(&b.from)));
    top_corrections.truncate(TOP_CORRECTIONS_N);

    ErrorProfile {
        session_id: session_id.to_string(),
        basis: basis.to_string(),
        total_errors: total,
        type_distribution: type_counts,
        habitual,
        top_corrections,
        mastery_summary: mastery,
    }
}

/// 由错题本（mistakes）构建 confirmed 基准的观测集（weight = 聚合次数）
pub fn observations_from_mistakes(mistakes: &[Mistake]) -> Vec<ErrorObservation> {
    mistakes
        .iter()
        .map(|m| ErrorObservation {
            error_type: m.error_type.clone(),
            original: m.original.clone(),
            correction: m.correction.clone(),
            weight: m.count.max(1),
        })
        .collect()
}

/// 由错题本（mistakes）构建掌握度摘要
pub fn mastery_summary_from_mistakes(mistakes: &[Mistake]) -> MasterySummary {
    let mut s = MasterySummary {
        confirmed: mistakes.len() as u32,
        new: 0,
        reviewing: 0,
        mastered: 0,
    };
    for m in mistakes {
        match m.mastery_status.as_deref() {
            Some("mastered") => s.mastered += 1,
            Some("reviewing") => s.reviewing += 1,
            _ => s.new += 1,
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(
        error_type: &str,
        original: &str,
        correction: Option<&str>,
        weight: u32,
    ) -> ErrorObservation {
        ErrorObservation {
            error_type: error_type.into(),
            original: original.into(),
            correction: correction.map(Into::into),
            weight,
        }
    }

    fn mistake(id: i64, status: Option<&str>) -> Mistake {
        Mistake {
            id,
            session_id: "s_1".into(),
            lang: "zh".into(),
            error_type: "spelling".into(),
            original: "a".into(),
            correction: Some("b".into()),
            sentence: "".into(),
            created_at: "".into(),
            count: 1,
            mastery_status: status.map(Into::into),
            next_review_at: None,
        }
    }

    #[test]
    fn dominant_and_top_corrections_and_examples() {
        let observations = vec![
            obs("spelling", "错吴", Some("错误"), 3),
            obs("spelling", "地址地址", Some("地址"), 1),
            obs("grammar", "的的", Some("的"), 1),
        ];
        let p = build_error_profile("s_1", BASIS_CONFIRMED, &observations, None);
        assert_eq!(p.total_errors, 5);
        assert_eq!(p.type_distribution.get("spelling"), Some(&4));
        let spelling = p
            .habitual
            .iter()
            .find(|h| h.error_type == "spelling")
            .unwrap();
        assert!(spelling.is_dominant, "占比 4/5 应判主导");
        let grammar = p
            .habitual
            .iter()
            .find(|h| h.error_type == "grammar")
            .unwrap();
        assert!(!grammar.is_dominant);
        assert_eq!(
            spelling.examples,
            vec!["错吴 → 错误".to_string(), "地址地址 → 地址".to_string()]
        );
        assert_eq!(p.top_corrections.len(), 1, "仅出现 ≥2 次的改错对入选");
        assert_eq!(p.top_corrections[0].from, "错吴");
        assert_eq!(p.top_corrections[0].count, 3);
    }

    #[test]
    fn mastery_summary_counts_by_status() {
        let s = mastery_summary_from_mistakes(&[
            mistake(1, Some("mastered")),
            mistake(2, Some("reviewing")),
            mistake(3, None),
        ]);
        assert_eq!((s.confirmed, s.mastered, s.reviewing, s.new), (3, 1, 1, 1));
    }
}
