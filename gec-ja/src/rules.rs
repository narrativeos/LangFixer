//! 日文 GEC 规则集
//!
//! 基于 Sudachi 词性标注的规则检测（助词缺失、动词变形、敬语混用），
//! 以及不依赖词性的本地规则（离线兜底使用）。
//!
//! 词表（名词+动词组合）外置于 `lexicons/ja_gec_lexicon.json`，
//! 支持不重新编译即可扩充；文件缺失或损坏时回退到内置默认词表（见 rules 模块内置的 `Lexicon`）。

use std::path::Path;

use serde::Deserialize;
use shared_model::{Error, Suggestion, error_types};

/// Sudachi token（features[0] 为词性，如「名詞」「助詞-一般」「動詞-終形」）
#[derive(Debug, Clone, Deserialize)]
pub struct Token {
    pub text: String,
    #[serde(default)]
    pub pos: String,
    pub span: [usize; 2],
}

/// 需要词性信息的完整规则集
pub fn detect_with_tokens(text: &str, tokens: &[Token], _lexicon: &Lexicon) -> Vec<Error> {
    let mut out = Vec::new();
    out.extend(detect_missing_particle(tokens));
    out.extend(detect_honorific_mixing_by_scan(text));
    out
}

/// 不依赖词性的本地规则（离线兜底）
pub fn detect_local(text: &str, lexicon: &Lexicon) -> Vec<Error> {
    let mut out = Vec::new();
    out.extend(detect_missing_particle_by_scan(text, lexicon));
    out.extend(detect_honorific_mixing_by_scan(text));
    out
}

/// 内置默认词表（外置 JSON 缺失或损坏时的兜底）
const DEFAULT_LEXICON_JSON: &str = include_str!("../../lexicons/ja_gec_lexicon.json");

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

/// 外置词表（`lexicons/ja_gec_lexicon.json`）
#[derive(Debug, Clone, Deserialize)]
pub struct Lexicon {
    /// 常见「名词 + 动词」组合（助词缺失检测用）
    #[serde(default)]
    pub noun_verb_pairs: Vec<(String, String)>,
    /// 规则分类定义
    #[serde(default)]
    pub categories: Vec<CategoryDef>,
    /// 规则元数据（id / category / priority / enabled / examples）
    #[serde(default)]
    pub rules: Vec<RuleEntry>,
}

impl Lexicon {
    /// 从目录加载词表（`{dir}/ja_gec_lexicon.json`）；
    /// 文件缺失或解析失败时回退到内置默认词表
    pub fn load(dir: &Path) -> Self {
        let path = dir.join("ja_gec_lexicon.json");
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
        serde_json::from_str(DEFAULT_LEXICON_JSON).expect("内置日文词表 JSON 应可解析")
    }
}

/// 助词缺失：名词直接接动词（缺少 を/を）
fn detect_missing_particle(tokens: &[Token]) -> Vec<Error> {
    let mut out = Vec::new();
    for (i, token) in tokens.iter().enumerate() {
        let is_noun = token.pos.starts_with("名詞");
        if let Some(next) = tokens.get(i + 1)
            && is_noun
            && next.pos.starts_with("動詞")
        {
            out.push(Error::new_with_meta(
                "ja",
                error_types::PARTICLE,
                0.65,
                [token.span[0], next.span[1]],
                format!("{}{}", token.text, next.text),
                Some(format!("{}を{}", token.text, next.text)),
                "名词后直接接动词，疑似缺少助词「を」",
                Some("grammar"),
                Some("JA_PARTICLE"),
                vec![Suggestion::new(
                    format!("{}を{}", token.text, next.text),
                    0.65,
                )],
            ));
        }
    }
    out
}

/// 助词缺失（离线：常见名词+动词组合扫描）
fn detect_missing_particle_by_scan(text: &str, lexicon: &Lexicon) -> Vec<Error> {
    let mut out = Vec::new();
    for (noun, verb) in &lexicon.noun_verb_pairs {
        let pair = format!("{noun}{verb}");
        for (idx, _) in text.match_indices(&pair) {
            // span 为字节偏移（与 shared_model::Error 契约一致）
            let end = idx + pair.len();
            out.push(Error::new_with_meta(
                "ja",
                error_types::PARTICLE,
                0.6,
                [idx, end],
                pair.clone(),
                Some(format!("{noun}を{verb}")),
                "名词后直接接动词，疑似缺少助词「を」",
                Some("grammar"),
                Some("JA_PARTICLE"),
                vec![Suggestion::new(format!("{noun}を{verb}"), 0.6)],
            ));
        }
    }
    out
}

/// 敬语混用：です/ます体系 与 だ/である体系 混用（基于原文扫描，在线/离线共用）
fn detect_honorific_mixing_by_scan(text: &str) -> Vec<Error> {
    let mut out = Vec::new();
    let has_polite = text.contains("です") || text.contains("ます");
    let has_plain = text.contains("だ。") || text.contains("である") || text.contains("なんだ");
    if has_polite && has_plain {
        out.push(Error::new_with_meta(
            "ja",
            error_types::HONORIFIC,
            0.7,
            [0, text.chars().count()],
            text.to_string(),
            None,
            "敬语（です/ます）与简体（だ/である）混用，请统一语体",
            Some("style"),
            Some("JA_HONORIFIC"),
            vec![],
        ));
    }
    out
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
    fn missing_particle_with_pos() {
        // span 为字节偏移（本=3 字节，読む=6 字节）
        let tokens = vec![
            token("本", "名詞-普通名詞", [0, 3]),
            token("読む", "動詞-終形", [3, 9]),
        ];
        let errors = detect_with_tokens("本読む", &tokens, &Lexicon::default());
        assert!(errors.iter().any(|e| e.error_type == error_types::PARTICLE
            && e.correction.as_deref() == Some("本を読む")));
    }

    #[test]
    fn missing_particle_by_scan() {
        let errors = detect_local("私は毎日本読む。", &Lexicon::default());
        assert!(
            errors
                .iter()
                .any(|e| e.error_type == error_types::PARTICLE && e.original == "本読む")
        );
    }

    #[test]
    fn honorific_mixing() {
        let errors = detect_local("これは本です。私は学生だ。", &Lexicon::default());
        assert!(
            errors
                .iter()
                .any(|e| e.error_type == error_types::HONORIFIC)
        );
    }

    #[test]
    fn clean_sentences_no_errors() {
        // 正确日文句子不应产生任何错误（误报回归）
        for s in [
            "私は学生です。",
            "これは本です。",
            "水を飲みます。",
            "今日はいい天気ですね。",
        ] {
            let errors = detect_local(s, &Lexicon::default());
            assert!(errors.is_empty(), "误报 [{s}]: {errors:?}");
        }
    }
}
