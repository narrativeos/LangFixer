//! LangFixer 规模化词表存储（SQLite：WAL + FTS5）
//!
//! 规则体系分两层，量级差异决定存储形态：
//! - **规则定义**（`rule_defs`）：每语言几十条（id / 分类 / 优先级 / 开关 / 描述）；
//! - **示例对**（`rule_examples`）：规模化主体——目标量级每语言几十万条、
//!   覆盖几十种语言（上亿行），因此必须以数据库承载，JSON 文件只作
//!   **导入/导出交换格式**（人类可编辑、git 可 diff、跨语言团队可协作）。
//!
//! 设计要点：
//! - **WAL 模式 + 10s busy timeout**：api-server 高频单行写（沉淀采纳 / 开关）
//!   与检测路径点查可并发；
//! - **`UNIQUE (lang, rule_id, wrong, right)`**：示例对判重的唯一依据
//!   （`upsert_example` 用 `INSERT ... ON CONFLICT DO NOTHING`，重复采纳不产生新行）；
//! - **FTS5 全文索引**（`wrong`/`right`，触发器同步）：支撑规则管理页的
//!   服务端搜索（`list_rules` 的 `q` 参数）；FTS5 对 CJK 按整 token 切分，
//!   故搜索对 CJK 子串不敏感——`list_rules` 在 FTS 无结果时回退
//!   `lower(wrong) LIKE`（英文子串搜索仍可用）；
//! - **自动播种**：`open` 时若库为空且提供 `seed_dir`，持 `flock` 文件锁
//!   导入目录下全部 `{lang}_gec_lexicon.json`（并行进程/重启不重复导入）；
//! - 检测器（gec-en 等）的模式规则词表（past_tenses / misspellings 等
//!   类型化表）仍由 JSON 直接消费——本库只承载「规则定义 + 示例对」这一
//!   通用可规模化层，不改变检测路径。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use fs2::FileExt;
use rusqlite::{Connection, Error as SqlError, OptionalExtension, Result as SqlResult, params};
use serde::{Deserialize, Serialize};

pub mod feedback_log;
pub mod knowledge_store;
pub mod optimization_round;
pub mod reflection_center;
pub mod training_center;
pub use feedback_log::{FeedbackLog, FeedbackLogStore, FeedbackStats};
pub use knowledge_store::KnowledgeStore;
pub use optimization_round::{OptimizationRoundRow, OptimizationRoundStore};
pub use reflection_center::{
    ReflectionCenterStore, ReflectionReportFull, ReflectionReportRow, ReflectionSuggestionRow,
    SuggestionSeed,
};
pub use training_center::{TrainingCenterStore, TrainingJobRow};

/// 规则来源：`manual`（人工/词表种子）/ `llm_distill`（LLM 兜底命中经用户确认沉淀）
pub mod sources {
    pub const MANUAL: &str = "manual";
    pub const LLM_DISTILL: &str = "llm_distill";
}

/// 规则定义（`rule_defs` 行）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleDef {
    pub id: String,
    pub lang: String,
    pub category: String,
    #[serde(default)]
    pub priority: u32,
    pub enabled: bool,
    #[serde(default)]
    pub description: String,
    /// 来源（`manual` / `llm_distill`）
    #[serde(default = "default_source")]
    pub source: String,
}

fn default_source() -> String {
    sources::MANUAL.to_string()
}

impl RuleDef {
    pub fn lang(&self) -> &str {
        &self.lang
    }
}

/// 示例对（`rule_examples` 行）：`wrong` → `right`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleExample {
    pub id: i64,
    pub lang: String,
    pub rule_id: String,
    pub wrong: String,
    pub right: String,
    #[serde(default = "default_source")]
    pub source: String,
    #[serde(default)]
    pub created_at: String,
}

/// `list_rules` 结果行：规则定义 + 示例计数
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleRow {
    pub def: RuleDef,
    /// 该规则示例对总数（分页/搜索过滤后仍为全量计数）
    pub example_count: usize,
}

/// `list_rules` 分页结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleList {
    pub lang: String,
    /// 满足过滤条件（category / q）的规则总数
    pub total: usize,
    pub page: usize,
    pub page_size: usize,
    pub rules: Vec<RuleRow>,
}

/// `list_rule_examples` 分页结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExampleList {
    pub lang: String,
    pub rule_id: String,
    /// 该规则示例对总数
    pub total: usize,
    pub page: usize,
    pub page_size: usize,
    pub examples: Vec<RuleExample>,
}

/// 词表存储（SQLite 单文件；连接以 `Mutex` 串行化）
pub struct LexiconStore {
    conn: std::sync::Mutex<Connection>,
    /// 自动播种目录（`{lang}_gec_lexicon.json`；None = 不自动播种）
    seed_dir: Option<PathBuf>,
}

/// `sync_rules_to_file` 结果：本次写入的规则 id
/// （`None` = 无新增，文件本就包含 store 全部规则）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncLexiconOutcome {
    pub updated_rule: Option<String>,
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS rule_defs (
    id TEXT NOT NULL,
    lang TEXT NOT NULL,
    category TEXT NOT NULL,
    priority INTEGER NOT NULL DEFAULT 10,
    enabled INTEGER NOT NULL DEFAULT 1,
    description TEXT NOT NULL DEFAULT '',
    source TEXT NOT NULL DEFAULT 'manual',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (lang, id)
);
CREATE TABLE IF NOT EXISTS rule_examples (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    lang TEXT NOT NULL,
    rule_id TEXT NOT NULL,
    wrong TEXT NOT NULL,
    right TEXT NOT NULL,
    source TEXT NOT NULL DEFAULT 'manual',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE (lang, rule_id, wrong, right)
);
CREATE INDEX IF NOT EXISTS idx_rule_examples_lang_rule
    ON rule_examples (lang, rule_id);
CREATE INDEX IF NOT EXISTS idx_rule_examples_wrong
    ON rule_examples (lang, lower(wrong));
CREATE VIRTUAL TABLE IF NOT EXISTS rule_examples_fts USING fts5 (
    wrong, right, content='rule_examples', content_rowid='id'
);
CREATE TRIGGER IF NOT EXISTS rule_examples_ai AFTER INSERT ON rule_examples
BEGIN
    INSERT INTO rule_examples_fts (rowid, wrong, right)
    VALUES (new.id, new.wrong, new.right);
END;
CREATE TRIGGER IF NOT EXISTS rule_examples_ad AFTER DELETE ON rule_examples
BEGIN
    INSERT INTO rule_examples_fts (rule_examples_fts, rowid, wrong, right)
    VALUES ('delete', old.id, old.wrong, old.right);
END;
CREATE TRIGGER IF NOT EXISTS rule_examples_au AFTER UPDATE ON rule_examples
BEGIN
    INSERT INTO rule_examples_fts (rule_examples_fts, rowid, wrong, right)
    VALUES ('delete', old.id, old.wrong, old.right);
    INSERT INTO rule_examples_fts (rowid, wrong, right)
    VALUES (new.id, new.wrong, new.right);
END;
"#;

