//! 请求级摘要账本（Detect Pipeline v2 §7 可观测性）
//!
//! 每次 detect / analyze 落一行（**不受缓存资格 / 历史复用门控**——partial /
//! degraded / 回放更要留痕），供实时看板聚合与完整性/质量两路告警：
//! - 完整性告警：24h 内所有权空洞非零或 dropped 账本非零的请求（数据静默
//!   丢失即最高优先级信号，写行处另有即时 `tracing::error!`）；
//! - 质量告警：24h 内 analyze 归因 `grammar_novice`（规则缺失兜底）占比异常，
//!   或「partial 但零检出」反复出现（partial 却报不出错误的可疑轮次）。
//!
//! 与 detect_history 共用同一 SQLite 文件；回放行 `quality_status = NULL`
//! （回放无本次质量核算）。聚合按 day × 规则指纹分桶——规则/模型变更前后
//! 的指标必须分开看，否则趋势失真。

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{Connection, params};

/// 一行的全部输入（引用 run_pipeline 现场，避免中间拷贝）
pub struct SummaryRow<'a> {
    /// `"detect"` | `"analyze"`
    pub entry: &'static str,
    pub lang: &'a str,
    pub level: u8,
    /// full/partial/degraded；`None` = 回放（未实际核算）
    pub quality_status: Option<&'a str>,
    pub coverage: Option<f64>,
    pub unowned_chars: usize,
    pub dropped: gec_core::DroppedLedger,
    /// 降级原因 JSON 数组（非空才有）
    pub degraded_json: Option<String>,
    pub chunks: Option<&'a gec_core::ChunkStats>,
    pub sentences: Option<&'a gec_core::SentenceStats>,
    pub version_json: Option<String>,
    pub error_count: usize,
    pub from_history: bool,
    /// 本次为 LRU 命中回放（非历史复用、无 quality）
    pub from_cache: bool,
    pub total_ms: Option<u64>,
}

/// 请求摘要存储（SQLite，共用 langfixer.db）
pub struct RequestSummaryStore {
    conn: Mutex<Connection>,
}

