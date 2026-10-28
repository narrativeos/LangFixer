use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::RwLock;

use attribution_engine::{KnowledgeStore, StagingStore, TableStore};
use gec_core::{
    ActionTier, LanguageRouter, LlmDetectConfig, LlmGecEngine, ModelRegistry, compose_detector,
};
use gec_en::{DetectorConfig as EnConfig, EnglishDetector};
use gec_ja::{DetectorConfig as JaConfig, JapaneseDetector};
use gec_zh::anomaly_distill::{
    GoldSet, anomaly_rule_id, append_pattern_to_lexicon, distill_pattern, evaluate_candidate,
};
use gec_zh::{ChineseDetector, DetectorConfig as ZhConfig};
use langfixer_llm::LlmService;
use langfixer_llm::templates::TemplateStore;
use lexicon_store::LexiconStore;
use rusqlite::Connection;

use crate::settings::AppSettings;
use shared_model::{
    AnalyzedError, AttributionContext, DecisionTableMeta, Error, KnowledgePoint, KnowledgeSource,
    SessionRecord, SessionStore, derive_issue_type, error_types,
};

/// 程序版本号（crate 版本；写入检测历史，便于跨版本对比改进 / 按版本筛选训练数据）
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 语言 / 引擎元信息（前端语言切换器展示真实引擎信息）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LanguageInfo {
    pub lang: String,
    pub name: String,
    pub nlp_service: String,
    pub engine: String,
}

/// 规则信息（与 api-server 的 /gec/rules 响应同构；
/// 示例对不随列表返回，经 list_rule_examples 按需分页加载）
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct RuleInfo {
    pub id: String,
    pub category: String,
    #[serde(default)]
    pub priority: u32,
    pub enabled: bool,
    #[serde(default)]
    pub description: String,
    /// 该规则示例对总数
    pub example_count: usize,
    /// 来源（`manual` / `llm_distill`）
    pub source: String,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct CategoryInfo {
    pub id: String,
    pub name: String,
    pub rule_count: usize,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct RulesResponse {
    pub lang: String,
    /// 满足过滤条件（category / q）的规则总数
    pub total: usize,
    pub page: usize,
    pub page_size: usize,
    pub categories: Vec<CategoryInfo>,
    pub rules: Vec<RuleInfo>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct ToggleResponse {
    pub id: String,
    pub enabled: bool,
}

/// 自定义词（白名单）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CustomWord {
    pub id: i64,
    pub lang: String,
    pub word: String,
    #[serde(default)]
    pub reason: String,
    pub created_at: String,
}

/// 自定义词存储（SQLite 持久化 + 内存索引，与 api-server 同构）
pub struct CustomWordStore {
    conn: std::sync::Mutex<Connection>,
    index: std::sync::Mutex<std::collections::HashMap<String, std::collections::HashSet<String>>>,
}

impl CustomWordStore {
    pub fn open(path: &Path) -> Result<Self, rusqlite::Error> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS custom_words (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                lang TEXT NOT NULL,
                word TEXT NOT NULL,
                reason TEXT DEFAULT '',
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                UNIQUE(lang, word)
            );
            CREATE INDEX IF NOT EXISTS idx_custom_words_lang ON custom_words(lang);",
        )?;
        let mut index: std::collections::HashMap<String, std::collections::HashSet<String>> =
            std::collections::HashMap::new();
        {
            let mut stmt = conn.prepare("SELECT lang, word FROM custom_words")?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (lang, word) = row?;
                index.entry(lang).or_default().insert(word);
            }
        }
        Ok(Self {
            conn: std::sync::Mutex::new(conn),
            index: std::sync::Mutex::new(index),
        })
    }

    pub fn add(&self, lang: &str, word: &str, reason: &str) -> Result<CustomWord, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let created_at = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        conn.execute(
            "INSERT INTO custom_words (lang, word, reason, created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(lang, word) DO UPDATE SET reason = excluded.reason",
            rusqlite::params![lang, word, reason, created_at],
        )?;
        let id = conn
            .query_row(
                "SELECT id FROM custom_words WHERE lang = ?1 AND word = ?2",
                rusqlite::params![lang, word],
                |r| r.get(0),
            )
            .unwrap_or(0);
        self.index
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(lang.to_string())
            .or_default()
            .insert(word.to_string());
        Ok(CustomWord {
            id,
            lang: lang.to_string(),
            word: word.to_string(),
            reason: reason.to_string(),
            created_at,
        })
    }

    pub fn remove(&self, id: i64) -> Result<bool, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        // 先取 lang/word（删除后无法回查），再执行删除并同步内存索引
        let target = conn
            .query_row(
                "SELECT lang, word FROM custom_words WHERE id = ?1",
                [id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .ok();
        let n = conn.execute("DELETE FROM custom_words WHERE id = ?1", [id])?;
        if n > 0 {
            if let Some((lang, word)) = target {
                if let Some(set) = self
                    .index
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get_mut(&lang)
                {
                    set.remove(&word);
                }
            }
        }
        Ok(n > 0)
    }

    pub fn list(&self, lang: &str) -> Result<Vec<CustomWord>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT id, lang, word, reason, created_at FROM custom_words WHERE lang = ?1 ORDER BY id",
        )?;
        let rows = stmt
            .query_map([lang], |r| {
                Ok(CustomWord {
                    id: r.get(0)?,
                    lang: r.get(1)?,
                    word: r.get(2)?,
                    reason: r.get(3)?,
                    created_at: r.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn is_whitelisted(&self, lang: &str, word: &str) -> bool {
        self.index
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(lang)
            .map(|s| s.contains(word))
            .unwrap_or(false)
    }
}
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
    pub created_at: String,
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
        // (lang, text_hash) 索引在迁移后创建（旧库该列由上面迁移补出，先建会失败）
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_detect_history_lang_hash ON detect_history(lang, text_hash);",
        )?;
        Ok(Self {
            conn: std::sync::Mutex::new(conn),
        })
    }

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
    ) -> Result<i64, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let created_at = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        conn.execute(
            "INSERT INTO detect_history (session_id, lang, text, errors_json, analyzed_json, timings_json, version, params_json, text_hash, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![session_id, lang, text, errors_json, analyzed_json, timings_json, version, params_json, text_hash, created_at],
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
            "SELECT id, session_id, lang, text, errors_json, analyzed_json, timings_json, version, params_json, text_hash, created_at
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
            created_at: r.get(10)?,
        })
    }

    pub fn get(&self, id: i64) -> Result<Option<DetectHistory>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT id, session_id, lang, text, errors_json, analyzed_json, timings_json, version, params_json, text_hash, created_at
             FROM detect_history WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map([id], |r| self.map_row(r))?;
        Ok(rows.next().transpose()?)
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
            "SELECT id, session_id, lang, text, errors_json, analyzed_json, timings_json, version, params_json, text_hash, created_at
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
        Ok(conn.execute("DELETE FROM detect_history", [])?)
    }
}

/// 桌面端核心：检测器路由 + 决策表存储 + 会话存储 + LLM 扩展
/// + 运行时设置（设置界面可改）/ 规则开关 / 自定义词 / 检测历史（与 api-server 能力对齐）
pub struct Core {
    /// 语言路由（`RwLock`：HanLP 地址 / 离线开关 / 词表目录变更时热重建 zh/en/ja 检测器）
    pub router: RwLock<Arc<LanguageRouter>>,
    pub tables: TableStore,
    /// 知识库（知识源注册表 + 知识点集合；与决策表分离，不随产品内置发布）
    pub knowledge: KnowledgeStore,
    /// 知识源导入草稿区（书 → agent 挖掘 → 草稿 → 人工一键发布；持久化于知识库目录 staging/）
    pub ingest: Arc<StagingStore>,
    /// 草稿持久化目录（`{GEC_KNOWLEDGE_DIR}/staging`）
    pub ingest_dir: PathBuf,
    /// 知识库目录（`GEC_KNOWLEDGE_DIR`；发布/热更新写回此目录，跨重启存活）
    pub knowledge_dir: PathBuf,
    pub sessions: SessionStore,
    /// 运行时设置（设置界面可改；持久化到 `GEC_SETTINGS_FILE`，默认 `langfixer-settings.json`）
    pub settings: RwLock<AppSettings>,
    /// 设置持久化文件路径（构建时捕获，避免并行进程间环境变量互相干扰）
    pub settings_path: PathBuf,
    /// LLM 讲解服务（`RwLock`：LLM 参数变更时重建；未配置 API Key 时回退静态建议）
    pub llm: RwLock<LlmService>,
    /// 纠错模型注册表（`prompts/models.json`；`RwLock`：`prompts_dir` 变更时热重载）
    pub model_registry: RwLock<ModelRegistry>,
    /// zh/en/ja 检测器启动时基础配置（HanLP 地址 / 离线开关 / 词表目录变更时据此重建）
    zh_base_cfg: ZhConfig,
    en_base_cfg: EnConfig,
    ja_base_cfg: JaConfig,
    /// 规则词表存储（SQLite：规则定义 + 示例 + 运行时开关；
    /// 启动时从词表目录播种，旧版 JSON 开关状态一次性迁移后归档）
    pub lexicon_store: Arc<LexiconStore>,
    /// 用户自定义词（SQLite 持久化）
    pub custom_words: CustomWordStore,
    /// 检测历史（SQLite 持久化，全部留存）
    pub detect_history: DetectHistoryStore,
    /// 生效词表文件哈希（启动 + 检测器热重建时重算；规则侧版本指纹输入——
    /// 检测器只在构造时读词表文件，故「生效词表」由最近一次构建决定，不每次读盘）
    pub lexicon_hash: RwLock<String>,
    /// 学习知识存储（向量召回用；SQLite 持久化）
    pub learned_knowledge: Arc<lexicon_store::KnowledgeStore>,
    /// 嵌入引擎（未配置时为 None；`RwLock`：设置界面变更嵌入配置时热更新）
    pub embedding_engine: RwLock<Option<Arc<gec_core::EmbeddingEngine>>>,
    /// 载体（图片 / PDF / Word）审校 job 存储（`media-flow`：内存索引 + 磁盘 job 目录，
    /// 原件副本隔离；与 api-server 同一实现，两端行为不分叉）
    pub media_jobs: Arc<RwLock<media_flow::MediaJobStore>>,
    /// L3 飞轮数据目录（三期闭环：确认样本 jsonl；`GEC_L3_DATA_DIR` > 默认 `feature-model/l3_data`）
    pub l3_data: PathBuf,
    /// 反馈日志（SQLite：检测结果采纳/放弃留痕 + 规则反馈统计；反思流程数据基础）
    pub feedback_log: Arc<lexicon_store::FeedbackLogStore>,
    /// 反思中心（SQLite：反思报告 + 建议队列；P1 进化闭环「反思」环节）
    pub reflection_center: Arc<lexicon_store::ReflectionCenterStore>,
    /// 训练中心（SQLite：训练作业 + 状态机；P2 进化闭环「训练」环节，
    /// 消费反思建议队列）
    pub training_center: Arc<lexicon_store::TrainingCenterStore>,
    /// 优化轮次（SQLite：轮次提案 + 人工确认门 + 执行结果；P3 进化闭环
    /// 「贡献与轮次」环节）
    pub optimization_round: Arc<lexicon_store::OptimizationRoundStore>,
}

