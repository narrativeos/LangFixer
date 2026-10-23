//! 优化轮次存储（`optimization_round` 表）
//!
//! 进化中心 P3 的持久化层（与词表 / 反馈日志 / 反思中心 / 训练作业共用
//! 同一 SQLite 文件）：每次「自动评估 → 提案」落一行轮次，带状态机
//! （`proposed` → `confirmed` → `running` → `completed` / `failed`；
//! `proposed` → `rejected` 人工门拒绝；迁移合法性由
//! `gec_core::optimization_round::round_status` 定义）与动作集合 /
//! 执行结果（JSON）。
//!
//! 本模块只存**序列化后的 JSON**（`evidence_json` / `actions_json` /
//! `result_json`），不依赖 gec-core 的具体类型——存储与核心逻辑解耦。

use std::path::Path;

use rusqlite::{Connection, Result as SqlResult, params};
use serde::{Deserialize, Serialize};

/// 优化轮次行
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptimizationRoundRow {
    pub id: i64,
    pub lang: String,
    /// 状态（`proposed` / `confirmed` / `running` / `completed` / `failed` / `rejected`）
    pub status: String,
    /// 轮次总述（为什么现在开轮）
    pub reason: String,
    /// 证据列表（反馈统计 / 池规模 / 测评数字）
    pub evidence: serde_json::Value,
    /// 动作集合（`[{kind, target, reason, payload}]`）
    pub actions: serde_json::Value,
    /// 执行结果（completed / failed 后填充；failed 时含 `error` 字段）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    pub created_at: String,
    pub updated_at: String,
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS optimization_round (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    lang TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'proposed',
    reason TEXT NOT NULL DEFAULT '',
    evidence_json TEXT NOT NULL DEFAULT '[]',
    actions_json TEXT NOT NULL DEFAULT '[]',
    result_json TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_optimization_round_status ON optimization_round (status, lang);
"#;

/// 优化轮次存储
pub struct OptimizationRoundStore {
    conn: std::sync::Mutex<Connection>,
}

fn row_from(r: &rusqlite::Row) -> SqlResult<OptimizationRoundRow> {
    Ok(OptimizationRoundRow {
        id: r.get(0)?,
        lang: r.get(1)?,
        status: r.get(2)?,
        reason: r.get(3)?,
        evidence: serde_json::from_str(r.get::<_, String>(4)?.as_str())
            .unwrap_or(serde_json::Value::Null),
        actions: serde_json::from_str(r.get::<_, String>(5)?.as_str())
            .unwrap_or(serde_json::Value::Null),
        result: r
            .get::<_, Option<String>>(6)?
            .map(|s| serde_json::from_str(&s).unwrap_or(serde_json::Value::Null)),
        created_at: r.get(7)?,
        updated_at: r.get(8)?,
    })
}

const COLS: &str =
    "id, lang, status, reason, evidence_json, actions_json, result_json, created_at, updated_at";

impl OptimizationRoundStore {
    /// 打开（或创建）存储（与词表 / 反馈日志 / 反思中心 / 训练作业共用同一 db 文件）
    pub fn open(path: &Path) -> SqlResult<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: std::sync::Mutex::new(conn),
        })
    }

    /// 内存存储（测试用）
    pub fn open_in_memory() -> SqlResult<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: std::sync::Mutex::new(conn),
        })
    }

    /// 插入轮次（初始状态 `proposed`）→ 轮次 id
    pub fn insert_round(
        &self,
        lang: &str,
        reason: &str,
        evidence: &serde_json::Value,
        actions: &serde_json::Value,
        now: &str,
    ) -> SqlResult<i64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO optimization_round (lang, status, reason, evidence_json, actions_json, created_at, updated_at)
             VALUES (?1, 'proposed', ?2, ?3, ?4, ?5, ?5)",
            params![lang, reason, evidence.to_string(), actions.to_string(), now],
        )?;
        Ok(conn.last_insert_rowid())
    }
    /// 轮次列表（新 → 旧；`lang` / `status` 可选过滤）
    pub fn list_rounds(
        &self,
        lang: Option<&str>,
        status: Option<&str>,
        limit: usize,
    ) -> SqlResult<Vec<OptimizationRoundRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLS} FROM optimization_round ORDER BY id DESC LIMIT ?1"
        ))?;
        let rows = stmt.query_map(rusqlite::params![limit as i64], |r| row_from(r))?;
        let mut out = Vec::new();
        for row in rows {
            let row = row?;
            if status.map_or(true, |s| row.status == s) && lang.map_or(true, |l| row.lang == l) {
                out.push(row);
            }
        }
        Ok(out)
    }

    /// 轮次详情
    pub fn get_round(&self, id: i64) -> SqlResult<Option<OptimizationRoundRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLS} FROM optimization_round WHERE id = ?1"
        ))?;
        let mut rows = stmt.query_map(params![id], |r| row_from(r))?;
        rows.next().transpose()
    }

    /// 状态迁移（含合法性校验）+ 可选结果写入
    ///
    /// 迁移不合法（如 `completed` → `running`、`confirmed` → `rejected`）
    /// 返回 `Ok(false)`；轮次不存在返回 `Ok(false)`。
    pub fn transition(
        &self,
        id: i64,
        to: &str,
        result: Option<&serde_json::Value>,
        now: &str,
    ) -> SqlResult<bool> {
        let conn = self.conn.lock().unwrap();
        let from = match conn
            .query_row(
                "SELECT status FROM optimization_round WHERE id = ?1",
                params![id],
                |r| r.get::<_, String>(0),
            )
            .ok()
        {
            Some(s) => s,
            None => return Ok(false),
        };
        if !can_transition(&from, to) {
            return Ok(false);
        }
        if let Some(r) = result {
            conn.execute(
                "UPDATE optimization_round SET status = ?1, result_json = ?2, updated_at = ?3 WHERE id = ?4",
                params![to, r.to_string(), now, id],
            )?;
        } else {
            conn.execute(
                "UPDATE optimization_round SET status = ?1, updated_at = ?2 WHERE id = ?3",
                params![to, now, id],
            )?;
        }
        Ok(true)
    }
}

