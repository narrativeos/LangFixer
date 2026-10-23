//! LangFixer LLM 扩展层
//!
//! - [`client`]：OpenAI 兼容 `/v1/chat/completions` 客户端（非流式 + SSE 流式）
//! - [`templates`]：外置提示词模板（`prompts/*.json`，按语言 × CEFR 分级，版本化管理）
//! - [`LlmService`]：将归因结果（错误类型 / 归因标签 / CEFR）渲染为个性化讲解
//!
//! 未配置 `LLM_API_KEY` 时服务不可用，调用方应回退到决策表静态补救建议。

pub mod client;
pub mod error;
pub mod templates;

use error::LlmError;
use futures_util::stream::BoxStream;
use templates::{PromptTemplate, TemplateStore};

/// LLM 服务配置（环境变量驱动；可 JSON 序列化，供 api-server 设置界面持久化）
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LlmConfig {
    /// OpenAI 兼容 API 地址，如 `https://api.openai.com/v1`
    pub base_url: String,
    /// API Key（空字符串表示服务未启用）
    pub api_key: String,
    /// 模型名，如 `gpt-4o-mini`
    pub model: String,
    /// 连接与单次读超时（秒；流式生成只要数据持续到达就不会中断，停滞连接仍可被检测）
    pub timeout_secs: u64,
    /// 提示词模板目录（`{dir}/*_prompt.json`）
    pub prompts_dir: std::path::PathBuf,
}

