use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;

use gec_core::detector::{DetectorError, GECDetector};
use gec_core::timings::NlpCall;
use shared_model::Error;

use crate::config::DetectorConfig;
use crate::offline::OfflineChineseDetector;
use crate::rules;

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use axum::response::IntoResponse;

    use super::*;

    /// mock `/analyze`：记录每次收到的文本；每段返回其开头 2 字的 token
    /// （块内字符偏移）；含 `fail_marker` 的请求回 500
    async fn mock_analyze(seen: Arc<Mutex<Vec<String>>>, fail_marker: Option<String>) -> String {
        let app = axum::Router::new().route(
            "/analyze",
            axum::routing::post(move |body: axum::body::Bytes| {
                let seen = seen.clone();
                let fail = fail_marker.clone();
                async move {
                    let req: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    let text = req["text"].as_str().unwrap_or("").to_string();
                    seen.lock().unwrap().push(text.clone());
                    if let Some(m) = &fail {
                        if text.contains(m) {
                            return axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response();
                        }
                    }
                    let tok_text: String = text.chars().take(2).collect();
                    let n = tok_text.chars().count();
                    let resp = serde_json::json!({
                        "content": { "tokens": [{ "text": tok_text, "pos": "NT", "span": [0, n] }] }
                    });
                    (axum::http::StatusCode::OK, axum::Json(resp)).into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        format!("http://{addr}")
    }

    fn detector(endpoint: String, chunk_chars: usize) -> ChineseDetector {
        ChineseDetector::new(DetectorConfig {
            endpoint,
            timeout: std::time::Duration::from_secs(5),
            offline_only: false,
            lexicons_dir: PathBuf::from("lexicons"),
            chunk_chars,
        })
    }

    fn sentences() -> Vec<&'static str> {
        vec![
            "他昨天在图书馆借了三本书，但是他还忘记带借书证。",
            "老师今天讲的内容我没有听懂，需要课后复习一遍。",
            "虽然天气很冷但是同学们仍然坚持每天晨跑锻炼。",
            "我们的学校有一个很大的图书馆可以安静地读书。",
            "图书馆的借阅规则规定每人一次最多借十本书。",
            "明天上午我们要去操场举行升旗仪式和活动。",
        ]
    }

    #[tokio::test]
    async fn long_text_is_chunked_serially_and_spans_offset() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let endpoint = mock_analyze(seen.clone(), None).await;
        let detector = detector(endpoint, 60);
        let text: String = sentences().concat(); // ≈132 字 > 60

        let tokens = detector.analyze(&text).await.unwrap();

        let pieces = seen.lock().unwrap().clone();
        assert!(pieces.len() >= 2, "长文必须分块，实发 {pieces:?}");
        assert_eq!(pieces.concat(), text, "块必须首尾相接铺满全文");
        for p in &pieces {
            assert!(
                p.chars().count() <= 60,
                "块超上限: {} 字",
                p.chars().count()
            );
        }
        // 合并后的 token span 在**全文字符空间**：每块一个 token，偏移 = 块起始字符下标
        assert_eq!(tokens.len(), pieces.len());
        let mut base = 0usize;
        for (piece, tok) in pieces.iter().zip(&tokens) {
            let head: String = piece.chars().take(2).collect();
            assert_eq!(tok.text, head);
            assert_eq!(
                tok.span,
                [base, base + head.chars().count()],
                "span 未平移回全文字符空间"
            );
            let got: String = text
                .chars()
                .skip(tok.span[0])
                .take(tok.span[1] - tok.span[0])
                .collect();
            assert_eq!(got, tok.text);
            base += piece.chars().count();
        }
        // 多块串行计入**一次** NlpCall 总耗时
        let call = detector.last_nlp_call().unwrap();
        assert!(call.cold_start);
        let _ = call.ms;
    }

    #[tokio::test]
    async fn chunk_failure_fails_whole_and_stops_serially() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let endpoint = mock_analyze(seen.clone(), Some("虽然天气".to_string())).await;
        let detector = detector(endpoint, 60);
        let text: String = sentences().concat();

        let err = detector.analyze(&text).await.unwrap_err();
        assert!(matches!(err, DetectorError::ServiceUnavailable(_)));
        // 串行且遇错即停：只发到含故障块的序号，不再发后续块
        let pieces = seen.lock().unwrap().clone();
        assert!(pieces.len() >= 2);
        assert!(pieces.last().unwrap().contains("虽然天气"));
        // 后续块（含最后两句）未被发送
        assert!(!pieces.iter().any(|p| p.contains("明天上午")));
    }

    #[tokio::test]
    async fn short_text_single_call_unchanged() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let endpoint = mock_analyze(seen.clone(), None).await;
        let detector = detector(endpoint, 60);
        let text = "他昨天在图书馆借了三本书。";

        let tokens = detector.analyze(text).await.unwrap();

        assert_eq!(*seen.lock().unwrap(), vec![text.to_string()]);
        assert_eq!(tokens[0].span, [0, 2]);
    }

    #[tokio::test]
    async fn chunk_chars_zero_disables_chunking() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let endpoint = mock_analyze(seen.clone(), None).await;
        let detector = detector(endpoint, 0);
        let text: String = sentences().concat();

        detector.analyze(&text).await.unwrap();

        // 0 = 整段单发（旧行为）
        assert_eq!(seen.lock().unwrap().len(), 1);
    }
}

