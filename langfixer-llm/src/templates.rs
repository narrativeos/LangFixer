//! 外置提示词模板
//!
//! 模板文件为 JSON（`{dir}/*_prompt.json`），命名约定 `{lang}_{cefr}_prompt.json`
//! （如 `zh_a2_prompt.json`）或 `{lang}_generic_prompt.json`（语言级通用模板）。
//! 另有 `{lang}_detect_prompt.json`（`cefr: "detect"`）为 LLM 兜底**检测**模板
//! （三级响应 level 2/3 使用；与讲解模板分离，由 `gec-core::LlmGecEngine` 消费）。
//! 模板按版本管理（`version` 字段），修改模板文件即可热更新提示词，无需重新编译。

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// 提示词模板
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptTemplate {
    /// 模板版本（如 "1.0.0"）
    pub version: String,
    /// 目标语言（zh / en / ja）
    pub lang: String,
    /// CEFR 分级（A1-C2；通用模板为 "generic"）
    pub cefr: String,
    /// system 提示词
    pub system: String,
    /// user 提示词（支持 `{{var}}` 占位符）
    pub user: String,
}

impl PromptTemplate {
    /// 渲染 user 提示词：`{{name}}` 替换为对应变量值
    pub fn render(&self, vars: &[(&str, &str)]) -> String {
        let mut out = self.user.clone();
        for (k, v) in vars {
            // 注意：format 字符串中 `{{`/`}}` 是转义，需 `{{{{{}}}}}` 才能得到字面量 `{{k}}`
            out = out.replace(&format!("{{{{{}}}}}", k), v);
        }
        out
    }
}

/// 模板库（按 `lang_cefr` 键索引）
#[derive(Debug, Clone, Default)]
pub struct TemplateStore {
    templates: HashMap<String, PromptTemplate>,
}

impl TemplateStore {
    /// 从目录加载所有 `*_prompt.json` 模板
    pub fn load(dir: &Path) -> Self {
        let mut store = Self::default();
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) => {
                tracing::debug!(dir = %dir.display(), error = %e, "提示词模板目录不存在");
                return store;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let is_prompt = path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.ends_with("_prompt.json"))
                .unwrap_or(false);
            if !is_prompt {
                continue;
            }
            match std::fs::read_to_string(&path)
                .ok()
                .and_then(|content| serde_json::from_str::<PromptTemplate>(&content).ok())
            {
                Some(t) => {
                    let key = format!("{}_{}", t.lang, t.cefr);
                    tracing::debug!(key = %key, version = %t.version, "加载提示词模板");
                    store.templates.insert(key, t);
                }
                None => tracing::warn!(path = %path.display(), "提示词模板解析失败，已跳过"),
            }
        }
        store
    }

    /// 按语言 + CEFR 精确查找
    pub fn for_lang_cefr(&self, lang: &str, cefr: &str) -> Option<&PromptTemplate> {
        self.templates.get(&format!("{lang}_{cefr}"))
    }

    /// 按 `lang_cefr` 键查找
    pub fn get(&self, key: &str) -> Option<&PromptTemplate> {
        self.templates.get(key)
    }

    pub fn is_empty(&self) -> bool {
        self.templates.is_empty()
    }

    pub fn len(&self) -> usize {
        self.templates.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_template(dir: &Path, name: &str, lang: &str, cefr: &str) {
        let t = PromptTemplate {
            version: "1.0.0".into(),
            lang: lang.into(),
            cefr: cefr.into(),
            system: "你是语言老师".into(),
            user: "错误类型：{{error_type}}，归因：{{attribution}}，水平：{{cefr}}".into(),
        };
        std::fs::write(dir.join(name), serde_json::to_string_pretty(&t).unwrap()).unwrap();
    }

    #[test]
    fn repo_prompts_have_ingest_templates() {
        // 仓库 prompts/ 目录：en/zh/ja 三语 ingest 模板必须存在且占位符齐全
        // （知识源挖掘循环渲染依赖这些占位符）
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../prompts");
        let store = TemplateStore::load(&dir);
        for lang in ["en", "zh", "ja"] {
            let t = store
                .for_lang_cefr(lang, "ingest")
                .unwrap_or_else(|| panic!("{lang} 缺少 ingest 模板"));
            assert!(!t.system.is_empty(), "{lang} ingest 模板 system 为空");
            for ph in [
                "{{source_id}}",
                "{{chunk_index}}",
                "{{total_chunks}}",
                "{{chunk}}",
            ] {
                assert!(t.user.contains(ph), "{lang} ingest 模板缺占位符 {ph}");
            }
        }
    }

    #[test]
    fn load_and_lookup() {
        let dir = std::env::temp_dir().join("langfixer_llm_templates_test");
        std::fs::create_dir_all(&dir).unwrap();
        write_template(&dir, "zh_a2_prompt.json", "zh", "A2");
        write_template(&dir, "en_b1_prompt.json", "en", "B1");
        // 非模板文件应被忽略
        std::fs::write(dir.join("notes.txt"), "ignore me").unwrap();

        let store = TemplateStore::load(&dir);
        assert_eq!(store.len(), 2);
        assert!(store.for_lang_cefr("zh", "A2").is_some());
        assert!(store.for_lang_cefr("en", "B1").is_some());
        assert!(store.for_lang_cefr("zh", "B1").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn render_replaces_placeholders() {
        let t = PromptTemplate {
            version: "1.0.0".into(),
            lang: "zh".into(),
            cefr: "A2".into(),
            system: "sys".into(),
            user: "类型 {{error_type}} / 归因 {{attribution}} / 水平 {{cefr}} / 历史 {{student_history_count}} 次".into(),
        };
        let out = t.render(&[
            ("error_type", "spelling"),
            ("attribution", "knowledge_gap"),
            ("cefr", "A2"),
            ("student_history_count", "3"),
        ]);
        assert_eq!(
            out,
            "类型 spelling / 归因 knowledge_gap / 水平 A2 / 历史 3 次"
        );
    }
}
