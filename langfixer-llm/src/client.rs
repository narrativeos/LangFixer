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

/// 消息内容：OpenAI 兼容端点的 `content` 有两种合法形态——
/// 纯字符串，或 **parts 数组**（多模态：`text` / `image_url`）。
///
/// `untagged` 保证：纯文本仍然序列化为 `"content": "..."`（与多模态接入前
/// **逐字节一致**，既有讲解/检测链路零行为变化），parts 才序列化为数组。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

impl MessageContent {
    /// 取纯文本（parts 形态则按顺序拼接其中的 text parts）
    pub fn as_text(&self) -> String {
        match self {
            Self::Text(s) => s.clone(),
            Self::Parts(parts) => parts
                .iter()
                .filter_map(|p| p.text.as_deref())
                .collect::<String>(),
        }
    }

    pub fn is_text(&self) -> bool {
        matches!(self, Self::Text(_))
    }
}

/// 多模态内容片段
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentPart {
    /// `text` / `image_url`
    #[serde(rename = "type")]
    pub part_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_url: Option<ImageUrl>,
}

/// 图片片段（`url` 支持 `https://…` 与 `data:image/png;base64,…`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageUrl {
    pub url: String,
    /// OpenAI 可选：`low` / `high` / `auto`（缺省不发送，兼容严格端点）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// 聊天消息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: MessageContent,
}

impl ChatMessage {
    pub fn system(content: &str) -> Self {
        Self {
            role: "system".into(),
            content: MessageContent::Text(content.into()),
        }
    }
    pub fn user(content: &str) -> Self {
        Self {
            role: "user".into(),
            content: MessageContent::Text(content.into()),
        }
    }
    /// 多模态 user 消息（parts 顺序即模型读取顺序）
    pub fn user_parts(parts: Vec<ContentPart>) -> Self {
        Self {
            role: "user".into(),
            content: MessageContent::Parts(parts),
        }
    }
    /// 文本 + 图片（图片放后面：多数视觉模型对「指令在前、图在后」更稳）
    pub fn user_with_image(text: &str, image_url: &str) -> Self {
        Self::user_parts(vec![
            ContentPart {
                part_type: "text".into(),
                text: Some(text.into()),
                image_url: None,
            },
            ContentPart {
                part_type: "image_url".into(),
                text: None,
                image_url: Some(ImageUrl {
                    url: image_url.into(),
                    detail: None,
                }),
            },
        ])
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
    #[serde(default, deserialize_with = "deserialize_response_content")]
    content: Option<String>,
}

/// 响应 `content` 宽松解析：字符串（常规端点）或 parts 数组
/// （部分视觉端点把 `content` 返回为 `[{type:"text",text:"…"}]`）
fn deserialize_response_content<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(match value {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(s)) => Some(s),
        Some(serde_json::Value::Array(parts)) => {
            let text: String = parts
                .iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                .collect();
            if text.is_empty() { None } else { Some(text) }
        }
        Some(_) => None,
    })
}

