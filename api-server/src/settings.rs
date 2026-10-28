//! 运行时设置（设置界面）
//!
//! - `GET /gec/settings` — 读取当前设置（HanLP 地址 + 离线开关 + 目录配置 + LLM 参数 + LLM 请求模式）
//! - `PUT /gec/settings` — 更新设置（即时生效，无需重启；持久化到 JSON 文件）
//!
//! 优先级：设置文件（`GEC_SETTINGS_FILE`，默认 `langfixer-settings.json`）> 环境变量 > 内置默认。
//! LLM 请求模式为固定默认：前端请求用流式（SSE），外部 API 调用用非流式（见 `routes::settings`）。

use std::path::Path;

use gec_core::{DecisionConfig, EmbeddingConfig, L3FeatureConfig};
use gec_zh::DetectorConfig as ZhConfig;
use langfixer_llm::LlmConfig;
use serde::{Deserialize, Serialize};

// 多模态设置住在 `media-flow`：api-server 与 desktop-app 共用同一份设置面
pub use media_flow::settings::MediaSettings;

/// 决策表目录默认值（与 `GEC_TABLES_DIR` 缺省一致）
pub const DEFAULT_TABLES_DIR: &str = "decision-tables";
/// 规则词表目录默认值（与 `GEC_LEXICONS_DIR` 缺省一致）
pub const DEFAULT_LEXICONS_DIR: &str = "lexicons";

fn default_tables_dir() -> String {
    DEFAULT_TABLES_DIR.to_string()
}
fn default_lexicons_dir() -> String {
    DEFAULT_LEXICONS_DIR.to_string()
}

/// 运行时设置（JSON 持久化）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    /// HanLP（NarrativeOS narrative-operator-nlp）服务地址（中文 + 英文检测器共用）
    pub hanlp_endpoint: String,
    /// LLM（OpenAI 兼容）参数
    pub llm: LlmConfig,
    /// 离线模式（等价环境变量 `GEC_OFFLINE=1`）：检测器跳过远端 NLP 服务，仅用本地规则
    #[serde(default)]
    pub offline_mode: bool,
    /// 决策表目录（等价环境变量 `GEC_TABLES_DIR`）
    #[serde(default = "default_tables_dir")]
    pub tables_dir: String,
    /// 规则词表目录（等价环境变量 `GEC_LEXICONS_DIR`；zh/en/ja 共用）
    #[serde(default = "default_lexicons_dir")]
    pub lexicons_dir: String,
    /// 决策模型（vegaml）层参数（设置界面可改，持久化；
    /// **默认关闭**（P0.5：L3 特征模型是唯一的 pre-LLM 门控，决策模型是 opt-in 层）；
    /// `enabled=true` 显式开启时在 `rule` 引擎组合中挂载决策模型层；
    /// 等价环境变量 `GEC_DEC_*`）
    #[serde(default)]
    pub decision: DecisionConfig,
    /// L3 特征模型层参数（三期：level 3 先跑特征模型，LLM 收缩到纯长尾；
    /// 设置界面可改，持久化；等价环境变量 `GEC_L3_*`）
    #[serde(default)]
    pub l3: L3FeatureConfig,
    /// 嵌入引擎配置（向量召回用；未配置时知识召回静默禁用）
    #[serde(default)]
    pub embedding: EmbeddingConfig,
    /// 当前选中的纠错模型预设 id（`prompts/models.json` 注册表；缺省 `generic`）。
    /// 决定 LLM 兜底层的输出适配（json / plain）与生成参数（CEC 工作流 E）。
    #[serde(default = "default_llm_model_id")]
    pub llm_model_id: String,
    /// 知识层总开关（默认开）：关闭后知识向量召回（②.5）、知识库富化与
    /// 数据驱动归点全部旁路——测评消融臂 `knowledge: false` 即切此开关。
    /// 等价环境变量 `GEC_KNOWLEDGE_ENABLED=0` 关闭。
    #[serde(default = "default_true")]
    pub knowledge_enabled: bool,
    /// 多模态审校（图片 / PDF / Word）设置（`MULTIMODAL.md` §13）。
    /// **默认关闭**：P0 只上后端，未显式开启时不影响既有文本主链路。
    #[serde(default)]
    pub media: MediaSettings,
}

fn default_true() -> bool {
    true
}

