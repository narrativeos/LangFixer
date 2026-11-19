//! 文档切块与所有权图：文档级（10 万字+）LLM 并行复查的底座
//!
//! 设计契约（文档级架构重构 P1）：
//! - [`split_document`] 产出的各块 `owner` 区间**逐字节铺满全文且互不重叠**：
//!   每个字符恰好归属一个块（所有权图）。跨块现象（重复上报、合并歧义）
//!   从结构上消除——块结果直接按文档偏移拼接，不再依赖跨块 span 去重。
//! - 每块实际送 LLM 的文本 `send` = 上下文前缀（上一块尾部，只读）+ 归属段。
//!   上下文前缀仅供模型理解跨块语境；落在前缀区的错误归上一块所有，
//!   由执行器（[`crate::llm::LlmGecEngine::detect_document`]）丢弃，避免双报。
//! - 偏移一律**字节偏移**（与 [`shared_model::Error::span`] 同语义）。

/// 缺省目标块大小（字符数）：约 1–2k token，处于多数模型的稳定检测窗口，
/// 且单块失败/重试的成本足够小
pub const DEFAULT_CHUNK_CHARS: usize = 1500;
/// 缺省上下文前缀长度（字符数）：上一块尾部送作只读上下文
pub const DEFAULT_CONTEXT_CHARS: usize = 120;
/// 段落超过 `target * LONG_PARA_FACTOR` 时按句切分；单句仍超过同阈值按字数硬切
const LONG_PARA_FACTOR: usize = 2;

/// 一个审校块
#[derive(Debug, Clone, PartialEq)]
pub struct DocChunk {
    /// 归属区间（文档字节偏移 `[start, end)`，相邻块首尾相接、互不重叠）
    pub owner: [usize; 2],
    /// 上下文前缀字节数（`send` 头部属于上一块的部分，本块不拥有）
    pub context_prefix: usize,
    /// 实际送 LLM 的文本（上下文前缀 + 归属段；`send[context_prefix..]` == 归属段原文）
    pub send: String,
}

/// 按段落边界（超长段落按句、超长句按字数硬切）把全文切成目标约
/// `target_chars` 字符的块，返回带所有权与上下文前缀的块列表。
///
/// 不变式（由测试锁定）：
/// - 空文本 → 空列表；
/// - `owner` 区间从 0 起首尾相接铺满 `text.len()`，互不重叠；
/// - `send` 以归属段结尾：`send[context_prefix..] == text[owner]`；
/// - 上下文前缀不超过 [`DEFAULT_CONTEXT_CHARS`] 字符。
pub fn split_document(text: &str, target_chars: usize) -> Vec<DocChunk> {
    split_document_with_context(text, target_chars, DEFAULT_CONTEXT_CHARS)
}

/// [`split_document`] 的可注入上下文长度版本（测试与特殊场景用）
pub fn split_document_with_context(
    text: &str,
    target_chars: usize,
    context_chars: usize,
) -> Vec<DocChunk> {
    if text.is_empty() {
        return Vec::new();
    }
    let target = target_chars.max(50);
    // ① 归属区间序列（铺满全文）
    let mut owned: Vec<[usize; 2]> = Vec::new();
    let mut cur_start = 0usize;
    let mut cur_chars = 0usize;
    let mut pos = 0usize;
    while pos < text.len() {
        // 段落 = 到下一个 '\n'（含）
        let para_end = match text[pos..].find('\n') {
            Some(i) => pos + i + 1,
            None => text.len(),
        };
        // 超长段落按句切；其余保持段落原子性
        let units: Vec<[usize; 2]> = if para_end - pos > target * LONG_PARA_FACTOR {
            split_sentences(text, pos, para_end, target)
        } else {
            vec![[pos, para_end]]
        };
        for u in units {
            let u_chars = text[u[0]..u[1]].chars().count();
            if cur_chars > 0 && cur_chars + u_chars > target {
                owned.push([cur_start, u[0]]);
                cur_start = u[0];
                cur_chars = 0;
            }
            cur_chars += u_chars;
        }
        pos = para_end;
    }
    if cur_start < text.len() {
        owned.push([cur_start, text.len()]);
    }
    // ② 上下文前缀 + send 文本
    owned
        .iter()
        .enumerate()
        .map(|(i, o)| {
            let ctx_start = if i == 0 {
                o[0]
            } else {
                back_chars(text, o[0], context_chars)
            };
            DocChunk {
                owner: *o,
                context_prefix: o[0] - ctx_start,
                send: text[ctx_start..o[1]].to_string(),
            }
        })
        .collect()
}

