//! 会话（Session）模型
//!
//! 一次检测会话 = 一份卷子 / 一份书稿的完整检测周期。
//! 所有错误归因、习惯性分析、风格统计都围绕 Session 进行。

use std::collections::HashMap;
use std::sync::RwLock;

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

/// 会话存储（内存实现；后续可替换为持久化）
#[derive(Debug, Default)]
pub struct SessionStore {
    records: RwLock<HashMap<String, SessionRecord>>,
}

impl SessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, session_id: &str) -> Option<SessionRecord> {
        self.records.read().unwrap().get(session_id).cloned()
    }

    /// 获取或创建会话
    pub fn get_or_create(&self, session_id: &str, lang: &str) -> SessionRecord {
        let mut map = self.records.write().unwrap();
        map.entry(session_id.to_string())
            .or_insert_with(|| SessionRecord::new(SessionMeta::new(session_id, lang)))
            .clone()
    }

    /// 记录一次犯错，返回最新档案
    pub fn record_error(
        &self,
        session_id: &str,
        lang: &str,
        error_type: &str,
        occurred_at: &str,
    ) -> SessionRecord {
        let mut map = self.records.write().unwrap();
        let record = map
            .entry(session_id.to_string())
            .or_insert_with(|| SessionRecord::new(SessionMeta::new(session_id, lang)));
        record.record(lang, error_type, occurred_at);
        record.clone()
    }

    /// 归档会话
    pub fn archive(&self, session_id: &str) -> bool {
        let mut map = self.records.write().unwrap();
        if let Some(record) = map.get_mut(session_id) {
            record.meta.status = "archived".into();
            true
        } else {
            false
        }
    }

    /// 恢复会话
    pub fn restore(&self, session_id: &str) -> bool {
        let mut map = self.records.write().unwrap();
        if let Some(record) = map.get_mut(session_id) {
            record.meta.status = "active".into();
            true
        } else {
            false
        }
    }

    /// 删除会话
    pub fn remove(&self, session_id: &str) -> bool {
        self.records.write().unwrap().remove(session_id).is_some()
    }

    /// 清空全部会话，返回删除数量
    pub fn clear(&self) -> usize {
        self.records.write().unwrap().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_records_and_counts() {
        let store = SessionStore::new();
        let record = store.record_error("s_alice", "zh", "spelling", "2026-10-09T00:00:00Z");
        assert_eq!(record.total_errors(), 1);
        assert!(record.is_novice());
        assert_eq!(record.error_type_counts.get("spelling"), Some(&1));

        let record = store.record_error("s_alice", "zh", "spelling", "2026-10-10T00:00:00Z");
        assert_eq!(record.total_errors(), 2);
        assert!(!record.is_novice());
    }

    #[test]
    fn archive_and_restore() {
        let store = SessionStore::new();
        let _ = store.get_or_create("s_1", "zh");
        assert!(store.archive("s_1"));
        let r = store.get("s_1").unwrap();
        assert_eq!(r.meta.status, "archived");
        assert!(store.restore("s_1"));
        let r = store.get("s_1").unwrap();
        assert_eq!(r.meta.status, "active");
    }
}