impl Core {
    pub fn from_env() -> Self {
        // 运行时设置：设置文件（GEC_SETTINGS_FILE）> 环境变量 > 内置默认（桌面端默认离线）
        let settings_path = PathBuf::from(
            std::env::var("GEC_SETTINGS_FILE")
                .unwrap_or_else(|_| "langfixer-settings.json".to_string()),
        );
        let settings = AppSettings::load(&settings_path);

        // 载体（图片 / PDF / Word）job 根目录：设置 media.workdir > 相对当前目录的 media-jobs/
        // （与 api-server 同口径；job 目录内是原件副本，**永不回写原件**）
        let media_root = if settings.media.workdir.is_empty() {
            PathBuf::from("media-jobs")
        } else {
            PathBuf::from(&settings.media.workdir)
        };

        // 检测器：按 `GEC_ENGINE_{LANG}` 环境变量组合引擎
        // （rule 默认 / decision 决策模型层 / statistical / llm 预留）
        // HanLP 地址 / 离线开关 / 词表目录 / 决策模型层取运行时设置（设置界面可改，即时生效）
        let zh_base_cfg = ZhConfig::from_env();
        let en_base_cfg = EnConfig::from_env();
        let ja_base_cfg = JaConfig::from_env();
        let mut router = LanguageRouter::new();
        let zh_cfg = ZhConfig {
            endpoint: settings.hanlp_endpoint.clone(),
            offline_only: settings.offline_mode,
            lexicons_dir: PathBuf::from(settings.lexicons_dir.clone()),
            ..zh_base_cfg.clone()
        };
        let en_cfg = EnConfig {
            endpoint: settings.hanlp_endpoint.clone(),
            offline_only: settings.offline_mode,
            lexicons_dir: PathBuf::from(settings.lexicons_dir.clone()),
            ..en_base_cfg.clone()
        };
        let ja_cfg = JaConfig {
            offline_only: settings.offline_mode,
            lexicons_dir: PathBuf::from(settings.lexicons_dir.clone()),
            ..ja_base_cfg.clone()
        };

        let tables = TableStore::new();
        let dir = Path::new(&settings.tables_dir);
        match tables.load_from_dir(dir) {
            Ok(n) => tracing::info!(dir = %dir.display(), count = n, "决策表加载完成"),
            Err(e) => tracing::error!(dir = %dir.display(), error = %e, "决策表加载失败"),
        }

        // 知识库：独立于决策表（GEC_KNOWLEDGE_DIR；目录缺失时优雅降级为纯表级归因）
        let knowledge = KnowledgeStore::new();
        let kb_dir_str =
            std::env::var("GEC_KNOWLEDGE_DIR").unwrap_or_else(|_| "knowledge-base".to_string());
        let kb_dir = PathBuf::from(&kb_dir_str);
        match knowledge.load_from_dir(&kb_dir) {
            Ok(n) => tracing::info!(count = n, "知识库加载完成"),
            Err(e) => tracing::error!(error = %e, "知识库加载失败"),
        }

        // 知识源导入草稿区（持久化于知识库目录 staging/，重启恢复未完成会话）
        let ingest = Arc::new(StagingStore::new());
        let ingest_dir = kb_dir.join("staging");
        match ingest.load_from_dir(&ingest_dir) {
            Ok(n) if n > 0 => tracing::info!(count = n, "知识源导入草稿已恢复"),
            Err(e) => tracing::warn!(error = %e, "知识源导入草稿加载失败（不影响主链路）"),
            _ => {}
        }

        // LLM 讲解服务：配置取运行时设置（设置界面可改，持久化）；模板库从 prompts_dir 加载
        let llm = LlmService {
            config: settings.llm.clone(),
            templates: TemplateStore::load(&settings.llm.prompts_dir),
        };
        if llm.config.enabled() {
            tracing::info!(
                model = %llm.config.model,
                templates = llm.templates.len(),
                "LLM 讲解服务已启用"
            );
        } else {
            tracing::debug!("LLM 讲解服务未启用（未配置 API Key），将使用静态建议");
        }

        // 纠错模型注册表：从 `prompts_dir/models.json` 加载（`prompts_dir` 变更时热重载；CEC 工作流 E）
        let model_registry = ModelRegistry::load(&settings.llm.prompts_dir.join("models.json"));

        // 自定义词 / 检测历史 / 规则词表：SQLite 持久化（与 api-server 同构，
        // db 文件：`GEC_DB_FILE` 环境变量 > 默认工作目录 `langfixer.db`）
        let db_path = PathBuf::from(
            std::env::var("GEC_DB_FILE").unwrap_or_else(|_| "langfixer.db".to_string()),
        );
        let custom_words = match CustomWordStore::open(&db_path) {
            Ok(store) => {
                tracing::info!(?db_path, "自定义词存储已初始化");
                store
            }
            Err(e) => {
                tracing::error!(error = %e, "自定义词存储初始化失败，将使用内存模式");
                let tmp = std::env::temp_dir().join("langfixer_desktop_words.db");
                CustomWordStore::open(&tmp).expect("无法打开临时 SQLite")
            }
        };
        let detect_history = match DetectHistoryStore::open(&db_path) {
            Ok(store) => {
                tracing::info!(?db_path, "检测历史存储已初始化");
                store
            }
            Err(e) => {
                tracing::error!(error = %e, "检测历史存储初始化失败，将使用内存模式");
                let tmp = std::env::temp_dir().join("langfixer_desktop_history.db");
                DetectHistoryStore::open(&tmp).expect("无法打开临时 SQLite")
            }
        };

        // 规则词表存储（SQLite，与上面共用同一个 db 文件）：
        // 1. 打开库并从词表目录播种（已存在的规则保留运行时 enabled/source 状态）
        // 2. 一次性迁移旧版 JSON 开关状态（GEC_RULE_STATE_FILE），迁移后归档为 .migrated
        let lexicons_dir = PathBuf::from(settings.lexicons_dir.clone());
        let old_state_path = PathBuf::from(
            std::env::var("GEC_RULE_STATE_FILE")
                .unwrap_or_else(|_| "langfixer-rule-state.json".to_string()),
        );
        let lexicon_store = match LexiconStore::open(&db_path, Some(&lexicons_dir)) {
            Ok(store) => {
                tracing::info!(
                    "规则词表存储已初始化: {:?}（播种目录 {:?}）",
                    db_path,
                    lexicons_dir
                );
                // 播种后再迁移旧版 JSON 开关状态（播种保留已有状态，旧文件更新）
                match lexicon_store::migrate_rule_state_file(&store, &old_state_path) {
                    Ok(n) if n > 0 => {
                        tracing::info!(applied = n, "旧版规则开关状态（JSON）已迁移到 SQLite")
                    }
                    Ok(_) => {}
                    Err(e) => tracing::error!(
                        error = %e,
                        "旧版规则开关状态迁移失败（保留词表默认状态）"
                    ),
                }
                Arc::new(store)
            }
            Err(e) => {
                tracing::error!(error = %e, "规则词表存储初始化失败，将使用临时空存储");
                let tmp = std::env::temp_dir().join("langfixer_lexicon.db");
                Arc::new(LexiconStore::open(&tmp, None).expect("无法打开临时 SQLite 词表库"))
            }
        };

        // 学习知识存储（向量召回用；与 lexicon_store 共用同一 SQLite 文件）
        let learned_knowledge = match lexicon_store::KnowledgeStore::open(&db_path) {
            Ok(store) => Arc::new(store),
            Err(e) => {
                tracing::error!(error = %e, "学习知识存储初始化失败，将使用临时空存储");
                let tmp = std::env::temp_dir().join("langfixer_knowledge.db");
                Arc::new(
                    lexicon_store::KnowledgeStore::open(&tmp).expect("无法打开临时知识 SQLite"),
                )
            }
        };

        // 反馈日志存储（与词表 / 学习知识共用同一 SQLite 文件）
        let feedback_log = match lexicon_store::FeedbackLogStore::open(&db_path) {
            Ok(store) => Arc::new(store),
            Err(e) => {
                tracing::error!(error = %e, "反馈日志存储初始化失败，将使用临时空存储");
                let tmp = std::env::temp_dir().join("langfixer_feedback.db");
                Arc::new(
                    lexicon_store::FeedbackLogStore::open(&tmp)
                        .expect("无法打开临时反馈日志 SQLite"),
                )
            }
        };

        // 反思中心存储（SQLite：反思报告 + 建议队列；与反馈日志共用同一 db 文件）
        let reflection_center = match lexicon_store::ReflectionCenterStore::open(&db_path) {
            Ok(store) => Arc::new(store),
            Err(e) => {
                tracing::error!(error = %e, "反思中心存储初始化失败，将使用临时空存储");
                let tmp = std::env::temp_dir().join("langfixer_reflection.db");
                Arc::new(
                    lexicon_store::ReflectionCenterStore::open(&tmp)
                        .expect("无法打开临时反思中心 SQLite"),
                )
            }
        };

        // 训练中心存储（SQLite：训练作业 + 状态机；P2 消费反思建议队列）
        let training_center = match lexicon_store::TrainingCenterStore::open(&db_path) {
            Ok(store) => Arc::new(store),
            Err(e) => {
                tracing::error!(error = %e, "训练中心存储初始化失败，将使用临时空存储");
                let tmp = std::env::temp_dir().join("langfixer_training.db");
                Arc::new(
                    lexicon_store::TrainingCenterStore::open(&tmp)
                        .expect("无法打开临时训练中心 SQLite"),
                )
            }
        };

        // 优化轮次存储（SQLite：轮次提案 + 人工确认门；P3 闭环决策环节）
        let optimization_round = match lexicon_store::OptimizationRoundStore::open(&db_path) {
            Ok(store) => Arc::new(store),
            Err(e) => {
                tracing::error!(error = %e, "优化轮次存储初始化失败，将使用临时空存储");
                let tmp = std::env::temp_dir().join("langfixer_round.db");
                Arc::new(
                    lexicon_store::OptimizationRoundStore::open(&tmp)
                        .expect("无法打开临时优化轮次 SQLite"),
                )
            }
        };

        // 嵌入引擎（未配置时为 None）
        let embedding_engine = if settings.embedding.is_configured() {
            Some(Arc::new(gec_core::EmbeddingEngine::new(
                settings.embedding.clone(),
            )))
        } else {
            None
        };

        // 知识向量召回（嵌入引擎已配置时接线 ②.5；未配置或知识层开关关闭时 None，静默跳过）
        let knowledge_recall: Option<Arc<dyn gec_core::KnowledgeRecall>> =
            if settings.knowledge_enabled {
                embedding_engine.as_ref().map(|eng| {
                    let impl_: Arc<dyn gec_core::KnowledgeRecall> =
                        Arc::new(crate::knowledge_recall::KnowledgeRecallImpl::new(
                            learned_knowledge.clone(),
                            eng.clone(),
                        ));
                    impl_
                })
            } else {
                None
            };
        // 注册检测器（传入知识召回；②.5 知识向量召回独立于决策层生效）
        router.register(compose_detector(
            "zh",
            Arc::new(ChineseDetector::new(zh_cfg)),
            Some(settings.decision.clone()),
            knowledge_recall.clone(),
            Some(settings.l3.clone()),
        ));
        router.register(compose_detector(
            "en",
            Arc::new(EnglishDetector::new(en_cfg)),
            Some(settings.decision.clone()),
            knowledge_recall.clone(),
            Some(settings.l3.clone()),
        ));
        router.register(compose_detector(
            "ja",
            Arc::new(JapaneseDetector::new(ja_cfg)),
            Some(settings.decision.clone()),
            knowledge_recall,
            Some(settings.l3.clone()),
        ));

        // 生效词表文件哈希（规则侧版本指纹输入；检测器热重建时同步刷新）
        let lexicon_hash = shared_model::lexicon_files_hash(Path::new(&settings.lexicons_dir));

        Self {
            router: RwLock::new(Arc::new(router)),
            tables,
            knowledge,
            ingest,
            ingest_dir,
            knowledge_dir: kb_dir,
            sessions: SessionStore::new(),
            settings: RwLock::new(settings),
            settings_path,
            llm: RwLock::new(llm),
            model_registry: RwLock::new(model_registry),
            zh_base_cfg,
            en_base_cfg,
            ja_base_cfg,
            lexicon_store,
            custom_words,
            detect_history,
            lexicon_hash: RwLock::new(lexicon_hash),
            learned_knowledge,
            embedding_engine: RwLock::new(embedding_engine),
            media_jobs: Arc::new(RwLock::new(media_flow::MediaJobStore::new(&media_root))),
            l3_data: std::env::var("GEC_L3_DATA_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| gec_core::default_l3_data_dir()),
            feedback_log,
            reflection_center,
            training_center,
            optimization_round,
        }
    }

    /// 构建指定语言的 LLM 兜底检测引擎（三级响应 level 2/3 的第三层）
    ///
    /// 从运行时 LLM 服务（设置界面可改）+ 检测模板
    /// （预设 `prompt_template` > `{lang}_detect` 提示词）现构建，
    /// 现构建，支持设置热更新；LLM 未配置或缺检测模板时返回 `None`
    /// （level 2/3 请求报错「LLM 兜底检测未配置」）。
    pub fn llm_detect_engine(&self, lang: &str) -> Option<Arc<LlmGecEngine>> {
        let service = self.llm.read().unwrap();
        if !service.config.enabled() {
            return None;
        }
        // 查选中的纠错模型预设（注册表）：覆盖输出格式 / max_tokens / 提示词模板；
        // 预设若指定 base_url / model（如 cec3/cec4 本地 vllm）则用预设值，
        // 否则沿用用户当前 LLM 端点（generic）。api_key 恒来自用户 LLM 配置。
        let model_id = self.settings.read().unwrap().llm_model_id.clone();
        let entry = self.model_registry.read().unwrap().get(&model_id).cloned();
        // 提示词模板按预设 `prompt_template`（如 cec4/cec3 用 CEC 官方模板
        // `zh_detect_cec`——模型卡原文 instruction + 原始句子输入，即模型训练格式）；
        // 未指定或未找到时回退语言级 `{lang}_detect` 模板。
        let template = match entry
            .as_ref()
            .map(|e| e.prompt_template.as_str())
            .filter(|k| !k.is_empty())
        {
            Some(key) => service
                .templates
                .get(key)
                .or_else(|| service.templates.for_lang_cefr(lang, "detect"))?,
            None => service.templates.for_lang_cefr(lang, "detect")?,
        };
        let (base_url, model, output_format, max_tokens) = match &entry {
            Some(e) if !e.base_url.is_empty() => (
                e.base_url.clone(),
                e.model.clone(),
                e.output_format.clone(),
                e.max_tokens,
            ),
            Some(e) => (
                service.config.base_url.clone(),
                service.config.model.clone(),
                e.output_format.clone(),
                e.max_tokens,
            ),
            None => (
                service.config.base_url.clone(),
                service.config.model.clone(),
                gec_core::OUTPUT_JSON.to_string(),
                0,
            ),
        };
        Some(Arc::new(LlmGecEngine::new(
            lang,
            LlmDetectConfig::from_parts(
                base_url,
                service.config.api_key.clone(),
                model,
                service.config.timeout_secs,
                template.system.clone(),
                template.user.clone(),
            )
            .with_output_format(output_format)
            .with_max_tokens(max_tokens),
        )))
    }

    /// 当前检测参数快照（存入历史 `params_json`；重复检测复用历史的比对依据）。
    ///
    /// 含请求侧参数（level / force_llm / category）+ 运行时状态
    /// （决策层 / L3 / LLM 配置 / **决策模型版本号**）+ 规则侧版本指纹
    /// （决策表元数据 + 生效词表 + 禁用规则集 + 自定义词白名单）。
    ///
    /// 决策模型版本号来自决策服务 `GET /health`（引擎内缓存，仅首查一次）；
    /// 决策层未启用或服务不可用 / 该语言无模型时为 `None`。
    pub async fn current_detect_params(
        &self,
        lang: &str,
        level: u8,
        force_llm: bool,
        category: &Option<String>,
    ) -> shared_model::DetectParams {
        // 设置快照（块作用域内读取并立即释放 guard——std RwLock guard 的 drop 点在
        // 作用域末尾，async fn 中若跨 await 会使 future 非 Send）
        let (decision_enabled, l3_enabled, llm_configured, llm_model) = {
            let s = self.settings.read().unwrap();
            (
                s.decision.enabled(),
                s.l3.enabled(),
                s.llm.enabled(),
                s.llm.model.clone(),
            )
        };
        let meta = self.tables.get_meta(lang);
        let disabled = self
            .lexicon_store
            .disabled_rule_ids(lang)
            .unwrap_or_default();
        let words: Vec<String> = self
            .custom_words
            .list(lang)
            .map(|ws| ws.into_iter().map(|w| w.word).collect())
            .unwrap_or_default();
        // 决策模型版本号：仅决策层启用时查询（读锁内只克隆引擎 Arc，锁立即释放）
        let model_version = if decision_enabled {
            let engine = self.router.read().unwrap().get(lang).cloned();
            if let Some(e) = engine.as_ref().and_then(|e| e.decision_engine()) {
                e.model_version().await
            } else {
                None
            }
        } else {
            None
        };
        shared_model::DetectParams {
            level,
            force_llm,
            category: category.clone(),
            decision_enabled,
            l3_enabled,
            llm_configured,
            llm_model: llm_configured.then_some(llm_model),
            model_version,
            rules_fingerprint: shared_model::rules_fingerprint(
                meta.as_ref(),
                &self.lexicon_hash.read().unwrap(),
                &disabled,
                &words,
            ),
        }
    }

    /// 应用新设置（设置界面 `update_settings` 命令）：
    /// - HanLP 地址 / 离线开关 / 词表目录 / 决策模型层变更 → 热重建 zh/en/ja 检测器
    ///   （保留启动时引擎类型；离线开关由设置界面显式控制，不再隐式随地址变更）
    /// - LLM 参数变更 → 重建 LLM 服务（含 `prompts_dir` 变更时热重载模板库），后续讲解请求立即使用新参数
    /// - 决策表目录变更 → 从新目录重新加载决策表
    /// - 更新内存设置并持久化到设置文件
    /// 按给定设置重建 zh/en/ja 检测器（整体替换路由表）
    ///
    /// 检测代码只在检测器**构造时**从词表文件加载规则——新采纳的规则
    /// 写回词表文件后需要重建检测器才能被检出（`llm_feedback` 采纳路径使用）。
    pub fn rebuild_router(&self, next: &AppSettings) {
        // 构建知识召回（嵌入引擎配置可能随设置变更；知识层开关关闭时不接线——测评消融臂）
        let recall: Option<Arc<dyn gec_core::KnowledgeRecall>> =
            if next.embedding.is_configured() && next.knowledge_enabled {
                let eng = Arc::new(gec_core::EmbeddingEngine::new(next.embedding.clone()));
                let r: Arc<dyn gec_core::KnowledgeRecall> =
                    Arc::new(crate::knowledge_recall::KnowledgeRecallImpl::new(
                        self.learned_knowledge.clone(),
                        eng,
                    ));
                Some(r)
            } else {
                None
            };
        let mut new_router = LanguageRouter::new();
        new_router.register(compose_detector(
            "zh",
            Arc::new(ChineseDetector::new(ZhConfig {
                endpoint: next.hanlp_endpoint.clone(),
                offline_only: next.offline_mode,
                lexicons_dir: PathBuf::from(next.lexicons_dir.clone()),
                ..self.zh_base_cfg.clone()
            })),
            Some(next.decision.clone()),
            recall.clone(),
            Some(next.l3.clone()),
        ));
        new_router.register(compose_detector(
            "en",
            Arc::new(EnglishDetector::new(EnConfig {
                endpoint: next.hanlp_endpoint.clone(),
                offline_only: next.offline_mode,
                lexicons_dir: PathBuf::from(next.lexicons_dir.clone()),
                ..self.en_base_cfg.clone()
            })),
            Some(next.decision.clone()),
            recall.clone(),
            Some(next.l3.clone()),
        ));
        new_router.register(compose_detector(
            "ja",
            Arc::new(JapaneseDetector::new(JaConfig {
                offline_only: next.offline_mode,
                lexicons_dir: PathBuf::from(next.lexicons_dir.clone()),
                ..self.ja_base_cfg.clone()
            })),
            Some(next.decision.clone()),
            recall,
            Some(next.l3.clone()),
        ));
        *self.router.write().unwrap() = Arc::new(new_router);
        // 检测器重建 ⇒ 生效词表变化 ⇒ 刷新词表文件哈希（规则侧版本指纹输入）
        *self.lexicon_hash.write().unwrap() =
            shared_model::lexicon_files_hash(Path::new(&next.lexicons_dir));
        tracing::info!(
            endpoint = %next.hanlp_endpoint,
            offline = next.offline_mode,
            lexicons = %next.lexicons_dir,
            decision = next.decision.enabled(),
            l3 = next.l3.enabled(),
            "zh/en/ja 检测器已重建"
        );
    }

    pub fn apply_settings(&self, next: AppSettings) {
        let old = self.settings.read().unwrap().clone();
        if old.hanlp_endpoint != next.hanlp_endpoint
            || old.offline_mode != next.offline_mode
            || old.lexicons_dir != next.lexicons_dir
            || old.decision != next.decision
            || old.l3 != next.l3
            || old.embedding != next.embedding
        {
            self.rebuild_router(&next);
            // 同步更新嵌入引擎（采纳 / 知识条目写入路径使用最新配置）
            if old.embedding != next.embedding {
                *self.embedding_engine.write().unwrap() = if next.embedding.is_configured() {
                    Some(Arc::new(gec_core::EmbeddingEngine::new(
                        next.embedding.clone(),
                    )))
                } else {
                    None
                };
                tracing::info!(
                    configured = next.embedding.is_configured(),
                    "嵌入引擎配置变更，已重建（即时生效）"
                );
                // 预热嵌入服务（把模型拉进内存，避免首次知识召回时 Ollama 冷启动
                // 20s+ 拖慢检测关键路径）；后台执行，不阻塞设置请求
                if next.embedding.is_configured() {
                    let engine = self.embedding_engine.read().unwrap().clone();
                    if let Some(engine) = engine {
                        tauri::async_runtime::spawn(async move {
                            match engine.warmup().await {
                                Ok(()) => {
                                    tracing::info!("嵌入服务预热完成（首次知识召回不再命中冷启动）")
                                }
                                Err(e) => tracing::warn!(
                                    error = %e,
                                    "嵌入服务预热失败（首次知识召回将静默跳过，不影响检测）"
                                ),
                            }
                        });
                    }
                }
            }
        }
        if old.llm != next.llm {
            let service = LlmService {
                config: next.llm.clone(),
                templates: TemplateStore::load(&next.llm.prompts_dir),
            };
            tracing::info!(
                base_url = %next.llm.base_url,
                model = %next.llm.model,
                prompts = %next.llm.prompts_dir.display(),
                enabled = next.llm.enabled(),
                "LLM 参数变更，LLM 讲解服务已重建（即时生效）"
            );
            *self.llm.write().unwrap() = service;
            // prompts_dir 变更 → 热重载纠错模型注册表（CEC 工作流 E）
            if old.llm.prompts_dir != next.llm.prompts_dir {
                let registry = ModelRegistry::load(&next.llm.prompts_dir.join("models.json"));
                tracing::info!(
                    dir = %next.llm.prompts_dir.display(),
                    models = registry.models.len(),
                    "模型注册表已重载（提示词目录变更，即时生效）"
                );
                *self.model_registry.write().unwrap() = registry;
            }
        }
        if old.tables_dir != next.tables_dir {
            let dir = Path::new(&next.tables_dir);
            match self.tables.load_from_dir(dir) {
                Ok(n) => tracing::info!(
                    dir = %dir.display(),
                    count = n,
                    "决策表目录变更，决策表已重新加载（即时生效）"
                ),
                Err(e) => tracing::error!(
                    dir = %dir.display(),
                    error = %e,
                    "决策表目录变更，决策表重新加载失败（保留当前表）"
                ),
            }
        }
        *self.settings.write().unwrap() = next.clone();
        match next.save_to(&self.settings_path) {
            Ok(()) => tracing::info!(
                path = %self.settings_path.display(),
                "设置已保存（即时生效）"
            ),
            Err(e) => tracing::error!(
                path = %self.settings_path.display(),
                error = %e,
                "设置持久化失败（内存中已生效，重启后回退环境变量）"
            ),
        }
    }
}

/// 检测文本中的语法错误（支持可选 category 过滤 + 规则开关 + 自定义词白名单 + 三级响应级别 + 漏报强制兜底，与 Web 端一致）
/// 检测响应（Tauri 命令返回；与 API `DetectResponse` 同构）
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct DetectResult {
    pub errors: Vec<Error>,
    /// 决策模型是否弃权
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub decision_abstained: bool,
    /// 各阶段计时（规则 / NLP（含预热）/ 决策 / L3 / 召回 / LLM / 总耗时）
    #[serde(default)]
    pub timings: gec_core::DetectTimings,
    /// 本次结果来自历史复用（同文本 + 参数一致 + 无 LLM 的近期记录）
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub from_history: bool,
    /// LLM 兜底层是否实际执行且未检出错误（「LLM 复查确认干净」）
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub llm_confirmed_clean: bool,
    /// 层级贡献摘要：本次检测实际参与的每层状态与检出数（前端「检测路径」）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<gec_core::LayerReport>,
}

/// 检测 + 归因响应（Tauri 命令返回）
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct AnalyzeResult {
    pub lang: String,
    pub errors: Vec<AnalyzedError>,
    pub session_id: String,
    /// 决策模型是否弃权
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub decision_abstained: bool,
    /// 各阶段计时（规则 / NLP（含预热）/ 决策 / L3 / 召回 / LLM / 总耗时）
    #[serde(default)]
    pub timings: gec_core::DetectTimings,
    /// 本次结果来自历史复用（同文本 + 参数一致 + 无 LLM 的近期记录）
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub from_history: bool,
    /// LLM 兜底层是否实际执行且未检出错误（「LLM 复查确认干净」）
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub llm_confirmed_clean: bool,
    /// 层级贡献摘要：本次检测实际参与的每层状态与检出数（前端「检测路径」）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<gec_core::LayerReport>,
}

/// 历史复用（重复检测）：查同文本历史，最近一条**参数一致且不含 LLM**
/// 的记录直接反序列化其结果（跳过引擎；调用方不写新历史）。
///
/// 门槛（[`shared_model::history_reusable`]）：
/// - 记录携带可解析的 `params_json`（旧记录不可复用）；
/// - 存储文本与当前文本一致（`text_hash` 命中后的精确校验，防哈希碰撞）；
/// - 参数逐项一致（level / force_llm / category / 决策层 / L3 / LLM 配置 /
///   **决策模型版本号** / **规则侧版本指纹**——规则侧或模型侧任何变化都会使历史失效）；
/// - 记录为 level 1 非强制运行，且结果中无 `source == "llm"` 的错误。
async fn try_reuse_history(
    core: &Core,
    lang: &str,
    text: &str,
    category: &Option<String>,
    level: u8,
) -> Option<gec_core::DetectOutcome> {
    let hash = shared_model::text_hash(text);
    let candidates = core.detect_history.latest_for_text(lang, &hash, 10).ok()?;
    if candidates.is_empty() {
        return None;
    }
    let current = core
        .current_detect_params(lang, level, false, category)
        .await;
    let idx = candidates.iter().position(|c| {
        shared_model::history_reusable(
            c.params_json.as_deref().unwrap_or(""),
            &c.text,
            &c.errors_json,
            &current,
            text,
        )
    })?;
    let h = &candidates[idx];
    let errors: Vec<Error> = serde_json::from_str(&h.errors_json).ok()?;
    tracing::debug!(
        history_id = h.id,
        errors = errors.len(),
        "重复检测命中历史，复用结果（跳过引擎）"
    );
    Some(gec_core::DetectOutcome {
        errors,
        ..Default::default()
    })
}

#[tauri::command]
pub async fn detect(
    core: tauri::State<'_, Arc<Core>>,
    lang: String,
    text: String,
    category: Option<String>,
    // 三级响应级别：1 = 规则（+ 决策模型，若启用）；2 = + 决策模型明确弃权时
    // LLM 兜底；3 = + 强制 LLM（缺省 1）
    level: Option<u8>,
    // 「漏报」强制兜底：无视 level 触发条件，无条件对全文跑一次 LLM 兜底
    force_llm: bool,
) -> Result<DetectResult, String> {
    let level = level.unwrap_or(1);
    if !(1..=3).contains(&level) {
        return Err(format!("无效响应级别: {level}（应为 1-3）"));
    }
    // 读锁内仅克隆路由 Arc（锁立即释放），避免 guard 跨 await（非 Send）
    let router = core.router.read().unwrap().clone();
    // level 1 = 常规检测；level 2/3 或 force_llm = 完整级联（LLM 兜底按需/强制触发）
    // 统一走 detect_level：各阶段计时（含 NLP 预热）随结果返回并持久化
    let (mut outcome, from_history) = if level <= 1 && !force_llm {
        // 历史复用：同文本 + 参数一致 + 无 LLM 的最近记录直接复用（跳过引擎）
        if let Some(reused) = try_reuse_history(&**core, &lang, &text, &category, 1).await {
            (reused, true)
        } else {
            let engine = router
                .get(&lang)
                .cloned()
                .ok_or_else(|| format!("语言 {lang} 无检测器"))?;
            (
                engine
                    .detect_level(&text, 1, false, None)
                    .await
                    .map_err(|e| e.to_string())?,
                false,
            )
        }
    } else {
        let engine = router
            .get(&lang)
            .cloned()
            .ok_or_else(|| format!("语言 {lang} 无检测器"))?;
        let llm = core.llm_detect_engine(&lang);
        (
            engine
                .detect_level(&text, level, force_llm, llm.as_deref())
                .await
                .map_err(|e| match e {
                    gec_core::DetectorError::LlmNotConfigured(m) => m,
                    other => other.to_string(),
                })?,
            false,
        )
    };
    // 过滤已禁用的规则（开关状态存于 SQLite 词表存储；未入库的规则 id 视为启用；
    // 批量查询：一次读规则表 + 内存匹配，而非逐条点查（N+1））
    let rule_ids: Vec<&str> = outcome
        .errors
        .iter()
        .filter_map(|e| e.rule_id.as_deref())
        .collect();
    let disabled: std::collections::HashSet<String> = core
        .lexicon_store
        .get_rules(&lang, &rule_ids)
        .ok()
        .map(|defs| {
            defs.into_iter()
                .filter_map(|d| d.filter(|d| !d.enabled).map(|d| d.id))
                .collect()
        })
        .unwrap_or_default();
    outcome
        .errors
        .retain(|e| !e.rule_id.as_deref().is_some_and(|id| disabled.contains(id)));
    // 过滤自定义词白名单
    outcome
        .errors
        .retain(|e| !core.custom_words.is_whitelisted(&lang, &e.original));
    // 按 category 过滤
    if let Some(cat) = &category {
        outcome
            .errors
            .retain(|e| e.category.as_deref() == Some(cat.as_str()));
    }
    // 填充 W3C ITS 2.0 issue_type
    for e in &mut outcome.errors {
        if e.issue_type.is_none() {
            e.issue_type = Some(derive_issue_type(&e.error_type).to_string());
        }
    }
    // anomaly 自动采纳（二期飞轮 L1）：LLM 高置信命中自动沉淀到向量知识库；
    // 转后台任务（逐候选 embed 调用 + 知识库读写不阻塞检测响应）
    {
        let core = (*core).clone();
        let lang = lang.clone();
        let errors = outcome.errors.clone();
        tokio::spawn(async move {
            auto_adopt_anomalies_cmd(&core, &lang, &errors).await;
        });
    }
    // 自动持久化（与 api-server detect 路由对齐：session_id=None、无归因结果；
    // 含各阶段计时 + 参数快照 + 文本哈希；后台写入，不阻塞检测响应）
    // 历史复用不写新记录（避免短时间反复检测刷出重复历史）
    if !from_history {
        let core = (*core).clone();
        let lang = lang.clone();
        let text = text.clone();
        let errors_json =
            serde_json::to_string(&outcome.errors).unwrap_or_else(|_| "[]".to_string());
        let timings_json = (!outcome.timings.is_empty())
            .then(|| serde_json::to_string(&outcome.timings).unwrap_or_else(|_| "{}".to_string()));
        let params = core
            .current_detect_params(&lang, level, force_llm, &category)
            .await;
        let params_json = serde_json::to_string(&params).ok();
        let text_hash = shared_model::text_hash(&text);
        tokio::spawn(async move {
            if let Err(e) = core.detect_history.insert(
                None,
                &lang,
                &text,
                &errors_json,
                None,
                timings_json.as_deref(),
                Some(APP_VERSION),
                params_json.as_deref(),
                Some(&text_hash),
            ) {
                tracing::warn!(error = %e, "检测历史写入失败（不影响返回结果）");
            }
        });
    }
    Ok(DetectResult {
        errors: outcome.errors,
        decision_abstained: outcome.decision_abstained,
        timings: outcome.timings,
        from_history,
        llm_confirmed_clean: outcome.llm_confirmed_clean,
        layers: outcome.layers,
    })
}

/// 检测 + 归因一站式（支持三级响应级别 + 漏报强制兜底）
#[tauri::command]
pub async fn analyze(
    core: tauri::State<'_, Arc<Core>>,
    lang: String,
    text: String,
    session_id: Option<String>,
    // 三级响应级别（1/2/3，缺省 1；语义同 `detect` 的 level）
    level: Option<u8>,
    // 「漏报」强制兜底（语义同 `detect` 的 force_llm）
    force_llm: bool,
) -> Result<AnalyzeResult, String> {
    let level = level.unwrap_or(1);
    if !(1..=3).contains(&level) {
        return Err(format!("无效响应级别: {level}（应为 1-3）"));
    }
    let router = core.router.read().unwrap().clone();
    // level 1 = 常规检测；level 2/3 或 force_llm = 完整级联（LLM 兜底按需/强制触发）
    // 统一走 detect_level：各阶段计时（含 NLP 预热）随结果返回并持久化
    let (mut outcome, from_history) = if level <= 1 && !force_llm {
        // 历史复用：同文本 + 参数一致 + 无 LLM 的最近记录直接复用（跳过引擎）
        if let Some(reused) = try_reuse_history(&**core, &lang, &text, &None, 1).await {
            (reused, true)
        } else {
            let engine = router
                .get(&lang)
                .cloned()
                .ok_or_else(|| format!("语言 {lang} 无检测器"))?;
            (
                engine
                    .detect_level(&text, 1, false, None)
                    .await
                    .map_err(|e| e.to_string())?,
                false,
            )
        }
    } else {
        let engine = router
            .get(&lang)
            .cloned()
            .ok_or_else(|| format!("语言 {lang} 无检测器"))?;
        let llm = core.llm_detect_engine(&lang);
        (
            engine
                .detect_level(&text, level, force_llm, llm.as_deref())
                .await
                .map_err(|e| match e {
                    gec_core::DetectorError::LlmNotConfigured(m) => m,
                    other => other.to_string(),
                })?,
            false,
        )
    };
    // 与 detect 命令一致的后处理：过滤已禁用规则 + 自定义词白名单
    // （「放弃」反馈 / 「停用规则」建议在重检测时同样生效）
    let rule_ids: Vec<&str> = outcome
        .errors
        .iter()
        .filter_map(|e| e.rule_id.as_deref())
        .collect();
    let disabled: std::collections::HashSet<String> = core
        .lexicon_store
        .get_rules(&lang, &rule_ids)
        .ok()
        .map(|defs| {
            defs.into_iter()
                .filter_map(|d| d.filter(|d| !d.enabled).map(|d| d.id))
                .collect()
        })
        .unwrap_or_default();
    outcome
        .errors
        .retain(|e| !e.rule_id.as_deref().is_some_and(|id| disabled.contains(id)));
    outcome
        .errors
        .retain(|e| !core.custom_words.is_whitelisted(&lang, &e.original));
    let engine = core.tables.get(&lang);
    let (history, meta) = match &session_id {
        Some(id) => {
            let record = core.sessions.get_or_create(id, &lang);
            (record.total_errors(), Some(record.meta))
        }
        None => (0, None),
    };

    let mut out = Vec::new();
    for error in outcome.errors {
        let attribution = match &engine {
            Some(engine) => {
                // 英文错误：推断具体知识点，命中细粒度规则。
                // 数据优先：已发布知识点的改错对精确命中优先（新导入书自动参与归点），
                // 未命中回退静态映射。知识层开关关闭（测评消融臂）→ 只用静态映射。
                let knowledge_on = core.settings.read().unwrap().knowledge_enabled;
                let grammar_point = if lang == "en" {
                    let fallback = gec_en::classify_point(
                        &error.error_type,
                        &error.original,
                        error.correction.as_deref().unwrap_or(""),
                    );
                    if knowledge_on {
                        attribution_engine::knowledge::classify_point_en(
                            &core.knowledge,
                            &error.error_type,
                            &error.original,
                            error.correction.as_deref().unwrap_or(""),
                            fallback,
                        )
                    } else {
                        fallback
                    }
                } else {
                    None
                };
                let ctx = AttributionContext {
                    lang: lang.clone(),
                    error_type: error.error_type.clone(),
                    confidence: error.confidence,
                    session_error_count: history,
                    session_meta: meta.clone(),
                    grammar_point,
                };
                // 表级归因 + 知识库富化（点级补救/CEFR/实例 + 知识源引用；知识层开关关闭时跳过）
                let mut result = engine.attribute(&ctx).await.map_err(|e| e.to_string())?;
                if knowledge_on {
                    core.knowledge.enrich(&ctx, &mut result);
                }
                Some(result)
            }
            None => None,
        };
        out.push(AnalyzedError { error, attribution });
    }
    let session_id = session_id.unwrap_or_else(|| format!("s_{}", uuid::Uuid::new_v4().simple()));
    // 自动持久化（含归因结果 + 各阶段计时 + 参数快照 + 文本哈希；与 api-server analyze 路由对齐；
    // 后台写入，不阻塞检测响应）
    // 历史复用不写新记录（避免短时间反复检测刷出重复历史）
    if !from_history {
        let core = (*core).clone();
        let lang = lang.clone();
        let text = text.clone();
        let errors_json = serde_json::to_string(&out.iter().map(|a| &a.error).collect::<Vec<_>>())
            .unwrap_or_else(|_| "[]".to_string());
        let analyzed_json = serde_json::to_string(&out).ok();
        let timings_json = (!outcome.timings.is_empty())
            .then(|| serde_json::to_string(&outcome.timings).unwrap_or_else(|_| "{}".to_string()));
        let params = core
            .current_detect_params(&lang, level, force_llm, &None)
            .await;
        let params_json = serde_json::to_string(&params).ok();
        let text_hash = shared_model::text_hash(&text);
        let session_id = session_id.clone();
        tokio::spawn(async move {
            if let Err(e) = core.detect_history.insert(
                Some(&session_id),
                &lang,
                &text,
                &errors_json,
                analyzed_json.as_deref(),
                timings_json.as_deref(),
                Some(APP_VERSION),
                params_json.as_deref(),
                Some(&text_hash),
            ) {
                tracing::warn!(error = %e, "检测历史写入失败（不影响返回结果）");
            }
        });
    }
    Ok(AnalyzeResult {
        lang,
        errors: out,
        session_id,
        decision_abstained: outcome.decision_abstained,
        timings: outcome.timings,
        from_history,
        llm_confirmed_clean: outcome.llm_confirmed_clean,
        layers: outcome.layers,
    })
}

/// 列出所有决策表
#[tauri::command]
pub fn list_tables(core: tauri::State<'_, Arc<Core>>) -> Vec<DecisionTableMeta> {
    core.tables.list()
}

/// 获取指定语言决策表原始 JSON
#[tauri::command]
pub fn get_table(core: tauri::State<'_, Arc<Core>>, lang: String) -> Result<String, String> {
    core.tables
        .get_raw(&lang)
        .ok_or_else(|| format!("语言 {lang} 无决策表"))
}

/// 热更新决策表
#[tauri::command]
pub fn update_table(
    core: tauri::State<'_, Arc<Core>>,
    lang: String,
    table_json: String,
) -> Result<DecisionTableMeta, String> {
    core.tables
        .update(&lang, &table_json)
        .map_err(|e| e.to_string())
}

/// 知识源注册表（知识库「知识库」页展示；目录缺失时返回空列表）
#[tauri::command]
pub fn knowledge_sources(core: tauri::State<'_, Arc<Core>>) -> Vec<KnowledgeSource> {
    core.knowledge.sources()
}

/// 知识点列表（可按知识源过滤；1:N 关联）
#[tauri::command]
pub fn knowledge_points(
    core: tauri::State<'_, Arc<Core>>,
    lang: String,
    source_id: Option<String>,
) -> Vec<KnowledgePoint> {
    core.knowledge.points(&lang, source_id.as_deref())
}

// ---------------------------------------------------------------------------
// 知识源导入管线（一键导入：正文分块 → 后台逐块 agent 挖掘 → 草稿区 → 人工一键发布；
// 与 API 端 /gec/knowledge/ingest* 同构）
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
pub struct IngestReq {
    pub source_id: String,
    pub title: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub edition: String,
    pub lang: String,
    #[serde(default)]
    pub license_scope: String,
    pub text: String,
}

/// 桌面端默认块大小（与 API 端一致）
const INGEST_CHUNK_CHARS: usize = 6000;

/// 跑一块的 agent 抽取；返回 `None` = 成功，`Some(错误文本)` = 失败（记入会话）
async fn run_ingest_chunk(
    core: &Arc<Core>,
    lang: &str,
    source_id: &str,
    index: usize,
    total: usize,
    chunk_body: &str,
) -> Option<String> {
    let service = core.llm.read().unwrap().clone();
    let Some(template) = service.templates.for_lang_cefr(lang, "ingest") else {
        return Some(format!("语言 {lang} 缺少 ingest 模板"));
    };
    let idx = index.to_string();
    let tot = total.to_string();
    let input = template.render(&[
        ("source_id", source_id),
        ("chunk_index", &idx),
        ("total_chunks", &tot),
        ("chunk", chunk_body),
    ]);
    let ctx = crate::agent_tools::AgentToolCtx::new(core, lang);
    let tools = ctx.build_tools();
    let config = LlmDetectConfig::from_parts(
        service.config.base_url.clone(),
        service.config.api_key.clone(),
        service.config.model.clone(),
        service.config.timeout_secs,
        template.system.clone(),
        input.clone(),
    );
    match tokio::time::timeout(
        gec_core::agent_timeout(),
        gec_core::run_agent(&config, &template.system, &input, tools),
    )
    .await
    {
        Ok(Ok(_result)) => None,
        Ok(Err(e)) => Some(format!("块 {index} agent 运行失败: {e}")),
        Err(_) => Some(format!("块 {index} agent 运行超时")),
    }
}

/// 一键导入开始：注册知识源 + 分块 + 后台逐块 agent 挖掘（草稿只进草稿区）
#[tauri::command]
pub async fn start_ingest(
    core: tauri::State<'_, Arc<Core>>,
    req: IngestReq,
) -> Result<serde_json::Value, String> {
    let core = core.inner().clone();
    if req.source_id.trim().is_empty() || req.title.trim().is_empty() {
        return Err("source_id / title 不能为空".into());
    }
    if req.text.trim().len() < 100 {
        return Err("正文过短（< 100 字符）".into());
    }
    let service = core.llm.read().unwrap().clone();
    if !service.config.enabled() {
        return Err("LLM 未配置：知识源挖掘由内置 agent 执行，请先在设置中配置 LLM".into());
    }
    if service
        .templates
        .for_lang_cefr(&req.lang, "ingest")
        .is_none()
    {
        return Err(format!(
            "语言 {} 缺少 ingest 提示词模板（prompts/{{lang}}_ingest_prompt.json）",
            req.lang
        ));
    }
    let source = KnowledgeSource {
        source_id: req.source_id.trim().to_string(),
        title: req.title.trim().to_string(),
        author: req.author,
        edition: req.edition,
        lang: req.lang.clone(),
        license_scope: if req.license_scope.is_empty() {
            "internal_test".to_string()
        } else {
            req.license_scope
        },
        point_count: 0,
    };
    let chunks = attribution_engine::ingest::chunk_text(&req.text, INGEST_CHUNK_CHARS);
    if chunks.is_empty() {
        return Err("正文分块结果为空".into());
    }
    let summary = core.ingest.open_session(source, chunks.len())?;
    tracing::info!(
        source_id = %summary.source_id,
        chunks = chunks.len(),
        "知识源导入会话已开启（后台逐块 agent 挖掘）"
    );
    let lang = req.lang.clone();
    let source_id = summary.source_id.clone();
    let chunks_total = chunks.len();
    tokio::spawn(async move {
        let total = chunks.len();
        for chunk in chunks {
            if !core.ingest.is_active(&source_id) {
                break;
            }
            let mut err =
                run_ingest_chunk(&core, &lang, &source_id, chunk.index, total, &chunk.text).await;
            if err.is_some() && core.ingest.is_active(&source_id) {
                err = run_ingest_chunk(&core, &lang, &source_id, chunk.index, total, &chunk.text)
                    .await
                    .map(|e| format!("{e}（重试后仍失败）"));
            }
            let active = core
                .ingest
                .mark_chunk_done(&source_id, err)
                .unwrap_or(false);
            let _ = core.ingest.save_to_dir(&core.ingest_dir);
            if !active {
                break;
            }
        }
        tracing::info!(source_id = %source_id, "知识源逐块挖掘完成（等待人工审核发布）");
    });
    Ok(serde_json::json!({
        "source_id": summary.source_id,
        "chunks_total": chunks_total,
    }))
}

/// 导入会话摘要列表
#[tauri::command]
pub fn ingest_sessions(
    core: tauri::State<'_, Arc<Core>>,
) -> Vec<attribution_engine::ingest::IngestSummary> {
    core.ingest.list()
}

/// 会话详情（进度 / 草稿 / 错误 / 状态）
#[tauri::command]
pub fn ingest_session(
    core: tauri::State<'_, Arc<Core>>,
    source_id: String,
) -> Result<attribution_engine::ingest::IngestSession, String> {
    core.ingest
        .session(&source_id)
        .ok_or_else(|| format!("导入会话不存在: {source_id}"))
}

/// 草稿明细（含逐条归并预判）
#[tauri::command]
pub fn ingest_drafts(
    core: tauri::State<'_, Arc<Core>>,
    source_id: String,
) -> Result<Vec<serde_json::Value>, String> {
    let session = core
        .ingest
        .session(&source_id)
        .ok_or_else(|| format!("导入会话不存在: {source_id}"))?;
    let drafts: Vec<attribution_engine::ingest::DraftPoint> =
        session.drafts.iter().map(|d| d.draft.clone()).collect();
    let plan = attribution_engine::ingest::plan_merge(&core.knowledge, &session.source, &drafts);
    Ok(session
        .drafts
        .iter()
        .zip(plan.actions)
        .map(|(s, a)| {
            serde_json::json!({
                "seq": s.seq,
                "staged_at": s.staged_at,
                "draft": s.draft,
                "action": a.action,
                "point_id": a.point_id,
            })
        })
        .collect())
}

/// 发布预览（归并计划，不落库）
#[tauri::command]
pub fn ingest_plan(
    core: tauri::State<'_, Arc<Core>>,
    source_id: String,
) -> Result<attribution_engine::ingest::MergePlan, String> {
    let session = core
        .ingest
        .session(&source_id)
        .ok_or_else(|| format!("导入会话不存在: {source_id}"))?;
    let drafts: Vec<attribution_engine::ingest::DraftPoint> =
        session.drafts.iter().map(|d| d.draft.clone()).collect();
    Ok(attribution_engine::ingest::plan_merge(
        &core.knowledge,
        &session.source,
        &drafts,
    ))
}

/// 人工一键发布（知识库热更新；桌面端本地操作无需 token）
#[tauri::command]
pub fn publish_ingest(
    core: tauri::State<'_, Arc<Core>>,
    source_id: String,
) -> Result<attribution_engine::ingest::ApplyReport, String> {
    crate::agent_tools::publish_session(&core, &source_id)
}

/// 放弃会话（丢弃草稿；生效知识库不受影响）
#[tauri::command]
pub fn discard_ingest(
    core: tauri::State<'_, Arc<Core>>,
    source_id: String,
) -> Result<serde_json::Value, String> {
    let dropped = core.ingest.discard(&source_id)?;
    let staged_file = core.ingest_dir.join(format!("{source_id}.json"));
    if staged_file.exists() {
        let _ = std::fs::remove_file(&staged_file);
    }
    Ok(serde_json::json!({ "source_id": source_id, "dropped_drafts": dropped }))
}
/// 解析预览（书本文件字节 → 元数据/章节/告警；不落任何状态）
#[tauri::command]
pub fn parse_preview(
    core: tauri::State<'_, Arc<Core>>,
    filename: String,
    data: Vec<u8>,
) -> Result<serde_json::Value, String> {
    let _ = core;
    if data.is_empty() {
        return Err("文件为空".into());
    }
    let doc = document_parse::parse_document(&data, &filename).map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "format": doc.format,
        "title": doc.title,
        "author": doc.author,
        "lang_hint": doc.lang_hint,
        "char_count": doc.char_count,
        "warnings": doc.warnings,
        "suggested_source_id": document_parse::suggest_source_id(&doc.title, &filename),
        "chapters": doc.chapters.iter().enumerate().map(|(i, c)| serde_json::json!({
            "index": i,
            "heading": c.heading,
            "chars": c.text.chars().count(),
        })).collect::<Vec<_>>(),
    }))
}

