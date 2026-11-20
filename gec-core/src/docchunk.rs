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
/// 句表句段字数上限：单段超过此值（缺标点/无终结符长串）进入自动断句路径
/// （见 [`auto_break`]；正常有标点文本完全不受此 cap 影响）
pub const SENTENCE_HARD_CAP: usize = 200;
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

/// 句级切分（结构切分，Detect Pipeline v2 §3.1）：按句子边界把全文切成
/// **字节 span** 序列，铺满全文且互不重叠。边界规则：
/// - 中英文句末标点串（。！？；!?; 及英文句号 `.`）结束一句，紧接的引号/括号
///   收尾符一并归入该句；英文句号带守卫：小数（`3.5`）、省略号、文件名/域名段
///   不作边界；
/// - 软换行（含 CRLF）不断句；空行（连续≥一个空行，即 run 内≥2个换行符）是段落
///   边界，仅当该段已有内容时切分；
/// - 纯标点/空白片段保留在铺满序列中（所有权不变式），句级消费方用
///   [`is_sentence`] 过滤；句表统计只计含实词的句。
///
/// 与 [`split_document`] 内部超长段按句切分同一套规则，是全库唯一切句实现
/// （知识召回层亦委派至此；版本向量的 splitter 成分即本实现的版本号）；句级
/// 切分是全局唯一的一次结构切分（句表），供句覆盖统计、③-T 定向复查消费方复用；
/// 其确定性只随 `shared_model::SPLITTER_VERSION` 变。空文本 → 空列表。
///
/// 超过 [`SENTENCE_HARD_CAP`] 字的无标点/无终结符长段进入自动断句（`auto_break`：
/// 分句线索优先、字数兜底）；边界来源见 [`sentence_spans_with_origin`]。
pub fn sentence_spans(text: &str) -> Vec<[usize; 2]> {
    sentence_spans_with_origin(text)
        .into_iter()
        .map(|(s, _)| s)
        .collect()
}

/// 句段右边界来源（块矩阵/分块视图的「切分依据」展示用）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundaryOrigin {
    /// 句末标点串 / 空行段落边界 / 区段收尾（常规标点切句路径）
    Punct,
    /// 自动断句：篇章标记词（然而/但是/因此…）前切
    Marker,
    /// 自动断句：句末语气词 / 冒号 / 换行 / 顿逗后切，或英文句首大写字母前切
    Pause,
    /// 自动断句：窗口内无任何线索，字数硬切兜底
    Fallback,
}

/// [`sentence_spans`] 的带来源标注版本：返回 (字节 span, 右边界来源)。
/// 正常标点句段一律 [`BoundaryOrigin::Punct`]；超 cap 长段内部边界标
/// Marker/Pause/Fallback。铺满/不重叠/字符边界不变式与 [`sentence_spans`] 相同。
pub fn sentence_spans_with_origin(text: &str) -> Vec<([usize; 2], BoundaryOrigin)> {
    punct_segments(text, 0, text.len())
        .into_iter()
        .flat_map(|[s, e]| {
            if text[s..e].chars().count() > SENTENCE_HARD_CAP {
                auto_break(text, s, e, SENTENCE_HARD_CAP)
            } else {
                vec![([s, e], BoundaryOrigin::Punct)]
            }
        })
        .collect()
}

/// 自动断句的分句强线索词（须处从句首位；在其前切分）
const MARKERS: &[&str] = &[
    "然而", "但是", "可是", "因此", "所以", "于是", "而且", "并且", "不过", "此外", "另外", "其次",
    "最后", "总之", "反之", "否则",
];

