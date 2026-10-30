//! 会话存储（SQLite 持久化实现）
//!
//! 与内存实现 [`crate::session::SessionStore`] 同构的 API（get / get_or_create /
//! record_error / set_profile / archive / restore / remove / clear），
//! api-server 与 desktop-app 共用：会话与学习者画像（母语 / 自评 CEFR）跨重启存活。
//!
//! 表结构：`sessions`（元数据 + 画像）+ `session_history`（犯错流水，级联删除）。

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{Connection, params};

use crate::session::{ErrorHistoryEntry, SessionMeta, SessionRecord};

/// 会话存储（SQLite 持久化；`Mutex<Connection>` 与 detect_history 等存储同构）
pub struct SqliteSessionStore {
    conn: Mutex<Connection>,
}

impl SqliteSessionStore {
    /// 从 SQLite 文件打开（不存在则创建；与 detect_history 等共用 db 文件）
    pub fn open(path: &Path) -> Result<Self, rusqlite::Error> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                session_id TEXT PRIMARY KEY,
                lang TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'active',
                native_language TEXT,
                cefr TEXT,
                created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS session_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                lang TEXT NOT NULL,
                error_type TEXT NOT NULL,
                occurred_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_session_history_session ON session_history(session_id);",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// 读取会话完整记录（不存在返回 None）
    fn load_record(&self, session_id: &str) -> Option<SessionRecord> {
        let conn = self.conn.lock().unwrap();
        let meta: Option<SessionMeta> = conn
            .query_row(
                "SELECT session_id, lang, status, native_language, cefr, created_at
                 FROM sessions WHERE session_id = ?",
                params![session_id],
                |r| {
                    Ok(SessionMeta {
                        session_id: r.get(0)?,
                        lang: r.get(1)?,
                        status: r.get(2)?,
                        native_language: r.get(3)?,
                        cefr: r.get(4)?,
                        created_at: r.get(5)?,
                    })
                },
            )
            .ok();
        let meta = meta?;
        let history: Vec<ErrorHistoryEntry> = conn
            .prepare(
                "SELECT lang, error_type, occurred_at FROM session_history
                 WHERE session_id = ? ORDER BY id",
            )
            .and_then(|mut stmt| {
                let rows = stmt.query_map(params![session_id], |r| {
                    Ok(ErrorHistoryEntry {
                        lang: r.get(0)?,
                        error_type: r.get(1)?,
                        occurred_at: r.get(2)?,
                    })
                })?;
                Ok(rows.filter_map(|row| row.ok()).collect::<Vec<_>>())
            })
            .unwrap_or_default();
        let mut record = SessionRecord::new(meta);
        for e in &history {
            *record
                .error_type_counts
                .entry(e.error_type.clone())
                .or_insert(0) += 1;
        }
        record.history = history;
        Some(record)
    }