#[derive(serde::Deserialize)]
pub struct IngestFileReq {
    pub filename: String,
    pub data: Vec<u8>,
    pub source_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub edition: String,
    pub lang: String,
    #[serde(default)]
    pub license_scope: String,
    /// 章节索引选择（空 = 全部）
    #[serde(default)]
    pub chapters: Option<Vec<usize>>,
}

/// 一键导入开始（文件路径）：解析 → 章节选择 → 分块 → 后台逐块挖掘
#[tauri::command]
pub async fn start_ingest_file(
    core: tauri::State<'_, Arc<Core>>,
    req: IngestFileReq,
) -> Result<serde_json::Value, String> {
    let core = core.inner().clone();
    if req.source_id.trim().is_empty() {
        return Err("source_id 不能为空".into());
    }
    let service = core.llm.read().unwrap().clone();
    if !service.config.enabled() {
        return Err("LLM 未配置：知识源挖掘由内置 agent 执行，请先在设置中配置 LLM".into());
    }
    if service
        .templates
        .for_lang_cefr(&req.lang, "ingest")
        .is_none()
    {
        return Err(format!(
            "语言 {} 缺少 ingest 提示词模板（prompts/{{lang}}_ingest_prompt.json）",
            req.lang
        ));
    }
    let doc =
        document_parse::parse_document(&req.data, &req.filename).map_err(|e| e.to_string())?;
    let chapters: Vec<attribution_engine::ingest::ChapterInput> = doc
        .chapters
        .iter()
        .enumerate()
        .filter(|(i, _)| req.chapters.as_ref().is_none_or(|sel| sel.contains(i)))
        .map(|(_, c)| attribution_engine::ingest::ChapterInput {
            heading: c.heading.clone(),
            text: c.text.clone(),
        })
        .collect();
    if chapters.is_empty() {
        return Err("所选章节为空".into());
    }
    let title = if req.title.trim().is_empty() {
        doc.title.clone()
    } else {
        req.title.trim().to_string()
    };
    if title.is_empty() {
        return Err("title 不能为空（文件未携带标题时需手填）".into());
    }
    let source = KnowledgeSource {
        source_id: req.source_id.trim().to_string(),
        title,
        author: if req.author.is_empty() {
            doc.author.clone()
        } else {
            req.author
        },
        edition: req.edition,
        lang: req.lang.clone(),
        license_scope: if req.license_scope.is_empty() {
            "internal_test".to_string()
        } else {
            req.license_scope
        },
        point_count: 0,
    };
    let chunks = attribution_engine::ingest::chunk_chapters(&chapters, INGEST_CHUNK_CHARS);
    if chunks.is_empty() {
        return Err("正文分块结果为空".into());
    }
    let summary = core.ingest.open_session(source, chunks.len())?;
    tracing::info!(
        source_id = %summary.source_id,
        format = doc.format,
        chapters = chapters.len(),
        chunks = chunks.len(),
        "知识源导入会话已开启（文件 → 章节分块 → 逐块 agent 挖掘）"
    );
    let lang = req.lang.clone();
    let source_id = summary.source_id.clone();
    tokio::spawn(async move {
        let total = chunks.len();
        for chunk in chunks {
            if !core.ingest.is_active(&source_id) {
                break;
            }
            let mut err =
                run_ingest_chunk(&core, &lang, &source_id, chunk.index, total, &chunk.text).await;
            if err.is_some() && core.ingest.is_active(&source_id) {
                err = run_ingest_chunk(&core, &lang, &source_id, chunk.index, total, &chunk.text)
                    .await
                    .map(|e| format!("{e}（重试后仍失败）"));
            }
            let active = core
                .ingest
                .mark_chunk_done(&source_id, err)
                .unwrap_or(false);
            let _ = core.ingest.save_to_dir(&core.ingest_dir);
            if !active {
                break;
            }
        }
        tracing::info!(source_id = %source_id, "知识源逐块挖掘完成（等待人工审核发布）");
    });
    Ok(serde_json::json!({
        "source_id": summary.source_id,
        "chunks_total": summary.chunks_total,
    }))
}

