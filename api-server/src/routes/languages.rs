//! 语言 / 引擎元信息 API
//!
//! - GET /gec/languages — 各语言的 NLP 服务 + 引擎类型（rule / statistical / llm）
//!
//! 前端语言切换器据此展示真实引擎信息，避免与后端实际能力不一致
//! （如英文主路径是 HanLP 英文分词 + POS，而非 LanguageTool）。

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use serde::Serialize;

use crate::state::AppState;

#[derive(Serialize)]
pub struct LanguageInfo {
    pub lang: String,
    pub name: String,
    pub nlp_service: String,
    pub engine: String,
}

/// 各语言 NLP 主路径服务（与 gec-zh / gec-en / gec-ja 检测器实现一一对应）
fn nlp_service(lang: &str) -> &'static str {
    match lang {
        "zh" => "HanLP v2（分词 + POS + NER）",
        "en" => "HanLP 英文（分词 + POS）",
        "ja" => "Sudachi（分词 + 词性）",
        _ => "本地规则",
    }
}

/// GET /gec/languages
pub async fn list(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<LanguageInfo>>, axum::http::StatusCode> {
    let router = state.router.read().unwrap();
    let mut out = Vec::new();
    for lang in router.supported_languages() {
        let engine = router
            .get(&lang)
            .map(|e| e.kind().as_str().to_string())
            .unwrap_or_else(|| "rule".to_string());
        out.push(LanguageInfo {
            lang: lang.clone(),
            name: match lang.as_str() {
                "zh" => "中文",
                "en" => "英文",
                "ja" => "日文",
                other => other,
            }
            .to_string(),
            nlp_service: nlp_service(&lang).to_string(),
            engine,
        });
    }
    Ok(Json(out))
}