impl LlmConfig {
    /// 从环境变量读取：
    /// - `LLM_BASE_URL`（默认 `https://api.openai.com/v1`）
    /// - `LLM_API_KEY`（缺省 → 服务未启用）
    /// - `LLM_MODEL`（默认 `gpt-4o-mini`）
    /// - `LLM_TIMEOUT_SECS`（默认 30）
    /// - `LLM_PROMPTS_DIR`（默认 `prompts`）
    pub fn from_env() -> Self {
        Self {
            base_url: std::env::var("LLM_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".to_string()),
            api_key: std::env::var("LLM_API_KEY").unwrap_or_default(),
            model: std::env::var("LLM_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string()),
            timeout_secs: std::env::var("LLM_TIMEOUT_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(30),
            prompts_dir: std::path::PathBuf::from(
                std::env::var("LLM_PROMPTS_DIR").unwrap_or_else(|_| "prompts".to_string()),
            ),
        }
    }

    /// 是否已配置 API Key（未配置时上层应回退静态建议）
    pub fn enabled(&self) -> bool {
        !self.api_key.is_empty()
    }
}

/// LLM 服务：配置 + 提示词模板库
#[derive(Clone)]
pub struct LlmService {
    pub config: LlmConfig,
    pub templates: TemplateStore,
}

impl LlmService {
    /// 从环境变量构建（模板目录缺失时为空库，调用时返回 `TemplateNotFound`）
    pub fn from_env() -> Self {
        let config = LlmConfig::from_env();
        let templates = TemplateStore::load(&config.prompts_dir);
        Self { config, templates }
    }

    /// 是否可用（已配置 API Key 且模板库非空）
    pub fn is_available(&self) -> bool {
        self.config.enabled() && !self.templates.is_empty()
    }

    /// 解析提示词模板：按 `lang` + `cefr` 精确匹配，无匹配时回退 `{lang}_generic`
    fn resolve_template(&self, lang: &str, cefr: &str) -> Option<&PromptTemplate> {
        self.templates
            .for_lang_cefr(lang, cefr)
            .or_else(|| self.templates.get(&format!("{lang}_generic")))
    }

    /// 解析讲解目标语言：优先用户显式选择（`target` 非空）；
    /// 否则回退到检测语言的显示名（zh→中文 / en→English / ja→日本語）
    ///
    /// 返回 `(显示名, 是否用户显式指定)`；显式指定时调用方应在 system 提示词追加覆盖指令。
    fn resolve_target_language(lang: &str, target: Option<&str>) -> (String, bool) {
        let explicit = target.map(str::trim).is_some_and(|t| !t.is_empty());
        let display = if explicit {
            target.unwrap().trim().to_string()
        } else {
            match lang {
                "zh" => "中文".to_string(),
                "en" => "English".to_string(),
                "ja" => "日本語".to_string(),
                _ => "与检测语言一致".to_string(),
            }
        };
        (display, explicit)
    }

    /// 渲染提示词变量（含用户画像：母语 + 历史错误类型分布 + 讲解目标语言）
    fn render_vars(
        &self,
        error_type: &str,
        attribution: &str,
        cefr: &str,
        student_history_count: u32,
        native_language: Option<&str>,
        error_distribution: Option<&str>,
        target_language: &str,
    ) -> Vec<(String, String)> {
        vec![
            ("error_type".into(), error_type.to_string()),
            ("attribution".into(), attribution.to_string()),
            (
                "cefr".into(),
                if cefr.is_empty() {
                    "A1".into()
                } else {
                    cefr.to_string()
                },
            ),
            (
                "student_history_count".into(),
                student_history_count.to_string(),
            ),
            (
                "native_language".into(),
                native_language.unwrap_or("unknown").to_string(),
            ),
            (
                "error_distribution".into(),
                error_distribution
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "暂无历史记录".to_string()),
            ),
            ("target_language".into(), target_language.to_string()),
        ]
    }

    /// 通用补全（自定义 system + user 提示词；反思流程等非讲解场景用）
    ///
    /// 未配置 API Key 时返回 [`LlmError::NotConfigured`]（不发起网络请求）。
    pub async fn complete(&self, system: &str, user: &str) -> Result<String, LlmError> {
        if !self.config.enabled() {
            return Err(LlmError::NotConfigured);
        }
        let client = client::LlmClient::new(&self.config);
        client.complete(system, user).await
    }

    /// 生成个性化讲解（非流式）
    ///
    /// 按 `lang` + `cefr` 选择提示词模板（无匹配时回退 `{lang}_generic`），
    /// 注入用户画像（母语 / 历史错误分布）后调用 LLM。
    /// `target_language` 为讲解目标语言（如 `"English"`）；`None`/空时跟随检测语言。
    #[allow(clippy::too_many_arguments)]
    pub async fn explain(
        &self,
        lang: &str,
        error_type: &str,
        attribution: &str,
        cefr: &str,
        student_history_count: u32,
        native_language: Option<&str>,
        error_distribution: Option<&str>,
        target_language: Option<&str>,
    ) -> Result<String, LlmError> {
        if !self.config.enabled() {
            return Err(LlmError::NotConfigured);
        }
        let template = self
            .resolve_template(lang, cefr)
            .ok_or_else(|| LlmError::TemplateNotFound(lang.to_string()))?;
        let (target, explicit) = Self::resolve_target_language(lang, target_language);
        // 用户显式指定讲解语言时，在 system 追加覆盖指令（模板 system 可能固定了检测语言）
        let system = if explicit {
            format!(
                "{} 重要：讲解与练习必须使用「{}」。",
                template.system, target
            )
        } else {
            template.system.clone()
        };
        let vars = self.render_vars(
            error_type,
            attribution,
            cefr,
            student_history_count,
            native_language,
            error_distribution,
            &target,
        );
        let user_prompt = template.render(
            &vars
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect::<Vec<_>>(),
        );
        let client = client::LlmClient::new(&self.config);
        client.complete(&system, &user_prompt).await
    }

    /// 生成个性化讲解（SSE 流式）：逐 token 产出
    ///
    /// 返回 `(template, 流)`；流元素 `Ok(Some(token))`，流结束 `Ok(None)`。
    /// `target_language` 为讲解目标语言（如 `"English"`）；`None`/空时跟随检测语言。
    #[allow(clippy::too_many_arguments)]
    pub async fn stream_explain(
        &self,
        lang: &str,
        error_type: &str,
        attribution: &str,
        cefr: &str,
        student_history_count: u32,
        native_language: Option<&str>,
        error_distribution: Option<&str>,
        target_language: Option<&str>,
    ) -> Result<
        (
            PromptTemplate,
            BoxStream<'static, Result<Option<String>, LlmError>>,
        ),
        LlmError,
    > {
        if !self.config.enabled() {
            return Err(LlmError::NotConfigured);
        }
        let template = self
            .resolve_template(lang, cefr)
            .ok_or_else(|| LlmError::TemplateNotFound(lang.to_string()))?
            .clone();
        let (target, explicit) = Self::resolve_target_language(lang, target_language);
        // 用户显式指定讲解语言时，在 system 追加覆盖指令（模板 system 可能固定了检测语言）
        let system = if explicit {
            format!(
                "{} 重要：讲解与练习必须使用「{}」。",
                template.system, target
            )
        } else {
            template.system.clone()
        };
        let vars = self.render_vars(
            error_type,
            attribution,
            cefr,
            student_history_count,
            native_language,
            error_distribution,
            &target,
        );
        let user_prompt = template.render(
            &vars
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect::<Vec<_>>(),
        );
        let client = client::LlmClient::new(&self.config);
        let stream = client.stream(&system, &user_prompt).await?;
        Ok((template, stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_from_env_defaults() {
        // 测试环境不设置 LLM_API_KEY → 未启用
        unsafe { std::env::remove_var("LLM_API_KEY") };
        let config = LlmConfig::from_env();
        assert!(!config.enabled());
        assert_eq!(config.base_url, "https://api.openai.com/v1");
        assert_eq!(config.model, "gpt-4o-mini");
    }

    #[test]
    fn service_unavailable_without_key() {
        unsafe { std::env::remove_var("LLM_API_KEY") };
        let service = LlmService::from_env();
        // 无 API Key → 不可用
        assert!(!service.is_available());
    }

    #[test]
    fn template_fallback_to_generic() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../prompts");
        let store = TemplateStore::load(&dir);
        // 精确匹配
        assert!(store.for_lang_cefr("zh", "A2").is_some());
        // 无匹配（A3 无对应模板文件）→ generic 回退
        assert!(store.for_lang_cefr("zh", "A3").is_none());
        assert!(store.get("zh_generic").is_some());
        // 未知语言 → 无模板
        assert!(store.get("xx_generic").is_none());
    }

    #[test]
    fn render_vars_includes_profile() {
        let service = LlmService::from_env();
        let vars = service.render_vars(
            "spelling",
            "knowledge_gap",
            "A2",
            3,
            Some("en"),
            Some("spelling: 2, grammar: 1"),
            "中文",
        );
        let get = |k: &str| vars.iter().find(|(key, _)| key == k).unwrap().1.clone();
        assert_eq!(get("error_type"), "spelling");
        assert_eq!(get("attribution"), "knowledge_gap");
        assert_eq!(get("cefr"), "A2");
        assert_eq!(get("student_history_count"), "3");
        assert_eq!(get("native_language"), "en");
        assert_eq!(get("error_distribution"), "spelling: 2, grammar: 1");
        assert_eq!(get("target_language"), "中文");
    }

    #[test]
    fn render_vars_defaults_when_profile_missing() {
        let service = LlmService::from_env();
        let vars = service.render_vars("grammar", "unknown", "", 0, None, None, "中文");
        let get = |k: &str| vars.iter().find(|(key, _)| key == k).unwrap().1.clone();
        assert_eq!(get("cefr"), "A1");
        assert_eq!(get("native_language"), "unknown");
        assert_eq!(get("error_distribution"), "暂无历史记录");
        assert_eq!(get("target_language"), "中文");
    }

    #[test]
    fn resolve_target_language() {
        // 未显式指定 → 检测语言显示名
        assert_eq!(
            LlmService::resolve_target_language("zh", None),
            ("中文".to_string(), false)
        );
        assert_eq!(
            LlmService::resolve_target_language("en", None),
            ("English".to_string(), false)
        );
        assert_eq!(
            LlmService::resolve_target_language("ja", None),
            ("日本語".to_string(), false)
        );
        // 空白视为未指定
        assert_eq!(
            LlmService::resolve_target_language("zh", Some("  ")),
            ("中文".to_string(), false)
        );
        // 显式指定 → 优先且标记 explicit
        assert_eq!(
            LlmService::resolve_target_language("zh", Some("English")),
            ("English".to_string(), true)
        );
        // 未知语言 → 兜底文案
        assert_eq!(
            LlmService::resolve_target_language("xx", None),
            ("与检测语言一致".to_string(), false)
        );
    }

    #[test]
    fn config_serde_roundtrip() {
        // 设置界面持久化依赖 LlmConfig 的 JSON 序列化
        let config = LlmConfig {
            base_url: "http://127.0.0.1:9000/v1".into(),
            api_key: "sk-test".into(),
            model: "gpt-4o-mini".into(),
            timeout_secs: 60,
            prompts_dir: std::path::PathBuf::from("prompts"),
        };
        let json = serde_json::to_string(&config).unwrap();
        let back: LlmConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.base_url, config.base_url);
        assert_eq!(back.api_key, "sk-test");
        assert_eq!(back.model, config.model);
        assert_eq!(back.timeout_secs, 60);
        assert_eq!(back.prompts_dir, config.prompts_dir);
    }

    #[test]
    fn explain_not_configured() {
        // 无 LLM_API_KEY → NotConfigured（不发起网络请求）
        unsafe { std::env::remove_var("LLM_API_KEY") };
        let service = LlmService::from_env();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let err = rt
            .block_on(service.explain("zh", "spelling", "unknown", "A1", 0, None, None, None))
            .unwrap_err();
        assert!(matches!(err, LlmError::NotConfigured));
    }
}