/// 表内既有规则行 `_id` 清单（喂给 agent，避免重复造同 ID 行）
fn existing_rule_ids(core: &Core, lang: &str) -> String {
    let Some(raw) = core.tables.get_raw(lang) else {
        return String::new();
    };
    let Ok(jdm) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return String::new();
    };
    attribution_engine::dataset::table_rows(&jdm)
        .iter()
        .filter_map(|r| r.get("_id").and_then(|v| v.as_str()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// 跑一个 error_type 批次的规则派生 agent；`None` = 成功
async fn run_rules_batch(
    core: &Arc<Core>,
    lang: &str,
    source_id: &str,
    error_type: &str,
    points_json: &str,
    existing_rules: &str,
) -> Option<String> {
    let service = core.llm.read().unwrap().clone();
    let Some(template) = service.templates.for_lang_cefr(lang, "rules_ingest") else {
        return Some(format!("语言 {lang} 缺少 rules_ingest 模板"));
    };
    let input = template.render(&[
        ("source_id", source_id),
        ("error_type", error_type),
        ("points", points_json),
        ("existing_rules", existing_rules),
    ]);
    let ctx = crate::agent_tools::AgentToolCtx::new(core, lang);
    let tools = ctx.build_tools();
    let config = LlmDetectConfig::from_parts(
        service.config.base_url.clone(),
        service.config.api_key.clone(),
        service.config.model.clone(),
        service.config.timeout_secs,
        template.system.clone(),
        input.clone(),
    );
    match tokio::time::timeout(
        gec_core::agent_timeout(),
        gec_core::run_agent(&config, &template.system, &input, tools),
    )
    .await
    {
        Ok(Ok(_result)) => None,
        Ok(Err(e)) => Some(format!("规则派生 {error_type} agent 运行失败: {e}")),
        Err(_) => Some(format!("规则派生 {error_type} agent 运行超时")),
    }
}

/// 开启规则派生（知识点挖掘完成后、发布前；按 error_type 分批跑 agent）
#[tauri::command]
pub async fn derive_rules(
    core: tauri::State<'_, Arc<Core>>,
    source_id: String,
) -> Result<serde_json::Value, String> {
    let core = core.inner().clone();
    let session = core
        .ingest
        .session(&source_id)
        .ok_or_else(|| format!("导入会话不存在: {source_id}"))?;
    if session.drafts.is_empty() {
        return Err("会话没有知识点草稿，先完成挖掘".into());
    }
    let mut types: Vec<String> = session
        .drafts
        .iter()
        .map(|d| d.draft.error_type.clone())
        .collect();
    types.sort();
    types.dedup();
    core.ingest.begin_rules(&source_id, types.len())?;
    let service = core.llm.read().unwrap().clone();
    if !service.config.enabled() {
        return Err("LLM 未配置：规则派生由内置 agent 执行，请先在设置中配置 LLM".into());
    }
    if service
        .templates
        .for_lang_cefr(&session.source.lang, "rules_ingest")
        .is_none()
    {
        return Err(format!(
            "语言 {} 缺少 rules_ingest 提示词模板（prompts/{{lang}}_rules_ingest_prompt.json）",
            session.source.lang
        ));
    }
    let lang = session.source.lang.clone();
    let sid = source_id.clone();
    let total_batches = types.len();
    tokio::spawn(async move {
        for error_type in types {
            if !core.ingest.is_rules_active(&sid) {
                break;
            }
            let session = match core.ingest.session(&sid) {
                Some(s) => s,
                None => break,
            };
            let points: Vec<&attribution_engine::ingest::DraftPoint> = session
                .drafts
                .iter()
                .map(|d| &d.draft)
                .filter(|d| d.error_type == error_type)
                .collect();
            let points_json = serde_json::to_string(&points).unwrap_or_default();
            let existing = existing_rule_ids(&core, &lang);
            let err =
                run_rules_batch(&core, &lang, &sid, &error_type, &points_json, &existing).await;
            let active = core.ingest.mark_rule_batch_done(&sid, err).unwrap_or(false);
            let _ = core.ingest.save_to_dir(&core.ingest_dir);
            if !active {
                break;
            }
        }
        tracing::info!(source_id = %sid, "规则派生批次完成（等待人工审阅 rules-plan）");
    });
    Ok(serde_json::json!({ "source_id": source_id, "batches_total": total_batches }))
}

/// 规则归并预览（新增/替换/冲突；不落库）
#[tauri::command]
pub fn ingest_rules_plan(
    core: tauri::State<'_, Arc<Core>>,
    source_id: String,
) -> Result<Option<attribution_engine::ingest::RulePlan>, String> {
    let core = core.inner();
    let session = core
        .ingest
        .session(&source_id)
        .ok_or_else(|| format!("导入会话不存在: {source_id}"))?;
    if session.rule_drafts.is_empty() {
        return Ok(None);
    }
    let lang = session.source.lang.clone();
    let raw = core
        .tables
        .get_raw(&lang)
        .ok_or_else(|| format!("语言 {lang} 无决策表"))?;
    let drafts: Vec<attribution_engine::ingest::DraftRule> = session
        .rule_drafts
        .iter()
        .map(|d| d.draft.clone())
        .collect();
    Ok(Some(attribution_engine::ingest::plan_rules(
        &raw, &lang, &source_id, &drafts,
    )?))
}

// ─────────────────────────────────────────────────────────────────────────────
// 知识源资源包（盘点 / 打包导出 / 导入 / 删除）
// 与 api-server routes/knowledge_dataset.rs 同一语义（纯逻辑在
// attribution_engine::dataset；桌面端本地操作、无 token 鉴权）。
// ─────────────────────────────────────────────────────────────────────────────

pub(crate) fn tables_dir_of(core: &Core) -> PathBuf {
    PathBuf::from(core.settings.read().unwrap().tables_dir.clone())
}

/// 某语言决策表文件路径（发布派生规则等写回用）
pub(crate) fn tables_file_path(core: &Core, lang: &str) -> PathBuf {
    tables_dir_of(core).join(format!("{lang}_gec_attribution.json"))
}

pub(crate) fn pack_atomic_write(path: &std::path::Path, content: &str) -> Result<(), String> {
    // 临时名带随机后缀：并发写同一目标不互相踩临时文件
    let tmp = path.with_extension(format!(
        "{}.tmp.{}",
        path.extension().and_then(|e| e.to_str()).unwrap_or("tmp"),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::write(&tmp, content).map_err(|e| format!("写入失败 {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("替换失败 {}: {e}", path.display()))?;
    Ok(())
}

/// 资源包盘点（删除前预览）
#[tauri::command]
pub fn dataset_summary(
    core: tauri::State<'_, Arc<Core>>,
    source_id: String,
) -> Result<attribution_engine::PackSummary, String> {
    let src = core
        .knowledge
        .sources()
        .into_iter()
        .find(|s| s.source_id == source_id)
        .ok_or_else(|| format!("知识源不存在: {source_id}"))?;
    let staging = core.ingest.session(&source_id);
    let tables: Vec<(String, serde_json::Value)> = core
        .tables
        .list()
        .into_iter()
        .filter_map(|m| {
            core.tables
                .get_raw(&m.lang)
                .and_then(|raw| serde_json::from_str(&raw).ok())
                .map(|jdm| (m.lang, jdm))
        })
        .collect();
    let dir = tables_dir_of(&core);
    let aux = (
        dir.join(format!("{}_points.json", src.lang)).exists(),
        dir.join(format!("{}_point_index.json", src.lang)).exists(),
    );
    attribution_engine::summarize(&core.knowledge, staging.as_ref(), &tables, aux, &source_id)
}

/// 打包导出资源包 → zip 压缩包写盘（单条目 pack.json，deflate 压缩）。
/// `path` = 前端原生「另存为」对话框选定的保存地址（桌面端弹出保存对话框）；
/// 缺省写 `{知识库目录}/resource-packs/{source_id}-resource-pack.zip`。
/// 返回保存地址与组成统计。
#[tauri::command]
pub fn dataset_export(
    core: tauri::State<'_, Arc<Core>>,
    source_id: String,
    path: Option<String>,
) -> Result<serde_json::Value, String> {
    let src = core
        .knowledge
        .sources()
        .into_iter()
        .find(|s| s.source_id == source_id)
        .ok_or_else(|| format!("知识源不存在: {source_id}"))?;
    let staging = core.ingest.session(&source_id);
    let mut pack = attribution_engine::export_pack(&core.knowledge, staging.as_ref(), &source_id)?;
    // 决策表组件：凡存在该源标记行的语言都携带整表快照（与 clean 的跨语言
    // 摘除范围对称，保证「导出 → 删除 → 导入」完整往返）
    for meta in core.tables.list() {
        let Some(raw) = core.tables.get_raw(&meta.lang) else {
            continue;
        };
        if let Ok(jdm) = serde_json::from_str::<serde_json::Value>(&raw) {
            if attribution_engine::tagged_rule_count(&jdm, &source_id) > 0 {
                pack.components
                    .decision_tables
                    .push(attribution_engine::PackDecisionTable {
                        lang: meta.lang.clone(),
                        jdm,
                    });
            }
        }
    }
    let dir = tables_dir_of(&core);
    // aux 组件（源原始提取数据）：条目级按源过滤（entry.source_id 归属；
    // 同语言多源时其他源条目不混入包，旧格式整文件判给本源并注入标记）
    for (aux_path, slot) in [
        (
            dir.join(format!("{}_points.json", src.lang)),
            &mut pack.components.aux_points,
        ),
        (
            dir.join(format!("{}_point_index.json", src.lang)),
            &mut pack.components.aux_index,
        ),
    ] {
        if aux_path.exists() {
            let content = std::fs::read_to_string(&aux_path).map_err(|e| e.to_string())?;
            let aux: serde_json::Value =
                serde_json::from_str(&content).map_err(|e| e.to_string())?;
            let (owned, _) = attribution_engine::aux_split_source(&aux, &source_id);
            if !attribution_engine::aux_is_empty(&owned) {
                *slot = Some(owned);
            }
        }
    }
    pack.checksum = attribution_engine::dataset::pack_checksum(&pack.source, &pack.components);

    let zip = attribution_engine::dataset::pack_to_zip(&pack)?;
    // 保存位置：「另存为」选定路径优先；缺省写默认导出目录
    let file_path = match path {
        Some(p) => {
            let p = PathBuf::from(p);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("创建保存目录失败 {}: {e}", parent.display()))?;
            }
            p
        }
        None => {
            let packs_dir = core.knowledge_dir.join("resource-packs");
            std::fs::create_dir_all(&packs_dir).map_err(|e| format!("创建导出目录失败: {e}"))?;
            let mut name = format!("{source_id}-resource-pack.zip");
            // 删除前自动备份不覆盖既有导出：同名时加时间戳
            if packs_dir.join(&name).exists() {
                name = format!(
                    "{source_id}-resource-pack-{}.zip",
                    chrono::Utc::now().format("%Y%m%d%H%M%S")
                );
            }
            packs_dir.join(name)
        }
    };
    // 原子写：临时文件 + rename（大 zip 导出中途失败不留半截文件）
    let tmp = file_path.with_extension(format!("zip.tmp.{}", uuid::Uuid::new_v4().simple()));
    std::fs::write(&tmp, &zip).map_err(|e| format!("写入失败 {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &file_path)
        .map_err(|e| format!("替换失败 {}: {e}", file_path.display()))?;
    Ok(serde_json::json!({
        "file_path": file_path.display().to_string(),
        "filename": file_path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
        "size_bytes": zip.len(),
        "points": pack.components.knowledge_points.len(),
        "has_table": !pack.components.decision_tables.is_empty(),
        "has_aux": pack.components.aux_points.is_some(),
        "has_staging": pack.components.staging.is_some(),
    }))
}

/// 导入资源包（校验 → 知识库热更新 → 决策表按 _meta 合并 → 决策数据写回 → 草稿恢复）
#[tauri::command]
pub fn dataset_import(
    core: tauri::State<'_, Arc<Core>>,
    pack: attribution_engine::ResourcePack,
) -> Result<serde_json::Value, String> {
    apply_pack(core.inner(), pack)
}

/// 导入资源包（字节自适应：zip 压缩包 / 裸 JSON——web-ui 文件选择器直接上传字节）
#[tauri::command]
pub fn dataset_import_bytes(
    core: tauri::State<'_, Arc<Core>>,
    data: Vec<u8>,
) -> Result<serde_json::Value, String> {
    let pack = attribution_engine::dataset::pack_from_bytes(&data)?;
    apply_pack(core.inner(), pack)
}

fn apply_pack(
    core: &Arc<Core>,
    pack: attribution_engine::ResourcePack,
) -> Result<serde_json::Value, String> {
    attribution_engine::validate_pack(&pack)?;
    let source_id = pack.source.source_id.clone();

    // ⓪ 预检：全部决策表合并结果先算出并通过引擎校验，再开始任何落盘——
    //   杜绝「知识库已写入、表更新中途失败」的半应用状态
    struct TableUpdate {
        lang: String,
        json: String,
        installed: bool,
        replaced: usize,
        added: usize,
    }
    let mut updates: Vec<TableUpdate> = Vec::new();
    for dt in &pack.components.decision_tables {
        let (json, installed, replaced, added) = match core.tables.get_raw(&dt.lang) {
            None => (
                serde_json::to_string(&dt.jdm).map_err(|e| e.to_string())?,
                true,
                0,
                0,
            ),
            Some(raw) => {
                let mut jdm: serde_json::Value =
                    serde_json::from_str(&raw).map_err(|e| e.to_string())?;
                let (r, a) = attribution_engine::merge_tagged_rows(
                    &mut jdm,
                    &attribution_engine::dataset::table_rows(&dt.jdm),
                    &source_id,
                );
                (
                    serde_json::to_string(&jdm).map_err(|e| e.to_string())?,
                    false,
                    r,
                    a,
                )
            }
        };
        attribution_engine::AttributionEngine::validate(&json)
            .map_err(|e| format!("决策表合并结果校验未通过（{}），整体拒收: {e}", dt.lang))?;
        updates.push(TableUpdate {
            lang: dt.lang.clone(),
            json,
            installed,
            replaced,
            added,
        });
    }

    let knowledge = attribution_engine::import_pack(&core.knowledge, &pack)?;
    core.knowledge.save_to_dir(&core.knowledge_dir)?;

    let mut table_installed = false;
    let mut rules_replaced = 0usize;
    let mut rules_added = 0usize;
    for u in updates {
        core.tables
            .update(&u.lang, &u.json)
            .map_err(|e| e.to_string())?;
        pack_atomic_write(
            &tables_dir_of(core).join(format!("{}_gec_attribution.json", u.lang)),
            &u.json,
        )?;
        table_installed |= u.installed;
        rules_replaced += u.replaced;
        rules_added += u.added;
    }

    // 决策数据（aux 文件）：条目级合并——该源条目以资源包为权威，
    // 其他源条目原样保留（同语言多源不互相覆盖）
    let dir = tables_dir_of(core);
    let mut aux_written = 0usize;
    for (value, name) in [
        (&pack.components.aux_points, "points"),
        (&pack.components.aux_index, "point_index"),
    ] {
        if let Some(v) = value {
            let path = dir.join(format!("{}_{}.json", pack.source.lang, name));
            let merged = if path.exists() {
                let content = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
                let current: serde_json::Value =
                    serde_json::from_str(&content).map_err(|e| e.to_string())?;
                attribution_engine::aux_merge_source(&current, v, &source_id)
            } else {
                v.clone()
            };
            let json = serde_json::to_string_pretty(&merged).map_err(|e| e.to_string())?;
            pack_atomic_write(&path, &json)?;
            aux_written += 1;
        }
    }

    // staging 草稿会话：仅当无同 ID 会话时恢复（不打断进行中的挖掘；
    // 检查 + 插入同锁完成，杜绝与并发 ingest 启动的 TOCTOU）
    let mut staging_restored = false;
    if let Some(session) = &pack.components.staging {
        if core.ingest.put_session_if_absent(session.clone()) {
            std::fs::create_dir_all(&core.ingest_dir).map_err(|e| e.to_string())?;
            pack_atomic_write(
                &core.ingest_dir.join(format!("{source_id}.json")),
                &serde_json::to_string_pretty(session).map_err(|e| e.to_string())?,
            )?;
            staging_restored = true;
        }
    }
    Ok(serde_json::json!({
        "knowledge": knowledge,
        "table_installed": table_installed,
        "rules_replaced": rules_replaced,
        "rules_added": rules_added,
        "aux_written": aux_written,
        "staging_restored": staging_restored,
    }))
}

/// 删除资源包（知识库 + 决策表该源规则行 + 决策数据 + 草稿；不可逆）
#[tauri::command]
pub fn dataset_clean(
    core: tauri::State<'_, Arc<Core>>,
    source_id: String,
    keep_orphan_points: Option<bool>,
) -> Result<serde_json::Value, String> {
    let src = core
        .knowledge
        .sources()
        .into_iter()
        .find(|s| s.source_id == source_id)
        .ok_or_else(|| format!("知识源不存在: {source_id}"))?;

    // ⓪ 预检：各决策表摘除结果先算出并通过引擎校验，再开始任何落盘——
    //   杜绝「知识点已删、表更新中途失败」的半应用状态
    let mut table_updates: Vec<(String, String, usize)> = Vec::new(); // (lang, json, removed)
    for table in core.tables.list() {
        let Some(raw) = core.tables.get_raw(&table.lang) else {
            continue;
        };
        let mut jdm: serde_json::Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let removed = attribution_engine::remove_tagged_rows(&mut jdm, &source_id);
        if removed == 0 {
            continue;
        }
        let json = serde_json::to_string(&jdm).map_err(|e| e.to_string())?;
        attribution_engine::AttributionEngine::validate(&json)
            .map_err(|e| format!("决策表摘除结果校验未通过（{}），中止删除: {e}", table.lang))?;
        table_updates.push((table.lang, json, removed));
    }

    let knowledge = attribution_engine::clean_pack(
        &core.knowledge,
        &source_id,
        keep_orphan_points.unwrap_or(false),
    )?;
    core.knowledge.save_to_dir(&core.knowledge_dir)?;

    let mut rules_removed = 0usize;
    for (lang, json, removed) in table_updates {
        core.tables
            .update(&lang, &json)
            .map_err(|e| e.to_string())?;
        pack_atomic_write(
            &tables_dir_of(&core).join(format!("{}_gec_attribution.json", lang)),
            &json,
        )?;
        rules_removed += removed;
    }

    // 决策数据（aux 文件）：条目级摘除该源条目（entry.source_id 归属；
    // 同语言其他源条目保留）；摘空后删除文件
    let dir = tables_dir_of(&core);
    let mut aux_removed = 0usize;
    for name in ["points", "point_index"] {
        let path = dir.join(format!("{}_{}.json", src.lang, name));
        if !path.exists() {
            continue;
        }
        let content = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let aux: serde_json::Value = serde_json::from_str(&content).map_err(|e| e.to_string())?;
        let (_, rest) = attribution_engine::aux_split_source(&aux, &source_id);
        if attribution_engine::aux_is_empty(&rest) {
            std::fs::remove_file(&path).map_err(|e| format!("删除失败 {}: {e}", path.display()))?;
        } else {
            let json = serde_json::to_string_pretty(&rest).map_err(|e| e.to_string())?;
            pack_atomic_write(&path, &json)?;
        }
        aux_removed += 1;
    }

    let staging_removed = core.ingest.discard(&source_id).is_ok();
    let staged_file = core.ingest_dir.join(format!("{source_id}.json"));
    if staged_file.exists() {
        let _ = std::fs::remove_file(&staged_file);
    }
    Ok(serde_json::json!({
        "knowledge": knowledge,
        "rules_removed": rules_removed,
        "aux_removed": aux_removed,
        "staging_removed": staging_removed,
    }))
}

/// LLM 个性化讲解（按 lang + cefr 选择提示词模板）
///
/// 提供 `session_id` 时注入会话画像（母语 + 历史错误分布）。
/// 未配置 `LLM_API_KEY` 时返回错误，前端应回退到决策表静态补救建议。
/// 桌面端无 SSE 通道：内部走流式请求并收集全文后一次性返回
/// （非流式请求在生成完成前收不到任何数据，思考模型的长生成会被读超时中断）。
/// 返回 JSON：`{ explanation, exercises, model, template, template_version }`
#[tauri::command]
pub async fn explain(
    core: tauri::State<'_, Arc<Core>>,
    lang: String,
    error_type: String,
    attribution: Option<String>,
    cefr: Option<String>,
    session_id: Option<String>,
    session_error_count: Option<u32>,
    target_language: Option<String>,
) -> Result<String, String> {
    let cefr = cefr.unwrap_or_else(|| "A1".to_string());
    let attribution = attribution.unwrap_or_else(|| "unknown".to_string());
    // 会话画像：母语 + 历史错误类型分布（有 session_id 时以档案为准）
    let (native_language, distribution, history_from_store) = match session_id.as_deref() {
        Some(id) => {
            let record = core.sessions.get_or_create(id, &lang);
            let distribution = if record.error_type_counts.is_empty() {
                None
            } else {
                let mut items: Vec<(String, u32)> = record
                    .error_type_counts
                    .iter()
                    .map(|(k, v)| (k.clone(), *v))
                    .collect();
                items.sort_by(|a, b| b.1.cmp(&a.1));
                Some(
                    items
                        .iter()
                        .map(|(k, v)| format!("{k}: {v}"))
                        .collect::<Vec<_>>()
                        .join(", "),
                )
            };
            let native = record.meta.lang.clone();
            (Some(native), distribution, record.total_errors())
        }
        None => (None, None, 0),
    };
    let history = session_error_count
        .or(Some(history_from_store))
        .unwrap_or(0);

    let llm = core.llm.read().unwrap().clone();
    let (_template, mut stream) = llm
        .stream_explain(
            &lang,
            &error_type,
            &attribution,
            &cefr,
            history,
            native_language.as_deref(),
            distribution.as_deref(),
            target_language.as_deref(),
        )
        .await
        .map_err(|e| e.to_string())?;
    let mut full = String::new();
    use futures_util::StreamExt;
    while let Some(item) = stream.next().await {
        match item {
            Ok(Some(token)) => full.push_str(&token),
            Ok(None) => break,
            Err(e) => return Err(e.to_string()),
        }
    }
    let (explanation, exercises) = match full.split_once("【练习】") {
        Some((ex, exx)) => (ex.trim().to_string(), exx.trim().to_string()),
        None => (full, String::new()),
    };
    let template_key = llm
        .templates
        .for_lang_cefr(&lang, &cefr)
        .map(|t| format!("{}_{}", t.lang, t.cefr))
        .or_else(|| Some(format!("{lang}_generic")))
        .unwrap_or_default();
    let template_version = llm
        .templates
        .get(&template_key)
        .map(|t| t.version.clone())
        .unwrap_or_default();
    Ok(serde_json::json!({
        "explanation": explanation,
        "exercises": exercises,
        "model": llm.config.model,
        "template": template_key,
        "template_version": template_version,
    })
    .to_string())
}

/// 记录会话犯错历史
#[tauri::command]
pub fn record_error(
    core: tauri::State<'_, Arc<Core>>,
    session_id: String,
    lang: String,
    error_type: String,
) -> SessionRecord {
    let now = chrono::Utc::now().to_rfc3339();
    core.sessions
        .record_error(&session_id, &lang, &error_type, &now)
}

/// 列出支持的语言及其真实引擎元信息（NLP 服务 + 引擎类型）
///
/// 前端语言切换器据此展示，避免与后端实际能力不一致
/// （如英文主路径是 HanLP 英文分词 + POS，而非 LanguageTool）
#[tauri::command]
pub fn languages(core: tauri::State<'_, Arc<Core>>) -> Vec<LanguageInfo> {
    let router = core.router.read().unwrap().clone();
    router
        .supported_languages()
        .into_iter()
        .map(|lang| {
            let engine = router
                .get(&lang)
                .map(|e| e.kind().as_str().to_string())
                .unwrap_or_else(|| "rule".to_string());
            LanguageInfo {
                lang: lang.clone(),
                name: match lang.as_str() {
                    "zh" => "中文",
                    "en" => "英文",
                    "ja" => "日文",
                    other => other,
                }
                .to_string(),
                nlp_service: match lang.as_str() {
                    "zh" => "HanLP v2（分词 + POS + NER）",
                    "en" => "HanLP 英文（分词 + POS）",
                    "ja" => "Sudachi（分词 + 词性）",
                    _ => "本地规则",
                }
                .to_string(),
                engine,
            }
        })
        .collect()
}

/// 列出指定语言的规则（SQLite 词表存储；支持分类过滤 / 搜索 / 分页，
/// 与 api-server 的 GET /gec/rules 同构；`enabled` 为运行时开关状态）
#[tauri::command]
pub fn list_rules(
    core: tauri::State<'_, Arc<Core>>,
    lang: String,
    category: Option<String>,
    q: Option<String>,
    page: Option<usize>,
    page_size: Option<usize>,
) -> Result<RulesResponse, String> {
    let list = core
        .lexicon_store
        .list_rules(
            &lang,
            category.as_deref(),
            q.as_deref(),
            page.unwrap_or(1),
            page_size.unwrap_or(50),
        )
        .map_err(|e| e.to_string())?;
    let cats = core
        .lexicon_store
        .list_categories(&lang)
        .map_err(|e| e.to_string())?;
    Ok(RulesResponse {
        lang: list.lang,
        total: list.total,
        page: list.page,
        page_size: list.page_size,
        categories: cats
            .into_iter()
            .map(|(id, n)| CategoryInfo {
                id: id.clone(),
                name: id,
                rule_count: n as usize,
            })
            .collect(),
        rules: list
            .rules
            .into_iter()
            .map(|row| RuleInfo {
                id: row.def.id,
                category: row.def.category,
                priority: row.def.priority,
                enabled: row.def.enabled,
                description: row.def.description,
                example_count: row.example_count,
                source: row.def.source,
            })
            .collect(),
    })
}

/// 切换规则开关（规则 id 全局唯一，命名约定带语言前缀，无需传 lang；
/// 开关持久化到 SQLite，重启不丢）
#[tauri::command]
pub fn toggle_rule(
    core: tauri::State<'_, Arc<Core>>,
    id: String,
    enabled: bool,
) -> Result<ToggleResponse, String> {
    let def = core
        .lexicon_store
        .rule_defs_by_id(&id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("规则 {id} 不存在"))?;
    core.lexicon_store
        .toggle_rule(&def.lang, &def.id, enabled)
        .map_err(|e| e.to_string())?;
    Ok(ToggleResponse { id, enabled })
}

/// 批量设置规则开关（每个 id 必须存在于该语言词表，否则报错；
/// 返回该语言全部规则的当前开关状态）
#[tauri::command]
pub fn put_toggles(
    core: tauri::State<'_, Arc<Core>>,
    lang: String,
    toggles: HashMap<String, bool>,
) -> Result<serde_json::Value, String> {
    if toggles.is_empty() {
        return Err("toggles 为空".into());
    }
    let defs = core
        .lexicon_store
        .rule_defs(&lang)
        .map_err(|e| e.to_string())?;
    let known: std::collections::HashSet<&str> = defs.iter().map(|d| d.id.as_str()).collect();
    for id in toggles.keys() {
        if !known.contains(id.as_str()) {
            return Err(format!("规则 {id} 不存在"));
        }
    }
    for (id, enabled) in &toggles {
        core.lexicon_store
            .toggle_rule(&lang, id, *enabled)
            .map_err(|e| e.to_string())?;
    }
    // 更新后重新读取全部规则，响应反映最新开关状态
    let defs = core
        .lexicon_store
        .rule_defs(&lang)
        .map_err(|e| e.to_string())?;
    let toggles: HashMap<String, bool> = defs.into_iter().map(|d| (d.id, d.enabled)).collect();
    Ok(serde_json::json!({
        "lang": lang,
        "count": toggles.len(),
        "toggles": toggles,
    }))
}

/// 列出某规则的示例对（分页；与 api-server 的 GET /gec/rules/:id/examples 同构）
#[tauri::command]
pub fn list_rule_examples(
    core: tauri::State<'_, Arc<Core>>,
    lang: String,
    id: String,
    page: Option<usize>,
    page_size: Option<usize>,
) -> Result<lexicon_store::ExampleList, String> {
    core.lexicon_store
        .list_rule_examples(&lang, &id, page.unwrap_or(1), page_size.unwrap_or(50))
        .map_err(|e| e.to_string())
}

/// 添加自定义词（白名单）
#[tauri::command]
pub fn add_custom_word(
    core: tauri::State<'_, Arc<Core>>,
    lang: String,
    word: String,
    reason: Option<String>,
) -> Result<CustomWord, String> {
    core.custom_words
        .add(&lang, &word, &reason.unwrap_or_default())
        .map_err(|e| e.to_string())
}

/// 列出指定语言的自定义词
#[tauri::command]
pub fn list_custom_words(
    core: tauri::State<'_, Arc<Core>>,
    lang: String,
) -> Result<Vec<CustomWord>, String> {
    core.custom_words
        .list(&lang)
        .map_err(|e| format!("查询自定义词失败: {e}"))
}

/// 删除自定义词
#[tauri::command]
pub fn remove_custom_word(core: tauri::State<'_, Arc<Core>>, id: i64) -> Result<bool, String> {
    core.custom_words
        .remove(id)
        .map_err(|e| format!("删除自定义词失败: {e}"))
}

/// LLM 兜底结果反馈（采纳/拒绝）
///
/// "adopt" 尝试构建规则：
/// - 构建成功（store 入库 + 写回词表文件 + 重建检测器）→ 返回规则信息（默认停用），
///   用户在检测页快捷操作「启用 / 停用」（开关实时生效，无需再重建）；
/// - 构建失败（入库错误 / 该语言无词表文件 → 规则永远无法被检测器加载）
///   → 返回知识候选（不落库），UI 提示「沉淀为知识」并测试召回效果，
///   用户选择「保留」（写入知识存储，②.5 召回）或「放弃」。
#[tauri::command]
pub async fn llm_feedback(
    core: tauri::State<'_, Arc<Core>>,
    lang: String,
    error: shared_model::Error,
    action: String,
) -> Result<serde_json::Value, String> {
    llm_feedback_inner(&core, &lang, &error, &action).await
}

/// LLM 兜底结果反馈核心逻辑（`llm_feedback` 命令与 `error_feedback` 共用）
async fn llm_feedback_inner(
    core: &Core,
    lang: &str,
    error: &shared_model::Error,
    action: &str,
) -> Result<serde_json::Value, String> {
    match action {
        "adopt" => {
            // anomaly（AI 污染）无 correction：走模式蒸馏分支（二期飞轮）
            if error.error_type == error_types::ANOMALY {
                return anomaly_adopt_cmd(core, lang, error).await;
            }
            let rule_id = error
                .rule_id
                .clone()
                .unwrap_or_else(|| format!("LLM_{}", error.error_type.to_uppercase()));
            let wrong = &error.original;
            let right = error
                .correction
                .as_deref()
                .ok_or_else(|| "采纳操作需要 correction 字段（修正文本）".to_string())?;

            let settings = core.settings.read().unwrap().clone();
            let lexicons_dir = PathBuf::from(&settings.lexicons_dir);

            // ⓪ 目标规则在词表文件中但不在 store（建库后才加入文件，播种只在库空时执行）：
            //    先按文件回填定义（保留文件 enabled / category / priority / description），
            //    防止 `upsert_example` 自动创建「默认停用」影子定义、
            //    随后 `sync_rules_to_file` 把停用状态写回文件关掉原本启用的规则
            if let Err(e) =
                core.lexicon_store
                    .backfill_rule_from_file(&lexicons_dir, &lang, &rule_id)
            {
                tracing::warn!(
                    error = %e,
                    rule_id = %rule_id,
                    "从词表文件回填规则定义失败（继续默认流程）"
                );
            }

            // ① 尝试构建规则：store 入库（真正的新规则默认停用，可在规则面板启用）
            let (rule_ok, created) = match core.lexicon_store.upsert_example(
                &lang,
                &rule_id,
                wrong,
                right,
                "llm_distill",
            ) {
                Ok(created) => (true, created),
                Err(e) => {
                    tracing::warn!(error = %e, rule_id = %rule_id, "规则入库失败");
                    (false, false)
                }
            };

            // ② 规则能否生效的前提：写回词表文件（检测器只在构造时加载词表）
            let file_exists = lexicons_dir
                .join(format!("{lang}_gec_lexicon.json"))
                .exists();

            if rule_ok && file_exists {
                match core.lexicon_store.sync_rules_to_file(&lexicons_dir, &lang) {
                    Ok(outcome) => {
                        // ③ 重建检测器：新规则进入检测词表（新规则默认停用；
                        //    文件既有规则保持原启用状态；启用后即时生效）
                        core.rebuild_router(&settings);
                        tracing::info!(
                            rule = %rule_id,
                            created = created,
                            updated = ?outcome.updated_rule,
                            "采纳：规则已写回词表文件，检测器已重建"
                        );
                        let enabled = core
                            .lexicon_store
                            .get_rule(&lang, &rule_id)
                            .ok()
                            .flatten()
                            .map(|d| d.enabled)
                            .unwrap_or(false);
                        let message = if created {
                            if enabled {
                                format!(
                                    "已采纳：「{wrong}」→「{right}」补入规则 {rule_id}（规则保持启用，后续同类错误将继续报告）"
                                )
                            } else {
                                format!(
                                    "已采纳：「{wrong}」→「{right}」编译为规则 {rule_id}（默认停用，可在本页或规则面板启用）"
                                )
                            }
                        } else if enabled {
                            format!(
                                "该示例已存在于规则 {rule_id}（规则保持启用，可在本页或规则面板切换开关）"
                            )
                        } else {
                            format!(
                                "该示例已存在于规则 {rule_id}（默认停用，可在本页或规则面板启用）"
                            )
                        };
                        return Ok(serde_json::json!({
                            "action": "adopt",
                            "via": "rule",
                            "message": message,
                            "rule": {
                                "id": rule_id,
                                "wrong": wrong,
                                "right": right,
                                "enabled": enabled,
                                "created": created,
                            },
                        }));
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "规则写回词表文件失败，转知识分支");
                    }
                }
            }

            // ④ 知识分支：规则失败（入库错误 / 无词表文件 / 写回失败）
            //    返回知识候选（不落库）——由 UI 展示内容 + 测试召回效果后，
            //    用户选择「保留」（写入知识存储，②.5 召回）或「放弃」
            let reason: String = if !rule_ok {
                "规则入库失败".to_string()
            } else if !file_exists {
                format!("该语言无词表文件（{lang}），规则无法被检测器加载")
            } else {
                "规则写回词表文件失败".to_string()
            };
            Ok(serde_json::json!({
                "action": "adopt",
                "via": "knowledge",
                "message": format!("规则构建失败（{reason}），建议沉淀为知识"),
                "knowledge": {
                    "text": wrong,
                    "error_type": error.error_type,
                    "correction": right,
                },
            }))
        }
        "reject" => {
            let word = error.original.clone();
            let cw = core
                .custom_words
                .add(&lang, &word, "LLM 兜底结果被用户拒绝")
                .map_err(|e| format!("白名单写入失败: {e}"))?;
            Ok(serde_json::json!({
                "action": "reject",
                "message": format!("已拒绝：「{word}」加入白名单"),
                "word_id": cw.id,
            }))
        }
        other => Err(format!("无效操作: {other}（应为 \"adopt\" 或 \"reject\"）")),
    }
}

/// anomaly（AI 生成文本污染）采纳分支：模式蒸馏（二期自学习飞轮；
/// 与 api-server `anomaly_adopt` 同逻辑，Tauri 命令返回 JSON）
async fn anomaly_adopt_cmd(
    core: &Core,
    lang: &str,
    error: &Error,
) -> Result<serde_json::Value, String> {
    let wrong = &error.original;
    let settings = core.settings.read().unwrap().clone();
    let lexicons_dir = PathBuf::from(&settings.lexicons_dir);
    let lex_file = lexicons_dir.join(format!("{lang}_gec_lexicon.json"));

    // ① 目标规则 + 现有模式（词表文件是模式数据的权威来源）
    let (rule_id, existing) = match std::fs::read_to_string(&lex_file)
        .ok()
        .and_then(|c| serde_json::from_str::<gec_zh::rules::Lexicon>(&c).ok())
    {
        Some(lex) => (
            anomaly_rule_id(&lex)
                .unwrap_or("ZH_ANOMALY_ASSISTANT_REPLY")
                .to_string(),
            lex.rules
                .iter()
                .filter(|r| r.category == "anomaly")
                .flat_map(|r| r.patterns.iter().cloned())
                .collect::<Vec<_>>(),
        ),
        None => {
            tracing::warn!(
                path = %lex_file.display(),
                "词表文件缺失/解析失败，蒸馏使用空模式集"
            );
            ("ZH_ANOMALY_ASSISTANT_REPLY".to_string(), Vec::new())
        }
    };

    // ①.5 目标规则在文件中但不在 store（建库后才加入文件，播种只在库空时执行）：
    //      先按文件回填定义（保留文件 enabled / category / priority / description），
    //      防止后续 upsert 自动创建「默认停用」影子定义、
    //      随后 sync_rules_to_file 把停用状态写回文件关掉原本启用的规则
    if let Err(e) = core
        .lexicon_store
        .backfill_rule_from_file(&lexicons_dir, lang, &rule_id)
    {
        tracing::warn!(
            error = %e,
            rule = %rule_id,
            "从词表文件回填规则定义失败（继续默认流程）"
        );
    }

    // ② 金集（缺失 → 降级知识分支）
    let gold_path = GoldSet::default_path();
    let gold = GoldSet::load(&gold_path).map_err(|e| {
        tracing::warn!(path = %gold_path.display(), error = %e, "anomaly 金集加载失败");
        format!("金集缺失，无法评估蒸馏模式（{e}）")
    })?;

    // ③ LLM 教师蒸馏（未配置 / 缺模板 / 失败 → 降级）
    //    guard 不可跨 await（非 Send）——先在锁内克隆 LLM 引擎与模板，再立即释放
    let (llm, template) = {
        let service = core.llm.read().unwrap();
        let t = service
            .templates
            .for_lang_cefr(lang, "anomaly_distill")
            .cloned();
        let llm = if service.config.enabled() {
            t.as_ref().map(|t| {
                LlmGecEngine::new(
                    lang,
                    LlmDetectConfig::from_parts(
                        service.config.base_url.clone(),
                        service.config.api_key.clone(),
                        service.config.model.clone(),
                        service.config.timeout_secs,
                        t.system.clone(),
                        t.user.clone(),
                    ),
                )
            })
        } else {
            None
        };
        (llm, t)
    };
    let (pattern, gate) = match (llm, template) {
        (Some(llm), Some(t)) => {
            match distill_pattern(&llm, &t.system, &t.user, wrong, &existing, &gold).await {
                Ok(p) => {
                    let g = evaluate_candidate(&p, &gold, wrong);
                    (p, g)
                }
                Err(e) => {
                    return Ok(anomaly_knowledge_fallback_cmd(
                        wrong,
                        &error.error_type,
                        &rule_id,
                        None,
                        &format!("蒸馏失败：{e}"),
                    ));
                }
            }
        }
        (None, Some(_)) => {
            return Ok(anomaly_knowledge_fallback_cmd(
                wrong,
                &error.error_type,
                &rule_id,
                None,
                "LLM 服务未配置（请在设置界面配置 LLM 参数）",
            ));
        }
        (_, None) => {
            return Ok(anomaly_knowledge_fallback_cmd(
                wrong,
                &error.error_type,
                &rule_id,
                None,
                "蒸馏模板缺失（prompts/{lang}_anomaly_distill_prompt.json）",
            ));
        }
    };

    // ④ 门槛：通过 → 写回词表 + 重建检测器；不过 → 降级知识分支
    if gate.accepted {
        let created = core
            .lexicon_store
            .upsert_example(lang, &rule_id, wrong, "", "llm_distill")
            .map_err(|e| e.to_string())?;
        core.lexicon_store
            .sync_rules_to_file(&lexicons_dir, lang)
            .map_err(|e| format!("词表文件写回失败: {e}"))?;
        append_pattern_to_lexicon(&lexicons_dir, lang, &rule_id, &pattern)
            .map_err(|e| format!("模式写回词表失败: {e}"))?;
        core.rebuild_router(&settings);
        tracing::info!(
            lang,
            rule = %rule_id,
            pattern = %pattern.pattern,
            "anomaly 蒸馏模式已沉淀：写回词表文件，检测器已重建"
        );
        return Ok(serde_json::json!({
            "action": "adopt",
            "via": "anomaly",
            "message": format!(
                "已确认污染：蒸馏新模式「{}」并沉淀到规则 {rule_id}（金集零误报），后续检测将自动命中",
                pattern.note
            ),
            "anomaly": {
                "pattern": pattern,
                "accepted": true,
                "gate": gate,
                "rule_id": rule_id,
                "created": created,
            },
        }));
    }

    // ⑤ 门槛不过：实例仍入库留痕，降级知识分支
    let _ = core
        .lexicon_store
        .upsert_example(lang, &rule_id, wrong, "", "llm_distill");
    let _ = core.lexicon_store.sync_rules_to_file(&lexicons_dir, lang);
    tracing::info!(
        lang,
        rule = %rule_id,
        reason = %gate.reason,
        "anomaly 蒸馏模式未过金集门槛，降级知识分支"
    );
    Ok(anomaly_knowledge_fallback_cmd(
        wrong,
        &error.error_type,
        &rule_id,
        Some((pattern, gate)),
        "蒸馏模式未过金集门槛",
    ))
}

/// anomaly 降级知识分支（Tauri 命令 JSON 形态）
fn anomaly_knowledge_fallback_cmd(
    wrong: &str,
    error_type: &str,
    rule_id: &str,
    distill: Option<(
        gec_zh::rules::AnomalyPattern,
        gec_zh::anomaly_distill::GateReport,
    )>,
    reason: &str,
) -> serde_json::Value {
    let (pattern, gate) = match distill {
        Some((p, g)) => (Some(p), Some(g)),
        None => (None, None),
    };
    serde_json::json!({
        "action": "adopt",
        "via": "anomaly",
        "message": format!(
            "模式蒸馏未沉淀（{reason}），已降级为知识候选；可选择「保留」沉淀到向量知识库"
        ),
        "knowledge": {
            "text": wrong,
            "error_type": error_type,
            "correction": "",
        },
        "anomaly": {
            "pattern": pattern,
            "gate": gate,
            "accepted": false,
            "rule_id": rule_id,
            "reason": reason,
        },
    })
}

/// 知识召回测试候选（「沉淀为知识」流程：规则构建失败后 UI 展示的内容）
///
/// 召回测试只消费 `text`（生成嵌入向量）；`error_type` / `correction`
/// 随请求透传以保持与知识条目同构（用户「保留」时由 UI 另行提交入库）。
#[derive(Debug, serde::Deserialize)]
#[allow(dead_code)]
pub struct RecallCandidate {
    /// 含错误的原始文本片段
    pub text: String,
    /// 错误类型（如 article / tense / spelling）
    pub error_type: String,
    /// 修正文本
    pub correction: String,
}

/// 知识召回测试：给定全文与候选知识，计算余弦相似度并判定 ②.5 知识召回层
/// 是否会命中（阈值与检测流水线一致：0.7）
///
/// 用于「规则构建失败 → 沉淀为知识」流程：UI 展示测试结果后，
/// 用户选择「保留」（写入知识存储）或「放弃」。
/// 未配置嵌入服务 / 嵌入调用失败时 `would_hit = null`（无法测试）。
#[tauri::command]
pub async fn test_knowledge_recall(
    core: tauri::State<'_, Arc<Core>>,
    _lang: String,
    text: String,
    candidate: RecallCandidate,
) -> Result<serde_json::Value, String> {
    const THRESHOLD: f64 = 0.7;
    // guard 不可跨 await（非 Send）——先克隆 Arc 再立即释放
    let engine = {
        let guard = core.embedding_engine.read().unwrap();
        guard.as_ref().cloned()
    };
    let engine = match engine {
        Some(engine) => engine,
        None => {
            return Ok(serde_json::json!({
                "similarity": null,
                "threshold": THRESHOLD,
                "would_hit": null,
                "note": "未配置嵌入服务，无法测试召回效果",
            }));
        }
    };
    let (query_emb, cand_emb) = match (
        engine.embed(&text).await,
        engine.embed(&candidate.text).await,
    ) {
        (Ok(a), Ok(b)) => (a, b),
        (a, b) => {
            let e = a
                .err()
                .or(b.err())
                .unwrap_or_else(|| "嵌入失败".to_string());
            tracing::warn!(error = %e, "召回测试：嵌入调用失败");
            return Ok(serde_json::json!({
                "similarity": null,
                "threshold": THRESHOLD,
                "would_hit": null,
                "note": format!("嵌入服务调用失败（{e}），无法测试召回效果"),
            }));
        }
    };
    let similarity = gec_core::cosine_similarity(&query_emb, &cand_emb);
    let would_hit = similarity >= THRESHOLD;
    Ok(serde_json::json!({
        "similarity": similarity,
        "threshold": THRESHOLD,
        "would_hit": would_hit,
        "note": if would_hit {
            "预期命中：重检测该文本时知识层将直接返回修正（无需 LLM 兜底）"
        } else {
            "预期未命中：相似度低于阈值；保留后对其他相似文本仍可能生效"
        },
    }))
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

/// LLM 参数视图（get_settings 响应；与 Web 端 `LlmSettingsView` 同构）
#[derive(Debug, Clone, serde::Serialize)]
pub struct LlmSettingsView {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub timeout_secs: u64,
    /// 提示词模板目录（等价环境变量 `LLM_PROMPTS_DIR`）
    pub prompts_dir: String,
    /// 是否已启用（API Key 非空）
    pub enabled: bool,
}

/// LLM 请求模式（固定默认，与 Web 端同构：前端用流式，外部 API 用非流式）
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct LlmModes {
    pub frontend: &'static str,
    pub external_api: &'static str,
}

impl Default for LlmModes {
    fn default() -> Self {
        Self {
            frontend: "stream",
            external_api: "non_stream",
        }
    }
}

/// 决策模型（vegaml）层参数视图（get_settings 响应；与 Web 端 `DecisionSettingsView` 同构）
#[derive(Debug, Clone, serde::Serialize)]
pub struct DecisionSettingsView {
    pub endpoint: String,
    pub model: String,
    pub timeout_secs: u64,
    pub abstain_threshold: f64,
    pub enabled: bool,
}

/// 决策模型（vegaml）层参数更新（字段均可缺省 = 保持不变）
#[derive(Debug, Clone, serde::Deserialize)]
pub struct UpdateDecisionRequest {
    pub endpoint: Option<String>,
    pub model: Option<String>,
    pub timeout_secs: Option<u64>,
    pub abstain_threshold: Option<f64>,
    pub enabled: Option<bool>,
}

/// L3 特征模型层参数视图（get_settings 响应；与 Web 端 `L3SettingsView` 同构）
#[derive(Debug, Clone, serde::Serialize)]
pub struct L3SettingsView {
    /// 特征模型服务地址（等价环境变量 `GEC_L3_ENDPOINT`）
    pub endpoint: String,
    /// 模型名（微调后为 checkpoint 名）
    pub model: String,
    /// 请求超时（秒）
    pub timeout_secs: u64,
    /// 弃权阈值（概率灰区上界；灰区 / 服务不可用时 LLM 长尾兜底）
    pub abstain_threshold: f64,
    /// 特征模型层开关（`true` 时 level 3 先跑特征模型，LLM 收缩到纯长尾）
    pub enabled: bool,
}

/// L3 特征模型层参数更新（字段均可缺省 = 保持不变）
#[derive(Debug, Clone, serde::Deserialize)]
pub struct UpdateL3Request {
    pub endpoint: Option<String>,
    pub model: Option<String>,
    pub timeout_secs: Option<u64>,
    pub abstain_threshold: Option<f64>,
    pub enabled: Option<bool>,
}

/// 嵌入引擎（向量召回）参数视图（get_settings 响应；与 Web 端 `EmbeddingSettingsView` 同构）
#[derive(Debug, Clone, serde::Serialize)]
pub struct EmbeddingSettingsView {
    /// 嵌入服务地址（OpenAI 兼容 `/v1/embeddings`；空 = 未配置，向量召回静默禁用）
    pub endpoint: String,
    /// API Key（Bearer token；本地服务如 Ollama 可留空）
    pub api_key: String,
    /// 嵌入模型名
    pub model: String,
    /// 请求超时（秒）
    pub timeout_secs: u64,
}

/// 嵌入引擎参数更新（字段均可缺省 = 保持不变；endpoint 允许空 = 未配置）
#[derive(Debug, Clone, serde::Deserialize)]
pub struct UpdateEmbeddingRequest {
    pub endpoint: Option<String>,
    pub api_key: Option<String>,
    pub model: Option<String>,
    pub timeout_secs: Option<u64>,
}

/// 纠错模型预设视图（GET 响应；设置界面「纠错模型」选择器列表）
#[derive(Debug, Clone, serde::Serialize)]
pub struct ModelView {
    pub id: String,
    pub name: String,
    /// 输出格式（`json` / `plain`）
    pub output_format: String,
    pub enabled: bool,
}

/// get_settings / update_settings 响应（与 Web 端 `GET /gec/settings` 同构）
#[derive(Debug, Clone, serde::Serialize)]
pub struct SettingsResponse {
    pub hanlp_endpoint: String,
    pub llm: LlmSettingsView,
    pub llm_modes: LlmModes,
    /// 离线模式（等价环境变量 `GEC_OFFLINE=1`；桌面端默认开启）
    pub offline_mode: bool,
    /// 决策表目录（等价环境变量 `GEC_TABLES_DIR`）
    pub tables_dir: String,
    /// 规则词表目录（等价环境变量 `GEC_LEXICONS_DIR`；zh/en/ja 共用）
    pub lexicons_dir: String,
    /// 决策模型（vegaml）层参数
    pub decision: DecisionSettingsView,
    /// L3 特征模型层参数（三期：level 3 先跑特征模型，LLM 收缩到纯长尾）
    pub l3: L3SettingsView,
    /// 嵌入引擎（向量召回）参数（未配置时知识召回静默禁用）
    pub embedding: EmbeddingSettingsView,
    /// 当前选中的纠错模型预设 id（`prompts/models.json` 注册表；缺省 `generic`）
    pub llm_model_id: String,
    /// 知识层总开关（知识向量召回 + 知识库富化 + 数据驱动归点；默认开）
    pub knowledge_enabled: bool,
    /// 载体（图片 / PDF / Word）审校设置（与 api-server 同一份定义：`media_flow::settings`）
    pub media: media_flow::settings::MediaSettings,
    /// 可用纠错模型预设列表（「纠错模型」选择器；仅含 enabled 条目）
    pub models: Vec<ModelView>,
}

fn to_settings_response(s: &AppSettings, registry: &ModelRegistry) -> SettingsResponse {
    SettingsResponse {
        hanlp_endpoint: s.hanlp_endpoint.clone(),
        llm: LlmSettingsView {
            base_url: s.llm.base_url.clone(),
            api_key: s.llm.api_key.clone(),
            model: s.llm.model.clone(),
            timeout_secs: s.llm.timeout_secs,
            prompts_dir: s.llm.prompts_dir.display().to_string(),
            enabled: s.llm.enabled(),
        },
        llm_modes: LlmModes::default(),
        offline_mode: s.offline_mode,
        tables_dir: s.tables_dir.clone(),
        lexicons_dir: s.lexicons_dir.clone(),
        decision: DecisionSettingsView {
            endpoint: s.decision.endpoint.clone(),
            model: s.decision.model.clone(),
            timeout_secs: s.decision.timeout_secs,
            abstain_threshold: s.decision.abstain_threshold,
            enabled: s.decision.enabled(),
        },
        l3: L3SettingsView {
            endpoint: s.l3.endpoint.clone(),
            model: s.l3.model.clone(),
            timeout_secs: s.l3.timeout_secs,
            abstain_threshold: s.l3.abstain_threshold,
            enabled: s.l3.enabled(),
        },
        embedding: EmbeddingSettingsView {
            endpoint: s.embedding.endpoint.clone(),
            api_key: s.embedding.api_key.clone(),
            model: s.embedding.model.clone(),
            timeout_secs: s.embedding.timeout_secs,
        },
        llm_model_id: s.llm_model_id.clone(),
        knowledge_enabled: s.knowledge_enabled,
        media: s.media.clone(),
        models: registry
            .enabled()
            .iter()
            .map(|m| ModelView {
                id: m.id.clone(),
                name: m.name.clone(),
                output_format: m.output_format.clone(),
                enabled: m.enabled,
            })
            .collect(),
    }
}

/// update_settings 请求体（部分更新：仅提供要修改的字段；与 Web 端 `PUT /gec/settings` 同构）
#[derive(Debug, Clone, serde::Deserialize)]
pub struct UpdateSettingsRequest {
    /// HanLP 服务地址
    pub hanlp_endpoint: Option<String>,
    /// 当前选中的纠错模型预设 id（`prompts/models.json` 注册表；缺省 `generic`）
    pub llm_model_id: Option<String>,
    /// LLM 参数
    pub llm: Option<UpdateLlmRequest>,
    /// 离线模式开关
    pub offline_mode: Option<bool>,
    /// 决策表目录
    pub tables_dir: Option<String>,
    /// 规则词表目录
    pub lexicons_dir: Option<String>,
    /// 决策模型（vegaml）层参数
    pub decision: Option<UpdateDecisionRequest>,
    /// L3 特征模型层参数
    pub l3: Option<UpdateL3Request>,
    /// 嵌入引擎（向量召回）参数
    pub embedding: Option<UpdateEmbeddingRequest>,
    /// 知识层总开关（测评消融臂 `knowledge: false` 用；默认开）
    pub knowledge_enabled: Option<bool>,
    /// 载体（图片 / PDF / Word）审校设置（部分更新；与 api-server 同构）
    pub media: Option<UpdateMediaRequest>,
}

/// 载体审校设置更新（字段均可缺省 = 保持不变）
///
/// 与 api-server 的 `UpdateMediaRequest` 同构：设置面板只暴露这几项，
/// CLI 路径 / 超时 / 压缩参数留在设置文件与环境变量里
#[derive(Debug, Clone, serde::Deserialize)]
pub struct UpdateMediaRequest {
    pub enabled: Option<bool>,
    pub apply_mode: Option<String>,
    pub workdir: Option<String>,
    pub use_structure_layer: Option<bool>,
    pub use_vision: Option<bool>,
    pub use_ocr: Option<bool>,
    pub vision_model: Option<String>,
    pub level: Option<u8>,
    pub force_llm: Option<bool>,
    pub min_confidence: Option<f64>,
}

/// LLM 参数更新（字段均可缺省 = 保持不变）
#[derive(Debug, Clone, serde::Deserialize)]
pub struct UpdateLlmRequest {
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub model: Option<String>,
    pub timeout_secs: Option<u64>,
    pub prompts_dir: Option<String>,
}

/// update_settings — 更新设置（即时生效 + 持久化到设置文件，重启后仍生效）
#[tauri::command]
pub fn update_settings(
    core: tauri::State<'_, Arc<Core>>,
    body: UpdateSettingsRequest,
) -> Result<SettingsResponse, String> {
    let mut next = core.settings.read().unwrap().clone();
    if let Some(endpoint) = body.hanlp_endpoint.clone() {
        if endpoint.trim().is_empty() {
            return Err("hanlp_endpoint 不能为空".into());
        }
        next.hanlp_endpoint = endpoint;
    }
    if let Some(model_id) = body.llm_model_id.clone() {
        if model_id.trim().is_empty() {
            return Err("llm_model_id 不能为空".into());
        }
        next.llm_model_id = model_id;
    }
    if let Some(llm) = &body.llm {
        if let Some(v) = &llm.base_url {
            next.llm.base_url = v.clone();
        }
        if let Some(v) = &llm.api_key {
            next.llm.api_key = v.clone();
        }
        if let Some(v) = &llm.model {
            next.llm.model = v.clone();
        }
        if let Some(v) = llm.timeout_secs {
            next.llm.timeout_secs = v.max(1);
        }
        if let Some(v) = &llm.prompts_dir {
            if v.trim().is_empty() {
                return Err("llm.prompts_dir 不能为空".into());
            }
            next.llm.prompts_dir = PathBuf::from(v);
        }
    }
    if let Some(v) = body.offline_mode {
        next.offline_mode = v;
    }
    if let Some(dir) = body.tables_dir.clone() {
        if dir.trim().is_empty() {
            return Err("tables_dir 不能为空".into());
        }
        next.tables_dir = dir;
    }
    if let Some(dir) = body.lexicons_dir.clone() {
        if dir.trim().is_empty() {
            return Err("lexicons_dir 不能为空".into());
        }
        next.lexicons_dir = dir;
    }
    if let Some(decision) = &body.decision {
        if let Some(v) = &decision.endpoint {
            if v.trim().is_empty() {
                return Err("decision.endpoint 不能为空".into());
            }
            next.decision.endpoint = v.clone();
        }
        if let Some(v) = &decision.model {
            next.decision.model = v.clone();
        }
        if let Some(v) = decision.timeout_secs {
            next.decision.timeout_secs = v.max(1);
        }
        if let Some(v) = decision.abstain_threshold {
            next.decision.abstain_threshold = v.clamp(0.0, 1.0);
        }
        if let Some(v) = decision.enabled {
            next.decision.enabled = v;
        }
    }
    if let Some(l3) = &body.l3 {
        if let Some(v) = &l3.endpoint {
            if v.trim().is_empty() {
                return Err("l3.endpoint 不能为空".into());
            }
            next.l3.endpoint = v.clone();
        }
        if let Some(v) = &l3.model {
            next.l3.model = v.clone();
        }
        if let Some(v) = l3.timeout_secs {
            next.l3.timeout_secs = v.max(1);
        }
        if let Some(v) = l3.abstain_threshold {
            next.l3.abstain_threshold = v.clamp(0.0, 1.0);
        }
        if let Some(v) = l3.enabled {
            next.l3.enabled = v;
        }
    }
    if let Some(embedding) = &body.embedding {
        // 与决策层 / 特征层不同：endpoint 允许空（= 未配置，向量召回静默禁用）
        if let Some(v) = &embedding.endpoint {
            next.embedding.endpoint = v.trim().to_string();
        }
        if let Some(v) = &embedding.api_key {
            next.embedding.api_key = v.clone();
        }
        if let Some(v) = &embedding.model {
            next.embedding.model = v.trim().to_string();
        }
        if let Some(v) = embedding.timeout_secs {
            next.embedding.timeout_secs = v.max(1);
        }
    }
    if let Some(v) = body.knowledge_enabled {
        next.knowledge_enabled = v;
    }
    if let Some(media) = &body.media {
        // 载体设置即时生效（job 每次读当前设置）；但 `workdir` 只影响**新** job 根目录之外
        // 的目录解析——job 存储是在启动时按 `media.workdir` 建的，改目录要重启才换地方
        if let Some(v) = media.enabled {
            next.media.enabled = v;
        }
        if let Some(v) = &media.apply_mode {
            let v = v.trim().to_string();
            if !matches!(v.as_str(), "overlay" | "comment" | "in_place" | "list_only") {
                return Err(format!(
                    "media.apply_mode 无效: {v}（应为 overlay / comment / in_place / list_only）"
                ));
            }
            next.media.apply_mode = v;
        }
        if let Some(v) = &media.workdir {
            next.media.workdir = v.trim().to_string();
        }
        if let Some(v) = media.use_structure_layer {
            next.media.use_structure_layer = v;
        }
        if let Some(v) = media.use_vision {
            next.media.use_vision = v;
        }
        if let Some(v) = media.use_ocr {
            next.media.use_ocr = v;
        }
        if let Some(v) = &media.vision_model {
            next.media.vision_model = v.trim().to_string();
        }
        if let Some(v) = media.level {
            if !(1..=3).contains(&v) {
                return Err("media.level 应为 1-3".to_string());
            }
            next.media.level = v;
        }
        if let Some(v) = media.force_llm {
            next.media.force_llm = v;
        }
        if let Some(v) = media.min_confidence {
            next.media.min_confidence = v.clamp(0.0, 1.0);
        }
    }
    core.apply_settings(next.clone());
    let registry = core.model_registry.read().unwrap();
    Ok(to_settings_response(&next, &registry))
}

/// get_settings — 当前设置（设置界面；设置文件 > 环境变量 > 内置默认）
#[tauri::command]
pub fn get_settings(core: tauri::State<'_, Arc<Core>>) -> SettingsResponse {
    let settings = core.settings.read().unwrap();
    let registry = core.model_registry.read().unwrap();
    to_settings_response(&settings, &registry)
}

/// 设置测试响应（与 Web 端 `POST /gec/settings/test` 响应同构）
#[derive(Debug, Clone, serde::Serialize)]
pub struct SettingsTestResponse {
    pub target: String,
    pub ok: bool,
    pub message: String,
    pub latency_ms: u64,
    /// `target = "all"` 时：全部项目的逐项结果
    #[serde(skip_serializing_if = "Option::is_none")]
    pub results: Option<Vec<SettingsTestResponse>>,
}

fn to_test_response(r: &gec_core::settings_test::TestResult) -> SettingsTestResponse {
    SettingsTestResponse {
        target: r.target.clone(),
        ok: r.ok,
        message: r.message.clone(),
        latency_ms: r.latency_ms,
        results: None,
    }
}

/// test_setting — 测试指定配置项的服务连通性 / 可用性（只读探测，不修改配置）
///
/// `target` = `hanlp` / `llm` / `decision` / `l3` / `embedding` /
/// `tables_dir` / `lexicons_dir` / `all`（并行测试全部项目）
#[tauri::command]
pub async fn test_setting(
    core: tauri::State<'_, Arc<Core>>,
    target: String,
) -> Result<SettingsTestResponse, String> {
    let s = core.settings.read().unwrap().clone();
    let t = target.trim().to_lowercase();
    // (单项/汇总结果, `all` 时的逐项结果)
    let (r, all_results) = match t.as_str() {
        "hanlp" => (
            gec_core::settings_test::test_hanlp(&s.hanlp_endpoint).await,
            None,
        ),
        "llm" => (
            gec_core::settings_test::test_llm(
                &s.llm.base_url,
                &s.llm.api_key,
                &s.llm.model,
                s.llm.timeout_secs,
            )
            .await,
            None,
        ),
        "decision" => (
            gec_core::settings_test::test_decision(&s.decision).await,
            None,
        ),
        "l3" => (gec_core::settings_test::test_l3(&s.l3).await, None),
        "embedding" => (
            gec_core::settings_test::test_embedding(&s.embedding).await,
            None,
        ),
        "tables_dir" => (
            gec_core::settings_test::test_tables_dir(&s.tables_dir),
            None,
        ),
        "lexicons_dir" => (
            gec_core::settings_test::test_lexicons_dir(&s.lexicons_dir),
            None,
        ),
        "all" => {
            let results = gec_core::settings_test::test_all(
                &s.hanlp_endpoint,
                &s.llm.base_url,
                &s.llm.api_key,
                &s.llm.model,
                s.llm.timeout_secs,
                &s.decision,
                &s.l3,
                &s.embedding,
                &s.tables_dir,
                &s.lexicons_dir,
            )
            .await;
            let ok_count = results.iter().filter(|r| r.ok).count();
            let failed: Vec<String> = results
                .iter()
                .filter(|r| !r.ok)
                .map(|r| r.target.clone())
                .collect();
            let message = if failed.is_empty() {
                format!("全部 {} 项测试通过", results.len())
            } else {
                format!(
                    "{} / {} 项通过；未通过：{}",
                    ok_count,
                    results.len(),
                    failed.join("、")
                )
            };
            let latency = results.iter().map(|r| r.latency_ms).max().unwrap_or(0);
            (
                gec_core::settings_test::TestResult {
                    target: "all".to_string(),
                    ok: failed.is_empty(),
                    message,
                    latency_ms: latency,
                },
                Some(results),
            )
        }
        other => {
            return Err(format!(
                "未知测试目标: {other}（支持 hanlp / llm / decision / l3 / embedding / tables_dir / lexicons_dir / all）"
            ));
        }
    };
    let mut resp = to_test_response(&r);
    if let Some(results) = all_results {
        resp.results = Some(results.iter().map(to_test_response).collect());
    }
    Ok(resp)
}

// ─────────────────────────────────────────────────────────────────────────────
// 学习知识管理（向量召回）
// ─────────────────────────────────────────────────────────────────────────────

/// list_learned_knowledge — 列出指定语言的学习知识条目
#[tauri::command]
pub fn list_learned_knowledge(
    core: tauri::State<'_, Arc<Core>>,
    lang: String,
) -> Result<Vec<shared_model::LearnedKnowledge>, String> {
    core.learned_knowledge
        .list(&lang)
        .map_err(|e| e.to_string())
}

/// add_learned_knowledge — 添加学习知识条目（自动生成嵌入向量）
#[tauri::command]
pub async fn add_learned_knowledge(
    core: tauri::State<'_, Arc<Core>>,
    lang: String,
    text: String,
    error_type: String,
    correction: String,
    source: String,
) -> Result<i64, String> {
    let embedding = {
        // guard 不可跨 await（非 Send）——先克隆 Arc 再立即释放
        let guard = core.embedding_engine.read().unwrap();
        guard.as_ref().cloned()
    };
    let embedding = match embedding.as_deref() {
        Some(engine) => engine.embed(&text).await.map_err(|e| e.to_string())?,
        None => vec![],
    };
    core.learned_knowledge
        .insert(&lang, &text, &error_type, &correction, &embedding, &source)
        .map_err(|e| e.to_string())
}

/// remove_learned_knowledge — 删除指定知识条目
#[tauri::command]
pub fn remove_learned_knowledge(
    core: tauri::State<'_, Arc<Core>>,
    id: i64,
) -> Result<bool, String> {
    core.learned_knowledge.remove(id).map_err(|e| e.to_string())
}

/// anomaly 自动采纳（二期飞轮 L1；与 api-server `auto_adopt_anomalies` 同逻辑）：
/// LLM 兜底检出的污染句，置信度 ≥ 阈值时无需用户确认即沉淀到学习知识库
/// （source `llm_adopt`，②.5 向量召回）。全程非阻断。
const ANOMALY_AUTO_ADOPT_CONF: f64 = 0.9;

async fn auto_adopt_anomalies_cmd(core: &Arc<Core>, lang: &str, errors: &[Error]) {
    let candidates: Vec<&Error> = errors
        .iter()
        .filter(|e| {
            e.error_type == error_types::ANOMALY
                && e.source.as_deref() == Some(shared_model::sources::LLM)
                && e.confidence >= ANOMALY_AUTO_ADOPT_CONF
        })
        .collect();
    if candidates.is_empty() {
        return;
    }
    let existing = match core.learned_knowledge.list(lang) {
        Ok(items) => items,
        Err(e) => {
            tracing::warn!(error = %e, "anomaly 自动采纳：知识库读取失败，跳过");
            return;
        }
    };
    // guard 不可跨 await（非 Send）——先克隆 Arc 再立即释放
    let engine = {
        let guard = core.embedding_engine.read().unwrap();
        guard.as_ref().cloned()
    };
    for e in candidates {
        if existing.iter().any(|k| k.text == e.original) {
            continue;
        }
        let embedding = match engine.as_deref() {
            Some(eng) => eng.embed(&e.original).await.unwrap_or_default(),
            None => Vec::new(),
        };
        match core.learned_knowledge.insert(
            lang,
            &e.original,
            &e.error_type,
            "",
            &embedding,
            "llm_adopt",
        ) {
            Ok(id) if id > 0 => tracing::info!(
                lang,
                entry = id,
                text = %e.original,
                confidence = e.confidence,
                "anomaly 自动采纳：LLM 高置信命中已沉淀到向量知识库（llm_adopt）"
            ),
            Ok(_) => tracing::debug!(
                lang,
                text = %e.original,
                "anomaly 自动采纳：重复条目（唯一约束命中，并发重复写），跳过"
            ),
            Err(err) => tracing::warn!(
                error = %err,
                "anomaly 自动采纳：知识库写入失败（不影响检测结果）"
            ),
        }
    }
}

// ─── L3 特征模型训练飞轮（三期闭环；与 api-server /gec/l3/* 同构） ──────────
//
// - 采集：前端「采纳」LLM 错误（正样本）→ l3_collect；
// - 提醒：l3_status 返回样本数 + 上次重训练时间 + 当前模型 F1（转发
//   feature-model GET /l3/status）+ retrain_ready 建议；
// - 重训练：l3_retrain 转发 feature-model POST /l3-retrain（合并样本 →
//   重训练 → dev 评测 → F1 未显著退化则热切换）。

/// 单语言样本文件上限（防无限膨胀；超出后保留最新 N 条）
const L3_MAX_SAMPLES: usize = 5000;
/// 提醒阈值：上次重训练后样本 ≥ 该值 → retrain_ready
const L3_RETRAIN_READY_POS: usize = 10;
/// 提醒阈值：从未重训练过时，累计样本 ≥ 该值即可
const L3_RETRAIN_READY_FIRST: usize = 30;
/// 距上次重训练至少 7 天才再次提醒（从未训练过不限时间）
const L3_RETRAIN_COOLDOWN: i64 = 7 * 24 * 3600;

/// 追加一条样本到 `l3_data/<lang>/confirmed.jsonl`（按 文本+标签 去重，上限截断）
pub(crate) fn l3_collect_sample(
    dir: &Path,
    lang: &str,
    sample: &serde_json::Value,
) -> Option<usize> {
    let lang_dir = dir.join(lang);
    if let Err(e) = std::fs::create_dir_all(&lang_dir) {
        tracing::warn!(lang, error = %e, "L3 飞轮：创建样本目录失败");
        return None;
    }
    let path = lang_dir.join("confirmed.jsonl");
    let mut lines: Vec<String> = match std::fs::read_to_string(&path) {
        Ok(s) => s
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.to_string())
            .collect(),
        Err(_) => Vec::new(),
    };
    let key = l3_sample_key(sample);
    let dup = lines.iter().any(|l| {
        serde_json::from_str::<serde_json::Value>(l)
            .map(|v| l3_sample_key(&v) == key)
            .unwrap_or(false)
    });
    if dup {
        return None;
    }
    lines.push(serde_json::to_string(sample).unwrap_or_default());
    if lines.len() > L3_MAX_SAMPLES {
        let drop = lines.len() - L3_MAX_SAMPLES;
        lines.drain(0..drop);
    }
    if let Err(e) = std::fs::write(&path, format!("\n{}", lines.join("\n"))) {
        tracing::warn!(lang, error = %e, "L3 飞轮：样本写入失败");
        return None;
    }
    Some(lines.len())
}

/// 样本去重键：`文本|标签`
fn l3_sample_key(v: &serde_json::Value) -> String {
    format!(
        "{}|{}",
        v.get("text").and_then(|v| v.as_str()).unwrap_or(""),
        v.get("label").and_then(|v| v.as_u64()).unwrap_or(1)
    )
}

/// 读取某语言样本总数（文件缺失 = 0）
fn l3_count_samples(dir: &Path, lang: &str) -> usize {
    let path = dir.join(lang).join("confirmed.jsonl");
    match std::fs::read_to_string(&path) {
        Ok(s) => s.lines().filter(|l| !l.trim().is_empty()).count(),
        Err(_) => 0,
    }
}

// ── 采纳 / 放弃沉淀资产（adopted / rejected；与 api-server routes/l3.rs 同构）──

/// 采纳平行语料与负偏好池上限（长期训练资产）
const L3_MAX_ADOPTED: usize = 20000;
/// 句对编辑密度上限：超过视为「用户改写」而非「纠错」，打 weak 标记
const L3_WEAK_EDIT_RATIO: f64 = 0.4;

/// 采纳/放弃沉淀开关：**默认开**（桌面端自用）；显式 `GEC_L3_ADOPT_COLLECT=0` 关闭
fn adopt_collect_enabled() -> bool {
    match std::env::var("GEC_L3_ADOPT_COLLECT") {
        Ok(v) => v != "0" && !v.eq_ignore_ascii_case("false"),
        Err(_) => true,
    }
}

/// 简易切句（。！？；.!? + 换行，≥4 字）
fn l3_split_sentences(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for c in text.chars() {
        cur.push(c);
        if matches!(c, '。' | '！' | '？' | '；' | '.' | '!' | '?' | '\n') {
            let s = cur.trim().to_string();
            if s.chars().count() >= 4 {
                out.push(s);
            }
            cur.clear();
        }
    }
    let tail = cur.trim().to_string();
    if tail.chars().count() >= 4 {
        out.push(tail);
    }
    out
}

/// 字符编辑密度（1 - LCS/max_len）；超长句用长度差近似
fn l3_char_diff_ratio(a: &str, b: &str) -> f64 {
    let (x, y): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let (n, m) = (x.len(), y.len());
    let mx = n.max(m);
    if mx == 0 {
        return 0.0;
    }
    if mx > 400 {
        return (mx - n.min(m)) as f64 / mx as f64;
    }
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if x[i] == y[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    1.0 - dp[0][0] as f64 / mx as f64
}

/// 追加一条 JSONL 记录到 `l3_data/<lang>/<file>`（按 `key_of` 去重，超 `max` 截断保留最新）
fn l3_append_jsonl(
    dir: &Path,
    lang: &str,
    file: &str,
    sample: &serde_json::Value,
    max: usize,
    key_of: &dyn Fn(&serde_json::Value) -> String,
) -> Option<usize> {
    let lang_dir = dir.join(lang);
    if let Err(e) = std::fs::create_dir_all(&lang_dir) {
        tracing::warn!(lang, error = %e, "L3 沉淀：创建样本目录失败");
        return None;
    }
    let path = lang_dir.join(file);
    let mut lines: Vec<String> = match std::fs::read_to_string(&path) {
        Ok(s) => s
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.to_string())
            .collect(),
        Err(_) => Vec::new(),
    };
    let key = key_of(sample);
    let dup = lines.iter().any(|l| {
        serde_json::from_str::<serde_json::Value>(l)
            .map(|v| key_of(&v) == key)
            .unwrap_or(false)
    });
    if dup {
        return None;
    }
    lines.push(serde_json::to_string(sample).unwrap_or_default());
    if lines.len() > max {
        let drop = lines.len() - max;
        lines.drain(0..drop);
    }
    if let Err(e) = std::fs::write(&path, format!("\n{}", lines.join("\n"))) {
        tracing::warn!(lang, error = %e, "L3 沉淀：样本写入失败");
        return None;
    }
    Some(lines.len())
}

/// 采纳/放弃反馈的资产沉淀（非阻断）：
/// - adopt + `ctx.src_text`/`ctx.corrected_text` → 切句对齐写 `adopted.jsonl`（句对平行语料）
/// - reject → 片段级负偏好写 `rejected.jsonl`（偏好/DPO 材料）
pub(crate) fn collect_feedback_assets_cmd(
    core: &Core,
    lang: &str,
    error: &shared_model::Error,
    action: &str,
    reason: &Option<String>,
    ctx: Option<&serde_json::Value>,
) {
    if !adopt_collect_enabled() || !matches!(lang, "en" | "zh") {
        return;
    }
    let dir = core.l3_data.clone();
    match action {
        "adopt" => {
            let src = ctx
                .and_then(|c| c.get("src_text"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let dst = ctx
                .and_then(|c| c.get("corrected_text"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if src.trim().is_empty() || dst.trim().is_empty() {
                return;
            }
            let src_s = l3_split_sentences(src);
            let dst_s = l3_split_sentences(dst);
            let pairs: Vec<(String, String)> = if !src_s.is_empty() && src_s.len() == dst_s.len() {
                src_s
                    .into_iter()
                    .zip(dst_s)
                    .filter(|(a, b)| a != b)
                    .collect()
            } else {
                let (a, b) = (src.trim(), dst.trim());
                if a != b {
                    vec![(a.to_string(), b.to_string())]
                } else {
                    Vec::new()
                }
            };
            let key_of = |v: &serde_json::Value| -> String {
                format!(
                    "{}|{}",
                    v.get("src").and_then(|v| v.as_str()).unwrap_or(""),
                    v.get("dst").and_then(|v| v.as_str()).unwrap_or("")
                )
            };
            let mut added = 0usize;
            for (s, d) in pairs {
                let sample = serde_json::json!({
                    "src": s,
                    "dst": d,
                    "weak": l3_char_diff_ratio(&s, &d) > L3_WEAK_EDIT_RATIO,
                    "source": "adopt",
                    "ts": chrono::Utc::now().to_rfc3339(),
                });
                if l3_append_jsonl(
                    &dir,
                    lang,
                    "adopted.jsonl",
                    &sample,
                    L3_MAX_ADOPTED,
                    &key_of,
                )
                .is_some()
                {
                    added += 1;
                }
            }
            if added > 0 {
                tracing::info!(lang, added, "沉淀：采纳平行句对已汇入 adopted.jsonl");
            }
        }
        "reject" => {
            let sample = serde_json::json!({
                "fragment": error.original,
                "rejected_correction": error.correction,
                "error_type": error.error_type,
                "source_layer": error.source,
                "reason": reason.clone().unwrap_or_default(),
                "ts": chrono::Utc::now().to_rfc3339(),
            });
            let key_of = |v: &serde_json::Value| -> String {
                format!(
                    "{}|{}",
                    v.get("fragment").and_then(|v| v.as_str()).unwrap_or(""),
                    v.get("rejected_correction")
                        .and_then(|v| v.as_str())
                        .unwrap_or(""),
                )
            };
            if l3_append_jsonl(
                &dir,
                lang,
                "rejected.jsonl",
                &sample,
                L3_MAX_ADOPTED,
                &key_of,
            )
            .is_some()
            {
                tracing::info!(lang, "沉淀：放弃负偏好已汇入 rejected.jsonl");
            }
        }
        _ => {}
    }
}

/// JSONL 资产计数（文件缺失 = 0）
fn l3_count_jsonl(dir: &Path, lang: &str, file: &str) -> usize {
    let path = dir.join(lang).join(file);
    match std::fs::read_to_string(&path) {
        Ok(s) => s.lines().filter(|l| !l.trim().is_empty()).count(),
        Err(_) => 0,
    }
}

/// 读取上次重训练时间（feature-model 写的 retrain_meta.json）
fn l3_last_retrain_at(dir: &Path, lang: &str) -> Option<String> {
    let s = std::fs::read_to_string(dir.join("retrain_meta.json")).ok()?;
    let meta = serde_json::from_str::<serde_json::Value>(&s).ok()?;
    meta.get(lang)?
        .get("ts")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// 是否建议重训练（样本积累够 + 冷却期）
fn l3_retrain_ready(total: usize, last_at: Option<&str>) -> bool {
    let enough = match last_at {
        Some(_) => total >= L3_RETRAIN_READY_POS,
        None => total >= L3_RETRAIN_READY_FIRST,
    };
    if !enough {
        return false;
    }
    match last_at {
        None => true,
        Some(ts) => {
            // RFC 3339（`+08:00`）优先；兼容无冒号时区（`+0800`）的旧元数据
            let secs = chrono::DateTime::parse_from_rfc3339(ts)
                .or_else(|_| chrono::DateTime::parse_from_str(ts, "%Y-%m-%dT%H:%M:%S%z"))
                .map(|d| d.timestamp())
                .unwrap_or(0);
            chrono::Utc::now().timestamp() - secs >= L3_RETRAIN_COOLDOWN
        }
    }
}

/// L3 飞轮：采集一条样本（前端「采纳」LLM 错误 / 干净句自动汇入）
#[tauri::command]
pub async fn l3_collect(
    core: tauri::State<'_, Arc<Core>>,
    lang: String,
    text: String,
    label: Option<u8>,
    error_type: Option<String>,
    source: Option<String>,
) -> Result<serde_json::Value, String> {
    if !matches!(lang.as_str(), "en" | "zh") {
        return Err(format!("不支持的语言: {lang}"));
    }
    if text.trim().is_empty() {
        return Err("text 不能为空".into());
    }
    let label = label.unwrap_or(1);
    let sample = serde_json::json!({
        "text": text,
        "label": label,
        "error_type": error_type.unwrap_or_default(),
        "source": source.unwrap_or_else(|| {
            if label == 1 {
                "llm_adopt".to_string()
            } else {
                "llm_clean".to_string()
            }
        }),
        "ts": chrono::Utc::now().to_rfc3339(),
    });
    match l3_collect_sample(&core.l3_data, &lang, &sample) {
        Some(total) => Ok(serde_json::json!({ "added": true, "total": total })),
        None => Ok(serde_json::json!({
            "added": false,
            "total": l3_count_samples(&core.l3_data, &lang),
        })),
    }
}

/// L3 飞轮：状态查询（样本数 + 上次重训练 + 当前模型 F1）
#[tauri::command]
pub async fn l3_status(
    core: tauri::State<'_, Arc<Core>>,
    lang: Option<String>,
) -> Result<serde_json::Value, String> {
    let lang = lang.unwrap_or_else(|| "zh".to_string());
    if !matches!(lang.as_str(), "en" | "zh") {
        return Err(format!("不支持的语言: {lang}"));
    }
    let dir = core.l3_data.clone();
    let total = l3_count_samples(&dir, &lang);
    let last = l3_last_retrain_at(&dir, &lang);
    // 记录实际解析的数据目录：与 feature-model 服务读取目录不一致时重训看不到样本
    // （开发 = 仓库内 feature-model/l3_data；打包应用 = ~/.langfixer/l3_data；
    // Python 服务侧可用 L3_DATA_DIR 环境变量对齐）
    tracing::info!(data_dir = %dir.display(), "L3 飞轮数据目录（解析结果）");

    let cfg = core.settings.read().unwrap().l3.clone();
    let (service_available, model, f1) = match reqwest::Client::new()
        .get(format!("{}/l3/status", cfg.endpoint.trim_end_matches('/')))
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => match resp.json::<serde_json::Value>().await {
            Ok(v) => {
                let m = v.get("models").and_then(|m| m.get(&lang));
                (
                    true,
                    m.and_then(|m| m.get("model"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    m.and_then(|m| m.get("f1")).and_then(|v| v.as_f64()),
                )
            }
            Err(_) => (true, None, None),
        },
        _ => (false, None, None),
    };

    Ok(serde_json::json!({
        "samples": total,
        "adopted": l3_count_jsonl(&dir, &lang, "adopted.jsonl"),
        "rejected": l3_count_jsonl(&dir, &lang, "rejected.jsonl"),
        "last_retrain_at": last,
        "retrain_ready": l3_retrain_ready(total, last.as_deref()),
        "service_available": service_available,
        "model": model,
        "f1": f1,
        "data_dir": dir.to_string_lossy().into_owned(),
    }))
}

/// L3 飞轮：触发重训练（转发 feature-model POST /l3-retrain）
#[tauri::command]
pub async fn l3_retrain(
    core: tauri::State<'_, Arc<Core>>,
    lang: String,
) -> Result<serde_json::Value, String> {
    l3_retrain_forward(&core, &lang).await
}

/// L3 重训练转发（`l3_retrain` 命令与训练中心 `train_run_l3_retrain` 共用）
pub(crate) async fn l3_retrain_forward(
    core: &Core,
    lang: &str,
) -> Result<serde_json::Value, String> {
    if !matches!(lang, "en" | "zh") {
        return Err(format!("不支持的语言: {lang}"));
    }
    let cfg = core.settings.read().unwrap().l3.clone();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(cfg.timeout_secs.max(120)))
        .build()
        .map_err(|e| format!("HTTP 客户端构建失败: {e}"))?;
    let resp = client
        .post(format!("{}/l3-retrain", cfg.endpoint.trim_end_matches('/')))
        .json(&serde_json::json!({ "lang": lang }))
        .send()
        .await
        .map_err(|e| format!("特征模型服务不可达（{e}"))?;
    let (code, body) = (resp.status(), resp.json::<serde_json::Value>().await);
    let body = body.map_err(|e| format!("特征模型响应解析失败: {e}"))?;
    if !code.is_success() {
        let msg = body
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("重训练失败");
        return Err(msg.to_string());
    }
    // 记录本次重训练时间（feature-model 侧也会写 retrain_meta.json，双写以本地为准）
    let dir = core.l3_data.clone();
    let retrain_at = chrono::Utc::now().to_rfc3339();
    let meta_path = dir.join("retrain_meta.json");
    let existing = std::fs::read_to_string(&meta_path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
    let mut meta = existing.unwrap_or_else(serde_json::Value::default);
    if !meta.is_object() {
        meta = serde_json::Map::new().into();
    }
    meta[lang] = serde_json::json!({
        "ts": retrain_at,
        "confirmed": l3_count_samples(&dir, lang),
        "source": "desktop-app",
    });
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(&meta_path, meta.to_string());

    Ok(serde_json::json!({
        "swapped": body.get("swapped").and_then(|v| v.as_bool()).unwrap_or(false),
        "reason": body.get("reason").and_then(|v| v.as_str()),
        "before_f1": body.get("before_f1").and_then(|v| v.as_f64()),
        "after_f1": body.get("after_f1").and_then(|v| v.as_f64()),
        "samples": body.get("samples").cloned(),
        "retrain_at": retrain_at,
    }))
}
// ─────────────────────────────────────────────────────────────────────────────
// 统一反馈 + 反思流程（镜像 api-server `/gec/feedback*`）
// ─────────────────────────────────────────────────────────────────────────────

/// 反馈日志写入（失败仅告警，不阻断反馈主流程）
fn log_feedback_cmd(
    core: &Core,
    lang: &str,
    source: &str,
    error: &shared_model::Error,
    action: &str,
    reason: &Option<String>,
) {
    let rule_id = error.rule_id.as_deref();
    if let Err(e) = core.feedback_log.insert(
        lang,
        source,
        rule_id,
        &error.error_type,
        &error.original,
        error.correction.as_deref().unwrap_or(""),
        action,
        reason.as_deref().unwrap_or(""),
    ) {
        tracing::warn!(error = %e, "反馈日志写入失败（不阻断）");
    }
}

/// L3 飞轮样本采集（非阻断；en/zh 才有 L3 模型）
fn l3_collect_feedback(
    core: &Core,
    lang: &str,
    error: &shared_model::Error,
    label: u8,
    source: &str,
) {
    if !matches!(lang, "en" | "zh") {
        return;
    }
    let sample = serde_json::json!({
        "text": error.original,
        "label": label,
        "error_type": if label == 1 { error.error_type.to_string() } else { String::new() },
        "source": source,
        "ts": chrono::Utc::now().to_rfc3339(),
    });
    let _ = l3_collect_sample(&core.l3_data, lang, &sample);
}

/// 模型层（decision / feature / statistical）采纳：蒸馏为规则（失败时降级/// 降级知识候选）
async fn adopt_as_rule_cmd(
    core: &Core,
    lang: &str,
    error: &shared_model::Error,
    rule_id_prefix: &str,
) -> Result<serde_json::Value, String> {
    let rule_id = error
        .rule_id
        .clone()
        .unwrap_or_else(|| format!("{rule_id_prefix}_{}", error.error_type.to_uppercase()));
    let wrong = &error.original;
    let right = error
        .correction
        .as_deref()
        .ok_or_else(|| "采纳操作需要 correction 字段（修正文本）".to_string())?;
    let settings = core.settings.read().unwrap().clone();
    let lexicons_dir = std::path::PathBuf::from(&settings.lexicons_dir);
    // ⓪ 目标规则在词表文件中但不在 store（建库后才加入文件，播种只在库空时执行）：
    //    先按文件回填定义（保留文件 enabled / category / priority / description），
    //    防止 `upsert_example` 自动创建「默认停用」影子定义、
    //    随后 `sync_rules_to_file` 把停用状态写回文件关掉原本启用的规则
    if let Err(e) = core
        .lexicon_store
        .backfill_rule_from_file(&lexicons_dir, lang, &rule_id)
    {
        tracing::warn!(
            error = %e,
            rule_id = %rule_id,
            "从词表文件回填规则定义失败（继续默认流程）"
        );
    }
    // ① 优先：编译为规则（真正的新规则默认停用，可在规则面板启用）
    let (rule_ok, created) =
        match core
            .lexicon_store
            .upsert_example(lang, &rule_id, wrong, right, "llm_distill")
        {
            Ok(created) => (true, created),
            Err(e) => {
                tracing::warn!(error = %e, rule_id = %rule_id, "规则入库失败，fallback 到知识存储");
                (false, false)
            }
        };
    // ② 规则能否生效的前提：写回词表文件（检测器只在构造时加载词表）
    let file_exists = lexicons_dir
        .join(format!("{lang}_gec_lexicon.json"))
        .exists();
    if rule_ok && file_exists {
        match core.lexicon_store.sync_rules_to_file(&lexicons_dir, lang) {
            Ok(outcome) => {
                core.rebuild_router(&settings);
                tracing::info!(
                    rule = %rule_id,
                    created = created,
                    updated = ?outcome.updated_rule,
                    "采纳：规则已写回词表文件，检测器已重建"
                );
                let enabled = core
                    .lexicon_store
                    .get_rule(lang, &rule_id)
                    .ok()
                    .flatten()
                    .map(|d| d.enabled)
                    .unwrap_or(false);
                let message = if created {
                    if enabled {
                        format!(
                            "已采纳：「{wrong}」→「{right}」补入规则 {rule_id}（规则保持启用，后续同类错误将继续报告）"
                        )
                    } else {
                        format!(
                            "已采纳：「{wrong}」→「{right}」编译为规则 {rule_id}（默认停用，可在规则面板启用）"
                        )
                    }
                } else if enabled {
                    format!("该示例已存在于规则 {rule_id}（规则保持启用，可在规则面板切换开关）")
                } else {
                    format!("该示例已存在于规则 {rule_id}（默认停用，可在规则面板启用）")
                };
                return Ok(serde_json::json!({
                    "action": "adopt",
                    "message": message,
                    "via": "rule",
                    "rule": {
                        "id": rule_id,
                        "wrong": wrong,
                        "right": right,
                        "enabled": enabled,
                        "created": created,
                    },
                }));
            }
            Err(e) => {
                tracing::warn!(error = %e, "规则写回词表文件失败，转知识分支");
            }
        }
    }
    // ④ 知识分支：规则失败 → 知识候选（UI 展示 + 召回测试后用户选择保留/放弃）
    let reason: String = if !rule_ok {
        "规则入库失败".to_string()
    } else if !file_exists {
        format!("该语言无词表文件（{lang}），规则无法被检测器加载")
    } else {
        "规则写回词表文件失败".to_string()
    };
    Ok(serde_json::json!({
        "action": "adopt",
        "message": format!("规则构建失败（{reason}），建议沉淀为知识"),
        "via": "knowledge",
        "knowledge": {
            "text": wrong,
            "error_type": error.error_type,
            "correction": right,
        },
    }))
}

/// 组装反思上下文：按来源层从各存储定位具体对象
fn build_reflection_context_cmd<'a>(
    core: &Core,
    lang: &str,
    error: &'a shared_model::Error,
    reason: Option<&'a str>,
) -> gec_core::ReflectionContext<'a> {
    let source = gec_core::normalize_source(error);
    let mut rule_def: Option<gec_core::RuleDefInfo> = None;
    let mut rule_feedback: Option<(i64, i64)> = None;
    let mut matched_example: Option<gec_core::ExampleInfo> = None;
    let mut knowledge_item: Option<gec_core::KnowledgeInfo> = None;
    if source == shared_model::sources::RULE {
        if let Some(rule_id) = &error.rule_id {
            if let Some(def) = core.lexicon_store.rule_defs_by_id(rule_id).ok().flatten() {
                let count = core
                    .lexicon_store
                    .list_rule_examples(lang, rule_id, 1, 1)
                    .map(|l| l.total)
                    .unwrap_or(0);
                rule_def = Some(gec_core::RuleDefInfo {
                    id: def.id,
                    category: def.category,
                    description: def.description,
                    enabled: def.enabled,
                    example_count: count,
                });
            }
            rule_feedback = core.feedback_log.stats_by_rule_distinct(lang, rule_id).ok(); // 已含本次（log_feedback_cmd 先于本函数执行）；按 original 去重
            if let Ok(list) = core.lexicon_store.list_rule_examples(lang, rule_id, 1, 200) {
                matched_example = list
                    .examples
                    .into_iter()
                    .find(|ex| ex.wrong.trim() == error.original.trim())
                    .map(|ex| gec_core::ExampleInfo {
                        wrong: ex.wrong,
                        right: ex.right,
                        source: ex.source,
                    });
            }
        }
    } else if source == shared_model::sources::KNOWLEDGE {
        if let Ok(items) = core.learned_knowledge.list(lang) {
            // ① 精确：条目文本 == original
            if let Some(it) = items
                .iter()
                .find(|it| it.text.trim() == error.original.trim())
            {
                knowledge_item = Some(gec_core::KnowledgeInfo {
                    id: it.id,
                    text: it.text.clone(),
                    error_type: it.error_type.clone(),
                    correction: it.correction.clone(),
                    source: it.source.clone(),
                });
            } else {
                // ② 模糊：bigram 相似度最高者
                let mut best: Option<(i64, f64)> = None;
                for it in &items {
                    let sim = gec_core::fuzzy_similarity(&it.text, &error.original);
                    if sim >= gec_core::knowledge_recall::FUZZY_THRESHOLD
                        && best.as_ref().is_none_or(|(_, b)| sim > *b)
                    {
                        best = Some((it.id, sim));
                    }
                }
                if let Some((id, _)) = best {
                    knowledge_item =
                        items
                            .iter()
                            .find(|it| it.id == id)
                            .map(|it| gec_core::KnowledgeInfo {
                                id: it.id,
                                text: it.text.clone(),
                                error_type: it.error_type.clone(),
                                correction: it.correction.clone(),
                                source: it.source.clone(),
                            });
                }
            }
        }
    }
    gec_core::ReflectionContext {
        error,
        reason,
        rule_def,
        rule_feedback,
        matched_example,
        knowledge_item,
    }
}
/// 检测结果统一反馈（采纳 / 放弃）+ 反思流程（全部来源层）
///
/// 镜像 api-server `POST /gec/feedback`：
/// - adopt：llm → 复用现有 LLM 反馈采纳分支（anomaly 蒸馏 / 规则蒸馏 / 知识候选）；
///   rule（非 anomaly）→ 确认规则正确（留痕 + 示例补强）；
///   模型层（decision/feature/statistical）→ 蒸馏为规则 + L3 正样本；
///   knowledge → 确认知识条目正确（留痕）；
/// - reject：白名单止血 + 反馈留痕 + 反思报告（诊断 + 可执行建议）
///   + 模型层自动汇入 L3 负样本。
#[tauri::command]
pub async fn error_feedback(
    core: tauri::State<'_, Arc<Core>>,
    lang: String,
    error: shared_model::Error,
    action: String,
    reason: Option<String>,
    ctx: Option<serde_json::Value>,
) -> Result<serde_json::Value, String> {
    let res = error_feedback_body(
        core.clone(),
        lang.clone(),
        error.clone(),
        action.clone(),
        reason.clone(),
    )
    .await?;
    // 资产沉淀（非阻断）：adopt → adopted.jsonl 句对；reject → rejected.jsonl 负偏好
    collect_feedback_assets_cmd(core.inner(), &lang, &error, &action, &reason, ctx.as_ref());
    Ok(res)
}

async fn error_feedback_body(
    core: tauri::State<'_, Arc<Core>>,
    lang: String,
    error: shared_model::Error,
    action: String,
    reason: Option<String>,
) -> Result<serde_json::Value, String> {
    let source = gec_core::normalize_source(&error).to_string();
    let llm_reflect_ready = core.llm.read().unwrap().is_available();
    match action.as_str() {
        "adopt" => {
            if source == shared_model::sources::LLM {
                // 复用现有 LLM 反馈采纳分支（anomaly 蒸馏 / 规则蒸馏 / 知识候选）
                let res = llm_feedback_inner(core.inner(), &lang, &error, "adopt").await?;
                // 采纳留痕（成功后记录；失败请求不落日志）
                log_feedback_cmd(core.inner(), &lang, &source, &error, "adopt", &reason);
                l3_collect_feedback(core.inner(), &lang, &error, 1, "llm_adopt");
                return Ok(res);
            }
            if source == shared_model::sources::RULE {
                if error.error_type == error_types::ANOMALY {
                    let res = anomaly_adopt_cmd(core.inner(), &lang, &error).await?;
                    // 采纳留痕（anomaly 分支提前返回，须在此记录；失败请求不落日志）
                    log_feedback_cmd(core.inner(), &lang, &source, &error, "adopt", &reason);
                    return Ok(res);
                }
                // 确认规则正确：留痕 + 示例补强（幂等）
                log_feedback_cmd(core.inner(), &lang, &source, &error, "adopt", &reason);
                let mut reinforced = false;
                if let (Some(rule_id), Some(right)) =
                    (error.rule_id.as_deref(), error.correction.as_deref())
                {
                    if !right.trim().is_empty() {
                        // 规则在词表文件中但不在 store（建库后才加入文件，播种只在库空时执行）：
                        // 先按文件回填定义，防止 upsert 自动创建「默认停用」影子定义
                        let lexicons_dir =
                            PathBuf::from(&core.settings.read().unwrap().lexicons_dir);
                        if let Err(e) = core.lexicon_store.backfill_rule_from_file(
                            &lexicons_dir,
                            &lang,
                            rule_id,
                        ) {
                            tracing::warn!(
                                error = %e,
                                rule_id = %rule_id,
                                "从词表文件回填规则定义失败（继续默认流程）"
                            );
                        }
                        if let Ok(true) = core.lexicon_store.upsert_example(
                            &lang,
                            rule_id,
                            &error.original,
                            right,
                            "manual",
                        ) {
                            reinforced = true;
                        }
                    }
                }
                let rule_id = error
                    .rule_id
                    .clone()
                    .unwrap_or_else(|| "（未入库规则）".to_string());
                let message = if reinforced {
                    format!(
                        "已采纳：确认规则 {rule_id} 正确，并补强示例「{}」→「{}」",
                        error.original,
                        error.correction.as_deref().unwrap_or("")
                    )
                } else {
                    format!("已采纳：确认规则 {rule_id} 正确（后续同类错误将继续报告）")
                };
                return Ok(serde_json::json!({
                    "action": "adopt",
                    "message": message,
                    "via": "confirm",
                    "rule": { "id": rule_id, "enabled": true },
                }));
            }
            if matches!(
                source.as_str(),
                shared_model::sources::DECISION
                    | shared_model::sources::FEATURE
                    | shared_model::sources::STATISTICAL
            ) {
                let res = adopt_as_rule_cmd(core.inner(), &lang, &error, "MODEL").await?;
                // 采纳留痕（成功后记录；失败请求不落日志）
                log_feedback_cmd(core.inner(), &lang, &source, &error, "adopt", &reason);
                l3_collect_feedback(core.inner(), &lang, &error, 1, "model_adopt");
                return Ok(res);
            }
            if source == shared_model::sources::KNOWLEDGE {
                log_feedback_cmd(core.inner(), &lang, &source, &error, "adopt", &reason);
                return Ok(serde_json::json!({
                    "action": "adopt",
                    "message": format!(
                        "已采纳：确认知识条目「{}」为真实错误（后续同类错误将继续报告）",
                        error.original
                    ),
                    "via": "confirm",
                }));
            }
            Err(format!("未知来源层: {source}"))
        }
        "reject" => {
            // ① 白名单：original 不再报告（所有来源层通用止血手段）
            let word = error.original.clone();
            let cw = core
                .custom_words
                .add(&lang, &word, "检测结果被用户放弃")
                .map_err(|e| e.to_string())?;
            // ② 反馈留痕（反思流程的数据基础）
            log_feedback_cmd(core.inner(), &lang, &source, &error, "reject", &reason);
            // ③ 反思：按来源层诊断偏差来源，产出报告 + 可执行建议
            let ctx = build_reflection_context_cmd(core.inner(), &lang, &error, reason.as_deref());
            let report = gec_core::reflect_rejection(&ctx);
            // ④ 模型层偏差：自动汇入 L3 飞轮负样本（非阻断）
            if matches!(
                source.as_str(),
                shared_model::sources::DECISION
                    | shared_model::sources::FEATURE
                    | shared_model::sources::STATISTICAL
            ) {
                l3_collect_feedback(core.inner(), &lang, &error, 0, "reject_model");
            }
            // ⑤ 自进化 agent：LLM 已配置时自主完成低风险处置并产出高风险提案
            //    （同步 + 超时保护；失败/未配置 → agent: null，行为与引入前一致）
            let (agent_available, agent_result, agent_error) = {
                let source = error.source.as_deref().unwrap_or("未知");
                let rule = error.rule_id.as_deref().unwrap_or("（无关联规则）");
                let correction = error.correction.as_deref().unwrap_or("（无）");
                let task = gec_core::build_rejection_task(
                    &lang,
                    &error.original,
                    &error.error_type,
                    source,
                    rule,
                    correction,
                    &report.diagnosis,
                );
                run_agent_cmd(core.inner(), &lang, &task, Some(&error)).await
            };
            if agent_available && agent_result.is_none() {
                tracing::warn!(
                    error = %agent_error.unwrap_or_default(),
                    "自进化 agent 运行失败（已回退确定性路径）"
                );
            }
            tracing::info!(
                lang = %lang,
                source = %source,
                subject = %report.subject,
                "检测结果被放弃：反思完成"
            );
            Ok(serde_json::json!({
                "action": "reject",
                "message": format!("已放弃：「{word}」加入白名单；偏差诊断：{}", report.diagnosis),
                "word_id": cw.id,
                "reflection": report,
                "llm_reflect_ready": llm_reflect_ready,
                "agent": agent_result,
            }))
        }
        other => Err(format!("无效操作: {other}（应为 \"adopt\" 或 \"reject\"）")),
    }
}

/// 运行一次自进化 agent（桌面端镜像；与 API 端 `/gec/agent/run` 同构）
///
/// 返回 `(agent_available, result, error)`：LLM 未配置 / 该语言无 agent 模板时
/// `agent_available = false`，行为与未引入 agent 前完全一致。
async fn run_agent_cmd(
    core: &Arc<Core>,
    lang: &str,
    task: &str,
    error: Option<&shared_model::Error>,
) -> (bool, Option<gec_core::AgentRunResult>, Option<String>) {
    let service = core.llm.read().unwrap().clone();
    let Some(template) = service.templates.for_lang_cefr(lang, "agent") else {
        return (false, None, None);
    };
    if !service.config.enabled() {
        return (false, None, None);
    }
    let ctx = crate::agent_tools::AgentToolCtx::new(core, lang);
    let tools = ctx.build_tools();
    let input = template.render(&[("task", task)]);
    let config = LlmDetectConfig::from_parts(
        service.config.base_url.clone(),
        service.config.api_key.clone(),
        service.config.model.clone(),
        service.config.timeout_secs,
        template.system.clone(),
        input.clone(),
    );
    match tokio::time::timeout(
        gec_core::agent_timeout(),
        gec_core::run_agent(&config, &template.system, &input, tools),
    )
    .await
    {
        Ok(Ok(result)) => {
            // 审计：执行 / 提案动作记入反馈日志（source="agent"）
            let rule_id = error.and_then(|e| e.rule_id.as_deref());
            let error_type = error.map(|e| e.error_type.clone()).unwrap_or_default();
            let original = error.map(|e| e.original.clone()).unwrap_or_default();
            let correction = error.and_then(|e| e.correction.clone()).unwrap_or_default();
            for a in &result.executed {
                let _ = core.feedback_log.insert(
                    lang,
                    "agent",
                    rule_id,
                    &error_type,
                    &original,
                    &correction,
                    &format!("agent:{}", a.tool),
                    &a.params.to_string(),
                );
            }
            for p in &result.proposals {
                let _ = core.feedback_log.insert(
                    lang,
                    "agent",
                    rule_id,
                    &error_type,
                    &original,
                    &correction,
                    &format!("agent_proposal:{}", p.tool),
                    &p.params.to_string(),
                );
            }
            (true, Some(result), None)
        }
        Ok(Err(e)) => (true, None, Some(e.to_string())),
        Err(_) => (true, None, Some("agent 运行超时".into())),
    }
}

/// POST 等价：手动触发一次 agent run（前端「让 agent 处理」入口）
#[tauri::command]
pub async fn feedback_agent(
    core: tauri::State<'_, Arc<Core>>,
    lang: String,
    task: String,
    error: Option<shared_model::Error>,
) -> Result<serde_json::Value, String> {
    if task.trim().is_empty() {
        return Err("task 不能为空".into());
    }
    let (agent_available, result, err) =
        run_agent_cmd(core.inner(), &lang, &task, error.as_ref()).await;
    Ok(serde_json::json!({
        "agent_available": agent_available,
        "result": result,
        "error": err,
    }))
}

/// POST 等价：用户确认后执行 agent 提案的高风险动作（与 API 端 `/gec/agent/confirm` 同构）
///
/// 安全约束同 API 端：仅接受 Confirm 层级工具；Autonomous 工具在 agent 循环内
/// 已直接执行，不接受二次提交（防重放）。审计记入反馈日志（`source="agent_confirm"`）。
#[tauri::command]
pub async fn confirm_agent_proposal(
    core: tauri::State<'_, Arc<Core>>,
    lang: String,
    tool: String,
    params: Option<serde_json::Value>,
    error: Option<shared_model::Error>,
) -> Result<serde_json::Value, String> {
    if tool.trim().is_empty() {
        return Err("tool 不能为空".into());
    }
    let ctx = crate::agent_tools::AgentToolCtx::new(core.inner(), &lang);
    let tools = ctx.build_tools();
    let mt = tools
        .iter()
        .find(|t| t.name == tool)
        .ok_or_else(|| format!("未知 agent 工具: {}", tool))?;
    if mt.tier != ActionTier::Confirm {
        return Err(format!(
            "工具 {} 不是 Confirm 层级（自主动作在 agent 循环内已执行，不可二次提交）",
            tool
        ));
    }
    let params = match params {
        Some(p) if !p.is_null() => p,
        _ => serde_json::Value::Object(serde_json::Map::new()),
    };
    let message = mt.executor.execute(params.clone()).await.map_err(|e| e)?;
    // 审计：确认执行的高风险动作记入反馈日志
    let rule_id = error.as_ref().and_then(|e| e.rule_id.as_deref());
    let error_type = error
        .as_ref()
        .map(|e| e.error_type.clone())
        .unwrap_or_default();
    let original = error
        .as_ref()
        .map(|e| e.original.clone())
        .unwrap_or_default();
    let correction = error
        .as_ref()
        .and_then(|e| e.correction.clone())
        .unwrap_or_default();
    let _ = core.feedback_log.insert(
        &lang,
        "agent_confirm",
        rule_id,
        &error_type,
        &original,
        &correction,
        &format!("agent_confirm:{}", tool),
        &params.to_string(),
    );
    Ok(serde_json::json!({ "ok": true, "message": message }))
}

/// 执行反思建议（前端一键按钮）
///
/// `action`：`disable_rule` / `exclude_word` / `remove_example` / `remove_knowledge` / `l3_negative`
#[tauri::command]
pub fn feedback_action(
    core: tauri::State<'_, Arc<Core>>,
    action: String,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let p = &payload;
    let get = |k: &str| p.get(k).and_then(|v| v.as_str()).unwrap_or("");
    match action.as_str() {
        "disable_rule" => {
            let (lang, rule_id) = (get("lang"), get("rule_id"));
            if lang.is_empty() || rule_id.is_empty() {
                return Err("disable_rule 需要 lang + rule_id".into());
            }
            core.lexicon_store
                .toggle_rule(lang, rule_id, false)
                .map_err(|e| e.to_string())?;
            Ok(serde_json::json!({
                "ok": true,
                "message": format!("规则 {rule_id} 已停用（可在规则面板重新启用）"),
            }))
        }
        "remove_example" => {
            let (lang, rule_id, wrong, right) =
                (get("lang"), get("rule_id"), get("wrong"), get("right"));
            if lang.is_empty() || rule_id.is_empty() || wrong.is_empty() {
                return Err("remove_example 需要 lang + rule_id + wrong".into());
            }
            let deleted = core
                .lexicon_store
                .delete_example(lang, rule_id, wrong, right)
                .map_err(|e| e.to_string())?;
            Ok(serde_json::json!({
                "ok": deleted,
                "message": if deleted {
                    format!("示例「{wrong}」已从规则 {rule_id} 删除")
                } else {
                    format!("示例「{wrong}」不在规则 {rule_id} 中（可能已被删除）")
                },
            }))
        }
        "remove_knowledge" => {
            let id = p.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
            if id <= 0 {
                return Err("remove_knowledge 需要 id".into());
            }
            let removed = core
                .learned_knowledge
                .remove(id)
                .map_err(|e| e.to_string())?;
            Ok(serde_json::json!({
                "ok": removed,
                "message": if removed {
                    format!("知识条目 #{id} 已删除（后续不再召回）")
                } else {
                    format!("知识条目 #{id} 不存在（可能已被删除）")
                },
            }))
        }
        "l3_negative" => {
            let (lang, text) = (get("lang"), get("text"));
            if lang.is_empty() || text.is_empty() {
                return Err("l3_negative 需要 lang + text".into());
            }
            if !matches!(lang, "en" | "zh") {
                return Err(format!("L3 飞轮暂不支持语言: {lang}"));
            }
            let error_type = get("error_type");
            let sample = serde_json::json!({
                "text": text,
                "label": 0,
                "error_type": error_type,
                "source": "reflect_negative",
                "ts": chrono::Utc::now().to_rfc3339(),
            });
            let added = l3_collect_sample(&core.l3_data, lang, &sample).is_some();
            Ok(serde_json::json!({
                "ok": true,
                "message": if added {
                    format!("「{text}」已汇入 L3 飞轮负样本（积累后重训练可系统性修正）")
                } else {
                    format!("「{text}」已在 L3 飞轮负样本中（去重）")
                },
            }))
        }
        // 规则排除词（payload: {lang, rule_id, word}）：写回词表 rules[].excludes
        // + 重建检测器——规则保持启用，仅该词不再触发（POS 误标等单点误报的精准对策）
        "exclude_word" => {
            let (lang, rule_id, word) = (get("lang"), get("rule_id"), get("word"));
            if lang.is_empty() || rule_id.is_empty() || word.is_empty() {
                return Err("exclude_word 需要 lang + rule_id + word".into());
            }
            let settings = core.settings.read().unwrap().clone();
            let lexicons_dir = PathBuf::from(settings.lexicons_dir.clone());
            let added = lexicon_store::append_rule_exclude(&lexicons_dir, lang, rule_id, word)
                .map_err(|e| e.to_string())?;
            if added {
                // 检测器只在构造时加载词表 → 重建即时生效（与采纳路径同一机制）
                core.rebuild_router(&settings);
            }
            Ok(serde_json::json!({
                "ok": true,
                "message": if added {
                    format!(
                        "「{word}」已加入规则 {rule_id} 排除名单（该词不再触发此规则，检测器已重建）"
                    )
                } else {
                    format!("「{word}」已在规则 {rule_id} 排除名单中")
                },
            }))
        }
        other => Err(format!("未知操作: {other}")),
    }
}

/// LLM 深度反思（`{lang}_reflect` 模板；与确定性反思分离，避免阻塞放弃操作）
#[tauri::command]
pub async fn reflect_llm(
    core: tauri::State<'_, Arc<Core>>,
    lang: String,
    error: shared_model::Error,
    reason: Option<String>,
) -> Result<serde_json::Value, String> {
    let llm = core.llm.read().unwrap().clone();
    if !llm.is_available() {
        return Err("LLM 服务未启用（请在「设置」界面配置 LLM 参数）".into());
    }
    let template = llm
        .templates
        .for_lang_cefr(&lang, "reflect")
        .ok_or_else(|| format!("语言 {lang} 无反思模板（prompts/{{lang}}_reflect_prompt.json）"))?;
    let source = gec_core::normalize_source(&error);
    let reason = reason.as_deref().unwrap_or("");
    let user = template.render(&[
        ("lang", &lang),
        ("source", source),
        ("error_type", &error.error_type),
        ("original", &error.original),
        (
            "correction",
            error.correction.as_deref().unwrap_or("（无修正）"),
        ),
        ("rule_id", error.rule_id.as_deref().unwrap_or("（无）")),
        (
            "reason",
            if reason.is_empty() {
                "（用户未说明）"
            } else {
                reason
            },
        ),
    ]);
    let analysis = llm
        .complete(&template.system, &user)
        .await
        .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "llm_analysis": analysis,
        "model": llm.config.model,
        "template": format!("{lang}_reflect"),
    }))
}

/// 反馈历史（设置界面「反馈历史」面板）
#[tauri::command]
pub fn feedback_history(
    core: tauri::State<'_, Arc<Core>>,
    lang: String,
    limit: Option<usize>,
) -> Result<serde_json::Value, String> {
    let limit = limit.unwrap_or(50);
    let stats = core.feedback_log.stats(&lang).map_err(|e| e.to_string())?;
    let recent = core
        .feedback_log
        .list(Some(&lang), limit)
        .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "lang": lang,
        "stats": stats,
        "recent": recent,
    }))
}
// ③ 重建检测器：新规则进入检测词表（默认停用

// ─────────────────────────────────────────────────────────────────────────────
// 测评中心（进化中心 P0）：Python 测评服务（datasets/eval_server.py）代理
// web-ui client.ts 桌面端走这 4 个命令（Web 端走 api-server /gec/eval/*）
// ─────────────────────────────────────────────────────────────────────────────

fn eval_endpoint() -> String {
    std::env::var("GEC_EVAL_ENDPOINT")
        .map(|s| s.trim_end_matches('/').to_string())
        .unwrap_or_else(|_| "http://127.0.0.1:4807".to_string())
}

async fn eval_forward(
    method: &str,
    path: &str,
    body: Option<&serde_json::Value>,
    timeout: std::time::Duration,
) -> Result<serde_json::Value, String> {
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| format!("HTTP 客户端构建失败: {e}"))?;
    let mut req = client.request(
        method
            .parse::<reqwest::Method>()
            .map_err(|e| e.to_string())?,
        format!("{}{}", eval_endpoint(), path),
    );
    if let Some(b) = body {
        req = req.json(b);
    }
    let resp = req.send().await.map_err(|e| {
        format!("测评服务未运行（{path} 不可达：{e}；启动：cd datasets && python eval_server.py）")
    })?;
    let (code, body) = (resp.status(), resp.json::<serde_json::Value>().await);
    let body = body.map_err(|e| format!("测评服务响应解析失败: {e}"))?;
    if !code.is_success() {
        let msg = body
            .get("detail")
            .and_then(|v| v.as_str())
            .or_else(|| body.get("error").and_then(|v| v.as_str()))
            .unwrap_or("评测请求失败");
        return Err(format!("{msg}（HTTP {code}）"));
    }
    Ok(body)
}

/// 测评中心：发起评测（转发测评服务 POST /eval/run；body 透传）
#[tauri::command]
pub async fn run_eval(body: serde_json::Value) -> Result<serde_json::Value, String> {
    eval_forward(
        "POST",
        "/eval/run",
        Some(&body),
        std::time::Duration::from_secs(30),
    )
    .await
}

/// 测评中心：任务进度（转发 GET /eval/job/:id）
#[tauri::command]
pub async fn eval_job(id: String) -> Result<serde_json::Value, String> {
    eval_forward(
        "GET",
        &format!("/eval/job/{id}"),
        None,
        std::time::Duration::from_secs(10),
    )
    .await
}

/// 测评中心：历史报告列表（转发 GET /eval/reports）
#[tauri::command]
pub async fn list_eval_reports() -> Result<serde_json::Value, String> {
    eval_forward(
        "GET",
        "/eval/reports",
        None,
        std::time::Duration::from_secs(10),
    )
    .await
}

/// 测评中心：报告详情（转发 GET /eval/report/:id）
#[tauri::command]
pub async fn get_eval_report(id: String) -> Result<serde_json::Value, String> {
    eval_forward(
        "GET",
        &format!("/eval/report/{id}"),
        None,
        std::time::Duration::from_secs(10),
    )
    .await
}

/// 反思中心：生成反思报告（反馈聚合 + 测评证据 → 报告 + 建议队列落库）
#[tauri::command]
pub async fn run_reflect(
    core: tauri::State<'_, Arc<Core>>,
    body: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lang = body
        .get("lang")
        .and_then(|v| v.as_str())
        .unwrap_or("zh")
        .to_string();
    let include_eval = body
        .get("include_eval")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let eval_report_id = body
        .get("eval_report_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let stats: Vec<gec_core::FeedbackStatIn> = core
        .feedback_log
        .stats(&lang)
        .map_err(|e| format!("反馈统计查询失败: {e}"))?
        .into_iter()
        .map(|s| gec_core::FeedbackStatIn {
            source: s.source,
            rule_id: s.rule_id,
            adopted: s.adopted,
            rejected: s.rejected,
            last_at: s.last_at,
        })
        .collect();
    let recent: Vec<gec_core::FeedbackEntryIn> = core
        .feedback_log
        .list(Some(&lang), 50)
        .map_err(|e| format!("反馈明细查询失败: {e}"))?
        .into_iter()
        .map(|e| gec_core::FeedbackEntryIn {
            source: e.source,
            rule_id: e.rule_id,
            error_type: e.error_type,
            original: e.original,
            correction: e.correction,
            action: e.action,
            reason: e.reason,
            created_at: e.created_at,
        })
        .collect();

    // 测评证据（可选；服务不可达 / 无匹配报告 → 降级为纯反馈数据）
    let mut eval = if include_eval {
        let endpoint = std::env::var("GEC_EVAL_ENDPOINT")
            .map(|s| s.trim_end_matches('/').to_string())
            .unwrap_or_else(|_| "http://127.0.0.1:4807".to_string());
        gec_core::fetch_eval_evidence(&endpoint, &lang, eval_report_id.as_deref()).await
    } else {
        None
    };
    if include_eval && eval.is_none() {
        eval = Some(gec_core::EvalEvidence::default());
    }

    let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    // L3 重训练状态（「重训无效」检查）：与 api-server 反思路由同源
    let l3_retrain = {
        let mut st = gec_core::reflection_center::L3RetrainStateIn::default();
        if let Ok(jobs) = core
            .training_center
            .list_jobs(Some(&lang), Some("completed"), 50)
        {
            if let Some(job) = jobs
                .into_iter()
                .find(|j| j.kind == gec_core::job_kind::L3_RETRAIN)
            {
                st.last_job_report_id = job
                    .payload
                    .get("report_id")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                st.f1_before = job.payload.get("gate_f1").and_then(|v| v.as_f64());
            }
        }
        st.f1_after = eval.as_ref().and_then(|e| {
            e.layers
                .iter()
                .find(|l| l.name == "feature")
                .and_then(|l| l.gate_f1)
        });
        st
    };
    let mut report = gec_core::build_report(&lang, &created_at, &stats, &recent, eval, &l3_retrain);
    if include_eval
        && report
            .eval_evidence
            .as_ref()
            .is_some_and(|e| e.report_id.is_empty())
    {
        report
            .findings
            .push("测评证据不可用（测评服务未运行或无匹配报告）——本报告仅基于反馈数据".to_string());
    }
    // 已停用规则不再「建议停用」（反馈是历史数据；规则已停用，建议无意义）
    if let Ok(defs) = core.lexicon_store.rule_defs(&lang) {
        let disabled: std::collections::HashSet<&str> = defs
            .iter()
            .filter(|d| !d.enabled)
            .map(|d| d.id.as_str())
            .collect();
        if !disabled.is_empty() {
            let before = report.suggestions.len();
            report.suggestions.retain(|s| {
                !(s.kind == gec_core::reflection_center::suggestion_kinds::RULE_TUNE
                    && disabled.contains(s.target.as_str()))
            });
            let dropped = before - report.suggestions.len();
            if dropped > 0 {
                report.findings.push(format!(
                    "已剔除 {dropped} 条「停用规则」建议——目标规则当前已停用（反馈为历史数据）"
                ));
            }
        }
    }

    let report_json = serde_json::to_string(&report).map_err(|e| e.to_string())?;
    let seeds: Vec<lexicon_store::SuggestionSeed> = report
        .suggestions
        .iter()
        .map(|s| lexicon_store::SuggestionSeed {
            kind: s.kind.clone(),
            target: s.target.clone(),
            summary: s.summary.clone(),
            evidence: s.evidence.clone(),
            payload: s.payload.clone(),
            priority: s.priority,
        })
        .collect();
    let (id, suggestion_ids) = core
        .reflection_center
        .save_report(&lang, &created_at, &report_json, &seeds)
        .map_err(|e| format!("反思报告保存失败: {e}"))?;
    Ok(serde_json::json!({
        "id": id,
        "report": report,
        "suggestion_ids": suggestion_ids,
    }))
}

/// 反思中心：报告历史（新 → 旧）
#[tauri::command]
pub fn list_reflect_reports(
    core: tauri::State<'_, Arc<Core>>,
    lang: Option<String>,
    limit: Option<usize>,
) -> Result<serde_json::Value, String> {
    let reports = core
        .reflection_center
        .list_reports(lang.as_deref(), limit.unwrap_or(20))
        .map_err(|e| format!("反思报告列表查询失败: {e}"))?;
    Ok(serde_json::json!({ "reports": reports }))
}

/// 反思中心：报告详情
#[tauri::command]
pub fn get_reflect_report(
    core: tauri::State<'_, Arc<Core>>,
    id: i64,
) -> Result<serde_json::Value, String> {
    let full = core
        .reflection_center
        .get_report(id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("反思报告 #{id} 不存在"))?;
    serde_json::to_value(full).map_err(|e| e.to_string())
}

/// 反思中心：建议队列（按状态 / 语言过滤）
#[tauri::command]
pub fn list_reflect_suggestions(
    core: tauri::State<'_, Arc<Core>>,
    status: Option<String>,
    lang: Option<String>,
    limit: Option<usize>,
) -> Result<serde_json::Value, String> {
    let suggestions = core
        .reflection_center
        .list_suggestions(status.as_deref(), lang.as_deref(), limit.unwrap_or(50))
        .map_err(|e| format!("建议队列查询失败: {e}"))?;
    Ok(serde_json::json!({ "suggestions": suggestions }))
}

/// 反思中心：更新建议状态（P2 消费入口：pending / applied / consumed / dismissed）
#[tauri::command]
pub fn update_reflect_suggestion(
    core: tauri::State<'_, Arc<Core>>,
    id: i64,
    status: String,
) -> Result<serde_json::Value, String> {
    if !lexicon_store::reflection_center::suggestion_status::ALL.contains(&status.as_str()) {
        return Err(format!(
            "无效状态: {status}（应为 {}）",
            lexicon_store::reflection_center::suggestion_status::ALL.join(" / ")
        ));
    }
    if !core
        .reflection_center
        .update_suggestion_status(id, &status)
        .map_err(|e| e.to_string())?
    {
        return Err(format!("建议 #{id} 不存在"));
    }
    Ok(serde_json::json!({ "id": id, "status": status }))
}

// ─────────────────────────────────────────────────────────────────────────────
// 训练中心（进化中心 P2；镜像 api-server `/gec/train/*`）
// ─────────────────────────────────────────────────────────────────────────────

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// 作业生命周期：`proposed` → `confirmed` → `running` → `completed` / `failed`
fn training_complete_job(
    core: &Core,
    kind: &str,
    lang: &str,
    source_suggestion: Option<i64>,
    payload: &serde_json::Value,
    result: Result<serde_json::Value, String>,
) -> i64 {
    let id =
        match core
            .training_center
            .insert_job(kind, lang, source_suggestion, payload, &now_iso())
        {
            Ok(id) => id,
            Err(e) => {
                tracing::warn!(kind, error = %e, "训练作业落库失败");
                return 0;
            }
        };
    let _ = core
        .training_center
        .transition(id, "confirmed", None, &now_iso());
    let _ = core
        .training_center
        .transition(id, "running", None, &now_iso());
    let (to, res) = match result {
        Ok(v) => ("completed", v),
        Err(e) => ("failed", serde_json::json!({ "error": e })),
    };
    let _ = core
        .training_center
        .transition(id, to, Some(&res), &now_iso());
    id
}

/// 读取飞轮确认样本 → `PoolSample`（label 1=error / 0=clean）
fn load_flywheel_samples(core: &Core, lang: &str) -> Vec<gec_core::PoolSample> {
    let path = core.l3_data.join(lang).join("confirmed.jsonl");
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).ok()?;
            let sentence = match v.get("text").and_then(|t| t.as_str()) {
                Some(s) if !s.trim().is_empty() => s.to_string(),
                _ => return None,
            };
            let label = v.get("label")?.as_u64().unwrap_or(1);
            Some(gec_core::PoolSample {
                sentence,
                label: if label == 1 { "error" } else { "clean" }.to_string(),
                error_type: v
                    .get("error_type")
                    .and_then(|e| e.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string()),
                correction: None,
                source: "flywheel".to_string(),
            })
        })
        .collect()
}