/// 超长无标点段自动断句：在字数窗口 `(cap/2, cap]` 内取**得分最高**的分句线索点
/// （篇章标记 3 分 > 语气词/冒号/换行 2 分 > 顿逗/英文句首大写 1 分，同分取最靠后），
/// 窗口无候选则在 cap 字处硬切兜底。**确定性纯函数**：同一输入边界恒同，铺满且
/// 互不重叠；只在 [`SENTENCE_HARD_CAP`] 超限处触发，有标点文本零影响。
fn auto_break(
    text: &str,
    start: usize,
    end: usize,
    cap: usize,
) -> Vec<([usize; 2], BoundaryOrigin)> {
    let chars: Vec<(usize, usize, char)> = text[start..end]
        .char_indices()
        .map(|(o, c)| (start + o, start + o + c.len_utf8(), c))
        .collect();
    let total = chars.len();
    // 切点候选（下一片首字符下标 j，分数，来源）；按 j 非降序生成（供 partition_point）
    let mut cands: Vec<(usize, u8, BoundaryOrigin)> = Vec::new();
    for (i, &(bs, _, ch)) in chars.iter().enumerate() {
        if i > 0 {
            let prev = chars[i - 1].2;
            // 篇章标记从句首前切（标记须完整落在本段内）
            if MARKERS.iter().any(|m| text[bs..end].starts_with(m)) {
                cands.push((i, 3, BoundaryOrigin::Marker));
            }
            // 空白后的英文句首大写字母前切
            if ch.is_ascii_uppercase() && prev == ' ' {
                cands.push((i, 1, BoundaryOrigin::Pause));
            }
        }
        // 语气词/冒号/换行/顿逗在其后切
        let after: Option<(u8, BoundaryOrigin)> = match ch {
            '吗' | '呢' | '吧' | '嘛' | '：' | '、' | '，' | '\n' => {
                Some((2, BoundaryOrigin::Pause))
            }
            ',' => Some((1, BoundaryOrigin::Pause)),
            _ => None,
        };
        if let Some((score, origin)) = after {
            if i + 1 < total {
                cands.push((i + 1, score, origin));
            }
        }
    }
    let mut out = Vec::new();
    let mut cur = 0usize;
    while cur < total {
        if total - cur <= cap {
            out.push(([chars[cur].0, end], BoundaryOrigin::Punct));
            break;
        }
        let lo = cur + cap / 2;
        let hi = cur + cap;
        let ci = cands.partition_point(|(j, _, _)| *j <= lo);
        let cj = cands.partition_point(|(j, _, _)| *j <= hi);
        match cands[ci..cj]
            .iter()
            .max_by_key(|(j, score, _)| (*score, *j))
        {
            Some(&(j, _, origin)) => {
                out.push(([chars[cur].0, chars[j].0], origin));
                cur = j;
            }
            None => {
                // 无任何线索：字数兜底硬切（cap ≥ 2 且剩余 > cap ⇒ cur+cap ≤ total）
                out.push(([chars[cur].0, chars[cur + cap].0], BoundaryOrigin::Fallback));
                cur += cap;
            }
        }
    }
    out
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

/// 句末终止符（中英文）：无条件断句
fn is_terminator(ch: char) -> bool {
    matches!(ch, '。' | '！' | '？' | '；' | '!' | '?' | ';')
}

/// 引号/括号类收尾符：紧跟在终止符串后时归入同一句（不得另起空句）
fn is_closer(ch: char) -> bool {
    matches!(
        ch,
        '\u{201c}'
            | '\u{201d}'
            | '\u{2018}'
            | '\u{2019}'
            | '「'
            | '」'
            | '『'
            | '』'
            | '【'
            | '】'
            | '（'
            | '）'
            | '('
            | ')'
            | '［'
            | '］'
            | '['
            | ']'
            | '}'
            | '>'
            | '»'
    )
}

fn is_newline(ch: char) -> bool {
    ch == '\n' || ch == '\r'
}

/// 英文句号 `.` 是否作句边界：前一个字符是字母/数字且不是另一个句号；
/// 后继为空白/收尾符/文尾。小数（`3.5`）、省略号（`x...`）、文件名/域名
/// （后缀紧跟字母/数字）因此都不会被误切；缩写（`Mr. Smith`）会误切，
/// 已知取舍
fn is_period_break(prev: Option<char>, next: Option<char>) -> bool {
    let prev_ok = prev
        .map(|c| c.is_alphanumeric() && c != '.')
        .unwrap_or(false);
    let next_ok = match next {
        None => true,
        Some(c) => !c.is_alphanumeric() && c != '.' && (c.is_whitespace() || is_closer(c)),
    };
    prev_ok && next_ok
}

/// span 内是否含实词（字母/数字，含 CJK）：句表铺满序列会包含纯标点/纯空白
/// 片段，句级消费方用本函数过滤出「真实句子」
pub fn is_sentence(text: &str, span: &[usize; 2]) -> bool {
    text[span[0]..span[1]].chars().any(|c| c.is_alphanumeric())
}

/// 段落内按句边界切分；单句仍超过 `target * LONG_PARA_FACTOR` 时按字数硬切
/// （预算块路径专用兜底；句表路径的自动断句见 [`auto_break`]）
fn split_sentences(text: &str, start: usize, end: usize, target: usize) -> Vec<[usize; 2]> {
    let hard_cap = target * LONG_PARA_FACTOR;
    punct_segments(text, start, end)
        .into_iter()
        .flat_map(|[s, e]| {
            if text[s..e].chars().count() > hard_cap {
                hard_split(text, s, e, hard_cap)
            } else {
                vec![[s, e]]
            }
        })
        .collect()
}

/// 标点边界切分（无字数兜底）：句末标点串（含英文句号守卫）+ 空行分段；
/// 软换行不断句。返回铺满且互不重叠的 span 序列
fn punct_segments(text: &str, start: usize, end: usize) -> Vec<[usize; 2]> {
    let chars: Vec<(usize, usize, char)> = text[start..end]
        .char_indices()
        .map(|(o, c)| (start + o, start + o + c.len_utf8(), c))
        .collect();
    let mut segs: Vec<[usize; 2]> = Vec::new();
    let mut seg_start = start;
    let mut i = 0usize;
    while i < chars.len() {
        let (cs, ce, ch) = chars[i];
        let prev = if i > 0 { Some(chars[i - 1].2) } else { None };
        let next = chars.get(i + 1).map(|c| c.2);
        let period_break = ch == '.' && is_period_break(prev, next);
        if is_terminator(ch) || period_break {
            // 终止符串 + 紧随的引号/括号收尾符归同一句
            let mut j = i + 1;
            let mut e = ce;
            while j < chars.len() && (is_terminator(chars[j].2) || is_closer(chars[j].2)) {
                e = chars[j].1;
                j += 1;
            }
            segs.push([seg_start, e]);
            seg_start = e;
            i = j;
            continue;
        }
        if is_newline(ch) {
            // 软换行不断句；空行（≥2 个逻辑换行）是段落边界，仅当前句已有内容时切；
            // 切点在换行 run 前，空行归入下一句 span（铺满不变式不破）
            let mut j = i + 1;
            let mut nl = 0usize;
            while j < chars.len() && is_newline(chars[j].2) {
                j += 1;
            }
            for (_, _, c) in &chars[i..j] {
                // \r\n 计一个换行，孤立 \r / \n 各计一个
                nl += usize::from(*c == '\n');
            }
            if nl >= 2 && text[seg_start..cs].chars().any(|c| !c.is_whitespace()) {
                segs.push([seg_start, cs]);
                seg_start = cs;
            }
            i = j;
            continue;
        }
        i += 1;
    }
    if seg_start < end {
        segs.push([seg_start, end]);
    }
    segs
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

    #[test]
    fn english_period_splits_sentences() {
        let t = "Hello world. Next one!";
        let sp = sentence_spans(t);
        assert_eq!(sp.len(), 2, "英文句号应作断句边界: {sp:?}");
        assert_eq!(&t[sp[0][0]..sp[0][1]], "Hello world.");
        assert_eq!(unowned_bytes(t.len(), &sp), 0);
    }

    #[test]
    fn decimal_ellipsis_and_filenames_are_not_breaks() {
        // 小数、省略号、文件名/域名都不算句边界
        for t in [
            "The value is 3.5 today.",
            "Wait... and then we left.",
            "See file.txt and x.com for it.",
        ] {
            let sp = sentence_spans(t);
            assert_eq!(sp.len(), 1, "{t} 应只按末尾句点切一刀: {sp:?}");
        }
    }

    #[test]
    fn quote_and_punct_runs_end_one_sentence() {
        let t = "他喊道「你好！」然后走了。";
        let sp = sentence_spans(t);
        assert_eq!(sp.len(), 2, "引号收尾不得另起空句: {sp:?}");
        assert_eq!(&t[sp[0][0]..sp[0][1]], "他喊道「你好！」");
        let t2 = "真的吗？？？我们走。";
        let sp2 = sentence_spans(t2);
        assert_eq!(sp2.len(), 2, "连续标点串只结束一句: {sp2:?}");
        assert_eq!(&t2[sp2[0][0]..sp2[0][1]], "真的吗？？？");
    }

    #[test]
    fn wrapped_line_stays_one_sentence() {
        // 软换行不得把句子拦腰砍断（英文长句换行、CRLF 都算软换行）
        for t in [
            "this line continues\nand ends here。",
            "this line continues\r\nand ends here。",
        ] {
            let sp = sentence_spans(t);
            assert_eq!(sp.len(), 1, "软换行不断句: {sp:?}");
        }
    }

    #[test]
    fn blank_line_breaks_paragraphs() {
        let t = "段落一\n\n段落二。";
        let sp = sentence_spans(t);
        assert_eq!(sp.len(), 2, "空行是段落边界: {sp:?}");
        assert_eq!(&t[sp[0][0]..sp[0][1]], "段落一");
        assert_eq!(&t[sp[1][0]..sp[1][1]], "\n\n段落二。");
        assert_eq!(unowned_bytes(t.len(), &sp), 0);
        // 句尾后的空行不另生空句
        let t2 = "好。\n\n走。";
        let sp2 = sentence_spans(t2);
        assert_eq!(sp2.len(), 2, "空行不得造出空句: {sp2:?}");
        assert_eq!(&t2[sp2[1][0]..sp2[1][1]], "\n\n走。");
    }

    #[test]
    fn sentence_filter_drops_punctuation_and_whitespace_spans() {
        let t = "。。。abc";
        let sp: Vec<[usize; 2]> = sentence_spans(t)
            .into_iter()
            .filter(|s| is_sentence(t, s))
            .collect();
        assert_eq!(sp.len(), 1);
        assert_eq!(&t[sp[0][0]..sp[0][1]], "abc");
        assert!(!is_sentence("好。\n", &[6, 7]));
    }

    // ── 自动断句（punct-v3）──

    /// 不变式：铺满、不重叠、字符边界对齐、每段 ≤ SENTENCE_HARD_CAP 字
    fn assert_spans_tile(t: &str, sp: &[[usize; 2]], capped: bool) {
        assert_eq!(sp[0][0], 0);
        assert_eq!(sp.last().unwrap()[1], t.len());
        for w in sp.windows(2) {
            assert_eq!(w[0][1], w[1][0], "铺满且互不重叠");
        }
        assert_eq!(unowned_bytes(t.len(), sp), 0);
        for s in sp {
            assert!(t.is_char_boundary(s[0]) && t.is_char_boundary(s[1]));
            if capped {
                assert!(t[s[0]..s[1]].chars().count() <= SENTENCE_HARD_CAP, "超 cap");
            }
        }
    }

    #[test]
    fn auto_break_caps_punctuation_free_run() {
        let t = "内容".repeat(225); // 450 字无任何线索
        let sp = sentence_spans(&t);
        assert!(sp.len() >= 3, "450 字应切成 ≥3 段: {}", sp.len());
        assert_spans_tile(&t, &sp, true);
        // 兜底硬切在 cap 处，末段收尾在段尾
        let o = sentence_spans_with_origin(&t);
        assert_eq!(o[0].1, BoundaryOrigin::Fallback);
        assert_eq!(o.last().unwrap().1, BoundaryOrigin::Punct);
    }

    #[test]
    fn auto_break_prefers_discourse_marker() {
        let t = format!("{}然而{}", "一".repeat(150), "二".repeat(100));
        let o = sentence_spans_with_origin(&t);
        assert_eq!(o.len(), 2);
        assert_eq!(o[0].1, BoundaryOrigin::Marker);
        // 切点在「然」前：首段不含然而，次段以然而开头
        assert!(!t[o[0].0[0]..o[0].0[1]].contains("然而"));
        assert!(t[o[1].0[0]..].starts_with("然而"));
        assert_spans_tile(&t, &o.iter().map(|x| x.0).collect::<Vec<_>>(), true);
    }

    #[test]
    fn auto_break_beats_pause_over_comma_and_prefers_higher_score() {
        // 顿逗在窗口内但篇章标记分更高 → 选标记；无标记时选顿逗
        let both = format!(
            "{}，{}然而{}",
            "甲".repeat(110),
            "乙".repeat(50),
            "丙".repeat(40)
        );
        let o = sentence_spans_with_origin(&both);
        assert_eq!(o[0].1, BoundaryOrigin::Marker, "同窗口内高分优先");

        let comma = format!("{}，{}", "甲".repeat(150), "乙".repeat(120));
        let o2 = sentence_spans_with_origin(&comma);
        assert_eq!(o2[0].1, BoundaryOrigin::Pause);
        // 顿逗归属前段
        assert!(comma[o2[0].0[0]..o2[0].0[1]].ends_with('，'));
    }

    #[test]
    fn auto_break_modal_particle_ends_sentence() {
        let t = format!("{}吗 {}", "甲".repeat(140), "乙".repeat(120));
        let o = sentence_spans_with_origin(&t);
        assert_eq!(o[0].1, BoundaryOrigin::Pause);
        assert!(t[o[0].0[0]..o[0].0[1]].ends_with('吗'));
    }

    #[test]
    fn under_cap_text_boundaries_unchanged_and_marked_punct() {
        // cap 以下的有标点文本：边界与标点切句一致，来源全 Punct
        let t = format!("{}。{}", "甲".repeat(120), "乙".repeat(130));
        let o = sentence_spans_with_origin(&t);
        assert_eq!(
            o.len(),
            2,
            "cap 不切正常标点句: {:?}",
            o.iter().map(|x| x.0).collect::<Vec<_>>()
        );
        assert!(o.iter().all(|x| x.1 == BoundaryOrigin::Punct));
        let p: Vec<[usize; 2]> = o.iter().map(|x| x.0).collect();
        assert_spans_tile(&t, &p, false);
    }

    #[test]
    fn auto_break_is_deterministic() {
        let t = format!(
            "{}。{}，{}然而{}{}",
            "甲".repeat(60),
            "乙".repeat(320),
            "丙".repeat(90),
            "丁".repeat(50),
            "。\n\n".repeat(3) + &"戊".repeat(240)
        );
        let a = sentence_spans_with_origin(&t);
        let b = sentence_spans_with_origin(&t);
        assert_eq!(
            a.iter().map(|x| (x.0, x.1)).collect::<Vec<_>>(),
            b.iter().map(|x| (x.0, x.1)).collect::<Vec<_>>()
        );
        let p: Vec<[usize; 2]> = a.iter().map(|x| x.0).collect();
        assert_spans_tile(&t, &p, true);
    }
}
