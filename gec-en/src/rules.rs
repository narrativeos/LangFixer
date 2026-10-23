//! 英文 GEC 规则集
//!
//! 基于 HanLP 分词/POS 结果的规则检测（需要 token 信息），
//! 以及不依赖 POS 的本地规则（离线兜底使用）。
//!
//! 词表（过去式表 / 常见动词 / 时间指示词）外置于
//! `lexicons/en_gec_lexicon.json`，支持不重新编译即可扩充；
//! 文件缺失或损坏时回退到内置默认词表（见 rules 模块内置的 `Lexicon`）。

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, RwLock};

use once_cell::sync::Lazy;
use regex::Regex;
use serde::Deserialize;
use shared_model::{EntryCondition, Error, Suggestion, error_types, window_around};

/// HanLP token（只取 GEC 需要的字段）
#[derive(Debug, Clone, Deserialize)]
pub struct Token {
    pub text: String,
    #[serde(default)]
    pub pos: String,
    pub span: [usize; 2],
}

/// 内置默认词表（外置 JSON 缺失或损坏时的兜底）
const DEFAULT_LEXICON_JSON: &str = include_str!("../../lexicons/en_gec_lexicon.json");

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

/// 带条件的拼写条目（对象形式，JSON 字段 `wrong`/`right`/`condition`）
#[derive(Debug, Clone, Deserialize)]
pub struct ConditionalMisspelling {
    pub wrong: String,
    pub right: String,
    #[serde(default)]
    pub condition: EntryCondition,
}

/// 拼写错误条目：
/// - 普通二元组 `[错, 对]`——无条件，命中即触发（传统格式）；
/// - 对象形式 `{wrong, right, condition}`——仅当 [`EntryCondition`]
///   满足时触发。本规则仅有本地路径（无 POS），token 条件无法求值 →
///   按保守原则跳过该条目；`context_regex` 可正常求值。
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum MisspellingEntry {
    Plain((String, String)),
    Conditional(ConditionalMisspelling),
}

impl MisspellingEntry {
    pub fn wrong(&self) -> &str {
        match self {
            Self::Plain((w, _)) => w,
            Self::Conditional(c) => &c.wrong,
        }
    }