/// 训练中心：领域池构建（模型层反馈 + 飞轮确认样本 + 增强器干净句）
#[tauri::command]
pub async fn train_build_pool(
    core: tauri::State<'_, Arc<Core>>,
    body: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lang = body
        .get("lang")
        .and_then(|v| v.as_str())
        .unwrap_or("zh")
        .to_string();
    if !matches!(lang.as_str(), "en" | "zh") {
        return Err(format!("不支持的语言: {lang}"));
    }
    let clean_sentences: Vec<String> = body
        .get("clean_sentences")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let entries: Vec<gec_core::FeedbackEntryIn> = core
        .feedback_log
        .list(Some(&lang), 500)
        .map_err(|e| format!("反馈明细查询失败: {e}"))?
        .into_iter()
        .filter(|f| gec_core::is_model_source(&f.source))
        .map(|f| gec_core::FeedbackEntryIn {
            source: f.source,
            rule_id: f.rule_id,
            error_type: f.error_type,
            original: f.original,
            correction: f.correction,
            action: f.action,
            reason: f.reason,
            created_at: f.created_at,
        })
        .collect();
    let flywheel = load_flywheel_samples(&core, &lang);
    let input = gec_core::PoolInput {
        lang: lang.clone(),
        feedback: entries,
        flywheel,
        clean_sentences: clean_sentences.clone(),
    };
    let pool = gec_core::build_domain_pool(&input);
    let payload = serde_json::json!({
        "lang": lang,
        "feedback": input.feedback.len(),
        "flywheel": input.flywheel.len(),
        "augment": clean_sentences.len(),
    });
    let job_id = training_complete_job(
        &core,
        gec_core::job_kind::DOMAIN_POOL,
        &lang,
        None,
        &payload,
        Ok(serde_json::to_value(&pool).unwrap_or(serde_json::Value::Null)),
    );
    Ok(serde_json::json!({ "job_id": job_id, "pool": pool }))
}

