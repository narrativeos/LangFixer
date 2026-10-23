use std::sync::Arc;

use shared_model::{AttributionContext, AttributionResult, AttributionTrace, WeinerDimensions};
use zen_engine::DecisionEngine;
use zen_engine::model::DecisionContent;

use crate::runtime::ZenWorker;
use crate::weiner;

/// ZEN 决策表归因引擎
///
/// 输入 [`AttributionContext`]，输出 [`AttributionResult`]
/// （归因标签 + 韦纳三维度 + 补救措施 + 决策追踪）。
pub struct AttributionEngine {
    decision: Arc<zen_engine::Decision>,
    table_name: String,
    worker: Arc<ZenWorker>,
}

impl AttributionEngine {
    /// 从 JDM JSON（graph 格式）构建引擎
    pub fn new(table_json: &str, table_name: &str) -> Result<Self, AttributionError> {
        let content: DecisionContent = serde_json::from_str(table_json)
            .map_err(|e| AttributionError::LoadFailed(format!("JSON 解析失败: {e}")))?;
        let engine = DecisionEngine::default();
        let decision = engine
            .create_decision(Arc::new(content))
            .map_err(|e| AttributionError::LoadFailed(format!("决策表编译失败: {e}")))?;
        Ok(Self {
            decision: Arc::new(decision),
            table_name: table_name.to_string(),
            worker: ZenWorker::shared(),
        })
    }

    /// 校验决策表 JSON 是否可用（热更新前调用；失败时保留旧表）
    pub fn validate(table_json: &str) -> Result<(), AttributionError> {
        let content: DecisionContent = serde_json::from_str(table_json)
            .map_err(|e| AttributionError::LoadFailed(format!("JSON 解析失败: {e}")))?;
        let engine = DecisionEngine::default();
        let decision = engine
            .create_decision(Arc::new(content))
            .map_err(|e| AttributionError::LoadFailed(format!("决策表编译失败: {e}")))?;
        decision
            .validate()
            .map_err(|e| AttributionError::LoadFailed(format!("决策表校验失败: {e}")))
    }

    pub fn table_name(&self) -> &str {
        &self.table_name
    }

    /// 对归因上下文执行决策表
    ///
    /// 求值在专用工作线程的独立 tokio 运行时中执行
    /// （zen 的 evaluate future 非 Send），本 future 保持 Send，
    /// 因此可以安全地在 axum / tokio 多线程运行时中 await。
    pub async fn attribute(
        &self,
        context: &AttributionContext,
    ) -> Result<AttributionResult, AttributionError> {
        let input = build_context(context);
        let rx = self.worker.submit(&self.decision, input).await;
        let result = rx
            .await
            .map_err(|e| AttributionError::EvaluationFailed(e.to_string()))?
            .map_err(AttributionError::EvaluationFailed)?;
        Ok(extract_result(&result, &self.table_name))
    }
}

/// 将归因上下文映射为 JDM 输入（camelCase，与决策表 input reference 对齐）
fn build_context(ctx: &AttributionContext) -> serde_json::Value {
    serde_json::json!({
        "errorType": ctx.error_type,
        "confidence": ctx.confidence,
        "studentHistoryCount": ctx.session_error_count,
        "isNovice": ctx.session_error_count <= 1,
        "grammarPoint": ctx.grammar_point.clone().unwrap_or_default(),
        "lang": ctx.lang,
    })
}

