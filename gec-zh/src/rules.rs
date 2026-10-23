//! 中文 GEC 规则集
//!
//! 基于 HanLP 分词/POS 结果的规则检测（需要 token 信息），
//! 以及不依赖 POS 的本地规则（离线兜底使用）。
//!
//! 词表（同音/形近混淆词、标点映射）外置于
//! `lexicons/zh_gec_lexicon.json`，支持不重新编译即可扩充；
//! 文件缺失或损坏时回退到内置默认词表（见 rules 模块内置的 `Lexicon`）。

use std::path::Path;

use serde::{Deserialize, Serialize};
use shared_model::{EntryCondition, Error, Suggestion, TokenRef, error_types, window_around};

/// HanLP token（只取 GEC 需要的字段）
#[derive(Debug, Clone, Deserialize)]
pub struct Token {
    pub text: String,
    #[serde(default)]
    pub pos: String,
    pub span: [usize; 2],
}

/// 内置默认词表（外置 JSON 缺失或损坏时的兜底）
const DEFAULT_LEXICON_JSON: &str = include_str!("../../lexicons/zh_gec_lexicon.json");

/// 异常模式（词表 `rules[].patterns` 条目；仅 `anomaly` 分类使用）
///
/// - `pattern`：正则（Rust `regex` 语法），对句级文本做**句首锚定**匹配；
/// - `strong`：强信号（命中置信 0.9）；弱信号置信 0.6（需人工确认）；
/// - `note`：模式说明，进入错误 message。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct AnomalyPattern {
    pub pattern: String,
    #[serde(default = "default_true")]
    pub strong: bool,
    #[serde(default)]
    pub note: String,
}

/// 规则元数据（词表 `rules` 数组条目）
#[derive(Debug, Clone, Deserialize)]
pub struct RuleEntry {
    pub id: String,
    pub category: String,
    #[serde(default)]
    pub priority: u32,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub examples: Vec<RuleExample>,
    /// 异常模式（仅 `anomaly` 分类使用；其他分类为空）
    #[serde(default)]
    pub patterns: Vec<AnomalyPattern>,
    /// 排除词（文件侧数据，与 `patterns` 同模式：store 不承载，词表文件为准）：
    /// 用户确认「该词不触发本规则」的词（如被 POS 误标的名词）；
    /// 命中该词的错误在检测器侧直接跳过，无需各检测入口逐一过滤
    #[serde(default)]
    pub excludes: Vec<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
pub struct RuleExample {
    pub wrong: String,
    pub right: String,
}

/// 规则分类定义
#[derive(Debug, Clone, Deserialize)]
pub struct CategoryDef {
    pub id: String,
    pub name: String,
}

/// 带条件的混淆词条目（对象形式，JSON 字段 `wrong`/`right`/`note`/`condition`）
#[derive(Debug, Clone, Deserialize)]
pub struct ConditionalConfusable {
    pub wrong: String,
    pub right: String,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub condition: EntryCondition,
}

/// 同音/形近混淆词条目：
/// - 普通三元组 `[错, 对, 说明]`——无条件，命中即触发（传统格式）；
/// - 对象形式 `{wrong, right, note, condition}`——仅当 [`EntryCondition`]
///   满足时触发（token 条件仅在 POS 路径求值；离线路径无法求值时
///   按保守原则跳过该条目）。
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ConfusableEntry {
    Plain((String, String, String)),
    Conditional(ConditionalConfusable),
}

impl ConfusableEntry {
    pub fn wrong(&self) -> &str {
        match self {
            Self::Plain((w, _, _)) => w,
            Self::Conditional(c) => &c.wrong,
        }
    }

    pub fn right(&self) -> &str {
        match self {
            Self::Plain((_, r, _)) => r,
            Self::Conditional(c) => &c.right,
        }
    }

    pub fn note(&self) -> &str {
        match self {
            Self::Plain((_, _, n)) => n,
            Self::Conditional(c) => &c.note,
        }
    }

