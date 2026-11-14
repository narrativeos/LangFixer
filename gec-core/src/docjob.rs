//! 文档级审校作业（DocJob）：作业化编排 + 块级 checkpoint / 断点续跑
//!
//! 10 万字+ 的产品形态（调研结论：同步请求范式在文档级不成立）：
//! - **请求变作业**：创建即切块落库（SQLite），后台并行执行，进度/结果随时可查；
//! - **块是工作单元**：状态 `pending / done / failed / parse_failed` 逐块持久化，
//!   每块完成即 checkpoint——进程重启 / 网络中断后 `run_doc_job` 只跑 pending 块；
//! - **覆盖率是一等公民**：`done / total` 即深度复查覆盖率，失败/解析失败块
//!   如实呈现，可 `reset_failed_chunks` 后重跑；
//! - 取消：调用方置 `AtomicBool`，未开跑的块保持 pending（作业 `cancelled`）。
//!
//! 与 [`crate::llm::LlmGecEngine::detect_document`] 的分工：后者是「即席全文
//! 复查」（同步、无持久化，供 level 3 / force_llm 路径）；本模块是持久化作业，
//! 二者共用切块（[`crate::docchunk`]）与所有权重映射（`remap_chunk_errors`）。

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, params};
use shared_model::Error;

use crate::detector::DetectorError;
use crate::docchunk::{split_document, DocChunk};
use crate::llm::{remap_chunk_errors, LlmGecEngine};
use crate::triage::{FAST_THRESHOLD, chunk_risk};

/// 作业状态机：`created → running → done / cancelled`（`cancelling` 为过渡态）
#[derive(Debug, Clone)]
pub struct DocJob {
    pub id: String,
    pub lang: String,
    pub text: String,
    pub status: String,
    pub params_json: Option<String>,
    pub chunk_target: usize,
    pub created_at: String,
}

/// 块行（send 文本不落库——切块确定性，由偏移 + 全文重建）
#[derive(Debug, Clone)]
pub struct ChunkRow {
    pub index: usize,
    pub owner: [usize; 2],
    pub context_prefix: usize,
    /// `pending / done / cached / failed / parse_failed / skipped`
    pub status: String,
    pub result_json: Option<String>,
    pub error: Option<String>,
    /// 风险分（triage 统计信号；-1 = 未计算）
    pub risk: f64,
}

impl ChunkRow {
    fn doc_chunk<'a>(&self, text: &'a str) -> Option<DocChunk> {
        let start = self.owner[0].checked_sub(self.context_prefix)?;
        if self.owner[1] > text.len() || start > self.owner[0] {
            return None;
        }
        Some(DocChunk {
            owner: self.owner,
            context_prefix: self.context_prefix,
            send: text[start..self.owner[1]].to_string(),
        })
    }
}

/// 作业进度（覆盖率核算）
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct DocJobProgress {
    pub total: usize,
    pub done: usize,
    /// 命中内容寻址缓存的块（同样计入覆盖率，未消耗 LLM）
    pub cached: usize,
    pub failed: usize,
    pub parse_failed: usize,
    pub pending: usize,
    /// 预算模式跳过的低风险块（未复查）
    pub skipped: usize,
}

impl DocJobProgress {
    /// 深度复查覆盖率：(done + cached) / total
    /// （failed / parse_failed / pending / skipped 均**不算**已复查）
    pub fn coverage(&self) -> f64 {
        if self.total == 0 {
            return 0.0;
        }
        (self.done + self.cached) as f64 / self.total as f64
    }
}

/// LLM 预算模式：风险分诊决定「哪些块值得深度复查」
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DocBudget {
    /// 全量：所有块都深度复查（默认）
    #[default]
    All,
    /// 经济：低风险（fast，`risk < FAST_THRESHOLD`）块标 `skipped`，
    /// 覆盖率如实下降——用可控的召回换成本
    SkipLowRisk,
}

pub struct DocJobStore {
    conn: Mutex<Connection>,
}