/// 中文检测器
///
/// - 主路径：`POST {endpoint}/analyze`（HanLP v2 分词 + POS + NER）→ Rust 侧 GEC 规则
/// - 兜底：远端不可达时降级为纯本地规则检测
/// - 词表：`lexicons/zh_gec_lexicon.json` 外置（缺失时用内置默认）
pub struct ChineseDetector {
    endpoint: String,
    client: reqwest::Client,
    fallback: OfflineChineseDetector,
    lexicon: rules::Lexicon,
    offline_only: bool,
    /// `/analyze` 长文本分块字数上限（0 = 不分块）
    chunk_chars: usize,
    /// NLP 服务是否已完成首次成功调用（冷启动预热完成）
    nlp_warmed: AtomicBool,
    /// 最近一次 NLP 服务调用（`None` = 未调用，如纯本地检测）
    nlp_last: Mutex<Option<NlpCall>>,
}

impl ChineseDetector {
    pub fn new(config: DetectorConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .build()
            .unwrap_or_default();
        let lexicon = rules::Lexicon::load(&config.lexicons_dir);
        Self {
            endpoint: config.endpoint,
            client,
            fallback: OfflineChineseDetector::new(lexicon.clone()),
            lexicon,
            offline_only: config.offline_only,
            chunk_chars: config.chunk_chars,
            nlp_warmed: AtomicBool::new(false),
            nlp_last: Mutex::new(None),
        }
    }
}

/// HanLP 服务 `/analyze` 响应（只取需要的字段）
#[derive(serde::Deserialize)]
struct AnalyzeResponse {
    #[serde(default)]
    content: AnalyzeContent,
}

#[derive(serde::Deserialize, Default)]
struct AnalyzeContent {
    #[serde(default)]
    tokens: Vec<rules::Token>,
}

#[async_trait]
impl GECDetector for ChineseDetector {
    fn language(&self) -> &str {
        "zh"
    }