impl LexiconStore {
    /// 打开（不存在则创建）SQLite 词表库；建 schema（含 FTS5 索引与触发器），
    /// 开启 WAL 模式与 10s busy timeout。
    ///
    /// `seed_dir` 非 None 且库为空时，从该目录导入全部 `{lang}_gec_lexicon.json`
    /// （持 flock 文件锁，防并行进程重复播种）。
    pub fn open(path: &Path, seed_dir: Option<&Path>) -> SqlResult<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "busy_timeout", "10000")?;
        conn.execute_batch(SCHEMA)?;
        let store = Self {
            conn: std::sync::Mutex::new(conn),
            seed_dir: seed_dir.map(PathBuf::from),
        };
        if store.is_empty()? {
            let Some(dir) = &store.seed_dir else {
                return Ok(store);
            };
            if let Err(e) = store.seed_from_dir(dir) {
                tracing::warn!(dir = %dir.display(), error = %e, "词表自动播种失败");
            }
        }
        Ok(store)
    }

    /// 内存库（测试用；不播种）
    pub fn open_in_memory() -> SqlResult<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: std::sync::Mutex::new(conn),
            seed_dir: None,
        })
    }

    /// 库是否为空（无任何规则定义）
    pub fn is_empty(&self) -> SqlResult<bool> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM rule_defs", params![], |r| {
                r.get::<_, i64>(0)
            })
            .optional()?
            .unwrap_or(0);
        Ok(n == 0)
    }

    /// 从目录导入全部 `{lang}_gec_lexicon.json`（持 flock 文件锁串行化）
    ///
    /// 返回 `(成功导入的语言数, 失败数)`；单文件失败不中断整体。
    pub fn seed_from_dir(&self, dir: &Path) -> SqlResult<(usize, usize)> {
        let lock_path = dir.join(".lexicon-seed.lock");
        // 锁文件：`create` 但不 `truncate`（内容无意义，仅用于持锁）
        let _lock: Option<std::fs::File> = match std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
        {
            Ok(f) => match f.lock_exclusive() {
                Ok(()) => Some(f),
                Err(e) => {
                    // 锁获取失败（只读目录等）时退化为无锁播种
                    //（import 幂等：重复导入不会产生重复行）
                    tracing::warn!(path = %lock_path.display(), error = %e, "播种锁获取失败，无锁播种");
                    None
                }
            },
            Err(e) => {
                tracing::warn!(path = %lock_path.display(), error = %e, "播种锁文件创建失败，无锁播种");
                None
            }
        };
        let mut ok = 0;
        let mut failed = 0;
        let mut conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let tx = match conn.transaction() {
            Ok(t) => t,
            Err(e) => {
                drop(_lock);
                return Err(e);
            }
        };
        match std::fs::read_dir(dir) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if !name.ends_with("_gec_lexicon.json") {
                        continue;
                    }
                    let lang = name
                        .strip_suffix("_gec_lexicon.json")
                        .unwrap_or(&name)
                        .to_string();
                    let content = match std::fs::read_to_string(entry.path()) {
                        Ok(c) => c,
                        Err(e) => {
                            tracing::warn!(lang = %lang, path = %entry.path().display(), error = %e, "词表读取失败");
                            failed += 1;
                            continue;
                        }
                    };
                    let v: serde_json::Value = match serde_json::from_str(&content) {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::warn!(lang = %lang, path = %entry.path().display(), error = %e, "词表解析失败");
                            failed += 1;
                            continue;
                        }
                    };
                    match self.import_lexicon_tx(&tx, &lang, &v) {
                        Ok(()) => ok += 1,
                        Err(e) => {
                            tracing::warn!(lang = %lang, path = %entry.path().display(), error = %e, "词表导入失败");
                            failed += 1;
                        }
                    }
                }
            }
            Err(e) => {
                tracing::debug!(dir = %dir.display(), error = %e, "播种目录不可读");
            }
        }
        tx.commit()?;
        drop(_lock);
        Ok((ok, failed))
    }

    /// 导入一个词表 JSON（`{lang}_gec_lexicon.json` 结构）到库
    ///
    /// 幂等：`rule_defs` upsert（保留已有 enabled / source 状态），
    /// `rule_examples` 按唯一索引去重（重复导入不产生新行）。
    /// 词表中的 `categories` 数组不落库（纯展示元数据，UI 可从 rule_defs 聚合）。
    pub fn import_lexicon(&self, lang: &str, path: &Path) -> SqlResult<()> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| SqlError::InvalidParameterName(e.to_string()))?;
        let v: serde_json::Value = serde_json::from_str(&content)
            .map_err(|e| SqlError::InvalidParameterName(e.to_string()))?;
        let mut conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let tx = conn.transaction()?;
        self.import_lexicon_tx(&tx, lang, &v)?;
        tx.commit()?;
        Ok(())
    }

    /// 从词表文件回填单条规则定义（「文件 → store」方向桥，与 `sync_rules_to_file` 相反）
    ///
    /// 背景：`upsert_example` 对缺失规则会自动创建「默认停用」的定义
    /// （LLM 蒸馏新规则默认停用、待人工审核）。但自动播种**只在库为空时**执行，
    /// 建库之后才加入文件的规则（如后期添加的种子规则）从未入库——此时直接
    /// `upsert_example` 会在 store 建出「停用」的影子定义，随后的
    /// `sync_rules_to_file` 会把该停用状态写回文件 `enabled`（连同
    /// category / priority / description），把文件里原本启用的规则关掉。
    ///
    /// 采纳路径应在 `upsert_example` 之前调用本方法：
    /// - 规则不在文件中 → `Ok(false)`，不做任何变更（真正的新规则仍走
    ///   `upsert_example` 的「默认停用」自动创建——正确行为）；
    /// - 规则在文件中 → 定义 upsert：缺失行按文件的 `category` / `priority` /
    ///   `enabled` / `description` 插入（source=`manual`）；已有行只回填
    ///   `category` / `priority` / `description`（`enabled` / `source` 是
    ///   运行时状态，原样保留——用户手动停用过的规则不会被文件默认值重新打开）；
    /// - 顺带导入该规则在文件中的示例对（`INSERT OR IGNORE`，source=`manual`），
    ///   使随后的 `sync_rules_to_file` 不会丢掉文件侧示例。
    ///
    /// 返回 `Ok(true)` = 在文件中找到该规则并已回填。
    pub fn backfill_rule_from_file(
        &self,
        dir: &Path,
        lang: &str,
        rule_id: &str,
    ) -> SqlResult<bool> {
        let file = dir.join(format!("{lang}_gec_lexicon.json"));
        let content = match std::fs::read_to_string(&file) {
            Ok(c) => c,
            Err(_) => return Ok(false), // 无文件 → 无从回填（按新规则处理）
        };
        let v: serde_json::Value = serde_json::from_str(&content)
            .map_err(|e| SqlError::InvalidParameterName(format!("词表文件解析失败: {e}")))?;
        // 规则不在文件中 → 无操作（真正的新规则仍走 `upsert_example` 的「默认停用」自动创建）
        let rule = match v.get("rules").and_then(|r| r.as_array()).and_then(|arr| {
            arr.iter()
                .find(|r| r.get("id").and_then(|x| x.as_str()) == Some(rule_id))
                .cloned()
        }) {
            Some(r) => r,
            None => return Ok(false),
        };
        let category = rule
            .get("category")
            .and_then(|x| x.as_str())
            .unwrap_or("other")
            .to_string();
        let priority = rule.get("priority").and_then(|x| x.as_u64()).unwrap_or(10) as u32;
        let enabled = rule
            .get("enabled")
            .and_then(|x| x.as_bool())
            .unwrap_or(true);
        let description = rule
            .get("description")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let examples: Vec<(String, String)> = rule
            .get("examples")
            .and_then(|x| x.as_array())
            .map(|exs| {
                exs.iter()
                    .filter_map(|ex| {
                        let wrong = ex.get("wrong")?.as_str()?;
                        let right = ex.get("right")?.as_str()?;
                        Some((wrong.to_string(), right.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default();

        let mut conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let tx = conn.transaction()?;
        let exists: Option<i64> = tx
            .query_row(
                "SELECT 1 FROM rule_defs WHERE lang = ?1 AND id = ?2",
                params![lang, rule_id],
                |r| r.get(0),
            )
            .optional()?;
        if exists.is_some() {
            // 已有定义：只回填文件侧元数据（enabled / source 为运行时状态，保留）
            tx.execute(
                "UPDATE rule_defs SET category = ?3, priority = ?4, description = ?5
                 WHERE lang = ?1 AND id = ?2",
                params![lang, rule_id, category, priority, description],
            )?;
        } else {
            // 缺失定义：按文件状态插入（此时文件是 enabled 的唯一权威来源）
            tx.execute(
                "INSERT INTO rule_defs (id, lang, category, priority, enabled, description, source)
                 VALUES (?2, ?1, ?3, ?4, ?5, ?6, 'manual')",
                params![
                    lang,
                    rule_id,
                    category,
                    priority,
                    enabled as i64,
                    description
                ],
            )?;
        }
        for (wrong, right) in &examples {
            tx.execute(
                "INSERT OR IGNORE INTO rule_examples (lang, rule_id, wrong, right, source)
                 VALUES (?1, ?2, ?3, ?4, 'manual')",
                params![lang, rule_id, wrong, right],
            )?;
        }
        tx.commit()?;
        Ok(true)
    }

    /// `import_lexicon` 的事务内核（调用方持有连接锁；供 `seed_from_dir` 复用，
    /// 避免对同一把连接锁二次加锁造成死锁）
    fn import_lexicon_tx(
        &self,
        tx: &rusqlite::Transaction,
        lang: &str,
        v: &serde_json::Value,
    ) -> SqlResult<()> {
        let rules: Vec<serde_json::Value> = v
            .get("rules")
            .and_then(|r: &serde_json::Value| r.as_array())
            .cloned()
            .unwrap_or_default();
        for r in &rules {
            let id_v: Option<&serde_json::Value> = r.get("id");
            let id = match id_v.and_then(|x| x.as_str()) {
                Some(id) => id,
                None => continue,
            };
            let category = r
                .get("category")
                .and_then(|x: &serde_json::Value| x.as_str())
                .unwrap_or("other");
            let priority = r
                .get("priority")
                .and_then(|x: &serde_json::Value| x.as_u64())
                .unwrap_or(10) as u32;
            let enabled = r
                .get("enabled")
                .and_then(|x: &serde_json::Value| x.as_bool())
                .unwrap_or(true);
            let description = r
                .get("description")
                .and_then(|x: &serde_json::Value| x.as_str())
                .unwrap_or("");
            // upsert：已有定义保留其 enabled / source（运行时状态不被种子覆盖）
            tx.execute(
                "INSERT INTO rule_defs (id, lang, category, priority, enabled, description, source)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'manual')
                 ON CONFLICT (lang, id) DO UPDATE SET
                     category = excluded.category,
                     priority = excluded.priority,
                     description = excluded.description",
                params![id, lang, category, priority, enabled as i64, description],
            )?;
            if let Some(exs) = r
                .get("examples")
                .and_then(|x: &serde_json::Value| x.as_array())
            {
                for ex in exs {
                    let wrong_v: Option<&serde_json::Value> = ex.get("wrong");
                    let right_v: Option<&serde_json::Value> = ex.get("right");
                    let (wrong, right) = match (
                        wrong_v.and_then(|x| x.as_str()),
                        right_v.and_then(|x| x.as_str()),
                    ) {
                        (Some(w), Some(rt)) => (w, rt),
                        _ => continue,
                    };
                    tx.execute(
                        "INSERT OR IGNORE INTO rule_examples (lang, rule_id, wrong, right, source)
                         VALUES (?1, ?2, ?3, ?4, 'manual')",
                        params![lang, id, wrong, right],
                    )?;
                }
            }
        }
        Ok(())
    }

    /// 导出指定语言的词表 JSON（与导入格式同构：`lang` / `rules`；
    /// 示例对按库内顺序展开）
    pub fn export_lexicon(&self, lang: &str) -> SqlResult<serde_json::Value> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT id, lang, category, priority, enabled, description, source
             FROM rule_defs WHERE lang = ?1 ORDER BY id",
        )?;
        let defs: Vec<RuleDef> = stmt
            .query_map(params![lang], |row| {
                Ok(RuleDef {
                    id: row.get(0)?,
                    lang: row.get(1)?,
                    category: row.get(2)?,
                    priority: row.get::<_, i64>(3)? as u32,
                    enabled: row.get::<_, i64>(4)? != 0,
                    description: row.get(5)?,
                    source: row.get(6)?,
                })
            })?
            .collect::<SqlResult<_>>()?;
        let mut ex_stmt = conn.prepare(
            "SELECT wrong, right FROM rule_examples WHERE lang = ?1 AND rule_id = ?2 ORDER BY id",
        )?;
        let mut rules = Vec::with_capacity(defs.len());
        for d in defs {
            let examples: Vec<serde_json::Value> = ex_stmt
                .query_map(params![lang, d.id], |row| {
                    Ok(serde_json::json!({
                        "wrong": row.get::<_, String>(0)?,
                        "right": row.get::<_, String>(1)?,
                    }))
                })?
                .collect::<SqlResult<_>>()?;
            rules.push(serde_json::json!({
                "id": d.id,
                "category": d.category,
                "priority": d.priority,
                "enabled": d.enabled,
                "description": d.description,
                "examples": examples,
            }));
        }
        Ok(serde_json::json!({ "lang": lang, "rules": rules }))
    }

    /// 将 store 中的规则同步回词表文件 `{dir}/{lang}_gec_lexicon.json`
    ///
    /// 检测器（gec-en 等）只在**构造时**从词表文件加载规则——store 里新采纳的
    /// 规则不同步回文件就永远不会被检出。本方法是「store → 文件」方向的桥：
    /// - **文件必须已存在**，否则返回 `Err`（该语言无词表 → 规则无法被检测器
    ///   加载，调用方应走知识分支）；
    /// - 合并策略：store 导出的规则按 `id` upsert（store 为准，含运行时开关
    ///   状态）；文件中 store 没有的规则**原样保留**（防误删）；
    /// - 其余 section（`past_tenses` / `misspellings` / `categories` …）原样保留；
    /// - 原子写回（临时文件 + rename）。
    pub fn sync_rules_to_file(&self, dir: &Path, lang: &str) -> SqlResult<SyncLexiconOutcome> {
        let file = dir.join(format!("{lang}_gec_lexicon.json"));
        let content = std::fs::read_to_string(&file).map_err(|e| {
            SqlError::InvalidParameterName(format!(
                "词表文件不存在，无法同步规则: {} ({e})",
                file.display()
            ))
        })?;
        let mut v: serde_json::Value = serde_json::from_str(&content).map_err(|e| {
            SqlError::InvalidParameterName(format!("词表文件解析失败: {} ({e})", file.display()))
        })?;

        let exported = self.export_lexicon(lang)?;
        let exported_rules = exported
            .get("rules")
            .cloned()
            .unwrap_or_else(|| serde_json::json!([]));

        // 按 id 合并：**保持文件内规则原序**（upsert 原位更新，新规则追加末尾）——
        // 不用 HashMap（迭代顺序随机，写回会打乱 rules 数组，扰动检测结果的展示顺序）
        let mut merged: Vec<serde_json::Value> = v
            .get("rules")
            .and_then(|r| r.as_array())
            .map(|a| a.clone())
            .unwrap_or_default();
        let mut updated: Option<String> = None;
        if let Some(arr) = exported_rules.as_array() {
            for r in arr {
                if let Some(id) = r.get("id").and_then(|v| v.as_str()) {
                    if let Some(pos) = merged
                        .iter()
                        .position(|m| m.get("id").and_then(|v| v.as_str()) == Some(id))
                    {
                        // store 导出规则不承载 `patterns` / `excludes`（数据以词表文件为准）；
                        // upsert 覆盖时保留文件侧已有条目，避免误删
                        let mut r = r.clone();
                        let prev = &merged[pos];
                        if let Some(patterns) = prev.get("patterns") {
                            r["patterns"] = patterns.clone();
                        }
                        if let Some(excludes) = prev.get("excludes") {
                            r["excludes"] = excludes.clone();
                        }
                        merged[pos] = r;
                    } else {
                        merged.push(r.clone());
                        updated = Some(id.to_string());
                    }
                }
            }
        }
        v["rules"] = serde_json::Value::Array(merged);

        // 原子写回（临时文件 + rename，避免半写文件被检测器读到）
        let tmp = dir.join(format!(".{lang}_gec_lexicon.json.tmp"));
        let out = serde_json::to_string_pretty(&v)
            .map_err(|e| SqlError::InvalidParameterName(e.to_string()))?;
        std::fs::write(&tmp, out)
            .map_err(|e| SqlError::InvalidParameterName(format!("词表写入失败: {e}")))?;
        std::fs::rename(&tmp, &file)
            .map_err(|e| SqlError::InvalidParameterName(format!("词表落盘失败: {e}")))?;

        Ok(SyncLexiconOutcome {
            updated_rule: updated,
        })
    }

    /// 列出规则（分页 + 可选分类过滤 + 可选搜索）
    ///
    /// `q` 搜索策略：FTS5（wrong/right 分词匹配）为主；FTS 无命中时回退
    /// `lower(wrong) LIKE %q%` + `lower(id)/lower(description) LIKE`
    /// （FTS5 默认分词器对 CJK 子串不敏感，LIKE 兜底保证英文子串搜索可用）。
    /// 排序：示例数降序 → id 升序；`page` 从 1 起，`page_size` 上限 200。
    pub fn list_rules(
        &self,
        lang: &str,
        category: Option<&str>,
        q: Option<&str>,
        page: usize,
        page_size: usize,
    ) -> SqlResult<RuleList> {
        let page = page.max(1);
        let page_size = page_size.clamp(1, 200);
        let q = q.map(str::trim).filter(|s| !s.is_empty());
        self.run_list_query(lang, category, q, page, page_size)
    }

    /// 构建规则列表查询（`use_fts` = FTS5 主搜索；否则 LIKE 兜底）。
    /// 返回 `(select_list, from_where, binds)`：
    /// - `select_list`：主查询的 SELECT 列（含示例计数子查询）；
    /// - `from_where`：`FROM rule_defs d WHERE ...`（count 查询用 `SELECT 1` 复用）；
    /// - `binds`：依次为 category / 搜索词（**不含 lang**，调用方在 `?1` 位置补 lang；
    ///   不含分页值——LIMIT/OFFSET 由 `run_list_query` 在 count 之后追加）。
    fn build_list_query(
        &self,
        category: Option<&str>,
        q: Option<&str>,
        use_fts: bool,
    ) -> (String, String, Vec<String>) {
        let select_list = String::from(
            "SELECT d.id, d.lang, d.category, d.priority, d.enabled, d.description, d.source,
                    (SELECT COUNT(*) FROM rule_examples e WHERE e.lang = d.lang AND e.rule_id = d.id) AS cnt",
        );
        let mut from_where = String::from("FROM rule_defs d WHERE d.lang = ?1");
        let mut binds: Vec<String> = Vec::new();
        if let Some(category) = category {
            from_where.push_str(" AND d.category = ?2");
            binds.push(category.to_string());
        }
        if let Some(query) = q {
            if use_fts {
                // FTS5 查询串：按空白分词，每词加双引号转义，OR 连接
                let fts_q = query
                    .split_whitespace()
                    .map(|t| format!("\"{}\"", t.replace('"', "")))
                    .collect::<Vec<_>>()
                    .join(" OR ");
                if !fts_q.is_empty() {
                    let idx = if category.is_some() { 3 } else { 2 };
                    from_where.push_str(&format!(
                        " AND d.id IN (SELECT e.rule_id FROM rule_examples e
                          JOIN rule_examples_fts f ON f.rowid = e.id
                          WHERE e.lang = d.lang AND f.rule_examples_fts MATCH ?{})",
                        idx
                    ));
                    binds.push(fts_q);
                }
            } else {
                let like = format!("%{}%", query.to_lowercase());
                let idx = if category.is_some() { 3 } else { 2 };
                from_where.push_str(&format!(
                    " AND (lower(d.id) LIKE ?{} OR lower(d.description) LIKE ?{}
                          OR EXISTS (SELECT 1 FROM rule_examples e
                                    WHERE e.lang = d.lang AND e.rule_id = d.id
                                      AND lower(e.wrong) LIKE ?{}))",
                    idx,
                    idx + 1,
                    idx + 2
                ));
                binds.push(like.clone());
                binds.push(like.clone());
                binds.push(like);
            }
        }
        (select_list, from_where, binds)
    }

    /// 持锁执行列表查询：count →（FTS 无命中时 LIKE 回退）→ 分页切片
    fn run_list_query(
        &self,
        lang: &str,
        category: Option<&str>,
        q: Option<&str>,
        page: usize,
        page_size: usize,
    ) -> SqlResult<RuleList> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let (select_list, from_where, binds) = self.build_list_query(category, q, q.is_some());
        let count_for = |conn: &Connection, from_where: &String, binds: &Vec<String>| -> i64 {
            let count_sql = format!("SELECT COUNT(*) FROM (SELECT 1 {} )", from_where);
            let mut all: Vec<String> = vec![lang.to_string()];
            all.extend_from_slice(binds);
            let mut cs = match conn.prepare(&count_sql) {
                Ok(cs) => cs,
                Err(_) => return 0,
            };
            match cs
                .query_map(rusqlite::params_from_iter(all.iter()), |r| {
                    r.get::<_, i64>(0)
                })
                .and_then(|mut it| it.next().transpose())
            {
                Ok(Some(n)) => n,
                _ => 0,
            }
        };

        let total = count_for(&conn, &from_where, &binds);
        let (select_list, from_where, binds) = if q.is_some() && total == 0 {
            // FTS 无命中 → LIKE 回退（子串搜索兜底）
            let (sl2, fw2, binds2) = self.build_list_query(category, q, false);
            let total2 = count_for(&conn, &fw2, &binds2);
            if total2 > 0 {
                (sl2, fw2, binds2)
            } else {
                (select_list, from_where, binds)
            }
        } else {
            (select_list, from_where, binds)
        };
        let total = count_for(&conn, &from_where, &binds);

        let mut all: Vec<String> = vec![lang.to_string()];
        all.extend_from_slice(&binds);
        let sql =
            format!("{select_list} {from_where} ORDER BY cnt DESC, d.id ASC LIMIT ? OFFSET ?",);
        all.push(page_size.to_string());
        all.push(((page - 1) * page_size).to_string());
        let mut stmt = conn.prepare(&sql)?;
        let rows: Vec<(RuleDef, i64)> = stmt
            .query_map(rusqlite::params_from_iter(all.iter()), |row| {
                Ok((
                    RuleDef {
                        id: row.get(0)?,
                        lang: row.get(1)?,
                        category: row.get(2)?,
                        priority: row.get::<_, i64>(3)? as u32,
                        enabled: row.get::<_, i64>(4)? != 0,
                        description: row.get(5)?,
                        source: row.get(6)?,
                    },
                    row.get(7)?,
                ))
            })?
            .collect::<SqlResult<_>>()?;
        let rules = rows
            .into_iter()
            .map(|(def, cnt)| RuleRow {
                def,
                example_count: cnt as usize,
            })
            .collect();
        Ok(RuleList {
            lang: lang.to_string(),
            total: total as usize,
            page,
            page_size,
            rules,
        })
    }

    /// 列出某语言的分类聚合（`category` → 规则数；UI 分类过滤下拉用）
    pub fn list_categories(&self, lang: &str) -> SqlResult<Vec<(String, i64)>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT category, COUNT(*) FROM rule_defs WHERE lang = ?1
             GROUP BY category ORDER BY category",
        )?;
        stmt.query_map(params![lang], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<SqlResult<_>>()
    }

    /// 列出某规则的示例对（分页；`page` 从 1 起，`page_size` 上限 200）
    pub fn list_rule_examples(
        &self,
        lang: &str,
        rule_id: &str,
        page: usize,
        page_size: usize,
    ) -> SqlResult<ExampleList> {
        let page = page.max(1);
        let page_size = page_size.clamp(1, 200);
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let total: i64 = conn.query_row(
            "SELECT COUNT(*) FROM rule_examples WHERE lang = ?1 AND rule_id = ?2",
            params![lang, rule_id],
            |r| r.get::<_, i64>(0),
        )?;
        let mut stmt = conn.prepare(
            "SELECT id, lang, rule_id, wrong, right, source, created_at
             FROM rule_examples WHERE lang = ?1 AND rule_id = ?2
             ORDER BY id LIMIT ?3 OFFSET ?4",
        )?;
        let examples: Vec<RuleExample> = stmt
            .query_map(
                params![
                    lang,
                    rule_id,
                    page_size as i64,
                    (page - 1) as i64 * page_size as i64
                ],
                |row| {
                    Ok(RuleExample {
                        id: row.get(0)?,
                        lang: row.get(1)?,
                        rule_id: row.get(2)?,
                        wrong: row.get(3)?,
                        right: row.get(4)?,
                        source: row.get(5)?,
                        created_at: row.get(6)?,
                    })
                },
            )?
            .collect::<SqlResult<_>>()?;
        Ok(ExampleList {
            lang: lang.to_string(),
            rule_id: rule_id.to_string(),
            total: total as usize,
            page,
            page_size,
            examples,
        })
    }

    /// 写入一条示例对（幂等）
    ///
    /// - 规则定义不存在时自动创建（`source` 记入，`enabled = 0`
    ///   ——LLM 沉淀的规则默认停用，待人工审核后开启）；
    /// - 示例对按 `UNIQUE (lang, rule_id, wrong, right)` 判重：
    ///   返回 `Ok(true)` = 新增，`Ok(false)` = 已存在（未写入）。
    pub fn upsert_example(
        &self,
        lang: &str,
        rule_id: &str,
        wrong: &str,
        right: &str,
        source: &str,
    ) -> SqlResult<bool> {
        let mut conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO rule_defs (id, lang, category, priority, enabled, description, source)
             VALUES (?1, ?2, 'other', 10, 0, '', ?3)
             ON CONFLICT (lang, id) DO NOTHING",
            params![rule_id, lang, source],
        )?;
        let n = tx.execute(
            "INSERT INTO rule_examples (lang, rule_id, wrong, right, source)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (lang, rule_id, wrong, right) DO NOTHING",
            params![lang, rule_id, wrong, right, source],
        )?;
        tx.commit()?;
        Ok(n > 0)
    }

    /// 删除一条示例对（反思流程「删除示例」建议的执行端）
    ///
    /// 按 `UNIQUE (lang, rule_id, wrong, right)` 定位；FTS 索引由触发器同步。
    /// 返回 `true` = 实际删除，`false` = 示例不存在。
    pub fn delete_example(
        &self,
        lang: &str,
        rule_id: &str,
        wrong: &str,
        right: &str,
    ) -> SqlResult<bool> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let n = conn.execute(
            "DELETE FROM rule_examples WHERE lang = ?1 AND rule_id = ?2 AND wrong = ?3 AND right = ?4",
            params![lang, rule_id, wrong, right],
        )?;
        Ok(n > 0)
    }

    /// 按 `wrong` 删除规则下**全部**示例对（修正侧未知时的兜底；
    /// agent `remove_example` 省略 `right` 时用）。返回删除条数。
    pub fn delete_example_by_wrong(
        &self,
        lang: &str,
        rule_id: &str,
        wrong: &str,
    ) -> SqlResult<usize> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let n = conn.execute(
            "DELETE FROM rule_examples WHERE lang = ?1 AND rule_id = ?2 AND wrong = ?3",
            params![lang, rule_id, wrong],
        )?;
        Ok(n)
    }

    /// 切换规则开关；规则不存在时返回 `Err(QueryReturnedNoRows)`。
    ///
    /// 注意：只写 store，不即时改词表文件；`enabled` 以 store 为权威
    /// （检测路径经 `get_rules` 从 store 过滤禁用规则，故停用即时生效、
    /// 无需重建检测器；`sync_rules_to_file` 时 store 值回写文件）。
    pub fn toggle_rule(&self, lang: &str, rule_id: &str, enabled: bool) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let n = conn.execute(
            "UPDATE rule_defs SET enabled = ?3 WHERE lang = ?1 AND id = ?2",
            params![lang, rule_id, enabled as i64],
        )?;
        if n == 0 {
            return Err(SqlError::QueryReturnedNoRows);
        }
        Ok(())
    }

    /// 点查单条规则定义（检测路径过滤禁用规则用）
    pub fn get_rule(&self, lang: &str, rule_id: &str) -> SqlResult<Option<RuleDef>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let row: Option<RuleDef> = conn
            .query_row(
                "SELECT id, lang, category, priority, enabled, description, source
                 FROM rule_defs WHERE lang = ?1 AND id = ?2",
                params![lang, rule_id],
                |r| {
                    Ok(RuleDef {
                        id: r.get(0)?,
                        lang: r.get(1)?,
                        category: r.get(2)?,
                        priority: r.get::<_, i64>(3)? as u32,
                        enabled: r.get::<_, i64>(4)? != 0,
                        description: r.get(5)?,
                        source: r.get(6)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    /// 批量点查规则定义（检测路径：一次性过滤禁用规则）
    ///
    /// `rule_ids` → 对应 `RuleDef` 列表（未入库 → `None`，顺序与输入一致）。
    /// 一次全表读（规则表小：数百行量级）+ 内存匹配，
    /// 避免逐条 `get_rule` 的 N+1（每条一次加锁点查）。
    pub fn get_rules(&self, lang: &str, rule_ids: &[&str]) -> SqlResult<Vec<Option<RuleDef>>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT id, lang, category, priority, enabled, description, source
                      FROM rule_defs WHERE lang = ?1",
        )?;
        let rows = stmt.query_map(params![lang], |r| {
            Ok(RuleDef {
                id: r.get(0)?,
                lang: r.get(1)?,
                category: r.get(2)?,
                priority: r.get::<_, i64>(3)? as u32,
                enabled: r.get::<_, i64>(4)? != 0,
                description: r.get(5)?,
                source: r.get(6)?,
            })
        })?;
        let mut map = std::collections::HashMap::new();
        for d in rows.filter_map(std::result::Result::ok) {
            map.insert(d.id.clone(), d);
        }
        Ok(rule_ids.iter().map(|id| map.get(*id).cloned()).collect())
    }

    /// 某语言当前**禁用**的规则 id 列表（升序）——规则侧版本指纹的输入之一
    /// （`shared_model::rules_fingerprint`；禁用集变化 → 指纹变化 → 历史不可复用）
    pub fn disabled_rule_ids(&self, lang: &str) -> SqlResult<Vec<String>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt =
            conn.prepare("SELECT id FROM rule_defs WHERE lang = ?1 AND enabled = 0 ORDER BY id")?;
        let rows = stmt
            .query_map(params![lang], |r| r.get::<_, String>(0))?
            .collect::<SqlResult<Vec<_>>>();
        rows
    }

    /// 迁移旧版 JSON 开关状态（`{rule_id: enabled}`，原 `GEC_RULE_STATE_FILE`）：
    /// 逐条覆盖库内 `enabled`。须在播种之后调用（播种保留已有状态，
    /// 旧文件代表比词表默认更新的运行时状态）。返回实际应用的条数。
    ///
    /// 旧文件不带语言维度，故按 `id` 反查其所属 `lang` 再更新
    ///（rule_id 跨语言唯一性由命名约定保证，如 `EN_*` / `ZH_*`）。
    pub fn migrate_rule_state(&self, old: &HashMap<String, bool>) -> SqlResult<usize> {
        let mut conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let tx = conn.transaction()?;
        let mut n = 0;
        for (id, enabled) in old {
            let lang: Option<String> = tx
                .query_row(
                    "SELECT lang FROM rule_defs WHERE id = ?1 LIMIT 1",
                    params![id],
                    |r| r.get(0),
                )
                .optional()?;
            let applied = match lang {
                Some(lang) => tx
                    .execute(
                        "UPDATE rule_defs SET enabled = ?3 WHERE lang = ?1 AND id = ?2",
                        params![lang, id, *enabled as i64],
                    )
                    .map(|c| c > 0)
                    .unwrap_or(false),
                None => false,
            };
            if applied {
                n += 1;
            }
        }
        tx.commit()?;
        Ok(n)
    }

    /// 按规则 id 点查（不带语言维度；rule_id 命名约定保证跨语言唯一，
    /// 如 `EN_*` / `ZH_*`，开关路由无需调用方传 lang）
    pub fn rule_defs_by_id(&self, rule_id: &str) -> SqlResult<Option<RuleDef>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let row: Option<RuleDef> = conn
            .query_row(
                "SELECT id, lang, category, priority, enabled, description, source
                 FROM rule_defs WHERE id = ?1 LIMIT 1",
                params![rule_id],
                |r| {
                    Ok(RuleDef {
                        id: r.get(0)?,
                        lang: r.get(1)?,
                        category: r.get(2)?,
                        priority: r.get::<_, i64>(3)? as u32,
                        enabled: r.get::<_, i64>(4)? != 0,
                        description: r.get(5)?,
                        source: r.get(6)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    /// 列出某语言的规则定义（不分页；规则定义量级小，供开关播种等内部用途）
    pub fn rule_defs(&self, lang: &str) -> SqlResult<Vec<RuleDef>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT id, lang, category, priority, enabled, description, source
             FROM rule_defs WHERE lang = ?1 ORDER BY id",
        )?;
        stmt.query_map(params![lang], |row| {
            Ok(RuleDef {
                id: row.get(0)?,
                lang: row.get(1)?,
                category: row.get(2)?,
                priority: row.get::<_, i64>(3)? as u32,
                enabled: row.get::<_, i64>(4)? != 0,
                description: row.get(5)?,
                source: row.get(6)?,
            })
        })?
        .collect::<SqlResult<_>>()
    }
}

/// 一次性迁移旧版 JSON 规则开关状态文件（`{rule_id: enabled}`，原
/// `GEC_RULE_STATE_FILE`）到库：逐条覆盖 `enabled`（须在播种之后调用——
/// 播种保留已有状态，旧文件代表比词表默认更新的运行时状态），
/// 成功后把原文件归档为 `<原名>.migrated`。
///
/// 文件不存在时返回 `Ok(0)`；JSON 解析失败返回 `Err`（文件保留，不归档）。
/// 返回实际应用的条数。api-server 与 desktop-app 共用，保证迁移行为一致。
pub fn migrate_rule_state_file(store: &LexiconStore, path: &Path) -> SqlResult<usize> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Ok(0),
    };
    let old: HashMap<String, bool> = serde_json::from_str(&content)
        .map_err(|e| SqlError::InvalidParameterName(e.to_string()))?;
    let n = store.migrate_rule_state(&old)?;
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_default();
    let archived = path.with_extension(format!("{ext}.migrated"));
    if let Err(e) = std::fs::rename(path, &archived) {
        tracing::warn!(
            path = %path.display(),
            error = %e,
            "旧版规则开关文件归档失败（状态已入库，可手动删除）"
        );
    }
    Ok(n)
}

