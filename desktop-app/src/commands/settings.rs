use std::path::PathBuf;
use std::sync::Arc;

use gec_core::ModelRegistry;

use crate::settings::AppSettings;

use super::core::Core;
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
        if let Some(v) = &media.mode {
            // 不用 JobMode::parse 兜底：那是宽松解析，会把拼错的值悄悄变成 advanced
            let v = v.trim().to_ascii_lowercase().replace('-', "_");
            if !matches!(v.as_str(), "simple" | "advanced") {
                return Err(format!(
                    "media.mode 无效: {v}（应为 simple = 零 CLI / advanced = 三家适配器）"
                ));
            }
            next.media.mode = v;
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
            let summary = gec_core::settings_test::summarize(results.clone());
            (summary, Some(results))
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