impl RequestSummaryStore {
    pub fn open(path: &Path) -> Result<Self, rusqlite::Error> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS request_summary (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                day TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                entry TEXT NOT NULL,
                lang TEXT NOT NULL,
                level INTEGER NOT NULL,
                quality_status TEXT,
                coverage REAL,
                unowned_chars INTEGER NOT NULL DEFAULT 0,
                dropped_ownership INTEGER NOT NULL DEFAULT 0,
                dropped_pii_mask INTEGER NOT NULL DEFAULT 0,
                dropped_out_of_range INTEGER NOT NULL DEFAULT 0,
                dropped_out_of_scope INTEGER NOT NULL DEFAULT 0,
                degraded_json TEXT,
                chunks_mode TEXT,
                chunks_total INTEGER,
                chunks_done INTEGER,
                chunks_failed INTEGER,
                chunks_parse_failed INTEGER,
                sentences_total INTEGER,
                sentences_reviewed INTEGER,
                version_json TEXT,
                error_count INTEGER NOT NULL DEFAULT 0,
                fallback_count INTEGER,
                from_history INTEGER NOT NULL DEFAULT 0,
                from_cache INTEGER NOT NULL DEFAULT 0,
                total_ms INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_request_summary_day ON request_summary(day);
            CREATE INDEX IF NOT EXISTS idx_request_summary_time ON request_summary(created_at DESC);",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// 写一行（同步——账本不许丢；单行 INSERT 微秒级，不值得异步）
    pub fn insert(&self, row: &SummaryRow) -> Result<i64, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let now = chrono::Utc::now();
        let created_at = now.format("%Y-%m-%d %H:%M:%S").to_string();
        let day = now.format("%Y-%m-%d").to_string();
        let (chunks_mode, chunks_total, chunks_done, chunks_failed, chunks_parse_failed) =
            match row.chunks {
                Some(c) => (
                    Some(c.mode.clone()),
                    Some(c.total as i64),
                    Some(c.done as i64),
                    Some(c.failed as i64),
                    Some(c.parse_failed as i64),
                ),
                None => (None, None, None, None, None),
            };
        let (sentences_total, sentences_reviewed) = match row.sentences {
            Some(s) => (Some(s.total as i64), Some(s.reviewed as i64)),
            None => (None, None),
        };
        conn.execute(
            "INSERT INTO request_summary (
                day, created_at, entry, lang, level, quality_status, coverage,
                unowned_chars, dropped_ownership, dropped_pii_mask,
                dropped_out_of_range, dropped_out_of_scope, degraded_json,
                chunks_mode, chunks_total, chunks_done, chunks_failed, chunks_parse_failed,
                sentences_total, sentences_reviewed, version_json, error_count,
                fallback_count, from_history, from_cache, total_ms)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,NULL,?23,?24,?25)",
            params![
                day,
                created_at,
                row.entry,
                row.lang,
                row.level as i64,
                row.quality_status,
                row.coverage,
                row.unowned_chars as i64,
                row.dropped.ownership as i64,
                row.dropped.pii_mask as i64,
                row.dropped.out_of_range as i64,
                row.dropped.out_of_scope as i64,
                row.degraded_json,
                chunks_mode,
                chunks_total,
                chunks_done,
                chunks_failed,
                chunks_parse_failed,
                sentences_total,
                sentences_reviewed,
                row.version_json,
                row.error_count as i64,
                row.from_history as i64,
                row.from_cache as i64,
                row.total_ms.map(|v| v as i64),
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// analyze 归因完成后回填兜底归因（grammar_novice）条数
    pub fn set_fallback(&self, id: i64, count: usize) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "UPDATE request_summary SET fallback_count = ?2 WHERE id = ?1",
            params![id, count as i64],
        )?;
        Ok(())
    }

    /// 两路告警（24h 窗口）
    pub fn alerts(&self) -> Result<serde_json::Value, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        // ① 完整性：所有权空洞 / dropped 账本非零的行数（只统计实际检测行——
        //   回放行账本列恒 0，天然不触发）
        let lossy_rows: i64 = conn.query_row(
            "SELECT COUNT(*) FROM request_summary
             WHERE created_at >= datetime('now', '-1 day')
               AND (unowned_chars != 0
                    OR dropped_ownership + dropped_pii_mask
                       + dropped_out_of_range + dropped_out_of_scope > 0)",
            [],
            |r| r.get(0),
        )?;
        // ② 质量：analyze 行归因兜底占比 + 「partial 却零检出」轮次
        let (analyze_rows, fb_sum, err_sum): (i64, i64, i64) = conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(fallback_count), 0), COALESCE(SUM(error_count), 0)
             FROM request_summary
             WHERE created_at >= datetime('now', '-1 day') AND entry = 'analyze'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let partial_empty: i64 = conn.query_row(
            "SELECT COUNT(*) FROM request_summary
             WHERE created_at >= datetime('now', '-1 day')
               AND quality_status = 'partial' AND error_count = 0",
            [],
            |r| r.get(0),
        )?;
        let fallback_ratio = if err_sum > 0 {
            fb_sum as f64 / err_sum as f64
        } else {
            0.0
        };
        // 样本门槛 ≥20 防个位数误报；partial∧零检出 ≥5 视为模式而非巧合
        let quality_triggered = (err_sum >= 20 && fallback_ratio > 0.30) || partial_empty >= 5;
        Ok(serde_json::json!({
            "window_hours": 24,
            "integrity": {
                "lossy_rows": lossy_rows,
                "triggered": lossy_rows > 0,
            },
            "quality": {
                "analyze_rows": analyze_rows,
                "fallback_sum": fb_sum,
                "error_sum": err_sum,
                "fallback_ratio": (fallback_ratio * 1000.0).round() / 1000.0,
                "partial_empty_runs": partial_empty,
                "triggered": quality_triggered,
            },
        }))
    }

    /// 按 day × 规则指纹聚合（近 `days` 天，看板时间序列数据源）
    pub fn daily_summary(&self, days: i64) -> Result<serde_json::Value, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        // 一次取行、内存聚合：P95 需要原始 total_ms 序列，SQL 端 percentile 不可得；
        // 规则指纹从 version_json 取（->> 依赖 SQLite ≥3.38，不赌版本，Rust 端解析）
        let mut stmt = conn.prepare(
            "SELECT day, version_json, coverage, degraded_json,
                    dropped_ownership, dropped_pii_mask, dropped_out_of_range, dropped_out_of_scope,
                    chunks_done, chunks_total,
                    sentences_total, sentences_reviewed,
                    from_history, from_cache, total_ms
             FROM request_summary
             WHERE created_at >= datetime('now', ?1)",
        )?;
        let window = format!("-{} day", days);
        let rows = stmt.query_map(params![window], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<f64>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, Option<i64>>(8)?,
                r.get::<_, Option<i64>>(9)?,
                r.get::<_, Option<i64>>(10)?,
                r.get::<_, Option<i64>>(11)?,
                r.get::<_, i64>(12)?,
                r.get::<_, i64>(13)?,
                r.get::<_, Option<i64>>(14)?,
            ))
        })?;
        // key = (day, 规则指纹)；空指纹归 ""（旧行无版本）
        let mut buckets: std::collections::BTreeMap<(String, String), DayAgg> =
            std::collections::BTreeMap::new();
        for row in rows {
            let (
                day,
                version_json,
                coverage,
                degraded_json,
                d_own,
                d_pii,
                d_range,
                d_scope,
                chunks_done,
                chunks_total,
                sentences_total,
                sentences_reviewed,
                from_history,
                from_cache,
                total_ms,
            ) = row?;
            let fingerprint = version_json
                .as_deref()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                .and_then(|v| {
                    v.get("rules_fingerprint")
                        .and_then(|f| f.as_str())
                        .map(|s| s.to_string())
                })
                .unwrap_or_default();
            let a = buckets
                .entry((day, fingerprint))
                .or_insert_with(DayAgg::default);
            a.requests += 1;
            if let Some(c) = coverage {
                a.coverage_sum += c;
                a.coverage_n += 1;
            }
            if degraded_json
                .as_deref()
                .is_some_and(|s| s.contains("_unavailable"))
            {
                a.unavailable += 1;
            }
            a.from_cache += from_cache;
            a.from_history += from_history;
            a.d_own += d_own;
            a.d_pii += d_pii;
            a.d_range += d_range;
            a.d_scope += d_scope;
            a.chunks_done += chunks_done.unwrap_or(0);
            a.chunks_total += chunks_total.unwrap_or(0);
            a.sentences_total += sentences_total.unwrap_or(0);
            a.sentences_reviewed += sentences_reviewed.unwrap_or(0);
            if let Some(ms) = total_ms {
                a.latencies.push(ms);
            }
        }
        let out: Vec<serde_json::Value> = buckets
            .into_iter()
            .map(
                |((day, fingerprint), a)| {
                    serde_json::json!({
                        "day": day,
                        "rules_fingerprint": fingerprint,
                        "requests": a.requests,
                        "coverage_avg": (a.coverage_n > 0)
                            .then(|| (a.coverage_sum / a.coverage_n as f64 * 1000.0).round() / 1000.0),
                        "sentences_reviewed_rate": (a.sentences_total > 0)
                            .then(|| (a.sentences_reviewed as f64 / a.sentences_total as f64 * 1000.0).round() / 1000.0),
                        "unavailable_rate": (a.unavailable as f64 / a.requests as f64 * 1000.0).round() / 1000.0,
                        "cache_hit_rate": (a.from_cache as f64 / a.requests as f64 * 1000.0).round() / 1000.0,
                        "history_reuse_rate": (a.from_history as f64 / a.requests as f64 * 1000.0).round() / 1000.0,
                        "dropped": {
                            "ownership": a.d_own,
                            "pii_mask": a.d_pii,
                            "out_of_range": a.d_range,
                            "out_of_scope": a.d_scope,
                        },
                        // 段级完成块数 ≈ LLM 出站调用数（预算层执行代理指标）
                        "llm_calls": a.chunks_done,
                        "chunks_total": a.chunks_total,
                        "p95_total_ms": p95(&a.latencies),
                    })
                },
            )
            .collect();
        Ok(serde_json::Value::Array(out))
    }
}

