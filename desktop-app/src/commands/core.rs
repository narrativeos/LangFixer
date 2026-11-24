use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::RwLock;

use attribution_engine::{KnowledgeStore, StagingStore, TableStore};
use gec_core::{LanguageRouter, LlmDetectConfig, LlmGecEngine, ModelRegistry, compose_detector};
use gec_en::{DetectorConfig as EnConfig, EnglishDetector};
use gec_ja::{DetectorConfig as JaConfig, JapaneseDetector};
use gec_zh::{ChineseDetector, DetectorConfig as ZhConfig};
use langfixer_llm::LlmService;
use langfixer_llm::templates::TemplateStore;
use lexicon_store::LexiconStore;

use crate::settings::AppSettings;
use shared_model::session_store::SqliteSessionStore;

use super::custom_words::CustomWordStore;
use super::history::DetectHistoryStore;
use super::util::llm_breaker;
/// 程序版本号（crate 版本；写入检测历史，便于跨版本对比改进 / 按版本筛选训练数据）
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

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
    /// 会话 + 学习者画像存储（SQLite 持久化：母语 / 自评 CEFR 跨重启存活，与 api-server 同构）
    pub sessions: SqliteSessionStore,
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

        // 会话 + 学习者画像存储（SQLite，与上面共用同一个 db 文件；与 api-server 同构）
        let sessions = match SqliteSessionStore::open(&db_path) {
            Ok(store) => {
                tracing::info!(?db_path, "会话存储已初始化");
                store
            }
            Err(e) => {
                tracing::error!(error = %e, "会话存储初始化失败，将使用临时 SQLite");
                let tmp = std::env::temp_dir().join("langfixer_desktop_sessions.db");
                SqliteSessionStore::open(&tmp).expect("无法打开临时 SQLite")
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
            sessions,
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
        Some(Arc::new(LlmGecEngine::with_breaker(
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
            llm_breaker(),
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
            splitter: shared_model::SPLITTER_VERSION.into(),
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
    ///
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
