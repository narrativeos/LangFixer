use std::path::Path;
use std::sync::{Arc, RwLock};

use attribution_engine::{KnowledgeStore, StagingStore, TableStore};
use gec_core::{
    EmbeddingEngine, LanguageRouter, LlmDetectConfig, LlmGecEngine, ModelRegistry, compose_detector,
};
use gec_en::{DetectorConfig as EnConfig, EnglishDetector};
use gec_ja::{DetectorConfig as JaConfig, JapaneseDetector};
use gec_zh::{ChineseDetector, DetectorConfig as ZhConfig};
use langfixer_llm::LlmService;
use langfixer_llm::templates::TemplateStore;
use lexicon_store::{
    FeedbackLogStore, KnowledgeStore as LearnedKnowledgeStore, LexiconStore,
    OptimizationRoundStore, ReflectionCenterStore, TrainingCenterStore,
};
use shared_model::SessionStore;

use crate::cache::LruCache;
use crate::custom_words::CustomWordStore;
use crate::detect_history::DetectHistoryStore;
use crate::settings::AppSettings;
use crate::table_history::TableHistoryStore;

/// 应用状态：语言路由 + 决策表存储 + 会话存储 + 运行时设置（HanLP 地址 / LLM 参数）
pub struct AppState {
    /// 语言路由（`RwLock`：HanLP 地址变更时热重建 zh/en 检测器）
    pub router: RwLock<LanguageRouter>,
    pub tables: TableStore,
    /// 知识库（知识源注册表 + 知识点集合；与决策表分离，不随产品内置发布）
    pub knowledge: KnowledgeStore,
    /// 知识源导入草稿区（书 → agent 挖掘 → 草稿 → 人工一键发布；
    /// 与生效知识库分离，发布前不影响运行时归因；持久化于知识库目录 staging/）
    pub ingest: Arc<StagingStore>,
    /// 草稿持久化目录（`{GEC_KNOWLEDGE_DIR}/staging`）
    pub ingest_dir: std::path::PathBuf,
    /// 知识库目录（`GEC_KNOWLEDGE_DIR`；发布/热更新写回此目录，跨重启存活）
    pub knowledge_dir: std::path::PathBuf,
    pub sessions: SessionStore,
    /// 运行时设置（设置界面可改；持久化到 `GEC_SETTINGS_FILE`）
    pub settings: RwLock<AppSettings>,
    /// 设置持久化文件路径（构建时捕获，避免并行进程间环境变量互相干扰）
    pub settings_path: std::path::PathBuf,
    /// LLM 提示词模板库（启动时从磁盘加载；`prompts_dir` 变更时热重载）
    pub llm_templates: RwLock<TemplateStore>,
    /// 纠错模型注册表（`prompts/models.json`；决定 LLM 兜底层输出适配 json/plain；
    /// `prompts_dir` 变更时热重载；设置界面「纠错模型」选择器数据源）
    pub model_registry: RwLock<ModelRegistry>,
    /// zh/en/ja 检测器启动时基础配置（HanLP 地址 / 离线开关 / 词表目录变更时据此重建）
    zh_base_cfg: ZhConfig,
    en_base_cfg: EnConfig,
    ja_base_cfg: JaConfig,
    /// 决策表热更新 token（`GEC_TABLES_UPDATE_TOKEN`；未设置时 PUT /gec/tables/:lang 不设鉴权）
    pub tables_update_token: Option<String>,
    /// 规则词表存储（SQLite：规则定义 + 示例 + 运行时开关；
    /// 启动时从词表目录播种，旧版 JSON 开关状态一次性迁移后归档）
    pub lexicon_store: Arc<LexiconStore>,
    /// LRU 检测缓存
    pub cache: LruCache,
    /// 用户自定义词（SQLite 持久化）
    pub custom_words: CustomWordStore,
    /// 检测历史（SQLite 持久化，全部留存）
    pub detect_history: DetectHistoryStore,
    /// 多模态审校 job 存储（内存表 + `{workdir}/{job_id}/` 目录；
    /// 原件只读副本隔离，见 `media_flow::jobs` 与 `MULTIMODAL.md` §9）
    /// `Arc` 是为了把同一份 store 交给 `media_flow::FlowCtx`（编排层与路由共享状态）
    pub media_jobs: Arc<RwLock<media_flow::MediaJobStore>>,
    /// 生效词表文件哈希（启动 + 检测器热重建时重算；规则侧版本指纹输入——
    /// 检测器只在构造时读词表文件，故「生效词表」由最近一次构建决定，不每次读盘）
    pub lexicon_hash: RwLock<String>,
    /// 决策表版本历史（SQLite 持久化，支持回滚）
    pub table_history: TableHistoryStore,
    /// 学习知识存储（向量召回用；SQLite 持久化，与 lexicon_store 共用同一 db 文件）
    pub learned_knowledge: Arc<LearnedKnowledgeStore>,
    /// 嵌入引擎（调用 LLM /v1/embeddings 端点；未配置时为 None；
    /// `RwLock`：设置界面变更嵌入配置时热更新）
    pub embedding_engine: RwLock<Option<Arc<EmbeddingEngine>>>,
    /// 反馈日志（SQLite：检测结果采纳/放弃留痕 + 规则反馈统计；反思流程数据基础）
    pub feedback_log: Arc<FeedbackLogStore>,
    /// 反思中心（SQLite：反思报告 + 建议队列；P1 进化闭环「反思」环节，
    /// 与反馈日志共用同一 db 文件）
    pub reflection_center: Arc<ReflectionCenterStore>,
    /// 训练中心（SQLite：训练作业 + 状态机；P2 进化闭环「训练」环节，
    /// 消费反思建议队列；与反馈日志 / 反思中心共用同一 db 文件）
    pub training_center: Arc<TrainingCenterStore>,
    /// 优化轮次（SQLite：轮次提案 + 人工确认门 + 执行结果；P3 进化闭环
    /// 「贡献与轮次」环节；与反馈日志 / 反思中心 / 训练作业共用同一 db 文件）
    pub optimization_round: Arc<OptimizationRoundStore>,
    /// 决策表目录（资源包导入/删除写回决策表与 aux 数据文件；
    /// `RwLock`：设置界面变更 `tables_dir` 时同步更新）
    pub tables_dir: RwLock<std::path::PathBuf>,
}

