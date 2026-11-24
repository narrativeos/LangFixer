use std::path::Path;
use std::sync::Arc;

use rusqlite::Connection;

use super::core::Core;
/// 检测历史（SQLite 持久化，与 api-server 同构）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DetectHistory {
    pub id: i64,
    pub session_id: Option<String>,
    pub lang: String,
    pub text: String,
    pub errors_json: String,
    pub analyzed_json: Option<String>,
    /// JSON 序列化的各阶段计时（缓存命中等未实际检测时为 null）
    #[serde(default)]
    pub timings_json: Option<String>,
    /// 产生该记录时的程序版本号（旧记录无版本时为 null）
    #[serde(default)]
    pub version: Option<String>,
    /// 检测时的参数快照（JSON 序列化的 `shared_model::DetectParams`；旧记录为 null = 不可复用）
    #[serde(default)]
    pub params_json: Option<String>,
    /// 文本稳定哈希（`shared_model::text_hash`；同文本历史快速查找键；旧记录为 null）
    #[serde(default)]
    pub text_hash: Option<String>,
    /// 「应用全部建议修正后」的确定性纠正版（`shared_model::apply_corrections`；
    /// 数据沉淀资产 A；旧记录为 null）
    #[serde(default)]
    pub corrected_text: Option<String>,
    /// 层级贡献摘要 JSON（`gec_core::LayerReport` 数组；前端「检测路径」回放；
    /// 旧记录为 null）
    #[serde(default)]
    pub layers_json: Option<String>,
    /// 块矩阵句表 JSON（`{spans, origins}`：字节区间 + 切分来源；写入时由
    /// 全局切句器从文本重算，与实时事件流 Units 同源同口径；旧记录为 null）
    #[serde(default)]
    pub units_json: Option<String>,
    pub created_at: String,
}

/// 文本的句表（`{spans, origins}` JSON）：走全局切句器
/// [`gec_core::docchunk::sentence_spans_with_origin`]，与实时事件流 Units 同源同口径；
/// 滤除零宽片段，切不出可用句段时 None（与旧记录「无句表」口径一致）
fn units_json_for(text: &str) -> Option<String> {
    let spans: Vec<([usize; 2], gec_core::docchunk::BoundaryOrigin)> =
        gec_core::docchunk::sentence_spans_with_origin(text)
            .into_iter()
            .filter(|(s, _)| s[1] > s[0])
            .collect();
    (!spans.is_empty()).then(|| {
        serde_json::json!({
            "spans": spans.iter().map(|(s, _)| s).collect::<Vec<_>>(),
            "origins": spans.iter().map(|(_, o)| o.as_str()).collect::<Vec<_>>(),
        })
        .to_string()
    })
}

pub struct DetectHistoryStore {
    conn: std::sync::Mutex<Connection>,
}

