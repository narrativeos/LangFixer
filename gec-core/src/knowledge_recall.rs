//! ②.5 知识召回：trait 定义 + 句级匹配工具
//!
//! 知识召回层把「LLM 兜底 / 手动录入的已确认错误」沉淀为知识库，
//! 检测流水线在决策模型之后、LLM 兜底之前查询它——重复错误无需再付 LLM 成本。
//!
//! **句级召回**（本模块的核心设计）：
//! - 输入文本按句切分（[`split_sentences`]），**每句一个向量**做余弦匹配
//!   （实现侧批量嵌入，一次 HTTP 往返），命中带**句锚点**（[`RecallHit`]）；
//! - 命中后两级定位（[`knowledge_hit_to_error`]）：
//!   1. 知识条目文本是句子的**精确子串** → 精确 span（原有行为）；
//!   2. 非精确子串但字符 bigram 相似度 ≥ [`FUZZY_THRESHOLD`] → **句级粗定位**
//!      （模糊召回：同一错误句的轻微改写也能命中，span 放宽到整句）。
//!
//! 嵌入服务不可用时实现侧静默返回空列表，不阻断检测。

use async_trait::async_trait;
use shared_model::{Error, LearnedKnowledge, sources};

/// 模糊召回阈值：知识条目与句子的字符 bigram Jaccard 相似度 ≥ 该值 → 句级命中
pub const FUZZY_THRESHOLD: f64 = 0.8;

/// 知识召回命中：知识条目 + 它在输入文本中匹配到的句子（带字节偏移锚点）
#[derive(Debug, Clone)]
pub struct RecallHit {
    /// 命中的知识条目
    pub item: LearnedKnowledge,
    /// 句子向量与知识条目的余弦相似度
    pub sim: f64,
    /// 命中的句子原文（`sentence_span` 对应的文本）
    pub sentence: String,
    /// 句子在输入文本中的字节偏移 [start, end)
    pub sentence_span: (usize, usize),
}

/// 知识向量召回（检测流水线 ②.5）
///
/// 实现侧负责：句切分 + 批量嵌入 + 知识库余弦匹配；
/// 命中到错误项的转换由 [`knowledge_hit_to_error`] 在引擎侧完成。
#[async_trait]
pub trait KnowledgeRecall: Send + Sync {
    /// 句级召回：返回所有（句子 × 知识条目）余弦相似度达标的命中
    async fn recall(&self, lang: &str, text: &str) -> Vec<RecallHit>;
}

/// 按句切分文本，返回 `(句子原文, 起始字节偏移, 结束字节偏移)` 列表
///
/// 句子终结符：`。！？!?；;`（终结符归属前一句）；
/// 换行符（`\n`/`\r`）仅在当前片段已含实质内容时才断句，
/// 否则作为下一句的前导空白（避免空句 / 纯标点片段）；
/// 无终结符的尾部残余作为最后一句。
pub fn split_sentences(text: &str) -> Vec<(String, usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut seg_start = 0usize;
    for (i, ch) in text.char_indices() {
        let is_boundary = match ch {
            '.' | '。' | '！' | '？' | '!' | '?' | '；' | ';' => true,
            '\n' | '\r' => text[seg_start..i].chars().any(|c| c.is_alphanumeric()),
            _ => false,
        };
        if !is_boundary {
            continue;
        }
        let end = i + ch.len_utf8();
        let seg = text.get(start..end).unwrap_or("");
        if seg.chars().any(|c| c.is_alphanumeric()) {
            out.push((seg.to_string(), start, end));
        }
        // 无论片段是否为有效句子都推进起点（纯标点片段被消费掉）
        start = end;
        seg_start = end;
    }
    if start < text.len() {
        let seg = text.get(start..).unwrap_or("");
        if seg.chars().any(|c| c.is_alphanumeric()) {
            out.push((seg.to_string(), start, text.len()));
        }
    }
    out
}

fn char_bigrams(s: &str) -> Vec<(u32, u32)> {
    let cs: Vec<char> = s.chars().collect();
    cs.windows(2).map(|w| (w[0] as u32, w[1] as u32)).collect()
}

/// 字符 bigram Jaccard 相似度（多重集合交集 / 并集）：0.0 ~ 1.0
///
/// 用于 ②.5 模糊召回：知识条目与句子的 bigram 重叠度，
/// 对轻微改写（词序微调、增删个别词）保持较高相似度。
pub fn fuzzy_similarity(a: &str, b: &str) -> f64 {
    let (a, b) = (a.trim(), b.trim());
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    if a == b {
        return 1.0;
    }
    let ba = char_bigrams(a);
    let mut bb = char_bigrams(b);
    let mut inter = 0usize;
    for g in &ba {
        if let Some(pos) = bb.iter().position(|x| x == g) {
            inter += 1;
            bb.remove(pos);
        }
    }
    let union = ba.len() + bb.len();
    if union == 0 {
        0.0
    } else {
        inter as f64 / union as f64
    }
}