/// 排队 / 过载状态码：429（限流）与全部 5xx（网关过载时给 502/503/504）
fn is_queue_status(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

/// `Retry-After` 的 delta-seconds 形式（网关实测用法）
///
/// HTTP-date 形式**不解析**：客户端与网关时钟未必对齐，算出来的等待不可信，
/// 回 `None` 让调用方按自己的退避表等。上限 3600s：网关乱给（比如 24h）时
/// 不能把调用方挂死，调用方另有重试预算兜底。
fn retry_after_secs(value: Option<&reqwest::header::HeaderValue>) -> Option<u64> {
    value
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(|s| s.min(3600))
}

/// 非 2xx → 错误：排队类状态码带上 `status` 与 `Retry-After`（调用方据此等），
/// 其余（401/404/400…）仍是 `Request("HTTP …")`——那是要人修配置，别重试
fn status_error(resp: &reqwest::Response) -> LlmError {
    let status = resp.status();
    if is_queue_status(status) {
        LlmError::Queued {
            status: status.as_u16(),
            retry_after_secs: retry_after_secs(resp.headers().get(reqwest::header::RETRY_AFTER)),
        }
    } else {
        LlmError::Request(format!("HTTP {status}"))
    }
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

    /// 复用同一连接池换模型（视觉档常用不同模型，避免重建客户端）
    pub fn with_model(&self, model: &str) -> Self {
        Self {
            http: self.http.clone(),
            base_url: self.base_url.clone(),
            api_key: self.api_key.clone(),
            model: if model.trim().is_empty() {
                self.model.clone()
            } else {
                model.trim().to_string()
            },
        }
    }

    /// 当前请求使用的模型名
    pub fn model(&self) -> &str {
        &self.model
    }

    /// 非流式补全：返回完整响应文本
    pub async fn complete(&self, system: &str, user: &str) -> Result<String, LlmError> {
        self.complete_messages(&[ChatMessage::system(system), ChatMessage::user(user)], 0.7)
            .await
    }

    /// 非流式补全（多模态入口：消息 `content` 可为文本或 parts）
    pub async fn complete_messages(
        &self,
        messages: &[ChatMessage],
        temperature: f32,
    ) -> Result<String, LlmError> {
        let body = CompletionRequest {
            model: self.model.clone(),
            messages: messages.to_vec(),
            stream: false,
            temperature,
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
            return Err(status_error(&resp));
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
            return Err(status_error(&resp));
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

    // ---- 多模态（P1 视觉档前置）----

    #[test]
    fn text_only_content_serialization_is_byte_identical() {
        // 铁律：纯文本分支必须与多模态接入前逐字节一致，
        // 否则既有讲解 / 检测 / 兜底链路的请求体可能被供应商端差异打断
        assert_eq!(
            serde_json::to_string(&ChatMessage::user("你好")).unwrap(),
            r#"{"role":"user","content":"你好"}"#
        );
        assert_eq!(
            serde_json::to_string(&ChatMessage::system("sys")).unwrap(),
            r#"{"role":"system","content":"sys"}"#
        );
        let round: ChatMessage = serde_json::from_str(r#"{"role":"user","content":"hi"}"#).unwrap();
        assert!(round.content.is_text());
        assert_eq!(round.content.as_text(), "hi");
    }

    #[test]
    fn image_parts_serialization_matches_openai_shape() {
        let msg = ChatMessage::user_with_image("转录这张图", "data:image/png;base64,AAAA");
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "role": "user",
                "content": [
                    { "type": "text", "text": "转录这张图" },
                    { "type": "image_url", "image_url": { "url": "data:image/png;base64,AAAA" } }
                ]
            })
        );
        // detail 只在显式给出时才发送（严格端点会拒绝未知字段）
        let with_detail = ChatMessage::user_parts(vec![ContentPart {
            part_type: "image_url".into(),
            text: None,
            image_url: Some(ImageUrl {
                url: "https://x/y.png".into(),
                detail: Some("high".into()),
            }),
        }]);
        assert_eq!(
            serde_json::to_value(&with_detail).unwrap()["content"][0]["image_url"]["detail"],
            "high"
        );
    }

    #[tokio::test]
    async fn complete_messages_sends_image_and_parses_array_content() {
        // mock 视觉端点：断言收到 image_url part，并返回 parts 数组形态的 content
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(|body: axum::body::Bytes| async move {
                let req: serde_json::Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(req["model"], "vision-model", "with_model 必须生效");
                let parts = req["messages"][1]["content"]
                    .as_array()
                    .expect("视觉请求的 content 必须是 parts 数组");
                assert_eq!(parts[0]["type"], "text");
                assert_eq!(parts[1]["type"], "image_url");
                assert!(
                    parts[1]["image_url"]["url"]
                        .as_str()
                        .unwrap()
                        .starts_with("data:image/jpeg;base64,"),
                    "必须走 data URL"
                );
                axum::Json(serde_json::json!({
                    "choices": [{
                        "message": {
                            "content": [
                                { "type": "text", "text": "[{\"text\":\"报到\"}]" }
                            ]
                        }
                    }]
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        let base = LlmClient::new(&test_config(&format!("http://{addr}/v1")));
        let client = base.with_model("vision-model");
        assert_eq!(client.model(), "vision-model");
        let out = client
            .complete_messages(
                &[
                    ChatMessage::system("只输出 JSON"),
                    ChatMessage::user_with_image("转录", "data:image/jpeg;base64,AAAA"),
                ],
                0.0,
            )
            .await
            .unwrap();
        assert_eq!(
            out, "[{\"text\":\"报到\"}]",
            "数组形态 content 应拼回纯文本"
        );
    }

    // ---- 排队感知（网关过载 ≠ 服务坏了）----

    /// mock 网关：按给定状态码回话，可选带 `Retry-After`
    async fn status_server(status: u16, retry_after: Option<&'static str>) -> String {
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(move |_body: axum::body::Bytes| async move {
                let mut headers = axum::http::HeaderMap::new();
                if let Some(v) = retry_after {
                    headers.insert(
                        axum::http::header::RETRY_AFTER,
                        axum::http::HeaderValue::from_static(v),
                    );
                }
                (
                    axum::http::StatusCode::from_u16(status).unwrap(),
                    headers,
                    "",
                )
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
    async fn queued_status_carries_retry_after() {
        let base_url = status_server(503, Some("7")).await;
        let e = LlmClient::new(&test_config(&base_url))
            .complete("s", "u")
            .await
            .unwrap_err();
        assert!(e.is_transient(), "503 是排队，不是坏了：{e}");
        assert!(e.is_queued(), "必须把「排队」与「故障」分开：{e}");
        assert_eq!(
            e.retry_after_secs(),
            Some(7),
            "网关给的等待秒数必须交给调用方（猜的退避只会短）"
        );
        assert!(e.to_string().contains("HTTP 503"), "文案要带状态码：{e}");
    }

    #[tokio::test]
    async fn rate_limited_429_is_queueing_too() {
        let base_url = status_server(429, None).await;
        let e = LlmClient::new(&test_config(&base_url))
            .complete("s", "u")
            .await
            .unwrap_err();
        assert!(e.is_queued() && e.is_transient(), "429 是限流：{e}");
        assert_eq!(e.retry_after_secs(), None, "网关没给就不该编一个数");
    }

    #[tokio::test]
    async fn auth_failure_stays_non_retryable() {
        let base_url = status_server(401, None).await;
        let e = LlmClient::new(&test_config(&base_url))
            .complete("s", "u")
            .await
            .unwrap_err();
        assert!(
            !e.is_transient() && !e.is_queued(),
            "401 是要人修配置，重试只是拖慢失败：{e}"
        );
        assert!(e.to_string().contains("HTTP 401"), "文案要带状态码：{e}");
    }

    #[test]
    fn retry_after_parsing_is_conservative() {
        use reqwest::header::HeaderValue;
        assert_eq!(
            retry_after_secs(Some(&HeaderValue::from_static("12"))),
            Some(12)
        );
        assert_eq!(
            retry_after_secs(Some(&HeaderValue::from_static(" 3 "))),
            Some(3)
        );
        assert_eq!(
            retry_after_secs(Some(&HeaderValue::from_static(
                "Wed, 21 Oct 2099 07:28:00 GMT"
            ))),
            None,
            "HTTP-date 不猜（两端时钟未必对齐）"
        );
        assert_eq!(
            retry_after_secs(Some(&HeaderValue::from_static("86400"))),
            Some(3600),
            "网关乱给时不能把调用方挂死"
        );
        assert_eq!(retry_after_secs(None), None);
    }
}