impl DetectHistoryStore {
    pub fn open(path: &Path) -> Result<Self, rusqlite::Error> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS detect_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT,
                lang TEXT NOT NULL,
                text TEXT NOT NULL,
                errors_json TEXT NOT NULL,
                analyzed_json TEXT,
                timings_json TEXT,
                version TEXT,
                params_json TEXT,
                text_hash TEXT,
                corrected_text TEXT,
                layers_json TEXT,
                units_json TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE INDEX IF NOT EXISTS idx_detect_history_session ON detect_history(session_id);
            CREATE INDEX IF NOT EXISTS idx_detect_history_lang ON detect_history(lang);
            CREATE INDEX IF NOT EXISTS idx_detect_history_time ON detect_history(created_at DESC);",
        )?;
        // 轻量迁移：旧库缺列时逐个补齐，兼容既有 detect_history 表
        let has_timings: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('detect_history') WHERE name = 'timings_json'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0)
            > 0;
        if !has_timings {
            conn.execute_batch("ALTER TABLE detect_history ADD COLUMN timings_json TEXT")?;
        }
        let has_version: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('detect_history') WHERE name = 'version'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0)
            > 0;
        if !has_version {
            conn.execute_batch("ALTER TABLE detect_history ADD COLUMN version TEXT")?;
        }
        let has_params: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('detect_history') WHERE name = 'params_json'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0)
            > 0;
        if !has_params {
            conn.execute_batch("ALTER TABLE detect_history ADD COLUMN params_json TEXT")?;
        }
        let has_text_hash: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('detect_history') WHERE name = 'text_hash'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0)
            > 0;
        if !has_text_hash {
            conn.execute_batch("ALTER TABLE detect_history ADD COLUMN text_hash TEXT")?;
        }
        let has_corrected: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('detect_history') WHERE name = 'corrected_text'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0)
            > 0;
        if !has_corrected {
            conn.execute_batch("ALTER TABLE detect_history ADD COLUMN corrected_text TEXT")?;
        }
        let has_layers: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('detect_history') WHERE name = 'layers_json'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0)
            > 0;
        if !has_layers {
            conn.execute_batch("ALTER TABLE detect_history ADD COLUMN layers_json TEXT")?;
        }
        let has_units: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('detect_history') WHERE name = 'units_json'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0)
            > 0;
        if !has_units {
            conn.execute_batch("ALTER TABLE detect_history ADD COLUMN units_json TEXT")?;
        }
        // (lang, text_hash) 索引在迁移后创建（旧库该列由上面迁移补出，先建会失败）
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_detect_history_lang_hash ON detect_history(lang, text_hash);",
        )?;
        Ok(Self {
            conn: std::sync::Mutex::new(conn),
        })
    }

    // 参数与 detect_history 表列一一对应（与 api-server `DetectHistoryStore::insert` 同构：
    // 两端写入同一组列，消除历史行口径漂移）
    #[allow(clippy::too_many_arguments)]
    pub fn insert(
        &self,
        session_id: Option<&str>,
        lang: &str,
        text: &str,
        errors_json: &str,
        analyzed_json: Option<&str>,
        timings_json: Option<&str>,
        version: Option<&str>,
        params_json: Option<&str>,
        text_hash: Option<&str>,
        corrected_text: Option<&str>,
        layers_json: Option<&str>,
    ) -> Result<i64, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let created_at = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        // 审计 F4：句表随文本确定性沉淀（同一全局切句器），历史打开即可回放块矩阵
        let units_json = units_json_for(text);
        conn.execute(
            "INSERT INTO detect_history (session_id, lang, text, errors_json, analyzed_json, timings_json, version, params_json, text_hash, corrected_text, layers_json, units_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            rusqlite::params![session_id, lang, text, errors_json, analyzed_json, timings_json, version, params_json, text_hash, corrected_text, layers_json, units_json, created_at],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// list_page / count_page 共用过滤条件（session_id / lang / 全文关键词字面子串，
    /// LIKE 通配符转义；与 api-server 端同语义）
    fn build_filter(
        session_id: Option<&str>,
        lang: Option<&str>,
        q: Option<&str>,
    ) -> (String, Vec<String>) {
        let mut where_sql = String::from(" WHERE 1=1");
        let mut args: Vec<String> = Vec::new();
        if let Some(s) = session_id {
            where_sql.push_str(" AND session_id = ?");
            args.push(s.to_string());
        }
        if let Some(l) = lang {
            where_sql.push_str(" AND lang = ?");
            args.push(l.to_string());
        }
        if let Some(q) = q.map(str::trim).filter(|s| !s.is_empty()) {
            let escaped = q
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_");
            where_sql.push_str(" AND text LIKE ? ESCAPE '\\'");
            args.push(format!("%{escaped}%"));
        }
        (where_sql, args)
    }

    /// 分页查询（新 → 旧；limit 钳制 1..=200；offset 页偏移）
    pub fn list_page(
        &self,
        session_id: Option<&str>,
        lang: Option<&str>,
        q: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<DetectHistory>, rusqlite::Error> {
        let limit = limit.clamp(1, 200);
        let offset = offset.max(0);
        let (where_sql, mut args) = Self::build_filter(session_id, lang, q);
        args.push(limit.to_string());
        args.push(offset.to_string());
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let sql = format!(
            "SELECT id, session_id, lang, text, errors_json, analyzed_json, timings_json, version, params_json, text_hash, corrected_text, layers_json, units_json, created_at
             FROM detect_history{where_sql}
             ORDER BY id DESC LIMIT ? OFFSET ?"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(args.iter()), |r| self.map_row(r))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// 与过滤条件匹配的记录总数（分页用；口径与 list_page 一致）
    pub fn count_page(
        &self,
        session_id: Option<&str>,
        lang: Option<&str>,
        q: Option<&str>,
    ) -> Result<i64, rusqlite::Error> {
        let (where_sql, args) = Self::build_filter(session_id, lang, q);
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let n: i64 = conn.query_row(
            &format!("SELECT COUNT(*) FROM detect_history{where_sql}"),
            rusqlite::params_from_iter(args.iter()),
            |r| r.get(0),
        )?;
        Ok(n)
    }

    fn map_row(&self, r: &rusqlite::Row) -> rusqlite::Result<DetectHistory> {
        Ok(DetectHistory {
            id: r.get(0)?,
            session_id: r.get(1)?,
            lang: r.get(2)?,
            text: r.get(3)?,
            errors_json: r.get(4)?,
            analyzed_json: r.get(5)?,
            timings_json: r.get(6)?,
            version: r.get(7)?,
            params_json: r.get(8)?,
            text_hash: r.get(9)?,
            corrected_text: r.get(10)?,
            layers_json: r.get(11)?,
            units_json: r.get(12)?,
            created_at: r.get(13)?,
        })
    }

    pub fn get(&self, id: i64) -> Result<Option<DetectHistory>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT id, session_id, lang, text, errors_json, analyzed_json, timings_json, version, params_json, text_hash, corrected_text, layers_json, units_json, created_at
             FROM detect_history WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map([id], |r| self.map_row(r))?;
        rows.next().transpose()
    }

    /// 同文本最近记录（新 → 旧）：重复检测复用历史的候选集
    /// （按 `(lang, text_hash)` 索引查找；旧记录 `text_hash` 为 NULL 不会命中）
    pub fn latest_for_text(
        &self,
        lang: &str,
        text_hash: &str,
        limit: i64,
    ) -> Result<Vec<DetectHistory>, rusqlite::Error> {
        let limit = limit.clamp(1, 20);
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT id, session_id, lang, text, errors_json, analyzed_json, timings_json, version, params_json, text_hash, corrected_text, layers_json, units_json, created_at
             FROM detect_history WHERE lang = ?1 AND text_hash = ?2
             ORDER BY id DESC LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![lang, text_hash, limit], |r| {
                self.map_row(r)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn remove(&self, id: i64) -> Result<bool, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        Ok(conn.execute("DELETE FROM detect_history WHERE id = ?1", [id])? > 0)
    }

    pub fn clear(&self) -> Result<usize, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute("DELETE FROM detect_history", [])
    }

    /// 级联删除某会话的全部检测历史（与 api-server 端同语义；删会话时调用）
    pub fn remove_by_session(&self, session_id: &str) -> Result<usize, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "DELETE FROM detect_history WHERE session_id = ?1",
            [session_id],
        )
    }
}