#[derive(Default)]
struct DayAgg {
    requests: i64,
    coverage_sum: f64,
    coverage_n: i64,
    unavailable: i64,
    from_cache: i64,
    from_history: i64,
    d_own: i64,
    d_pii: i64,
    d_range: i64,
    d_scope: i64,
    chunks_done: i64,
    chunks_total: i64,
    sentences_total: i64,
    sentences_reviewed: i64,
    latencies: Vec<i64>,
}

/// P95（最近秩法；空序列 null）
fn p95(values: &[i64]) -> serde_json::Value {
    if values.is_empty() {
        return serde_json::Value::Null;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let rank = ((sorted.len() as f64) * 0.95).ceil() as usize;
    serde_json::json!(sorted[rank.saturating_sub(1).min(sorted.len() - 1)])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(tag: &str) -> RequestSummaryStore {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "lf_summary_{}_{}_{}",
            tag,
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        RequestSummaryStore::open(&dir.join("t.db")).unwrap()
    }

    fn row<'a>(entry: &'static str, lang: &'a str) -> SummaryRow<'a> {
        SummaryRow {
            entry,
            lang,
            level: 1,
            quality_status: Some("full"),
            coverage: Some(1.0),
            unowned_chars: 0,
            dropped: gec_core::DroppedLedger::default(),
            degraded_json: None,
            chunks: None,
            sentences: None,
            version_json: Some(
                r#"{"rules_fingerprint":"fp-test","model_version":"m","splitter":"s1"}"#
                    .to_string(),
            ),
            error_count: 2,
            from_history: false,
            from_cache: false,
            total_ms: Some(42),
        }
    }

    #[test]
    fn insert_and_set_fallback_roundtrip() {
        let s = temp_store("rt");
        let mut r = row("analyze", "zh");
        r.error_count = 5;
        let id = s.insert(&r).unwrap();
        s.set_fallback(id, 3).unwrap();
        let a = s.alerts().unwrap();
        assert_eq!(a["quality"]["analyze_rows"], 1);
        assert_eq!(a["quality"]["fallback_sum"], 3);
        assert_eq!(a["quality"]["error_sum"], 5);
        // 样本不足 20 → 不触发（防个位数误报）
        assert_eq!(a["quality"]["triggered"], false);
    }

    #[test]
    fn integrity_alert_fires_on_dropped_or_unowned() {
        let s = temp_store("integrity");
        let mut ok = row("detect", "zh");
        s.insert(&ok).unwrap();
        let a = s.alerts().unwrap();
        assert_eq!(a["integrity"]["lossy_rows"], 0);
        assert_eq!(a["integrity"]["triggered"], false);
        // 丢弃账本非零 → 行计入；所有权空洞同理
        ok.dropped.pii_mask = 2;
        s.insert(&ok).unwrap();
        let mut hole = row("detect", "zh");
        hole.unowned_chars = 7;
        s.insert(&hole).unwrap();
        let a = s.alerts().unwrap();
        assert_eq!(a["integrity"]["lossy_rows"], 2);
        assert_eq!(a["integrity"]["triggered"], true);
    }

    #[test]
    fn replay_row_has_null_quality_and_never_triggers() {
        let s = temp_store("replay");
        let mut r = row("detect", "zh");
        r.quality_status = None;
        r.coverage = None;
        r.version_json = None;
        r.from_cache = true;
        let id = s.insert(&r).unwrap();
        assert!(id > 0);
        let a = s.alerts().unwrap();
        assert_eq!(a["integrity"]["lossy_rows"], 0, "回放行不得误触完整性告警");
    }

    #[test]
    fn daily_summary_aggregates_by_day_and_fingerprint() {
        let s = temp_store("daily");
        let chunks = gec_core::ChunkStats {
            mode: "chunked".into(),
            total: 4,
            done: 3,
            failed: 1,
            parse_failed: 0,
        };
        let mut r1 = row("detect", "zh");
        r1.chunks = Some(&chunks);
        r1.total_ms = Some(10);
        s.insert(&r1).unwrap();
        let mut r2 = row("detect", "zh");
        r2.quality_status = Some("partial");
        r2.coverage = Some(0.75);
        r2.degraded_json = Some(r#"["llm_partial"]"#.to_string());
        r2.from_cache = true;
        r2.total_ms = Some(90);
        s.insert(&r2).unwrap();
        // 另一指纹 → 另开一桶
        let mut r3 = row("analyze", "en");
        r3.version_json = Some(r#"{"rules_fingerprint":"fp-other"}"#.to_string());
        s.insert(&r3).unwrap();

        let out = s.daily_summary(14).unwrap();
        assert_eq!(out.as_array().unwrap().len(), 2, "按指纹分桶: {out}");
        let zh = out
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["rules_fingerprint"] == "fp-test")
            .unwrap();
        assert_eq!(zh["requests"], 2);
        assert_eq!(zh["chunks_total"], 4);
        assert_eq!(zh["llm_calls"], 3);
        assert_eq!(zh["cache_hit_rate"], 0.5);
        assert_eq!(zh["unavailable_rate"], 0.0);
        assert!(zh["coverage_avg"].as_f64().unwrap() > 0.87);
        assert!(zh["p95_total_ms"].as_i64().unwrap() >= 10);
        let en = out
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["rules_fingerprint"] == "fp-other")
            .unwrap();
        assert_eq!(en["requests"], 1);
    }

    #[test]
    fn partial_empty_runs_threshold() {
        let s = temp_store("partial");
        for _ in 0..5 {
            let mut r = row("detect", "zh");
            r.quality_status = Some("partial");
            r.error_count = 0;
            s.insert(&r).unwrap();
        }
        let a = s.alerts().unwrap();
        assert_eq!(a["quality"]["partial_empty_runs"], 5);
        assert_eq!(a["quality"]["triggered"], true, "partial 零检出≥5 次应触发");
    }

    #[test]
    fn p95_edge_cases() {
        assert!(p95(&[]).is_null());
        assert_eq!(p95(&[7]), 7);
        let v: Vec<i64> = (1..=100).collect();
        assert_eq!(p95(&v), 95);
    }
}
