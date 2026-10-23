//! 反思中心存储（`reflection_report` + `reflection_suggestion` 表）
//!
//! 进化中心 P1 的持久化层（与词表 / 反馈日志共用同一 SQLite 文件）：
//! - **反思报告**（`reflection_report`）：`gec_core::reflection_center::build_report`
//!   的完整产出（JSON 序列化），每次「生成反思报告」落一行；
//! - **建议队列**（`reflection_suggestion`）：报告中的建议逐条落行，带状态机
//!   （`pending` → `applied` / `consumed` / `dismissed`）——P2 训练中心消费
//!   队列时按 id 更新状态，形成「反思 → 训练 → 测评」闭环的可追溯记录。
//!
//! 本模块只存**序列化后的 JSON**（`report_json` / `payload_json` /
//! `evidence_json`），不依赖 gec-core 的具体类型——存储与核心逻辑解耦。

use std::path::Path;

use rusqlite::{Connection, Result as SqlResult, params};
use serde::{Deserialize, Serialize};

/// 建议队列状态
pub mod suggestion_status {
    /// 待处理（默认；P2 尚未消费）
    pub const PENDING: &str = "pending";
    /// 已执行（建议对应的动作已落地，如规则已停用 / 条件已入库）
    pub const APPLIED: &str = "applied";
    /// 已被 P2 训练中心消费（如负样本已汇入飞轮、重训练已发起）
    pub const CONSUMED: &str = "consumed";
    /// 已忽略（人工判定不需要执行）
    pub const DISMISSED: &str = "dismissed";

    /// 合法状态集合
    pub const ALL: [&str; 4] = [PENDING, APPLIED, CONSUMED, DISMISSED];
}

/// 反思报告列表行
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReflectionReportRow {
    pub id: i64,
    pub lang: String,
    pub created_at: String,
    /// 报告中的建议条数
    pub n_suggestions: i64,
}

/// 反思报告详情（完整 JSON 产出）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReflectionReportFull {
    pub id: i64,
    pub lang: String,
    pub created_at: String,
    /// 报告 JSON（`gec_core::ReflectionCenterReport` 序列化）
    pub report: serde_json::Value,
}

/// 建议队例行
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReflectionSuggestionRow {
    pub id: i64,
    pub report_id: i64,
    pub lang: String,
    pub kind: String,
    pub target: String,
    pub summary: String,
    pub payload: serde_json::Value,
    pub evidence: Vec<String>,
    pub priority: i64,
    /// `pending` / `applied` / `consumed` / `dismissed`
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_at: Option<String>,
    pub created_at: String,
}

