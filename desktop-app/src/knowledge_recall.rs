//! 知识向量召回实现（桌面端，句级）：组合 KnowledgeStore + EmbeddingEngine
//!
//! 与 api-server 的 `knowledge_recall` 模块同构（同参数、同行为）。
//! 输入文本按句切分，每句生成一个向量（批量嵌入，一次 HTTP 往返），
//! 在知识库中做余弦相似度匹配；命中带句锚点（`RecallHit`），
//! 由引擎侧 `knowledge_hit_to_error` 做两级定位（精确子串 / 模糊句级）。
//! 批量嵌入不支持时回退**全文单向量**（锚点 = 全文 span，保持句级改造前的召回行为）；
//! 嵌入服务不可用时静默返回空，不阻断检测。

use std::sync::Arc;

use async_trait::async_trait;
use gec_core::{EmbeddingEngine, KnowledgeRecall, RecallHit, split_sentences};
use lexicon_store::KnowledgeStore;

/// 召回参数（句级：每句最多 top_k 条命中）
const PER_SENTENCE_TOP_K: usize = 2;
const RECALL_THRESHOLD: f64 = 0.7;

/// 知识向量召回实现
pub struct KnowledgeRecallImpl {
    store: Arc<KnowledgeStore>,
    engine: Arc<EmbeddingEngine>,
}

impl KnowledgeRecallImpl {
    pub fn new(store: Arc<KnowledgeStore>, engine: Arc<EmbeddingEngine>) -> Self {
        Self { store, engine }
    }

    /// 全文单向量召回（批量嵌入不支持时的回退路径）：
    /// 锚点 = 全文 span（`(0, text.len())`），引擎侧 `knowledge_hit_to_error`
    /// 在全文内做两级定位（精确子串 → 精确 span；高相似 → 全文级粗定位）。
    /// 注意：不能把单个全文向量拿去逐句索引（句数 > 1 时会越界 panic）。
    async fn recall_whole_text(&self, lang: &str, text: &str) -> Vec<RecallHit> {
        let emb = match self.engine.embed(text).await {
            Ok(emb) => emb,
            Err(e) => {
                tracing::debug!(error = %e, "嵌入生成失败，跳过知识召回");
                return vec![];
            }
        };
        match self
            .store
            .recall(lang, &emb, PER_SENTENCE_TOP_K, RECALL_THRESHOLD)
        {
            Ok(results) => results
                .into_iter()
                .map(|(item, sim)| RecallHit {
                    item,
                    sim,
                    sentence: text.to_string(),
                    sentence_span: (0, text.len()),
                })
                .collect(),
            Err(e) => {
                tracing::debug!(error = %e, "知识召回查询失败，跳过");
                vec![]
            }
        }
    }
}