/// 向规则的排除词表追加一个词（词表文件 `rules[].excludes`，与 `patterns` 同模式：
/// 文件侧数据，store 不承载；写入后调用方重建检测器即时生效）
///
/// 用途：反思建议「将某词加入规则排除名单」——用户确认该词不触发本规则
/// （典型：POS 把名词误标为动词），规则保持启用，仅该词在检测器侧不再触发。
///
/// 返回 `true` = 新写入；`false` = 已存在（幂等）。规则不在词表文件中 → `Err`。
pub fn append_rule_exclude(dir: &Path, lang: &str, rule_id: &str, word: &str) -> SqlResult<bool> {
    let file = dir.join(format!("{lang}_gec_lexicon.json"));
    let content = std::fs::read_to_string(&file).map_err(|e| {
        SqlError::InvalidParameterName(format!("词表文件读取失败（{}）: {e}", file.display()))
    })?;
    let mut v: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| SqlError::InvalidParameterName(e.to_string()))?;
    let rules = v
        .get_mut("rules")
        .and_then(|r| r.as_array_mut())
        .ok_or_else(|| {
            SqlError::InvalidParameterName(format!("词表文件缺少 rules 数组: {}", file.display()))
        })?;
    let rule = rules
        .iter_mut()
        .find(|r| r.get("id").and_then(|v| v.as_str()) == Some(rule_id))
        .ok_or_else(|| {
            SqlError::InvalidParameterName(format!(
                "规则 {rule_id} 不在词表文件中（{}）",
                file.display()
            ))
        })?;
    let excludes = match rule.get_mut("excludes") {
        Some(a @ serde_json::Value::Array(_)) => a.as_array_mut().unwrap(),
        _ => {
            rule["excludes"] = serde_json::Value::Array(Vec::new());
            rule["excludes"].as_array_mut().unwrap()
        }
    };
    if excludes.iter().any(|w| w.as_str() == Some(word)) {
        return Ok(false); // 已存在，不重复写入
    }
    excludes.push(serde_json::Value::String(word.to_string()));
    // 原子写回（临时文件 + rename，避免半写文件被检测器读到）
    let tmp = dir.join(format!(".{lang}_gec_lexicon.json.tmp"));
    let out = serde_json::to_string_pretty(&v)
        .map_err(|e| SqlError::InvalidParameterName(e.to_string()))?;
    std::fs::write(&tmp, out)
        .map_err(|e| SqlError::InvalidParameterName(format!("词表写入失败: {e}")))?;
    std::fs::rename(&tmp, &file)
        .map_err(|e| SqlError::InvalidParameterName(format!("词表落盘失败: {e}")))?;
    Ok(true)
}