/// 检测历史分页响应（与 Web 端 GET /gec/history 同构：items + total + 回显偏移）
#[derive(Debug, Clone, serde::Serialize)]
pub struct HistoryPage {
    pub items: Vec<DetectHistory>,
    pub total: i64,
    pub offset: i64,
    pub limit: i64,
}

/// 列出检测历史（limit 默认 50，最大 200；offset 页偏移；q 全文关键词字面子串）
#[tauri::command]
pub fn list_history(
    core: tauri::State<'_, Arc<Core>>,
    session_id: Option<String>,
    lang: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
    q: Option<String>,
) -> Result<HistoryPage, String> {
    let limit = limit.unwrap_or(50).clamp(1, 200);
    let offset = offset.unwrap_or(0).max(0);
    let items = core
        .detect_history
        .list_page(
            session_id.as_deref(),
            lang.as_deref(),
            q.as_deref(),
            limit,
            offset,
        )
        .map_err(|e| format!("查询检测历史失败: {e}"))?;
    let total = core
        .detect_history
        .count_page(session_id.as_deref(), lang.as_deref(), q.as_deref())
        .map_err(|e| format!("查询检测历史失败: {e}"))?;
    Ok(HistoryPage {
        items,
        total,
        offset,
        limit,
    })
}

/// 查看单条检测历史详情
#[tauri::command]
pub fn get_history(core: tauri::State<'_, Arc<Core>>, id: i64) -> Result<DetectHistory, String> {
    core.detect_history
        .get(id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("history#{id} 不存在"))
}

/// 删除单条检测历史
#[tauri::command]
pub fn remove_history(core: tauri::State<'_, Arc<Core>>, id: i64) -> Result<bool, String> {
    core.detect_history
        .remove(id)
        .map_err(|e| format!("删除检测历史失败: {e}"))
}

/// 清空全部检测历史
#[tauri::command]
pub fn clear_history(core: tauri::State<'_, Arc<Core>>) -> Result<usize, String> {
    core.detect_history
        .clear()
        .map_err(|e| format!("清空检测历史失败: {e}"))
}

// ─────────────────────────── 设置界面（与 Web 端 /gec/settings 同构） ───────────────────────────