    /// 条目的附加条件；普通形式返回 None（无条件，命中即触发）
    pub fn condition(&self) -> Option<&EntryCondition> {
        match self {
            Self::Plain(_) => None,
            Self::Conditional(c) => Some(&c.condition),
        }
    }
}

/// 外置词表（`lexicons/zh_gec_lexicon.json`）
#[derive(Debug, Clone, Deserialize)]
pub struct Lexicon {
    /// 同音/形近混淆词：`[错, 对, 说明]` 或 `{wrong, right, note, condition}`
    #[serde(default)]
    pub confusables: Vec<ConfusableEntry>,
    /// 中英文标点映射：[ASCII 标点, 全角标点]
    #[serde(default)]
    pub punct_map: Vec<(String, String)>,
    /// 规则分类定义
    #[serde(default)]
    pub categories: Vec<CategoryDef>,
    /// 规则元数据（id / category / priority / enabled / examples）
    #[serde(default)]
    pub rules: Vec<RuleEntry>,
}

impl Lexicon {
    /// 从目录加载词表（`{dir}/zh_gec_lexicon.json`）；
    /// 文件缺失或解析失败时回退到内置默认词表
    pub fn load(dir: &Path) -> Self {
        let path = dir.join("zh_gec_lexicon.json");
        match std::fs::read_to_string(&path) {
            Ok(content) => match serde_json::from_str(&content) {
                Ok(l) => l,
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "词表解析失败，使用内置默认词表"
                    );
                    Self::default()
                }
            },
            Err(e) => {
                tracing::debug!(
                    path = %path.display(),
                    error = %e,
                    "词表文件不存在，使用内置默认词表"
                );
                Self::default()
            }
        }
    }
}

impl Default for Lexicon {
    fn default() -> Self {
        serde_json::from_str(DEFAULT_LEXICON_JSON).expect("内置中文词表 JSON 应可解析")
    }
}

/// 需要 POS 信息的完整规则集
pub fn detect_with_tokens(text: &str, tokens: &[Token], lexicon: &Lexicon) -> Vec<Error> {
    let mut out = Vec::new();
    out.extend(detect_homophones(text, tokens, lexicon));
    out.extend(detect_de_da_de(text, tokens));
    out.extend(detect_punctuation(text, tokens, lexicon));
    out.extend(detect_missing_subject(text, tokens, lexicon));
    out.extend(detect_anomalies(text, lexicon));
    out
}

/// HanLP `/analyze` 返回的 token span 为**字符偏移**（Python `len` 语义），
/// 而 [`shared_model::Error::span`] 契约为**字节偏移**——CJK 文本两者相差约 3 倍。
/// 检测器在规则执行前统一换算（越界钳制见 [`gec_core::char_span_to_bytes`]）。
pub fn tokens_to_byte_spans(text: &str, tokens: &[Token]) -> Vec<Token> {
    tokens
        .iter()
        .map(|t| Token {
            text: t.text.clone(),
            pos: t.pos.clone(),
            span: gec_core::char_span_to_bytes(text, t.span),
        })
        .collect()
}

/// 不依赖 POS 的本地规则（离线兜底）
pub fn detect_local(text: &str, lexicon: &Lexicon) -> Vec<Error> {
    let mut out = Vec::new();
    out.extend(detect_homophones_by_scan(text, lexicon));
    out.extend(detect_punctuation_by_scan(text, lexicon));
    out.extend(detect_anomalies(text, lexicon));
    out
}

/// 同音/形近错别字（基于 token 精确匹配）
fn detect_homophones(text: &str, tokens: &[Token], lexicon: &Lexicon) -> Vec<Error> {
    let _ = text;
    let mut out = Vec::new();
    for (i, token) in tokens.iter().enumerate() {
        if let Some(entry) = lexicon.confusables.iter().find(|e| e.wrong() == token.text) {
            // 附加条件：token 条件（邻接 POS/文本）+ 文本条件（周边窗口正则）
            if let Some(cond) = entry.condition() {
                let prev = tokens
                    .get(i.saturating_sub(1))
                    .map(|t| TokenRef::new(&t.text, &t.pos));
                let next = tokens.get(i + 1).map(|t| TokenRef::new(&t.text, &t.pos));
                let window = window_around(text, token.span[0], token.span[1], 4);
                if !cond.check_tokens(prev.as_ref(), next.as_ref()) || !cond.check_context(&window)
                {
                    continue;
                }
            }
            let correction = entry.right().to_string();
            let note = entry.note().to_string();
            out.push(Error::new_with_meta(
                "zh",
                error_types::SPELLING,
                0.6,
                token.span,
                &token.text,
                Some(correction.clone()),
                format!("疑似同音/形近错别字：{note}"),
                Some("spelling"),
                Some("ZH_CONFUSABLE"),
                vec![Suggestion::new(correction, 0.6)],
            ));
        }
    }
    out
}

