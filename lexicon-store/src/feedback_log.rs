//! 反馈日志存储（`feedback_log` 表）
//!
//! 记录用户对检测结果（任意来源层：rule / knowledge / decision / feature /
//! statistical / llm）的「采纳 / 放弃」操作。
//!
//! 两个用途：
//! 1. **反思流程的数据基础**：「某规则的历史采纳/拒绝统计」是诊断规则质量
//!    （误报率偏高 → 建议停用/收紧）的核心依据；
//! 2. **反馈历史面板**：可视化各规则/层的采纳/放弃积累，观察规则质量趋势。
//!
//! 与词表 / 学习知识共用同一 SQLite 文件（各建各表，互不干扰）。

use std::path::Path;

use rusqlite::{Connection, Result as SqlResult, params};
use serde::{Deserialize, Serialize};

/// 反馈日志行
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackLog {
    pub id: i64,
    pub lang: String,
    /// 错误来源层（`rule` / `knowledge` / `decision` / `feature` / `statistical` / `llm`）
    pub source: String,
    /// 关联规则 id（规则层来源时有值）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_id: Option<String>,
    pub error_type: String,
    /// 被反馈的原文片段
    pub original: String,
    /// 建议修正（无修正时为空）
    #[serde(default)]
    pub correction: String,
    /// 操作：`adopt`（采纳）/ `reject`（放弃）
    pub action: String,
    /// 用户可选的放弃原因
    #[serde(default)]
    pub reason: String,
    pub created_at: String,
}

/// 按（来源层, 规则 id）聚合的反馈统计
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackStats {
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_id: Option<String>,
    pub adopted: i64,
    pub rejected: i64,
    /// 最近一次反馈时间
    pub last_at: String,
}

