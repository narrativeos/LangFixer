use std::path::Path;
use std::sync::Arc;

use rusqlite::Connection;

use super::core::Core;
/// 自定义词（白名单）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CustomWord {
    pub id: i64,
    pub lang: String,
    pub word: String,
    #[serde(default)]
    pub reason: String,
    pub created_at: String,
}

/// 自定义词存储（SQLite 持久化 + 内存索引，与 api-server 同构）
pub struct CustomWordStore {
    conn: std::sync::Mutex<Connection>,
    index: std::sync::Mutex<std::collections::HashMap<String, std::collections::HashSet<String>>>,
}

impl CustomWordStore {
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
        let mut index: std::collections::HashMap<String, std::collections::HashSet<String>> =
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
            conn: std::sync::Mutex::new(conn),
            index: std::sync::Mutex::new(index),
        })
    }

    pub fn add(&self, lang: &str, word: &str, reason: &str) -> Result<CustomWord, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let created_at = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        conn.execute(
            "INSERT INTO custom_words (lang, word, reason, created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(lang, word) DO UPDATE SET reason = excluded.reason",
            rusqlite::params![lang, word, reason, created_at],
        )?;
        let id = conn
            .query_row(
                "SELECT id FROM custom_words WHERE lang = ?1 AND word = ?2",
                rusqlite::params![lang, word],
                |r| r.get(0),
            )
            .unwrap_or(0);
        self.index
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(lang.to_string())
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

    pub fn remove(&self, id: i64) -> Result<bool, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        // 先取 lang/word（删除后无法回查），再执行删除并同步内存索引
        let target = conn
            .query_row(
                "SELECT lang, word FROM custom_words WHERE id = ?1",
                [id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .ok();
        let n = conn.execute("DELETE FROM custom_words WHERE id = ?1", [id])?;
        if n > 0
            && let Some((lang, word)) = target
            && let Some(set) = self
                .index
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get_mut(&lang)
        {
            set.remove(&word);
        }
        Ok(n > 0)
    }

    pub fn list(&self, lang: &str) -> Result<Vec<CustomWord>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT id, lang, word, reason, created_at FROM custom_words WHERE lang = ?1 ORDER BY id",
        )?;
        let rows = stmt
            .query_map([lang], |r| {
                Ok(CustomWord {
                    id: r.get(0)?,
                    lang: r.get(1)?,
                    word: r.get(2)?,
                    reason: r.get(3)?,
                    created_at: r.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn is_whitelisted(&self, lang: &str, word: &str) -> bool {
        self.index
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(lang)
            .map(|s| s.contains(word))
            .unwrap_or(false)
    }
}

/// 添加自定义词（白名单）
#[tauri::command]
pub fn add_custom_word(
    core: tauri::State<'_, Arc<Core>>,
    lang: String,
    word: String,
    reason: Option<String>,
) -> Result<CustomWord, String> {
    core.custom_words
        .add(&lang, &word, &reason.unwrap_or_default())
        .map_err(|e| e.to_string())
}

/// 列出指定语言的自定义词
#[tauri::command]
pub fn list_custom_words(
    core: tauri::State<'_, Arc<Core>>,
    lang: String,
) -> Result<Vec<CustomWord>, String> {
    core.custom_words
        .list(&lang)
        .map_err(|e| format!("查询自定义词失败: {e}"))
}

/// 删除自定义词
#[tauri::command]
pub fn remove_custom_word(core: tauri::State<'_, Arc<Core>>, id: i64) -> Result<bool, String> {
    core.custom_words
        .remove(id)
        .map_err(|e| format!("删除自定义词失败: {e}"))
}
