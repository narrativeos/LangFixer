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
use axum::extract::State;
use serde::{Deserialize, Serialize};

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
    /// 决策模型（vegaml）层参数（设置界面可改，即时生效 + 持久化）
    pub decision: DecisionSettingsView,
    /// L3 特征模型层参数（三期：level 3 先跑特征模型，LLM 收缩到纯长尾）
    pub l3: L3SettingsView,
    /// 嵌入引擎（向量召回）参数（未配置时知识召回静默禁用）
    pub embedding: EmbeddingSettingsView,
    /// 当前选中的纠错模型预设 id（`prompts/models.json` 注册表；缺省 `generic`）
    pub llm_model_id: String,
    /// 知识层总开关（知识向量召回 + 知识库富化 + 数据驱动归点；默认开）
    pub knowledge_enabled: bool,
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
    /// 知识层总开关（测评消融臂 `knowledge: false` 用；默认开）
    pub knowledge_enabled: Option<bool>,
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

/// LLM 参数更新（字段均可缺省 = 保持不变）
#[derive(Deserialize)]
pub struct UpdateLlmRequest {
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub model: Option<String>,
    pub timeout_secs: Option<u64>,
    pub prompts_dir: Option<String>,
}

fn to_response(s: &AppSettings, registry: &gec_core::ModelRegistry) -> SettingsResponse {
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

/// GET /gec/settings — 当前设置
pub async fn get(State(state): State<Arc<AppState>>) -> Json<SettingsResponse> {
    let s = state.settings.read().unwrap();
    let registry = state.model_registry.read().unwrap();
    Json(to_response(&s, &registry))
}

/// PUT /gec/settings — 更新设置（即时生效 + 持久化）
pub async fn put(
    State(state): State<Arc<AppState>>,
    Json(req): Json<UpdateSettingsRequest>,
) -> Result<Json<SettingsResponse>, ApiError> {
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
    if let Some(v) = req.knowledge_enabled {
        next.knowledge_enabled = v;
    }
    state.apply_settings(next.clone());
    let registry = state.model_registry.read().unwrap();
    Ok(Json(to_response(&next, &registry)))
}

/// POST /gec/settings/test — 测试指定配置项的服务连通性 / 可用性
///
/// 对当前设置发起一次最小化真实调用（只读探测，不修改任何配置）：
/// - `hanlp`：`POST {endpoint}/analyze`（验证分词返回）
/// - `llm`：`POST {base_url}/chat/completions`（非流式最小提示）
/// - `decision`：`POST {endpoint}/decide`（vegaml 决策模型）
/// - `l3`：`POST {endpoint}/l3-detect`（feature-model 特征模型）
/// - `embedding`：`POST {endpoint}/embeddings`（OpenAI 兼容向量接口）
/// - `tables_dir` / `lexicons_dir`：目录存在性 + 数据文件计数
/// - `all`：并行执行以上全部，`results` 返回逐项结果
pub async fn test(
    State(state): State<Arc<AppState>>,
    Json(req): Json<TestSettingsRequest>,
) -> Result<Json<SettingsTestResponse>, ApiError> {
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
