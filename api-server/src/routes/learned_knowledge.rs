//! 学习知识管理 API（向量召回知识条目的 CRUD）
//!
//! - `GET /gec/learned-knowledge?lang=` — 列出某语言所有知识条目
//! - `POST /gec/learned-knowledge` — 手动添加知识条目（自动调嵌入引擎生成向量）
//! - `DELETE /gec/learned-knowledge/:id` — 删除指定条目

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use serde::{Deserialize, Serialize};

use crate::routes::ApiError;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct ListQuery {
    pub lang: String,
}

/// GET /gec/learned-knowledge?lang=
pub async fn list(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ListQuery>,
) -> Result<impl axum::response::IntoResponse, ApiError> {
    let items = state
        .learned_knowledge
        .list(&q.lang)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(axum::Json(items))
}

/// POST /gec/learned-knowledge — 添加知识条目
#[derive(Deserialize)]
pub struct AddRequest {
    pub lang: String,
    /// 含错误的原始文本片段
    pub text: String,
    /// 错误类型（如 article / tense / spelling）
    pub error_type: String,
    /// 修正文本
    #[serde(default)]
    pub correction: String,
    /// 来源（`manual` / `llm_adopt` / `llm_distill`）
    #[serde(default = "default_source")]
    pub source: String,
}

fn default_source() -> String {
    "manual".to_string()
}

#[derive(Serialize)]
pub struct AddResponse {
    pub id: i64,
}

pub async fn add(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AddRequest>,
) -> Result<Json<AddResponse>, ApiError> {
    // 生成嵌入向量（未配置嵌入引擎时使用零向量占位）
    // 注意：guard 不可跨 await（非 Send）——先克隆 Arc 再立即释放
    let engine = {
        let guard = state.embedding_engine.read().unwrap();
        guard.as_ref().cloned()
    };
    let embedding = match engine.as_deref() {
        Some(engine) => engine
            .embed(&req.text)
            .await
            .map_err(ApiError::ServiceUnavailable)?,
        None => {
            tracing::debug!("嵌入引擎未配置，知识条目使用空向量（不参与向量召回）");
            vec![]
        }
    };
    let id = state
        .learned_knowledge
        .insert(
            &req.lang,
            &req.text,
            &req.error_type,
            &req.correction,
            &embedding,
            &req.source,
        )
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(AddResponse { id }))
}

/// DELETE /gec/learned-knowledge/:id
pub async fn remove(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> Result<impl axum::response::IntoResponse, ApiError> {
    let ok = state
        .learned_knowledge
        .remove(id)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    if !ok {
        return Err(ApiError::NotFound(format!("知识条目 #{id} 不存在")));
    }
    Ok(axum::Json(serde_json::json!({ "deleted": true })))
}

// ─────────────────────────────────────────────────────────────────────────────
// 知识召回测试（「沉淀为知识」流程：规则构建失败后 UI 展示 + 用户保留/放弃）
// ─────────────────────────────────────────────────────────────────────────────

/// POST /gec/knowledge/recall-test 请求体
#[derive(Deserialize)]
pub struct RecallTestRequest {
    pub lang: String,
    /// 全文（生成查询向量）
    pub text: String,
    /// 候选知识（UI 展示的内容；仅 `text` 参与相似度计算）
    pub candidate: RecallCandidate,
}

/// 召回测试只消费 `text`（生成嵌入向量）；`error_type` / `correction`
/// 随请求透传以保持与知识条目同构（用户「保留」时由 UI 另行提交入库）。
#[derive(Deserialize)]
#[allow(dead_code)]
pub struct RecallCandidate {
    /// 含错误的原始文本片段
    pub text: String,
    /// 错误类型（如 article / tense / spelling）
    #[serde(default)]
    pub error_type: String,
    /// 修正文本
    #[serde(default)]
    pub correction: String,
}

#[derive(Serialize)]
pub struct RecallTestResponse {
    /// 余弦相似度（`null` = 无法测试）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub similarity: Option<f64>,
    /// 召回阈值（与 ②.5 知识召回层一致）
    pub threshold: f64,
    /// 是否预期命中（`null` = 无法测试：未配置嵌入服务 / 嵌入调用失败）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub would_hit: Option<bool>,
    pub note: String,
}

/// POST /gec/knowledge/recall-test — 知识召回测试
///
/// 给定全文与候选知识，计算余弦相似度并判定 ②.5 知识召回层是否会命中
/// （阈值 0.7，与检测流水线一致）。
pub async fn recall_test(
    State(state): State<Arc<AppState>>,
    Json(req): Json<RecallTestRequest>,
) -> Result<Json<RecallTestResponse>, ApiError> {
    const THRESHOLD: f64 = 0.7;
    // guard 不可跨 await（非 Send）——先克隆 Arc 再立即释放
    let engine = {
        let guard = state.embedding_engine.read().unwrap();
        guard.as_ref().cloned()
    };
    let engine = match engine {
        Some(engine) => engine,
        None => {
            return Ok(Json(RecallTestResponse {
                similarity: None,
                threshold: THRESHOLD,
                would_hit: None,
                note: "未配置嵌入服务，无法测试召回效果".to_string(),
            }));
        }
    };
    let (query_emb, cand_emb) = match (
        engine.embed(&req.text).await,
        engine.embed(&req.candidate.text).await,
    ) {
        (Ok(a), Ok(b)) => (a, b),
        (a, b) => {
            let e = a
                .err()
                .or(b.err())
                .unwrap_or_else(|| "嵌入失败".to_string());
            tracing::warn!(error = %e, "召回测试：嵌入调用失败");
            return Ok(Json(RecallTestResponse {
                similarity: None,
                threshold: THRESHOLD,
                would_hit: None,
                note: format!("嵌入服务调用失败（{e}），无法测试召回效果"),
            }));
        }
    };
    let similarity = gec_core::cosine_similarity(&query_emb, &cand_emb);
    let would_hit = similarity >= THRESHOLD;
    Ok(Json(RecallTestResponse {
        similarity: Some(similarity),
        threshold: THRESHOLD,
        would_hit: Some(would_hit),
        note: if would_hit {
            "预期命中：重检测该文本时知识层将直接返回修正（无需 LLM 兜底）"
        } else {
            "预期未命中：相似度低于阈值；保留后对其他相似文本仍可能生效"
        }
        .to_string(),
    }))
}