impl DocJobStore {
    pub fn open(path: &Path) -> Result<Self, rusqlite::Error> {
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self, rusqlite::Error> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self, rusqlite::Error> {
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS doc_jobs(
                 id TEXT PRIMARY KEY,
                 lang TEXT NOT NULL,
                 text TEXT NOT NULL,
                 status TEXT NOT NULL,
                 params_json TEXT,
                 chunk_target INTEGER NOT NULL,
                 created_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS doc_chunks(
                 job_id TEXT NOT NULL,
                 idx INTEGER NOT NULL,
                 owner_start INTEGER NOT NULL,
                 owner_end INTEGER NOT NULL,
                 context_prefix INTEGER NOT NULL,
                 status TEXT NOT NULL,
                 result_json TEXT,
                 error TEXT,
                 llm_ms INTEGER NOT NULL DEFAULT 0,
                 PRIMARY KEY (job_id, idx)
             );
             CREATE TABLE IF NOT EXISTS chunk_cache(
                 key TEXT PRIMARY KEY,
                 fp TEXT NOT NULL,
                 send_len INTEGER NOT NULL,
                 result_json TEXT NOT NULL,
                 created_at TEXT NOT NULL
             );")?;
        // 迁移：旧库补 risk 列（-1 = 未计算）
        let has_risk: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('doc_chunks') WHERE name = 'risk'",
            [],
            |r| r.get(0),
        )?;
        if has_risk == 0 {
            conn.execute("ALTER TABLE doc_chunks ADD COLUMN risk REAL NOT NULL DEFAULT -1", [])?;
        }
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// 创建作业并切块落库（全部块 `pending`）。返回块数。
    pub fn create_job(
        &self,
        id: &str,
        lang: &str,
        text: &str,
        params_json: Option<&str>,
        chunk_target: usize,
    ) -> Result<usize, rusqlite::Error> {
        let chunks = split_document(text, chunk_target);
        let conn = self.conn.lock().unwrap();
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO doc_jobs(id, lang, text, status, params_json, chunk_target, created_at)
             VALUES (?1, ?2, ?3, 'created', ?4, ?5, datetime('now'))",
            params![id, lang, text, params_json, chunk_target as i64],
        )?;
        for (i, c) in chunks.iter().enumerate() {
            let risk = chunk_risk(&text[c.owner[0]..c.owner[1]]).score;
            tx.execute(
                "INSERT INTO doc_chunks(job_id, idx, owner_start, owner_end, context_prefix, status, risk)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6)",
                params![
                    id,
                    i as i64,
                    c.owner[0] as i64,
                    c.owner[1] as i64,
                    c.context_prefix as i64,
                    risk
                ],
            )?;
        }
        tx.commit()?;
        Ok(chunks.len())
    }

    pub fn job(&self, id: &str) -> Result<Option<DocJob>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut q = conn
            .prepare("SELECT id, lang, text, status, params_json, chunk_target, created_at FROM doc_jobs WHERE id = ?1")?;
        let mut rows = q.query([id])?;
        match rows.next()? {
            Some(r) => Ok(Some(DocJob {
                id: r.get(0)?,
                lang: r.get(1)?,
                text: r.get(2)?,
                status: r.get(3)?,
                params_json: r.get(4)?,
                chunk_target: r.get::<_, i64>(5)? as usize,
                created_at: r.get(6)?,
            })),
            None => Ok(None),
        }
    }

    pub fn list_jobs(
        &self,
        limit: usize,
    ) -> Result<Vec<(DocJobMeta, DocJobProgress)>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut out = Vec::new();
        let metas: Vec<DocJobMeta> = {
            let mut q = conn
                .prepare("SELECT id, lang, status, created_at FROM doc_jobs ORDER BY created_at DESC LIMIT ?1")?;
            let rows = q.query_map([limit as i64], |r| {
                Ok(DocJobMeta {
                    id: r.get(0)?,
                    lang: r.get(1)?,
                    status: r.get(2)?,
                    created_at: r.get(3)?,
                })
            })?;
            rows.flatten().collect()
        };
        for m in metas {
            let p = Self::progress_conn(&conn, &m.id)?;
            out.push((m, p));
        }
        Ok(out)
    }

    /// 重启后应续跑的作业：`created`（建而未跑即终止）与 `running`
    /// （进程重启打断）。块只在完成时 checkpoint，被打断的块仍是
    /// `pending`，天然可续，无需租约。`cancelled`/`done` 不自动续
    /// （尊重用户取消意图 / 已是终态）。
    pub fn resumable_jobs(&self) -> Result<Vec<DocJob>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut q = conn.prepare(
            "SELECT id, lang, text, status, params_json, chunk_target, created_at
             FROM doc_jobs WHERE status IN ('created', 'running')
             ORDER BY created_at ASC LIMIT 50",
        )?;
        let rows = q.query_map([], |r| {
            Ok(DocJob {
                id: r.get(0)?,
                lang: r.get(1)?,
                text: r.get(2)?,
                status: r.get(3)?,
                params_json: r.get(4)?,
                chunk_target: r.get::<_, i64>(5)? as usize,
                created_at: r.get(6)?,
            })
        })?;
        Ok(rows.flatten().collect())
    }

    pub fn set_status(&self, id: &str, status: &str) -> Result<(), rusqlite::Error> {
        self.conn
            .lock()
            .unwrap()
            .execute("UPDATE doc_jobs SET status = ?2 WHERE id = ?1", params![id, status])?;
        Ok(())
    }

    fn progress_conn(conn: &Connection, id: &str) -> Result<DocJobProgress, rusqlite::Error> {
        let mut q =
            conn.prepare("SELECT status, COUNT(*) FROM doc_chunks WHERE job_id = ?1 GROUP BY status")?;
        let mut p = DocJobProgress::default();
        let rows = q.query_map([id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        for (s, n) in rows.flatten() {
            let n = n as usize;
            p.total += n;
            match s.as_str() {
                "done" => p.done = n,
                "cached" => p.cached = n,
                "skipped" => p.skipped = n,
                "failed" => p.failed = n,
                "parse_failed" => p.parse_failed = n,
                _ => p.pending += n,
            }
        }
        Ok(p)
    }

    pub fn progress(&self, id: &str) -> Result<DocJobProgress, rusqlite::Error> {
        Self::progress_conn(&self.conn.lock().unwrap(), id)
    }

    /// 待跑块：**高风险优先**（risk 降序）——进度更有价值，
    /// 取消时未跑的是低风险块；同风险按文档顺序（idx）
    pub fn pending_chunks(&self, id: &str) -> Result<Vec<ChunkRow>, rusqlite::Error> {
        self.select_chunks(id, "status = 'pending'", "risk DESC, idx ASC")
    }

    pub fn chunks(&self, id: &str) -> Result<Vec<ChunkRow>, rusqlite::Error> {
        self.select_chunks(id, "1 = 1", "idx ASC")
    }

    fn select_chunks(
        &self,
        id: &str,
        cond: &str,
        order: &str,
    ) -> Result<Vec<ChunkRow>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut q = conn.prepare(&format!(
            "SELECT idx, owner_start, owner_end, context_prefix, status, result_json, error, risk
             FROM doc_chunks WHERE job_id = ?1 AND {cond} ORDER BY {order}"
        ))?;
        let rows = q.query_map([id], |r| {
            Ok(ChunkRow {
                index: r.get::<_, i64>(0)? as usize,
                owner: [r.get::<_, i64>(1)? as usize, r.get::<_, i64>(2)? as usize],
                context_prefix: r.get::<_, i64>(3)? as usize,
                status: r.get(4)?,
                result_json: r.get(5)?,
                error: r.get(6)?,
                risk: r.get::<_, f64>(7)?,
            })
        })?;
        Ok(rows.flatten().collect())
    }

    /// 块级 checkpoint：完成一块即持久化（断点续跑的依据）
    pub fn checkpoint_chunk(
        &self,
        id: &str,
        idx: usize,
        status: &str,
        result_json: Option<&str>,
        error: Option<&str>,
        llm_ms: u64,
    ) -> Result<(), rusqlite::Error> {
        self.conn.lock().unwrap().execute(
            "UPDATE doc_chunks SET status = ?3, result_json = ?4, error = ?5, llm_ms = ?6
             WHERE job_id = ?1 AND idx = ?2",
            params![id, idx as i64, status, result_json, error, llm_ms as i64],
        )?;
        Ok(())
    }

    /// 把 failed / parse_failed 块重置为 pending（「失败块一键重跑」）
    pub fn reset_failed_chunks(&self, id: &str) -> Result<usize, rusqlite::Error> {
        let n = self.conn.lock().unwrap().execute(
            "UPDATE doc_chunks SET status = 'pending', error = NULL
             WHERE job_id = ?1 AND status IN ('failed', 'parse_failed')",
            [id],
        )?;
        Ok(n)
    }

    /// 聚合全部 done / cached 块的错误（文档坐标，按 span 排序）
    pub fn aggregated_errors(&self, id: &str) -> Result<Vec<Error>, rusqlite::Error> {
        let rows = self.select_chunks(id, "status IN ('done', 'cached')", "idx ASC")?;
        let mut out = Vec::new();
        for r in rows {
            if let Some(j) = r.result_json {
                if let Ok(v) = serde_json::from_str::<Vec<Error>>(&j) {
                    out.extend(v);
                }
            }
        }
        out.sort_by_key(|e| e.span[0]);
        Ok(out)
    }

    /// 块缓存查询：命中返回**块局部坐标**的错误（由调用方按所有权重映射）；
    /// 哈希碰撞防御 = fp + send_len 双重校验。
    pub fn cache_lookup(
        &self,
        key: &str,
        fp: &str,
        send_len: usize,
    ) -> Result<Option<Vec<Error>>, rusqlite::Error> {
        use rusqlite::OptionalExtension;
        let row = self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT result_json, send_len FROM chunk_cache WHERE key = ?1 AND fp = ?2",
                params![key, fp],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
            )
            .optional()?;
        match row {
            Some((json, len)) if len as usize == send_len => {
                Ok(Some(serde_json::from_str(&json).unwrap_or_default()))
            }
            _ => Ok(None),
        }
    }

    /// 块缓存写入（仅解析成功的结果；INSERT OR REPLACE 兼作 TTL 顺延）
    pub fn cache_store(
        &self,
        key: &str,
        fp: &str,
        send_len: usize,
        errors: &[Error],
    ) -> Result<(), rusqlite::Error> {
        let json = serde_json::to_string(errors).unwrap_or_else(|_| "[]".into());
        self.conn.lock().unwrap().execute(
            "INSERT OR REPLACE INTO chunk_cache(key, fp, send_len, result_json, created_at)
             VALUES (?1, ?2, ?3, ?4, datetime('now'))",
            params![key, fp, send_len as i64, json],
        )?;
        Ok(())
    }
}

