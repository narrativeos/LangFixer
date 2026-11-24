//! 块风险分诊（triage）：本地统计信号给块打风险分，**不替代** LLM 复查
//!
//! 定位（架构调研结论）：本地规则层在评测里几乎不贡献检出（MUCGEC 21/7258），
//! 因此本地信号**不做主检测**，只做两件事——
//! 1. **排队序**：高风险块先送 LLM（进度更有价值，取消时损失的是低风险块）；
//! 2. **预算分配**：`DocBudget::SkipLowRisk` 下低风险（fast）块跳过深度复查，
//!    如实计入 `skipped`（不算覆盖率）——用可控的召回换成本。
//!
//! 信号全部是零依赖统计量（标点异常 / 括号配对 / 超长句 / 重复句 / 字符异常）；
//! 宿主可经 [`set_extra_signal`] 注入更重的信号源（如 gec-zh 词典规则命中
//! 密度）——eval 实测纯统计信号对语义级错误（同音替换等）近乎全盲，
//! 词典命中是把「统计干净但含真实错误」的块抬出 fast 区的关键信号。

use std::sync::{Arc, OnceLock};

/// fast 阈值：风险分低于此值的块在预算模式下可跳过深度复查
pub const FAST_THRESHOLD: f64 = 0.2;
/// deep 阈值：高于此值的块值得更深的复查（当前仅作排队优先级标注）
pub const DEEP_THRESHOLD: f64 = 0.6;

/// 宿主注入的附加信号源（输入块归属区文本，输出该信号源的风险分与信号名）
pub type SignalFn = dyn Fn(&str) -> ChunkRisk + Send + Sync;

static EXTRA_SIGNAL: OnceLock<Arc<SignalFn>> = OnceLock::new();

/// 注册宿主侧附加信号源（进程内一次性；重复注册返回 Err）。
///
/// 典型用法：api-server 启动时注入 gec-zh `rules::detect_local` 的命中密度，
/// 使词典可见的疑似错误块不被预算模式跳过。gec-core 独立使用时不注册，
/// 行为与纯统计信号完全一致。
pub fn set_extra_signal(f: Arc<SignalFn>) -> Result<(), ()> {
    EXTRA_SIGNAL.set(f).map_err(|_| ())
}

/// 块风险（归属区文本的统计画像）
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChunkRisk {
    /// 综合风险分 ∈ [0, 1]
    pub score: f64,
    /// 命中的信号名（可解释性：UI 展示「为什么这块先查」）
    pub signals: Vec<String>,
}

/// 复查模式（由风险分映射；当前 deep/standard 都走完整 LLM 复查，
/// fast 仅在预算模式下被跳过）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewMode {
    Fast,
    Standard,
    Deep,
}

impl ReviewMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            ReviewMode::Fast => "fast",
            ReviewMode::Standard => "standard",
            ReviewMode::Deep => "deep",
        }
    }
}

pub fn review_mode(score: f64) -> ReviewMode {
    if score < FAST_THRESHOLD {
        ReviewMode::Fast
    } else if score >= DEEP_THRESHOLD {
        ReviewMode::Deep
    } else {
        ReviewMode::Standard
    }
}

/// 对块**归属区**文本做风险打分（上下文前缀不参与——那不是本块所有的内容）
///
/// 已注册的宿主附加信号源（[`set_extra_signal`]）自动并入：分数相加封顶 1.0，
/// 信号名拼接。
pub fn chunk_risk(text: &str) -> ChunkRisk {
    match EXTRA_SIGNAL.get() {
        Some(f) => chunk_risk_with(text, Some(f.as_ref())),
        None => chunk_risk_with(text, None),
    }
}

/// 显式传入附加信号源的风险打分（[`chunk_risk`] 的可测版本，不依赖进程全局）
pub fn chunk_risk_with(text: &str, extra: Option<&dyn Fn(&str) -> ChunkRisk>) -> ChunkRisk {
    let mut r = statistical_risk(text);
    if let Some(f) = extra {
        let x = f(text);
        if !x.signals.is_empty() || x.score > 0.0 {
            r.score = (r.score + x.score).min(1.0);
            r.signals.extend(x.signals);
        }
    }
    r
}