#[async_trait]
impl KnowledgeRecall for KnowledgeRecallImpl {
    async fn recall(&self, lang: &str, text: &str) -> Vec<RecallHit> {
        let sentences = split_sentences(text);
        if sentences.is_empty() {
            return vec![];
        }
        // 生成句子向量：单句 → 单条 embed；多句 → 批量 embed（一次往返）；
        // 批量不支持（服务返回错误）→ 回退全文单向量（锚点 = 全文 span）
        let embeddings: Vec<Vec<f32>> = if sentences.len() == 1 {
            let (s, _, _) = &sentences[0];
            match self.engine.embed(s).await {
                Ok(emb) => vec![emb],
                Err(e) => {
                    tracing::debug!(error = %e, "嵌入生成失败，跳过知识召回");
                    return vec![];
                }
            }
        } else {
            let refs: Vec<&str> = sentences.iter().map(|(s, _, _)| s.as_str()).collect();
            match self.engine.embed_batch(&refs).await {
                Ok(es) => es,
                Err(e) => {
                    tracing::debug!(error = %e, "批量嵌入失败，回退全文单向量");
                    return self.recall_whole_text(lang, text).await;
                }
            }
        };
        // 批量向量召回（每句取相似度 ≥ 阈值的 top_k 命中）：
        // 知识库只加载一次（`recall_many`），N 句 = 1 次全表读 + O(N×K) 余弦，
        // 而非 N 次全表读（每次 list 都解码全部 embedding BLOB）；
        // 向量数与句数不一致时（服务少返回向量）按较短者截断，防越界
        let refs: Vec<&[f32]> = embeddings.iter().map(|e| e.as_slice()).collect();
        match self
            .store
            .recall_many(lang, &refs, PER_SENTENCE_TOP_K, RECALL_THRESHOLD)
        {
            Ok(per_sentence) => {
                let mut hits = Vec::new();
                for i in 0..sentences.len().min(per_sentence.len()) {
                    let (sentence, start, end) = &sentences[i];
                    for (item, sim) in per_sentence[i].iter() {
                        hits.push(RecallHit {
                            item: item.clone(),
                            sim: *sim,
                            sentence: sentence.clone(),
                            sentence_span: (*start, *end),
                        });
                    }
                }
                hits
            }
            Err(e) => {
                tracing::debug!(error = %e, "知识召回查询失败，跳过");
                Vec::new()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gec_core::EmbeddingConfig;

    /// 只支持单条 `input` 的嵌入服务：数组 `input` → 400，字符串 → 单向量
    async fn mock_single_only_server() -> String {
        use axum::response::IntoResponse;
        let app = axum::Router::new().route(
            "/v1/embeddings",
            axum::routing::post(|body: axum::body::Bytes| async move {
                let req: serde_json::Value = serde_json::from_slice(&body).unwrap();
                match &req["input"] {
                    serde_json::Value::Array(_) => (
                        axum::http::StatusCode::BAD_REQUEST,
                        axum::Json(serde_json::json!({"error": "array input not supported"})),
                    ),
                    _ => (
                        axum::http::StatusCode::OK,
                        axum::Json(serde_json::json!({
                            "model": "mock",
                            "data": [{ "index": 0, "embedding": [1.0, 0.0] }]
                        })),
                    ),
                }
                .into_response()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        format!("http://{addr}/v1")
    }

    #[tokio::test]
    async fn batch_unsupported_falls_back_to_whole_text() {
        // 回归：批量嵌入不支持（数组 input → 400）+ 多句文本时，
        // 回退全文单向量（锚点 = 全文 span），不得越界 panic
        let endpoint = mock_single_only_server().await;
        let engine = Arc::new(EmbeddingEngine::new(EmbeddingConfig {
            endpoint,
            ..Default::default()
        }));
        let store = Arc::new(KnowledgeStore::open_in_memory().unwrap());
        // 知识条目向量与 mock 返回向量一致（余弦 = 1.0 ≥ 0.7）→ 应命中
        store
            .insert("en", "gpus", "spelling", "GPUs", &[1.0, 0.0], "test")
            .unwrap();
        let impl_ = KnowledgeRecallImpl::new(store, engine);
        let text = "I saw the gpus today. Second sentence here.";
        let hits = impl_.recall("en", text).await;
        assert_eq!(hits.len(), 1, "全文回退应产出命中: {:?}", hits);
        assert_eq!(hits[0].sentence_span, (0, text.len()), "锚点 = 全文 span");
        assert_eq!(hits[0].sentence, text);
        assert_eq!(hits[0].item.text, "gpus");
        // 引擎侧两级定位：精确子串 → 精确 span
        let err = gec_core::knowledge_hit_to_error(&hits[0]).unwrap();
        let pos = text.find("gpus").unwrap();
        assert_eq!(err.span, [pos, pos + 4]);
    }

    #[tokio::test]
    async fn batch_supported_sentence_level_hits() {
        // 对照组：批量嵌入支持时走句级路径（锚点 = 句 span）
        // mock 按输入内容返回向量：含 "gpus" → [1,0]（与知识条目同向，余弦 1.0 命中）；
        // 其他句子 → [0,1]（正交，余弦 0 < 阈值，不命中）
        use axum::response::IntoResponse;
        let app = axum::Router::new().route(
            "/v1/embeddings",
            axum::routing::post(|body: axum::body::Bytes| async move {
                let req: serde_json::Value = serde_json::from_slice(&body).unwrap();
                let data = match &req["input"] {
                    serde_json::Value::Array(items) => items
                        .iter()
                        .enumerate()
                        .map(|(i, item)| {
                            let hit = item.as_str().map(|s| s.contains("gpus")).unwrap_or(false);
                            serde_json::json!({
                                "index": i,
                                "embedding": if hit { [1.0, 0.0] } else { [0.0, 1.0] }
                            })
                        })
                        .collect::<Vec<_>>(),
                    _ => vec![serde_json::json!({ "index": 0, "embedding": [1.0, 0.0] })],
                };
                (
                    axum::http::StatusCode::OK,
                    axum::Json(serde_json::json!({ "model": "mock", "data": data })),
                )
                    .into_response()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        let engine = Arc::new(EmbeddingEngine::new(EmbeddingConfig {
            endpoint: format!("http://{addr}/v1"),
            ..Default::default()
        }));
        let store = Arc::new(KnowledgeStore::open_in_memory().unwrap());
        store
            .insert("en", "gpus", "spelling", "GPUs", &[1.0, 0.0], "test")
            .unwrap();
        let impl_ = KnowledgeRecallImpl::new(store, engine);
        let hits = impl_
            .recall("en", "I saw the gpus today. Second one.")
            .await;
        assert_eq!(hits.len(), 1);
        // 句级锚点：命中的是第一句（含 "gpus"），span 为该句在全文中的字节区间
        let first_end = "I saw the gpus today.".len();
        assert_eq!(hits[0].sentence_span, (0, first_end));
        assert_eq!(hits[0].sentence, "I saw the gpus today.");
    }
}