/// 内容寻址块缓存 key：`{fp}:{hash(send)}`——
/// 相同「复查函数」（模型+提示词+格式，见 `LlmGecEngine::cache_fingerprint`）下，
/// 相同 send 文本 → 相同结果，修订文档中未变更的块天然命中（免显式 diff）。
fn chunk_cache_key(send: &str, fp: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    fp.hash(&mut h);
    send.hash(&mut h);
    format!("{fp}:{:016x}", h.finish())
}

/// 作业列表元数据（列表页不带全文）
#[derive(Debug, Clone, serde::Serialize)]
pub struct DocJobMeta {
    pub id: String,
    pub lang: String,
    pub status: String,
    pub created_at: String,
}

/// 作业默认块大小（`GEC_LLM_CHUNK_TARGET`，与即席分块路径同一环境变量）
pub fn default_chunk_target() -> usize {
    std::env::var("GEC_LLM_CHUNK_TARGET")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(crate::docchunk::DEFAULT_CHUNK_CHARS)
}

/// 作业默认并发（`GEC_LLM_CONCURRENCY`，默认 8）
pub fn default_concurrency() -> usize {
    std::env::var("GEC_LLM_CONCURRENCY")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n: &usize| *n > 0)
        .unwrap_or(8)
}

/// 采样地板：预算模式下每 N 个低风险块仍深度复查 1 个（按块序取模，
/// 重跑稳定）。防信号失效时的系统性召回盲区——即使风险信号全错，
/// 召回也有 ≈1/N 的下限，而不是塌到 0。
pub const SAMPLE_EVERY: usize = 3;

