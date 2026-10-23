//! 可切换纠错模型注册表（CEC 工作流 E）
//!
//! LLM 兜底层保持 BYO-OpenAI-兼容端点不变，但把「用哪个模型」做成可切换：
//! 注册表（`prompts/models.json`，与 `*_prompt.json` 同目录、同热更新机制）描述每个
//! 预设模型的 `base_url` / `model` / `prompt_template` / `output_format` / 生成参数；
//! 设置界面按 `id` 选择，切换即时生效（复用现有 LLM 参数热更新机制）。
//!
//! 每个条目的 `output_format` 决定 [`crate::llm::LlmGecEngine`] 如何解析响应：
//! - [`OUTPUT_JSON`]（`generic`）：现有 JSON 数组契约（`zh_detect` v1.2 prompt）；
//! - [`OUTPUT_PLAIN`]（`cec3` / `cec4` / 自训 COT 模型）：模型输出「think 块 + 纠正句」
//!   纯文本，引擎剥离 think 块、difflib 对齐生成 span、解析错误类型/原因。
//!
//! 注册表是「预设」：选中某条目后，其 `base_url` / `model` / `output_format` /
//! `max_tokens` 覆盖运行时 LLM 参数（`api_key` 仍来自用户 LLM 配置；本地 vllm 端点
//! 通常无需 key）。`generic` 条目的 `base_url` / `model` 为空，表示沿用用户当前端点。

use std::path::Path;

use serde::{Deserialize, Serialize};

/// 输出格式：JSON 数组契约（generic）
pub const OUTPUT_JSON: &str = "json";
/// 输出格式：think 块 + 纠正句纯文本（cec3 / cec4 / 自训 COT 模型）
pub const OUTPUT_PLAIN: &str = "plain";

/// 注册表中的单个模型预设
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelEntry {
    /// 唯一标识（设置界面选择器用；`generic` / `cec4` / `cec3` / `custom`）
    pub id: String,
    /// 显示名（设置界面展示）
    pub name: String,
    /// 建议的 OpenAI 兼容端点（空 = 沿用用户当前 LLM 端点）
    #[serde(default)]
    pub base_url: String,
    /// 建议的模型名（空 = 沿用用户当前模型名）
    #[serde(default)]
    pub model: String,
    /// 检测提示词模板键（`{lang}_detect` / `zh_detect_cec`，对应
    /// `prompts/{lang}_detect_prompt.json` / `prompts/zh_detect_cec_prompt.json`；
    /// 空 = 语言级 detect 模板）
    #[serde(default)]
    pub prompt_template: String,
    /// 输出格式（[`OUTPUT_JSON`] / [`OUTPUT_PLAIN`]）
    #[serde(default = "default_output_format")]
    pub output_format: String,
    /// 生成 max_tokens（0 = 用服务端默认；CEC4 需 ≥ 1024 容纳 think 块）
    #[serde(default)]
    pub max_tokens: u32,
    /// 是否启用（设置界面选择器只列 enabled 条目）
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_output_format() -> String {
    OUTPUT_JSON.to_string()
}
fn default_enabled() -> bool {
    true
}

/// 模型注册表（`prompts/models.json`）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelRegistry {
    /// 注册表版本
    #[serde(default)]
    pub version: String,
    /// 模型预设列表
    #[serde(default)]
    pub models: Vec<ModelEntry>,
}

impl ModelRegistry {
    /// 从 `models.json` 加载；文件缺失 / 解析失败时返回空注册表（调用方回退 generic）。
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path)
            .ok()
            .and_then(|c| serde_json::from_str::<ModelRegistry>(&c).ok())
        {
            Some(r) => {
                tracing::debug!(
                    path = %path.display(),
                    count = r.models.len(),
                    "加载纠错模型注册表"
                );
                r
            }
            None => {
                tracing::debug!(path = %path.display(), "模型注册表缺失，使用空注册表");
                Self::default()
            }
        }
    }

    /// 按 `id` 查找条目
    pub fn get(&self, id: &str) -> Option<&ModelEntry> {
        self.models.iter().find(|m| m.id == id)
    }

    /// 启用的条目（设置界面选择器列表）
    pub fn enabled(&self) -> Vec<&ModelEntry> {
        self.models.iter().filter(|m| m.enabled).collect()
    }

    /// 按 `id` 解析输出格式（未知 / 缺省 → [`OUTPUT_JSON`]）
    pub fn output_format_for(&self, id: &str) -> &'static str {
        match self.get(id).map(|m| m.output_format.as_str()) {
            Some(OUTPUT_PLAIN) => OUTPUT_PLAIN,
            _ => OUTPUT_JSON,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_registry(dir: &Path) -> std::path::PathBuf {
        let r = ModelRegistry {
            version: "1.0.0".into(),
            models: vec![
                ModelEntry {
                    id: "generic".into(),
                    name: "通用".into(),
                    base_url: String::new(),
                    model: String::new(),
                    prompt_template: "zh_detect".into(),
                    output_format: OUTPUT_JSON.into(),
                    max_tokens: 0,
                    enabled: true,
                },
                ModelEntry {
                    id: "cec4".into(),
                    name: "CEC4".into(),
                    base_url: "http://127.0.0.1:8000/v1".into(),
                    model: "twnlp/ChineseErrorCorrector4-4B".into(),
                    prompt_template: "zh_detect".into(),
                    output_format: OUTPUT_PLAIN.into(),
                    max_tokens: 2048,
                    enabled: true,
                },
                ModelEntry {
                    id: "off".into(),
                    name: "停用".into(),
                    base_url: String::new(),
                    model: String::new(),
                    prompt_template: String::new(),
                    output_format: OUTPUT_PLAIN.into(),
                    max_tokens: 0,
                    enabled: false,
                },
            ],
        };
        let p = dir.join("models.json");
        std::fs::write(&p, serde_json::to_string_pretty(&r).unwrap()).unwrap();
        p
    }

    #[test]
    fn load_lookup_and_enabled() {
        let dir = std::env::temp_dir().join("langfixer_models_test");
        std::fs::create_dir_all(&dir).unwrap();
        let p = write_registry(&dir);
        let r = ModelRegistry::load(&p);
        assert_eq!(r.models.len(), 3);
        assert_eq!(r.get("generic").unwrap().output_format, OUTPUT_JSON);
        assert_eq!(r.get("cec4").unwrap().output_format, OUTPUT_PLAIN);
        assert_eq!(r.get("cec4").unwrap().max_tokens, 2048);
        assert!(r.get("nope").is_none());
        // 选择器只列 enabled 条目
        let ids: Vec<&str> = r.enabled().iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["generic", "cec4"]);
        assert_eq!(r.output_format_for("cec4"), OUTPUT_PLAIN);
        assert_eq!(r.output_format_for("generic"), OUTPUT_JSON);
        assert_eq!(r.output_format_for("unknown"), OUTPUT_JSON);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_yields_empty_registry() {
        let r = ModelRegistry::load(Path::new("/nonexistent/models.json"));
        assert!(r.models.is_empty());
        assert_eq!(r.output_format_for("cec4"), OUTPUT_JSON);
    }

    #[test]
    fn serde_defaults_for_partial_entry() {
        // 只有 id/name 的条目也能解析（其余字段走默认）
        let e: ModelEntry = serde_json::from_str(r#"{"id":"x","name":"X"}"#).unwrap();
        assert_eq!(e.output_format, OUTPUT_JSON);
        assert!(e.enabled);
        assert_eq!(e.max_tokens, 0);
    }
}
