//! 会话（Session）模型
//!
//! 一次检测会话 = 一份卷子 / 一份书稿的完整检测周期。
//! 所有错误归因、习惯性分析、风格统计都围绕 Session 进行。
//!
//! 存储实现见 [`crate::session_store::SqliteSessionStore`]（api-server / desktop-app 共用）。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// 会话元数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub session_id: String,
    /// 语言
    pub lang: String,
    /// 状态：active / archived
    #[serde(default = "default_status")]
    pub status: String,
    /// ISO 8601 创建时间
    pub created_at: String,
    /// 学习者画像：母语（用户自报；LLM 讲解 prompt 注入用，区别于会话检测语言）
    #[serde(default)]
    pub native_language: Option<String>,
    /// 学习者画像：自评 CEFR 等级（A1-C2；讲解模板缺省档位）
    #[serde(default)]
    pub cefr: Option<String>,
}

fn default_status() -> String {
    "active".to_string()
}

impl SessionMeta {
    pub fn new(session_id: impl Into<String>, lang: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            lang: lang.into(),
            status: "active".into(),
            created_at: chrono::Utc::now().to_rfc3339(),
            native_language: None,
            cefr: None,
        }
    }
}

/// 单条犯错记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorHistoryEntry {
    pub lang: String,
    pub error_type: String,
    /// ISO 8601 时间戳
    pub occurred_at: String,
}

/// 会话完整记录（元数据 + 历史 + 错误类型分布）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    pub meta: SessionMeta,
    pub history: Vec<ErrorHistoryEntry>,
    /// 错误类型 -> 次数
    pub error_type_counts: HashMap<String, u32>,
}

impl SessionRecord {
    pub fn new(meta: SessionMeta) -> Self {
        Self {
            meta,
            history: Vec::new(),
            error_type_counts: HashMap::new(),
        }
    }

    pub fn total_errors(&self) -> u32 {
        self.history.len() as u32
    }

    /// 新手判定：历史犯错 <= 1 次
    pub fn is_novice(&self) -> bool {
        self.history.len() <= 1
    }
}

impl SessionRecord {
    pub fn record(
        &mut self,
        lang: impl Into<String>,
        error_type: impl Into<String>,
        occurred_at: impl Into<String>,
    ) {
        let et = error_type.into();
        self.history.push(ErrorHistoryEntry {
            lang: lang.into(),
            error_type: et.clone(),
            occurred_at: occurred_at.into(),
        });
        *self.error_type_counts.entry(et).or_insert(0) += 1;
    }
}

/// 错题本条目（Phase 2 学习扩展：用户确认「采纳」的错误）
///
/// 同 `error_type` + `original` + `correction` 聚合为一条，`count` 为出现次数，
/// `sentence` / `created_at` 取最近一次。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mistake {
    pub id: i64,
    pub session_id: String,
    pub lang: String,
    pub error_type: String,
    pub original: String,
    pub correction: Option<String>,
    /// 句级上下文（从原文按错误 span 提取；无原文时回退错误片段）
    pub sentence: String,
    /// 最近一次发生时间
    pub created_at: String,
    /// 出现次数（聚合）
    pub count: u32,
    /// 掌握度状态：`new`（未复习过）/ `reviewing`（复习中）/ `mastered`（已掌握）
    #[serde(default)]
    pub mastery_status: Option<String>,
    /// 下次复习时间（SQLite datetime 格式；已掌握为 None）
    #[serde(default)]
    pub next_review_at: Option<String>,
    /// 出现该错题的会话数（仅全局错题本视图填充；会话级视图为 None 不序列化）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sessions_seen: Option<u32>,
}

/// 从原文提取包含错误 span 的句子（按句末标点粗切；切出为空时回退 `fallback`）
///
/// `span` 为字节偏移（与 [`crate::error::Error::span`] 一致，非字符边界时自动吸附）。
pub fn sentence_around(src: &str, span: [usize; 2], fallback: &str) -> String {
    const TERMS: [char; 7] = ['。', '！', '？', '；', '\n', '.', '?'];
    let s = span[0].min(src.len());
    let mut e = span[1].min(src.len());
    while e < src.len() && !src.is_char_boundary(e) {
        e += 1;
    }
    let mut ls = 0usize;
    for (i, ch) in src.char_indices() {
        if i >= s {
            break;
        }
        if TERMS.contains(&ch) {
            ls = i + ch.len_utf8();
        }
    }
    let mut le = src.len();
    for (i, ch) in src[e..].char_indices() {
        if TERMS.contains(&ch) {
            le = e + i + ch.len_utf8();
            break;
        }
    }
    let sent = src[ls..le].trim();
    if sent.is_empty() {
        fallback.to_string()
    } else {
        sent.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_counts_and_novice() {
        let mut record = SessionRecord::new(SessionMeta::new("s_alice", "zh"));
        record.record("zh", "spelling", "2026-10-09T00:00:00Z");
        assert_eq!(record.total_errors(), 1);
        assert!(record.is_novice());
        assert_eq!(record.error_type_counts.get("spelling"), Some(&1));
        record.record("zh", "spelling", "2026-10-10T00:00:00Z");
        assert_eq!(record.total_errors(), 2);
        assert!(!record.is_novice());
    }

    #[test]
    fn meta_profile_fields_default_none() {
        let meta = SessionMeta::new("s_1", "zh");
        assert_eq!(meta.native_language, None);
        assert_eq!(meta.cefr, None);
    }

    #[test]
    fn sentence_around_zh_and_en() {
        let src = "第一句没有问题。这里有个错吴需要修正。后面也正常。";
        // 「错吴」的字节偏移
        let start = src.find("错吴").unwrap();
        let end = start + "错吴".len();
        let sent = sentence_around(src, [start, end], "错吴");
        assert_eq!(sent, "这里有个错吴需要修正。");

        let en = "This is fine. This on is wrong. End.";
        let start = en.find("on").unwrap();
        let sent = sentence_around(en, [start, start + 2], "on");
        assert_eq!(sent, "This on is wrong.");

        // 空原文 → 回退 fallback
        assert_eq!(sentence_around("", [0, 0], "frag"), "frag");
    }
}