/// 同音/形近错别字（离线：子串扫描）。
/// 带 token 条件的条目在此无法求值 → 按保守原则跳过（不触发）。
fn detect_homophones_by_scan(text: &str, lexicon: &Lexicon) -> Vec<Error> {
    let mut out = Vec::new();
    for entry in &lexicon.confusables {
        if let Some(cond) = entry.condition()
            && cond.needs_tokens()
        {
            continue;
        }
        for (idx, _) in text.match_indices(entry.wrong()) {
            // span 为字节偏移（与 shared_model::Error 契约一致）
            let end = idx + entry.wrong().len();
            if let Some(cond) = entry.condition() {
                let window = window_around(text, idx, end, 4);
                if !cond.check_context(&window) {
                    continue;
                }
            }
            let correction = entry.right().to_string();
            let note = entry.note().to_string();
            out.push(Error::new_with_meta(
                "zh",
                error_types::SPELLING,
                0.5,
                [idx, end],
                entry.wrong(),
                Some(correction.clone()),
                format!("疑似同音/形近错别字：{note}"),
                Some("spelling"),
                Some("ZH_CONFUSABLE"),
                vec![Suggestion::new(correction, 0.5)],
            ));
        }
    }
    out
}

/// 「的 / 地 / 得」误用（基于 POS）
fn detect_de_da_de(text: &str, tokens: &[Token]) -> Vec<Error> {
    let _ = text;
    let mut out = Vec::new();
    for (i, token) in tokens.iter().enumerate() {
        if token.text != "的" || token.pos != "DEG" {
            continue;
        }
        let next = tokens.get(i + 1);
        let prev = tokens.get(i.saturating_sub(1));
        // 的 + 副词 → 应为「地」（状语标记）
        if let Some(next) = next
            && next.pos == "AD"
        {
            out.push(Error::new_with_meta(
                "zh",
                error_types::GRAMMAR,
                0.7,
                token.span,
                "的",
                Some("地".to_string()),
                "副词前作状语标记应使用「地」（如：高兴\" 地\" 笑）",
                Some("grammar"),
                Some("ZH_DE_DA_DE"),
                vec![Suggestion::new("地", 0.7)],
            ));
        }
        // 动词 + 的 + 形容词 → 应为「得」（补语标记）
        if let (Some(prev), Some(next)) = (prev, next)
            && prev.pos == "VV"
            && next.pos == "VA"
        {
            out.push(Error::new_with_meta(
                "zh",
                error_types::GRAMMAR,
                0.7,
                token.span,
                "的",
                Some("得".to_string()),
                "动词后接结果补语应使用「得」（如：跑\" 得\" 快）",
                Some("grammar"),
                Some("ZH_DE_DA_DE"),
                vec![Suggestion::new("得", 0.7)],
            ));
        }
    }
    out
}

/// 标点误用（基于 token）
fn detect_punctuation(text: &str, tokens: &[Token], lexicon: &Lexicon) -> Vec<Error> {
    let _ = text;
    let mut out = Vec::new();
    for (i, token) in tokens.iter().enumerate() {
        if token.pos != "PU" {
            continue;
        }
        if let Some((_ascii, full)) = lexicon.punct_map.iter().find(|(a, _)| *a == token.text) {
            out.push(Error::new_with_meta(
                "zh",
                error_types::PUNCTUATION,
                0.85,
                token.span,
                &token.text,
                Some(full.clone()),
                "中文语境应使用全角标点",
                Some("typography"),
                Some("ZH_PUNCTUATION"),
                vec![Suggestion::new(full.clone(), 0.85)],
            ));
        }
        if let Some(next) = tokens.get(i + 1)
            && next.pos == "PU"
        {
            out.push(Error::new_with_meta(
                "zh",
                error_types::PUNCTUATION,
                0.8,
                [token.span[0], next.span[1]],
                format!("{}{}", token.text, next.text),
                Some(next.text.clone()),
                "连续重复标点，建议删除其一",
                Some("typography"),
                Some("ZH_PUNCTUATION"),
                vec![Suggestion::new(next.text.clone(), 0.8)],
            ));
        }
    }
    if let Some(last) = tokens.last()
        && last.pos != "PU"
    {
        out.push(Error::new_with_meta(
            "zh",
            error_types::PUNCTUATION,
            0.4,
            last.span,
            &last.text,
            Some("。".to_string()),
            "句末疑似缺少句号",
            Some("typography"),
            Some("ZH_PUNCTUATION"),
            vec![Suggestion::new("。", 0.4)],
        ));
    }
    out
}

