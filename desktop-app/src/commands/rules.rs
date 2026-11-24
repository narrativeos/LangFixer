use std::collections::HashMap;
use std::sync::Arc;

use super::core::Core;
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
