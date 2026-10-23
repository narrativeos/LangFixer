//! 决策表版本历史（SQLite）
//!
//! 每次 `PUT /gec/tables/:lang` 热更新成功后，把该版本完整 JDM JSON 存入
//! `table_versions` 表，支持列表查看与一键回滚（rollback 即把历史版本内容
//! 重新走一次 `TableStore::update` 校验 + 原子替换）。

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

/// 一条决策表版本记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableVersion {
    pub id: i64,
    pub lang: String,
    pub saved_at: String,
    pub rule_count: usize,
    /// 该版本完整 JDM JSON
    pub content: String,
}

/// 版本列表项（不含 content，避免列表响应过大）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableVersionMeta {
    pub id: i64,
    pub lang: String,
    pub saved_at: String,
    pub rule_count: usize,
}

/// 决策表版本存储
pub struct TableHistoryStore {
    conn: Mutex<Connection>,
}

impl TableHistoryStore {
    /// 从 SQLite 文件打开（不存在则创建）
    pub fn open(path: &Path) -> Result<Self, rusqlite::Error> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS table_versions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                lang TEXT NOT NULL,
                saved_at TEXT NOT NULL,
                rule_count INTEGER NOT NULL DEFAULT 0,
                content TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_table_versions_lang ON table_versions(lang, id DESC);",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// 保存一个版本，返回其 id
    pub fn save(
        &self,
        lang: &str,
        rule_count: usize,
        content: &str,
    ) -> Result<i64, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "INSERT INTO table_versions (lang, saved_at, rule_count, content) VALUES (?1, datetime('now'), ?2, ?3)",
            params![lang, rule_count, content],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// 列出某语言的版本（新 → 旧，最多 `limit` 条）
    pub fn list(&self, lang: &str, limit: usize) -> Result<Vec<TableVersionMeta>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT id, lang, saved_at, rule_count FROM table_versions WHERE lang = ?1 ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![lang, limit as i64], |r| {
                Ok(TableVersionMeta {
                    id: r.get(0)?,
                    lang: r.get(1)?,
                    saved_at: r.get(2)?,
                    rule_count: r.get::<_, i64>(3)? as usize,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// 读取某版本的完整内容
    pub fn get(&self, id: i64) -> Result<Option<TableVersion>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT id, lang, saved_at, rule_count, content FROM table_versions WHERE id = ?1",
        )?;
        let mut iter = stmt.query_map(params![id], |r| {
            Ok(TableVersion {
                id: r.get(0)?,
                lang: r.get(1)?,
                saved_at: r.get(2)?,
                rule_count: r.get::<_, i64>(3)? as usize,
                content: r.get(4)?,
            })
        })?;
        match iter.next() {
            Some(Ok(v)) => Ok(Some(v)),
            Some(Err(e)) => Err(e),
            None => Ok(None),
        }
    }
}
