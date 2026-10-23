//! 学习知识存储（向量召回用）
//!
//! 存储从 LLM 兜底采纳 / 用户手动录入的错误样本，
//! 附带文本嵌入向量用于相似度召回。
//! 数据持久化到 SQLite（与词表共用同一数据库文件）。

use std::path::Path;

use rusqlite::{Connection, Result as SqlResult, params};
use shared_model::LearnedKnowledge;

/// 学习知识存储
pub struct KnowledgeStore {
    conn: std::sync::Mutex<Connection>,
}

impl KnowledgeStore {
    /// 打开（或创建）知识存储
    pub fn open(path: &Path) -> SqlResult<Self> {
        let conn = Connection::open(path)?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS learned_knowledge (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                lang TEXT NOT NULL,
                text TEXT NOT NULL,
                error_type TEXT NOT NULL,
                correction TEXT NOT NULL DEFAULT '',
                embedding BLOB,
                source TEXT NOT NULL DEFAULT 'manual',
                created_at TEXT NOT NULL DEFAULT ''
            )",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_learned_knowledge_lang ON learned_knowledge(lang)",
            [],
        )?;
        ensure_knowledge_unique_index(&conn)?;
        Ok(Self {
            conn: std::sync::Mutex::new(conn),
        })
    }

    /// 内存模式（测试用）
    pub fn open_in_memory() -> SqlResult<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS learned_knowledge (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                lang TEXT NOT NULL,
                text TEXT NOT NULL,
                error_type TEXT NOT NULL,
                correction TEXT NOT NULL DEFAULT '',
                embedding BLOB,
                source TEXT NOT NULL DEFAULT 'manual',
                created_at TEXT NOT NULL DEFAULT ''
            )",
            [],
        )?;
        ensure_knowledge_unique_index(&conn)?;
        Ok(Self {
            conn: std::sync::Mutex::new(conn),
        })
    }

    /// 插入知识条目（含嵌入向量）
    ///
    /// 若已存在相同 `(lang, text, error_type)` 的条目则跳过写入
    ///（唯一约束兜底并发重复写）并返回 `0`；否则返回新行 id。
    pub fn insert(
        &self,
        lang: &str,
        text: &str,
        error_type: &str,
        correction: &str,
        embedding: &[f32],
        source: &str,
    ) -> SqlResult<i64> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let emb_bytes: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();
        conn.execute(
            "INSERT OR IGNORE INTO learned_knowledge (lang, text, error_type, correction, embedding, source, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![lang, text, error_type, correction, emb_bytes, source, now],
        )?;
        if conn.changes() == 0 {
            return Ok(0);
        }
        Ok(conn.last_insert_rowid())
    }

    /// 列出指定语言的所有知识条目
    pub fn list(&self, lang: &str) -> SqlResult<Vec<LearnedKnowledge>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT id, lang, text, error_type, correction, embedding, source, created_at
             FROM learned_knowledge WHERE lang = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![lang], |row| {
            let emb_blob: Vec<u8> = row.get(5)?;
            let embedding: Vec<f32> = emb_blob
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect();
            Ok(LearnedKnowledge {
                id: row.get(0)?,
                lang: row.get(1)?,
                text: row.get(2)?,
                error_type: row.get(3)?,
                correction: row.get(4)?,
                embedding,
                source: row.get(6)?,
                created_at: row.get(7)?,
            })
        })?;
        rows.collect()
    }

    /// 删除知识条目
    pub fn remove(&self, id: i64) -> SqlResult<bool> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let n = conn.execute("DELETE FROM learned_knowledge WHERE id = ?1", params![id])?;
        Ok(n > 0)
    }

    /// 向量召回：对指定查询向量，返回余弦相似度最高的 top_k 条目
    ///
    /// 单条查询入口；句级批量召回（②.5 知识召回）请改用 [`Self::recall_many`]
    /// （知识库只加载一次，避免每句一次全表读）。
    pub fn recall(
        &self,
        lang: &str,
        query_embedding: &[f32],
        top_k: usize,
        threshold: f64,
    ) -> SqlResult<Vec<(LearnedKnowledge, f64)>> {
        self.recall_many(
            lang,
            std::slice::from_ref(&query_embedding),
            top_k,
            threshold,
        )
        .map(|v| v.into_iter().next().unwrap_or_default())
    }

    /// 批量向量召回：知识库**只加载一次**（一次 `list`），
    /// 对每个查询向量在内存中做余弦匹配
    ///
    /// `query_embeddings` 按顺序对应返回的外层列表（每个内层列表是该向量的
    /// 相似度 ≥ `threshold` 的 top_k 条目，按相似度降序）。
    /// 句级召回（每句一个向量）走本方法：N 句 = 1 次全表读 + O(N×K) 余弦，
    /// 而非 N 次全表读（每次 `list` 都要解码全部 embedding BLOB）。
    pub fn recall_many(
        &self,
        lang: &str,
        query_embeddings: &[&[f32]],
        top_k: usize,
        threshold: f64,
    ) -> SqlResult<Vec<Vec<(LearnedKnowledge, f64)>>> {
        let items = self.list(lang)?;
        let mut out = Vec::with_capacity(query_embeddings.len());
        for q in query_embeddings {
            let mut scored: Vec<(LearnedKnowledge, f64)> = Vec::new();
            for item in &items {
                if item.embedding.len() != q.len() {
                    continue;
                }
                let sim = cosine_similarity(q, &item.embedding);
                if sim >= threshold {
                    scored.push((item.clone(), sim));
                }
            }
            scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            scored.truncate(top_k);
            out.push(scored);
        }
        Ok(out)
    }
}

