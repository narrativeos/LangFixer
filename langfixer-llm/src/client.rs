//! OpenAI 兼容 LLM 客户端
//!
//! - [`LlmClient::complete`]：非流式，返回完整响应文本
//! - [`LlmClient::stream`]：SSE 流式（`data: {...}` 行），逐 token 产出
//!
//! 兼容任意 `/v1/chat/completions` 端点（OpenAI / DeepSeek / 本地 vLLM 等）。

use std::pin::Pin;
use std::task::Poll;
use std::time::Duration;

use futures_util::Stream;
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use serde::{Deserialize, Serialize};

use crate::LlmConfig;
use crate::error::LlmError;

/// 聊天消息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: &str) -> Self {
        Self {
            role: "system".into(),
            content: content.into(),
        }
    }
    pub fn user(content: &str) -> Self {
        Self {
            role: "user".into(),
            content: content.into(),
        }
    }
}

#[derive(Debug, Serialize)]
struct CompletionRequest {
    model: String,
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    stream: bool,
    temperature: f32,
}

#[derive(Debug, Deserialize)]
struct CompletionResponse {
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: Message,
}

#[derive(Debug, Deserialize)]
struct Message {
    content: Option<String>,
}

/// LLM 客户端
pub struct LlmClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
}

impl LlmClient {
    pub fn new(config: &LlmConfig) -> Self {
        let timeout = Duration::from_secs(config.timeout_secs);
        // 连接超时：服务不可达时快速失败；
        // 读超时（每次成功读到数据即重置）：流式生成只要 token 持续到达就不会中断，
        // 同时仍能检测停滞的连接。
        // 注意：reqwest 的 `.timeout()` 是覆盖「连接 → 响应体读完」的总时限，
        // 长生成（如思考模型）会被误杀，故此处不使用。
        let http = reqwest::Client::builder()
            .connect_timeout(timeout)
            .read_timeout(timeout)
            .build()
            .unwrap_or_default();
        Self {
            http,
            base_url: config.base_url.clone(),
            api_key: config.api_key.clone(),
            model: config.model.clone(),
        }
    }

    fn url(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }

    /// 非流式补全：返回完整响应文本
    pub async fn complete(&self, system: &str, user: &str) -> Result<String, LlmError> {
        let body = CompletionRequest {
            model: self.model.clone(),
            messages: vec![ChatMessage::system(system), ChatMessage::user(user)],
            stream: false,
            temperature: 0.7,
        };
        let resp = self
            .http
            .post(self.url())
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Request(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(LlmError::Request(format!("HTTP {}", resp.status())));
        }
        let cr: CompletionResponse = resp
            .json()
            .await
            .map_err(|e| LlmError::Request(e.to_string()))?;
        cr.choices
            .first()
            .and_then(|c| c.message.content.clone())
            .filter(|c| !c.is_empty())
            .ok_or_else(|| LlmError::Request("响应缺少 choices[0].message.content".into()))
    }

    /// SSE 流式补全：逐 token 产出（`Ok(Some(token))`，流结束 `Ok(None)`）
    ///
    /// 解析 `data: {...}` 行中的 `choices[0].delta.content`，`data: [DONE]` 结束。
    pub async fn stream(
        &self,
        system: &str,
        user: &str,
    ) -> Result<BoxStream<'static, Result<Option<String>, LlmError>>, LlmError> {
        let body = CompletionRequest {
            model: self.model.clone(),
            messages: vec![ChatMessage::system(system), ChatMessage::user(user)],
            stream: true,
            temperature: 0.7,
        };
        let resp = self
            .http
            .post(self.url())
            .bearer_auth(&self.api_key)
            .header("Accept", "text/event-stream")
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Request(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(LlmError::Request(format!("HTTP {}", resp.status())));
        }

