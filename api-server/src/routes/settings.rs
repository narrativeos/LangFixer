//! 设置界面 API
//!
//! - `GET /gec/settings` — 当前设置（HanLP 地址 + 离线开关 + 目录配置 + LLM 参数 + LLM 请求模式）
//! - `PUT /gec/settings` — 更新设置（部分更新；即时生效，无需重启；持久化到 JSON 文件）
//! - `POST /gec/settings/test` — 测试指定配置项的服务连通性（`target` = hanlp / llm / decision /
//!   l3 / embedding / tables_dir / lexicons_dir / all；只读探测，不修改配置）
//!
//! LLM 请求模式为固定默认：前端请求用流式（`POST /gec/explain/stream`），
//! 外部 API 调用用非流式（`POST /gec/explain`）。

use std::sync::Arc;

use axum::Json;
use axum::extract::{Extension, State};
use serde::{Deserialize, Serialize};

use crate::auth::{AuthContext, GatewayConfig};
use crate::routes::ApiError;
use crate::settings::AppSettings;
use crate::state::AppState;

/// LLM 请求模式（固定默认：前端用流式，外部 API 用非流式）
#[derive(Debug, Clone, Copy, Serialize)]
pub struct LlmModes {
    /// 前端（Web UI）讲解请求默认流式（SSE，`POST /gec/explain/stream`）
    pub frontend: &'static str,
    /// 外部 API 调用默认非流式（`POST /gec/explain`）
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

/// 决策模型（vegaml）层参数视图（GET 响应）
#[derive(Serialize)]
pub struct DecisionSettingsView {
    /// 决策模型服务地址（等价环境变量 `GEC_DEC_ENDPOINT`）
    pub endpoint: String,
    /// 模型名（微调后为 checkpoint 名）
    pub model: String,
    /// 请求超时（秒）
    pub timeout_secs: u64,
    /// 弃权阈值（conformal 校准后概率低于该值不产出错误）
    pub abstain_threshold: f64,
    /// 决策层开关（`true` 时 `rule` 引擎组合中挂载决策模型层）
    pub enabled: bool,
    /// 配置生效来源（审计 F7）：`settings_file`（设置文件）| `env`（环境变量
    /// GEC_DEC_*）| `default`（内置默认）；优先级：设置文件 > 环境变量 > 默认
    pub config_source: &'static str,
}

/// 决策模型（vegaml）层参数更新（字段均可缺省 = 保持不变）
#[derive(Deserialize)]
pub struct UpdateDecisionRequest {
    pub endpoint: Option<String>,
    pub model: Option<String>,
    pub timeout_secs: Option<u64>,
    pub abstain_threshold: Option<f64>,
    pub enabled: Option<bool>,
}

/// L3 特征模型层参数视图（GET 响应）
#[derive(Serialize)]
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
#[derive(Deserialize)]
pub struct UpdateL3Request {
    pub endpoint: Option<String>,
    pub model: Option<String>,
    pub timeout_secs: Option<u64>,
    pub abstain_threshold: Option<f64>,
    pub enabled: Option<bool>,
}

/// 嵌入引擎（向量召回）参数视图（GET 响应）
#[derive(Serialize)]
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
#[derive(Deserialize)]
pub struct UpdateEmbeddingRequest {
    pub endpoint: Option<String>,
    pub api_key: Option<String>,
    pub model: Option<String>,
    pub timeout_secs: Option<u64>,
}

/// LLM 参数视图（GET 响应）
#[derive(Serialize)]
pub struct LlmSettingsView {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub timeout_secs: u64,
    /// 提示词模板目录（等价环境变量 `LLM_PROMPTS_DIR`）
    pub prompts_dir: String,
    /// 思考模式开关（false = 缺省，检测链路请求显式关闭服务端思考）
    pub thinking: bool,
    /// 是否已启用（API Key 非空）
    pub enabled: bool,
}

/// 纠错模型预设视图（GET 响应；设置界面「纠错模型」选择器列表）
#[derive(Serialize)]
pub struct ModelView {
    pub id: String,
    pub name: String,
    /// 输出格式（`json` / `plain`）
    pub output_format: String,
    pub enabled: bool,
}

/// GET /gec/settings 响应
#[derive(Serialize)]
pub struct SettingsResponse {
    pub hanlp_endpoint: String,
    pub llm: LlmSettingsView,
    pub llm_modes: LlmModes,
    /// 离线模式（等价环境变量 `GEC_OFFLINE=1`）：检测器仅用本地规则，跳过远端 NLP 服务
    pub offline_mode: bool,
    /// 决策表目录（等价环境变量 `GEC_TABLES_DIR`）
    pub tables_dir: String,
    /// 规则词表目录（等价环境变量 `GEC_LEXICONS_DIR`；zh/en/ja 共用）
    pub lexicons_dir: String,
    /// 决策模型层参数（**默认开启**：zh v2 trackb 门控达标后重新接入
    /// 产品路径，EVOLUTION.md #11.19 补五；设置界面「决策模型」卡片可开关）
    pub decision: DecisionSettingsView,
    /// L3 特征模型层参数（三期：level 3 先跑特征模型，LLM 收缩到纯长尾）
    pub l3: L3SettingsView,
    /// 嵌入引擎（向量召回）参数（未配置时知识召回静默禁用）
    pub embedding: EmbeddingSettingsView,
    /// 当前选中的纠错模型预设 id（`prompts/models.json` 注册表；缺省 `generic`）
    pub llm_model_id: String,
    /// ③ 自动双判开关（影子臂并行判 + dual_judge_log 落盘）
    pub llm_dual_judge: bool,
    /// 双判影子臂预设 id（旧配置兼容：显式端点字段为空时按此预设解析；缺省 `arm3`）
    pub llm_dual_model_id: String,
    /// 双判影子臂端点 Base URL（展示的是生效值：显式配置或注册表预设解析结果；
    /// 两者皆空 = 双判会跳过）
    pub llm_dual_base_url: String,
    /// 双判影子臂模型名（同上）
    pub llm_dual_model: String,
    /// 双判影子臂 API Key（空 = 运行时复用主判密钥）
    pub llm_dual_api_key: String,
    /// 知识层总开关（知识向量召回 + 知识库富化 + 数据驱动归点；默认开）
    pub knowledge_enabled: bool,
    /// 载体（图片 / PDF / Word）审校设置（与桌面端同一份定义：`media_flow::settings`）
    pub media: media_flow::settings::MediaSettings,
    /// 文档级审校作业设置（块大小 / 并发 / 缓存 / 默认预算）
    pub doc_jobs: crate::settings::DocJobSettings,
    /// 生产库每日备份：enabled 是设置愿望，installed 是 launchd 事实（仅 macOS 可装）；
    /// 两态不一致时面板如实呈现，重开开关即可修复
    pub db_backup: DbBackupView,
    /// 可用纠错模型预设列表（选择器；仅含 enabled 条目）
    pub models: Vec<ModelView>,
}

/// PUT /gec/settings 请求体（部分更新：仅提供要修改的字段）
#[derive(Deserialize)]
pub struct UpdateSettingsRequest {
    /// HanLP 服务地址
    pub hanlp_endpoint: Option<String>,
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
    /// 当前选中的纠错模型预设 id（`prompts/models.json` 注册表）
    pub llm_model_id: Option<String>,
    /// ③ 自动双判开关
    pub llm_dual_judge: Option<bool>,
    /// 双判影子臂预设 id（旧配置兼容）
    pub llm_dual_model_id: Option<String>,
    /// 双判影子臂独立端点 Base URL（与影子模型成对；两者留空 = 沿用预设）
    pub llm_dual_base_url: Option<String>,
    /// 双判影子臂独立模型名
    pub llm_dual_model: Option<String>,
    /// 双判影子臂独立 API Key（留空 = 复用主判密钥）
    pub llm_dual_api_key: Option<String>,
    /// 知识层总开关（测评消融臂 `knowledge: false` 用；默认开）
    pub knowledge_enabled: Option<bool>,
    /// 载体（图片 / PDF / Word）审校设置（部分更新；两端同构）
    pub media: Option<UpdateMediaRequest>,
    /// 文档级审校作业设置（部分更新）
    pub doc_jobs: Option<UpdateDocJobRequest>,
    /// 生产库每日备份开关：Some(true) 注册 launchd 任务，Some(false) 撤销；
    /// 注册失败直接报错不落盘（设置不会说谎）
    pub db_backup_enabled: Option<bool>,
}

/// 生产库每日备份的设置态 + 实存态视图
#[derive(Serialize)]
pub struct DbBackupView {
    /// 设置愿望原值（开关真实位置；与 installed 分开，任务缺失时不说谎）
    pub setting: bool,
    /// 真实可用态 = setting ∧ installed
    pub enabled: bool,
    /// launchd 实存态（plist 在且已加载）
    pub installed: bool,
    /// 备份目录（任务烘焙的绝对路径；供面板透明展示与排障）
    pub dest: String,
}

/// 文档级审校作业设置更新（字段均可缺省 = 保持不变）
#[derive(Deserialize, Default)]
pub struct UpdateDocJobRequest {
    pub chunk_target: Option<usize>,
    pub concurrency: Option<usize>,
    pub cache: Option<bool>,
    /// `full` | `economical`
    pub budget: Option<String>,
}

/// 载体审校设置更新（字段均可缺省 = 保持不变）
///
/// 只列设置面板要改的项：CLI 路径 / 超时 / 压缩参数留在设置文件与环境变量里
#[derive(Deserialize)]
pub struct UpdateMediaRequest {
    pub enabled: Option<bool>,
    /// 工作模式：`simple`（零 CLI）/ `advanced`（三家适配器，可回写）
    pub mode: Option<String>,
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

/// 设置测试请求：`target` = `hanlp` / `llm` / `decision` / `l3` / `embedding` /
/// `tables_dir` / `lexicons_dir` / `all`（并行测试全部项目）
#[derive(Deserialize)]
pub struct TestSettingsRequest {
    pub target: String,
}

/// 单项测试结果（`all` 时 `results` 携带逐项结果，顶层 `ok` / `message` 为汇总）
#[derive(Serialize)]
pub struct SettingsTestResponse {
    pub target: String,
    pub ok: bool,
    pub message: String,
    pub latency_ms: u64,
    /// 未探测的跳过（该层未启用）：不与「通过」混谈
    #[serde(default)]
    pub skipped: bool,
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
        skipped: r.skipped,
        results: None,
    }
}

/// LLM 参数更新（字段均可缺省 = 保持不变）
#[derive(Deserialize)]
pub struct UpdateLlmRequest {
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub model: Option<String>,
    pub timeout_secs: Option<u64>,
    pub prompts_dir: Option<String>,
    /// 思考模式开关（缺省 = 保持不变）
    pub thinking: Option<bool>,
}

fn to_response(
    s: &AppSettings,
    registry: &gec_core::ModelRegistry,
    decision_config_source: &'static str,
    db_backup: DbBackupView,
) -> SettingsResponse {
    SettingsResponse {
        hanlp_endpoint: s.hanlp_endpoint.clone(),
        llm: LlmSettingsView {
            base_url: s.llm.base_url.clone(),
            api_key: s.llm.api_key.clone(),
            model: s.llm.model.clone(),
            timeout_secs: s.llm.timeout_secs,
            prompts_dir: s.llm.prompts_dir.display().to_string(),
            thinking: s.llm.thinking,
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
            config_source: decision_config_source,
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
        llm_dual_judge: s.llm_dual_judge,
        llm_dual_model_id: s.llm_dual_model_id.clone(),
        llm_dual_base_url: resolve_dual_endpoint(&s, registry)
            .map(|d| d.0)
            .unwrap_or_default(),
        llm_dual_model: resolve_dual_endpoint(&s, registry)
            .map(|d| d.1)
            .unwrap_or_default(),
        llm_dual_api_key: s.llm_dual_api_key.clone(),
        knowledge_enabled: s.knowledge_enabled,
        media: s.media.clone(),
        doc_jobs: s.doc_jobs.clone(),
        db_backup,
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

/// 影子臂生效端点 `(base_url, model, api_key)`，与 `state::dual_shadow_config` 同口径：
/// 显式成对字段优先；否则注册表预设（需自持端点且不与主臂同 id）；密钥留空复用主判。
/// `None` = 解析不出端点（双判会跳过，测试也如实跳过）
fn resolve_dual_endpoint(
    s: &crate::settings::AppSettings,
    registry: &gec_core::ModelRegistry,
) -> Option<(String, String, String)> {
    let key = if s.llm_dual_api_key.trim().is_empty() {
        s.llm.api_key.clone()
    } else {
        s.llm_dual_api_key.clone()
    };
    let base = s.llm_dual_base_url.trim();
    let model = s.llm_dual_model.trim();
    if !base.is_empty() && !model.is_empty() {
        return Some((base.to_string(), model.to_string(), key));
    }
    if !base.is_empty() || !model.is_empty() {
        return None; // 半空半满：保存校验会拦，这里再兑一层
    }
    let entry = registry
        .get(&s.llm_dual_model_id)
        .filter(|e| !e.base_url.is_empty() && e.id != s.llm_model_id)?;
    Some((entry.base_url.clone(), entry.model.clone(), key))
}

/// GET /gec/settings — 当前设置（网关形态下非超管看不到 LLM/embedding 密钥明文）
pub async fn get(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
) -> Json<SettingsResponse> {
    let s = state.settings.read().unwrap();
    let registry = state.model_registry.read().unwrap();
    let mut resp = to_response(
        &s,
        &registry,
        crate::settings::AppSettings::decision_config_source(&state.settings_path),
        db_backup_view(&state.db_path, s.db_backup_enabled),
    );
    if ctx.gateway && !ctx.admin {
        mask_secrets(&mut resp);
    }
    Json(resp)
}

/// 非超管读设置时密钥字段只回掩码（put 未提供该字段即保持原值，故编辑走「留空不改」）
fn mask_secrets(resp: &mut SettingsResponse) {
    for key in [&mut resp.llm.api_key, &mut resp.embedding.api_key] {
        if !key.is_empty() {
            *key = "***".to_owned();
        }
    }
}

/// 设置写入守卫。
/// - 网关形态：仅超级管理员密钥（或其会话）可改；
/// - 单机形态：owner 激活（超管密钥首次登录）前本机即用户、零变化；激活后要求已验证凭证。
fn guard_settings_write(ctx: &AuthContext, cfg: &GatewayConfig) -> Result<(), ApiError> {
    if ctx.gateway {
        if !ctx.admin {
            return Err(ApiError::Forbidden(
                "配置需要超级管理员密钥登录后修改".into(),
            ));
        }
        return Ok(());
    }
    let claimed = cfg
        .store
        .meta_get(crate::auth::META_OWNER_CLAIMED)
        .ok()
        .flatten()
        .is_some();
    if claimed && !ctx.authed {
        return Err(ApiError::Unauthorized(
            "本机设置已被超级管理员锁定，请先在验证页登录".into(),
        ));
    }
    Ok(())
}

/// PUT /gec/settings — 更新设置（即时生效 + 持久化）
pub async fn put(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Extension(cfg): Extension<Arc<GatewayConfig>>,
    Json(req): Json<UpdateSettingsRequest>,
) -> Result<Json<SettingsResponse>, ApiError> {
    guard_settings_write(&ctx, &cfg)?;
    let mut next = state.settings.read().unwrap().clone();
    if let Some(endpoint) = req.hanlp_endpoint.clone() {
        if endpoint.trim().is_empty() {
            return Err(ApiError::BadRequest("hanlp_endpoint 不能为空".into()));
        }
        next.hanlp_endpoint = endpoint;
    }
    if let Some(llm) = &req.llm {
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
                return Err(ApiError::BadRequest("llm.prompts_dir 不能为空".into()));
            }
            next.llm.prompts_dir = std::path::PathBuf::from(v);
        }
        if let Some(v) = llm.thinking {
            next.llm.thinking = v;
        }
    }
    if let Some(v) = req.offline_mode {
        next.offline_mode = v;
    }
    if let Some(dir) = req.tables_dir.clone() {
        if dir.trim().is_empty() {
            return Err(ApiError::BadRequest("tables_dir 不能为空".into()));
        }
        next.tables_dir = dir;
    }
    if let Some(dir) = req.lexicons_dir.clone() {
        if dir.trim().is_empty() {
            return Err(ApiError::BadRequest("lexicons_dir 不能为空".into()));
        }
        next.lexicons_dir = dir;
    }
    if let Some(decision) = &req.decision {
        if let Some(v) = &decision.endpoint {
            if v.trim().is_empty() {
                return Err(ApiError::BadRequest("decision.endpoint 不能为空".into()));
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
    if let Some(l3) = &req.l3 {
        if let Some(v) = &l3.endpoint {
            if v.trim().is_empty() {
                return Err(ApiError::BadRequest("l3.endpoint 不能为空".into()));
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
    if let Some(embedding) = &req.embedding {
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
    if let Some(model_id) = req.llm_model_id.clone() {
        if model_id.trim().is_empty() {
            return Err(ApiError::BadRequest("llm_model_id 不能为空".into()));
        }
        next.llm_model_id = model_id;
    }
    if let Some(on) = req.llm_dual_judge {
        next.llm_dual_judge = on;
    }
    if let Some(model_id) = req.llm_dual_model_id.clone() {
        if model_id.trim().is_empty() {
            return Err(ApiError::BadRequest("llm_dual_model_id 不能为空".into()));
        }
        next.llm_dual_model_id = model_id;
    }
    if let Some(v) = req.llm_dual_base_url.clone() {
        next.llm_dual_base_url = v.trim().to_string();
    }
    if let Some(v) = req.llm_dual_model.clone() {
        next.llm_dual_model = v.trim().to_string();
    }
    // 成对校验按落盘后的最终状态：半空半满会让双判静默跳过，必须当场拒绝
    if next.llm_dual_base_url.is_empty() != next.llm_dual_model.is_empty() {
        return Err(ApiError::BadRequest(
            "影子臂端点与模型需成对填写（两者同时留空 = 沿用注册表预设）".into(),
        ));
    }
    if let Some(v) = req.llm_dual_api_key.clone() {
        next.llm_dual_api_key = v;
    }
    if let Some(v) = req.knowledge_enabled {
        next.knowledge_enabled = v;
    }
    if let Some(media) = &req.media {
        // 载体设置：与 api-server / 桌面端同一份 `MediaSettings`（`media-flow`）
        if let Some(v) = media.enabled {
            next.media.enabled = v;
        }
        if let Some(v) = &media.mode {
            // 不用 JobMode::parse 兜底：那是宽松解析，会把拼错的值悄悄变成 advanced
            let v = v.trim().to_ascii_lowercase().replace('-', "_");
            if !matches!(v.as_str(), "simple" | "advanced") {
                return Err(ApiError::BadRequest(format!(
                    "media.mode 无效: {v}（应为 simple = 零 CLI / advanced = 三家适配器）"
                )));
            }
            next.media.mode = v;
        }
        if let Some(v) = &media.apply_mode {
            let v = v.trim().to_string();
            if !matches!(v.as_str(), "overlay" | "comment" | "in_place" | "list_only") {
                return Err(ApiError::BadRequest(format!(
                    "media.apply_mode 无效: {v}（应为 overlay / comment / in_place / list_only）"
                )));
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
                return Err(ApiError::BadRequest("media.level 应为 1-3".into()));
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
    if let Some(doc_jobs) = &req.doc_jobs {
        if let Some(v) = doc_jobs.chunk_target {
            if !(50..=20000).contains(&v) {
                return Err(ApiError::BadRequest(
                    "doc_jobs.chunk_target 应为 50-20000".into(),
                ));
            }
            next.doc_jobs.chunk_target = v;
        }
        if let Some(v) = doc_jobs.concurrency {
            if !(1..=64).contains(&v) {
                return Err(ApiError::BadRequest(
                    "doc_jobs.concurrency 应为 1-64".into(),
                ));
            }
            next.doc_jobs.concurrency = v;
        }
        if let Some(v) = doc_jobs.cache {
            next.doc_jobs.cache = v;
        }
        if let Some(v) = &doc_jobs.budget {
            let v = v.trim().to_ascii_lowercase();
            if !matches!(v.as_str(), "full" | "economical") {
                return Err(ApiError::BadRequest(
                    "doc_jobs.budget 无效（应为 full = 全量深度复查 / economical = 低风险块跳过）"
                        .into(),
                ));
            }
            next.doc_jobs.budget = v;
        }
    }
    if let Some(v) = req.db_backup_enabled {
        // 开关即注册/撤销 launchd 任务；失败直接报错且不落盘（设置不说谎）。
        let db_path = state.db_path.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            if v {
                crate::db_backup::install(&db_path)
            } else {
                crate::db_backup::uninstall()
            }
        })
        .await
        .map_err(|e| ApiError::BadRequest(format!("每日备份任务操作中断: {e}")))?;
        outcome.map_err(ApiError::BadRequest)?;
        next.db_backup_enabled = v;
    }
    state.apply_settings(next.clone());
    // 依赖监督联动：配置变更后后台重新探测对应服务（用户改完地址保存即见新状态，无需手动重测）
    {
        let touched: Vec<&'static str> = [
            (req.decision.is_some(), "decision"),
            (req.l3.is_some(), "l3"),
            (req.hanlp_endpoint.is_some(), "hanlp"),
            (req.llm.is_some(), "llm"),
            (req.embedding.is_some(), "embedding"),
            (req.offline_mode.is_some(), "hanlp"),
        ]
        .into_iter()
        .filter(|(hit, _)| *hit)
        .map(|(_, id)| id)
        .collect();
        if !touched.is_empty() {
            let st = state.clone();
            tokio::spawn(async move {
                for id in touched {
                    crate::deps::recheck(&st, id).await;
                }
            });
        }
    }
    let registry = state.model_registry.read().unwrap();
    Ok(Json(to_response(
        &next,
        &registry,
        crate::settings::AppSettings::decision_config_source(&state.settings_path),
        db_backup_view(&state.db_path, next.db_backup_enabled),
    )))
}

/// 每日备份视图：enabled = 设置愿望 ∧ launchd 实存态（设置不说谎）；setting 回显开关原位置；dest 供面板透明展示。
fn db_backup_view(db_path: &std::path::Path, setting_on: bool) -> DbBackupView {
    let installed = crate::db_backup::installed();
    DbBackupView {
        setting: setting_on,
        enabled: setting_on && installed,
        installed,
        dest: crate::db_backup::backup_dest(db_path).display().to_string(),
    }
}

/// POST /gec/settings/test — 测试指定配置项的服务连通性 / 可用性
///
/// 对当前设置发起一次最小化真实调用（只读探测，不修改任何配置）：
/// - `hanlp`：`POST {endpoint}/analyze`（验证分词返回）
/// - `llm`：`POST {base_url}/chat/completions`（非流式最小提示）
/// - `llm_dual`：同一次最小调用，但面向影子臂生效端点（显式成对 > 注册表预设；
///   解析不出 = 如实 skipped，不拿主臂端点凑数）
/// - `decision`：`POST {endpoint}/decide`（vegaml 决策模型）
/// - `l3`：`POST {endpoint}/l3-detect`（feature-model 特征模型）
/// - `embedding`：`POST {endpoint}/embeddings`（OpenAI 兼容向量接口）
/// - `tables_dir` / `lexicons_dir`：目录存在性 + 数据文件计数
/// - `all`：并行执行以上全部，`results` 返回逐项结果
pub async fn test(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Extension(cfg): Extension<Arc<GatewayConfig>>,
    Json(req): Json<TestSettingsRequest>,
) -> Result<Json<SettingsTestResponse>, ApiError> {
    // 探测会用服务器侧密钥发起外呼，口径与设置写入一致（配置面仅超管）
    guard_settings_write(&ctx, &cfg)?;
    let s = state.settings.read().unwrap().clone();
    let t = req.target.trim().to_lowercase();
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
        "llm_dual" => {
            let endpoint = resolve_dual_endpoint(&s, &state.model_registry.read().unwrap());
            match endpoint {
                Some((base, model, key)) => {
                    let mut r =
                        gec_core::settings_test::test_llm(&base, &key, &model, s.llm.timeout_secs)
                            .await;
                    r.target = "llm_dual".into(); // 探的是影子端点，响应不得自称 llm
                    (r, None)
                }
                None => (
                    gec_core::settings_test::TestResult {
                        target: "llm_dual".into(),
                        ok: false,
                        skipped: true,
                        message:
                            "影子臂端点未配置（或影子臂与主判臂同一模型）：双判会跳过，无可测端点"
                                .into(),
                        latency_ms: 0,
                    },
                    None,
                ),
            }
        }
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
            let summary = gec_core::settings_test::summarize(results.clone());
            (summary, Some(results))
        }
        other => {
            return Err(ApiError::BadRequest(format!(
                "未知测试目标: {other}（支持 hanlp / llm / decision / l3 / embedding / tables_dir / lexicons_dir / all）"
            )));
        }
    };
    let mut resp = to_test_response(&r);
    if let Some(results) = all_results {
        resp.results = Some(results.iter().map(to_test_response).collect());
    }
    Ok(Json(resp))
}

/// POST /gec/ops/unlock — 「平台运营」角色解锁验证（web 形态）
///
/// 前端「工作场景与角色」开启运营扩展（规则管理 / 知识维护 / 进化中心）前调用。
/// 鉴权与决策表热更新一致：设置了 `GEC_TABLES_UPDATE_TOKEN` 时要求匹配的
/// `X-Tables-Update-Token` 头，否则 401；未设置 token 时直接放行（本地开发语义）。
/// 桌面端本地单用户，前端不经此端点（直接解锁）。
pub async fn ops_unlock(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    if let Some(expected) = &state.tables_update_token {
        let got = headers
            .get("x-tables-update-token")
            .and_then(|v| v.to_str().ok());
        if got != Some(expected.as_str()) {
            return Err(ApiError::Unauthorized(
                "解锁运营功能需提供有效的 X-Tables-Update-Token".into(),
            ));
        }
    }
    Ok(Json(serde_json::json!({ "unlocked": true })))
}