/// 从决策表输出中提取归因结果
fn extract_result(result: &serde_json::Value, table_name: &str) -> AttributionResult {
    let get_str = |key: &str| -> Option<String> {
        result
            .get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    };

    let attribution = get_str("attribution").unwrap_or_else(|| "unknown".into());

    let weiner = match result.get("weiner") {
        Some(w) if w.get("locus").and_then(|v| v.as_str()).is_some() => {
            let dim = |k: &str| {
                w.get(k)
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string()
            };
            WeinerDimensions {
                locus: dim("locus"),
                stability: dim("stability"),
                controllability: dim("controllability"),
            }
        }
        _ => weiner::default_weiner(&attribution),
    };

    let remedial = match result.get("remedial") {
        Some(arr) if arr.is_array() => arr
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        Some(s) if s.is_string() => s
            .as_str()
            .unwrap()
            .split(';')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        _ => vec!["建议结合规则讲解进行针对性练习".to_string()],
    };

    let matched_rule = get_str("matchedRule").unwrap_or_else(|| "unknown".into());

    // 决策表未提供 cefrLevel 输出时缺省为 "unknown"
    let cefr = get_str("cefrLevel").unwrap_or_else(|| "unknown".into());

    // 决策表未提供 source 输出时缺省为空串
    let source = get_str("source").unwrap_or_default();

    // 典型"错误→改正"实例：ZEN 输出为单串（"；" 连接）或数组，逐项拆分
    let examples = match result.get("examples") {
        Some(arr) if arr.is_array() => arr
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .flat_map(|s| s.split('；'))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        Some(s) if s.is_string() => s
            .as_str()
            .unwrap()
            .split('；')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        _ => Vec::<String>::new(),
    };

    AttributionResult {
        attribution,
        weiner,
        remedial,
        trace: AttributionTrace {
            matched_rule,
            decision_table: table_name.to_string(),
        },
        cefr,
        source,
        // 知识源引用由知识库按 grammar_point 富化（KnowledgeStore::enrich）；
        // 旧版表的 source 字符串保留在上方 `source` 字段（历史数据兼容）
        sources: Vec::new(),
        examples,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AttributionError {
    #[error("failed to load decision table: {0}")]
    LoadFailed(String),
    #[error("failed to evaluate decision: {0}")]
    EvaluationFailed(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared_model::attribution_labels;

    fn table_path(name: &str) -> std::path::PathBuf {
        let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        p.push("../decision-tables");
        p.push(name);
        p
    }

    async fn load_zh() -> AttributionEngine {
        let content = std::fs::read_to_string(table_path("zh_gec_attribution.json")).unwrap();
        AttributionEngine::new(&content, "zh_gec_attribution").unwrap()
    }

    fn ctx(error_type: &str, confidence: f64, history: u32) -> AttributionContext {
        AttributionContext {
            lang: "zh".into(),
            error_type: error_type.into(),
            confidence,
            session_error_count: history,
            session_meta: None,
            grammar_point: None,
        }
    }

    #[tokio::test]
    async fn zh_spelling_old_hand_high_conf() {
        let engine = load_zh().await;
        let result = engine.attribute(&ctx("spelling", 0.95, 4)).await.unwrap();
        assert_eq!(result.attribution, attribution_labels::HABIT_SOLIDIFIED);
        assert_eq!(result.weiner.locus, "internal");
        assert_eq!(result.weiner.stability, "stable");
        assert_eq!(result.trace.matched_rule, "spelling_high_conf_old_hand");
        assert!(!result.remedial.is_empty());
    }

    #[tokio::test]
    async fn zh_spelling_novice() {
        let engine = load_zh().await;
        let result = engine.attribute(&ctx("spelling", 0.95, 0)).await.unwrap();
        assert_eq!(result.attribution, attribution_labels::KNOWLEDGE_GAP);
        assert_eq!(result.trace.matched_rule, "spelling_novice");
    }

    #[tokio::test]
    async fn zh_grammar_old_hand() {
        let engine = load_zh().await;
        let result = engine.attribute(&ctx("grammar", 0.9, 3)).await.unwrap();
        assert_eq!(result.attribution, attribution_labels::RULE_FORGOTTEN);
    }

    #[tokio::test]
    async fn zh_punctuation_novice() {
        let engine = load_zh().await;
        let result = engine.attribute(&ctx("punctuation", 0.9, 1)).await.unwrap();
        assert_eq!(result.attribution, attribution_labels::RULE_MISSING);
    }

    #[tokio::test]
    async fn zh_collocation() {
        let engine = load_zh().await;
        let result = engine.attribute(&ctx("collocation", 0.9, 5)).await.unwrap();
        assert_eq!(result.attribution, attribution_labels::COLLOCATION_WEAK);
    }

    #[tokio::test]
    async fn zh_default_rule() {
        let engine = load_zh().await;
        let result = engine
            .attribute(&ctx("unknown_type", 0.5, 2))
            .await
            .unwrap();
        assert_eq!(result.attribution, attribution_labels::KNOWLEDGE_GAP);
        assert_eq!(result.trace.matched_rule, "default_knowledge_gap");
        // 决策表提供 cefrLevel 输出列
        assert!(!result.cefr.is_empty());
        assert_ne!(result.cefr, "unknown");
    }

    #[tokio::test]
    async fn en_article_fallback_novice() {
        // 新表无 grammarPoint 输入：无知识点时命中该错误类型兜底规则
        let content = std::fs::read_to_string(table_path("en_gec_attribution.json")).unwrap();
        let engine = AttributionEngine::new(&content, "en_gec_attribution").unwrap();
        let mut c = ctx("article", 0.9, 0);
        c.lang = "en".into();
        let result = engine.attribute(&c).await.unwrap();
        assert_eq!(result.trace.matched_rule, "article_novice");
        assert!(result.source.is_empty(), "新表不再内嵌来源字符串");
        assert!(
            result.sources.is_empty(),
            "知识源引用由知识库富化（此处未加载 KB）"
        );
    }

    #[tokio::test]
    async fn en_article_fallback_old_hand() {
        // 老手（history > 1）+ 高置信 → 命中 old-hand 规则
        let content = std::fs::read_to_string(table_path("en_gec_attribution.json")).unwrap();
        let engine = AttributionEngine::new(&content, "en_gec_attribution").unwrap();
        let mut c = ctx("article", 0.9, 5);
        c.lang = "en".into();
        let result = engine.attribute(&c).await.unwrap();
        assert_eq!(result.trace.matched_rule, "article_old_hand");
        assert_eq!(result.attribution, attribution_labels::HABIT_SOLIDIFIED);
    }

    #[tokio::test]
    async fn en_unknown_type_hits_default() {
        let content = std::fs::read_to_string(table_path("en_gec_attribution.json")).unwrap();
        let engine = AttributionEngine::new(&content, "en_gec_attribution").unwrap();
        let mut c = ctx("unknown_type", 0.5, 2);
        c.lang = "en".into();
        let result = engine.attribute(&c).await.unwrap();
        assert_eq!(result.trace.matched_rule, "default_knowledge_gap");
    }

    #[test]
    fn en_table_is_rules_only() {
        // IP 回归守护：决策表只含可提取的规则，不得内嵌知识源内容
        // （讲次标题 / 书名 / 正文例句 / grammarPoint 点级规则均不得出现）
        let raw = std::fs::read_to_string(table_path("en_gec_attribution.json")).unwrap();
        for frag in [
            "grammarPoint",
            "第一讲",
            "第二讲",
            "大学英语语法",
            "徐广联",
            "讲座与测试",
            "LangFixer 示例英语语法指南",
        ] {
            assert!(!raw.contains(frag), "en 决策表不应包含: {frag}");
        }
    }
}
