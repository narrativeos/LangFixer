//! 用户自定义词表（白名单）+ SQLite 持久化
//!
//! 用户可添加自定义词/短语，检测时自动排除匹配的错误。
//! 数据持久化到 SQLite（`langfixer.db`），重启不丢失。

use std::collections::HashSet;
use std::path::Path;
use std::sync::Mutex;

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

/// 自定义词条
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomWord {
    pub id: i64,
    pub lang: String,
    pub word: String,
    #[serde(default)]
    pub reason: String,
    pub created_at: String,
}

/// 自定义词存储（SQLite 持久化 + 内存索引）
pub struct CustomWordStore {
    conn: Mutex<Connection>,
    /// lang → HashSet<word>（内存索引，加速检测时查询）
    index: Mutex<std::collections::HashMap<String, HashSet<String>>>,
}

impl CustomWordStore {
    /// 从 SQLite 文件加载（不存在则创建）
    pub fn open(path: &Path) -> Result<Self, rusqlite::Error> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS custom_words (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                lang TEXT NOT NULL,
                word TEXT NOT NULL,
                reason TEXT DEFAULT '',
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                UNIQUE(lang, word)
            );
            CREATE INDEX IF NOT EXISTS idx_custom_words_lang ON custom_words(lang);",
        )?;

        // 构建内存索引
        let mut index: std::collections::HashMap<String, HashSet<String>> =
            std::collections::HashMap::new();
        {
            let mut stmt = conn.prepare("SELECT lang, word FROM custom_words")?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (lang, word) = row?;
                index.entry(lang).or_default().insert(word);
            }
        }

        Ok(Self {
            conn: Mutex::new(conn),
            index: Mutex::new(index),
        })
    }

    /// 添加自定义词
    pub fn add(&self, lang: &str, word: &str, reason: &str) -> Result<CustomWord, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let created_at = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        conn.execute(
            "INSERT OR IGNORE INTO custom_words (lang, word, reason, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![lang, word, reason, created_at],
        )?;
        let id = conn
            .query_row(
                "SELECT id FROM custom_words WHERE lang = ?1 AND word = ?2",
                params![lang, word],
                |row| row.get(0),
            )
            .unwrap_or(0);

        // 更新内存索引
        let mut idx = self.index.lock().unwrap_or_else(|e| e.into_inner());
        idx.entry(lang.to_string())
            .or_default()
            .insert(word.to_string());

        Ok(CustomWord {
            id,
            lang: lang.to_string(),
            word: word.to_string(),
            reason: reason.to_string(),
            created_at,
        })
    }

    /// 删除自定义词
    pub fn remove(&self, id: i64) -> Result<bool, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        // 先查出 lang/word 以便更新索引
        let (lang, word) = conn
            .query_row(
                "SELECT lang, word FROM custom_words WHERE id = ?1",
                params![id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .unwrap_or_default();
        let n = conn.execute("DELETE FROM custom_words WHERE id = ?1", params![id])?;

        if n > 0 {
            let mut idx = self.index.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(set) = idx.get_mut(&lang) {
                set.remove(&word);
            }
        }
        Ok(n > 0)
    }

    /// 列出指定语言的所有自定义词
    pub fn list(&self, lang: &str) -> Result<Vec<CustomWord>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare("SELECT id, lang, word, reason, created_at FROM custom_words WHERE lang = ?1 ORDER BY word")?;
        let rows = stmt
            .query_map(params![lang], |row| {
                Ok(CustomWord {
                    id: row.get(0)?,
                    lang: row.get(1)?,
                    word: row.get(2)?,
                    reason: row.get(3)?,
                    created_at: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// 检查某个词是否在白名单中（检测时调用）
    pub fn is_whitelisted(&self, lang: &str, word: &str) -> bool {
        let idx = self.index.lock().unwrap_or_else(|e| e.into_inner());
        idx.get(lang).map(|set| set.contains(word)).unwrap_or(false)
    }
}