/// 唯一约束 `(lang, text, error_type)`：并发 auto_adopt 的兜底
///（「查重 → 插入」竞态窗口内两个任务可能同时通过查重）；
/// 旧库若已有重复数据导致建索引失败，先按组去重（保留最新行）再建，
/// 保证约束恒生效
fn ensure_knowledge_unique_index(conn: &Connection) -> SqlResult<()> {
    const CREATE: &str = "CREATE UNIQUE INDEX IF NOT EXISTS uq_learned_knowledge_lang_text_type
        ON learned_knowledge (lang, text, error_type)";
    if conn.execute(CREATE, []).is_err() {
        conn.execute(
            "DELETE FROM learned_knowledge
             WHERE id NOT IN (SELECT MAX(id) FROM learned_knowledge
                               GROUP BY lang, text, error_type)",
            [],
        )?;
        conn.execute(CREATE, [])?;
    }
    Ok(())
}

/// 余弦相似度
fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let mag_a: f32 = a.iter().map(|x| x * x).sum();
    let mag_b: f32 = b.iter().map(|x| x * x).sum();
    let denom = (mag_a * mag_b).sqrt();
    if denom == 0.0 {
        0.0
    } else {
        (dot / denom) as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_list() {
        let store = KnowledgeStore::open_in_memory().unwrap();
        let emb = vec![0.1, 0.2, 0.3, 0.4];
        let id = store
            .insert(
                "en",
                "I saw a apple.",
                "article",
                "an apple",
                &emb,
                "manual",
            )
            .unwrap();
        assert!(id > 0);
        let items = store.list("en").unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, "I saw a apple.");
        assert_eq!(items[0].embedding, emb);
    }

    #[test]
    fn recall_by_similarity() {
        let store = KnowledgeStore::open_in_memory().unwrap();
        store
            .insert(
                "en",
                "I saw a apple.",
                "article",
                "an apple",
                &[0.9, 0.1, 0.0, 0.0],
                "llm_adopt",
            )
            .unwrap();
        store
            .insert(
                "en",
                "He go to school.",
                "tense",
                "goes",
                &[0.0, 0.0, 0.9, 0.1],
                "manual",
            )
            .unwrap();
        let results = store.recall("en", &[0.85, 0.15, 0.0, 0.0], 5, 0.5).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].0.text, "I saw a apple.");
        assert!(results[0].1 > 0.9);
    }

    #[test]
    fn remove_entry() {
        let store = KnowledgeStore::open_in_memory().unwrap();
        let id = store
            .insert("en", "test", "grammar", "fix", &[1.0, 0.0], "manual")
            .unwrap();
        assert!(store.remove(id).unwrap());
        assert!(store.list("en").unwrap().is_empty());
    }
}