/// 运行/续跑作业：只处理 pending 块，每块完成即 checkpoint。
///
/// - `cancel`：调用方持有的取消位——置位后**未开跑**的块保持 pending，
///   作业终态 `cancelled`（已 checkpoint 的结果保留，可续跑）；
/// - `use_cache`：内容寻址块缓存（`chunk_cache`）——相同 send 文本 +
///   相同复查函数指纹 → 直接复用结果（块标 `cached`，计入覆盖率，
///   不消耗 LLM）；修订文档的未变更块天然命中，免显式 diff；
/// - `budget`：`SkipLowRisk` 下低风险块（fast）按 [`SAMPLE_EVERY`] 抽样后
///   跳过深度复查（未复查，覆盖率如实下降）；排队序恒为**高风险优先**
///   （见 `pending_chunks`）；
/// - `fast_threshold`：fast 判定阈值覆盖（`None` → [`FAST_THRESHOLD`]），
///   供 eval 扫描 cost–recall 曲线；
/// - 全部块处理完 → 作业 `done`；
/// - 返回进度 + 聚合错误（覆盖率 = `progress.coverage()`）。
pub async fn run_doc_job(
    store: Arc<DocJobStore>,
    llm: Arc<LlmGecEngine>,
    job_id: &str,
    concurrency: usize,
    use_cache: bool,
    budget: DocBudget,
    fast_threshold: Option<f64>,
    cancel: Arc<AtomicBool>,
) -> Result<(DocJobProgress, Vec<Error>), DetectorError> {
    use futures_util::StreamExt;
    let job = store
        .job(job_id)
        .map_err(|e| DetectorError::DetectionFailed(format!("作业读取失败: {e}")))?
        .ok_or_else(|| DetectorError::DetectionFailed(format!("作业不存在: {job_id}")))?;
    store
        .set_status(job_id, "running")
        .map_err(|e| DetectorError::DetectionFailed(format!("作业状态写入失败: {e}")))?;
    let pending = store
        .pending_chunks(job_id)
        .map_err(|e| DetectorError::DetectionFailed(format!("块读取失败: {e}")))?;
    let text: Arc<str> = Arc::from(job.text.as_str());
    let fp = llm.cache_fingerprint();
    let mut stream = futures_util::stream::iter(pending.into_iter().map(|row| {
        let store = store.clone();
        let llm = llm.clone();
        let cancel = cancel.clone();
        let text = text.clone();
        let fp = fp.clone();
        let job_id = job_id.to_string();
        async move {
            if cancel.load(Ordering::Relaxed) {
                return; // 保持 pending（可续跑）
            }
            // 预算模式：低风险块按采样地板抽样后跳过（skipped，不算覆盖率）。
            // 地板保证每 SAMPLE_EVERY 个低风险块至少深度复查 1 个——
            // 信号失效时召回仍有下限，不会出现系统性全盲。
            if matches!(budget, DocBudget::SkipLowRisk)
                && row.risk < fast_threshold.unwrap_or(FAST_THRESHOLD)
                && row.index % SAMPLE_EVERY != 0
            {
                store
                    .checkpoint_chunk(
                        &job_id,
                        row.index,
                        "skipped",
                        None,
                        Some("低风险块跳过深度复查（预算模式）"),
                        0,
                    )
                    .ok();
                return;
            }
            let Some(chunk) = row.doc_chunk(&text) else {
                store
                    .checkpoint_chunk(&job_id, row.index, "failed", None, Some("块偏移越界"), 0)
                    .ok();
                return;
            };
            let key = use_cache.then(|| chunk_cache_key(&chunk.send, &fp));
            if let Some(k) = &key {
                // 缓存命中：块局部坐标 → 所有权 remap → 直接 done（未消耗 LLM）
                if let Ok(Some(errors)) = store.cache_lookup(k, &fp, chunk.send.len()) {
                    let mapped = remap_chunk_errors(&chunk, errors);
                    let json = serde_json::to_string(&mapped).unwrap_or_else(|_| "[]".into());
                    store
                        .checkpoint_chunk(&job_id, row.index, "cached", Some(&json), None, 0)
                        .ok();
                    return;
                }
            }
            let t = std::time::Instant::now();
            match llm.detect_anchored(&chunk.send).await {
                Ok((errors, pf)) => {
                    let ms = t.elapsed().as_millis() as u64;
                    if pf {
                        store
                            .checkpoint_chunk(
                                &job_id,
                                row.index,
                                "parse_failed",
                                None,
                                Some("响应解析失败（该块未复查）"),
                                ms,
                            )
                            .ok();
                    } else {
                        if let Some(k) = &key {
                            // 仅缓存解析成功的**块局部**结果（remap 前坐标）
                            store.cache_store(k, &fp, chunk.send.len(), &errors).ok();
                        }
                        let mapped = remap_chunk_errors(&chunk, errors);
                        let json = serde_json::to_string(&mapped).unwrap_or_else(|_| "[]".into());
                        store
                            .checkpoint_chunk(&job_id, row.index, "done", Some(&json), None, ms)
                            .ok();
                    }
                }
                Err(e) => {
                    let ms = t.elapsed().as_millis() as u64;
                    tracing::warn!(error = %e, chunk = row.index, "文档作业块失败（记 failed，可重跑）");
                    store
                        .checkpoint_chunk(&job_id, row.index, "failed", None, Some(&e.to_string()), ms)
                        .ok();
                }
            }
        }
    }))
    .buffer_unordered(concurrency.max(1));
    while stream.next().await.is_some() {}
    // 终态：仍有 pending（取消）→ cancelled；否则 done
    let progress = store
        .progress(job_id)


        .map_err(|e| DetectorError::DetectionFailed(format!("进度读取失败: {e}")))?;
    let final_status = if progress.pending > 0 { "cancelled" } else { "done" };
    store
        .set_status(job_id, final_status)
        .map_err(|e| DetectorError::DetectionFailed(format!("作业状态写入失败: {e}")))?;
    let errors = store
        .aggregated_errors(job_id)
        .map_err(|e| DetectorError::DetectionFailed(format!("结果聚合失败: {e}")))?;
    Ok((progress, errors))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::LlmDetectConfig;
    use axum::response::IntoResponse;
    use std::sync::atomic::AtomicBool;

    fn config(base_url: &str) -> LlmDetectConfig {
        LlmDetectConfig::from_parts(
            base_url,
            "sk-test",
            "mock-model",
            5,
            "你是校对专家",
            "找出以下文本中的错误：{{text}}",
        )
        .with_retry(Vec::new(), 0)
    }

    /// 动态 mock：`respond(user)` 为 None → HTTP 400（网络层失败臂）
    async fn mock_server(respond: fn(&str) -> Option<String>) -> String {
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(move |body: axum::body::Bytes| async move {
                let req: serde_json::Value = serde_json::from_slice(&body).unwrap();
                let user = req["messages"][1]["content"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                match respond(&user) {
                    Some(content) => {
                        let resp = serde_json::json!({
                            "choices": [{ "message": { "content": content } }]
                        });
                        (axum::http::StatusCode::OK, axum::Json(resp)).into_response()
                    }
                    None => axum::http::StatusCode::BAD_REQUEST.into_response(),
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        format!("http://{addr}/v1")
    }

    fn sample_doc() -> String {
        let p1 = "第一段里有甲错需要检出，其余文字均为干净的叙述内容填充。".repeat(2);
        let p2 = "第二段是完全干净的叙述内容，没有任何需要检出的错误。".repeat(2);
        format!("{p1}\n{p2}\n")
    }

    #[test]
    fn create_job_persists_chunks() {
        let store = DocJobStore::open_in_memory().unwrap();
        let text = sample_doc();
        let n = store.create_job("j1", "zh", &text, None, 50).unwrap();
        assert!(n >= 2, "应切成多块: {n}");
        let job = store.job("j1").unwrap().unwrap();
        assert_eq!(job.status, "created");
        assert_eq!(job.text, text);
        let p = store.progress("j1").unwrap();
        assert_eq!((p.total, p.done, p.pending), (n, 0, n));
        assert_eq!(p.coverage(), 0.0);
    }

    #[test]
    fn resumable_jobs_covers_created_and_running_only() {
        let store = DocJobStore::open_in_memory().unwrap();
        let text = sample_doc();
        for id in ["a", "b", "c", "d"] {
            store.create_job(id, "zh", &text, None, 50).unwrap();
        }
        store.set_status("a", "running").unwrap(); // 跑中被打断
        store.set_status("b", "done").unwrap(); // 终态
        store.set_status("c", "cancelled").unwrap(); // 尊重取消意图
        // d 保持 created（建而未跑）
        let mut ids: Vec<String> = store
            .resumable_jobs()
            .unwrap()
            .into_iter()
            .map(|j| j.id)
            .collect();
        ids.sort();
        assert_eq!(ids, vec!["a".to_string(), "d".to_string()]);
        // 被打断的 running 作业：块仍是 pending，天然可续
        assert_eq!(store.pending_chunks("a").unwrap().len(), store.progress("a").unwrap().total);
    }

    #[tokio::test]
    async fn run_completes_aggregates_and_checkpoints() {
        // 所有权去重跨块成立：mock 对任何含第一段的请求都报「甲错」，
        // 聚合结果仍恰好 1 条且为文档字节偏移
        let respond: fn(&str) -> Option<String> = |user| {
            if user.contains("第一段") {
                Some(
                    r#"[{"error_type":"错别字","confidence":0.9,"original":"甲错","correction":"抱负","message":"音近误用"}]"#
                        .to_string(),
                )
            } else {
                Some("[]".to_string())
            }
        };
        let endpoint = mock_server(respond).await;
        let llm = Arc::new(LlmGecEngine::new("zh", config(&endpoint)));
        let store = Arc::new(DocJobStore::open_in_memory().unwrap());
        let text = sample_doc();
        let n = store.create_job("j1", "zh", &text, None, 50).unwrap();
        let (p, errors) = run_doc_job(
            store.clone(),
            llm,
            "j1",
            4,
            false,
            DocBudget::All,
            None,
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
        assert_eq!(p.done, n);
        assert_eq!(p.coverage(), 1.0);
        assert_eq!(errors.len(), 1, "所有权去重后恰好一条: {}", errors.len());
        let expect = text.find("甲错").unwrap();
        assert_eq!(errors[0].span, [expect, expect + "甲错".len()]);
        assert_eq!(store.job("j1").unwrap().unwrap().status, "done");
        // checkpoint 已持久化：done 块带 result_json
        assert!(store.chunks("j1").unwrap().iter().all(|c| c.status == "done"));
    }

    #[tokio::test]
    async fn resume_retries_failed_chunks_only() {
        // 第一轮：含「坏块」的块网络失败（400）→ failed，其余 done
        let bad: fn(&str) -> Option<String> = |user| {
            if user.contains("坏块") {
                None
            } else {
                Some("[]".to_string())
            }
        };
        let p_clean = "第一段是完全干净的叙述内容，没有任何需要检出的错误。".repeat(2);
        let p_bad = "第二段包含坏块标记，mock 对它返回失败状态。".repeat(2);
        let text = format!("{p_clean}\n{p_bad}\n");
        let endpoint = mock_server(bad).await;
        let llm_bad = Arc::new(LlmGecEngine::new("zh", config(&endpoint)));
        let store = Arc::new(DocJobStore::open_in_memory().unwrap());
        let n = store.create_job("j1", "zh", &text, None, 50).unwrap();
        let (p, _) = run_doc_job(
            store.clone(),
            llm_bad,
            "j1",
            4,
            false,
            DocBudget::All,
            None,
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
        assert_eq!(p.failed, 1, "恰好坏块失败: {p:?}");
        assert_eq!(p.done, n - 1, "其余块已 checkpoint（不重跑）");
        assert!(p.coverage() < 1.0);
        // 第二轮：reset failed → 全部块好 → done
        let good: fn(&str) -> Option<String> = |_| Some("[]".to_string());
        let endpoint = mock_server(good).await;
        let llm_good = Arc::new(LlmGecEngine::new("zh", config(&endpoint)));
        let reset = store.reset_failed_chunks("j1").unwrap();
        assert_eq!(reset, 1);
        let (p2, _) = run_doc_job(
            store.clone(),
            llm_good,
            "j1",
            4,
            false,
            DocBudget::All,
            None,
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
        assert_eq!(p2.done, n);
        assert_eq!(p2.failed, 0);
        assert_eq!(p2.coverage(), 1.0);
        assert_eq!(store.job("j1").unwrap().unwrap().status, "done");
    }

    #[tokio::test]
    async fn cancel_keeps_pending_and_status_cancelled() {
        let good: fn(&str) -> Option<String> = |_| Some("[]".to_string());
        let endpoint = mock_server(good).await;
        let llm = Arc::new(LlmGecEngine::new("zh", config(&endpoint)));
        let store = Arc::new(DocJobStore::open_in_memory().unwrap());
        let text = sample_doc();
        let n = store.create_job("j1", "zh", &text, None, 50).unwrap();
        let (p, _) = run_doc_job(store.clone(), llm, "j1", 4, false, DocBudget::All, None, Arc::new(AtomicBool::new(true)))
            .await
            .unwrap();
        assert_eq!(p.pending, n, "取消位置位 → 全部块保持 pending");
        assert_eq!(p.coverage(), 0.0);
        assert_eq!(store.job("j1").unwrap().unwrap().status, "cancelled");
    }

    #[tokio::test]
    async fn cache_hit_avoids_llm_calls() {
        // 第一轮：正常 mock（use_cache=true）→ 全部 done 并写入块缓存
        let good: fn(&str) -> Option<String> = |_| Some("[]".to_string());
        let endpoint = mock_server(good).await;
        let llm = Arc::new(LlmGecEngine::new("zh", config(&endpoint)));
        let store = Arc::new(DocJobStore::open_in_memory().unwrap());
        let text = sample_doc();
        let n = store.create_job("j1", "zh", &text, None, 50).unwrap();
        let (p, _) = run_doc_job(
            store.clone(),
            llm,
            "j1",
            4,
            true,
            DocBudget::All,
            None,
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
        assert_eq!(p.done, n);
        // 第二轮：相同文本 + **必然失败**的 mock（400）——若全部块命中缓存，
        // LLM 一次都不会被调用，failed 恒为 0
        let dead: fn(&str) -> Option<String> = |_| None;
        let endpoint = mock_server(dead).await;
        let llm_dead = Arc::new(LlmGecEngine::new("zh", config(&endpoint)));
        let n2 = store.create_job("j2", "zh", &text, None, 50).unwrap();
        assert_eq!(n2, n);
        let (p2, errors) = run_doc_job(
            store.clone(),
            llm_dead,
            "j2",
            4,
            true,
            DocBudget::All,
            None,
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
        assert_eq!(p2.cached, n, "全部块应命中缓存: {p2:?}");
        assert_eq!(p2.done, 0);
        assert_eq!(p2.failed, 0, "缓存命中路径不应触碰 LLM");
        assert_eq!(p2.coverage(), 1.0);
        assert!(errors.is_empty(), "缓存内容与命中路径结果一致（mock 恒空）");
    }

    #[tokio::test]
    async fn revised_document_rechecks_only_changed_chunks() {
        let good: fn(&str) -> Option<String> = |_| Some("[]".to_string());
        let endpoint = mock_server(good).await;
        let llm = Arc::new(LlmGecEngine::new("zh", config(&endpoint)));
        let store = Arc::new(DocJobStore::open_in_memory().unwrap());
        let p1 = "第一段是完全干净的叙述内容，没有任何需要检出的错误。".repeat(2);
        let p2 = "第二段原文也是干净的叙述内容填充文字。".repeat(2);
        let text1 = format!("{p1}\n{p2}\n");
        let (p, _) = run_doc_job(
            store.clone(),
            llm.clone(),
            {
                store.create_job("j1", "zh", &text1, None, 50).unwrap();
                "j1"
            },
            4,
            true,
            DocBudget::All,
            None,
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
        assert_eq!(p.done + p.cached, p.total);
        // 修订：p1 不变，p2 改 → p1 的块命中缓存，p2 的块重跑
        let p2b = "第二段被作者修订过，内容变了，需要重新复查。".repeat(2);
        let text2 = format!("{p1}\n{p2b}\n");
        store.create_job("j2", "zh", &text2, None, 50).unwrap();
        let (p2s, _) = run_doc_job(
            store.clone(),
            llm,
            "j2",
            4,
            true,
            DocBudget::All,
            None,
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
        assert_eq!(p2s.coverage(), 1.0);
        assert!(p2s.cached >= 1, "未变更段落应命中缓存: {p2s:?}");
        assert!(p2s.done >= 1, "变更段落必须重跑: {p2s:?}");
        assert_eq!(p2s.failed + p2s.parse_failed + p2s.pending, 0);
    }

    #[tokio::test]
    async fn risk_column_persisted_and_budget_skips_low_risk() {
        // 多个干净段（无信号 → fast）+ 高风险段（标点重复 + 括号不配对）。
        // 干净块需覆盖 index%3 != 0 的位置——采样地板下 index%3==0 的低风险块
        // 仍会被复查（保底抽样），其余才 skipped。
        let p1 = "第一段是完全干净的叙述内容。它的句子结构清晰，标点使用规范。最后一句同样没有任何异常。句间衔接自然。";
        let p2 = "第二段同样是完全干净的叙述内容。句子结构清晰，标点使用规范，没有任何需要复查的地方。段落主题明确。";
        let p3 = "第三段还是完全干净的叙述内容。通篇用词规范，句式平稳，没有任何异常信号。叙述节奏均匀。";
        let p_risky = "第四段问题很多！！标点重复了，还有（没闭合的括号。";
        let text = format!("{p1}\n{p2}\n{p3}\n{p_risky}\n");
        let good: fn(&str) -> Option<String> = |_| Some("[]".to_string());
        let endpoint = mock_server(good).await;
        let llm = Arc::new(LlmGecEngine::new("zh", config(&endpoint)));
        let store = Arc::new(DocJobStore::open_in_memory().unwrap());
        let n = store.create_job("j1", "zh", &text, None, 50).unwrap();
        assert!(n >= 4, "四段应成至少四块: {n}");
        let rows = store.chunks("j1").unwrap();
        let risky_start = text.find("第四段").unwrap();
        let risky = rows.iter().find(|c| c.owner[1] > risky_start).unwrap();
        let clean = rows.iter().find(|c| c.owner[0] == 0).unwrap();
        assert!(risky.risk > clean.risk, "风险分应持久化且高危更高: {rows:?}");
        // 经济模式：低风险块按地板抽样后 skipped，高风险块正常复查
        let (p, _) = run_doc_job(
            store.clone(),
            llm.clone(),
            "j1",
            4,
            false,
            DocBudget::SkipLowRisk,
            None,
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
        assert!(p.skipped >= 1, "低风险块应被跳过（地板之外）: {p:?}");
        assert!(p.done >= 2, "高风险块 + 地板抽样块应复查: {p:?}");
        assert!(p.coverage() < 1.0, "skipped 不算覆盖率");
        assert_eq!(store.job("j1").unwrap().unwrap().status, "done");
        // 全量模式：同样文本全部复查
        store.create_job("j2", "zh", &text, None, 50).unwrap();
        let (p2, _) = run_doc_job(
            store.clone(),
            llm,
            "j2",
            4,
            false,
            DocBudget::All,
            None,
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
        assert_eq!(p2.skipped, 0);
        assert_eq!(p2.coverage(), 1.0);
    }

    /// 文档级 eval harness（doc-detect-flow）：mucgec dev 句合成 120 句长文档，
    /// mock LLM 为「理想复查」（凡送端文本含 gold 句即报该句），度量流水线自身
    /// 的召回/误报/覆盖率/预算-召回/cache 命中，并落报告 JSON。
    ///
    /// 三臂：`full`（全量）→ 应零损耗；`economical`（跳低风险）→ 如实报告
    /// 召回换成本；`revised`（改一句 + 缓存）→ 未变更块应命中缓存。
    #[tokio::test]
    async fn doc_eval_synthetic_mucgec() {
        use std::sync::OnceLock;
        static GOLD: OnceLock<Vec<String>> = OnceLock::new();

        let raw = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../datasets/mucgec_gold.json"
        ))
        .expect("mucgec gold 缺失");
        let items: Vec<serde_json::Value> = serde_json::from_str(&raw).unwrap();
        let ok_len = |s: &str| s.chars().count() >= 8 && s.chars().count() <= 60;
        let mut gold: Vec<String> = Vec::new();
        let mut clean: Vec<String> = Vec::new();
        let mut seen: std::collections::HashSet<String> = Default::default();
        for it in &items {
            if it["split"] != "dev" {
                continue;
            }
            let s = it["src"].as_str().unwrap_or("").trim().to_string();
            if !ok_len(&s) || s.contains('\n') || !seen.insert(s.clone()) {
                continue;
            }
            let has_edits = it["edits"].as_array().is_some_and(|a| !a.is_empty());
            if has_edits && gold.len() < 60 {
                gold.push(s);
            } else if !has_edits && clean.len() < 50 {
                clean.push(s);
            }
        }
        assert!(gold.len() >= 50 && clean.len() >= 30, "gold 集供给不足");
        // 句级探针要求句互不包含（resolve_span 以整句定位，包含关系会错位）
        gold.retain(|s| !clean.iter().any(|o| o.contains(s.as_str())));
        clean.retain(|s| !gold.iter().any(|o| o.contains(s.as_str())));
        GOLD.set(gold.clone()).ok();

        // 文档：gold/clean 交替，记录每句字节区间
        let mut doc = String::new();
        let mut gold_ranges: Vec<[usize; 2]> = Vec::new();
        for (i, s) in gold.iter().enumerate() {
            let st = doc.len();
            doc.push_str(s);
            doc.push('\n');
            doc.push_str(&clean[i % clean.len()]);
            doc.push('\n');
            gold_ranges.push([st, st + s.len()]);
        }

        // 理想复查 mock：送端文本含 gold 句即报该句（含前缀区，由所有权 remap 去重）
        let respond: fn(&str) -> Option<String> = |user| {
            let mut arr: Vec<serde_json::Value> = Vec::new();
            for s in GOLD.get()? {
                if user.contains(s.as_str()) {
                    arr.push(serde_json::json!({
                        "error_type": "grammar",
                        "confidence": 0.9,
                        "original": s,
                        "correction": "（gold 探针）",
                        "message": "doc_eval gold probe",
                    }));
                }
            }
            Some(serde_json::to_string(&arr).unwrap())
        };
        let endpoint = mock_server(respond).await;
        let llm = Arc::new(LlmGecEngine::new("zh", config(&endpoint)));
        let no_cancel = || Arc::new(AtomicBool::new(false));

        // 指标：句级匹配（error span 与 gold 句区间重叠即命中）
        let metrics = |errs: &[Error]| -> (usize, usize) {
            let mut hits = 0usize;
            let mut fp = 0usize;
            for r in &gold_ranges {
                if errs.iter().any(|e| e.span[0] < r[1] && r[0] < e.span[1]) {
                    hits += 1;
                }
            }
            for e in errs {
                if !gold_ranges.iter().any(|r| e.span[0] < r[1] && r[0] < e.span[1]) {
                    fp += 1;
                }
            }
            (hits, fp)
        };

        // 臂 A：full（cache 开启，为臂 C 预热）
        let store_a = Arc::new(DocJobStore::open_in_memory().unwrap());
        store_a.create_job("full", "zh", &doc, None, 400).unwrap();
        let (pa, ea) =
            run_doc_job(store_a.clone(), llm.clone(), "full", 8, true, DocBudget::All, None, no_cancel())
                .await
                .unwrap();
        let (hits_a, fp_a) = metrics(&ea);

        // 臂 B：economical（独立 store，避免缓存影响 skipped 语义）
        let store_b = Arc::new(DocJobStore::open_in_memory().unwrap());
        store_b.create_job("econ", "zh", &doc, None, 400).unwrap();
        let (pb, eb) = run_doc_job(
            store_b.clone(),
            llm.clone(),
            "econ",
            8,
            false,
            DocBudget::SkipLowRisk,
            None,
            no_cancel(),
        )
        .await
        .unwrap();
        let (hits_b, fp_b) = metrics(&eb);
        // skip 决策质量：被跳过块中与 gold 句重叠 ≥1 的占比（越低越安全）
        let skipped_unsafe = store_b
            .chunks("econ")
            .unwrap()
            .into_iter()
            .filter(|c| c.status == "skipped")
            .filter(|c| {
                gold_ranges
                    .iter()
                    .any(|r| c.owner[0] < r[1] && r[0] < c.owner[1])
            })
            .count();

        // 臂 C：修订文档（复制一句制造重复 → 该块必须重跑，其余块应命中缓存）
        let dup = clean[7].clone();
        let revised = format!("{doc}{dup}\n");
        store_a.create_job("rev", "zh", &revised, None, 400).unwrap();
        let (pc, ec) = run_doc_job(
            store_a.clone(),
            llm.clone(),
            "rev",
            8,
            true,
            DocBudget::All,
            None,
            no_cancel(),
        )
        .await
        .unwrap();
        let (hits_c, fp_c) = metrics(&ec);
        let cache_hit_rate = if pc.cached + pc.done > 0 {
            pc.cached as f64 / (pc.cached + pc.done) as f64
        } else {
            0.0
        };

        let n_gold = gold_ranges.len();
        let report = serde_json::json!({
            "doc_chars": doc.chars().count(),
            "chunks": pa.total,
            "gold_sentences": n_gold,
            "full": {
                "coverage": pa.coverage(),
                "recall": hits_a as f64 / n_gold as f64,
                "false_positives": fp_a,
                "fp_per_1k_chars": fp_a as f64 * 1000.0 / doc.chars().count() as f64,
                "llm_calls": pa.done,
            },
            "economical": {
                "coverage": pb.coverage(),
                "recall": hits_b as f64 / n_gold as f64,
                "false_positives": fp_b,
                "skipped": pb.skipped,
                "skipped_chunks_with_gold": skipped_unsafe,
                "llm_calls": pb.done + pb.cached,
            },
            "revised": {
                "coverage": pc.coverage(),
                "recall": hits_c as f64 / n_gold as f64,
                "false_positives": fp_c,
                "cached": pc.cached,
                "done": pc.done,
                "cache_hit_rate": cache_hit_rate,
            },
        });
        let out_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../datasets/eval_reports");
        std::fs::create_dir_all(out_dir).ok();
        std::fs::write(
            format!("{out_dir}/doc_eval_latest.json"),
            serde_json::to_string_pretty(&report).unwrap(),
        )
        .ok();

        // 回归阈值：full 必须零损耗（所有权去重 + remap 不丢 gold）
        assert_eq!(pa.coverage(), 1.0);
        assert_eq!(hits_a, n_gold, "full 模式必须零漏检（gold 探针）: {report}");
        assert_eq!(fp_a, 0, "所有权去重不应产生重复/错位报告: {report}");
        // economical：确有跳过、覆盖率如实下降、召回不高于 full
        assert!(pb.skipped >= 1, "economical 应有跳过: {report}");
        assert!(pb.coverage() < 1.0);
        assert!(hits_b <= hits_a);
        // revised：多数块应命中缓存，gold 仍零漏检
        assert!(cache_hit_rate >= 0.5, "修订文档缓存命中过低: {report}");
        assert_eq!(hits_c, n_gold, "缓存 remap 不应丢 gold: {report}");
        assert_eq!(fp_c, 0);
        assert_eq!(fp_b, 0);
    }
}