    /// 确保会话行存在（INSERT OR IGNORE；与内存实现 get_or_create 建档语义一致）
    fn ensure_row(&self, session_id: &str, lang: &str) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "INSERT OR IGNORE INTO sessions (session_id, lang, created_at) VALUES (?, ?, ?)",
            params![session_id, lang, chrono::Utc::now().to_rfc3339()],
        );
    }

    pub fn get(&self, session_id: &str) -> Option<SessionRecord> {
        self.load_record(session_id)
    }

    /// 获取或创建会话
    pub fn get_or_create(&self, session_id: &str, lang: &str) -> SessionRecord {
        self.ensure_row(session_id, lang);
        self.load_record(session_id)
            .unwrap_or_else(|| SessionRecord::new(SessionMeta::new(session_id, lang)))
    }

    /// 记录一次犯错，返回最新档案
    pub fn record_error(
        &self,
        session_id: &str,
        lang: &str,
        error_type: &str,
        occurred_at: &str,
    ) -> SessionRecord {
        self.ensure_row(session_id, lang);
        {
            let conn = self.conn.lock().unwrap();
            let _ = conn.execute(
                "INSERT INTO session_history (session_id, lang, error_type, occurred_at)
                 VALUES (?, ?, ?, ?)",
                params![session_id, lang, error_type, occurred_at],
            );
        }
        self.get_or_create(session_id, lang)
    }

    /// 更新学习者画像（母语 / 自评 CEFR）；None / 空串字段保持原值（部分更新），
    /// 会话不存在时按 lang 创建
    pub fn set_profile(
        &self,
        session_id: &str,
        lang: &str,
        native_language: Option<String>,
        cefr: Option<String>,
    ) -> SessionRecord {
        self.ensure_row(session_id, lang);
        {
            let conn = self.conn.lock().unwrap();
            if let Some(n) = native_language.filter(|s| !s.trim().is_empty()) {
                let _ = conn.execute(
                    "UPDATE sessions SET native_language = ? WHERE session_id = ?",
                    params![n, session_id],
                );
            }
            if let Some(c) = cefr.filter(|s| !s.trim().is_empty()) {
                let _ = conn.execute(
                    "UPDATE sessions SET cefr = ? WHERE session_id = ?",
                    params![c, session_id],
                );
            }
        }
        self.get_or_create(session_id, lang)
    }

    fn set_status(&self, session_id: &str, status: &str) -> bool {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE sessions SET status = ? WHERE session_id = ?",
            params![status, session_id],
        )
        .map(|n| n > 0)
        .unwrap_or(false)
    }

    /// 归档会话
    pub fn archive(&self, session_id: &str) -> bool {
        self.set_status(session_id, "archived")
    }

    /// 恢复会话
    pub fn restore(&self, session_id: &str) -> bool {
        self.set_status(session_id, "active")
    }

    /// 删除会话（级联删除犯错流水）
    pub fn remove(&self, session_id: &str) -> bool {
        let conn = self.conn.lock().unwrap();
        let ok = conn
            .execute(
                "DELETE FROM sessions WHERE session_id = ?",
                params![session_id],
            )
            .map(|n| n > 0)
            .unwrap_or(false);
        let _ = conn.execute(
            "DELETE FROM session_history WHERE session_id = ?",
            params![session_id],
        );
        ok
    }

    /// 清空全部会话，返回删除数量
    pub fn clear(&self) -> usize {
        let conn = self.conn.lock().unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
            .unwrap_or(0);
        let _ = conn.execute("DELETE FROM session_history", []);
        let _ = conn.execute("DELETE FROM sessions", []);
        n.max(0) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_store(tag: &str) -> (SqliteSessionStore, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "langfixer_session_store_test_{}_{}.db",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        (SqliteSessionStore::open(&path).unwrap(), path)
    }

    #[test]
    fn profile_and_history_survive_reopen() {
        let (store, path) = tmp_store("reopen");
        let r = store.set_profile("s_a", "zh", Some("中文".into()), Some("B1".into()));
        assert_eq!(r.meta.native_language.as_deref(), Some("中文"));
        store.record_error("s_a", "zh", "spelling", "2026-10-01T00:00:00Z");
        store.record_error("s_a", "zh", "spelling", "2026-10-02T00:00:00Z");
        drop(store);
        // 重新打开同一 db：画像与犯错流水都在（Phase 1.5 核心断言：重启不丢档案）
        let store = SqliteSessionStore::open(&path).unwrap();
        let r = store.get("s_a").unwrap();
        assert_eq!(r.meta.native_language.as_deref(), Some("中文"));
        assert_eq!(r.meta.cefr.as_deref(), Some("B1"));
        assert_eq!(r.total_errors(), 2);
        assert_eq!(r.error_type_counts.get("spelling"), Some(&2));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn archive_remove_clear() {
        let (store, path) = tmp_store("arc");
        let _ = store.get_or_create("s_b", "en");
        assert!(store.archive("s_b"));
        assert_eq!(store.get("s_b").unwrap().meta.status, "archived");
        assert!(store.remove("s_b"));
        assert!(store.get("s_b").is_none());
        let _ = store.get_or_create("s_c", "en");
        assert_eq!(store.clear(), 1);
        assert!(store.get("s_c").is_none());
        let _ = std::fs::remove_file(&path);
    }
}