impl AppState {
    /// 构建应用状态
    ///
    /// - `tables_dir`：决策表目录（`None` = 运行时设置 / 环境变量）
    /// - `db_file`：SQLite 存储文件路径（`None` = `GEC_DB_FILE` 环境变量 > 默认 `langfixer.db`；
    ///   测试用显式传参避免进程级环境变量的并行竞态）
    pub fn from_env(tables_dir: Option<&Path>, db_file: Option<&Path>) -> Self {
        // 运行时设置：设置文件（GEC_SETTINGS_FILE）> 环境变量 > 内置默认
        let settings_path = std::path::PathBuf::from(
            std::env::var("GEC_SETTINGS_FILE")
                .unwrap_or_else(|_| "langfixer-settings.json".to_string()),
        );
        let settings = AppSettings::load(&settings_path);

        // 多模态 job 根目录：设置 media.workdir > 相对当前目录的 media-jobs/
        let media_root = if settings.media.workdir.is_empty() {
            std::path::PathBuf::from("media-jobs")
        } else {
            std::path::PathBuf::from(&settings.media.workdir)
        };

        // 决策表目录：显式指定（测试用）> 运行时设置（设置界面可改，持久化）
        let tables_dir = tables_dir
            .map(Path::to_path_buf)
            .unwrap_or_else(|| std::path::PathBuf::from(settings.tables_dir.clone()));

        // ── 先创建知识存储 + 嵌入引擎（router 注册时需要）──
        let db_path = std::path::PathBuf::from(
            db_file
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| {
                    std::env::var("GEC_DB_FILE").unwrap_or_else(|_| "langfixer.db".to_string())
                }),
        );
        let learned_knowledge = match LearnedKnowledgeStore::open(&db_path) {
            Ok(store) => {
                tracing::info!("学习知识存储已初始化: {:?}", db_path);
                Arc::new(store)
            }
            Err(e) => {
                tracing::error!(error = %e, "学习知识存储初始化失败，将使用空存储");
                let tmp = std::env::temp_dir().join("langfixer_knowledge.db");
                Arc::new(LearnedKnowledgeStore::open(&tmp).expect("无法打开临时知识 SQLite"))
            }
        };
        let embedding_engine = if settings.embedding.is_configured() {
            tracing::info!(
                endpoint = %settings.embedding.endpoint,
                model = %settings.embedding.model,
                "嵌入引擎已启用（向量召回可用）"
            );
            Some(Arc::new(EmbeddingEngine::new(settings.embedding.clone())))
        } else {
            tracing::debug!("嵌入引擎未配置（EMBEDDING_ENDPOINT 为空），向量召回禁用");
            None
        };
        // 组合知识召回实现（嵌入引擎未配置时为 None；知识层开关关闭时同样不接线）
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
                tracing::info!("知识层开关关闭（knowledge_enabled=false），向量召回不接线");
                None
            };

        // ── 注册检测器（传入知识召回）──
        let mut router = LanguageRouter::new();
        let zh_cfg = ZhConfig {
            endpoint: settings.hanlp_endpoint.clone(),
            offline_only: settings.offline_mode,
            lexicons_dir: std::path::PathBuf::from(settings.lexicons_dir.clone()),
            ..ZhConfig::from_env()
        };
        let en_cfg = EnConfig {
            endpoint: settings.hanlp_endpoint.clone(),
            offline_only: settings.offline_mode,
            lexicons_dir: std::path::PathBuf::from(settings.lexicons_dir.clone()),
            ..EnConfig::from_env()
        };
        let ja_cfg = JaConfig {
            offline_only: settings.offline_mode,
            lexicons_dir: std::path::PathBuf::from(settings.lexicons_dir.clone()),
            ..JaConfig::from_env()
        };
        router.register(compose_detector(
            "zh",
            Arc::new(ChineseDetector::new(zh_cfg.clone())),
            Some(settings.decision.clone()),
            knowledge_recall.clone(),
            Some(settings.l3.clone()),
        ));
        router.register(compose_detector(
            "en",
            Arc::new(EnglishDetector::new(en_cfg.clone())),
            Some(settings.decision.clone()),
            knowledge_recall.clone(),
            Some(settings.l3.clone()),
        ));
        router.register(compose_detector(
            "ja",
            Arc::new(JapaneseDetector::new(ja_cfg.clone())),
            Some(settings.decision.clone()),
            knowledge_recall,
            Some(settings.l3.clone()),
        ));
        tracing::info!(
            languages = ?router.supported_languages(),
            hanlp = %settings.hanlp_endpoint,
            offline = settings.offline_mode,
            tables = %tables_dir.display(),
            "检测器已注册"
        );

        let tables = TableStore::new();
        match tables.load_from_dir(&tables_dir) {
            Ok(n) => tracing::info!(count = n, "决策表加载完成"),
            Err(e) => tracing::error!(error = %e, "决策表加载失败"),
        }

        // 知识库：独立于决策表（GEC_KNOWLEDGE_DIR；目录缺失时优雅降级为纯表级归因）
        let knowledge = KnowledgeStore::new();
        let kb_dir_str =
            std::env::var("GEC_KNOWLEDGE_DIR").unwrap_or_else(|_| "knowledge-base".to_string());
        let kb_dir = std::path::PathBuf::from(&kb_dir_str);
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

        let llm_templates = RwLock::new(TemplateStore::load(&settings.llm.prompts_dir));
        let model_registry = RwLock::new(ModelRegistry::load(
            &settings.llm.prompts_dir.join("models.json"),
        ));
        if settings.llm.enabled() {
            tracing::info!(
                model = %settings.llm.model,
                templates = llm_templates.read().unwrap().len(),
                "LLM 讲解服务已启用"
            );
        } else {
            tracing::debug!("LLM 讲解服务未启用（未配置 LLM_API_KEY），将使用静态建议");
        }

        // 初始化自定义词存储（SQLite，与 learned_knowledge 共用同一 db 文件）
        let custom_words = match CustomWordStore::open(&db_path) {
            Ok(store) => {
                tracing::info!("自定义词存储已初始化: {:?}", db_path);
                store
            }
            Err(e) => {
                tracing::error!(error = %e, "自定义词存储初始化失败，将使用空存储");
                // 回退：使用内存模式（临时目录）
                let tmp = std::env::temp_dir().join("langfixer_test.db");
                CustomWordStore::open(&tmp).expect("无法打开临时 SQLite")
            }
        };

        // 初始化检测历史存储（SQLite，与 custom_words 共用同一个 db 文件）
        let detect_history = match DetectHistoryStore::open(&db_path) {
            Ok(store) => {
                tracing::info!("检测历史存储已初始化: {:?}", db_path);
                store
            }
            Err(e) => {
                tracing::error!(error = %e, "检测历史存储初始化失败，将使用空存储");
                let tmp = std::env::temp_dir().join("langfixer_history.db");
                DetectHistoryStore::open(&tmp).expect("无法打开临时 SQLite")
            }
        };

        // 初始化决策表版本历史存储（SQLite，与 detect_history 共用同一个 db 文件）
        let table_history = match TableHistoryStore::open(&db_path) {
            Ok(store) => {
                tracing::info!("决策表版本历史存储已初始化: {:?}", db_path);
                store
            }
            Err(e) => {
                tracing::error!(error = %e, "决策表版本历史存储初始化失败，将使用空存储");
                let tmp = std::env::temp_dir().join("langfixer_table_versions.db");
                TableHistoryStore::open(&tmp).expect("无法打开临时 SQLite")
            }
        };

        // 规则词表存储（SQLite，与其他存储共用同一个 db 文件）：
        // 1. 打开库并从词表目录播种（已存在的规则保留运行时 enabled/source 状态）
        // 2. 一次性迁移旧版 JSON 开关状态（GEC_RULE_STATE_FILE），迁移后归档为 .migrated
        let lexicons_dir = std::path::PathBuf::from(settings.lexicons_dir.clone());
        // 生效词表文件哈希（规则侧版本指纹输入；检测器热重建时同步刷新）
        let lexicon_hash = shared_model::lexicon_files_hash(&lexicons_dir);
        let old_state_path = std::path::PathBuf::from(
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
                tracing::error!(error = %e, "规则词表存储初始化失败，将使用空存储");
                let tmp = std::env::temp_dir().join("langfixer_lexicon.db");
                Arc::new(LexiconStore::open(&tmp, None).expect("无法打开临时 SQLite 词表库"))
            }
        };

        // 反馈日志存储（SQLite，与词表 / 学习知识共用同一 db 文件）
        let feedback_log = match FeedbackLogStore::open(&db_path) {
            Ok(store) => Arc::new(store),
            Err(e) => {
                tracing::error!(error = %e, "反馈日志存储初始化失败，将使用空存储");
                let tmp = std::env::temp_dir().join("langfixer_feedback.db");
                Arc::new(FeedbackLogStore::open(&tmp).expect("无法打开临时反馈日志 SQLite"))
            }
        };

        // 反思中心存储（SQLite：反思报告 + 建议队列；与反馈日志共用同一 db 文件）
        let reflection_center = match ReflectionCenterStore::open(&db_path) {
            Ok(store) => Arc::new(store),
            Err(e) => {
                tracing::error!(error = %e, "反思中心存储初始化失败，将使用空存储");
                let tmp = std::env::temp_dir().join("langfixer_reflection.db");
                Arc::new(ReflectionCenterStore::open(&tmp).expect("无法打开临时反思中心 SQLite"))
            }
        };

        // 训练中心存储（SQLite：训练作业 + 状态机；P2 消费反思建议队列）
        let training_center = match TrainingCenterStore::open(&db_path) {
            Ok(store) => Arc::new(store),
            Err(e) => {
                tracing::error!(error = %e, "训练中心存储初始化失败，将使用空存储");
                let tmp = std::env::temp_dir().join("langfixer_training.db");
                Arc::new(TrainingCenterStore::open(&tmp).expect("无法打开临时训练中心 SQLite"))
            }
        };

        // 优化轮次存储（SQLite：轮次提案 + 人工确认门；P3 闭环决策环节）
        let optimization_round = match OptimizationRoundStore::open(&db_path) {
            Ok(store) => Arc::new(store),
            Err(e) => {
                tracing::error!(error = %e, "优化轮次存储初始化失败，将使用空存储");
                let tmp = std::env::temp_dir().join("langfixer_round.db");
                Arc::new(OptimizationRoundStore::open(&tmp).expect("无法打开临时优化轮次 SQLite"))
            }
        };

        Self {
            router: RwLock::new(router),
            tables,
            knowledge,
            ingest,
            ingest_dir,
            knowledge_dir: kb_dir,
            tables_dir: RwLock::new(tables_dir.clone()),
            sessions: SessionStore::new(),
            settings: RwLock::new(settings),
            media_jobs: Arc::new(RwLock::new(media_flow::MediaJobStore::new(&media_root))),
            settings_path,
            llm_templates,
            model_registry,
            zh_base_cfg: zh_cfg,
            en_base_cfg: en_cfg,
            ja_base_cfg: ja_cfg,
            tables_update_token: std::env::var("GEC_TABLES_UPDATE_TOKEN")
                .ok()
                .filter(|v| !v.is_empty()),
            lexicon_store,
            cache: LruCache::default(),
            custom_words,
            detect_history,
            lexicon_hash: RwLock::new(lexicon_hash),
            table_history,
            learned_knowledge,
            embedding_engine: RwLock::new(embedding_engine),
            feedback_log,
            reflection_center,
            training_center,
            optimization_round,
        }
    }

    /// 当前 LLM 服务快照（基于最新运行时设置；模板库随 `prompts_dir` 变更热重载）
    pub fn llm_service(&self) -> LlmService {
        let s = self.settings.read().unwrap();
        LlmService {
            config: s.llm.clone(),
            templates: self.llm_templates.read().unwrap().clone(),
        }
    }

    /// LLM 服务是否可用（已配置 API Key 且模板库非空）
    pub fn is_llm_available(&self) -> bool {
        let s = self.settings.read().unwrap();
        s.llm.enabled() && !self.llm_templates.read().unwrap().is_empty()
    }

    /// 构建指定语言的 LLM 兜底检测引擎（三级响应 level 2/3 的第三层）
    ///
    /// 从运行时 LLM 参数（设置文件 > 环境变量）+ 检测模板
    /// （预设 `prompt_template` > `{lang}_detect` 提示词）现构建，支持设置热更新；
    /// LLM 未配置（无 API Key）或缺检测模板时返回 `None`
    /// （API 层对 level 2/3 请求返回 503 / 桌面端报错）。
    pub fn llm_detect_engine(&self, lang: &str) -> Option<Arc<LlmGecEngine>> {
        let service = self.llm_service();
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
    /// 决策模型版本号来自决策服务 `GET /health`（引擎内短 TTL 缓存，到期重查）；
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
        // 决策模型版本号：仅决策层启用时查询（读锁内只克隆引擎 Arc，锁立即释放，
        // 避免 guard 跨 await）
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

    /// 应用新设置（设置界面 `PUT /gec/settings`）：
    /// - HanLP 地址 / 离线开关 / 词表目录 / 决策模型层变更 → 热重建 zh/en/ja 检测器
    ///   （保留启动时引擎类型；离线开关由设置界面显式控制，不再隐式随地址变更）
    /// - LLM 参数变更 → 后续讲解请求立即使用新参数；`prompts_dir` 变更 → 热重载模板库
    /// - 决策表目录变更 → 从新目录重新加载决策表
    /// - 更新内存设置并持久化到设置文件
    /// 按给定设置重建 zh/en/ja 检测器（在现有路由上重新注册；guard 模式，
    /// 避免二次写锁死锁）
    ///
    /// 检测代码只在检测器**构造时**从词表文件加载规则——新采纳的规则
    /// 写回词表文件后需要重建检测器才能被检出（`llm_feedback` 采纳路径使用）。
    pub fn rebuild_router(&self, next: &AppSettings) {
        // 构建知识召回（嵌入引擎配置可能随设置变更；知识层开关关闭时不接线——测评消融臂）
        let recall: Option<Arc<dyn gec_core::KnowledgeRecall>> =
            if next.embedding.is_configured() && next.knowledge_enabled {
                let eng = Arc::new(EmbeddingEngine::new(next.embedding.clone()));
                let r: Arc<dyn gec_core::KnowledgeRecall> =
                    Arc::new(crate::knowledge_recall::KnowledgeRecallImpl::new(
                        self.learned_knowledge.clone(),
                        eng,
                    ));
                Some(r)
            } else {
                None
            };
        let mut router = self.router.write().unwrap();
        router.register(compose_detector(
            "zh",
            Arc::new(ChineseDetector::new(ZhConfig {
                endpoint: next.hanlp_endpoint.clone(),
                offline_only: next.offline_mode,
                lexicons_dir: std::path::PathBuf::from(next.lexicons_dir.clone()),
                ..self.zh_base_cfg.clone()
            })),
            Some(next.decision.clone()),
            recall.clone(),
            Some(next.l3.clone()),
        ));
        router.register(compose_detector(
            "en",
            Arc::new(EnglishDetector::new(EnConfig {
                endpoint: next.hanlp_endpoint.clone(),
                offline_only: next.offline_mode,
                lexicons_dir: std::path::PathBuf::from(next.lexicons_dir.clone()),
                ..self.en_base_cfg.clone()
            })),
            Some(next.decision.clone()),
            recall.clone(),
            Some(next.l3.clone()),
        ));
        router.register(compose_detector(
            "ja",
            Arc::new(JapaneseDetector::new(JaConfig {
                offline_only: next.offline_mode,
                lexicons_dir: std::path::PathBuf::from(next.lexicons_dir.clone()),
                ..self.ja_base_cfg.clone()
            })),
            Some(next.decision.clone()),
            recall,
            Some(next.l3.clone()),
        ));
        drop(router);
        // 检测器重建 ⇒ 生效词表变化 ⇒ 刷新词表文件哈希（规则侧版本指纹输入）
        *self.lexicon_hash.write().unwrap() =
            shared_model::lexicon_files_hash(std::path::Path::new(&next.lexicons_dir));
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
                    Some(Arc::new(EmbeddingEngine::new(next.embedding.clone())))
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
                        tokio::spawn(async move {
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
        if old.llm.prompts_dir != next.llm.prompts_dir {
            let store = TemplateStore::load(&next.llm.prompts_dir);
            let registry = ModelRegistry::load(&next.llm.prompts_dir.join("models.json"));
            tracing::info!(
                dir = %next.llm.prompts_dir.display(),
                templates = store.len(),
                models = registry.models.len(),
                "LLM 提示词模板目录变更，模板库 + 模型注册表已重载（即时生效）"
            );
            *self.llm_templates.write().unwrap() = store;
            *self.model_registry.write().unwrap() = registry;
        }
        if old.tables_dir != next.tables_dir {
            let dir = std::path::Path::new(&next.tables_dir);
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
            *self.tables_dir.write().unwrap() = dir.to_path_buf();
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

#[cfg(test)]
mod send_check_tests {
    use super::*;

    #[test]
    fn current_detect_params_future_is_send() {
        fn assert_send<T: Send>(t: &T) {
            let _ = t;
        }
        let dir = std::env::temp_dir().join(format!("sendcheck_{}", uuid::Uuid::new_v4().simple()));
        let _ = std::fs::create_dir_all(&dir);
        let db = dir.join("t.db");
        let state = AppState::from_env(Some(&dir), Some(&db));
        assert_send(&state.current_detect_params("zh", 1, false, &None));
    }

    /// 回归测试：与 run_detection 相同的嵌套调用链（Arc + as_ref + 嵌套 await）
    /// 的 future 必须 Send（std RwLock guard 跨 await 会使 future 非 Send）
    #[test]
    fn nested_chain_is_send() {
        fn assert_send<T: Send>(t: &T) {
            let _ = t;
        }
        let dir = std::env::temp_dir().join(format!("sendcheck_{}", uuid::Uuid::new_v4().simple()));
        let _ = std::fs::create_dir_all(&dir);
        let db = dir.join("t.db");
        let state: std::sync::Arc<AppState> =
            std::sync::Arc::new(AppState::from_env(Some(&dir), Some(&db)));
        let lang = "zh";
        let text = "t";
        let category: Option<String> = None;
        assert_send(&async {
            let _ =
                crate::routes::detect::try_reuse_history(state.as_ref(), lang, text, &category, 1)
                    .await;
        });
    }
}
