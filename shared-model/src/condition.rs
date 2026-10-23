//! 词表条目条件范式
//!
//! 词表条目可附加 [`EntryCondition`]：规则仅在条件满足时触发。
//!
//! 条件分两类：
//! - **token 条件**（`prev_pos` / `next_pos` / `prev_text` / `next_text`）：
//!   依赖分词/POS 结果，只能在在线（POS）路径求值；离线路径无法求值，
//!   按保守原则跳过该条目（不触发）。
//! - **文本条件**（`context_regex`）：命中位置周边窗口须匹配的正则，
//!   在线与离线路径均可求值（窗口见 [`window_around`]）。
//!
//! 所有子条件为 AND 关系；子条件为空表示不限制。
//! 无条件（或空条件）的条目命中即触发，保持传统行为。
//!
//! 词表 JSON 示例（对象形式，与无条件的数组形式可混用）：
//!
//! ```json
//! {
//!   "wrong": "象",
//!   "right": "像",
//!   "note": "「象」误用于「像」（如：好象→好像）",
//!   "condition": {
//!     "prev_text": ["好"],
//!     "context_regex": "好.象"
//!   }
//! }
//! ```

use regex::Regex;
use serde::{Deserialize, Serialize};

/// token 只读引用（条件求值所需的 text/POS）
#[derive(Debug, Clone, Copy)]
pub struct TokenRef<'a> {
    pub text: &'a str,
    pub pos: &'a str,
}

impl<'a> TokenRef<'a> {
    pub fn new(text: &'a str, pos: &'a str) -> Self {
        Self { text, pos }
    }
}

/// 词表条目的附加条件：规则仅在条件满足时触发
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EntryCondition {
    /// 前一 token 的 POS 须在此集合内（空 = 不限制）
    pub prev_pos: Vec<String>,
    /// 后一 token 的 POS 须在此集合内（空 = 不限制）
    pub next_pos: Vec<String>,
    /// 前一 token 的文本须在此集合内（空 = 不限制）
    pub prev_text: Vec<String>,
    /// 后一 token 的文本须在此集合内（空 = 不限制）
    pub next_text: Vec<String>,
    /// 命中位置周边窗口须匹配的正则（离线可求值，如「须紧邻汉字」）
    pub context_regex: Option<String>,
}

impl EntryCondition {
    /// 条件是否为空（无任何限制，命中即触发）
    pub fn is_empty(&self) -> bool {
        self.prev_pos.is_empty()
            && self.next_pos.is_empty()
            && self.prev_text.is_empty()
            && self.next_text.is_empty()
            && self.context_regex.is_none()
    }

    /// 是否需要 token（POS）信息才能求值。
    /// 离线路径若返回 true 应跳过该条目（保守原则：无法求值即不触发）。
    pub fn needs_tokens(&self) -> bool {
        !self.prev_pos.is_empty()
            || !self.next_pos.is_empty()
            || !self.prev_text.is_empty()
            || !self.next_text.is_empty()
    }

    /// 求值 token 条件：所有已指定子条件均须满足；
    /// 子条件已指定但对应邻接 token 缺失（句首/句尾）→ 不满足（保守）。
    pub fn check_tokens(&self, prev: Option<&TokenRef>, next: Option<&TokenRef>) -> bool {
        if !self.prev_pos.is_empty()
            && !prev.is_some_and(|p| self.prev_pos.iter().any(|x| *x == p.pos))
        {
            return false;
        }
        if !self.next_pos.is_empty()
            && !next.is_some_and(|p| self.next_pos.iter().any(|x| *x == p.pos))
        {
            return false;
        }
        if !self.prev_text.is_empty()
            && !prev.is_some_and(|p| self.prev_text.iter().any(|x| *x == p.text))
        {
            return false;
        }
        if !self.next_text.is_empty()
            && !next.is_some_and(|p| self.next_text.iter().any(|x| *x == p.text))
        {
            return false;
        }
        true
    }