/// 训练中心：LLM 训练数据构建（LLM 层 adopt 反馈带修正 → SFT JSONL）
#[tauri::command]
pub async fn train_build_llm_data(
    core: tauri::State<'_, Arc<Core>>,
    body: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lang = body
        .get("lang")
        .and_then(|v| v.as_str())
        .unwrap_or("zh")
        .to_string();
    if !matches!(lang.as_str(), "en" | "zh") {
        return Err(format!("不支持的语言: {lang}"));
    }
    let arm = body
        .get("arm")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let entries: Vec<gec_core::FeedbackEntryIn> = core
        .feedback_log
        .list(Some(&lang), 500)
        .map_err(|e| format!("反馈明细查询失败: {e}"))?
        .into_iter()
        .filter(|f| f.source == "llm")
        .map(|f| gec_core::FeedbackEntryIn {
            source: f.source,
            rule_id: f.rule_id,
            error_type: f.error_type,
            original: f.original,
            correction: f.correction,
            action: f.action,
            reason: f.reason,
            created_at: f.created_at,
        })
        .collect();
    let data = gec_core::build_llm_training_data(&lang, &arm, &entries);
    let payload = serde_json::json!({ "lang": lang, "arm": arm });
    let job_id = training_complete_job(
        &core,
        gec_core::job_kind::LLM_DATA,
        &lang,
        None,
        &payload,
        Ok(serde_json::json!({
            "n_samples": data.samples.len(),
            "jsonl": data.jsonl,
        })),
    );
    Ok(serde_json::json!({ "job_id": job_id, "data": data }))
}