    async fn detect(&self, text: &str) -> Result<Vec<Error>, DetectorError> {
        if self.offline_only {
            return self.fallback.detect(text).await;
        }
        match self.analyze(text).await {
            Ok(tokens) => {
                // HanLP `/analyze` 返回**字符偏移**（Python `len` 语义），而
                // [`Error::span`] 契约为**字节偏移**——CJK 文本两者相差约 3 倍，
                // 不换算会导致规则产出的 span 错位（高亮错位、apply_corrections
                // 在非字符边界切片 panic）。统一在入口处换算。
                let tokens = rules::tokens_to_byte_spans(text, &tokens);
                Ok(rules::detect_with_tokens(text, &tokens, &self.lexicon))
            }
            Err(e) => {
                tracing::warn!(error = %e, "HanLP 服务不可用，降级为离线检测");
                self.fallback.detect(text).await
            }
        }
    }

    fn last_nlp_call(&self) -> Option<NlpCall> {
        *self.nlp_last.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl ChineseDetector {
    /// 调用 HanLP `/analyze` 并记录本次 NLP 调用计时
    ///
    /// 检测器构建 / 配置重建后的**首次**调用包含 NLP 服务冷启动（模型加载）时间，
    /// 标记 `cold_start = true`（成功调用后才视为预热完成；失败不置位，
    /// 服务恢复后的下一次调用仍可能含冷启动）。
    async fn analyze(&self, text: &str) -> Result<Vec<rules::Token>, DetectorError> {
        let t = std::time::Instant::now();
        let cold_start = !self.nlp_warmed.load(Ordering::Relaxed);
        let result = self.analyze_chunks(text).await;
        let ms = t.elapsed().as_millis() as u64;
        if result.is_ok() {
            self.nlp_warmed.store(true, Ordering::Relaxed);
        }
        *self.nlp_last.lock().unwrap_or_else(|e| e.into_inner()) = Some(NlpCall { ms, cold_start });
        result
    }

    /// 分块调用 `/analyze`：长文本按句边界切成字符数 ≤ `chunk_chars` 的块
    /// **串行**发送（sidecar 单 worker，且对长文是三次方级劣化，见
    /// `gec_core::nlpchunk`），每块 token 的字符 span 加上块起始字符下标后按
    /// 文档顺序合并；任一块失败 → 整体 Err（离线兜底语义不变）。
    /// 短文 / 不分块配置下单发，行为与旧实现完全一致。
    async fn analyze_chunks(&self, text: &str) -> Result<Vec<rules::Token>, DetectorError> {
        if self.chunk_chars == 0 {
            return self.analyze_inner(text).await;
        }
        let pieces = gec_core::nlpchunk::split_capped_by_sentences(text, self.chunk_chars);
        if pieces.len() <= 1 {
            return self.analyze_inner(text).await;
        }
        tracing::debug!(
            chunks = pieces.len(),
            chars = text.chars().count(),
            "NLP /analyze 长文本分块串行调用"
        );
        let mut merged: Vec<rules::Token> = Vec::new();
        for piece in pieces {
            let slice = &text[piece[0]..piece[1]];
            // 服务返回的是块内**字符**偏移，平移回全文字符空间（字节换算在 detect 入口）
            let base = text[..piece[0]].chars().count();
            let tokens = self.analyze_inner(slice).await?;
            merged.extend(tokens.into_iter().map(|t| rules::Token {
                text: t.text,
                pos: t.pos,
                span: [t.span[0] + base, t.span[1] + base],
            }));
        }
        Ok(merged)
    }

    async fn analyze_inner(&self, text: &str) -> Result<Vec<rules::Token>, DetectorError> {
        let url = format!("{}/analyze", self.endpoint.trim_end_matches('/'));
        let resp = self
            .client
            .post(&url)
            .json(&serde_json::json!({ "text": text, "language": "modern" }))
            .send()
            .await
            .map_err(|e| DetectorError::ServiceUnavailable(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(DetectorError::ServiceUnavailable(format!(
                "HanLP 服务返回 HTTP {}",
                resp.status()
            )));
        }

        let body: AnalyzeResponse = resp
            .json()
            .await
            .map_err(|e| DetectorError::ServiceUnavailable(e.to_string()))?;
        Ok(body.content.tokens)
    }
}
