//! 检测历史 API：查询 / 删除
//!
//! - GET  /gec/history?session_id=&lang=&limit=&offset=&q= — 分页列出历史
//!   （响应 `{ items, total, offset, limit }`；q = 全文关键词字面子串匹配）
//! - GET  /gec/history/:id                       — 查看单条详情
//! - DELETE /gec/history/:id                     — 删除单条
//! - DELETE /gec/history                         — 清空全部

use std::sync::Arc;

use axum::extract::{Extension, Path, Query, State};
use serde::{Deserialize, Serialize};

use crate::auth::Tenant;
use crate::routes::ApiError;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct HistoryQuery {
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub lang: Option<String>,
    /// 全文关键词（字面子串，通配符转义；空/缺省 = 不过滤）
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: i64,
    /// 跳过行数（页偏移；负数按 0 处理）
    #[serde(default)]
    pub offset: i64,
}

fn default_limit() -> i64 {
    50
}

/// 列表响应（不含完整 text 和 errors_json，避免列表过大）
#[derive(Serialize)]
pub struct HistoryListItem {
    pub id: i64,
    pub session_id: Option<String>,
    pub lang: String,
    /// 文本前 80 字（预览）
    pub text_preview: String,
    /// 错误数量
    pub error_count: usize,
    /// 检测总耗时（毫秒；从 timings_json 解析；无计时记录时为 null）
    pub total_ms: Option<u64>,
    /// 产生该记录时的程序版本号（旧记录无版本时为 null）
    pub version: Option<String>,
    /// 检测时的参数快照（JSON 序列化的 DetectParams；旧记录为 null）——
    /// 前端据此展示参数标签（level / LLM / 规则指纹）
    pub params_json: Option<String>,
    pub created_at: String,
}

/// 列表分页响应：items（当前页）+ total（同过滤口径总数）+ 回显 offset/limit
#[derive(Serialize)]
pub struct HistoryListPage {
    pub items: Vec<HistoryListItem>,
    pub total: i64,
    pub offset: i64,
    pub limit: i64,
}

/// GET /gec/history — 分页列出（limit 钳制 1..=200；q 全文关键词；offset 页偏移）
///
/// 网关形态：先扫近 200 行按会话归属过滤（无归属行 fail-closed 隐藏）再内存分页；
/// 单机形态保持数据库分页原路径。
pub async fn list(
    State(state): State<Arc<AppState>>,
    Extension(tenant): Extension<Tenant>,
    Query(q): Query<HistoryQuery>,
) -> Result<impl axum::response::IntoResponse, ApiError> {
    let limit = q.limit.clamp(1, 200);
    let offset = q.offset.max(0);
    if let Some(sid) = q.session_id.as_deref() {
        tenant.guard_session(sid)?;
    }
    if tenant.gateway_active() {
        let scanned = state
            .detect_history
            .list_page(
                q.session_id.as_deref(),
                q.lang.as_deref(),
                q.q.as_deref(),
                200,
                0,
            )
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        let mut visible = Vec::with_capacity(scanned.len());
        for h in scanned {
            if tenant.may_see_session(h.session_id.as_deref())? {
                visible.push(to_list_item(h));
            }
        }
        let total = visible.len() as i64;
        let items = visible
            .into_iter()
            .skip(offset as usize)
            .take(limit as usize)
            .collect();
        return Ok(axum::Json(HistoryListPage {
            items,
            total,
            offset,
            limit,
        }));
    }
    let items = state
        .detect_history
        .list_page(
            q.session_id.as_deref(),
            q.lang.as_deref(),
            q.q.as_deref(),
            limit,
            offset,
        )
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let total = state
        .detect_history
        .count_page(q.session_id.as_deref(), q.lang.as_deref(), q.q.as_deref())
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let list: Vec<HistoryListItem> = items.into_iter().map(to_list_item).collect();

    Ok(axum::Json(HistoryListPage {
        items: list,
        total,
        offset,
        limit,
    }))
}

fn to_list_item(h: crate::detect_history::DetectHistory) -> HistoryListItem {
    let error_count = serde_json::from_str::<Vec<serde_json::Value>>(&h.errors_json)
        .map(|v| v.len())
        .unwrap_or(0);
    let preview: String = h.text.chars().take(80).collect();
    let total_ms = h
        .timings_json
        .as_deref()
        .and_then(|j| serde_json::from_str::<gec_core::DetectTimings>(j).ok())
        .map(|t| t.total_ms);
    HistoryListItem {
        id: h.id,
        session_id: h.session_id,
        lang: h.lang,
        text_preview: preview,
        error_count,
        total_ms,
        version: h.version,
        params_json: h.params_json,
        created_at: h.created_at,
    }
}

/// GET /gec/history/:id
pub async fn get(
    State(state): State<Arc<AppState>>,
    Extension(tenant): Extension<Tenant>,
    Path(id): Path<i64>,
) -> Result<impl axum::response::IntoResponse, ApiError> {
    let item = state
        .detect_history
        .get(id)
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::NotFound(format!("history#{id} 不存在")))?;
    // 网关形态：非本账号归属的记录按不存在处理（不泄露存在性）
    if !tenant.may_see_session(item.session_id.as_deref())? {
        return Err(ApiError::NotFound(format!("history#{id} 不存在")));
    }
    Ok(axum::Json(item))
}

/// DELETE /gec/history/:id
pub async fn remove(
    State(state): State<Arc<AppState>>,
    Extension(tenant): Extension<Tenant>,
    Path(id): Path<i64>,
) -> Result<impl axum::response::IntoResponse, ApiError> {
    let item = state
        .detect_history
        .get(id)
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::NotFound(format!("history#{id} 不存在")))?;
    if !tenant.may_see_session(item.session_id.as_deref())? {
        return Err(ApiError::NotFound(format!("history#{id} 不存在")));
    }
    let ok = state
        .detect_history
        .remove(id)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    if !ok {
        return Err(ApiError::NotFound(format!("history#{id} 不存在")));
    }
    Ok(axum::Json(serde_json::json!({ "deleted": true })))
}

/// DELETE /gec/history — 清空全部（网关形态拒绝：会波及他账号数据）
pub async fn clear(
    State(state): State<Arc<AppState>>,
    Extension(tenant): Extension<Tenant>,
) -> Result<impl axum::response::IntoResponse, ApiError> {
    tenant.deny_gateway("清空全部历史")?;
    let n = state
        .detect_history
        .clear()
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(axum::Json(serde_json::json!({ "deleted": n })))
}
