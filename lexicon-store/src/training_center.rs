//! 训练中心存储（`training_job` 表）
//!
//! 进化中心 P2 的持久化层（与词表 / 反馈日志 / 反思中心共用同一 SQLite 文件）：
//! 每次训练动作（领域池构建 / L3 重训练 / 词表条件化入库 / LLM 训练数据构建）
//! 落一行作业，带状态机（`proposed` → `confirmed` → `running` →
//! `completed` / `failed`，迁移合法性由 `gec_core::training_center::job_status`
//! 定义）与执行结果（JSON）。
//!
//! 本模块只存**序列化后的 JSON**（`payload_json` / `result_json`），
//! 不依赖 gec-core 的具体类型——存储与核心逻辑解耦。

use std::path::Path;

use rusqlite::{Connection, Result as SqlResult, params};
use serde::{Deserialize, Serialize};

/// 训练作业行
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrainingJobRow {
    pub id: i64,
    /// 作业类型（`domain_pool` / `l3_retrain` / `lexicon_condition` / `llm_data`）
    pub kind: String,
    pub lang: String,
    /// 状态（`proposed` / `confirmed` / `running` / `completed` / `failed`）
    pub status: String,
    /// 来源反思建议 id（`None` = 手动发起）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_suggestion: Option<i64>,
    /// 执行参数（按 kind 解释）
    pub payload: serde_json::Value,
    /// 执行结果（completed / failed 后填充；failed 时含 `error` 字段）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    pub created_at: String,
    pub updated_at: String,
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS training_job (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    kind TEXT NOT NULL,
    lang TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'proposed',
    source_suggestion INTEGER,
    payload_json TEXT NOT NULL DEFAULT '{}',
    result_json TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_training_job_status ON training_job (status, lang);
"#;

/// 训练中心存储
pub struct TrainingCenterStore {
    conn: std::sync::Mutex<Connection>,
}

impl TrainingCenterStore {
    /// 打开（或创建）存储（与词表 / 反馈日志 / 反思中心共用同一 db 文件）
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

    /// 插入作业（初始状态 `proposed`）→ 作业 id
    ///
    /// `created_at` / `updated_at` 由调用方传入（核心层保持纯函数，
    /// 时间戳在存储层填充）。
    pub fn insert_job(
        &self,
        kind: &str,
        lang: &str,
        source_suggestion: Option<i64>,
        payload: &serde_json::Value,
        now: &str,
    ) -> SqlResult<i64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO training_job (kind, lang, status, source_suggestion, payload_json, created_at, updated_at)
             VALUES (?1, ?2, 'proposed', ?3, ?4, ?5, ?5)",
            params![kind, lang, source_suggestion, payload.to_string(), now],
        )?;
        Ok(conn.last_insert_rowid())
    }
    /// 作业列表（新 → 旧；`lang` / `status` 可选过滤）
    pub fn list_jobs(
        &self,
        lang: Option<&str>,
        status: Option<&str>,
        limit: usize,
    ) -> SqlResult<Vec<TrainingJobRow>> {
        let conn = self.conn.lock().unwrap();
        const BASE: &str = "SELECT id, kind, lang, status, source_suggestion, payload_json, result_json, created_at, updated_at FROM training_job";
        let mut stmt = conn.prepare(&format!("{BASE} ORDER BY id DESC LIMIT ?1"))?;
        let rows = stmt.query_map(rusqlite::params![limit as i64], |r| {
            Ok(TrainingJobRow {
                id: r.get(0)?,
                kind: r.get(1)?,
                lang: r.get(2)?,
                status: r.get(3)?,
                source_suggestion: r.get(4)?,
                payload: serde_json::from_str(r.get::<_, String>(5)?.as_str())
                    .unwrap_or(serde_json::Value::Null),
                result: r
                    .get::<_, Option<String>>(6)?
                    .map(|s| serde_json::from_str(&s).unwrap_or(serde_json::Value::Null)),
                created_at: r.get(7)?,
                updated_at: r.get(8)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            let row = row?;
            if status.map_or(true, |s| row.status == s) && lang.map_or(true, |l| row.lang == l) {
                out.push(row);
            }
        }
        Ok(out)
    }

    /// 作业详情
    pub fn get_job(&self, id: i64) -> SqlResult<Option<TrainingJobRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, kind, lang, status, source_suggestion, payload_json, result_json, created_at, updated_at
             FROM training_job WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id], |r| {
            Ok(TrainingJobRow {
                id: r.get(0)?,
                kind: r.get(1)?,
                lang: r.get(2)?,
                status: r.get(3)?,
                source_suggestion: r.get(4)?,
                payload: serde_json::from_str(r.get::<_, String>(5)?.as_str())
                    .unwrap_or(serde_json::Value::Null),
                result: r
                    .get::<_, Option<String>>(6)?
                    .map(|s| serde_json::from_str(&s).unwrap_or(serde_json::Value::Null)),
                created_at: r.get(7)?,
                updated_at: r.get(8)?,
            })
        })?;
        rows.next().transpose()
    }

    /// 状态迁移（含合法性校验）+ 可选结果写入
    ///
    /// 迁移不合法（如 `completed` → `running`）返回 `Ok(false)`；
    /// 作业不存在返回 `Ok(false)`。
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
                "SELECT status FROM training_job WHERE id = ?1",
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
                "UPDATE training_job SET status = ?1, result_json = ?2, updated_at = ?3 WHERE id = ?4",
                params![to, r.to_string(), now, id],
            )?;
        } else {
            conn.execute(
                "UPDATE training_job SET status = ?1, updated_at = ?2 WHERE id = ?3",
                params![to, now, id],
            )?;
        }
        Ok(true)
    }
}