/// 训练中心：L3 重训练（转发 feature-model 飞轮；与 `l3_retrain` 同源）
#[tauri::command]
pub async fn train_run_l3_retrain(
    core: tauri::State<'_, Arc<Core>>,
    body: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lang = body
        .get("lang")
        .and_then(|v| v.as_str())
        .unwrap_or("zh")
        .to_string();
    if !matches!(lang.as_str(), "en" | "zh") {
        return Err(format!("不支持的语言: {lang}"));
    }
    let payload = serde_json::json!({ "lang": lang });
    let result: Result<serde_json::Value, String> = l3_retrain_forward(&core, &lang).await;
    let job_id = training_complete_job(
        &core,
        gec_core::job_kind::L3_RETRAIN,
        &lang,
        None,
        &payload,
        result.clone(),
    );
    match result {
        Ok(v) => Ok(serde_json::json!({ "job_id": job_id, "result": v })),
        Err(e) => Err(e),
    }
}

/// 训练中心：词表条件化入库（条件化排除条目；入库前先过测评消融）
#[tauri::command]
pub async fn train_apply_lexicon(
    core: tauri::State<'_, Arc<Core>>,
    body: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lang = body
        .get("lang")
        .and_then(|v| v.as_str())
        .unwrap_or("zh")
        .to_string();
    if !matches!(lang.as_str(), "en" | "zh") {
        return Err(format!("不支持的语言: {lang}"));
    }
    let suggestion_id = body.get("suggestion_id").and_then(|v| v.as_i64());
    let check_ablation = body
        .get("check_ablation")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // 建议 payload 补全 rule_id / words
    let (mut rule_id, mut words) = (
        body.get("rule_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        body.get("words")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|w| w.as_str().map(|s| s.to_string()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
    );
    if let Some(sid) = suggestion_id {
        if let Some(row) = core
            .reflection_center
            .get_suggestion(sid)
            .map_err(|e| format!("建议查询失败: {e}"))?
        {
            if rule_id.is_none() {
                rule_id = row
                    .payload
                    .get("rule_id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
            }
            if words.is_empty() {
                words = row
                    .payload
                    .get("originals")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|w| w.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
            }
        }
    }
    let rule_id = rule_id.ok_or_else(|| "rule_id 缺失（且建议 payload 无 rule_id）".to_string())?;
    if words.is_empty() {
        return Err("无可入库的排除文本".to_string());
    }
    // 测评消融（可选；不可达 / 无报告 → 降级为直接入库）
    let eval_evidence = if check_ablation {
        let endpoint = std::env::var("GEC_EVAL_ENDPOINT")
            .map(|s| s.trim_end_matches('/').to_string())
            .unwrap_or_else(|_| "http://127.0.0.1:4807".to_string());
        gec_core::fetch_eval_evidence(&endpoint, &lang, None).await
    } else {
        None
    };
    // 入库
    let lexicons_dir = std::path::PathBuf::from(core.settings.read().unwrap().lexicons_dir.clone());
    let mut applied = Vec::new();
    let mut skipped = 0;
    for w in &words {
        match lexicon_store::append_rule_exclude(&lexicons_dir, &lang, &rule_id, w) {
            Ok(true) => applied.push(serde_json::json!({ "rule_id": rule_id, "word": w })),
            Ok(false) => skipped += 1,
            Err(e) => return Err(format!("排除条目入库失败（{w}）: {e}")),
        }
    }
    let payload = serde_json::json!({
        "lang": lang,
        "rule_id": rule_id,
        "words": words,
    });
    let result = serde_json::json!({
        "applied": applied,
        "skipped": skipped,
        "eval_evidence": eval_evidence,
    });
    let job_id = training_complete_job(
        &core,
        gec_core::job_kind::LEXICON_CONDITION,
        &lang,
        suggestion_id,
        &payload,
        Ok(result),
    );
    let suggestion_consumed = match suggestion_id {
        Some(sid) if job_id > 0 => core
            .reflection_center
            .update_suggestion_status(sid, "consumed")
            .map_err(|e| format!("建议状态更新失败: {e}"))?,
        _ => false,
    };
    Ok(serde_json::json!({
        "job_id": job_id,
        "applied": applied,
        "skipped": skipped,
        "eval_evidence": eval_evidence,
        "suggestion_consumed": suggestion_consumed,
    }))
}

/// 训练中心：**P1 → P2 消费入口**——反思建议 → 分派训练动作 → 建议 consumed
///
/// - `l3_sample` → L3 重训练（feature-model 不可达 → 作业 failed，建议保持 pending）
/// - `lexicon_condition` → 词表条件化入库
/// - `llm_arm` → LLM 训练数据构建
/// - `rule_tune` → 错误（人工操作，不产生训练作业）
#[tauri::command]
pub async fn train_run_suggestion(
    core: tauri::State<'_, Arc<Core>>,
    body: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let id = body
        .get("suggestion_id")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| "suggestion_id 缺失".to_string())?;
    let row = core
        .reflection_center
        .get_suggestion(id)
        .map_err(|e| format!("建议查询失败: {e}"))?
        .ok_or_else(|| format!("建议 #{id} 不存在"))?;
    let suggestion = gec_core::reflection_center::ReflectionSuggestion {
        kind: row.kind.clone(),
        target: row.target.clone(),
        summary: row.summary.clone(),
        evidence: row.evidence.clone(),
        payload: row.payload.clone(),
        priority: row.priority as u8,
    };
    let plan = gec_core::plan_from_suggestion(&suggestion)
        .ok_or_else(|| format!("建议类型 {} 不是训练作业（人工操作）", row.kind))?;
    let lang = plan.lang.clone();

    let (job_kind, result): (&str, Result<serde_json::Value, String>) = match plan.kind.as_str() {
        gec_core::job_kind::L3_RETRAIN => {
            let r: Result<serde_json::Value, String> = l3_retrain_forward(&core, &lang).await;
            (gec_core::job_kind::L3_RETRAIN, r)
        }
        gec_core::job_kind::LEXICON_CONDITION => {
            let rule_id = plan
                .payload
                .get("rule_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "建议 payload 缺 rule_id".to_string())?;
            let words = plan
                .payload
                .get("originals")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|w| w.as_str().map(|s| s.to_string()))
                        .collect::<Vec<_>>()
                })
                .ok_or_else(|| "建议 payload 缺 originals".to_string())?;
            if words.is_empty() {
                return Err("建议无可入库的排除文本".to_string());
            }
            let lexicons_dir =
                std::path::PathBuf::from(core.settings.read().unwrap().lexicons_dir.clone());
            let mut applied = Vec::new();
            let mut skipped = 0;
            for w in &words {
                match lexicon_store::append_rule_exclude(&lexicons_dir, &lang, rule_id, w) {
                    Ok(true) => applied.push(serde_json::json!({ "rule_id": rule_id, "word": w })),
                    Ok(false) => skipped += 1,
                    Err(e) => return Err(format!("排除条目入库失败（{w}）: {e}")),
                }
            }
            (
                gec_core::job_kind::LEXICON_CONDITION,
                Ok(serde_json::json!({ "applied": applied, "skipped": skipped })),
            )
        }
        gec_core::job_kind::LLM_DATA => {
            let entries: Vec<gec_core::FeedbackEntryIn> = core
                .feedback_log
                .list(Some(&lang), 500)
                .map_err(|e| format!("反馈明细查询失败: {e}"))?
                .into_iter()
                .filter(|f| f.source == "llm")
                .map(|f| gec_core::FeedbackEntryIn {
                    source: f.source,
                    rule_id: f.rule_id,
                    error_type: f.error_type,
                    original: f.original,
                    correction: f.correction,
                    action: f.action,
                    reason: f.reason,
                    created_at: f.created_at,
                })
                .collect();
            let data = gec_core::build_llm_training_data(
                &lang,
                plan.payload
                    .get("current_arm")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
                &entries,
            );
            (
                gec_core::job_kind::LLM_DATA,
                Ok(serde_json::json!({
                    "n_samples": data.samples.len(),
                    "jsonl": data.jsonl,
                })),
            )
        }
        _ => return Err(format!("未知作业类型: {}", plan.kind)),
    };

    let job_id = training_complete_job(
        &core,
        job_kind,
        &lang,
        Some(id),
        &plan.payload,
        result.clone(),
    );
    match result {
        // 失败：作业已标 failed，建议保持 pending（可重试）
        Err(e) => Err(e),
        Ok(v) => {
            let suggestion_consumed = if job_id > 0 {
                core.reflection_center
                    .update_suggestion_status(id, "consumed")
                    .map_err(|e| format!("建议状态更新失败: {e}"))?
            } else {
                false
            };
            Ok(serde_json::json!({
                "job_id": job_id,
                "job_kind": job_kind,
                "result": v,
                "suggestion_consumed": suggestion_consumed,
            }))
        }
    }
}