fn default_llm_model_id() -> String {
    "generic".to_string()
}

impl AppSettings {
    /// 加载设置：设置文件存在且可解析时优先使用；否则从环境变量构建（与旧行为一致）
    pub fn load(path: &Path) -> Self {
        if let Ok(content) = std::fs::read_to_string(path) {
            match serde_json::from_str::<Self>(&content) {
                Ok(s) => {
                    tracing::info!(
                        path = %path.display(),
                        "已加载设置文件（优先于环境变量）"
                    );
                    return s;
                }
                Err(e) => tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "设置文件解析失败，回退环境变量"
                ),
            }
        }
        Self::from_env()
    }

    /// 从环境变量构建（设置文件缺失时的默认来源）：
    /// `HANLP_ENDPOINT` / `GEC_OFFLINE` / `GEC_TABLES_DIR` / `GEC_LEXICONS_DIR` / `LLM_*` / `GEC_DEC_*`
    pub fn from_env() -> Self {
        Self {
            hanlp_endpoint: ZhConfig::from_env().endpoint,
            llm: LlmConfig::from_env(),
            offline_mode: std::env::var("GEC_OFFLINE")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            tables_dir: std::env::var("GEC_TABLES_DIR")
                .unwrap_or_else(|_| DEFAULT_TABLES_DIR.to_string()),
            lexicons_dir: std::env::var("GEC_LEXICONS_DIR")
                .unwrap_or_else(|_| DEFAULT_LEXICONS_DIR.to_string()),
            decision: DecisionConfig::from_env(),
            l3: L3FeatureConfig::from_env(),
            embedding: EmbeddingConfig {
                endpoint: std::env::var("EMBEDDING_ENDPOINT").unwrap_or_default(),
                api_key: std::env::var("EMBEDDING_API_KEY").unwrap_or_default(),
                ..EmbeddingConfig::default()
            },
            llm_model_id: default_llm_model_id(),
            knowledge_enabled: std::env::var("GEC_KNOWLEDGE_ENABLED")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(true),
            media: MediaSettings::from_env(),
        }
    }

    /// 持久化到指定文件（调用方负责错误处理）
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        let content = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_env_defaults_when_no_file() {
        unsafe {
            std::env::remove_var("HANLP_ENDPOINT");
            std::env::remove_var("LLM_API_KEY");
            std::env::remove_var("LLM_BASE_URL");
            std::env::remove_var("LLM_MODEL");
            std::env::remove_var("GEC_OFFLINE");
            std::env::remove_var("GEC_TABLES_DIR");
            std::env::remove_var("GEC_LEXICONS_DIR");
        }
        let s = AppSettings::from_env();
        assert_eq!(s.hanlp_endpoint, "http://127.0.0.1:8000");
        assert!(!s.llm.enabled());
        assert_eq!(s.llm.model, "gpt-4o-mini");
        assert!(!s.offline_mode);
        assert_eq!(s.tables_dir, DEFAULT_TABLES_DIR);
        assert_eq!(s.lexicons_dir, DEFAULT_LEXICONS_DIR);
    }

    #[test]
    fn load_old_settings_file_without_new_fields() {
        // 旧版设置文件（无 offline_mode / tables_dir / lexicons_dir）仍可加载，新字段取默认值
        let dir = std::env::temp_dir().join(format!(
            "langfixer_settings_test_{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("old.json");
        std::fs::write(
            &path,
            r#"{"hanlp_endpoint":"http://10.0.0.9:8000","llm":{"base_url":"https://api.openai.com/v1","api_key":"","model":"gpt-4o-mini","timeout_secs":30,"prompts_dir":"prompts"}}"#,
        )
        .unwrap();
        let s = AppSettings::load(&path);
        assert_eq!(s.hanlp_endpoint, "http://10.0.0.9:8000");
        assert!(!s.offline_mode);
        assert_eq!(s.tables_dir, DEFAULT_TABLES_DIR);
        assert_eq!(s.lexicons_dir, DEFAULT_LEXICONS_DIR);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_prefers_file_over_env() {
        let dir = std::env::temp_dir().join(format!(
            "langfixer_settings_test_{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.json");
        let s = AppSettings {
            hanlp_endpoint: "http://10.0.0.5:8000".into(),
            llm: LlmConfig {
                base_url: "http://10.0.0.5:9000/v1".into(),
                api_key: "sk-file".into(),
                model: "deepseek-chat".into(),
                timeout_secs: 99,
                prompts_dir: std::path::PathBuf::from("prompts"),
            },
            offline_mode: true,
            tables_dir: "custom-tables".into(),
            lexicons_dir: "custom-lexicons".into(),
            decision: DecisionConfig::default(),
            l3: L3FeatureConfig::default(),
            embedding: EmbeddingConfig::default(),
            llm_model_id: "generic".into(),
            knowledge_enabled: false,
            media: MediaSettings::default(),
        };
        s.save_to(&path).unwrap();
        let loaded = AppSettings::load(&path);
        assert_eq!(loaded.hanlp_endpoint, "http://10.0.0.5:8000");
        assert_eq!(loaded.llm.api_key, "sk-file");
        assert!(loaded.llm.enabled());
        assert!(loaded.offline_mode);
        assert_eq!(loaded.tables_dir, "custom-tables");
        assert_eq!(loaded.lexicons_dir, "custom-lexicons");
        assert!(
            !loaded.knowledge_enabled,
            "知识层开关关闭应随设置文件持久化"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn knowledge_enabled_defaults_true_and_parses_legacy_files() {
        // 旧设置文件（无该字段）→ 默认开启；显式 false → 关闭
        let legacy = r#"{"hanlp_endpoint":"http://x","llm":{"base_url":"","api_key":"","model":"","timeout_secs":30,"prompts_dir":"prompts"}}"#;
        let s: AppSettings = serde_json::from_str(legacy).unwrap();
        assert!(s.knowledge_enabled, "缺字段默认开启（向后兼容）");
        let with_off = legacy[..legacy.len() - 1].to_string() + r#","knowledge_enabled":false}"#;
        let s2: AppSettings = serde_json::from_str(&with_off).unwrap();
        assert!(!s2.knowledge_enabled);
    }

    #[test]
    fn media_defaults_are_off_and_overlay() {
        unsafe {
            for k in [
                "GEC_MEDIA_ENABLED",
                "GEC_MEDIA_APPLY_MODE",
                "GEC_MEDIA_WORKDIR",
                "GEC_MEDIA_VISION",
                "GEC_MEDIA_OCR",
                "PHOTOCRAFT_CLI",
            ] {
                std::env::remove_var(k);
            }
        }
        let m = MediaSettings::from_env();
        assert!(
            !m.enabled,
            "多模态默认关闭：未显式开启不得改变既有文本主链路行为"
        );
        assert_eq!(
            m.apply_mode, "overlay",
            "默认回写模式必须是 Overlay（决策 ①）"
        );
        assert!(m.use_structure_layer, "A 档（结构层）默认开——它是真值来源");
        assert_eq!(
            (m.timeout_secs, m.vision_timeout_secs),
            (120, 300),
            "视觉档超时必须比协作工具宽：整页转录不是 CLI 调用那种量级"
        );
        assert!(!m.use_vision && !m.use_ocr, "B/C 档默认关（P1/P2 再开）");
        assert_eq!(
            m.level, 3,
            "检测级别默认 3：多模态默认火力全开（识别能力优先，性能后置）"
        );
        assert!(
            m.force_llm,
            "默认强制 LLM 层；无凭据时由引擎降级并在 layers 标注"
        );
        assert_eq!(m.cjk_font, "Heiti SC");
        // 旧设置文件无 media 字段 → 取默认（向后兼容）
        let legacy = r#"{"hanlp_endpoint":"http://x","llm":{"base_url":"","api_key":"","model":"","timeout_secs":30,"prompts_dir":"prompts"}}"#;
        let s: AppSettings = serde_json::from_str(legacy).unwrap();
        assert!(!s.media.enabled);
        assert_eq!(s.media.apply_mode, "overlay");
        // 模式字符串宽松解析（UI / env / 请求三处输入共用）
        assert_eq!(
            media_core::ApplyMode::parse("IN-PLACE"),
            media_core::ApplyMode::InPlace
        );
        assert_eq!(
            media_core::ApplyMode::parse("list_only"),
            media_core::ApplyMode::ListOnly
        );
        assert_eq!(
            media_core::ApplyMode::parse("whatever"),
            media_core::ApplyMode::Overlay
        );
    }
}