    pub fn right(&self) -> &str {
        match self {
            Self::Plain((_, r)) => r,
            Self::Conditional(c) => &c.right,
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

/// 外置词表（`lexicons/en_gec_lexicon.json`）
#[derive(Debug, Clone, Deserialize)]
pub struct Lexicon {
    /// 常见动词及其过去式（时态规则用）
    #[serde(default)]
    pub past_tenses: Vec<(String, String)>,
    /// 常见动词（主谓一致规则用）
    #[serde(default)]
    pub common_verbs: Vec<String>,
    /// 过去时间指示词（时态规则用）
    #[serde(default)]
    pub past_time_markers: Vec<String>,
    /// 形容词比较级/最高级误用（错→对），如 ("more better","better")
    #[serde(default)]
    pub adjective_comparatives: Vec<(String, String)>,
    /// 形副同形（形容词→副词），如 ("quiet","quietly")
    #[serde(default)]
    pub adverb_pairs: Vec<(String, String)>,
    /// 不可数名词误用（错→对），如 ("informations","information")
    #[serde(default)]
    pub uncountable_nouns: Vec<(String, String)>,
    /// 反身代词（主语、同指宾语、反身形式），如 ("He","him","himself")
    #[serde(default)]
    pub reflexive_pronouns: Vec<(String, String, String)>,
    /// 动词+介词 误用（错→对），如 ("discuss about","discuss")
    #[serde(default)]
    pub verb_preposition_confusables: Vec<(String, String)>,
    /// 后接动名词（-ing）的动词
    #[serde(default)]
    pub gerund_verbs: Vec<String>,
    /// 后接不定式（to do）的动词
    #[serde(default)]
    pub infinitive_verbs: Vec<String>,
    /// 虚拟语气标记词（wish/if）
    #[serde(default)]
    pub subjunctive_markers: Vec<String>,
    /// 否定算子（don't/can't…），双重否定检测用
    #[serde(default)]
    pub negation_operators: Vec<String>,
    /// 否定词（错→对），如 ("nothing","anything")
    #[serde(default)]
    pub negation_markers: Vec<(String, String)>,
    /// 拼写错误（错→对），如 (\"exemple\",\"example\")；
    /// 亦可为对象形式 `{wrong, right, condition}`（带附加条件）
    #[serde(default)]
    pub misspellings: Vec<MisspellingEntry>,
    /// 规则分类定义
    #[serde(default)]
    pub categories: Vec<CategoryDef>,
    /// 规则元数据（id / category / priority / enabled / examples）
    #[serde(default)]
    pub rules: Vec<RuleEntry>,
}

impl Lexicon {
    /// 从目录加载词表（`{dir}/en_gec_lexicon.json`）；
    /// 文件缺失或解析失败时回退到内置默认词表
    pub fn load(dir: &Path) -> Self {
        let path = dir.join("en_gec_lexicon.json");
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
        serde_json::from_str(DEFAULT_LEXICON_JSON).expect("内置英文词表 JSON 应可解析")
    }
}

static RE_A_BEFORE_VOWEL: Lazy<Regex> = Lazy::new(|| Regex::new(r"\ba\s+([aeiou])\w+").unwrap());
static RE_AN_BEFORE_CONSONANT: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\ban\s+([bcdfghjklmnpqrstvwxz])\w+").unwrap());

/// 正则编译缓存：`detect_local` 每次调用都要为词表各条目编译正则，
/// 按 pattern 字符串缓存避免重复编译（`Regex::clone` 开销极低）
static RE_CACHE: Lazy<Arc<RwLock<HashMap<String, Regex>>>> =
    Lazy::new(|| Arc::new(RwLock::new(HashMap::new())));

fn cached_regex(pattern: &str) -> Regex {
    let mut map = RE_CACHE.write().unwrap_or_else(|e| e.into_inner());
    if let Some(re) = map.get(pattern) {
        return re.clone();
    }
    let re = Regex::new(pattern).unwrap();
    map.insert(pattern.to_string(), re.clone());
    re
}

// ─────────────────────────────────────────────────────────────────────────────
// POS-based rules (main path: HanLP tokens available)
// ─────────────────────────────────────────────────────────────────────────────

/// 需要词性信息的完整规则集
pub fn detect_with_tokens(text: &str, tokens: &[Token], lexicon: &Lexicon) -> Vec<Error> {
    let _ = text;
    let mut out = Vec::new();
    out.extend(detect_articles_pos(text, tokens));
    out.extend(detect_agreement_pos(text, tokens));
    out.extend(detect_tense_pos(text, tokens, lexicon));
    out.extend(detect_prepositions_pos(text, tokens));
    out
}

/// 冠词错误（POS）：DT "a"/"an" + 下一个词首字母判断
fn detect_articles_pos(text: &str, tokens: &[Token]) -> Vec<Error> {
    let _ = text;
    let mut out = Vec::new();
    for (i, token) in tokens.iter().enumerate() {
        if token.pos != "DT" {
            continue;
        }
        let lower = token.text.to_lowercase();
        if lower != "a" && lower != "an" {
            continue;
        }
        let next = match tokens.get(i + 1) {
            Some(n) => n,
            None => continue,
        };
        let first_char = next.text.chars().next().unwrap_or(' ');
        let is_vowel = "aeiouAEIOU".contains(first_char);
        if lower == "a" && is_vowel {
            out.push(Error::new(
                "en",
                error_types::ARTICLE,
                0.85,
                token.span,
                &token.text,
                Some("an".to_string()),
                format!("元音音素前应使用 \"an {}...\"", next.text),
            ));
        } else if lower == "an" && !is_vowel {
            out.push(Error::new(
                "en",
                error_types::ARTICLE,
                0.85,
                token.span,
                &token.text,
                Some("a".to_string()),
                format!("辅音音素前应使用 \"a {}...\"", next.text),
            ));
        }
    }
    out
}

/// 主谓一致（POS）：PRP (he/she/it) + VB (base form, 应为 VBS)
fn detect_agreement_pos(text: &str, tokens: &[Token]) -> Vec<Error> {
    let _ = text;
    let mut out = Vec::new();
    for (i, token) in tokens.iter().enumerate() {
        if token.pos != "PRP" {
            continue;
        }
        let lower = token.text.to_lowercase();
        if !matches!(lower.as_str(), "he" | "she" | "it") {
            continue;
        }
        let next = match tokens.get(i + 1) {
            Some(n) => n,
            None => continue,
        };
        if next.pos == "VB" {
            let corrected = format!("{}s", next.text);
            out.push(Error::new(
                "en",
                error_types::AGREEMENT,
                0.8,
                next.span,
                &next.text,
                Some(corrected),
                format!("第三人称单数主语 \"{}\" 后动词需加 -s", token.text),
            ));
        }
    }
    out
}

/// 时态错误（POS）：句中出现过去时间指示词 + VB（base form 应为 VBD）
fn detect_tense_pos(text: &str, tokens: &[Token], lexicon: &Lexicon) -> Vec<Error> {
    let _ = text;
    let mut out = Vec::new();
    let has_past_marker = tokens.iter().any(|t| {
        let lower = t.text.to_lowercase();
        lexicon
            .past_time_markers
            .iter()
            .any(|m| lower == *m || lower.starts_with(m))
    });
    if !has_past_marker {
        return out;
    }
    for token in tokens {
        if token.pos != "VB" {
            continue;
        }
        let lower = token.text.to_lowercase();
        if let Some((_, past)) = lexicon.past_tenses.iter().find(|(v, _)| *v == lower) {
            out.push(Error::new(
                "en",
                error_types::TENSE,
                0.75,
                token.span,
                &token.text,
                Some(past.clone()),
                format!(
                    "过去时间语境中 \"{}\" 应使用过去式 \"{}\"",
                    token.text, past
                ),
            ));
        }
    }
    out
}

/// 介词误用（POS）：IN "on" + DT "the" + NN (morning/evening/night)
fn detect_prepositions_pos(text: &str, tokens: &[Token]) -> Vec<Error> {
    let _ = text;
    let mut out = Vec::new();
    for (i, token) in tokens.iter().enumerate() {
        if token.pos != "IN" || token.text.to_lowercase() != "on" {
            continue;
        }
        let dt = match tokens.get(i + 1) {
            Some(t) if t.pos == "DT" && t.text.to_lowercase() == "the" => t,
            _ => continue,
        };
        let noun = match tokens.get(i + 2) {
            Some(t) if t.pos == "NN" || t.pos == "NNS" => t,
            _ => continue,
        };
        if matches!(
            noun.text.to_lowercase().as_str(),
            "morning" | "evening" | "night"
        ) {
            out.push(Error::new(
                "en",
                error_types::PREPOSITION,
                0.8,
                token.span,
                &token.text,
                Some("in".to_string()),
                format!("\"the {}\" 前应使用介词 \"in\"", noun.text),
            ));
            let _ = dt;
        }
    }
    out
}

// ─────────────────────────────────────────────────────────────────────────────
// Local rules (offline fallback: no POS available)
// ─────────────────────────────────────────────────────────────────────────────

/// 不依赖 POS 的本地规则（离线兜底）
pub fn detect_local(text: &str, lexicon: &Lexicon) -> Vec<Error> {
    let mut out = Vec::new();
    out.extend(detect_articles_local(text));
    out.extend(detect_agreement_local(text, lexicon));
    out.extend(detect_tense_local(text, lexicon));
    out.extend(detect_prepositions_local(text));
    // 扩展错误类型：覆盖英文错误分类体系（25 类）中词表/正则可达的类型
    out.extend(detect_adjective_local(text, lexicon));
    out.extend(detect_adverb_local(text, lexicon));
    out.extend(detect_noun_local(text, lexicon));
    out.extend(detect_pronoun_local(text, lexicon));
    out.extend(detect_verb_local(text, lexicon));
    out.extend(detect_gerund_local(text, lexicon));
    out.extend(detect_infinitive_local(text, lexicon));
    out.extend(detect_subjunctive_local(text, lexicon));
    out.extend(detect_negation_local(text, lexicon));
    // 拼写错误（词表驱动，高精度）
    out.extend(detect_spelling_local(text, lexicon));
    // 按 (span, error_type) 去重，避免多条规则命中同一位置
    out.sort_by_key(|e| (e.span, e.error_type.clone()));
    out.dedup_by(|a, b| a.span == b.span && a.error_type == b.error_type);
    out
}

/// 冠词错误（离线）：a + 元音开头 / an + 辅音开头
fn detect_articles_local(text: &str) -> Vec<Error> {
    let mut out = Vec::new();
    for caps in RE_A_BEFORE_VOWEL.captures_iter(text) {
        let m = caps.get(0).unwrap();
        let word = caps.get(1).unwrap();
        out.push(Error::new_with_meta(
            "en",
            error_types::ARTICLE,
            0.8,
            [m.start(), m.start() + 1],
            "a",
            Some("an".to_string()),
            format!("元音音素前应使用 \"an {}...\"", &word.as_str()),
            Some("grammar"),
            Some("EN_ARTICLE"),
            vec![Suggestion::new("an", 0.8)],
        ));
    }
    for caps in RE_AN_BEFORE_CONSONANT.captures_iter(text) {
        let m = caps.get(0).unwrap();
        let word = caps.get(1).unwrap();
        out.push(Error::new_with_meta(
            "en",
            error_types::ARTICLE,
            0.8,
            [m.start(), m.start() + 2],
            "an",
            Some("a".to_string()),
            format!("辅音音素前应使用 \"a {}...\"", &word.as_str()),
            Some("grammar"),
            Some("EN_ARTICLE"),
            vec![Suggestion::new("a", 0.8)],
        ));
    }
    out
}

/// 主谓一致（离线）：he/she/it + 动词原形（缺 -s）
fn detect_agreement_local(text: &str, lexicon: &Lexicon) -> Vec<Error> {
    let mut out = Vec::new();
    for verb in &lexicon.common_verbs {
        let re = cached_regex(&format!(r"(?i)\b(he|she|it)\s+{verb}\b"));
        for caps in re.captures_iter(text) {
            let m = caps.get(0).unwrap();
            let subject = caps.get(1).unwrap().as_str();
            out.push(Error::new(
                "en",
                error_types::AGREEMENT,
                0.75,
                [m.start(), m.end()],
                m.as_str(),
                Some(format!("{} {}s", subject, verb)),
                format!("第三人称单数主语 \"{}\" 后动词需加 -s", subject),
            ));
        }
    }
    out
}

/// 时态错误（离线）：yesterday 出现在句中 + 动词原形
fn detect_tense_local(text: &str, lexicon: &Lexicon) -> Vec<Error> {
    let mut out = Vec::new();
    if !text.to_lowercase().contains("yesterday") {
        return out;
    }
    for (verb, past) in &lexicon.past_tenses {
        let re = cached_regex(&format!(r"(?i)\b{verb}\b"));
        for m in re.find_iter(text) {
            out.push(Error::new(
                "en",
                error_types::TENSE,
                0.7,
                [m.start(), m.end()],
                m.as_str(),
                Some(past.clone()),
                format!(
                    "\"yesterday\" 表示过去时间，\"{}\" 应使用过去式 \"{}\"",
                    verb, past
                ),
            ));
        }
    }
    out
}

/// 常见介词误用（离线）：on the morning/evening/night → in the ...；
/// in + 星期（in Monday → on Monday）
fn detect_prepositions_local(text: &str) -> Vec<Error> {
    let mut out = Vec::new();
    let re = cached_regex(r"\bon\s+the\s+(morning|evening|night)\b");
    for m in re.find_iter(text) {
        out.push(Error::new(
            "en",
            error_types::PREPOSITION,
            0.75,
            [m.start(), m.start() + 2],
            "on",
            Some("in".to_string()),
            format!(
                "\"the {}\" 前应使用介词 \"in\"",
                m.as_str().split_whitespace().last().unwrap_or("")
            ),
        ));
    }
    // in + 星期 → on（on Monday / on Tuesday ...）
    let re_day =
        cached_regex(r"(?i)\b(in)\s+(monday|tuesday|wednesday|thursday|friday|saturday|sunday)\b");
    for caps in re_day.captures_iter(text) {
        let m = caps.get(1).unwrap();
        let day = caps.get(2).unwrap();
        out.push(Error::new(
            "en",
            error_types::PREPOSITION,
            0.85,
            [m.start(), m.end()],
            m.as_str(),
            Some("on".to_string()),
            format!("星期 \"{}\" 前应使用介词 \"on\"", day.as_str()),
        ));
    }
    out
}

/// 拼写错误（离线）：词表 misspellings（错→对）整词匹配。
/// 带 token 条件的条目在此无法求值 → 按保守原则跳过；
/// `context_regex` 条件对命中位置周边窗口求值。
fn detect_spelling_local(text: &str, lexicon: &Lexicon) -> Vec<Error> {
    let mut out = Vec::new();
    for entry in &lexicon.misspellings {
        if let Some(cond) = entry.condition()
            && cond.needs_tokens()
        {
            continue;
        }
        let wrong = entry.wrong();
        let re = cached_regex(&format!(r"(?i)\b{}\b", regex::escape(wrong)));
        for m in re.find_iter(text) {
            if let Some(cond) = entry.condition() {
                let window = window_around(text, m.start(), m.end(), 8);
                if !cond.check_context(&window) {
                    continue;
                }
            }
            let right = entry.right().to_string();
            out.push(Error::new_with_meta(
                "en",
                error_types::SPELLING,
                0.9,
                [m.start(), m.end()],
                m.as_str(),
                Some(right.clone()),
                format!("拼写错误：\"{}\" 应为 \"{}\"", m.as_str(), right),
                Some("spelling"),
                Some("EN_SPELLING"),
                vec![Suggestion::new(right, 0.9)],
            ));
        }
    }
    out
}

// ─────────────────────────────────────────────────────────────────────────────
// 扩展错误类型（覆盖英文错误分类体系中词表/正则可达的类型）
// 均为词表/正则驱动的本地规则，不依赖 POS，在线/离线两条路径共用。
// ─────────────────────────────────────────────────────────────────────────────

/// 形容词比较级/最高级误用：more better / most best / gooder → 正确形式
fn detect_adjective_local(text: &str, lexicon: &Lexicon) -> Vec<Error> {
    let mut out = Vec::new();
    for (wrong, right) in &lexicon.adjective_comparatives {
        let re = cached_regex(&format!(r"(?i)\b{}\b", regex::escape(wrong)));
        for m in re.find_iter(text) {
            out.push(Error::new_with_meta(
                "en",
                error_types::ADJECTIVE,
                0.8,
                [m.start(), m.end()],
                m.as_str(),
                Some(right.clone()),
                format!("比较级/最高级误用：\"{}\" 应为 \"{}\"", wrong, right),
                Some("grammar"),
                Some("EN_ADJECTIVE"),
                vec![Suggestion::new(right.clone(), 0.8)],
            ));
        }
    }
    out
}

/// 系动词/状态动词：其后接形容词作表语是正确用法（You look quiet），不应触发形副同形检测
const LINKING_VERBS: &[&str] = &[
    "be", "is", "am", "are", "was", "were", "look", "feel", "seem", "appear", "sound", "taste",
    "smell", "become", "get", "turn", "go", "stay", "remain", "lie", "sit", "happen", "keep",
];

/// 形副同形误用：实义动词后误用形容词作状语（run quick → run quickly）。
/// 系动词（look/feel/seem…）后接形容词是表语，属正确用法，予以排除。
fn detect_adverb_local(text: &str, lexicon: &Lexicon) -> Vec<Error> {
    let mut out = Vec::new();
    let verbs: std::collections::HashSet<String> = lexicon
        .common_verbs
        .iter()
        .map(|v| v.to_lowercase())
        .collect();
    for (adj, adv) in &lexicon.adverb_pairs {
        let re = cached_regex(&format!(r"(?i)\b(\w+)\s+({})\b", regex::escape(adj)));
        for caps in re.captures_iter(text) {
            let verb = caps.get(1).unwrap();
            let verb_lower = verb.as_str().to_lowercase();
            // 动词可能是屈折形式（runs/looks），去 -s 后匹配基础形式
            let base = verb_lower.strip_suffix('s').unwrap_or(verb_lower.as_str());
            let is_common = verbs.contains(&verb_lower) || verbs.contains(base);
            let is_linking =
                LINKING_VERBS.contains(&verb_lower.as_str()) || LINKING_VERBS.contains(&base);
            // 前词须是常见动词，且非系动词（系动词后接形容词作表语是正确用法）
            if !is_common || is_linking {
                continue;
            }
            let adjm = caps.get(2).unwrap();
            out.push(Error::new(
                "en",
                error_types::ADVERB,
                0.7,
                [adjm.start(), adjm.end()],
                adjm.as_str(),
                Some(adv.clone()),
                format!("修饰动词 \"{}\" 时应用副词 \"{}\"", verb.as_str(), adv),
            ));
        }
    }
    out
}

/// 不可数名词误用：a information / informations → information
fn detect_noun_local(text: &str, lexicon: &Lexicon) -> Vec<Error> {
    let mut out = Vec::new();
    for (wrong, right) in &lexicon.uncountable_nouns {
        let re = cached_regex(&format!(r"(?i)\b{}\b", regex::escape(wrong)));
        for m in re.find_iter(text) {
            out.push(Error::new_with_meta(
                "en",
                error_types::NOUN,
                0.8,
                [m.start(), m.end()],
                m.as_str(),
                Some(right.clone()),
                format!("不可数名词误用：\"{}\" 应为 \"{}\"", wrong, right),
                Some("grammar"),
                Some("EN_NOUN"),
                vec![Suggestion::new(right.clone(), 0.8)],
            ));
        }
    }
    out
}

/// 反身代词缺失：主谓同指时宾语应为反身代词（He hurt him → He hurt himself）。
/// 限制主语与宾语之间最多 2 个词，避免跨子句误匹配（如 "it will be easier for us"）。
fn detect_pronoun_local(text: &str, lexicon: &Lexicon) -> Vec<Error> {
    let mut out = Vec::new();
    for (subject, object, reflexive) in &lexicon.reflexive_pronouns {
        let re = cached_regex(&format!(
            r"(?i)\b{}\b(?:\s+\w+){{0,2}}\s+({})\b",
            regex::escape(subject),
            regex::escape(object)
        ));
        for caps in re.captures_iter(text) {
            let obj = caps.get(1).unwrap();
            out.push(Error::new(
                "en",
                error_types::PRONOUN,
                0.7,
                [obj.start(), obj.end()],
                obj.as_str(),
                Some(reflexive.clone()),
                format!("主谓同指时宾语应使用反身代词 \"{}\"", reflexive),
            ));
        }
    }
    out
}

/// 动词+介词 误用：discuss about → discuss（及物动词后无需介词）
fn detect_verb_local(text: &str, lexicon: &Lexicon) -> Vec<Error> {
    let mut out = Vec::new();
    for (wrong, right) in &lexicon.verb_preposition_confusables {
        let re = cached_regex(&format!(r"(?i)\b{}\b", regex::escape(wrong)));
        for m in re.find_iter(text) {
            out.push(Error::new_with_meta(
                "en",
                error_types::VERB,
                0.8,
                [m.start(), m.end()],
                m.as_str(),
                Some(right.clone()),
                format!("\"{}\" 后无需再加介词，应为 \"{}\"", wrong, right),
                Some("grammar"),
                Some("EN_VERB_PREP"),
                vec![Suggestion::new(right.clone(), 0.8)],
            ));
        }
    }
    out
}

/// "to" 后若为这些词（冠词/限定词/介词/代词等），说明非 "to+动词原形"，跳过
const NON_VERB_AFTER_TO: &[&str] = &[
    "the", "a", "an", "of", "in", "on", "at", "to", "for", "with", "by", "from", "up", "out",
    "off", "me", "you", "him", "her", "it", "us", "them", "this", "that", "these", "those", "my",
    "your", "his", "its", "our", "their", "no", "not", "so", "as", "or", "but", "and",
];

/// 动名词缺失：后接动名词的动词误用 to-infinitive（enjoy to play → enjoy playing）。
/// 若 "to" 后是冠词/限定词等（如 keep to the rules），非 "to+动词"，跳过以免误报。
fn detect_gerund_local(text: &str, lexicon: &Lexicon) -> Vec<Error> {
    let mut out = Vec::new();
    for verb in &lexicon.gerund_verbs {
        let re = cached_regex(&format!(r"(?i)\b{}\s+to\s+(\w+)", regex::escape(verb)));
        for caps in re.captures_iter(text) {
            let base = caps.get(1).unwrap();
            let base_lower = base.as_str().to_lowercase();
            if NON_VERB_AFTER_TO.contains(&base_lower.as_str()) {
                continue;
            }
            let m = caps.get(0).unwrap();
            out.push(Error::new_with_meta(
                "en",
                error_types::GERUND,
                0.75,
                [m.start(), m.end()],
                m.as_str(),
                Some(format!("{} {}ing", verb, base.as_str())),
                format!("\"{}\" 后应接动名词（-ing），而非 \"to do\"", verb),
                Some("grammar"),
                Some("EN_GERUND"),
                vec![Suggestion::new(
                    format!("{} {}ing", verb, base.as_str()),
                    0.75,
                )],
            ));
        }
    }
    out
}

/// 不定式缺失：后接不定式的动词误用动名词（want playing → want to play）
fn detect_infinitive_local(text: &str, lexicon: &Lexicon) -> Vec<Error> {
    let mut out = Vec::new();
    for verb in &lexicon.infinitive_verbs {
        let re = cached_regex(&format!(r"(?i)\b{}\s+([a-z]+)ing\b", regex::escape(verb)));
        for caps in re.captures_iter(text) {
            let stem = caps.get(1).unwrap();
            let m = caps.get(0).unwrap();
            let base = strip_ing(stem.as_str());
            out.push(Error::new_with_meta(
                "en",
                error_types::INFINITIVE,
                0.7,
                [m.start(), m.end()],
                m.as_str(),
                Some(format!("{} to {}", verb, base)),
                format!("\"{}\" 后应接不定式（to do），而非动名词", verb),
                Some("grammar"),
                Some("EN_INFINITIVE"),
                vec![Suggestion::new(format!("{} to {}", verb, base), 0.7)],
            ));
        }
    }
    out
}

/// 虚拟语气：wish/if 后 be 动词 was → were（I wish I was → I wish I were）
fn detect_subjunctive_local(text: &str, lexicon: &Lexicon) -> Vec<Error> {
    let mut out = Vec::new();
    if lexicon.subjunctive_markers.is_empty() {
        return out;
    }
    let markers = lexicon
        .subjunctive_markers
        .iter()
        .map(|s| regex::escape(s))
        .collect::<Vec<_>>()
        .join("|");
    let re = cached_regex(&format!(
        r"(?i)\b({})\b[^.!?]*\b(I|he|she|it|we|they)\s+(was)\b",
        markers
    ));
    for caps in re.captures_iter(text) {
        let marker = caps.get(1).unwrap();
        let was = caps.get(3).unwrap();
        out.push(Error::new_with_meta(
            "en",
            error_types::SUBJUNCTIVE,
            0.75,
            [was.start(), was.end()],
            was.as_str(),
            Some("were".to_string()),
            format!(
                "\"{}\" 后的虚拟语气中 be 动词应用 \"were\"",
                marker.as_str()
            ),
            Some("grammar"),
            Some("EN_SUBJUNCTIVE"),
            vec![Suggestion::new("were", 0.75)],
        ));
    }
    out
}

/// 双重否定：否定算子 + 否定词（don't know nothing → don't know anything）
fn detect_negation_local(text: &str, lexicon: &Lexicon) -> Vec<Error> {
    let mut out = Vec::new();
    if lexicon.negation_operators.is_empty() {
        return out;
    }
    let ops = lexicon
        .negation_operators
        .iter()
        .map(|s| regex::escape(s))
        .collect::<Vec<_>>()
        .join("|");
    for (neg, pos) in &lexicon.negation_markers {
        let re = cached_regex(&format!(
            r"(?i)\b({})\b[^.!?]*\b({})\b",
            ops,
            regex::escape(neg)
        ));
        for caps in re.captures_iter(text) {
            let op = caps.get(1).unwrap();
            let negm = caps.get(2).unwrap();
            out.push(Error::new_with_meta(
                "en",
                error_types::NEGATION,
                0.7,
                [negm.start(), negm.end()],
                negm.as_str(),
                Some(pos.clone()),
                format!(
                    "双重否定：已有否定词 \"{}\"，此处 \"{}\" 应改为 \"{}\"",
                    op.as_str(),
                    neg,
                    pos
                ),
                Some("grammar"),
                Some("EN_NEGATION"),
                vec![Suggestion::new(pos.clone(), 0.7)],
            ));
        }
    }
    out
}

/// 从 -ing 形式还原动词原形（处理双写辅音；e 结尾动词天然保留）。
fn strip_ing(w: &str) -> String {
    if w.len() > 3 && w.ends_with("ing") {
        let stem = &w[..w.len() - 3];
        let b = stem.as_bytes();
        if b.len() >= 2
            && b[b.len() - 1] == b[b.len() - 2]
            && !matches!(b[b.len() - 1] as char, 'a' | 'e' | 'i' | 'o' | 'u')
        {
            return stem[..stem.len() - 1].to_string();
        }
        return stem.to_string();
    }
    w.to_string()
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

    // ── POS-based tests ──

    #[test]
    fn pos_article_rules() {
        let tokens = vec![
            token("I", "PRP", [0, 1]),
            token("saw", "VBD", [2, 5]),
            token("a", "DT", [6, 7]),
            token("apple", "NN", [8, 13]),
        ];
        let errors = detect_with_tokens("I saw a apple.", &tokens, &Lexicon::default());
        assert!(errors.iter().any(|e| e.error_type == error_types::ARTICLE
            && e.original == "a"
            && e.correction.as_deref() == Some("an")));
    }

    #[test]
    fn pos_agreement_rules() {
        let tokens = vec![
            token("He", "PRP", [0, 2]),
            token("play", "VB", [3, 7]),
            token("football", "NN", [8, 16]),
        ];
        let errors = detect_with_tokens("He play football.", &tokens, &Lexicon::default());
        assert!(
            errors.iter().any(|e| e.error_type == error_types::AGREEMENT
                && e.correction.as_deref() == Some("plays"))
        );
    }

    #[test]
    fn pos_tense_rules() {
        let tokens = vec![
            token("Yesterday", "RB", [0, 9]),
            token("I", "PRP", [10, 11]),
            token("go", "VB", [12, 14]),
            token("to", "TO", [15, 17]),
            token("the", "DT", [18, 21]),
            token("park", "NN", [22, 26]),
        ];
        let errors =
            detect_with_tokens("Yesterday I go to the park.", &tokens, &Lexicon::default());
        assert!(
            errors
                .iter()
                .any(|e| e.error_type == error_types::TENSE
                    && e.correction.as_deref() == Some("went"))
        );
    }

    #[test]
    fn pos_preposition_rules() {
        let tokens = vec![
            token("I", "PRP", [0, 1]),
            token("saw", "VBD", [2, 5]),
            token("him", "PRP", [6, 9]),
            token("on", "IN", [10, 12]),
            token("the", "DT", [13, 16]),
            token("morning", "NN", [17, 24]),
        ];
        let errors = detect_with_tokens("I saw him on the morning.", &tokens, &Lexicon::default());
        assert!(
            errors
                .iter()
                .any(|e| e.error_type == error_types::PREPOSITION
                    && e.correction.as_deref() == Some("in"))
        );
    }

    #[test]
    fn pos_clean_text_no_errors() {
        let tokens = vec![
            token("She", "PRP", [0, 3]),
            token("plays", "VBS", [4, 9]),
            token("football", "NN", [10, 18]),
            token("in", "IN", [19, 21]),
            token("the", "DT", [22, 25]),
            token("morning", "NN", [26, 33]),
        ];
        let errors = detect_with_tokens(
            "She plays football in the morning.",
            &tokens,
            &Lexicon::default(),
        );
        assert!(errors.is_empty());
    }

    // ── Local (offline) tests ──

    #[test]
    fn article_rules_local() {
        let errors = detect_local("I saw a apple and an car.", &Lexicon::default());
        assert!(errors.iter().any(|e| e.error_type == error_types::ARTICLE
            && e.original == "a"
            && e.correction.as_deref() == Some("an")));
        assert!(errors.iter().any(|e| e.error_type == error_types::ARTICLE
            && e.original == "an"
            && e.correction.as_deref() == Some("a")));
    }

    #[test]
    fn agreement_rules_local() {
        let errors = detect_local("He play football every day.", &Lexicon::default());
        assert!(errors.iter().any(|e| e.error_type == error_types::AGREEMENT
            && e.correction.as_deref() == Some("He plays")));
    }

    #[test]
    fn tense_rules_local() {
        let errors = detect_local("Yesterday I go to the park.", &Lexicon::default());
        assert!(
            errors
                .iter()
                .any(|e| e.error_type == error_types::TENSE
                    && e.correction.as_deref() == Some("went"))
        );
    }

    #[test]
    fn preposition_rules_local() {
        let errors = detect_local("I saw him on the morning.", &Lexicon::default());
        assert!(
            errors
                .iter()
                .any(|e| e.error_type == error_types::PREPOSITION
                    && e.correction.as_deref() == Some("in"))
        );
    }

    #[test]
    fn clean_text_has_no_errors() {
        assert!(detect_local("She plays football in the morning.", &Lexicon::default()).is_empty());
    }

    // ── 扩展错误类型测试 ──

    #[test]
    fn adjective_comparative_local() {
        let errors = detect_local("This is more better than that.", &Lexicon::default());
        assert!(
            errors.iter().any(|e| e.error_type == error_types::ADJECTIVE
                && e.correction.as_deref() == Some("better"))
        );
    }

    #[test]
    fn adverb_after_content_verb_local() {
        // 实义动词 + 形容词 → 副词（run 是实义动词，非系动词）
        let errors = detect_local("He runs slow.", &Lexicon::default());
        assert!(
            errors.iter().any(|e| e.error_type == error_types::ADVERB
                && e.correction.as_deref() == Some("slowly"))
        );
    }

    #[test]
    fn adverb_linking_verb_not_flagged() {
        // 系动词 + 形容词表语是正确用法，不应误报
        assert!(detect_local("You look quiet.", &Lexicon::default()).is_empty());
        assert!(detect_local("She feels eager.", &Lexicon::default()).is_empty());
        assert!(detect_local("He seems gentle.", &Lexicon::default()).is_empty());
    }

    #[test]
    fn noun_uncountable_local() {
        let errors = detect_local("I need some informations.", &Lexicon::default());
        assert!(
            errors.iter().any(|e| e.error_type == error_types::NOUN
                && e.correction.as_deref() == Some("information"))
        );
    }

    #[test]
    fn pronoun_reflexive_local() {
        let errors = detect_local("He hurt him yesterday.", &Lexicon::default());
        assert!(
            errors.iter().any(|e| e.error_type == error_types::PRONOUN
                && e.correction.as_deref() == Some("himself"))
        );
    }

    #[test]
    fn verb_preposition_local() {
        let errors = detect_local("We discuss about it.", &Lexicon::default());
        assert!(errors.iter().any(
            |e| e.error_type == error_types::VERB && e.correction.as_deref() == Some("discuss")
        ));
    }

    #[test]
    fn gerund_after_gerund_verb_local() {
        let errors = detect_local("I enjoy to play.", &Lexicon::default());
        assert!(errors.iter().any(|e| e.error_type == error_types::GERUND
            && e.correction.as_deref() == Some("enjoy playing")));
    }

    #[test]
    fn infinitive_after_infinitive_verb_local() {
        let errors = detect_local("I want playing.", &Lexicon::default());
        assert!(
            errors
                .iter()
                .any(|e| e.error_type == error_types::INFINITIVE
                    && e.correction.as_deref() == Some("want to play"))
        );
    }

    #[test]
    fn subjunctive_wish_were_local() {
        let errors = detect_local("I wish I was you.", &Lexicon::default());
        assert!(
            errors
                .iter()
                .any(|e| e.error_type == error_types::SUBJUNCTIVE
                    && e.correction.as_deref() == Some("were"))
        );
    }

    #[test]
    fn negation_double_negative_local() {
        let errors = detect_local("I don't know nothing.", &Lexicon::default());
        assert!(errors.iter().any(|e| e.error_type == error_types::NEGATION
            && e.correction.as_deref() == Some("anything")));
    }

    #[test]
    fn strip_ing_helper() {
        // 常规 -ing 反转
        assert_eq!(strip_ing("playing"), "play");
        assert_eq!(strip_ing("wanting"), "want");
        // 双写辅音还原
        assert_eq!(strip_ing("running"), "run");
        assert_eq!(strip_ing("planning"), "plan");
        // 注：e-dropping 动词（decide/hope/refuse 等）去 -ing 会丢失尾 e，
        // 属 -ing 反转的固有局限；此类动词的常规不定式误用（want playing）不受影响。
    }

    #[test]
    fn extended_clean_text_has_no_errors() {
        // 干净句子不应触发任何扩展规则
        assert!(detect_local("She is a good student.", &Lexicon::default()).is_empty());
    }

    #[test]
    fn rule_metadata_populated() {
        // 验证新字段 category / rule_id / suggestions 正确填充
        let errors = detect_local("This is more better than that.", &Lexicon::default());
        let adj_err = errors
            .iter()
            .find(|e| e.error_type == error_types::ADJECTIVE)
            .expect("应检测到形容词比较级错误");
        assert_eq!(adj_err.category.as_deref(), Some("grammar"));
        assert_eq!(adj_err.rule_id.as_deref(), Some("EN_ADJECTIVE"));
        assert!(!adj_err.suggestions.is_empty());
        assert_eq!(adj_err.suggestions[0].text, "better");
    }

    #[test]
    fn lexicon_rules_and_categories_loaded() {
        let lex = Lexicon::default();
        // 词表应包含 categories 和 rules 元数据
        assert!(!lex.categories.is_empty());
        assert!(!lex.rules.is_empty());
        // 每条 rule 都有 id 和 category
        for r in &lex.rules {
            assert!(!r.id.is_empty());
            assert!(!r.category.is_empty());
            assert!(r.enabled);
        }
        // 至少有一条 grammar 分类
        assert!(lex.categories.iter().any(|c| c.id == "grammar"));
    }

    #[test]
    fn spelling_local() {
        let errors = detect_local("I dont like this.", &Lexicon::default());
        assert!(errors.iter().any(|e| e.error_type == error_types::SPELLING
            && e.original == "dont"
            && e.correction.as_deref() == Some("don't")));
        let errors = detect_local("This is a exemple of my life.", &Lexicon::default());
        assert!(errors.iter().any(|e| e.error_type == error_types::SPELLING
            && e.original == "exemple"
            && e.correction.as_deref() == Some("example")));
        // 元数据：category=spelling, rule_id=EN_SPELLING
        let err = errors
            .iter()
            .find(|e| e.error_type == error_types::SPELLING)
            .unwrap();
        assert_eq!(err.category.as_deref(), Some("spelling"));
        assert_eq!(err.rule_id.as_deref(), Some("EN_SPELLING"));
    }

    #[test]
    fn spelling_clean_word_not_flagged() {
        // 正确拼写的词不应被误报（whole-word 匹配）
        assert!(detect_local("I prefer the example above.", &Lexicon::default()).is_empty());
        assert!(
            detect_local("Their peole were here.", &Lexicon::default())
                .iter()
                .filter(|e| e.error_type == error_types::SPELLING)
                .count()
                == 1
        ); // 只有 peole 命中，people 是正确词
    }

    #[test]
    fn preposition_day_of_week_local() {
        let errors = detect_local("I saw him in Monday.", &Lexicon::default());
        assert!(
            errors
                .iter()
                .any(|e| e.error_type == error_types::PREPOSITION
                    && e.original == "in"
                    && e.correction.as_deref() == Some("on"))
        );
        // on Monday 是正确用法，不应误报
        assert!(
            !detect_local("I saw him on Monday.", &Lexicon::default())
                .iter()
                .any(|e| e.error_type == error_types::PREPOSITION)
        );
    }

    fn lexicon_from_json(json: &str) -> Lexicon {
        serde_json::from_str(json).expect("词表 JSON 应可解析")
    }

    #[test]
    fn conditional_misspelling_entry_parses() {
        // 二元组（无条件）与对象形式（带条件）可混用
        let lex = lexicon_from_json(
            r#"{
              "misspellings": [
                ["peole", "people"],
                {
                  "wrong": "wich",
                  "right": "which",
                  "condition": { "context_regex": "wich\\s+(one|two|three)" }
                }
              ]
            }"#,
        );
        assert_eq!(lex.misspellings.len(), 2);
        assert_eq!(lex.misspellings[0].wrong(), "peole");
        assert!(lex.misspellings[0].condition().is_none());
        assert_eq!(lex.misspellings[1].wrong(), "wich");
        assert!(lex.misspellings[1].condition().is_some());
    }

    #[test]
    fn conditional_misspelling_context_regex() {
        let lex = lexicon_from_json(
            r#"{
              "misspellings": [
                {
                  "wrong": "wich",
                  "right": "which",
                  "condition": { "context_regex": "wich\\s+(one|two|three)" }
                }
              ]
            }"#,
        );
        // 窗口匹配 → 触发
        let errors = detect_local("Tell me wich one you want.", &lex);
        assert!(
            errors
                .iter()
                .any(|e| e.original == "wich" && e.correction.as_deref() == Some("which"))
        );
        // 窗口不匹配 → 不触发
        let errors = detect_local("Tell me wich color you want.", &lex);
        assert!(!errors.iter().any(|e| e.original == "wich"));
    }

    #[test]
    fn conditional_misspelling_pos_only_skipped() {
        // 仅含 token 条件 → 本地路径无法求值，整体跳过（保守原则）
        let lex = lexicon_from_json(
            r#"{
              "misspellings": [
                {
                  "wrong": "recieve",
                  "right": "receive",
                  "condition": { "next_pos": ["NN"] }
                }
              ]
            }"#,
        );
        assert!(detect_local("I recieve a letter.", &lex).is_empty());
    }
}
