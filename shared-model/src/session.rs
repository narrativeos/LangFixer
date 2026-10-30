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
}