/// 训练中心：作业列表（新 → 旧；lang / status 可选过滤）
#[tauri::command]
pub async fn train_list_jobs(
    core: tauri::State<'_, Arc<Core>>,
    lang: Option<String>,
    status: Option<String>,
    limit: Option<usize>,
) -> Result<serde_json::Value, String> {
    let limit = limit.unwrap_or(50).min(200);
    let jobs = core
        .training_center
        .list_jobs(lang.as_deref(), status.as_deref(), limit)
        .map_err(|e| format!("作业列表查询失败: {e}"))?;
    Ok(serde_json::json!({ "jobs": jobs }))
}

/// 训练中心：作业详情
#[tauri::command]
pub async fn train_get_job(
    core: tauri::State<'_, Arc<Core>>,
    id: i64,
) -> Result<serde_json::Value, String> {
    let job = core
        .training_center
        .get_job(id)
        .map_err(|e| format!("作业查询失败: {e}"))?
        .ok_or_else(|| format!("作业 #{id} 不存在"))?;
    Ok(serde_json::json!({ "job": job }))
}

/// RFC3339 → SQLite `datetime('now')` 格式（反馈表 created_at 用 datetime('now')，
/// 字符串比较要求同格式；轮次增量窗口边界在此归一化）
fn sqlite_dt(iso: &str) -> String {
    match iso.split_once('T') {
        Some((d, t)) => format!("{} {}", d, &t[..t.len().min(8)]),
        None => iso.to_string(),
    }
}

/// 飞轮确认池规模（`l3_data/<lang>/confirmed.jsonl` 非空行数）
fn l3_pool_size(core: &Core, lang: &str) -> i64 {
    let path = core.l3_data.join(lang).join("confirmed.jsonl");
    std::fs::read_to_string(path)
        .map(|c| c.lines().filter(|l| !l.trim().is_empty()).count() as i64)
        .unwrap_or(0)
}

/// 测评中心：层级贡献测量（消融 Δ 常态化）
///
/// 拉取测评服务报告列表 → 按（benchmark, split, arm, mode）取最新组 →
/// 各消融臂各拉一份完整报告 → 层级贡献表（每层边际贡献 = Fβ 全开 − 该层关闭）。
/// 报告过滤字段匹配：`want` 为 null 或缺失 = 不过滤（该字段恒匹配）
fn report_field_matches(
    r: &serde_json::Value,
    field: &str,
    want: Option<&serde_json::Value>,
) -> bool {
    match want {
        Some(w) if w.is_null() => true,
        Some(w) => w
            .as_str()
            .and_then(|w| r.get(field).and_then(|v| v.as_str()).map(|s| s == w))
            .unwrap_or(false),
        None => true,
    }
}

#[tauri::command]
pub async fn eval_contributions(body: serde_json::Value) -> Result<serde_json::Value, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("HTTP 客户端构建失败: {e}"))?;
    let resp = client
        .get(format!("{}/eval/reports", eval_endpoint()))
        .send()
        .await
        .map_err(|e| {
            format!("测评服务未运行（{e}；启动：cd datasets && python eval_server.py）")
        })?;
    if !resp.status().is_success() {
        return Err(format!("测评服务 /eval/reports 返回 {}", resp.status()));
    }
    let list: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("测评服务响应解析失败: {e}"))?;
    let reports = list
        .get("reports")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "测评服务响应结构不符".to_string())?;

    // 过滤 + 分组（benchmark, split, arm, mode）
    let matched: Vec<&serde_json::Value> = reports
        .iter()
        .filter(|r| {
            [
                ("benchmark", body.get("benchmark")),
                ("split", body.get("split")),
                ("arm", body.get("arm")),
                ("mode", body.get("mode")),
            ]
            .iter()
            .all(|(field, want)| report_field_matches(r, field, *want))
        })
        .collect();
    if matched.is_empty() {
        return Err("无匹配测评报告：请先在「测评」页发起一次评测（默认参数即可）；若填写了 benchmark / split / arm / mode 过滤条件，请使其与已跑评测的参数一致（留空 = 不过滤，取最新组）".to_string());
    }
    let mut groups: std::collections::HashMap<
        (String, String, String, String),
        Vec<&serde_json::Value>,
    > = std::collections::HashMap::new();
    for r in &matched {
        let key = (
            r.get("benchmark")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            r.get("split")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            r.get("arm")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            r.get("mode")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        );
        groups.entry(key).or_default().push(r);
    }
    let group_ts = |v: &Vec<&serde_json::Value>| {
        v.iter()
            .filter_map(|r| r.get("ts").and_then(|x| x.as_str()).map(|s| s.to_string()))
            .max()
            .unwrap_or_default()
    };
    let (group_key, group) = groups
        .iter()
        .max_by_key(|g| group_ts(g.1))
        .expect("groups 非空");

    // 各消融规格取组内最新一份 → 拉完整报告
    let specs: [Option<gec_core::AblationSpec>; 5] = [
        None,
        Some(gec_core::AblationSpec {
            l3_off: true,
            ..Default::default()
        }),
        Some(gec_core::AblationSpec {
            decision_off: true,
            ..Default::default()
        }),
        Some(gec_core::AblationSpec {
            knowledge_off: true,
            ..Default::default()
        }),
        Some(gec_core::AblationSpec {
            l3_off: true,
            decision_off: true,
            ..Default::default()
        }),
    ];
    let mut summaries = Vec::new();
    for spec in specs {
        let entry = group
            .iter()
            .filter(|r| {
                gec_core::AblationSpec::from_report_value(
                    r.get("ablation").unwrap_or(&serde_json::Value::Null),
                ) == spec
            })
            .max_by(|a, b| {
                a.get("ts")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .cmp(b.get("ts").and_then(|v| v.as_str()).unwrap_or(""))
            })
            .cloned();
        let Some(entry) = entry else { continue };
        let id = entry.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let full: serde_json::Value = match client
            .get(format!("{}/eval/report/{}", eval_endpoint(), id))
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => match r.json().await {
                Ok(v) => v,
                Err(_) => continue,
            },
            _ => continue,
        };
        if let Some(s) = gec_core::parse_report_summary(&full) {
            summaries.push(s);
        }
    }
    let table = gec_core::measure_contributions(&summaries);
    Ok(serde_json::json!({
        "group": {
            "benchmark": group_key.0,
            "split": group_key.1,
            "arm": group_key.2,
            "mode": group_key.3,
        },
        "table": table,
    }))
}

/// 优化轮次：自动评估 → 提案轮次（或「无需行动」）
///
/// 增量窗口 = 上次轮次（任意状态）之后；反馈统计 / 明细按窗口过滤，
/// 飞轮池规模与测评证据为绝对值。
#[tauri::command]
pub async fn round_evaluate(
    core: tauri::State<'_, Arc<Core>>,
    body: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lang = body
        .get("lang")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "lang 缺失或为空".to_string())?;
    let report_id = body
        .get("report_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // 增量窗口边界：上次轮次的创建时间
    let rounds = core
        .optimization_round
        .list_rounds(Some(lang), None, 10)
        .map_err(|e| format!("轮次存储读取失败: {e}"))?;
    let since = rounds.first().map(|r| sqlite_dt(&r.created_at));

    let stats = core
        .feedback_log
        .stats_since(lang, since.as_deref())
        .map_err(|e| format!("反馈统计失败: {e}"))?;
    let entries = core
        .feedback_log
        .list_since(Some(lang), since.as_deref(), 500)
        .map_err(|e| format!("反馈明细读取失败: {e}"))?;
    // 上次 L3 重训练时间（任意状态的最新作业）
    let jobs = core
        .training_center
        .list_jobs(Some(lang), None, 100)
        .map_err(|e| format!("训练作业读取失败: {e}"))?;
    let last_retrain_at = jobs
        .iter()
        .filter(|j| j.kind == gec_core::job_kind::L3_RETRAIN)
        .map(|j| j.updated_at.clone())
        .max();

    let input = gec_core::RoundInput {
        lang: lang.to_string(),
        feedback_stats: stats
            .into_iter()
            .map(|s| gec_core::FeedbackStatIn {
                source: s.source,
                rule_id: s.rule_id,
                adopted: s.adopted,
                rejected: s.rejected,
                last_at: s.last_at,
            })
            .collect(),
        feedback_entries: entries
            .into_iter()
            .map(|f| gec_core::FeedbackEntryIn {
                source: f.source,
                rule_id: f.rule_id,
                error_type: f.error_type,
                original: f.original,
                correction: f.correction,
                action: f.action,
                reason: f.reason,
                created_at: f.created_at,
            })
            .collect(),
        pool_size: l3_pool_size(&core, lang),
        last_retrain_at,
        // 测评证据（服务不可达 / 无报告 → None，纯反馈判定）
        eval_evidence: gec_core::fetch_eval_evidence(&eval_endpoint(), lang, report_id.as_deref())
            .await,
    };

    match gec_core::plan_round(&input) {
        None => Ok(serde_json::json!({
            "action": "none",
            "reasons": gec_core::no_action_reasons(&input),
        })),
        Some(plan) => {
            let id = core
                .optimization_round
                .insert_round(
                    lang,
                    &plan.reason,
                    &serde_json::to_value(&plan.evidence).unwrap_or_default(),
                    &serde_json::to_value(&plan.actions).unwrap_or_default(),
                    &chrono::Utc::now().to_rfc3339(),
                )
                .map_err(|e| format!("轮次落库失败: {e}"))?;
            let row = core
                .optimization_round
                .get_round(id)
                .map_err(|e| format!("轮次读取失败: {e}"))?
                .ok_or_else(|| "轮次落库后读取失败".to_string())?;
            Ok(serde_json::json!({
                "action": "round_proposed",
                "round": row,
            }))
        }
    }
}

/// 优化轮次：列表（新 → 旧；lang / status 可选过滤）
#[tauri::command]
pub async fn round_list(
    core: tauri::State<'_, Arc<Core>>,
    lang: Option<String>,
    status: Option<String>,
    limit: Option<usize>,
) -> Result<serde_json::Value, String> {
    let limit = limit.unwrap_or(50).min(200);
    let rows = core
        .optimization_round
        .list_rounds(lang.as_deref(), status.as_deref(), limit)
        .map_err(|e| format!("轮次列表查询失败: {e}"))?;
    Ok(serde_json::json!({ "rounds": rows }))
}

/// 优化轮次：详情
#[tauri::command]
pub async fn round_get(
    core: tauri::State<'_, Arc<Core>>,
    id: i64,
) -> Result<serde_json::Value, String> {
    let row = core
        .optimization_round
        .get_round(id)
        .map_err(|e| format!("轮次查询失败: {e}"))?
        .ok_or_else(|| format!("轮次 #{id} 不存在"))?;
    Ok(serde_json::json!({ "round": row }))
}

/// 优化轮次：人工确认门（`proposed` → `confirmed`）
#[tauri::command]
pub async fn round_confirm(
    core: tauri::State<'_, Arc<Core>>,
    id: i64,
) -> Result<serde_json::Value, String> {
    let row = core
        .optimization_round
        .get_round(id)
        .map_err(|e| format!("轮次查询失败: {e}"))?
        .ok_or_else(|| format!("轮次 #{id} 不存在"))?;
    if row.status != "proposed" {
        return Err(format!(
            "轮次 #{id} 当前状态为 {}，只有 proposed 可确认",
            row.status
        ));
    }
    if !core
        .optimization_round
        .transition(id, "confirmed", None, &chrono::Utc::now().to_rfc3339())
        .map_err(|e| format!("轮次状态迁移失败: {e}"))?
    {
        return Err(format!("轮次 #{id} 状态迁移失败"));
    }
    let row = core
        .optimization_round
        .get_round(id)
        .map_err(|e| format!("轮次查询失败: {e}"))?
        .ok_or_else(|| format!("轮次 #{id} 不存在"))?;
    Ok(serde_json::json!({ "round": row }))
}

/// 优化轮次：人工拒绝（`proposed` → `rejected`）
#[tauri::command]
pub async fn round_reject(
    core: tauri::State<'_, Arc<Core>>,
    id: i64,
) -> Result<serde_json::Value, String> {
    let row = core
        .optimization_round
        .get_round(id)
        .map_err(|e| format!("轮次查询失败: {e}"))?
        .ok_or_else(|| format!("轮次 #{id} 不存在"))?;
    if row.status != "proposed" {
        return Err(format!(
            "轮次 #{id} 当前状态为 {}，只有 proposed 可拒绝",
            row.status
        ));
    }
    if !core
        .optimization_round
        .transition(id, "rejected", None, &chrono::Utc::now().to_rfc3339())
        .map_err(|e| format!("轮次状态迁移失败: {e}"))?
    {
        return Err(format!("轮次 #{id} 状态迁移失败"));
    }
    let row = core
        .optimization_round
        .get_round(id)
        .map_err(|e| format!("轮次查询失败: {e}"))?
        .ok_or_else(|| format!("轮次 #{id} 不存在"))?;
    Ok(serde_json::json!({ "round": row }))
}

/// 优化轮次：执行（`confirmed` → `running` → 终态）
///
/// 每个可自动动作（`l3_retrain` / `lexicon_condition` / `llm_data`）派生一个
/// P2 训练作业（走完整状态机）；`rule_tune` 标记 `human_required`（不阻断完成）。
/// 任一自动动作失败 → 轮次 `failed`（各动作结果在 `result.actions`）。
#[tauri::command]
pub async fn round_execute(
    core: tauri::State<'_, Arc<Core>>,
    id: i64,
) -> Result<serde_json::Value, String> {
    let row = core
        .optimization_round
        .get_round(id)
        .map_err(|e| format!("轮次查询失败: {e}"))?
        .ok_or_else(|| format!("轮次 #{id} 不存在"))?;
    if row.status != "confirmed" {
        return Err(format!(
            "轮次 #{id} 当前状态为 {}，只有 confirmed 可执行（先过人工确认门）",
            row.status
        ));
    }
    if !core
        .optimization_round
        .transition(id, "running", None, &chrono::Utc::now().to_rfc3339())
        .map_err(|e| format!("轮次状态迁移失败: {e}"))?
    {
        return Err(format!("轮次 #{id} 状态迁移失败"));
    }

    let actions: Vec<gec_core::RoundAction> =
        serde_json::from_str(&row.actions.to_string()).unwrap_or_default();
    let mut action_results: Vec<serde_json::Value> = Vec::new();
    let mut any_failed = false;

    for a in &actions {
        let result: Result<serde_json::Value, String> = match a.kind.as_str() {
            gec_core::job_kind::L3_RETRAIN => l3_retrain_forward(&core, &row.lang).await,
            gec_core::job_kind::LEXICON_CONDITION => {
                let rule_id = a
                    .payload
                    .get("rule_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "payload 缺 rule_id".to_string())?;
                let words = a
                    .payload
                    .get("originals")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|w| w.as_str().map(|s| s.to_string()))
                            .collect::<Vec<_>>()
                    })
                    .ok_or_else(|| "payload 缺 originals".to_string())?;
                if words.is_empty() {
                    return Err("payload 无可入库的排除文本".to_string());
                }
                let lexicons_dir =
                    std::path::PathBuf::from(core.settings.read().unwrap().lexicons_dir.clone());
                let mut applied = Vec::new();
                let mut skipped = 0;
                for w in &words {
                    match lexicon_store::append_rule_exclude(&lexicons_dir, &row.lang, rule_id, w) {
                        Ok(true) => {
                            applied.push(serde_json::json!({ "rule_id": rule_id, "word": w }))
                        }
                        Ok(false) => skipped += 1,
                        Err(e) => return Err(format!("排除条目入库失败（{w}）: {e}")),
                    }
                }
                Ok(serde_json::json!({ "applied": applied, "skipped": skipped }))
            }
            gec_core::job_kind::LLM_DATA => {
                let entries: Vec<gec_core::FeedbackEntryIn> = core
                    .feedback_log
                    .list(Some(&row.lang), 500)
                    .map_err(|e| format!("反馈明细查询失败: {e}"))?
                    .into_iter()
                    .filter(|f| f.source == "llm")
                    .map(|f| gec_core::FeedbackEntryIn {
                        source: f.source,
                        rule_id: f.rule_id,
                        error_type: f.error_type,
                        original: f.original,
                        correction: f.correction,
                        action: f.action,
                        reason: f.reason,
                        created_at: f.created_at,
                    })
                    .collect();
                let data = gec_core::build_llm_training_data(
                    &row.lang,
                    a.payload
                        .get("current_arm")
                        .and_then(|v| v.as_str())
                        .unwrap_or(""),
                    &entries,
                );
                Ok(serde_json::json!({
                    "n_samples": data.samples.len(),
                    "jsonl": data.jsonl,
                }))
            }
            "rule_tune" => {
                // 人工操作：不产生训练作业，不阻断轮次
                action_results.push(serde_json::json!({
                    "kind": "rule_tune",
                    "target": a.target,
                    "status": "human_required",
                    "note": "请在规则面板停用 / 收紧该规则",
                }));
                continue;
            }
            _ => return Err(format!("未知动作类型: {}", a.kind)),
        };

        // 派生 P2 训练作业（proposed → confirmed → running → 终态）
        let t0 = chrono::Utc::now().to_rfc3339();
        let job_id = core
            .training_center
            .insert_job(&a.kind, &row.lang, None, &a.payload, &t0)
            .unwrap_or(0);
        if job_id > 0 {
            let _ = core.training_center.transition(
                job_id,
                "confirmed",
                None,
                &chrono::Utc::now().to_rfc3339(),
            );
            let _ = core.training_center.transition(
                job_id,
                "running",
                None,
                &chrono::Utc::now().to_rfc3339(),
            );
        }
        let (job_status, err) = match result {
            Ok(v) => {
                if job_id > 0 {
                    let _ = core.training_center.transition(
                        job_id,
                        "completed",
                        Some(&v),
                        &chrono::Utc::now().to_rfc3339(),
                    );
                }
                ("completed".to_string(), None)
            }
            Err(e) => {
                if job_id > 0 {
                    let _ = core.training_center.transition(
                        job_id,
                        "failed",
                        Some(&serde_json::json!({ "error": e })),
                        &chrono::Utc::now().to_rfc3339(),
                    );
                }
                ("failed".to_string(), Some(e))
            }
        };
        if job_status == "failed" {
            any_failed = true;
        }
        action_results.push(serde_json::json!({
            "kind": a.kind,
            "target": a.target,
            "job_id": job_id,
            "status": job_status,
            "error": err,
        }));
    }

    let (to, result) = if any_failed {
        (
            "failed",
            serde_json::json!({ "actions": action_results, "error": "部分动作失败" }),
        )
    } else {
        (
            "completed",
            serde_json::json!({ "actions": action_results }),
        )
    };
    if !core
        .optimization_round
        .transition(id, to, Some(&result), &chrono::Utc::now().to_rfc3339())
        .map_err(|e| format!("轮次状态迁移失败: {e}"))?
    {
        return Err(format!("轮次 #{id} 状态迁移失败"));
    }
    let row = core
        .optimization_round
        .get_round(id)
        .map_err(|e| format!("轮次查询失败: {e}"))?
        .ok_or_else(|| format!("轮次 #{id} 不存在"))?;
    Ok(serde_json::json!({ "round": row }))
}

/// 优化轮次：报告（聚合视图：背景 + 各动作完整作业结果 + 执行后测评证据）
///
/// 任何状态都可查看：`proposed` / `confirmed` 显示计划；`completed` / `failed`
/// 附各派生作业的完整 `result` 与人类可读 `summary`。
#[tauri::command]
pub async fn round_report(
    core: tauri::State<'_, Arc<Core>>,
    id: i64,
) -> Result<serde_json::Value, String> {
    let row = core
        .optimization_round
        .get_round(id)
        .map_err(|e| format!("轮次查询失败: {e}"))?
        .ok_or_else(|| format!("轮次 #{id} 不存在"))?;

    // 本轮派生的作业（从 result.actions 提取 job_id；未执行 → 空）
    let mut jobs: Vec<serde_json::Value> = Vec::new();
    if let Some(list) = row
        .result
        .as_ref()
        .and_then(|v| v.get("actions"))
        .and_then(|v| v.as_array())
    {
        for a in list {
            if let Some(jid) = a.get("job_id").and_then(|v| v.as_i64()) {
                if let Some(job) = core
                    .training_center
                    .get_job(jid)
                    .map_err(|e| format!("作业查询失败: {e}"))?
                {
                    jobs.push(serde_json::to_value(job).unwrap_or_default());
                }
            }
        }
    }

    // 执行后测评证据（最新报告；不可达 → None）
    let eval_after = gec_core::fetch_eval_evidence(&eval_endpoint(), &row.lang, None).await;

    let report = gec_core::round_report(
        &serde_json::to_value(&row).unwrap_or_default(),
        &jobs,
        eval_after.as_ref(),
    );
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::report_field_matches;
    use serde_json::json;

    /// 回归：前端选「split 为 null 的报告组」时会显式传 split: null，
    /// 旧实现把 Some(Null) 当「必须匹配字符串」→ 恒 false → 误报「无匹配测评报告」
    #[test]
    fn null_want_means_no_filter() {
        let r =
            json!({ "benchmark": "mucgec", "split": null, "arm": "cec3", "mode": "capability" });
        assert!(report_field_matches(&r, "split", Some(&json!(null))));
        assert!(report_field_matches(&r, "split", None));
        assert!(report_field_matches(&r, "absent", Some(&json!(null))));
    }

    #[test]
    fn string_want_matches_exactly() {
        let r = json!({ "benchmark": "mucgec", "split": "dev", "arm": "cec3", "mode": "funnel" });
        assert!(report_field_matches(
            &r,
            "benchmark",
            Some(&json!("mucgec"))
        ));
        assert!(!report_field_matches(
            &r,
            "benchmark",
            Some(&json!("nacgec"))
        ));
        assert!(!report_field_matches(
            &r,
            "mode",
            Some(&json!("capability"))
        ));
        // 报告字段为 null 而 want 为具体值 → 不匹配
        let r2 = json!({ "split": null });
        assert!(!report_field_matches(&r2, "split", Some(&json!("dev"))));
    }
}
