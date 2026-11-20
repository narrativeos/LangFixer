use std::sync::Arc;

use axum::extract::{Json, State};
use shared_model::{AttributionContext, AttributionResult, coarse_error_type};

use crate::routes::ApiError;
use crate::state::AppState;

/// POST /gec/attribute — 对单个错误进行归因
///
/// 决策表产出表级结果后，知识库按 `grammar_point` 富化
/// （点级补救/CEFR/实例 + 知识源引用；知识库缺失时原样返回）。
pub async fn attribute(
    State(state): State<Arc<AppState>>,
    Json(ctx): Json<AttributionContext>,
) -> Result<Json<AttributionResult>, ApiError> {
    let engine = state
        .tables
        .get(&ctx.lang)
        .ok_or_else(|| ApiError::NotFound(format!("语言 {} 无决策表", ctx.lang)))?;
    // zh 细粒度类型在决策表按粗族回退（§11.11，同 analyze 路由）；en/ja 原样
    let mut ctx = ctx;
    if ctx.lang == "zh" {
        ctx.error_type = coarse_error_type(&ctx.error_type).to_string();
    }
    let mut result = engine.attribute(&ctx).await?;
    // 知识库富化（知识层开关关闭——测评消融臂——时跳过）
    if state.settings.read().unwrap().knowledge_enabled {
        state.knowledge.enrich(&ctx, &mut result);
    }
    Ok(Json(result))
}