/// 反馈日志存储
pub struct FeedbackLogStore {
    conn: std::sync::Mutex<Connection>,
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS feedback_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    lang TEXT NOT NULL,
    source TEXT NOT NULL,
    rule_id TEXT,
    error_type TEXT NOT NULL,
    original TEXT NOT NULL,
    correction TEXT NOT NULL DEFAULT '',
    action TEXT NOT NULL,
    reason TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_feedback_log_lang ON feedback_log (lang);
CREATE INDEX IF NOT EXISTS idx_feedback_log_rule ON feedback_log (lang, rule_id);
"#;

impl FeedbackLogStore {
    /// 打开（或创建）反馈日志存储（与词表 / 学习知识共用同一 db 文件）
    pub fn open(path: &Path) -> SqlResult<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: std::sync::Mutex::new(conn),
        })
    }

    /// 内存模式（测试用）
    pub fn open_in_memory() -> SqlResult<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: std::sync::Mutex::new(conn),
        })
    }

    /// 写入一条反馈，返回新行 id
    #[allow(clippy::too_many_arguments)]
    pub fn insert(
        &self,
        lang: &str,
        source: &str,
        rule_id: Option<&str>,
        error_type: &str,
        original: &str,
        correction: &str,
        action: &str,
        reason: &str,
    ) -> SqlResult<i64> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "INSERT INTO feedback_log (lang, source, rule_id, error_type, original, correction, action, reason)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![lang, source, rule_id, error_type, original, correction, action, reason],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// 列出最近反馈（`lang` 为 None 时不限语言；`limit` 上限 500）
    pub fn list(&self, lang: Option<&str>, limit: usize) -> SqlResult<Vec<FeedbackLog>> {
        let limit = limit.clamp(1, 500);
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let sql = if lang.is_some() {
            "SELECT id, lang, source, rule_id, error_type, original, correction, action, reason, created_at
             FROM feedback_log WHERE lang = ?1 ORDER BY id DESC LIMIT ?2"
        } else {
            "SELECT id, lang, source, rule_id, error_type, original, correction, action, reason, created_at
             FROM feedback_log ORDER BY id DESC LIMIT ?1"
        };
        let mut stmt = conn.prepare(sql)?;
        let rows = if let Some(lang) = lang {
            stmt.query_map(params![lang, limit as i64], map_row)?
        } else {
            stmt.query_map(params![limit as i64], map_row)?
        };
        rows.collect()
    }

    /// 按（来源层, 规则 id）聚合统计（反馈历史面板 + 反思诊断用）
    ///
    /// 规则层按 `rule_id` 细分；其余层 `rule_id` 为 NULL，按层聚合。
    pub fn stats(&self, lang: &str) -> SqlResult<Vec<FeedbackStats>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT source,
                    rule_id,
                    SUM(CASE WHEN action = 'adopt' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN action = 'reject' THEN 1 ELSE 0 END),
                    MAX(created_at)
             FROM feedback_log WHERE lang = ?1
             GROUP BY source, rule_id
             ORDER BY (SUM(CASE WHEN action = 'reject' THEN 1 ELSE 0 END)) DESC, source",
        )?;
        let rows = stmt.query_map(params![lang], |r| {
            Ok(FeedbackStats {
                source: r.get(0)?,
                rule_id: r.get(1)?,
                adopted: r.get::<_, i64>(2)?,
                rejected: r.get::<_, i64>(3)?,
                last_at: r.get(4)?,
            })
        })?;
        rows.collect()
    }

    /// 按（来源层, 规则 id）聚合统计（**时间窗口**：`created_at > since`；
    /// 进化中心 P3 优化轮次用——只统计上次轮次之后的反馈增量）
    ///
    /// `since` 为 `None` 时等价于 [`stats`]。
    pub fn stats_since(&self, lang: &str, since: Option<&str>) -> SqlResult<Vec<FeedbackStats>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let sql = if since.is_some() {
            "SELECT source,\n                    rule_id,\n                    SUM(CASE WHEN action = 'adopt' THEN 1 ELSE 0 END),\n                    SUM(CASE WHEN action = 'reject' THEN 1 ELSE 0 END),\n                    MAX(created_at)\n             FROM feedback_log WHERE lang = ?1 AND created_at > ?2\n             GROUP BY source, rule_id\n             ORDER BY (SUM(CASE WHEN action = 'reject' THEN 1 ELSE 0 END)) DESC, source"
        } else {
            "SELECT source,\n                    rule_id,\n                    SUM(CASE WHEN action = 'adopt' THEN 1 ELSE 0 END),\n                    SUM(CASE WHEN action = 'reject' THEN 1 ELSE 0 END),\n                    MAX(created_at)\n             FROM feedback_log WHERE lang = ?1\n             GROUP BY source, rule_id\n             ORDER BY (SUM(CASE WHEN action = 'reject' THEN 1 ELSE 0 END)) DESC, source"
        };
        let mut stmt = conn.prepare(sql)?;
        let mut p: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        p.push(Box::new(lang.to_string()));
        if let Some(s) = since {
            p.push(Box::new(s.to_string()));
        }
        let rows = stmt.query_map(rusqlite::params_from_iter(p.iter()), |r| {
            Ok(FeedbackStats {
                source: r.get(0)?,
                rule_id: r.get(1)?,
                adopted: r.get::<_, i64>(2)?,
                rejected: r.get::<_, i64>(3)?,
                last_at: r.get(4)?,
            })
        })?;
        rows.collect()
    }

    /// 反馈明细（**时间窗口**：`created_at > since`；`since` 为 `None` 时等价于 [`list`]）
    pub fn list_since(
        &self,
        lang: Option<&str>,
        since: Option<&str>,
        limit: usize,
    ) -> SqlResult<Vec<FeedbackLog>> {
        let limit = limit.clamp(1, 500);
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let sql = if lang.is_some() && since.is_some() {
            "SELECT id, lang, source, rule_id, error_type, original, correction, action, reason, created_at\n             FROM feedback_log WHERE lang = ?1 AND created_at > ?2 ORDER BY id DESC LIMIT ?3"
        } else if lang.is_some() {
            "SELECT id, lang, source, rule_id, error_type, original, correction, action, reason, created_at\n             FROM feedback_log WHERE lang = ?1 ORDER BY id DESC LIMIT ?2"
        } else if since.is_some() {
            "SELECT id, lang, source, rule_id, error_type, original, correction, action, reason, created_at\n             FROM feedback_log WHERE created_at > ?1 ORDER BY id DESC LIMIT ?2"
        } else {
            "SELECT id, lang, source, rule_id, error_type, original, correction, action, reason, created_at\n             FROM feedback_log ORDER BY id DESC LIMIT ?1"
        };
        let mut stmt = conn.prepare(sql)?;
        let mut p: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(l) = lang {
            p.push(Box::new(l.to_string()));
        }
        if let Some(s) = since {
            p.push(Box::new(s.to_string()));
        }
        p.push(Box::new(limit));
        let rows = stmt.query_map(rusqlite::params_from_iter(p.iter()), map_row)?;
        rows.collect()
    }

    /// 单规则反馈统计 `(采纳数, 拒绝数)`（反思诊断用；规则无反馈时 `(0, 0)`）
    pub fn stats_by_rule(&self, lang: &str, rule_id: &str) -> SqlResult<(i64, i64)> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let (adopted, rejected): (i64, i64) = conn
            .query_row(
                "SELECT SUM(CASE WHEN action = 'adopt' THEN 1 ELSE 0 END),
                        SUM(CASE WHEN action = 'reject' THEN 1 ELSE 0 END)
                 FROM feedback_log WHERE lang = ?1 AND rule_id = ?2",
                params![lang, rule_id],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
            )
            .unwrap_or((0, 0));
        Ok((adopted, rejected))
    }

    /// 单规则反馈统计（**按 original 文本去重**）`(采纳文本数, 拒绝文本数)`
    ///
    /// 反思诊断专用：同一文本被反复放弃（如多次重检测同一篇文档）只算 1 个
    /// 独立证据，避免「1 个文本点 4 次放弃」被放大成「4 次独立拒绝」而误触
    /// 高误报阈值。
    pub fn stats_by_rule_distinct(&self, lang: &str, rule_id: &str) -> SqlResult<(i64, i64)> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let (adopted, rejected): (i64, i64) = conn
            .query_row(
                "SELECT COUNT(DISTINCT CASE WHEN action = 'adopt' THEN original END),
                        COUNT(DISTINCT CASE WHEN action = 'reject' THEN original END)
                 FROM feedback_log WHERE lang = ?1 AND rule_id = ?2",
                params![lang, rule_id],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
            )
            .unwrap_or((0, 0));
        Ok((adopted, rejected))
    }
}

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<FeedbackLog> {
    Ok(FeedbackLog {
        id: row.get(0)?,
        lang: row.get(1)?,
        source: row.get(2)?,
        rule_id: row.get(3)?,
        error_type: row.get(4)?,
        original: row.get(5)?,
        correction: row.get(6)?,
        action: row.get(7)?,
        reason: row.get(8)?,
        created_at: row.get(9)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_list() {
        let store = FeedbackLogStore::open_in_memory().unwrap();
        let id = store
            .insert(
                "zh",
                "rule",
                Some("ZH_SPELL"),
                "spelling",
                "报复",
                "抱负",
                "adopt",
                "",
            )
            .unwrap();
        assert!(id > 0);
        store
            .insert(
                "zh",
                "rule",
                Some("ZH_SPELL"),
                "spelling",
                "报复",
                "抱负",
                "reject",
                "上下文误判",
            )
            .unwrap();
        let all = store.list(None, 10).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].action, "reject");
        assert_eq!(all[0].reason, "上下文误判");
        // 语言过滤
        store
            .insert(
                "en", "llm", None, "article", "a apple", "an apple", "adopt", "",
            )
            .unwrap();
        assert_eq!(store.list(Some("zh"), 10).unwrap().len(), 2);
        assert_eq!(store.list(Some("en"), 10).unwrap().len(), 1);
    }

    #[test]
    fn stats_aggregate() {
        let store = FeedbackLogStore::open_in_memory().unwrap();
        store
            .insert(
                "zh",
                "rule",
                Some("ZH_SPELL"),
                "spelling",
                "a",
                "b",
                "adopt",
                "",
            )
            .unwrap();
        store
            .insert(
                "zh",
                "rule",
                Some("ZH_SPELL"),
                "spelling",
                "c",
                "d",
                "reject",
                "",
            )
            .unwrap();
        store
            .insert(
                "zh",
                "rule",
                Some("ZH_SPELL"),
                "spelling",
                "e",
                "f",
                "reject",
                "",
            )
            .unwrap();
        store
            .insert("zh", "decision", None, "grammar", "g", "h", "reject", "")
            .unwrap();
        let stats = store.stats("zh").unwrap();
        assert_eq!(stats.len(), 2);
        let rule = stats
            .iter()
            .find(|s| s.rule_id.as_deref() == Some("ZH_SPELL"))
            .unwrap();
        assert_eq!(rule.adopted, 1);
        assert_eq!(rule.rejected, 2);
        let dec = stats.iter().find(|s| s.source == "decision").unwrap();
        assert!(dec.rule_id.is_none());
        assert_eq!(dec.rejected, 1);
    }

    #[test]
    fn stats_by_rule() {
        let store = FeedbackLogStore::open_in_memory().unwrap();
        store
            .insert("zh", "rule", Some("R1"), "spelling", "a", "b", "adopt", "")
            .unwrap();
        store
            .insert("zh", "rule", Some("R1"), "spelling", "c", "d", "reject", "")
            .unwrap();
        store
            .insert("zh", "rule", Some("R2"), "spelling", "e", "f", "reject", "")
            .unwrap();
        assert_eq!(store.stats_by_rule("zh", "R1").unwrap(), (1, 1));
        assert_eq!(store.stats_by_rule("zh", "R2").unwrap(), (0, 1));
        assert_eq!(store.stats_by_rule("zh", "NOPE").unwrap(), (0, 0));
    }

    #[test]
    fn stats_by_rule_distinct_dedups_repeated_originals() {
        let store = FeedbackLogStore::open_in_memory().unwrap();
        // 同一文本「a」被反复放弃 4 次（模拟多次重检测同一篇文档）
        for _ in 0..4 {
            store
                .insert("zh", "rule", Some("R1"), "grammar", "a", "", "reject", "")
                .unwrap();
        }
        // 另一文本「c」放弃 1 次 + 采纳 1 次
        store
            .insert("zh", "rule", Some("R1"), "grammar", "c", "", "reject", "")
            .unwrap();
        store
            .insert("zh", "rule", Some("R1"), "grammar", "c", "d", "adopt", "")
            .unwrap();
        // 原始行数：5 行（4 拒绝 + 1 采纳）
        assert_eq!(store.stats_by_rule("zh", "R1").unwrap(), (1, 5));
        // 去重后：1 个被采纳文本 + 2 个被拒绝文本（「a」「c」）
        assert_eq!(store.stats_by_rule_distinct("zh", "R1").unwrap(), (1, 2));
        assert_eq!(store.stats_by_rule_distinct("zh", "NOPE").unwrap(), (0, 0));
        #[test]
        fn stats_since_window() {
            let dir = std::env::temp_dir().join(format!("lf_fb_since_{}", std::process::id()));
            let _ = std::fs::remove_file(&dir);
            let store = FeedbackLogStore::open(&dir).unwrap();
            store
                .insert("zh", "feature", None, "grammar", "句子一", "", "reject", "")
                .unwrap();
            store
                .insert("zh", "feature", None, "grammar", "句子二", "", "reject", "")
                .unwrap();
            store
                .insert(
                    "zh",
                    "llm",
                    None,
                    "grammar",
                    "句子三",
                    "修正三",
                    "adopt",
                    "",
                )
                .unwrap();
            // 第一行回拨到更早时间（模拟「上次轮次之前」的旧反馈）
            {
                let conn = rusqlite::Connection::open(&dir).unwrap();
                conn.execute(
                "UPDATE feedback_log SET created_at = '2020-01-01 00:00:00' WHERE original = '句子一'",
                [],
            )
            .unwrap();
            }
            // since = None → 全量（与 stats 一致）
            let all = store.stats_since("zh", None).unwrap();
            assert_eq!(
                all.iter().find(|s| s.source == "feature").unwrap().rejected,
                2
            );
            // 时间窗口：只统计 2025 年之后（第一行被排除）
            let since = store
                .stats_since("zh", Some("2025-01-01 00:00:00"))
                .unwrap();
            assert_eq!(
                since
                    .iter()
                    .find(|s| s.source == "feature")
                    .unwrap()
                    .rejected,
                1
            );
            assert_eq!(since.iter().find(|s| s.source == "llm").unwrap().adopted, 1);
            // 明细窗口
            let list = store
                .list_since(Some("zh"), Some("2025-01-01 00:00:00"), 100)
                .unwrap();
            assert_eq!(list.len(), 2);
            let _ = std::fs::remove_file(&dir);
        }
    }
}
