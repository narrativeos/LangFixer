//! 文本嵌入引擎：调用 LLM 服务的 `/v1/embeddings` 端点生成向量表示。
//!
//! 用于知识存储的向量召回（余弦相似度匹配）。
//! 支持 OpenAI 兼容 API（`/v1/embeddings`）和自定义端点。

use serde::{Deserialize, Serialize};

use crate::circuit::CircuitBreaker;

/// 嵌入引擎配置
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingConfig {
    /// API 端点（如 `http://localhost:8080/v1`）
    pub endpoint: String,
    /// API Key（Bearer token）
    #[serde(default)]
    pub api_key: String,
    /// 嵌入模型名（如 `text-embedding-3-small`）
    #[serde(default = "default_embedding_model")]
    pub model: String,
    /// 超时（秒）
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
}

fn default_embedding_model() -> String {
    "text-embedding-3-small".to_string()
}
fn default_timeout() -> u64 {
    30
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            endpoint: String::new(),
            api_key: String::new(),
            model: default_embedding_model(),
            timeout_secs: 30,
        }
    }
}

impl EmbeddingConfig {
    /// 是否已配置 endpoint（未配置时上层应禁用向量召回）
    pub fn is_configured(&self) -> bool {
        !self.endpoint.is_empty()
    }

    /// 从环境变量构建：`EMBEDDING_ENDPOINT` / `EMBEDDING_API_KEY` / `EMBEDDING_MODEL`
    pub fn from_env() -> Self {
        Self {
            endpoint: std::env::var("EMBEDDING_ENDPOINT").unwrap_or_default(),
            api_key: std::env::var("EMBEDDING_API_KEY").unwrap_or_default(),
            model: std::env::var("EMBEDDING_MODEL").unwrap_or_else(|_| default_embedding_model()),
            timeout_secs: 30,
        }
    }
}

/// 嵌入引擎
pub struct EmbeddingEngine {
    config: EmbeddingConfig,
    client: reqwest::Client,
    /// 熔断器：服务宕机连续失败后跳闸，冷却期内请求立即失败（毫秒级），
    /// 避免每次调用都等满超时（引擎经 `Arc` 共享，熔断状态全局一致）
    breaker: CircuitBreaker,
}

impl EmbeddingEngine {
    pub fn new(config: EmbeddingConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .build()
            .unwrap_or_default();
        Self {
            config,
            client,
            breaker: CircuitBreaker::default(),
        }
    }

    /// 是否为空配置（未设置 endpoint）
    pub fn is_configured(&self) -> bool {
        !self.config.endpoint.is_empty()
    }

    /// 对文本生成嵌入向量
    pub async fn embed(&self, text: &str) -> Result<Vec<f32>, String> {
        if !self.is_configured() {
            return Err("嵌入引擎未配置（endpoint 为空）".to_string());
        }
        // 熔断器：服务宕机跳闸后冷却期内立即失败（毫秒级），
        // 避免每次调用都等满超时（调用方按既有回退路径处理）
        if !self.breaker.allow_request() {
            return Err("嵌入服务已熔断（连续失败），暂跳过；冷却到期后自动探测恢复".to_string());
        }
        let url = format!("{}/embeddings", self.config.endpoint.trim_end_matches('/'));
        let body = serde_json::json!({
            "model": self.config.model,
            "input": text,
        });
        let mut req = self
            .client
            .post(&url)
            .json(&body)
            .header("Content-Type", "application/json");
        if !self.config.api_key.is_empty() {
            req = req.bearer_auth(&self.config.api_key);
        }
        let resp = req.send().await.map_err(|e| {
            self.breaker.on_failure();
            format!("嵌入请求失败: {e}")
        })?;
        if !resp.status().is_success() {
            self.breaker.on_failure();
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("嵌入服务返回 {status}: {text}"));
        }
        let json: serde_json::Value = resp.json().await.map_err(|e| {
            self.breaker.on_failure();
            format!("解析嵌入响应失败: {e}")
        })?;
        self.breaker.on_success();
        // OpenAI 兼容格式: { "data": [{ "embedding": [f32, ...] }] }
        let embedding = json["data"][0]["embedding"]
            .as_array()
            .ok_or_else(|| "嵌入响应缺少 data[0].embedding 数组".to_string())?;
        let vec: Vec<f32> = embedding
            .iter()
            .filter_map(|v| v.as_f64().map(|f| f as f32))
            .collect();
        if vec.is_empty() {
            return Err("嵌入向量为空".to_string());
        }
        Ok(vec)
    }
    /// 批量嵌入多条文本（OpenAI 兼容 `input` 数组，一次 HTTP 往返）
    ///
    /// 用于 ②.5 句级召回：一次请求嵌入所有句子，避免逐句多次往返
    /// （Ollama 冷启动时尤其重要）。返回顺序与输入一致（按响应 `index` 排序）。
    /// 不支持数组 `input` 的服务会返回错误，调用方可回退到单条 [`embed`](Self::embed)。
    pub async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, String> {
        if !self.is_configured() {
            return Err("嵌入引擎未配置（endpoint 为空）".to_string());
        }
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        // 熔断器：同 `embed`（服务宕机跳闸后冷却期内立即失败）
        if !self.breaker.allow_request() {
            return Err("嵌入服务已熔断（连续失败），暂跳过；冷却到期后自动探测恢复".to_string());
        }
        let url = format!("{}/embeddings", self.config.endpoint.trim_end_matches('/'));
        let body = serde_json::json!({
            "model": self.config.model,
            "input": texts,
        });
        let mut req = self
            .client
            .post(&url)
            .json(&body)
            .header("Content-Type", "application/json");
        if !self.config.api_key.is_empty() {
            req = req.bearer_auth(&self.config.api_key);
        }
        let resp = req.send().await.map_err(|e| {
            self.breaker.on_failure();
            format!("嵌入请求失败: {e}")
        })?;
        if !resp.status().is_success() {
            self.breaker.on_failure();
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("嵌入服务返回 {status}: {text}"));
        }
        let json: serde_json::Value = resp.json().await.map_err(|e| {
            self.breaker.on_failure();
            format!("解析嵌入响应失败: {e}")
        })?;
        self.breaker.on_success();
        let data = json["data"]
            .as_array()
            .ok_or_else(|| "嵌入响应缺少 data 数组".to_string())?;
        let mut entries: Vec<(usize, Vec<f32>)> = data
            .iter()
            .map(|d| {
                let idx = d["index"].as_u64().unwrap_or(u64::MAX) as usize;
                let emb = d["embedding"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_f64().map(|f| f as f32))
                            .collect::<Vec<f32>>()
                    })
                    .unwrap_or_default();
                (idx, emb)
            })
            .collect();
        entries.sort_by_key(|(i, _)| *i);
        let mut out = Vec::new();
        for (_, e) in entries {
            if !e.is_empty() {
                out.push(e);
            }
        }
        if out.is_empty() {
            return Err("嵌入响应中没有有效向量".to_string());
        }
        Ok(out)
    }

    /// 预热嵌入服务（触发模型加载进内存，避免首次检测时的冷启动延迟）
    ///
    /// 设置变更（嵌入服务从「未配置 → 已配置」）时由上层异步调用；
    /// 失败不影响检测（知识召回在嵌入失败时静默跳过）。
    pub async fn warmup(&self) -> Result<(), String> {
        self.embed("warmup").await.map(|_| ())
    }
}

