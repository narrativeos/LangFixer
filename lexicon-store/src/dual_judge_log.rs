//! ③ 自动双判日志（`dual_judge_log` 表）
//!
//! 双判模式下每次 ③ 检测（全文兜底 / ③-T 定向复查 / 文档块）产生一行：
//! 主臂与影子臂各自检出的错误 JSON + 对照判定（verdict）。
//!
//! 用途：**臂间对照数据积累**——主臂（用户所选预设）与影子臂（另一端点）
//! 的命中差异是 KTO 错题本与 L3 教师批选样的直接来源；夜间批（T2）
//! 按 verdict=primary_only / shadow_only 抽样复核。
//!
//! 与反馈日志 / 词表共用同一 SQLite 文件（各建各表，互不干扰）。

use std::path::Path;

use rusqlite::{Connection, Result as SqlResult, params};
use serde::{Deserialize, Serialize};

/// 双判对照判定
pub mod verdict {
    /// 双臂均检出错误（命中是否重叠另看 json 明细）
    pub const BOTH: &str = "both";
    /// 仅主臂检出（影子臂沉默）
    pub const PRIMARY_ONLY: &str = "primary_only";
    /// 仅影子臂检出（主臂沉默）
    pub const SHADOW_ONLY: &str = "shadow_only";
    /// 双臂均沉默
    pub const NONE: &str = "none";
    /// 影子臂调用失败（端点不可达/超时）——主臂结果不受影响
    pub const SHADOW_ERROR: &str = "shadow_error";
    /// 影子臂响应成功但无法解析
    pub const SHADOW_PARSE_FAILED: &str = "shadow_parse_failed";
}

/// 双判日志行
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DualJudgeLog {
    pub id: i64,
    pub lang: String,
    /// 被检测文本（句子或块原文）
    pub text: String,
    pub primary_model: String,
    pub shadow_model: String,
    /// 主臂检出错误（Error 序列化 JSON 数组）
    pub primary_json: String,
    /// 影子臂检出错误（同上；失败时为 `[]`）
    pub shadow_json: String,
    /// 对照判定（见 [`verdict`]）
    pub verdict: String,
    pub created_at: String,
}

/// 双判日志存储
pub struct DualJudgeLogStore {
    conn: std::sync::Mutex<Connection>,
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS dual_judge_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    lang TEXT NOT NULL,
    text TEXT NOT NULL,
    primary_model TEXT NOT NULL,
    shadow_model TEXT NOT NULL,
    primary_json TEXT NOT NULL,
    shadow_json TEXT NOT NULL,
    verdict TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_dual_judge_verdict ON dual_judge_log (verdict, id);
CREATE INDEX IF NOT EXISTS idx_dual_judge_created ON dual_judge_log (created_at);
"#;

impl DualJudgeLogStore {
    /// 打开（或创建）双判日志存储（与词表 / 反馈日志共用同一 db 文件）
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

    /// 写入一条双判记录，返回新行 id
    #[allow(clippy::too_many_arguments)]
    pub fn insert(
        &self,
        lang: &str,
        text: &str,
        primary_model: &str,
        shadow_model: &str,
        primary_json: &str,
        shadow_json: &str,
        verdict: &str,
    ) -> SqlResult<i64> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "INSERT INTO dual_judge_log (lang, text, primary_model, shadow_model, primary_json, shadow_json, verdict)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![lang, text, primary_model, shadow_model, primary_json, shadow_json, verdict],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// 按 verdict 计数（观察面板用）
    pub fn verdict_counts(&self) -> SqlResult<Vec<(String, i64)>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT verdict, COUNT(*) FROM dual_judge_log GROUP BY verdict ORDER BY 2 DESC",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        rows.collect()
    }

    /// 列出最近双判记录（`limit` 上限 500）
    pub fn list(&self, limit: usize) -> SqlResult<Vec<DualJudgeLog>> {
        let limit = limit.clamp(1, 500);
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT id, lang, text, primary_model, shadow_model, primary_json, shadow_json, verdict, created_at
             FROM dual_judge_log ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |r| {
            Ok(DualJudgeLog {
                id: r.get(0)?,
                lang: r.get(1)?,
                text: r.get(2)?,
                primary_model: r.get(3)?,
                shadow_model: r.get(4)?,
                primary_json: r.get(5)?,
                shadow_json: r.get(6)?,
                verdict: r.get(7)?,
                created_at: r.get(8)?,
            })
        })?;
        rows.collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_list_and_counts() {
        let s = DualJudgeLogStore::open_in_memory().unwrap();
        s.insert(
            "zh",
            "测试句子",
            "gpt-4o-mini",
            "arm3-l2kto-v7",
            "[]",
            "[]",
            verdict::NONE,
        )
        .unwrap();
        s.insert(
            "zh",
            "另一句",
            "gpt-4o-mini",
            "arm3-l2kto-v7",
            "[{}]",
            "[]",
            verdict::PRIMARY_ONLY,
        )
        .unwrap();
        s.insert(
            "zh",
            "再一句",
            "gpt-4o-mini",
            "arm3-l2kto-v7",
            "[]",
            "[{}]",
            verdict::SHADOW_ONLY,
        )
        .unwrap();
        let mut counts: Vec<(String, i64)> = s.verdict_counts().unwrap();
        counts.sort();
        assert_eq!(
            counts,
            vec![
                ("none".to_string(), 1),
                ("primary_only".to_string(), 1),
                ("shadow_only".to_string(), 1),
            ]
        );
        let rows = s.list(10).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].verdict, verdict::SHADOW_ONLY, "最新在前");
    }
}
