//! 检测历史持久化（SQLite）
//!
//! 每次 detect / analyze 的输入文本和完整结果都写入 `detect_history` 表，
//! 支持按会话 ID / 语言 / 时间范围查询，以及单条删除。

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

/// 一条检测历史记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectHistory {
    pub id: i64,
    pub session_id: Option<String>,
    pub lang: String,
    pub text: String,
    /// JSON 序列化的错误列表（detect 接口返回的 Vec<Error>）
    pub errors_json: String,
    /// JSON 序列化的归因结果（analyze 接口返回的 Vec<AnalyzedError>；纯 detect 时为 null）
    pub analyzed_json: Option<String>,
    /// JSON 序列化的各阶段计时（[`gec_core::DetectTimings`]；缓存命中等未实际检测时为 null）
    #[serde(default)]
    pub timings_json: Option<String>,
    /// 产生该记录时的程序版本号（便于跨版本对比改进、按版本筛选训练数据）
    #[serde(default)]
    pub version: Option<String>,
    /// 检测时的参数快照（JSON 序列化的 [`shared_model::DetectParams`]；
    /// 旧记录为 null = 不可复用）
    #[serde(default)]
    pub params_json: Option<String>,
    /// 文本稳定哈希（[`shared_model::text_hash`]；同文本历史快速查找键；旧记录为 null）
    #[serde(default)]
    pub text_hash: Option<String>,
    /// 「应用全部建议修正后」的确定性纠正版（[`shared_model::apply_corrections`]；
    /// 数据沉淀资产 A；旧记录为 null）
    #[serde(default)]
    pub corrected_text: Option<String>,
    /// 层级贡献摘要 JSON（`gec_core::LayerReport` 数组；前端「检测路径」回放；
    /// 旧记录为 null）
    #[serde(default)]
    pub layers_json: Option<String>,
    pub created_at: String,
}

/// 检测历史存储
pub struct DetectHistoryStore {
    conn: Mutex<Connection>,
}