/// 待落库的建议（由调用方从 `gec_core::ReflectionCenterReport::suggestions` 序列化）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuggestionSeed {
    pub kind: String,
    pub target: String,
    pub summary: String,
    #[serde(default)]
    pub evidence: Vec<String>,
    #[serde(default)]
    pub payload: serde_json::Value,
    #[serde(default)]
    pub priority: u8,
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS reflection_report (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    lang TEXT NOT NULL,
    created_at TEXT NOT NULL,
    report_json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS reflection_suggestion (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    report_id INTEGER NOT NULL,
    lang TEXT NOT NULL,
    kind TEXT NOT NULL,
    target TEXT NOT NULL,
    summary TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    evidence_json TEXT NOT NULL,
    priority INTEGER NOT NULL DEFAULT 2,
    status TEXT NOT NULL DEFAULT 'pending',
    status_at TEXT,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_reflection_suggestion_status
    ON reflection_suggestion (status, lang);
"#;

/// 反思中心存储
pub struct ReflectionCenterStore {
    conn: std::sync::Mutex<Connection>,
}

impl ReflectionCenterStore {
    /// 打开（或创建）存储（与词表 / 反馈日志共用同一 db 文件）
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

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 保存一份反思报告 + 其建议队列，返回 (报告 id, 建议 id 列表)
    pub fn save_report(
        &self,
        lang: &str,
        created_at: &str,
        report_json: &str,
        suggestions: &[SuggestionSeed],
    ) -> SqlResult<(i64, Vec<i64>)> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO reflection_report (lang, created_at, report_json) VALUES (?1, ?2, ?3)",
            params![lang, created_at, report_json],
        )?;
        let report_id = conn.last_insert_rowid();
        let mut ids = Vec::new();
        for s in suggestions {
            conn.execute(
                "INSERT INTO reflection_suggestion
                 (report_id, lang, kind, target, summary, payload_json, evidence_json, priority, status, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'pending', ?9)",
                params![
                    report_id,
                    lang,
                    s.kind,
                    s.target,
                    s.summary,
                    serde_json::to_string(&s.payload).unwrap_or_else(|_| "null".into()),
                    serde_json::to_string(&s.evidence).unwrap_or_else(|_| "[]".into()),
                    s.priority as i64,
                    created_at
                ],
            )?;
            ids.push(conn.last_insert_rowid());
        }
        Ok((report_id, ids))
    }

    /// 报告列表（新 → 旧；可按语言过滤）
    pub fn list_reports(
        &self,
        lang: Option<&str>,
        limit: usize,
    ) -> SqlResult<Vec<ReflectionReportRow>> {
        let conn = self.lock();
        let base = "SELECT r.id, r.lang, r.created_at,
                   (SELECT COUNT(*) FROM reflection_suggestion s WHERE s.report_id = r.id) AS n
                   FROM reflection_report r";
        let sql = match lang {
            Some(_) => format!("{base} WHERE r.lang = ?1 ORDER BY r.id DESC LIMIT ?2"),
            None => format!("{base} ORDER BY r.id DESC LIMIT ?1"),
        };
        let mut stmt = conn.prepare(&sql)?;
        let rows = match lang {
            Some(l) => stmt.query_map(params![l, limit as i64], map_report_row)?,
            None => stmt.query_map(params![limit as i64], map_report_row)?,
        };
        rows.collect()
    }

    /// 报告详情（完整 JSON）
    pub fn get_report(&self, id: i64) -> SqlResult<Option<ReflectionReportFull>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, lang, created_at, report_json FROM reflection_report WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id], |r| {
            let report_str: String = r.get(3)?;
            Ok(ReflectionReportFull {
                id: r.get(0)?,
                lang: r.get(1)?,
                created_at: r.get(2)?,
                report: serde_json::from_str(&report_str).unwrap_or(serde_json::Value::Null),
            })
        })?;
        rows.next().transpose()
    }

    /// 建议队列（按状态 / 语言过滤；新 → 旧）
    pub fn list_suggestions(
        &self,
        status: Option<&str>,
        lang: Option<&str>,
        limit: usize,
    ) -> SqlResult<Vec<ReflectionSuggestionRow>> {
        let conn = self.lock();
        let mut conds: Vec<String> = Vec::new();
        let mut n = 1;
        if status.is_some() {
            conds.push(format!("status = ?{n}"));
            n += 1;
        }
        if lang.is_some() {
            conds.push(format!("lang = ?{n}"));
            n += 1;
        }
        let sql = if conds.is_empty() {
            "SELECT id, report_id, lang, kind, target, summary, payload_json, evidence_json, priority, status, status_at, created_at
             FROM reflection_suggestion ORDER BY id DESC LIMIT ?1"
                .to_string()
        } else {
            format!(
                "SELECT id, report_id, lang, kind, target, summary, payload_json, evidence_json, priority, status, status_at, created_at
             FROM reflection_suggestion WHERE {} ORDER BY id DESC LIMIT ?{n}",
                conds.join(" AND ")
            )
        };
        let mut stmt = conn.prepare(&sql)?;
        let rows = match (status, lang) {
            (Some(s), Some(l)) => {
                stmt.query_map(params![s, l, limit as i64], map_suggestion_row)?
            }
            (Some(s), None) => stmt.query_map(params![s, limit as i64], map_suggestion_row)?,
            (None, Some(l)) => stmt.query_map(params![l, limit as i64], map_suggestion_row)?,
            (None, None) => stmt.query_map(params![limit as i64], map_suggestion_row)?,
        };
        rows.collect()
    }

    /// 按 id 取单条建议（P2 训练中心消费入口用）
    pub fn get_suggestion(&self, id: i64) -> SqlResult<Option<ReflectionSuggestionRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, report_id, lang, kind, target, summary, payload_json, evidence_json, priority, status, status_at, created_at
             FROM reflection_suggestion WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id], map_suggestion_row)?;
        rows.next().transpose()
    }

    /// 更新建议状态（P2 消费 / 人工忽略），返回是否命中
    /// 更新建议状态（P2 消费 / 人工忽略），返回是否命中
    pub fn update_suggestion_status(&self, id: i64, status: &str) -> SqlResult<bool> {
        let conn = self.lock();
        let n = conn.execute(
            "UPDATE reflection_suggestion
             SET status = ?1, status_at = datetime('now') WHERE id = ?2",
            params![status, id],
        )?;
        Ok(n > 0)
    }

    /// 某报告的全部建议（priority 升序）
    pub fn suggestions_for_report(
        &self,
        report_id: i64,
    ) -> SqlResult<Vec<ReflectionSuggestionRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, report_id, lang, kind, target, summary, payload_json, evidence_json, priority, status, status_at, created_at
             FROM reflection_suggestion WHERE report_id = ?1 ORDER BY priority, id",
        )?;
        let rows = stmt.query_map(params![report_id], map_suggestion_row)?;
        rows.collect()
    }
}