        let (tx, rx) = tokio::sync::mpsc::channel::<Result<Option<String>, LlmError>>(32);
        let bytes: BoxStream<'_, Result<bytes::Bytes, LlmError>> = resp
            .bytes_stream()
            .map(|r| r.map_err(|e| LlmError::Request(e.to_string())))
            .boxed();
        tokio::spawn(async move {
            use futures_util::StreamExt;
            let mut buf = String::new();
            let mut stream = bytes;
            while let Some(item) = stream.next().await {
                match item {
                    Ok(chunk) => {
                        buf.push_str(&String::from_utf8_lossy(&chunk));
                        // 逐行处理 SSE
                        while let Some(pos) = buf.find('\n') {
                            let line: String = buf.drain(..=pos).collect();
                            let line = line.trim_end_matches('\r').trim();
                            let Some(payload) = line.strip_prefix("data:") else {
                                continue;
                            };
                            let payload = payload.trim();
                            if payload == "[DONE]" {
                                return;
                            }
                            let Ok(value) = serde_json::from_str::<serde_json::Value>(payload)
                            else {
                                continue;
                            };
                            if let Some(d) = value
                                .pointer("/choices/0/delta/content")
                                .and_then(|c| c.as_str())
                                && !d.is_empty()
                                && tx.send(Ok(Some(d.to_string()))).await.is_err()
                            {
                                return;
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e)).await;
                        return;
                    }
                }
            }
            // 流自然结束
            let _ = tx.send(Ok(None)).await;
        });
        Ok(ReceiverStream(rx).boxed())
    }
}

/// 将 mpsc `Receiver` 包装为独立类型，以便调用 `StreamExt::boxed`。
///
/// 新版 tokio（1.52+）已移除 `futures` feature，`mpsc::Receiver` 不再直接
/// 实现 `Stream`，因此这里基于 `Receiver::poll_recv` 自行实现。
struct ReceiverStream<R>(R);

impl<T> Stream for ReceiverStream<tokio::sync::mpsc::Receiver<T>> {
    type Item = T;
    fn poll_next(
        self: Pin<&mut ReceiverStream<tokio::sync::mpsc::Receiver<T>>>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<T>> {
        self.get_mut().0.poll_recv(cx)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::LlmConfig;
    use axum::response::IntoResponse;

    fn test_config(base_url: &str) -> LlmConfig {
        LlmConfig {
            base_url: base_url.into(),
            api_key: "sk-test".into(),
            model: "gpt-4o-mini".into(),
            timeout_secs: 5,
            prompts_dir: std::path::PathBuf::new(),
        }
    }

    /// 本地 mock OpenAI 服务（非流式 + SSE 流式）
    async fn mock_server() -> String {
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(|body: axum::body::Bytes| async move {
                let req: serde_json::Value = serde_json::from_slice(&body).unwrap();
                if req["stream"] == true {
                    let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"你好\"}}]}\n\n\
                             data: {\"choices\":[{\"delta\":{\"content\":\"，世界\"}}]}\n\n\
                             data: [DONE]\n\n";
                    sse.into_response()
                } else {
                    let resp = serde_json::json!({
                        "choices": [{"message": {"content": "完整讲解文本"}}]
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
        format!("http://{addr}/v1")
    }

    #[tokio::test]
    async fn complete_roundtrip() {
        let base_url = mock_server().await;
        let client = LlmClient::new(&test_config(&base_url));
        let text = client
            .complete("system prompt", "user prompt")
            .await
            .unwrap();
        assert_eq!(text, "完整讲解文本");
    }

    #[tokio::test]
    async fn stream_yields_tokens() {
        let base_url = mock_server().await;
        let client = LlmClient::new(&test_config(&base_url));
        let mut stream = client.stream("system prompt", "user prompt").await.unwrap();
        use futures_util::StreamExt;
        let mut tokens = Vec::new();
        while let Some(token) = stream.next().await {
            match token {
                Ok(Some(t)) => tokens.push(t),
                Ok(None) => break,
                Err(e) => panic!("stream error: {e}"),
            }
        }
        assert_eq!(tokens, vec!["你好", "，世界"]);
    }
}
