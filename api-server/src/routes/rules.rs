//! 规则管理 API：列表（分页/搜索/分类过滤）/ 开关 / 示例列表
//!
//! 数据源为 `lexicon-store`（SQLite）：规则定义 + 示例 + 运行时开关
//! 全部入库，不再读写 JSON 词表 / JSON 开关文件。

use std::collections::HashMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

use crate::routes::ApiError;
use crate::state::AppState;

/// 规则开关状态回写词表文件（best effort：失败仅告警——
/// DB 开关在检测路径已即时生效；文件同步是为防漂移：
/// 重新播种（空库）时文件默认值会把已停用规则静默重新启用）
fn sync_rule_toggles(state: &AppState, lang: &str) {
    let settings = state.settings.read().unwrap().clone();
    let lexicons_dir = std::path::PathBuf::from(&settings.lexicons_dir);
    let file = lexicons_dir.join(format!("{lang}_gec_lexicon.json"));
    if !file.exists() {
        return;
    }
    if let Err(e) = state.lexicon_store.sync_rules_to_file(&lexicons_dir, lang) {
        tracing::warn!(
            error = %e,
            lang,
            "规则开关状态回写词表文件失败（DB 开关已生效）"
        );
    }
}

#[derive(Debug, Serialize, Deserialize)]
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

#[derive(Debug, Serialize, Deserialize)]
pub struct CategoryInfo {
    pub id: String,
    /// 词表存储未存分类名（仅有 category id），暂以 id 作展示名
    pub name: String,
    pub rule_count: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RulesResponse {
    pub lang: String,
    /// 满足过滤条件（category / q）的规则总数
    pub total: usize,
    pub page: usize,
    pub page_size: usize,
    pub categories: Vec<CategoryInfo>,
    pub rules: Vec<RuleInfo>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ToggleResponse {
    pub id: String,
    pub enabled: bool,
}

/// 规则列表查询参数：`lang` 必填，其余可选
#[derive(Deserialize)]
pub struct RulesQuery {
    pub lang: String,
    #[serde(default)]
    pub category: Option<String>,
    /// 搜索（FTS5 全文，无结果时回退 LIKE）
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub page: Option<usize>,
    #[serde(default)]
    pub page_size: Option<usize>,
}

/// GET /gec/rules?lang=zh[&category=..&q=..&page=1&page_size=50]
///
/// 数据来自 SQLite 词表存储（启动时已从词表目录播种）。
pub async fn list_rules(
    State(state): State<Arc<AppState>>,
    Query(params): Query<RulesQuery>,
) -> Result<Json<RulesResponse>, ApiError> {
    let page = params.page.unwrap_or(1).max(1);
    let page_size = params.page_size.unwrap_or(50);
    // 未知语言 → 404（该语言词表未入库）
    let defs = state
        .lexicon_store
        .rule_defs(&params.lang)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    if defs.is_empty() {
        return Err(ApiError::NotFound(format!(
            "语言 {} 无规则词表",
            params.lang
        )));
    }
    let list = state
        .lexicon_store
        .list_rules(
            &params.lang,
            params.category.as_deref(),
            params.q.as_deref(),
            page,
            page_size,
        )
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let cats = state
        .lexicon_store
        .list_categories(&params.lang)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(RulesResponse {
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
    }))
}

/// 单条规则开关查询参数（`enabled` 缺省视为 true，兼容旧调用）
#[derive(Deserialize)]
pub struct ToggleQuery {
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

/// PUT /gec/rules/:id/toggle?enabled=false — 切换单条规则开关
///
/// 规则 id 全局唯一（命名约定带语言前缀），无需再传 `lang`。
pub async fn toggle_rule(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<ToggleQuery>,
) -> Result<(StatusCode, Json<ToggleResponse>), ApiError> {
    let def = state
        .lexicon_store
        .rule_defs_by_id(&id)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let Some(def) = def else {
        return Err(ApiError::NotFound(format!("规则 {id} 不存在")));
    };
    state
        .lexicon_store
        .toggle_rule(&def.lang, &def.id, params.enabled)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    sync_rule_toggles(&state, &def.lang);
    Ok((
        StatusCode::OK,
        Json(ToggleResponse {
            id,
            enabled: params.enabled,
        }),
    ))
}

/// PUT /gec/rules/:lang/toggles — 批量设置规则开关
///
/// body：`{ toggles: { "<rule_id>": true|false, ... } }`
/// 每个 id 必须存在于该语言词表（否则 404）。返回该语言全部规则的当前开关状态。
pub async fn put_toggles(
    State(state): State<Arc<AppState>>,
    Path(lang): Path<String>,
    Json(req): Json<PutTogglesRequest>,
) -> Result<Json<PutTogglesResponse>, ApiError> {
    if req.toggles.is_empty() {
        return Err(ApiError::BadRequest("toggles 为空".into()));
    }
    let defs = state
        .lexicon_store
        .rule_defs(&lang)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let known: std::collections::HashSet<&str> = defs.iter().map(|d| d.id.as_str()).collect();
    for id in req.toggles.keys() {
        if !known.contains(id.as_str()) {
            return Err(ApiError::NotFound(format!("规则 {id} 不存在")));
        }
    }
    for (id, enabled) in &req.toggles {
        state
            .lexicon_store
            .toggle_rule(&lang, id, *enabled)
            .map_err(|e| ApiError::Internal(e.to_string()))?;
    }
    sync_rule_toggles(&state, &lang);
    // 更新后重新读取全部规则，响应反映最新开关状态
    let defs = state
        .lexicon_store
        .rule_defs(&lang)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let toggles: HashMap<String, bool> = defs.into_iter().map(|d| (d.id, d.enabled)).collect();
    Ok(Json(PutTogglesResponse {
        lang,
        count: toggles.len(),
        toggles,
    }))
}

#[derive(Deserialize)]
pub struct PutTogglesRequest {
    pub toggles: HashMap<String, bool>,
}

#[derive(Serialize)]
pub struct PutTogglesResponse {
    pub lang: String,
    /// 该语言规则总数
    pub count: usize,
    /// 全部规则当前开关状态（id → enabled）
    pub toggles: HashMap<String, bool>,
}

/// 规则示例列表查询参数
#[derive(Deserialize)]
pub struct ExamplesQuery {
    pub lang: String,
    #[serde(default)]
    pub page: Option<usize>,
    #[serde(default)]
    pub page_size: Option<usize>,
}

/// GET /gec/rules/:id/examples?lang=zh[&page=1&page_size=50]
pub async fn list_examples(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<ExamplesQuery>,
) -> Result<Json<lexicon_store::ExampleList>, ApiError> {
    let list = state
        .lexicon_store
        .list_rule_examples(
            &params.lang,
            &id,
            params.page.unwrap_or(1),
            params.page_size.unwrap_or(50),
        )
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(list))
}