/// 从规则的排除词表移除一个词（[`append_rule_exclude`] 的逆操作；
/// agent 自主层「撤销误加的排除词」用）
///
/// 返回 `true` = 已移除；`false` = 不在排除名单（幂等）。规则不在词表文件中 → `Err`。
pub fn remove_rule_exclude(dir: &Path, lang: &str, rule_id: &str, word: &str) -> SqlResult<bool> {
    let file = dir.join(format!("{lang}_gec_lexicon.json"));
    let content = std::fs::read_to_string(&file).map_err(|e| {
        SqlError::InvalidParameterName(format!("词表文件读取失败（{}）: {e}", file.display()))
    })?;
    let mut v: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| SqlError::InvalidParameterName(e.to_string()))?;
    let rules = v
        .get_mut("rules")
        .and_then(|r| r.as_array_mut())
        .ok_or_else(|| {
            SqlError::InvalidParameterName(format!("词表文件缺少 rules 数组: {}", file.display()))
        })?;
    let rule = rules
        .iter_mut()
        .find(|r| r.get("id").and_then(|v| v.as_str()) == Some(rule_id))
        .ok_or_else(|| {
            SqlError::InvalidParameterName(format!(
                "规则 {rule_id} 不在词表文件中（{}）",
                file.display()
            ))
        })?;
    let excludes = match rule.get_mut("excludes") {
        Some(a @ serde_json::Value::Array(_)) => a.as_array_mut().unwrap(),
        _ => return Ok(false), // 无排除名单 → 词不在其中
    };
    let before = excludes.len();
    excludes.retain(|w| w.as_str() != Some(word));
    let removed = excludes.len() != before;
    if removed {
        // 原子写回（与 append 同模式）
        let tmp = dir.join(format!(".{lang}_gec_lexicon.json.tmp"));
        let out = serde_json::to_string_pretty(&v)
            .map_err(|e| SqlError::InvalidParameterName(e.to_string()))?;
        std::fs::write(&tmp, out)
            .map_err(|e| SqlError::InvalidParameterName(format!("词表写入失败: {e}")))?;
        std::fs::rename(&tmp, &file)
            .map_err(|e| SqlError::InvalidParameterName(format!("词表落盘失败: {e}")))?;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uuid_like() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        format!(
            "{}_{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
    }

    fn sample_lexicon_path(dir: &Path, lang: &str) -> PathBuf {
        let p = dir.join(format!("{lang}_gec_lexicon.json"));
        std::fs::write(
            &p,
            serde_json::to_string_pretty(&serde_json::json!({
                "categories": [{"id": "grammar", "name": "语法"}],
                "rules": [
                    {"id": "EN_ARTICLE", "category": "grammar", "priority": 10,
                     "enabled": true, "description": "冠词 a/an 误用",
                     "examples": [
                        {"wrong": "I saw a apple.", "right": "I saw an apple."},
                        {"wrong": "a apple", "right": "an apple"}
                     ]},
                    {"id": "EN_SPELLING", "category": "spelling", "priority": 10,
                     "enabled": true, "description": "拼写错误",
                     "examples": [{"wrong": "recieve", "right": "receive"}]}
                ]
            }))
            .unwrap(),
        )
        .unwrap();
        p
    }

    #[test]
    fn import_then_query() {
        let dir = std::env::temp_dir().join(format!("lexicon_store_test_{}", uuid_like()));
        std::fs::create_dir_all(&dir).unwrap();
        sample_lexicon_path(&dir, "en");
        let store = LexiconStore::open_in_memory().unwrap();
        store
            .import_lexicon("en", &dir.join("en_gec_lexicon.json"))
            .unwrap();

        let list = store.list_rules("en", None, None, 1, 50).unwrap();
        assert_eq!(list.total, 2);
        assert_eq!(list.rules[0].def.id, "EN_ARTICLE"); // 示例数多者在前
        assert_eq!(list.rules[0].example_count, 2);
        assert!(list.rules[0].def.enabled);

        let exs = store.list_rule_examples("en", "EN_ARTICLE", 1, 50).unwrap();
        assert_eq!(exs.total, 2);
        assert_eq!(exs.examples[0].wrong, "I saw a apple.");
        assert_eq!(exs.examples[0].source, "manual");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_is_idempotent() {
        let dir = std::env::temp_dir().join(format!("lexicon_store_test_{}", uuid_like()));
        std::fs::create_dir_all(&dir).unwrap();
        sample_lexicon_path(&dir, "en");
        let store = LexiconStore::open_in_memory().unwrap();
        let p = dir.join("en_gec_lexicon.json");
        store.import_lexicon("en", &p).unwrap();
        store.import_lexicon("en", &p).unwrap(); // 重复导入
        let list = store.list_rules("en", None, None, 1, 50).unwrap();
        assert_eq!(list.total, 2);
        assert_eq!(
            store
                .list_rule_examples("en", "EN_ARTICLE", 1, 50)
                .unwrap()
                .total,
            2,
            "重复导入不应产生重复示例"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn get_rules_batch_matches_point_query() {
        let dir = std::env::temp_dir().join(format!("lexicon_store_test_{}", uuid_like()));
        std::fs::create_dir_all(&dir).unwrap();
        sample_lexicon_path(&dir, "en");
        let store = LexiconStore::open_in_memory().unwrap();
        store
            .import_lexicon("en", &dir.join("en_gec_lexicon.json"))
            .unwrap();
        // 禁用一条规则（验证批量查询返回的启用状态）
        store.toggle_rule("en", "EN_ARTICLE", false).unwrap();

        // 批量查询：顺序与输入一致；未入库 → None
        let out = store
            .get_rules("en", &["EN_SPELLING", "NO_SUCH", "EN_ARTICLE"])
            .unwrap();
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].as_ref().map(|d| d.id.as_str()), Some("EN_SPELLING"));
        assert!(out[0].as_ref().unwrap().enabled);
        assert!(out[1].is_none(), "未入库的规则 id 应为 None");
        assert_eq!(out[2].as_ref().map(|d| d.id.as_str()), Some("EN_ARTICLE"));
        assert!(!out[2].as_ref().unwrap().enabled, "禁用状态应如实返回");

        // 与逐条点查结果一致
        for (got, id) in out
            .iter()
            .zip(["EN_SPELLING", "NO_SUCH", "EN_ARTICLE"].iter())
        {
            match (got.as_ref(), store.get_rule("en", id).unwrap().as_ref()) {
                (Some(a), Some(b)) => {
                    assert_eq!(a.id, b.id);
                    assert_eq!(a.enabled, b.enabled);
                    assert_eq!(a.category, b.category);
                }
                (a, b) => assert_eq!(a.is_some(), b.is_some()),
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn search_fts_and_like_fallback() {
        let dir = std::env::temp_dir().join(format!("lexicon_store_test_{}", uuid_like()));
        std::fs::create_dir_all(&dir).unwrap();
        sample_lexicon_path(&dir, "en");
        let store = LexiconStore::open_in_memory().unwrap();
        store
            .import_lexicon("en", &dir.join("en_gec_lexicon.json"))
            .unwrap();

        // FTS 整词命中
        let list = store
            .list_rules("en", None, Some("recieve"), 1, 50)
            .unwrap();
        assert_eq!(list.total, 1);
        assert_eq!(list.rules[0].def.id, "EN_SPELLING");

        // 子串搜索：FTS 不命中 → LIKE 回退（wrong 子串）
        let list = store.list_rules("en", None, Some("apple"), 1, 50).unwrap();
        assert_eq!(list.total, 1);
        assert_eq!(list.rules[0].def.id, "EN_ARTICLE");

        // 分类过滤 + 搜索
        let list = store
            .list_rules("en", Some("spelling"), Some("recieve"), 1, 50)
            .unwrap();
        assert_eq!(list.total, 1);

        // 无命中
        assert_eq!(
            store
                .list_rules("en", None, Some("zzz_no_match"), 1, 50)
                .unwrap()
                .total,
            0
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn upsert_dedup_and_autocreate_rule() {
        let store = LexiconStore::open_in_memory().unwrap();
        assert!(
            store
                .upsert_example("en", "EN_NEW", "a apple", "an apple", "llm_distill")
                .unwrap()
        );
        assert!(
            !store
                .upsert_example("en", "EN_NEW", "a apple", "an apple", "llm_distill")
                .unwrap(),
            "重复示例应对重"
        );
        let defs = store.rule_defs("en").unwrap();
        assert_eq!(defs.len(), 1);
        assert!(!defs[0].enabled, "LLM 沉淀规则默认停用");
        assert_eq!(defs[0].source, "llm_distill");
    }

    #[test]
    fn delete_example() {
        let store = LexiconStore::open_in_memory().unwrap();
        store
            .upsert_example("en", "EN_NEW", "a apple", "an apple", "llm_distill")
            .unwrap();
        store
            .upsert_example("en", "EN_NEW", "I go", "I goes", "manual")
            .unwrap();
        assert!(
            store
                .delete_example("en", "EN_NEW", "a apple", "an apple")
                .unwrap(),
            "存在的示例应删除成功"
        );
        assert!(
            !store
                .delete_example("en", "EN_NEW", "a apple", "an apple")
                .unwrap(),
            "重复删除应返回 false"
        );
        let list = store.list_rule_examples("en", "EN_NEW", 1, 50).unwrap();
        assert_eq!(list.total, 1, "删除后仅剩另一条示例");
        assert_eq!(list.examples[0].wrong, "I go");
    }

    #[test]
    fn delete_example_by_wrong_removes_all_pairs() {
        let store = LexiconStore::open_in_memory().unwrap();
        // 同一 wrong 对应两个不同 right（修正侧未知时按 wrong 兜底删除）
        store
            .upsert_example("en", "EN_NEW", "a apple", "an apple", "llm_distill")
            .unwrap();
        store
            .upsert_example("en", "EN_NEW", "a apple", "apple", "manual")
            .unwrap();
        store
            .upsert_example("en", "EN_NEW", "I go", "I goes", "manual")
            .unwrap();
        let n = store
            .delete_example_by_wrong("en", "EN_NEW", "a apple")
            .unwrap();
        assert_eq!(n, 2, "同一 wrong 的全部示例对应删除");
        let list = store.list_rule_examples("en", "EN_NEW", 1, 50).unwrap();
        assert_eq!(list.total, 1);
        assert_eq!(list.examples[0].wrong, "I go");
        assert_eq!(
            store
                .delete_example_by_wrong("en", "EN_NEW", "a apple")
                .unwrap(),
            0
        );
    }

    #[test]
    fn toggle_and_pagination() {
        let dir = std::env::temp_dir().join(format!("lexicon_store_test_{}", uuid_like()));
        std::fs::create_dir_all(&dir).unwrap();
        sample_lexicon_path(&dir, "en");
        let store = LexiconStore::open_in_memory().unwrap();
        store
            .import_lexicon("en", &dir.join("en_gec_lexicon.json"))
            .unwrap();

        store.toggle_rule("en", "EN_ARTICLE", false).unwrap();
        let d = store
            .rule_defs("en")
            .unwrap()
            .into_iter()
            .find(|d| d.id == "EN_ARTICLE")
            .unwrap();
        assert!(!d.enabled);
        assert!(store.toggle_rule("en", "NO_SUCH", true).is_err());

        // 分页：page_size=1 时两页各一条
        let p1 = store.list_rules("en", None, None, 1, 1).unwrap();
        let p2 = store.list_rules("en", None, None, 2, 1).unwrap();
        assert_eq!(p1.total, 2);
        assert_eq!(p1.rules.len(), 1);
        assert_eq!(p2.rules.len(), 1);
        assert_ne!(p1.rules[0].def.id, p2.rules[0].def.id);

        // 示例分页
        let e1 = store.list_rule_examples("en", "EN_ARTICLE", 1, 1).unwrap();
        assert_eq!(e1.total, 2);
        assert_eq!(e1.examples.len(), 1);

        // 分类聚合
        let cats = store.list_categories("en").unwrap();
        assert_eq!(cats.len(), 2);

        // 导出：与导入格式同构
        let v = store.export_lexicon("en").unwrap();
        assert_eq!(v["lang"], "en");
        assert_eq!(v["rules"].as_array().unwrap().len(), 2);
        let article = v["rules"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == "EN_ARTICLE")
            .unwrap();
        assert_eq!(article["examples"].as_array().unwrap().len(), 2);
        assert_eq!(article["enabled"], false); // 导出保留运行时开关状态

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn seed_from_dir_on_open() {
        let dir = std::env::temp_dir().join(format!("lexicon_store_test_{}", uuid_like()));
        std::fs::create_dir_all(&dir).unwrap();
        sample_lexicon_path(&dir, "en");
        let db = dir.join("lex.db");
        let store = LexiconStore::open(&db, Some(&dir)).unwrap();
        assert!(!store.is_empty().unwrap());
        assert_eq!(store.list_rules("en", None, None, 1, 50).unwrap().total, 2);
        // 已播种的库再次 open 不重复播种
        let store2 = LexiconStore::open(&db, Some(&dir)).unwrap();
        assert_eq!(
            store2
                .list_rule_examples("en", "EN_ARTICLE", 1, 50)
                .unwrap()
                .total,
            2
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sync_rules_to_file_merges_and_preserves() {
        let dir = std::env::temp_dir().join(format!("lexicon_sync_test_{}", uuid_like()));
        std::fs::create_dir_all(&dir).unwrap();
        sample_lexicon_path(&dir, "en");
        let db = dir.join("lex.db");
        let store = LexiconStore::open(&db, Some(&dir)).unwrap();

        // 采纳新示例 → 同步回文件
        store
            .upsert_example(
                "en",
                "LLM_TENSE",
                "I go to school yesterday.",
                "I went to school yesterday.",
                "llm_distill",
            )
            .unwrap();
        let out = store.sync_rules_to_file(&dir, "en").unwrap();
        assert_eq!(out.updated_rule.as_deref(), Some("LLM_TENSE"));

        let v: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("en_gec_lexicon.json")).unwrap(),
        )
        .unwrap();
        let rules = v["rules"].as_array().unwrap();
        // 原文件 2 条 + 新采纳 1 条
        assert_eq!(rules.len(), 3);
        // 顺序保持：文件原有规则原位不动，新规则追加末尾（不被随机重排）
        assert_eq!(
            rules
                .iter()
                .map(|r| r["id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["EN_ARTICLE", "EN_SPELLING", "LLM_TENSE"]
        );
        let new_rule = rules.iter().find(|r| r["id"] == "LLM_TENSE").unwrap();
        assert_eq!(new_rule["examples"].as_array().unwrap().len(), 1);
        assert_eq!(new_rule["enabled"], false); // 新规则默认停用
        // 其余 section 原样保留
        assert!(v.get("categories").is_some());

        // 幂等：再次同步无新增
        let out2 = store.sync_rules_to_file(&dir, "en").unwrap();
        assert_eq!(out2.updated_rule, None);

        // store 没有的文件规则保留（防误删）：文件里手工加一条 store 外的规则
        let mut v2: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("en_gec_lexicon.json")).unwrap(),
        )
        .unwrap();
        v2["rules"].as_array_mut().unwrap().push(serde_json::json!({
            "id": "FILE_ONLY", "category": "grammar", "priority": 10,
            "enabled": true, "description": "仅存在于文件", "examples": []
        }));
        std::fs::write(
            dir.join("en_gec_lexicon.json"),
            serde_json::to_string_pretty(&v2).unwrap(),
        )
        .unwrap();
        store.sync_rules_to_file(&dir, "en").unwrap();
        let v3: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("en_gec_lexicon.json")).unwrap(),
        )
        .unwrap();
        assert!(
            v3["rules"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["id"] == "FILE_ONLY")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 真实仓库词表（`lexicons/zh_gec_lexicon.json`）端到端：sync 后
    /// 顶层键序与 rules 数组顺序均保持原样（preserve_order + 原位 upsert）
    #[test]
    fn sync_rules_to_file_preserves_real_lexicon_order() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../lexicons/zh_gec_lexicon.json");
        if !src.is_file() {
            return; // 仓库布局异常时跳过
        }
        let dir = std::env::temp_dir().join(format!("lexicon_sync_real_{}", uuid_like()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::copy(&src, dir.join("zh_gec_lexicon.json")).unwrap();
        let before: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("zh_gec_lexicon.json")).unwrap(),
        )
        .unwrap();
        let before_keys: Vec<String> = before
            .as_object()
            .unwrap()
            .keys()
            .map(|k| k.to_string())
            .collect();
        let before_rule_ids: Vec<String> = before["rules"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|r| r["id"].as_str().map(|s| s.to_string()))
            .collect();

        let db = dir.join("lex.db");
        let store = LexiconStore::open(&db, Some(&dir)).unwrap();
        store.sync_rules_to_file(&dir, "zh").unwrap();

        let after: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("zh_gec_lexicon.json")).unwrap(),
        )
        .unwrap();
        let after_keys: Vec<String> = after
            .as_object()
            .unwrap()
            .keys()
            .map(|k| k.to_string())
            .collect();
        let after_rule_ids: Vec<String> = after["rules"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|r| r["id"].as_str().map(|s| s.to_string()))
            .collect();
        assert_eq!(after_keys, before_keys, "顶层键序应保持文件原序");
        assert_eq!(
            after_rule_ids, before_rule_ids,
            "rules 数组顺序应保持文件原序"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sync_rules_to_file_missing_file_errors() {
        let dir = std::env::temp_dir().join(format!("lexicon_sync_test_{}", uuid_like()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("lex.db");
        let store = LexiconStore::open(&db, None).unwrap();
        store
            .upsert_example("fr", "LLM_TENSE", "a", "b", "llm_distill")
            .unwrap();
        // 该语言无词表文件 → Err（调用方据此走知识分支）
        assert!(store.sync_rules_to_file(&dir, "fr").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn append_rule_exclude_writes_and_dedups() {
        let dir = std::env::temp_dir().join(format!("lexicon_sync_test_{}", uuid_like()));
        std::fs::create_dir_all(&dir).unwrap();
        sample_lexicon_path(&dir, "en");
        // 新写入
        assert!(append_rule_exclude(&dir, "en", "EN_ARTICLE", "potato").unwrap());
        // 幂等：重复追加不报错、不重复
        assert!(!append_rule_exclude(&dir, "en", "EN_ARTICLE", "potato").unwrap());
        let v: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("en_gec_lexicon.json")).unwrap(),
        )
        .unwrap();
        let rule = v["rules"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == "EN_ARTICLE")
            .unwrap();
        assert_eq!(rule["excludes"], serde_json::json!(["potato"]));
        // 其他字段不受影响
        assert_eq!(rule["examples"].as_array().unwrap().len(), 2);
        // 规则不在文件中 → Err
        assert!(append_rule_exclude(&dir, "en", "NO_SUCH_RULE", "x").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_rule_exclude_removes_and_idempotent() {
        let dir = std::env::temp_dir().join(format!("lexicon_remove_exclude_test_{}", uuid_like()));
        std::fs::create_dir_all(&dir).unwrap();
        sample_lexicon_path(&dir, "en");
        // 先加再删
        assert!(append_rule_exclude(&dir, "en", "EN_ARTICLE", "potato").unwrap());
        assert!(remove_rule_exclude(&dir, "en", "EN_ARTICLE", "potato").unwrap());
        // 幂等：再删一次 → false
        assert!(!remove_rule_exclude(&dir, "en", "EN_ARTICLE", "potato").unwrap());
        let v: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("en_gec_lexicon.json")).unwrap(),
        )
        .unwrap();
        let rule = v["rules"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == "EN_ARTICLE")
            .unwrap();
        assert_eq!(rule["excludes"], serde_json::json!([]));
        // 规则不在文件中 → Err
        assert!(remove_rule_exclude(&dir, "en", "NO_SUCH_RULE", "x").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sync_preserves_file_side_excludes() {
        let dir = std::env::temp_dir().join(format!("lexicon_sync_test_{}", uuid_like()));
        std::fs::create_dir_all(&dir).unwrap();
        sample_lexicon_path(&dir, "en");
        let db = dir.join("lex.db");
        let store = LexiconStore::open(&db, Some(&dir)).unwrap();
        // 文件侧手工加排除词（模拟 append_rule_exclude 的产物）
        append_rule_exclude(&dir, "en", "EN_ARTICLE", "potato").unwrap();
        // 触发一次 store → 文件同步（新示例 upsert）
        store
            .upsert_example("en", "EN_ARTICLE", "a apple", "an apple", "llm_distill")
            .unwrap();
        store.sync_rules_to_file(&dir, "en").unwrap();
        let v: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("en_gec_lexicon.json")).unwrap(),
        )
        .unwrap();
        let rule = v["rules"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == "EN_ARTICLE")
            .unwrap();
        assert_eq!(rule["excludes"], serde_json::json!(["potato"]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 回归：规则在文件中但不在 store（建库后才加入文件，播种只在库空时执行故未导入）——
    /// 采纳路径「回填 → upsert → sync」后，文件 `enabled` 不得被「默认停用」影子定义覆盖
    #[test]
    fn backfill_then_upsert_and_sync_keeps_file_enabled() {
        let dir = std::env::temp_dir().join(format!("lexicon_backfill_test_{}", uuid_like()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("zh_gec_lexicon.json");
        std::fs::write(
            &file,
            serde_json::to_string_pretty(&serde_json::json!({
                "lang": "zh",
                "rules": [
                    {"id": "ZH_ANOMALY_X", "category": "anomaly", "priority": 20,
                     "enabled": true, "description": "AI 污染",
                     "examples": [{"wrong": "w1", "right": ""}],
                     "patterns": [{"pattern": "^需要我再", "strong": true, "note": "种子"}]}
                ]
            }))
            .unwrap(),
        )
        .unwrap();
        let store = LexiconStore::open_in_memory().unwrap();
        assert!(
            store.get_rule("zh", "ZH_ANOMALY_X").unwrap().is_none(),
            "规则不在 store（模拟建库早于规则入文件的状态）"
        );
        // ① 回填：定义按文件状态入库
        assert!(
            store
                .backfill_rule_from_file(&dir, "zh", "ZH_ANOMALY_X")
                .unwrap()
        );
        let def = store.get_rule("zh", "ZH_ANOMALY_X").unwrap().unwrap();
        assert!(def.enabled, "回填的定义应保留文件 enabled 状态");
        assert_eq!(def.category, "anomaly");
        assert_eq!(def.priority, 20);
        assert_eq!(def.source, "manual");
        // ② 采纳 upsert（与生产采纳路径同构）
        store
            .upsert_example("zh", "ZH_ANOMALY_X", "w2", "", "llm_distill")
            .unwrap();
        // ③ 同步回文件
        store.sync_rules_to_file(&dir, "zh").unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        let rule = v["rules"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == "ZH_ANOMALY_X")
            .unwrap();
        assert_eq!(rule["enabled"], true, "文件 enabled 状态不得被 sync 覆盖");
        assert_eq!(rule["category"], "anomaly");
        assert_eq!(rule["priority"], 20);
        assert_eq!(rule["description"], "AI 污染");
        // patterns 为文件侧数据，保留
        assert_eq!(rule["patterns"][0]["pattern"], "^需要我再");
        // 文件侧示例保留 + 新增示例
        let exs: Vec<&str> = rule["examples"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["wrong"].as_str().unwrap())
            .collect();
        assert!(
            exs.contains(&"w1") && exs.contains(&"w2"),
            "examples: {exs:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 回填已有行（历史误建的影子定义）：只修元数据，运行时 `enabled` / `source` 原样保留
    #[test]
    fn backfill_existing_row_preserves_runtime_enabled() {
        let dir = std::env::temp_dir().join(format!("lexicon_backfill_test_{}", uuid_like()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("zh_gec_lexicon.json");
        std::fs::write(
            &file,
            serde_json::to_string_pretty(&serde_json::json!({
                "lang": "zh",
                "rules": [
                    {"id": "ZH_X", "category": "anomaly", "priority": 20,
                     "enabled": true, "description": "AI 污染",
                     "examples": [{"wrong": "w1", "right": ""}]}
                ]
            }))
            .unwrap(),
        )
        .unwrap();
        let store = LexiconStore::open_in_memory().unwrap();
        // 模拟历史误建：upsert 自动创建「停用」影子定义，用户随后在面板重新启用
        store
            .upsert_example("zh", "ZH_X", "w", "r", "llm_distill")
            .unwrap();
        store.toggle_rule("zh", "ZH_X", true).unwrap();
        assert!(store.backfill_rule_from_file(&dir, "zh", "ZH_X").unwrap());
        let def = store.get_rule("zh", "ZH_X").unwrap().unwrap();
        assert!(def.enabled, "运行时 enabled 状态应保留");
        assert_eq!(def.category, "anomaly", "文件侧元数据应被回填");
        assert_eq!(def.priority, 20);
        assert_eq!(def.source, "llm_distill", "source（运行时状态）应保留");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 规则不在文件 / 文件缺失 → 无操作（真正的新规则仍走「默认停用」自动创建）
    #[test]
    fn backfill_absent_rule_is_noop() {
        let dir = std::env::temp_dir().join(format!("lexicon_backfill_test_{}", uuid_like()));
        std::fs::create_dir_all(&dir).unwrap();
        sample_lexicon_path(&dir, "en");
        let store = LexiconStore::open_in_memory().unwrap();
        assert!(
            !store
                .backfill_rule_from_file(&dir, "en", "NO_SUCH")
                .unwrap()
        );
        assert!(store.get_rule("en", "NO_SUCH").unwrap().is_none());
        // 文件缺失 → 同样无操作
        let empty = std::env::temp_dir().join(format!("lexicon_backfill_test_{}", uuid_like()));
        std::fs::create_dir_all(&empty).unwrap();
        assert!(
            !store
                .backfill_rule_from_file(&empty, "en", "EN_ARTICLE")
                .unwrap()
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&empty);
    }
}
