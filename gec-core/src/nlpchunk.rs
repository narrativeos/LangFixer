//! NLP `/analyze` 长文本分块
//!
//! NLP sidecar（HanLP）对长文本是**三次方级劣化**：500 字 ≈ 1.9s、1000 字 ≈ 18s、
//! 6200 字外推 ≈ 数小时。客户端超时放弃后服务端仍继续慢磨，其后所有请求
//! （含健康探针）排队陪葬，表现为服务「挂死」。因此 `POST /analyze` 必须按句
//! 边界切成小块**串行**发送。
//!
//! 与 [`crate::docchunk`]（LLM 审校块，容忍段落原子性、块可达 2×target）不同，
//! 本模块保证每块**字符数硬上限**：单段无句末标点的长串经由
//! [`crate::docchunk::sentence_spans`] 的自动断句（字数兜底）切开，
//! 因此块大小不会因段落原子性而放大。
use crate::docchunk::sentence_spans;

/// 按句子边界把文本贪心打包成若干块（返回字节偏移 `[start, end)`，
/// 首尾相接、互不重叠、铺满全文；空文本 → 空列表）。
///
/// 不变式：
/// - 总字符数 ≤ `max_chars` 时返回单块 `[0, len]`（调用方可与不分块路径合并）；
/// - 否则每块字符数 ≤ `max_chars`，**唯一例外**是单个句子段本身就超过
///   `max_chars`（此时该块为这一整段，其大小由句表自动断句兜底在 ~200 字
///   量级；对 10s 请求超时仍留 ≥ 25 倍余量）；
/// - 块按文档顺序返回，拼接等于原文。
pub fn split_capped_by_sentences(text: &str, max_chars: usize) -> Vec<[usize; 2]> {
    if text.is_empty() {
        return Vec::new();
    }
    let max = max_chars.max(50);
    if text.chars().count() <= max {
        return vec![[0, text.len()]];
    }
    let mut owned: Vec<[usize; 2]> = Vec::new();
    let mut cur_start = 0usize;
    let mut cur_chars = 0usize;
    for seg in sentence_spans(text) {
        let seg_chars = text[seg[0]..seg[1]].chars().count();
        if cur_chars > 0 && cur_chars + seg_chars > max {
            owned.push([cur_start, seg[0]]);
            cur_start = seg[0];
            cur_chars = 0;
        }
        cur_chars += seg_chars;
    }
    if cur_start < text.len() {
        owned.push([cur_start, text.len()]);
    }
    owned
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars_of(text: &str, span: [usize; 2]) -> usize {
        text[span[0]..span[1]].chars().count()
    }

    fn assert_tiles(text: &str, spans: &[[usize; 2]]) {
        let mut pos = 0usize;
        for s in spans {
            assert_eq!(s[0], pos, "spans must tile contiguously");
            assert!(s[1] > s[0], "empty chunk");
            assert!(text.is_char_boundary(s[1]));
            pos = s[1];
        }
        assert_eq!(pos, text.len(), "spans must cover full text");
        let joined: String = spans.iter().map(|s| &text[s[0]..s[1]]).collect();
        assert_eq!(joined, text);
    }

    #[test]
    fn short_text_single_chunk() {
        let text = "今天天气很好。";
        assert_eq!(split_capped_by_sentences(text, 500), vec![[0, text.len()]]);
        assert!(split_capped_by_sentences("", 500).is_empty());
    }

    #[test]
    fn tiles_and_respects_cap_with_sentences() {
        let sentence = "他昨天在图书馆借了三本书，但是他还忘记带借书证。";
        let text: String = sentence.repeat(40); // ~1000 字
        let spans = split_capped_by_sentences(&text, 500);
        assert!(spans.len() >= 2);
        assert_tiles(&text, &spans);
        for s in &spans {
            assert!(
                chars_of(&text, *s) <= 500,
                "chunk over cap: {}",
                chars_of(&text, *s)
            );
        }
    }

    #[test]
    fn single_long_paragraph_does_not_stay_atomic() {
        // 单段（无换行）960 字：docchunk 会整段原子发出，本模块必须切开
        let base = "老师今天讲的内容我没有听懂，需要课后在复习一遍，明天还要交作业。";
        let text: String = base.repeat(30); // 32 字 × 30 ≈ 960 字单段
        let spans = split_capped_by_sentences(&text, 500);
        assert!(spans.len() >= 2, "900-char paragraph must split");
        assert_tiles(&text, &spans);
        for s in &spans {
            assert!(chars_of(&text, *s) <= 500);
        }
    }

    #[test]
    fn punctuation_free_run_capped_by_auto_break() {
        // 无标点长串：依赖句表 auto_break 的字数兜底，块不至于一发全文
        let text = "字".repeat(5000);
        let spans = split_capped_by_sentences(&text, 500);
        assert!(spans.len() >= 2);
        assert_tiles(&text, &spans);
        for s in &spans {
            assert!(chars_of(&text, *s) <= 500);
        }
    }

    #[test]
    fn newline_and_cjk_ascii_mix_tiles() {
        let text = "第一段结束。\n\nThe quick brown fox jumps over the lazy dog. 第二段开始，这里是中文内容。尾句。\n";
        let spans = split_capped_by_sentences(text, 30);
        assert_tiles(text, &spans);
    }
}