/// 句级切分（结构切分，Detect Pipeline v2 §3.1）：按句子边界
/// （。！？；!?; 与换行）把全文切成**字节 span** 序列，铺满全文且互不重叠。
///
/// 与 [`split_document`] 内部的超长段按句切分同一套边界字符；句级切分是
/// 全局唯一的一次结构切分（句表），供句覆盖统计、③-T 定向复查消费方复用；
/// 其确定性只随 `shared_model::SPLITTER_VERSION` 变。空文本 → 空列表。
pub fn sentence_spans(text: &str) -> Vec<[usize; 2]> {
    // target 取半上限：hard_cap = target×2 不溢出，且任何句子都不会超预算被硬切
    split_sentences(text, 0, text.len(), usize::MAX >> 1)
}

/// 段级所有权铺满校验（截断探测器，Detect Pipeline v2 §7 告警①）：
/// 返回 `[0, text.len())` 内**不属于任何区间**的字节数。
/// 区间需按起址排序传入；重叠不扣分（所有权去重负责）。
pub fn unowned_bytes(text_len: usize, ranges: &[[usize; 2]]) -> usize {
    let mut covered = 0usize;
    let mut reach = 0usize; // 已覆盖到的字节位置
    for r in ranges {
        let s = r[0].max(reach);
        if r[1] > s {
            covered += r[1] - s;
            reach = r[1];
        }
    }
    text_len.saturating_sub(covered.min(text_len))
}

/// 段落内按句边界切分；单句仍超过 `target * LONG_PARA_FACTOR` 时按字数硬切
fn split_sentences(text: &str, start: usize, end: usize, target: usize) -> Vec<[usize; 2]> {
    let mut segs: Vec<[usize; 2]> = Vec::new();
    let mut seg_start = start;
    for (off, ch) in text[start..end].char_indices() {
        let is_break = matches!(
            ch,
            '。' | '！' | '？' | '；' | '!' | '?' | ';' | '\n' | '\r'
        );
        if is_break {
            let b = start + off + ch.len_utf8();
            segs.push([seg_start, b]);
            seg_start = b;
        }
    }
    if seg_start < end {
        segs.push([seg_start, end]);
    }
    let hard_cap = target * LONG_PARA_FACTOR;
    segs.into_iter()
        .flat_map(|[s, e]| {
            if text[s..e].chars().count() > hard_cap {
                hard_split(text, s, e, hard_cap)
            } else {
                vec![[s, e]]
            }
        })
        .collect()
}

/// 按字符数硬切（UTF-8 边界安全）
fn hard_split(text: &str, start: usize, end: usize, max_chars: usize) -> Vec<[usize; 2]> {
    let mut out = Vec::new();
    let mut seg_start = start;
    let mut n = 0usize;
    for (off, ch) in text[start..end].char_indices() {
        n += 1;
        if n == max_chars {
            let b = start + off + ch.len_utf8();
            out.push([seg_start, b]);
            seg_start = b;
            n = 0;
        }
    }
    if seg_start < end {
        out.push([seg_start, end]);
    }
    out
}