/// 状态迁移合法性（与 `gec_core::training_center::job_status::can_transition`
/// 同源——存储层与核心解耦，此处保持独立副本）
fn can_transition(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        ("proposed", "confirmed")
            | ("proposed", "failed")
            | ("confirmed", "running")
            | ("confirmed", "failed")
            | ("running", "completed")
            | ("running", "failed")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_list() {
        let store = TrainingCenterStore::open_in_memory().unwrap();
        let id1 = store
            .insert_job(
                "domain_pool",
                "zh",
                None,
                &serde_json::json!({ "a": 1 }),
                "t0",
            )
            .unwrap();
        let id2 = store
            .insert_job("l3_retrain", "zh", Some(7), &serde_json::json!({}), "t1")
            .unwrap();
        assert_ne!(id1, id2);

        let all = store.list_jobs(None, None, 10).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].id, id2); // 新 → 旧
        assert_eq!(all[0].status, "proposed");
        assert_eq!(all[0].source_suggestion, Some(7));

        let zh = store.list_jobs(Some("zh"), None, 10).unwrap();
        assert_eq!(zh.len(), 2);
        let en = store.list_jobs(Some("en"), None, 10).unwrap();
        assert!(en.is_empty());

        let proposed = store.list_jobs(None, Some("proposed"), 10).unwrap();
        assert_eq!(proposed.len(), 2);
    }

    #[test]
    fn transition_state_machine() {
        let store = TrainingCenterStore::open_in_memory().unwrap();
        let id = store
            .insert_job("llm_data", "zh", None, &serde_json::json!({}), "t0")
            .unwrap();

        assert!(store.transition(id, "confirmed", None, "t1").unwrap());
        assert!(store.transition(id, "running", None, "t1").unwrap());
        assert!(
            store
                .transition(id, "completed", Some(&serde_json::json!({ "n": 3 })), "t2")
                .unwrap()
        );
        let job = store.get_job(id).unwrap().unwrap();
        assert_eq!(job.status, "completed");
        assert_eq!(job.result.as_ref().unwrap()["n"], 3);

        // 终态不可再迁移
        assert!(!store.transition(id, "running", None, "t3").unwrap());
        // 跳跃不合法
        let id2 = store
            .insert_job("llm_data", "zh", None, &serde_json::json!({}), "t0")
            .unwrap();
        assert!(!store.transition(id2, "completed", None, "t1").unwrap());
        assert!(!store.transition(id2, "running", None, "t1").unwrap());
        // 不存在的作业
        assert!(!store.transition(999, "running", None, "t1").unwrap());
    }

    #[test]
    fn get_job_roundtrip() {
        let store = TrainingCenterStore::open_in_memory().unwrap();
        let payload = serde_json::json!({ "lang": "zh", "originals": ["a", "b"] });
        let id = store
            .insert_job("lexicon_condition", "zh", Some(3), &payload, "t0")
            .unwrap();
        let job = store.get_job(id).unwrap().unwrap();
        assert_eq!(job.kind, "lexicon_condition");
        assert_eq!(job.source_suggestion, Some(3));
        assert_eq!(job.payload["originals"].as_array().unwrap().len(), 2);
        assert!(job.result.is_none());
        assert!(store.get_job(999).unwrap().is_none());
    }
}
