//! 决策表管理 API
//!
//! - `GET /gec/tables` — 列表（真实元数据：规则数 + 加载/更新时间）
//! - `GET /gec/tables/:lang` — 原始 JDM JSON
//! - `PUT /gec/tables/:lang` — 热更新（校验失败保留旧表；成功后自动存版本）
//! - `POST /gec/tables/:lang/validate` — dry-run 校验（不替换表，返回具体错误）
//! - `POST /gec/tables/:lang/try` — 用给定上下文对当前内存表试算（验证行行为）
//! - `GET /gec/tables/:lang/versions` — 版本历史（新 → 旧）
//! - `POST /gec/tables/:lang/rollback` — 回滚到指定版本（重新走校验 + 原子替换）

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use serde::{Deserialize, Serialize};
use shared_model::{AttributionContext, AttributionResult, DecisionTableMeta, DecisionTableView};

use crate::routes::ApiError;
use crate::state::AppState;
use crate::table_history::TableVersionMeta;

/// GET /gec/tables — 列出所有决策表
pub async fn list(State(state): State<Arc<AppState>>) -> Json<Vec<DecisionTableMeta>> {
    Json(state.tables.list())
}

/// GET /gec/tables/:lang — 获取指定语言决策表（真实元数据）
pub async fn get(
    State(state): State<Arc<AppState>>,
    Path(lang): Path<String>,
) -> Result<Json<DecisionTableView>, ApiError> {
    let raw = state
        .tables
        .get_raw(&lang)
        .ok_or_else(|| ApiError::NotFound(format!("语言 {lang} 无决策表")))?;
    let meta = state.tables.get_meta(&lang).unwrap_or(DecisionTableMeta {
        lang: lang.clone(),
        version: "1.0.0".into(),
        updated_at: chrono::Utc::now().to_rfc3339(),
        rule_count: 0,
    });
    Ok(Json(DecisionTableView { meta, raw }))
}

/// PUT /gec/tables/:lang — 更新决策表（热加载；校验失败时保留旧表）
///
/// 设置了 `GEC_TABLES_UPDATE_TOKEN` 时，请求必须携带匹配的
/// `X-Tables-Update-Token` 头，否则返回 401。
/// 更新成功后自动保存一个版本（供版本历史 / 回滚）。
pub async fn update(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(lang): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<DecisionTableMeta>, ApiError> {
    check_token(&state, &headers)?;
    let table_json = match body {
        serde_json::Value::String(s) => s,
        other => other.to_string(),
    };
    let meta = state.tables.update(&lang, &table_json)?;
    // 成功后存档（版本保存失败不影响更新本身，仅告警）
    if let Err(e) = state
        .table_history
        .save(&lang, meta.rule_count, &table_json)
    {
        tracing::warn!(lang = %lang, error = %e, "决策表版本存档失败");
    }
    Ok(Json(meta))
}

fn check_token(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    if let Some(expected) = &state.tables_update_token {
        let provided = headers
            .get("x-tables-update-token")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if provided != expected.as_str() {
            return Err(ApiError::Unauthorized(
                "更新决策表需提供有效的 X-Tables-Update-Token".into(),
            ));
        }
    }
    Ok(())
}

/// POST /gec/tables/:lang/validate — dry-run 校验（不替换当前表）
///
/// body 为待校验的 JDM JSON（字符串或对象均可）。
/// 返回 `{ ok, errors }`：前端保存前调用，把「校验失败保留旧表」提前为可见错误。
pub async fn validate(
    _state: State<Arc<AppState>>,
    Path(_lang): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Json<ValidateResponse> {
    let table_json = match body {
        serde_json::Value::String(s) => s,
        other => other.to_string(),
    };
    let errors = match attribution_engine::AttributionEngine::validate(&table_json) {
        Ok(()) => Vec::new(),
        Err(e) => vec![e.to_string()],
    };
    Json(ValidateResponse {
        ok: errors.is_empty(),
        errors,
    })
}

#[derive(Serialize)]
pub struct ValidateResponse {
    pub ok: bool,
    pub errors: Vec<String>,
}

/// POST /gec/tables/:lang/try — 用给定上下文对**当前内存表**试算
///
/// body 为 [`AttributionContext`]（`lang` 字段以路径 `:lang` 为准）。
/// 返回完整归因结果（含 `trace.matched_rule`），供「改行 → 试算 → 确认」闭环。
pub async fn try_(
    State(state): State<Arc<AppState>>,
    Path(lang): Path<String>,
    Json(mut ctx): Json<AttributionContext>,
) -> Result<Json<AttributionResult>, ApiError> {
    let engine = state
        .tables
        .get(&lang)
        .ok_or_else(|| ApiError::NotFound(format!("语言 {lang} 无决策表")))?;
    ctx.lang = lang;
    let result = engine.attribute(&ctx).await?;
    Ok(Json(result))
}

/// GET /gec/tables/:lang/versions — 版本历史（新 → 旧，默认最多 50 条）
pub async fn versions(
    State(state): State<Arc<AppState>>,
    Path(lang): Path<String>,
    Query(query): Query<VersionQuery>,
) -> Result<Json<Vec<TableVersionMeta>>, ApiError> {
    let limit = query.limit.clamp(1, 200);
    state
        .table_history
        .list(&lang, limit)
        .map(Json)
        .map_err(|e| ApiError::Internal(e.to_string()))
}

#[derive(Deserialize)]
pub struct VersionQuery {
    #[serde(default = "default_version_limit")]
    pub limit: usize,
}

fn default_version_limit() -> usize {
    50
}

/// POST /gec/tables/:lang/rollback — 回滚到指定版本
///
/// 把历史版本内容重新走 `TableStore::update`（校验 + 原子替换），
/// 回滚本身也会产生新版本。需要与 PUT 相同的 token 鉴权。
pub async fn rollback(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(lang): Path<String>,
    Json(req): Json<RollbackRequest>,
) -> Result<Json<DecisionTableMeta>, ApiError> {
    check_token(&state, &headers)?;
    let version = state
        .table_history
        .get(req.version)
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::NotFound(format!("版本 {} 不存在", req.version)))?;
    if version.lang != lang {
        return Err(ApiError::BadRequest(format!(
            "版本 {} 属于语言 {}，与路径 {lang} 不符",
            version.id, version.lang
        )));
    }
    let meta = state.tables.update(&lang, &version.content)?;
    if let Err(e) = state
        .table_history
        .save(&lang, meta.rule_count, &version.content)
    {
        tracing::warn!(lang = %lang, error = %e, "回滚版本存档失败");
    }
    Ok(Json(meta))
}

#[derive(Deserialize)]
pub struct RollbackRequest {
    pub version: i64,
}