/// 纯统计信号风险分（零依赖，不含宿主注入信号）
fn statistical_risk(text: &str) -> ChunkRisk {
    let mut r = ChunkRisk::default();
    let add = |score: &mut f64, signals: &mut Vec<String>, name: &str, w: f64| {
        *score = (*score + w).min(1.0);
        signals.push(name.to_string());
    };

    // ① 标点异常：连续重复标点（。。！！？等）与中英标点紧邻混用（,。！？等）
    let mut punct_repeat = 0usize;
    let mut punct_mix = 0usize;
    let chars: Vec<char> = text.chars().collect();
    let is_punct = |c: char| "，。！？；：、,.!?;:".contains(c);
    for w in chars.windows(2) {
        if is_punct(w[0]) && is_punct(w[1]) {
            if w[0] == w[1] {
                punct_repeat += 1;
            } else {
                punct_mix += 1;
            }
        }
    }
    if punct_repeat + punct_mix > 0 {
        add(
            &mut r.score,
            &mut r.signals,
            "punct_anomaly",
            (0.3 + 0.1 * (punct_repeat + punct_mix) as f64).min(0.5),
        );
    }

    // ② 括号/引号不配对（中英文分别计数，净差非零即异常）
    let pairs = [
        ('（', '）'),
        ('(', ')'),
        ('「', '」'),
        ('“', '”'),
        ('【', '】'),
        ('《', '》'),
    ];
    let unbalanced = pairs.iter().any(|(l, r)| {
        text.chars().filter(|c| c == l).count() != text.chars().filter(|c| c == r).count()
    });
    if unbalanced {
        add(&mut r.score, &mut r.signals, "unbalanced_pair", 0.3);
    }

    // ③ 超长句：>150 字无句终标点（丢失句子边界，模型与人都易漏检）
    let mut run = 0usize;
    let mut has_long_sentence = false;
    for c in &chars {
        run += 1;
        if "。！？!?.\n".contains(*c) {
            run = 0;
        } else if run > 150 {
            has_long_sentence = true;
            break;
        }
    }
    if has_long_sentence {
        add(&mut r.score, &mut r.signals, "long_sentence", 0.2);
    }

    // ④ 重复句：相邻两句完全相同（复制粘贴残留的高发形态）
    let sents: Vec<&str> = text
        .split(['。', '！', '？', '.', '!', '?', '\n'])
        .map(str::trim)
        .filter(|s| s.chars().count() >= 8)
        .collect();
    if sents.windows(2).any(|w| w[0] == w[1]) {
        add(&mut r.score, &mut r.signals, "repeated_sentence", 0.3);
    }

    // ⑤ 字符异常：控制字符（非 \n\t）或 3+ 连续空格（格式/编码残留）
    let ctrl = text
        .chars()
        .filter(|c| c.is_control() && *c != '\n' && *c != '\t')
        .count();
    let space_runs = text.match_indices("   ").count();
    if ctrl > 0 || space_runs > 0 {
        add(&mut r.score, &mut r.signals, "char_anomaly", 0.2);
    }

    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_text_is_fast() {
        let t = "这是一段完全正常的中文段落，句子结构清晰，标点使用规范。\n另一句同样规范的句子，没有任何异常。\n";
        let r = chunk_risk(t);
        assert!(r.score < FAST_THRESHOLD, "干净文本应为 fast: {r:?}");
        assert!(r.signals.is_empty(), "{r:?}");
        assert_eq!(review_mode(r.score), ReviewMode::Fast);
    }

    #[test]
    fn punct_anomaly_raises_score() {
        let r = chunk_risk("这句话有重复标点。。还有混用,。需要复查。");
        assert!(r.signals.contains(&"punct_anomaly".to_string()), "{r:?}");
        assert!(r.score >= FAST_THRESHOLD, "{r:?}");
    }

    #[test]
    fn unbalanced_pair_and_repetition_detected() {
        let r1 = chunk_risk("这段文字的（括号没有闭合，属于配对异常。");
        assert!(
            r1.signals.contains(&"unbalanced_pair".to_string()),
            "{r1:?}"
        );
        let dup = "这是一个足够长的句子用来触发重复检测的信号阈值。";
        let r2 = chunk_risk(&format!("{dup}。{dup}。\n"));
        assert!(
            r2.signals.contains(&"repeated_sentence".to_string()),
            "{r2:?}"
        );
        assert!(r2.score >= FAST_THRESHOLD);
    }

    #[test]
    fn long_sentence_detected() {
        let t = "没有句终标点的超长句子".repeat(30);
        let r = chunk_risk(&t);
        assert!(r.signals.contains(&"long_sentence".to_string()), "{r:?}");
    }

    #[test]
    fn char_anomaly_detected() {
        let r = chunk_risk("文字里有\u{1}控制字符，还有   三个连续空格。");
        assert!(r.signals.contains(&"char_anomaly".to_string()), "{r:?}");
    }

    #[test]
    fn mode_thresholds() {
        assert_eq!(review_mode(0.0), ReviewMode::Fast);
        assert_eq!(review_mode(0.3), ReviewMode::Standard);
        assert_eq!(review_mode(0.9), ReviewMode::Deep);
    }

    #[test]
    fn extra_signal_lifts_clean_text_out_of_fast() {
        let clean = "这是一段完全正常的中文段落，句子结构清晰，标点使用规范。\\n另一句同样规范的句子，没有任何异常。\\n";
        let base = chunk_risk_with(clean, None);
        assert!(base.score < FAST_THRESHOLD);
        // 词典命中类信号：干净统计画像 + 命中 → 抬出 fast 区
        let r = chunk_risk_with(
            clean,
            Some(&|_t| ChunkRisk {
                score: 0.5,
                signals: vec!["lexicon_hit:2".into()],
            }),
        );
        assert!(r.score >= FAST_THRESHOLD, "{r:?}");
        assert!(r.signals.contains(&"lexicon_hit:2".to_string()), "{r:?}");
    }

    #[test]
    fn extra_signal_score_caps_at_one() {
        let r = chunk_risk_with(
            "标点异常。。测试。",
            Some(&|_t| ChunkRisk {
                score: 0.9,
                signals: vec!["x".into()],
            }),
        );
        assert!(r.score <= 1.0, "{r:?}");
    }
}