/// 余弦相似度（知识召回 ②.5 与「召回测试」共用；任一向量为零向量时返回 0）
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    if a.is_empty() || b.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let mag_a: f32 = a.iter().map(|x| x * x).sum();
    let mag_b: f32 = b.iter().map(|x| x * x).sum();
    let denom = (mag_a * mag_b).sqrt();
    if denom == 0.0 {
        0.0
    } else {
        (dot / denom) as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_similarity_basics() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        let c = vec![0.0, 1.0, 0.0];
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < 1e-6);
        assert!(cosine_similarity(&a, &c).abs() < 1e-6);
        // 零向量 / 维度不匹配 → 0
        assert_eq!(cosine_similarity(&[], &a), 0.0);
        assert_eq!(cosine_similarity(&a, &vec![0.0, 0.0]), 0.0);
    }

    #[test]
    fn not_configured_returns_error() {
        let engine = EmbeddingEngine::new(EmbeddingConfig::default());
        assert!(!engine.is_configured());
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(engine.embed("test"));
        assert!(result.is_err());
    }

    #[test]
    fn config_defaults() {
        let c = EmbeddingConfig::default();
        assert_eq!(c.model, "text-embedding-3-small");
        assert_eq!(c.timeout_secs, 30);
    }

    /// 本地 mock 嵌入服务：回显 input（单条 / 数组），embedding = [index, index, ...]
    async fn mock_embeddings_server() -> String {
        use axum::response::IntoResponse;
        let app = axum::Router::new().route(
            "/v1/embeddings",
            axum::routing::post(|body: axum::body::Bytes| async move {
                let req: serde_json::Value = serde_json::from_slice(&body).unwrap();
                let data = match &req["input"] {
                    serde_json::Value::String(_) => vec![serde_json::json!([1.0, 0.0])],
                    serde_json::Value::Array(items) => items
                        .iter()
                        .enumerate()
                        .map(|(i, _)| serde_json::json!([i as f64, (i + 1) as f64]))
                        .collect(),
                    _ => vec![],
                };
                (
                    axum::http::StatusCode::OK,
                    axum::Json(serde_json::json!({
                        "model": "mock",
                        "data": data
                            .iter()
                            .enumerate()
                            .map(|(i, emb)| {
                                serde_json::json!({ "index": i, "embedding": emb })
                            })
                            .collect::<Vec<_>>(),
                    })),
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
    async fn embed_batch_round_trip() {
        let endpoint = mock_embeddings_server().await;
        let engine = EmbeddingEngine::new(EmbeddingConfig {
            endpoint,
            ..Default::default()
        });
        let out = engine.embed_batch(&["a", "bb", "ccc"]).await.unwrap();
        assert_eq!(out.len(), 3);
        assert_eq!(out[0], vec![0.0, 1.0]);
        assert_eq!(out[1], vec![1.0, 2.0]);
        assert_eq!(out[2], vec![2.0, 3.0]);
    }

    #[tokio::test]
    async fn warmup_uses_embed() {
        let endpoint = mock_embeddings_server().await;
        let engine = EmbeddingEngine::new(EmbeddingConfig {
            endpoint,
            ..Default::default()
        });
        engine.warmup().await.unwrap();
    }
}