/// 状态迁移合法性（与 `gec_core::optimization_round::round_status::can_transition`
/// 同源——存储层与核心解耦，此处保持独立副本）
fn can_transition(from: &str, to: &str) -> bool {
    match (from, to) {
        ("proposed", "confirmed")
        | ("proposed", "rejected")
        | ("proposed", "failed")
        | ("confirmed", "running")
        | ("confirmed", "failed")
        | ("running", "completed")
        | ("running", "failed") => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> OptimizationRoundStore {
        OptimizationRoundStore::open_in_memory().unwrap()
    }

    #[test]
    fn insert_and_get() {
        let s = store();
        let id = s
            .insert_round(
                "zh",
                "自动评估：2 类动作触发",
                &serde_json::json!(["证据1", "证据2"]),
                &serde_json::json!([{ "kind": "l3_retrain", "target": "feature" }]),
                "2026-10-17T00:00:00Z",
            )
            .unwrap();
        let row = s.get_round(id).unwrap().unwrap();
        assert_eq!(row.status, "proposed");
        assert_eq!(row.lang, "zh");
        assert_eq!(row.reason, "自动评估：2 类动作触发");
        assert_eq!(row.evidence.as_array().unwrap().len(), 2);
        assert_eq!(row.actions.as_array().unwrap()[0]["kind"], "l3_retrain");
        assert!(row.result.is_none());
    }

    #[test]
    fn list_filters() {
        let s = store();
        let a = s
            .insert_round(
                "zh",
                "r1",
                &serde_json::json!([]),
                &serde_json::json!([]),
                "t1",
            )
            .unwrap();
        let b = s
            .insert_round(
                "en",
                "r2",
                &serde_json::json!([]),
                &serde_json::json!([]),
                "t2",
            )
            .unwrap();
        assert_eq!(s.list_rounds(None, None, 10).unwrap().len(), 2);
        assert_eq!(s.list_rounds(Some("zh"), None, 10).unwrap().len(), 1);
        assert_eq!(s.list_rounds(Some("en"), None, 10).unwrap()[0].id, b);
        assert_eq!(s.list_rounds(None, None, 1).unwrap().len(), 1);
        assert_eq!(s.list_rounds(None, None, 1).unwrap()[0].id, b, "新 → 旧");
        let _ = a;
    }

    #[test]
    fn state_machine() {
        let s = store();
        let id = s
            .insert_round(
                "zh",
                "r",
                &serde_json::json!([]),
                &serde_json::json!([]),
                "t",
            )
            .unwrap();
        assert!(s.transition(id, "confirmed", None, "t2").unwrap());
        assert!(s.transition(id, "running", None, "t3").unwrap());
        assert!(
            s.transition(
                id,
                "completed",
                Some(&serde_json::json!({ "ok": true })),
                "t4"
            )
            .unwrap()
        );
        let row = s.get_round(id).unwrap().unwrap();
        assert_eq!(row.status, "completed");
        assert_eq!(row.result.unwrap()["ok"], true);
        assert!(!s.transition(id, "running", None, "t5").unwrap());
    }

    #[test]
    fn reject_gate() {
        let s = store();
        let id = s
            .insert_round(
                "zh",
                "r",
                &serde_json::json!([]),
                &serde_json::json!([]),
                "t",
            )
            .unwrap();
        assert!(s.transition(id, "rejected", None, "t2").unwrap());
        assert!(!s.transition(id, "confirmed", None, "t3").unwrap());
        assert!(!s.transition(id, "running", None, "t3").unwrap());
    }

    #[test]
    fn invalid_transitions_rejected() {
        let s = store();
        let id = s
            .insert_round(
                "zh",
                "r",
                &serde_json::json!([]),
                &serde_json::json!([]),
                "t",
            )
            .unwrap();
        assert!(!s.transition(id, "running", None, "t2").unwrap());
        assert!(!s.transition(id, "completed", None, "t2").unwrap());
        assert!(!s.transition(9999, "confirmed", None, "t2").unwrap());
    }
}
