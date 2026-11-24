use std::sync::Arc;

use super::core::Core;
/// 知识召回测试候选（「沉淀为知识」流程：规则构建失败后 UI 展示的内容）
///
/// 召回测试只消费 `text`（生成嵌入向量）；`error_type` / `correction`
/// 随请求透传以保持与知识条目同构（用户「保留」时由 UI 另行提交入库）。
#[derive(Debug, serde::Deserialize)]
#[allow(dead_code)]
pub struct RecallCandidate {
    /// 含错误的原始文本片段
    pub text: String,
    /// 错误类型（如 article / tense / spelling）
    pub error_type: String,
    /// 修正文本
    pub correction: String,
}

/// 知识召回测试：给定全文与候选知识，计算余弦相似度并判定 ②.5 知识召回层
/// 是否会命中（阈值与检测流水线一致：0.7）
///
/// 用于「规则构建失败 → 沉淀为知识」流程：UI 展示测试结果后，
/// 用户选择「保留」（写入知识存储）或「放弃」。
/// 未配置嵌入服务 / 嵌入调用失败时 `would_hit = null`（无法测试）。
#[tauri::command]
pub async fn test_knowledge_recall(
    core: tauri::State<'_, Arc<Core>>,
    _lang: String,
    text: String,
    candidate: RecallCandidate,
) -> Result<serde_json::Value, String> {
    const THRESHOLD: f64 = 0.7;
    // guard 不可跨 await（非 Send）——先克隆 Arc 再立即释放
    let engine = {
        let guard = core.embedding_engine.read().unwrap();
        guard.as_ref().cloned()
    };
    let engine = match engine {
        Some(engine) => engine,
        None => {
            return Ok(serde_json::json!({
                "similarity": null,
                "threshold": THRESHOLD,
                "would_hit": null,
                "note": "未配置嵌入服务，无法测试召回效果",
            }));
        }
    };
    let (query_emb, cand_emb) = match (
        engine.embed(&text).await,
        engine.embed(&candidate.text).await,
    ) {
        (Ok(a), Ok(b)) => (a, b),
        (a, b) => {
            let e = a
                .err()
                .or(b.err())
                .unwrap_or_else(|| "嵌入失败".to_string());
            tracing::warn!(error = %e, "召回测试：嵌入调用失败");
            return Ok(serde_json::json!({
                "similarity": null,
                "threshold": THRESHOLD,
                "would_hit": null,
                "note": format!("嵌入服务调用失败（{e}），无法测试召回效果"),
            }));
        }
    };
    let similarity = gec_core::cosine_similarity(&query_emb, &cand_emb);
    let would_hit = similarity >= THRESHOLD;
    Ok(serde_json::json!({
        "similarity": similarity,
        "threshold": THRESHOLD,
        "would_hit": would_hit,
        "note": if would_hit {
            "预期命中：重检测该文本时知识层将直接返回修正（无需 LLM 兜底）"
        } else {
            "预期未命中：相似度低于阈值；保留后对其他相似文本仍可能生效"
        },
    }))
}
