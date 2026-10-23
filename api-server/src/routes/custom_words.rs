//! 自定义词（白名单）API：添加 / 删除 / 列表

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

use crate::custom_words::CustomWord;
use crate::routes::ApiError;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct AddWordRequest {
    pub lang: String,
    pub word: String,
    #[serde(default)]
    pub reason: String,
}

#[derive(Serialize, Deserialize)]
pub struct ListWordsResponse {
    pub lang: String,
    pub words: Vec<CustomWord>,
}

/// POST /gec/custom-words — 添加自定义词
pub async fn add_word(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AddWordRequest>,
) -> Result<(StatusCode, Json<CustomWord>), ApiError> {
    let word = state
        .custom_words
        .add(&req.lang, &req.word, &req.reason)
        .map_err(|e| ApiError::Internal(format!("添加自定义词失败: {e}")))?;
    Ok((StatusCode::CREATED, Json(word)))
}

/// DELETE /gec/custom-words/:id — 删除自定义词
pub async fn remove_word(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> Result<(StatusCode, String), ApiError> {
    let removed = state
        .custom_words
        .remove(id)
        .map_err(|e| ApiError::Internal(format!("删除自定义词失败: {e}")))?;
    if removed {
        Ok((StatusCode::NO_CONTENT, String::new()))
    } else {
        Err(ApiError::NotFound(format!("自定义词 id={id} 不存在")))
    }
}

/// GET /gec/custom-words?lang=zh — 列出指定语言的自定义词
pub async fn list_words(
    State(state): State<Arc<AppState>>,
    Query(q): Query<LangQuery>,
) -> Result<Json<ListWordsResponse>, ApiError> {
    let words = state
        .custom_words
        .list(&q.lang)
        .map_err(|e| ApiError::Internal(format!("查询自定义词失败: {e}")))?;
    Ok(Json(ListWordsResponse {
        lang: q.lang,
        words,
    }))
}

#[derive(Deserialize)]
pub struct LangQuery {
    pub lang: String,
}