/// 标点误用（离线：字符扫描）
fn detect_punctuation_by_scan(text: &str, lexicon: &Lexicon) -> Vec<Error> {
    let mut out = Vec::new();
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    for (i, (idx, c)) in chars.iter().enumerate() {
        let mut full: char = '\0';
        let c_str = c.to_string();
        for (ascii, fullwidth) in &lexicon.punct_map {
            if ascii == &c_str {
                full = fullwidth.chars().next().unwrap_or('\0');
                break;
            }
        }
        if full == '\0' {
            continue;
        }
        let prev_cjk = chars
            .get(i.wrapping_sub(1))
            .is_some_and(|(_, p)| is_cjk(*p));
        let next_cjk = chars.get(i + 1).is_some_and(|(_, n)| is_cjk(*n));
        if prev_cjk || next_cjk {
            out.push(Error::new_with_meta(
                "zh",
                error_types::PUNCTUATION,
                0.85,
                [*idx, *idx + 1],
                c.to_string(),
                Some(full.to_string()),
                "中文语境应使用全角标点",
                Some("typography"),
                Some("ZH_PUNCTUATION"),
                vec![Suggestion::new(full.to_string(), 0.85)],
            ));
        }
    }
    out
}

fn is_cjk(c: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&c)
}

/// 基础句法：句子以动词开头 → 疑似成分残缺（缺主语）
fn detect_missing_subject(text: &str, tokens: &[Token], lexicon: &Lexicon) -> Vec<Error> {
    let _ = text;
    let mut out = Vec::new();
    // 规则级排除词（词表 rules[].excludes）：用户确认「该词不触发本规则」
    // （典型：POS 把名词误标为动词，如「靠山」）——检测器侧直接跳过，
    // 规则保持启用，仅该词不再触发
    let excludes: &[String] = lexicon
        .rules
        .iter()
        .find(|r| r.id == "ZH_MISSING_SUBJECT")
        .map(|r| r.excludes.as_slice())
        .unwrap_or(&[]);
    let mut sentence_starts = vec![0usize];
    for (i, t) in tokens.iter().enumerate() {
        if t.pos == "PU"
            && matches!(t.text.as_str(), "。" | "！" | "？" | "．")
            && i + 1 < tokens.len()
        {
            sentence_starts.push(i + 1);
        }
    }
    for start in sentence_starts {
        if tokens[start].pos == "VV" && !excludes.iter().any(|w| w == &tokens[start].text) {
            out.push(Error::new_with_meta(
                "zh",
                error_types::GRAMMAR,
                0.5,
                tokens[start].span,
                &tokens[start].text,
                None,
                "句子以动词开头，疑似成分残缺（缺主语）",
                Some("grammar"),
                Some("ZH_MISSING_SUBJECT"),
                vec![],
            ));
        }
    }
    out
}

/// 异常检测：AI 助手回复 / 元讨论混入正文（不依赖 POS，token / 离线双路径共用）
///
/// - 按句切分（`。！？!?\n` 为界），模式对句级文本做句首锚定匹配；
/// - 守卫：上一句界到本句首的窗口含奇数个引号字符 → 视为对话，跳过
///   （窗口局部化而非全文引号状态，防止未闭合引号把后续叙述句整体吞进"对话"）；
/// - 强信号命中置信 0.9，弱信号 0.6；同一句多模式命中取最高置信（去重）；
/// - `correction` 恒为 None：删除属内容决策，只标记不静默删。
pub fn detect_anomalies(text: &str, lexicon: &Lexicon) -> Vec<Error> {
    // 编译 anomaly 规则的模式（编译失败的模式跳过并告警）
    let mut compiled: Vec<(String, regex::Regex, bool, String)> = Vec::new();
    for rule in &lexicon.rules {
        if rule.category != "anomaly" {
            continue;
        }
        for p in &rule.patterns {
            match regex::Regex::new(&p.pattern) {
                Ok(re) => compiled.push((rule.id.clone(), re, p.strong, p.note.clone())),
                Err(e) => {
                    tracing::warn!(
                        rule = %rule.id,
                        pattern = %p.pattern,
                        error = %e,
                        "异常模式编译失败，跳过"
                    );
                }
            }
        }
    }
    if compiled.is_empty() {
        return Vec::new();
    }

    let sentences = split_sentences(text);
    let mut out = Vec::new();
    for (i, (start, end)) in sentences.iter().enumerate() {
        if sentence_in_quote(text, &sentences, i) {
            continue; // 对话内不触发（守卫）
        }
        let sent = &text[*start..*end];
        // 各模式命中取最高置信（强 0.9 > 弱 0.6）
        let mut best: Option<(f64, &str, &str)> = None;
        for (rule_id, re, strong, note) in &compiled {
            if re.is_match(sent) {
                let conf = if *strong { 0.9 } else { 0.6 };
                if best.as_ref().map_or(true, |(b, _, _)| conf > *b) {
                    best = Some((conf, note, rule_id));
                }
            }
        }
        if let Some((conf, note, rule_id)) = best {
            out.push(Error::new_with_meta(
                "zh",
                error_types::ANOMALY,
                conf,
                [*start, *end],
                sent,
                None,
                format!("疑似 AI 生成文本污染：{note}；不属于故事内容，建议删除"),
                Some("anomaly"),
                Some(rule_id),
                Vec::new(),
            ));
        }
    }
    out
}