impl DetectHistoryStore {
    /// 从 SQLite 文件打开（不存在则创建）
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
        // (lang, text_hash) 索引在迁移后创建（旧库该列由上面迁移补出，先建会失败）
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_detect_history_lang_hash ON detect_history(lang, text_hash);",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// 写入一条检测记录
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
        conn.execute(
            "INSERT INTO detect_history (session_id, lang, text, errors_json, analyzed_json, timings_json, version, params_json, text_hash, corrected_text, layers_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![session_id, lang, text, errors_json, analyzed_json, timings_json, version, params_json, text_hash, corrected_text, layers_json, created_at],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// 行 → 记录（列序：id, session_id, lang, text, errors_json, analyzed_json,
    /// timings_json, version, params_json, text_hash, corrected_text, layers_json, created_at）
    fn map_row(row: &rusqlite::Row) -> rusqlite::Result<DetectHistory> {
        Ok(DetectHistory {
            id: row.get(0)?,
            session_id: row.get(1)?,
            lang: row.get(2)?,
            text: row.get(3)?,
            errors_json: row.get(4)?,
            analyzed_json: row.get(5)?,
            timings_json: row.get(6)?,
            version: row.get(7)?,
            params_json: row.get(8)?,
            text_hash: row.get(9)?,
            corrected_text: row.get(10)?,
            layers_json: row.get(11)?,
            created_at: row.get(12)?,
        })
    }

    /// 按条件查询历史（limit 默认 50，最大 200）——等价 list_page(q=None, offset=0)
    pub fn list(
        &self,
        session_id: Option<&str>,
        lang: Option<&str>,
        limit: i64,
    ) -> Result<Vec<DetectHistory>, rusqlite::Error> {
        self.list_page(session_id, lang, None, limit, 0)
    }

    /// list_page / count_page 共用过滤条件（session_id / lang / 全文关键词）。
    /// 关键词按「整串字面子串」匹配：用户输入的 LIKE 通配符（% _ \）全部转义；
    /// ASCII 大小写不敏感（SQLite LIKE 默认），中文区分大小写场景无影响。
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

    /// 分页查询（新 → 旧）：q = 全文关键词（字面子串），offset = 跳过行数；
    /// limit 钳制 1..=200。总数用 count_page（同过滤口径），前端据此翻页。
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
            "SELECT id, session_id, lang, text, errors_json, analyzed_json, timings_json, version, params_json, text_hash, corrected_text, layers_json, created_at
             FROM detect_history{where_sql}
             ORDER BY id DESC LIMIT ? OFFSET ?"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(args.iter()), Self::map_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// 与过滤条件匹配的记录总数（分页用；口径与 list_page 完全一致）
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

    /// 查询单条
    pub fn get(&self, id: i64) -> Result<Option<DetectHistory>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let result = conn.query_row(
            "SELECT id, session_id, lang, text, errors_json, analyzed_json, timings_json, version, params_json, text_hash, corrected_text, layers_json, created_at
             FROM detect_history WHERE id = ?1",
            params![id],
            Self::map_row,
        );
        match result {
            Ok(h) => Ok(Some(h)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// 同文本最近记录（新 → 旧）：重复检测复用历史的候选集。
    ///
    /// 按 `(lang, text_hash)` 索引查找；旧记录 `text_hash` 为 NULL 不会命中。
    /// 候选由调用方用 [`shared_model::history_reusable`] 逐条判定（含文本精确校验）。
    pub fn latest_for_text(
        &self,
        lang: &str,
        text_hash: &str,
        limit: i64,
    ) -> Result<Vec<DetectHistory>, rusqlite::Error> {
        let limit = limit.clamp(1, 20);
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT id, session_id, lang, text, errors_json, analyzed_json, timings_json, version, params_json, text_hash, corrected_text, layers_json, created_at
             FROM detect_history WHERE lang = ?1 AND text_hash = ?2
             ORDER BY id DESC LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(params![lang, text_hash, limit], Self::map_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// 删除单条
    pub fn remove(&self, id: i64) -> Result<bool, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let n = conn.execute("DELETE FROM detect_history WHERE id = ?1", params![id])?;
        Ok(n > 0)
    }

    /// 删除指定 session 的所有历史记录
    pub fn remove_by_session(&self, session_id: &str) -> Result<usize, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let n = conn.execute(
            "DELETE FROM detect_history WHERE session_id = ?1",
            params![session_id],
        )?;
        Ok(n)
    }

    /// 清空全部
    pub fn clear(&self) -> Result<usize, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let n = conn.execute("DELETE FROM detect_history", [])?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db(name: &str) -> (std::path::PathBuf, std::fs::File) {
        let dir = std::env::temp_dir().join(format!("langfixer_hist_{}", name));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("detect_history.db");
        let _ = std::fs::remove_file(&path);
        let f = std::fs::File::create(&path).unwrap();
        (path, f)
    }

    /// 旧版 schema（无 timings_json / version 列）→ open() 自动补列，读写正常
    #[test]
    fn open_migrates_legacy_schema() {
        let (path, _f) = temp_db("legacy");
        {
            // 手工建旧版表（模拟既有 langfixer.db）
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE detect_history (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    session_id TEXT,
                    lang TEXT NOT NULL,
                    text TEXT NOT NULL,
                    errors_json TEXT NOT NULL,
                    analyzed_json TEXT,
                    created_at TEXT NOT NULL DEFAULT (datetime('now'))
                );",
            )
            .unwrap();
        }
        let store = DetectHistoryStore::open(&path).unwrap();
        let id = store
            .insert(
                None,
                "en",
                "hello",
                "[]",
                None,
                Some(r#"{"rule_ms":10,"total_ms":12}"#),
                Some("0.1.0"),
                Some(r#"{"level":1,"force_llm":false,"decision_enabled":false,"l3_enabled":true,"llm_configured":false,"rules_fingerprint":"ab12"}"#),
                Some("0000000000000001"),
                None,
                None,
            )
            .unwrap();
        let h = store.get(id).unwrap().unwrap();
        assert_eq!(
            h.timings_json.as_deref(),
            Some(r#"{"rule_ms":10,"total_ms":12}"#)
        );
        assert_eq!(h.version.as_deref(), Some("0.1.0"));
        assert!(h.params_json.is_some());
        assert_eq!(h.text_hash.as_deref(), Some("0000000000000001"));
        let list = store.list(None, None, 10).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(
            list[0].timings_json.as_deref(),
            Some(r#"{"rule_ms":10,"total_ms":12}"#)
        );
        assert_eq!(list[0].version.as_deref(), Some("0.1.0"));
        assert_eq!(list[0].params_json.as_deref(), h.params_json.as_deref());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// 新库：insert / list / get 往返（含 timings_json / version / params_json / text_hash 与过滤条件）
    #[test]
    fn insert_list_get_roundtrip() {
        let (path, _f) = temp_db("roundtrip");
        let store = DetectHistoryStore::open(&path).unwrap();
        let a = store
            .insert(
                Some("s1"),
                "en",
                "a",
                "[]",
                None,
                None,
                Some("0.1.0"),
                None,
                None,
                None,
                None,
            )
            .unwrap();
        let b = store
            .insert(
                Some("s1"),
                "zh",
                "b",
                "[]",
                Some(r#"[]"#),
                Some(r#"{"total_ms":5}"#),
                Some("0.2.0"),
                Some(r#"{"level":1,"force_llm":false,"decision_enabled":true,"l3_enabled":true,"llm_configured":false,"rules_fingerprint":"cd34"}"#),
                Some("0000000000000002"),
                None,
                Some(r#"[{"layer":"rule","status":"ran","found":1}]"#),
            )
            .unwrap();
        let c = store
            .insert(
                None, "en", "c", "[]", None, None, None, None, None, None, None,
            )
            .unwrap();
        assert_eq!(store.list(Some("s1"), None, 10).unwrap().len(), 2);
        assert_eq!(store.list(None, Some("zh"), 10).unwrap().len(), 1);
        assert_eq!(store.list(None, None, 10).unwrap().len(), 3);
        let got = store.get(b).unwrap().unwrap();
        assert_eq!(got.lang, "zh");
        assert_eq!(got.timings_json.as_deref(), Some(r#"{"total_ms":5}"#));
        assert_eq!(got.version.as_deref(), Some("0.2.0"));
        assert!(got.params_json.is_some());
        assert_eq!(got.text_hash.as_deref(), Some("0000000000000002"));
        assert_eq!(
            got.layers_json.as_deref(),
            Some(r#"[{"layer":"rule","status":"ran","found":1}]"#)
        );
        assert!(store.get(c).unwrap().is_some());
        assert_eq!(store.get(c).unwrap().unwrap().version, None);
        assert!(store.remove(a).unwrap());
        assert!(!store.remove(a).unwrap());
        assert_eq!(store.clear().unwrap(), 2);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// 同文本候选查询：`latest_for_text` 按 (lang, text_hash) 命中、新 → 旧排序；
    /// 旧记录（text_hash 为 NULL）不命中
    #[test]
    fn latest_for_text_orders_newest_first_and_skips_null_hash() {
        let (path, _f) = temp_db("latest");
        let store = DetectHistoryStore::open(&path).unwrap();
        // 旧记录：text_hash 为 NULL（模拟迁移前的存量数据）
        store
            .insert(
                None,
                "zh",
                "同一文本",
                "[]",
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
        let h1 = store
            .insert(
                None,
                "zh",
                "同一文本",
                "[]",
                None,
                None,
                None,
                None,
                Some("00000000000000aa"),
                None,
                None,
            )
            .unwrap();
        let h2 = store
            .insert(
                None,
                "zh",
                "同一文本",
                "[]",
                None,
                None,
                None,
                None,
                Some("00000000000000aa"),
                None,
                None,
            )
            .unwrap();
        let got = store.latest_for_text("zh", "00000000000000aa", 10).unwrap();
        assert_eq!(got.len(), 2, "NULL text_hash 的旧记录不应命中");
        assert_eq!(got[0].id, h2, "新 → 旧排序");
        assert_eq!(got[1].id, h1);
        // 其他语言 / 其他哈希不命中
        assert!(
            store
                .latest_for_text("en", "00000000000000aa", 10)
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .latest_for_text("zh", "00000000000000bb", 10)
                .unwrap()
                .is_empty()
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// 分页 + 全文关键词：offset 翻页不重不漏；q 为字面子串（通配符转义）；
    /// count_page 与 list_page 过滤口径一致
    #[test]
    fn paging_and_keyword_search() {
        let (path, _f) = temp_db("paging");
        let store = DetectHistoryStore::open(&path).unwrap();
        let texts = [
            "第一 alpha 文本",
            "第二 beta 文本",
            "第三 alpha 文本",
            "第四 gamma 文本",
            "第五 100% 完成",
        ];
        let mut ids = Vec::new();
        for t in texts {
            ids.push(
                store
                    .insert(
                        None, "zh", t, "[]", None, None, None, None, None, None, None,
                    )
                    .unwrap(),
            );
        }
        // 全量总数
        assert_eq!(store.count_page(None, Some("zh"), None).unwrap(), 5);
        // offset 翻页（新 → 旧）：不重不漏
        let p1 = store.list_page(None, Some("zh"), None, 2, 0).unwrap();
        let p2 = store.list_page(None, Some("zh"), None, 2, 2).unwrap();
        let p3 = store.list_page(None, Some("zh"), None, 2, 4).unwrap();
        let p4 = store.list_page(None, Some("zh"), None, 2, 6).unwrap();
        assert_eq!(
            [p1.len(), p2.len(), p3.len(), p4.len()],
            [2, 2, 1, 0],
            "5 条按 2/页：2+2+1+0"
        );
        let mut seen: Vec<i64> = [&p1[..], &p2[..], &p3[..]]
            .iter()
            .flat_map(|p| p.iter().map(|h| h.id))
            .collect();
        seen.sort_unstable();
        ids.sort_unstable();
        assert_eq!(seen, ids, "翻页不重不漏");
        assert_eq!(p1[0].id, *ids.last().unwrap(), "新 → 旧排序");
        // 关键词命中（中文子串 + ASCII 大小写不敏感）
        let hit = store
            .list_page(None, Some("zh"), Some("alpha"), 10, 0)
            .unwrap();
        assert_eq!(hit.len(), 2);
        assert_eq!(
            store.count_page(None, Some("zh"), Some("ALPHA")).unwrap(),
            2
        );
        // 通配符按字面转义：「%」只命中含 % 的文本，而不是全部
        assert_eq!(store.count_page(None, Some("zh"), Some("100%")).unwrap(), 1);
        assert_eq!(store.count_page(None, Some("zh"), Some("%")).unwrap(), 1);
        // 空/空白关键词 = 不过滤
        assert_eq!(store.count_page(None, Some("zh"), Some("  ")).unwrap(), 5);
        // 语言过滤与分页正交
        assert_eq!(store.count_page(None, Some("en"), None).unwrap(), 0);
        assert!(
            store
                .list_page(None, Some("en"), None, 10, 0)
                .unwrap()
                .is_empty()
        );
        // 旧接口 list 语义不变（= list_page offset 0）
        assert_eq!(store.list(None, Some("zh"), 3).unwrap().len(), 3);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