/// 知识召回命中 → 错误项（两级定位）
///
/// 1. 知识条目文本是命中句子的精确子串 → 精确 span（字节级）；
/// 2. 否则 bigram 相似度 ≥ [`FUZZY_THRESHOLD`] → 句级粗定位（span = 整句）。
/// 两级均未命中（理论上不会：召回阈值已过滤）→ `None`。
pub fn knowledge_hit_to_error(hit: &RecallHit) -> Option<Error> {
    let item = &hit.item;
    if item.text.trim().is_empty() {
        return None;
    }
    let correction = if item.correction.is_empty() {
        None
    } else {
        Some(item.correction.clone())
    };
    // ① 精确子串：精确定位
    if let Some(pos) = hit.sentence.find(&item.text) {
        let start = hit.sentence_span.0 + pos;
        let end = start + item.text.len();
        let mut err = Error::new(
            &item.lang,
            &item.error_type,
            0.8,
            [start, end],
            &item.text,
            correction,
            "知识召回：重复错误命中（精确）",
        );
        err.source = Some(sources::KNOWLEDGE.to_string());
        return Some(err);
    }
    // ② 模糊匹配：句级粗定位
    if fuzzy_similarity(&item.text, &hit.sentence) >= FUZZY_THRESHOLD {
        let (s, e) = hit.sentence_span;
        let mut err = Error::new(
            &item.lang,
            &item.error_type,
            0.6,
            [s, e],
            &hit.sentence,
            correction,
            "知识模糊召回：重复错误命中（句级）",
        );
        err.source = Some(sources::KNOWLEDGE.to_string());
        return Some(err);
    }
    None
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_sentences_basic() {
        let s = "第一句。第二句！第三句";
        let out = split_sentences(s);
        assert_eq!(out.len(), 3);
        // CJK 每字 3 字节：「第一句。」= 12 字节
        assert_eq!(out[0], ("第一句。".to_string(), 0, 12));
        assert_eq!(out[1], ("第二句！".to_string(), 12, 24));
        assert_eq!(out[2], ("第三句".to_string(), 24, 33));
    }

    #[test]
    fn split_sentences_english_and_newline() {
        let s = "Hello world. Second one!\nThird";
        let out = split_sentences(s);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].0, "Hello world.");
        // 句首空白保留在 span 内（original 与 span 一致）
        assert_eq!(out[1].0, " Second one!");
        assert_eq!(out[2].0, "\nThird");
        assert_eq!(out[2].2, s.len());
    }

    #[test]
    fn split_sentences_no_terminator_is_single() {
        let out = split_sentences("no terminator here");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0], ("no terminator here".to_string(), 0, 18));
    }

    #[test]
    fn split_sentences_skips_empty() {
        // 纯标点片段不算句子
        let out = split_sentences("。。。abc");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, "abc");
    }

    #[test]
    fn fuzzy_similarity_basics() {
        assert!((fuzzy_similarity("abc", "abc") - 1.0).abs() < 1e-9);
        assert!(fuzzy_similarity("abc", "xyz") < 0.1);
        // 轻微改写保持高相似度
        let a = "I saw a apple in the park";
        let b = "I saw a apple in the park.";
        assert!(fuzzy_similarity(a, b) > 0.9);
        // 完全不同
        assert!(fuzzy_similarity("the quick brown fox", "completely different text") < 0.5);
        assert_eq!(fuzzy_similarity("", "abc"), 0.0);
    }

    fn hit(item_text: &str, sentence: &str, span: (usize, usize)) -> RecallHit {
        RecallHit {
            item: LearnedKnowledge {
                id: 1,
                lang: "en".into(),
                text: item_text.into(),
                error_type: "article".into(),
                correction: "an apple".into(),
                embedding: vec![],
                source: "llm_adopt".into(),
                created_at: String::new(),
            },
            sim: 0.9,
            sentence: sentence.into(),
            sentence_span: span,
        }
    }

    #[test]
    fn exact_substring_gives_precise_span() {
        // 句子在文本中从偏移 10 开始；"a apple" 在句子内偏移 6 → 全局 [16, 23)
        let h = hit("a apple", "I saw a apple yesterday.", (10, 30));
        let e = knowledge_hit_to_error(&h).unwrap();
        assert_eq!(e.span, [16, 23]);
        assert_eq!(e.original, "a apple");
        assert_eq!(e.correction.as_deref(), Some("an apple"));
        assert_eq!(e.source.as_deref(), Some(sources::KNOWLEDGE));
    }

    #[test]
    fn fuzzy_match_gives_sentence_span() {
        // 知识条目与句子非子串但高度相似 → 句级 span
        let h = hit("I saw a apple.", "I saw a apples.", (0, 15));
        let e = knowledge_hit_to_error(&h).unwrap();
        assert_eq!(e.span, [0, 15]);
        assert_eq!(e.original, "I saw a apples.");
        assert!(e.confidence < 0.8, "模糊命中置信度低于精确命中");
    }

    #[test]
    fn dissimilar_hit_is_rejected() {
        let h = hit(
            "totally different sentence content",
            "I saw a apple.",
            (0, 14),
        );
        assert!(knowledge_hit_to_error(&h).is_none());
    }
}