/// 从字节位置 `from` 向前回退至多 `chars` 个字符，返回字符边界对齐的字节偏移
fn back_chars(text: &str, from: usize, chars: usize) -> usize {
    let mut pos = from.min(text.len());
    // 对齐到字符边界（owner 边界理论上总是对齐的——防御性回退）
    while pos > 0 && !text.is_char_boundary(pos) {
        pos -= 1;
    }
    let mut n = 0;
    while pos > 0 && n < chars {
        pos -= 1;
        while pos > 0 && !text.is_char_boundary(pos) {
            pos -= 1;
        }
        n += 1;
    }
    pos
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 不变式：owner 铺满全文、互不重叠；send 以归属段结尾
    fn assert_invariants(text: &str, chunks: &[DocChunk]) {
        if text.is_empty() {
            assert!(chunks.is_empty());
            return;
        }
        assert_eq!(chunks[0].owner[0], 0);
        assert_eq!(chunks.last().unwrap().owner[1], text.len());
        for w in chunks.windows(2) {
            assert_eq!(w[0].owner[1], w[1].owner[0], "块必须首尾相接");
        }
        for c in chunks {
            assert!(c.owner[0] < c.owner[1], "不允许空块");
            assert!(text.is_char_boundary(c.owner[0]));
            assert!(text.is_char_boundary(c.owner[1]));
            assert_eq!(
                &c.send[c.context_prefix..],
                &text[c.owner[0]..c.owner[1]],
                "send 的归属段必须与原文一致"
            );
            assert!(c.send[..c.context_prefix].chars().count() <= DEFAULT_CONTEXT_CHARS);
        }
    }

    #[test]
    fn empty_text_yields_no_chunks() {
        assert_invariants("", &split_document("", 1000));
    }

    #[test]
    fn short_text_single_chunk_no_context() {
        let t = "他报复很大。".to_string();
        let cs = split_document(&t, 1500);
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].context_prefix, 0);
        assert_eq!(cs[0].send, t);
        assert_invariants(&t, &cs);
    }

    #[test]
    fn paragraphs_tile_document() {
        let para = "这是一段六十个字符左右的中文段落，用来测试切块的段落原子性与偏移正确性，内容本身没有特别含义。一二三四五六七八九〇";
        let t = format!("{para}\n{para}\n{para}\n");
        let cs = split_document(&t, 50); // 段落约 60 字 > 目标 50 → 每段一块
        assert!(cs.len() >= 3, "三段至少三块: {}", cs.len());
        assert_invariants(&t, &cs);
        // 第二块起带上下文前缀（上一块尾部）
        assert!(cs[1].context_prefix > 0);
    }

    #[test]
    fn long_paragraph_splits_by_sentences_and_tiles() {
        let sent = "这个句子用来测试超长段落按句切分后的铺满不变式是否正确。";
        let t = sent.repeat(40); // 单段约 1500+ 字，无换行
        let cs = split_document(&t, 200);
        assert!(cs.len() > 5);
        assert_invariants(&t, &cs);
    }

    #[test]
    fn mixed_cjk_ascii_multibyte_tiles_exactly() {
        let mut t = String::new();
        for i in 0..50 {
            t.push_str(&format!("第 {i} 段：混合中英文 with ascii, 标点。！？；\n"));
        }
        let cs = split_document(&t, 120);
        assert!(cs.len() > 3);
        assert_invariants(&t, &cs);
    }

    #[test]
    fn context_prefix_capped_at_context_chars() {
        let para = "甲".repeat(300);
        let t = format!("{para}\n{para}\n");
        let cs = split_document(&t, 1500);
        assert_invariants(&t, &cs);
        for c in &cs {
            assert!(c.send[..c.context_prefix].chars().count() <= DEFAULT_CONTEXT_CHARS);
        }
    }

    #[test]
    fn sentence_spans_tile_full_text() {
        let t = "他昨天报复很大。今天心情不错！真的吗？好的；就这样\n第二句 here. 结尾无标点";
        let sp = sentence_spans(t);
        assert!(!sp.is_empty());
        // 铺满不变式：首尾相接、无空洞、无重叠
        assert_eq!(sp[0][0], 0);
        assert_eq!(sp.last().unwrap()[1], t.len());
        for w in sp.windows(2) {
            assert_eq!(w[0][1], w[1][0]);
        }
        assert_eq!(unowned_bytes(t.len(), &sp), 0);
        for s in &sp {
            assert!(t.is_char_boundary(s[0]) && t.is_char_boundary(s[1]));
        }
        // 空文本
        assert!(sentence_spans("").is_empty());
    }

    #[test]
    fn unowned_bytes_detects_gaps() {
        // [0,3) + [5,8) 在 [0,10) 内留下 [3,5) 与 [8,10) 共 4 字节空洞
        assert_eq!(unowned_bytes(10, &[[0, 3], [5, 8]]), 4);
        // 重叠不重复计数也不扣分
        assert_eq!(unowned_bytes(10, &[[0, 6], [3, 10]]), 0);
        assert_eq!(unowned_bytes(0, &[]), 0);
    }
}