/// 按句切分：`。！？!?\n` 为界；句 span 含终止标点，下一句首部空白不计入
fn split_sentences(text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for (i, c) in text.char_indices() {
        if matches!(c, '。' | '！' | '？' | '!' | '?' | '\n') {
            let end = i + c.len_utf8();
            if end > start {
                out.push((start, end));
            }
            start = end;
            while start < text.len() {
                let Some(n) = text[start..].chars().next() else {
                    break;
                };
                if matches!(n, '\n' | '\r' | ' ') {
                    start += n.len_utf8();
                } else {
                    break;
                }
            }
        }
    }
    if start < text.len() {
        out.push((start, text.len()));
    }
    out
}

/// 句子是否处于引号内（对话）：上一句界到本句首的窗口中引号字符数为奇数
fn sentence_in_quote(text: &str, sentences: &[(usize, usize)], i: usize) -> bool {
    let win_start = if i == 0 { 0 } else { sentences[i - 1].1 };
    let win_end = sentences[i].0;
    if win_start >= win_end {
        return false;
    }
    let n = text[win_start..win_end]
        .chars()
        .filter(|c| matches!(c, '"' | '「' | '『' | '」' | '』'))
        .count();
    n % 2 == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(text: &str, pos: &str, span: [usize; 2]) -> Token {
        Token {
            text: text.into(),
            pos: pos.into(),
            span,
        }
    }

    #[test]
    fn tokens_to_byte_spans_converts_cjk_char_offsets() {
        // HanLP 返回字符偏移（模拟 /analyze 真实响应）→ 换算为字节偏移；
        // 「，，」字符 [6,8) → 字节 [18,24)（CJK 每字 3 字节）
        let text = "今天天气很好，，我们去公园玩吧。";
        let char_tokens = vec![
            token("今天", "NT", [0, 2]),
            token("天气", "NN", [2, 4]),
            token("很", "AD", [4, 5]),
            token("好", "VA", [5, 6]),
            token("，", "PU", [6, 7]),
            token("，", "PU", [7, 8]),
        ];
        let byte_tokens = tokens_to_byte_spans(text, &char_tokens);
        assert_eq!(byte_tokens[4].span, [18, 21]);
        assert_eq!(byte_tokens[5].span, [21, 24]);
        // 换算后规则产出的 span 必须落在字符边界（apply_corrections 不 panic）
        let errors = detect_with_tokens(text, &byte_tokens, &Lexicon::default());
        let dup = errors
            .iter()
            .find(|e| e.rule_id.as_deref() == Some("ZH_PUNCTUATION"))
            .expect("连续重复标点应命中");
        assert_eq!(dup.span, [18, 24]);
        assert!(text.is_char_boundary(dup.span[0]) && text.is_char_boundary(dup.span[1]));
        // 端到端：应用修正不 panic 且结果正确
        let corrected = shared_model::apply_corrections(text, std::slice::from_ref(dup));
        assert_eq!(corrected, "今天天气很好，我们去公园玩吧。");
    }

    #[test]
    fn homophone_detection() {
        let tokens = vec![
            token("他", "PN", [0, 3]),
            token("時", "VV", [3, 6]),
            token("常", "AD", [6, 9]),
            token("迟到", "VV", [9, 15]),
        ];
        let errors = detect_with_tokens("他時常迟到", &tokens, &Lexicon::default());
        assert!(errors.iter().any(|e| e.error_type == error_types::SPELLING));
        assert!(errors.iter().any(|e| e.correction.as_deref() == Some("时")));
    }

    #[test]
    fn de_da_de_detection() {
        let tokens = vec![
            token("他", "PN", [0, 1]),
            token("的", "DEG", [1, 2]),
            token("高兴", "AD", [2, 4]),
            token("笑", "VV", [4, 5]),
        ];
        let errors = detect_with_tokens("他的高兴笑", &tokens, &Lexicon::default());
        assert!(errors.iter().any(|e| e.correction.as_deref() == Some("地")));
    }

    #[test]
    fn punctuation_detection() {
        let tokens = vec![
            token("今天", "NT", [0, 2]),
            token(",", "PU", [2, 3]),
            token("天气", "NN", [3, 5]),
            token("很好", "VA", [5, 7]),
            token(".", "PU", [7, 8]),
        ];
        let errors = detect_with_tokens("今天,天气很好.", &tokens, &Lexicon::default());
        assert!(
            errors
                .iter()
                .all(|e| e.error_type == error_types::PUNCTUATION)
        );
        assert_eq!(errors.len(), 2);
    }

    #[test]
    fn missing_subject_detection() {
        let tokens = vec![token("学习", "VV", [0, 2]), token("。", "PU", [2, 3])];
        let errors = detect_with_tokens("学习。", &tokens, &Lexicon::default());
        assert!(errors.iter().any(|e| e.error_type == error_types::GRAMMAR));
    }

    #[test]
    fn missing_subject_respects_rule_excludes() {
        let tokens = vec![token("靠山", "VV", [0, 2]), token("。", "PU", [2, 3])];
        // 无排除词：正常报告
        let errors = detect_with_tokens("靠山。", &tokens, &Lexicon::default());
        assert!(
            errors
                .iter()
                .any(|e| e.rule_id.as_deref() == Some("ZH_MISSING_SUBJECT")),
            "无排除词时应报告: {:?}",
            errors
        );
        // 排除词命中（POS 误标的名词）：就地修改既有规则条目（与 append_rule_exclude 行为一致）
        let mut lex = Lexicon::default();
        let rule = lex
            .rules
            .iter_mut()
            .find(|r| r.id == "ZH_MISSING_SUBJECT")
            .expect("内置词表应含 ZH_MISSING_SUBJECT");
        rule.excludes = vec!["靠山".into()];
        let got = detect_with_tokens("靠山。", &tokens, &lex);
        assert!(
            got.iter()
                .all(|e| e.rule_id.as_deref() != Some("ZH_MISSING_SUBJECT")),
            "排除词命中时不应报告: {got:?}"
        );
        // 其他词不受影响
        let tokens2 = vec![token("学习", "VV", [0, 2]), token("。", "PU", [2, 3])];
        let got2 = detect_with_tokens("学习。", &tokens2, &lex);
        assert!(
            got2.iter()
                .any(|e| e.rule_id.as_deref() == Some("ZH_MISSING_SUBJECT")),
            "排除名单外的词仍应报告: {got2:?}"
        );
    }

    #[test]
    fn local_detection_without_pos() {
        let errors = detect_local("他昨天去公园,他時常迟到.", &Lexicon::default());
        assert!(errors.iter().any(|e| e.error_type == error_types::SPELLING));
        assert!(
            errors
                .iter()
                .any(|e| e.error_type == error_types::PUNCTUATION)
        );
    }

    #[test]
    fn clean_text_local_no_errors() {
        // 正确中文句子（不含词表混淆词、全角标点）不应产生任何错误（误报回归）
        for s in ["我喜欢蓝色的天空。", "今天天气很好。", "他昨天去了公园。"]
        {
            let errors = detect_local(s, &Lexicon::default());
            assert!(errors.is_empty(), "误报 [{s}]: {errors:?}");
        }
    }

    fn lexicon_from_json(json: &str) -> Lexicon {
        serde_json::from_str(json).expect("词表 JSON 应可解析")
    }

    #[test]
    fn conditional_entry_plain_and_conditional_mix() {
        // 数组形式（无条件）与对象形式（带条件）可混用
        let lex = lexicon_from_json(
            r#"{
              "confusables": [
                ["岐", "吾", "繁体字"],
                {
                  "wrong": "象",
                  "right": "像",
                  "note": "「象」误用于「像」",
                  "condition": { "prev_text": ["好"], "context_regex": "好象" }
                }
              ]
            }"#,
        );
        assert_eq!(lex.confusables.len(), 2);
        assert_eq!(lex.confusables[0].wrong(), "岐");
        assert!(lex.confusables[0].condition().is_none());
        assert_eq!(lex.confusables[1].wrong(), "象");
        assert!(lex.confusables[1].condition().is_some());
    }

    #[test]
    fn conditional_entry_token_path() {
        let lex = lexicon_from_json(
            r#"{
              "confusables": [
                {
                  "wrong": "象",
                  "right": "像",
                  "note": "「象」误用于「像」",
                  "condition": { "prev_text": ["好"], "context_regex": "好象" }
                }
              ]
            }"#,
        );
        // 条件满足：「好」+ 象 → 触发
        let tokens = vec![
            token("他", "PN", [0, 3]),
            token("好", "AD", [3, 6]),
            token("象", "VV", [6, 9]),
            token("很高", "AD", [9, 15]),
        ];
        let errors = detect_with_tokens("他好象很高", &tokens, &lex);
        assert!(
            errors
                .iter()
                .any(|e| e.original == "象" && e.correction.as_deref() == Some("像"))
        );
        // 条件不满足：「很」+ 象 → 不触发
        let tokens = vec![
            token("他", "PN", [0, 3]),
            token("很", "AD", [3, 6]),
            token("象", "VV", [6, 9]),
        ];
        let errors = detect_with_tokens("他象很高", &tokens, &lex);
        assert!(
            !errors
                .iter()
                .any(|e| e.error_type == error_types::SPELLING && e.original == "象")
        );
    }

    #[test]
    fn conditional_entry_offline_path() {
        // 纯 context_regex 条目（无 token 条件）→ 离线路径可求值
        let lex = lexicon_from_json(
            r#"{
              "confusables": [
                {
                  "wrong": "象",
                  "right": "像",
                  "note": "「象」误用于「像」",
                  "condition": { "context_regex": "好象" }
                }
              ]
            }"#,
        );
        // 窗口匹配 context_regex → 触发
        let errors = detect_local("他好象很高兴", &lex);
        assert!(
            errors
                .iter()
                .any(|e| e.original == "象" && e.correction.as_deref() == Some("像"))
        );
        // 窗口不匹配 context_regex → 不触发
        let errors = detect_local("他象很高兴", &lex);
        assert!(!errors.iter().any(|e| e.original == "象"));
    }

    #[test]
    fn conditional_entry_pos_only_skipped_offline() {
        // 仅含 token 条件（无 context_regex）→ 离线路径整体跳过
        let lex = lexicon_from_json(
            r#"{
              "confusables": [
                {
                  "wrong": "既",
                  "right": "即",
                  "note": "「既」误用于「即」",
                  "condition": { "next_text": ["使", "便", "将", "可"] }
                }
              ]
            }"#,
        );
        assert!(detect_local("他既使知道", &lex).is_empty());
        // POS 路径：next_text 满足 → 触发
        let tokens = vec![
            token("他", "PN", [0, 3]),
            token("既", "CN", [3, 6]),
            token("使", "VV", [6, 9]),
        ];
        let errors = detect_with_tokens("他既使知道", &tokens, &lex);
        assert!(
            errors
                .iter()
                .any(|e| e.original == "既" && e.correction.as_deref() == Some("即"))
        );
        // next_text 不满足 → 不触发
        let tokens = vec![
            token("他", "PN", [0, 3]),
            token("既", "CN", [3, 6]),
            token("然", "CN", [6, 9]),
        ];
        let errors = detect_with_tokens("他既然知道", &tokens, &lex);
        assert!(!errors.iter().any(|e| e.original == "既"));
    }

    // ── 异常检测（AI 生成文本污染）──────────────────────────────────────

    /// 内置词表应含异常种子规则（离线兜底路径同样生效）
    #[test]
    fn default_lexicon_has_anomaly_seed_rule() {
        let lex = Lexicon::default();
        let rule = lex
            .rules
            .iter()
            .find(|r| r.id == "ZH_ANOMALY_ASSISTANT_REPLY")
            .expect("种子异常规则应存在于内置词表");
        assert_eq!(rule.category, "anomaly");
        assert!(!rule.patterns.is_empty());
        // 离线路径应能检出污染句
        let errors = detect_local("需要我再微调部分语句吗?", &lex);
        assert!(
            errors.iter().any(|e| e.error_type == error_types::ANOMALY),
            "{errors:?}"
        );
    }

    /// 用户测试文本回归：助手回复污染句（裸句）应被检出；引号内对话不触发
    #[test]
    fn anomaly_detects_assistant_reply_in_narrative() {
        let lex = Lexicon::default();
        let text = "\"靠山村很奇怪，家家户户门口挂着一块牌子，牌子上写着户主的姓名。\n\
需要我再微调部分语句，让整体文风更贴合悬疑灵异的氛围吗?\n\
江轻猜测，这是任务中一个重要的设定，给\"演员\"的一种提示。没走多远，江轻注意到田野间站着一个人:\"吴老二!\"阳光下，吴老二抽着旱烟，愁眉苦脸，不知在想些什么。陈天岳一把抓住江轻的手腕，霸道地将人拉到身后，严肃道:\"别乱跑，你待在我身后，万一他真是诡，突然袭击，我好保护你。\n\
\"我……好吧。\"江轻放弃挣扎,\"你松手，弄疼我了。\n\
老陈这力气是真大!\n\
两个人缓慢靠近，佝偻着腰背的吴老二猛地转身，脸色变了又变。\n\
他抖了抖烟丝，露出一口大黄牙，祥和地笑问:\"小伙子，有事吗?\"";
        let errors = detect_anomalies(text, &lex);
        assert_eq!(errors.len(), 1, "只应检出污染句: {errors:?}");
        let e = &errors[0];
        assert_eq!(e.error_type, error_types::ANOMALY);
        assert_eq!(
            e.original,
            "需要我再微调部分语句，让整体文风更贴合悬疑灵异的氛围吗?"
        );
        assert!((e.confidence - 0.9).abs() < 1e-9, "强信号应为 0.9: {e:?}");
        assert!(
            e.correction.is_none(),
            "删除属内容决策，correction 应为 None"
        );
        assert_eq!(e.rule_id.as_deref(), Some("ZH_ANOMALY_ASSISTANT_REPLY"));
        assert_eq!(&text[e.span[0]..e.span[1]], e.original);
    }

    /// 「写手角色」负例：角色合法地讨论写作（引号内）不触发；文本以对话开头也不触发
    #[test]
    fn anomaly_ignores_writer_character_dialogue() {
        let lex = Lexicon::default();
        // 对话在句中（引号包裹）
        let text = "老作家叹了口气:\"需要我再把这段的语句微调一下，让文风更贴合悬疑灵异的氛围吗?\"年轻人摇了摇头。";
        assert!(detect_anomalies(text, &lex).is_empty());
        // 文本以对话开头（引号在句首，锚定模式不匹配）
        let text2 = "\"需要我再微调部分语句吗?\"老作家问。";
        assert!(detect_anomalies(text2, &lex).is_empty());
    }

    /// 其他强信号模式：元说明开头 / 收尾话术
    #[test]
    fn anomaly_detects_meta_preamble_and_closing() {
        let lex = Lexicon::default();
        let e = detect_anomalies("以下是修改后的版本：\n靠山村的夜色很静。", &lex);
        assert!(
            e.iter()
                .any(|x| x.original.starts_with("以下是修改后的版本")),
            "{e:?}"
        );
        let e = detect_anomalies("故事到这里就结束了。\n如需进一步修改，请告诉我。", &lex);
        assert!(
            e.iter().any(|x| x.original.starts_with("如需进一步修改")),
            "{e:?}"
        );
    }

    /// 弱信号：服务性问句 + 元语言词汇（置信 0.6，需人工确认）
    #[test]
    fn anomaly_weak_signal_low_confidence() {
        let lex = Lexicon::default();
        let e = detect_anomalies("要不要把这段的语句再调整一下?", &lex);
        assert_eq!(e.len(), 1, "{e:?}");
        assert!((e[0].confidence - 0.6).abs() < 1e-9, "{e:?}");
    }

    /// 多模式命中同一句 → 只报一条，取最高置信（强 0.9 > 弱 0.6）
    #[test]
    fn anomaly_dedup_keeps_strongest() {
        let lex = Lexicon::default();
        let e = detect_anomalies(
            "需要我再微调部分语句，让整体文风更贴合悬疑灵异的氛围吗?",
            &lex,
        );
        assert_eq!(e.len(), 1, "{e:?}");
        assert!((e[0].confidence - 0.9).abs() < 1e-9, "{e:?}");
    }

    /// 句切分：span 含终止标点，句首空白（换行）不计入
    #[test]
    fn split_sentences_basics() {
        let text = "你好。世界!\n再见?";
        let spans = split_sentences(text);
        assert_eq!(spans, vec![(0, 9), (9, 16), (17, 24)]);
        assert_eq!(&text[0..9], "你好。");
        assert_eq!(&text[9..16], "世界!");
        assert_eq!(&text[17..24], "再见?");
    }
}
