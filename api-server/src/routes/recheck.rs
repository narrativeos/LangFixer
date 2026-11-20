use std::sync::Arc;

use axum::extract::{Json, State};
use serde_json::json;

use gec_core::targeted_llm_review;

use crate::routes::ApiError;
use crate::state::AppState;

/// POST /gec/recheck — 对全文中某一句做定向 LLM 复查
///
/// 场景：标准模式（L2）下决策模型给出「句级判断」verdict 项（只判定有错、
/// 给不出具体修改），前端标「待复查」。用户点「LLM 复查」时送本路由：
/// 复用 ③-T 的 [`targeted_llm_review`]，只复查该句所在的字节区间，
/// 返回定位到全文坐标的具体错误（source='llm'），供前端合流成可操作条目。
#[derive(serde::Deserialize)]
pub struct SentenceRecheckRequest {
    /// 语言（zh / en / ja）
    pub lang: String,
    /// 全文（span 坐标系）
    pub text: String,
    /// 待复查句在全文中的字节区间 [start, end)（与 Error.span 同语义）
    pub span: [usize; 2],
}

/// span 合法性校验（纯函数，便于单测）：非空且落在 text 字节范围内。
/// 越界多半是前端拿错了文本版本（如采纳改过原文后再复查），明确报错而非静默钳位。
fn validate_span(text: &str, span: &[usize; 2]) -> Result<(), String> {
    if span[0] >= span[1] {
        return Err(format!("复查区间为空: [{}, {})", span[0], span[1]));
    }
    if span[1] > text.len() {
        return Err(format!(
            "复查区间越界: [{}, {}) 超出全文长度 {}",
            span[0],
            span[1],
            text.len()
        ));
    }
    Ok(())
}

pub async fn recheck_sentence(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SentenceRecheckRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    validate_span(&req.text, &req.span).map_err(ApiError::BadRequest)?;
    let llm = state.llm_detect_engine(&req.lang).ok_or_else(|| {
        ApiError::ServiceUnavailable("AI 复查未配置（请先在设置中配置 LLM）".into())
    })?;
    let (errors, ledger) = targeted_llm_review(&llm, &req.text, &[req.span]).await?;
    // 单句复查理论上不该丢检出；丢了记 warn 留痕（§7 完整性数据源）
    if !ledger.is_empty() {
        tracing::warn!(?ledger, "定向复查丢弃了部分 LLM 检出");
    }
    Ok(Json(json!({ "errors": errors })))
}

#[cfg(test)]
mod tests {
    use super::validate_span;

    #[test]
    fn span_合法通过() {
        let text = "这是一句话。";
        assert!(validate_span(text, &[0, text.len()]).is_ok());
        // 中文多字节：字节区间不必对齐字符边界也属合法输入
        // （targeted_llm_review 自身会钳位对齐）
        assert!(validate_span(text, &[3, 9]).is_ok());
    }

    #[test]
    fn span_为空或倒序报错() {
        let text = "abc";
        assert!(validate_span(text, &[2, 2]).is_err());
        assert!(validate_span(text, &[2, 1]).is_err());
    }

    #[test]
    fn span_越界报错() {
        let text = "abc";
        let err = validate_span(text, &[0, 4]).unwrap_err();
        assert!(err.contains("越界"), "{err}");
    }
}