    /// 求值文本条件：`context_regex` 须匹配命中位置的周边窗口。
    /// 正则编译失败视为不满足（保守）。
    pub fn check_context(&self, window: &str) -> bool {
        match &self.context_regex {
            Some(re) => Regex::new(re).is_ok_and(|r| r.is_match(window)),
            None => true,
        }
    }
}

/// 取命中位置 `[start, end)` 的周边窗口（字符安全）：
/// 命中前后各至多 `n` 个字符。
pub fn window_around(text: &str, start: usize, end: usize, n: usize) -> String {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let s = chars
        .iter()
        .position(|(i, _)| *i >= start)
        .unwrap_or(chars.len());
    let e = chars
        .iter()
        .position(|(i, _)| *i >= end)
        .unwrap_or(chars.len());
    let lo = s.saturating_sub(n);
    let hi = (e + n).min(chars.len());
    chars[lo..hi].iter().map(|(_, c)| *c).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok<'a>(text: &'a str, pos: &'a str) -> TokenRef<'a> {
        TokenRef::new(text, pos)
    }

    #[test]
    fn empty_condition_always_passes() {
        let c = EntryCondition::default();
        assert!(c.is_empty());
        assert!(!c.needs_tokens());
        assert!(c.check_tokens(None, None));
        assert!(c.check_context("任意窗口"));
    }

    #[test]
    fn pos_and_text_conditions() {
        let c = EntryCondition {
            next_pos: vec!["AD".into()],
            ..Default::default()
        };
        assert!(c.needs_tokens());
        assert!(c.check_tokens(None, Some(&tok("高兴", "AD"))));
        assert!(!c.check_tokens(None, Some(&tok("跑", "VV"))));
        // 邻接 token 缺失 → 不满足（保守）
        assert!(!c.check_tokens(None, None));

        let c = EntryCondition {
            prev_text: vec!["好".into()],
            ..Default::default()
        };
        assert!(c.check_tokens(Some(&tok("好", "AD")), None));
        assert!(!c.check_tokens(Some(&tok("很", "AD")), None));
        assert!(!c.check_tokens(None, None));
    }

    #[test]
    fn context_regex_condition() {
        // 相邻匹配：「好」紧贴「象」
        let c = EntryCondition {
            context_regex: Some("好象".into()),
            ..Default::default()
        };
        assert!(!c.needs_tokens());
        assert!(c.check_context("他好象很高兴"));
        assert!(!c.check_context("他好像很高兴"));
        // 间隔匹配：「好」与「象」之间恰有一个字符
        let c2 = EntryCondition {
            context_regex: Some("好.象".into()),
            ..Default::default()
        };
        assert!(c2.check_context("他好得象个人"));
        assert!(!c2.check_context("他好象很高兴"));
        // 非法正则 → 保守不满足
        let bad = EntryCondition {
            context_regex: Some("(未闭合".into()),
            ..Default::default()
        };
        assert!(!bad.check_context("任意"));
    }

    #[test]
    fn window_around_is_char_safe() {
        // 「他昨天去公园,天气很好.」中「,」的字节偏移为 18
        let w = window_around("他昨天去公园,天气很好.", 18, 19, 2);
        // 窗口 = 命中前后各 2 字符
        assert!(w.contains('园'));
        assert!(w.contains(','));
        assert!(w.contains('天'));
        // 边界不越界
        assert_eq!(window_around("ab", 0, 1, 10), "ab");
    }

    #[test]
    fn condition_serde_defaults() {
        // 所有字段缺省 → 空条件
        let c: EntryCondition = serde_json::from_str("{}").unwrap();
        assert!(c.is_empty());
        let c: EntryCondition =
            serde_json::from_str(r#"{"next_pos":["AD"],"context_regex":"好.象"}"#).unwrap();
        assert_eq!(c.next_pos, vec!["AD".to_string()]);
        assert_eq!(c.context_regex.as_deref(), Some("好.象"));
    }
}