fn map_report_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ReflectionReportRow> {
    Ok(ReflectionReportRow {
        id: r.get(0)?,
        lang: r.get(1)?,
        created_at: r.get(2)?,
        n_suggestions: r.get(3)?,
    })
}

fn map_suggestion_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ReflectionSuggestionRow> {
    let payload_str: String = r.get(6)?;
    let evidence_str: String = r.get(7)?;
    Ok(ReflectionSuggestionRow {
        id: r.get(0)?,
        report_id: r.get(1)?,
        lang: r.get(2)?,
        kind: r.get(3)?,
        target: r.get(4)?,
        summary: r.get(5)?,
        payload: serde_json::from_str(&payload_str).unwrap_or(serde_json::Value::Null),
        evidence: serde_json::from_str(&evidence_str).unwrap_or_default(),
        priority: r.get(8)?,
        status: r.get(9)?,
        status_at: r.get(10)?,
        created_at: r.get(11)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(n: usize) -> Vec<SuggestionSeed> {
        (0..n)
            .map(|i| SuggestionSeed {
                kind: format!("kind_{i}"),
                target: format!("t{i}"),
                summary: format!("s{i}"),
                evidence: vec!["e1".into(), "e2".into()],
                payload: serde_json::json!({"i": i}),
                priority: (i % 3) as u8,
            })
            .collect()
    }

    #[test]
    fn save_and_list_reports() {
        let store = ReflectionCenterStore::open_in_memory().unwrap();
        let (id1, ids1) = store
            .save_report("zh", "2026-10-17T00:00:00Z", "{\"a\":1}", &seed(3))
            .unwrap();
        assert!(id1 > 0);
        assert_eq!(ids1.len(), 3);
        store
            .save_report("en", "2026-10-18T00:00:00Z", "{\"a\":2}", &[])
            .unwrap();
        let all = store.list_reports(None, 10).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].lang, "en", "新 → 旧");
        assert_eq!(all[1].n_suggestions, 3);
        let zh = store.list_reports(Some("zh"), 10).unwrap();
        assert_eq!(zh.len(), 1);
        assert_eq!(zh[0].n_suggestions, 3);
    }

    #[test]
    fn get_report_roundtrip() {
        let store = ReflectionCenterStore::open_in_memory().unwrap();
        let (id, _) = store
            .save_report("zh", "t", "{\"lang\":\"zh\",\"suggestions\":[]}", &[])
            .unwrap();
        let full = store.get_report(id).unwrap().unwrap();
        assert_eq!(full.report["lang"], "zh");
        assert!(store.get_report(9999).unwrap().is_none());
    }

    #[test]
    fn suggestion_queue_and_status() {
        let store = ReflectionCenterStore::open_in_memory().unwrap();
        let (rid, ids) = store.save_report("zh", "t", "{}", &seed(3)).unwrap();
        let all = store.list_suggestions(None, None, 10).unwrap();
        assert_eq!(all.len(), 3);
        assert!(all.iter().all(|s| s.status == "pending"));
        assert_eq!(all[0].evidence, vec!["e1", "e2"]);
        assert_eq!(all[0].payload["i"], 2, "新 → 旧：首行是最后一条");
        // 状态流转
        assert!(
            store
                .update_suggestion_status(ids[0], suggestion_status::CONSUMED)
                .unwrap()
        );
        let consumed = store.list_suggestions(Some("consumed"), None, 10).unwrap();
        assert_eq!(consumed.len(), 1);
        assert!(consumed[0].status_at.is_some());
        // 语言过滤
        assert!(
            store
                .list_suggestions(None, Some("en"), 10)
                .unwrap()
                .is_empty()
        );
        // 不存在的 id
        assert!(
            !store
                .update_suggestion_status(9999, suggestion_status::APPLIED)
                .unwrap()
        );
        // 报告维度查询
        assert_eq!(store.suggestions_for_report(rid).unwrap().len(), 3);
    }
}
