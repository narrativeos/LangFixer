//! 统计 GEC 引擎：外部统计序列模型服务的 HTTP 客户端
//!
//! 兼容 Harper / HuggingFace transformers server 类自建服务：
//! `POST {endpoint}/gec`，请求 `{"text": ..., "lang": ...}`，
//! 响应 `{"errors": [{error_type, confidence, span, original, correction?, message?}]}`
//! （span 为**请求文本内字节偏移**，与 [`shared_model::Error::span`] 同语义）。
//!
//! **预算窗分块（Detect Pipeline v2 P2，截断修复）**：下游服务普遍在
//! ~512 token 处静默截断（MacBERT max_length），全文单发等于只检前段。
//! 本层按 [`docchunk::split_document_with_context`] 滑窗切块逐窗调用（块≤窗），
//! 响应 span 平移回原文坐标；失败窗计入 [`ChunkStats`]（coverage 下降），
//! 绝不静默丢弃。窗执行情况与所有权空洞写入调用方自带的
//! [`RunReport`](crate::quality::RunReport)（C4：经调用栈携带，不落引擎共享槽，
//! 并发请求不互染；R4：空洞直达质量帐单，不再只是日志）。
//!
//! 环境变量：`GEC_STAT_ENDPOINT`（服务地址）/ `GEC_STAT_TIMEOUT_SECS` /
//! `GEC_STAT_MODEL`（模型名）/ `GEC_STAT_WINDOW_CHARS`（每窗字符预算，默认 500，
//! 为 512-token 截断线预留余量）。

use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;

use crate::detector::DetectorError;
use crate::docchunk;
use crate::engine::{GecEngine, GecEngineKind};
use crate::quality::ChunkStats;
use shared_model::Error;

/// 统计 GEC 服务配置（环境变量驱动）
#[derive(Debug, Clone)]
pub struct StatisticalConfig {
    /// 统计服务地址，如 `http://127.0.0.1:4803`
    pub endpoint: String,
    /// 请求超时
    pub timeout: Duration,
    /// 模型名（日志 / 元数据）
    pub model: String,
}

impl StatisticalConfig {
    pub fn from_env() -> Self {
        Self {
            endpoint: std::env::var("GEC_STAT_ENDPOINT")
                .unwrap_or_else(|_| "http://127.0.0.1:4803".to_string()),
            timeout: Duration::from_secs(
                std::env::var("GEC_STAT_TIMEOUT_SECS")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(30),
            ),
            model: std::env::var("GEC_STAT_MODEL").unwrap_or_else(|_| "hf-gec".to_string()),
        }
    }
}

/// 统计 GEC 引擎（序列模型服务）
pub struct StatisticalGecEngine {
    lang: String,
    client: reqwest::Client,
    config: StatisticalConfig,
    /// 每窗字符预算（服务 token 窗在 Rust 侧的字符近似；超窗即分块，不送全文）
    window_chars: usize,
}

impl StatisticalGecEngine {
    pub fn new(lang: impl Into<String>, config: StatisticalConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .build()
            .unwrap_or_default();
        // 默认 500 字/窗：MacBERT 类服务 max_length=512 token（含 [CLS]/[SEP]，
        // 中文约 1 字/token），预留边距避免服务侧静默截断尾 2 token
        let window_chars = std::env::var("GEC_STAT_WINDOW_CHARS")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|v| *v >= 100)
            .unwrap_or(500);
        Self {
            lang: lang.into(),
            client,
            config,
            window_chars,
        }
    }

    /// 从环境变量（`GEC_STAT_*`）构建
    pub fn from_env(lang: impl Into<String>) -> Self {
        Self::new(lang, StatisticalConfig::from_env())
    }

    /// 覆盖窗预算（测试与设置面板接入用）。下限 100：docchunk 目标块
    /// 地板 50 字（块上限 2×target），窗需 ≥100 才能保证 send ≤ 窗
    pub fn with_window_chars(mut self, chars: usize) -> Self {
        self.window_chars = chars.max(100);
        self
    }

    /// 按窗切块：target = max(窗/2, 50) 保证每块 send ≤ 窗
    /// （docchunk 块上限 2×target，target 地板 50）；无上下文前缀
    fn windows(&self, text: &str) -> Vec<docchunk::DocChunk> {
        docchunk::split_document_with_context(text, (self.window_chars / 2).max(50), 0)
    }
}

/// 统计服务 `/gec` 响应
#[derive(Deserialize)]
struct StatGecResponse {
    #[serde(default)]
    errors: Vec<StatError>,
}

#[derive(Deserialize)]
struct StatError {
    error_type: String,
    #[serde(default = "default_confidence")]
    confidence: f64,
    span: [usize; 2],
    original: String,
    #[serde(default)]
    correction: Option<String>,
    #[serde(default)]
    message: String,
}

fn default_confidence() -> f64 {
    0.5
}

#[async_trait]
impl GecEngine for StatisticalGecEngine {
    fn kind(&self) -> GecEngineKind {
        GecEngineKind::Statistical
    }
    fn name(&self) -> &str {
        &self.config.model
    }

    /// 滑窗逐块带段级事件版本：每窗复查前后各推一条 ChunkBegin/End
    /// （stage 用 rule——stat 臂挂载在级联的 rule 位，进度板同一节点 ①）；
    /// 单窗短文本不推（整层 Stage 已足够）。窗摘要/空洞写入 `run`（C4/R4）。
    async fn detect_traced(
        &self,
        text: &str,
        sink: Option<crate::progress::StageSink>,
        run: &mut crate::quality::RunReport,
    ) -> Result<Vec<Error>, DetectorError> {
        let url = format!("{}/gec", self.config.endpoint.trim_end_matches('/'));
        let windows = self.windows(text);
        // 所有权铺满校验（截断探测器）：窗切块必须无空洞；空洞写入本次调用
        // 的运帐（R4：非零即降级告警级，不得只留日志伪装成全质量）
        let unowned = docchunk::unowned_bytes(
            text.len(),
            &windows.iter().map(|w| w.owner).collect::<Vec<_>>(),
        );
        if unowned != 0 {
            tracing::error!(unowned, "stat 臂窗切块存在所有权空洞（切块器回归）");
            run.unowned_chars += unowned;
        }
        let total = windows.len();
        let trace = sink.is_some() && total > 1;
        let mut out: Vec<Error> = Vec::new();
        let mut failed = 0usize;
        let mut first_err: Option<DetectorError> = None;
        for (i, w) in windows.iter().enumerate() {
            if trace {
                sink.as_ref().unwrap()(crate::progress::StageEvent::ChunkBegin {
                    stage: shared_model::sources::RULE,
                    chunk: i as u32 + 1,
                    total: total as u32,
                });
            }
            let t = std::time::Instant::now();
            let r = self.post_window(&url, &w.send).await;
            if trace {
                sink.as_ref().unwrap()(crate::progress::StageEvent::ChunkEnd {
                    stage: shared_model::sources::RULE,
                    chunk: i as u32 + 1,
                    total: total as u32,
                    ok: r.is_ok(),
                    ms: t.elapsed().as_millis() as u64,
                });
            }
            match r {
                Ok(errors) => {
                    for e in errors {
                        out.push(self.remap_window_error(e, &w.send, w.owner[0]));
                    }
                }
                Err(e) => {
                    failed += 1;
                    tracing::warn!(
                        error = %e,
                        window = ?w.owner,
                        "统计臂窗失败（该窗本轮未复查，其余窗继续）"
                    );
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
            }
        }
        if total > 0 && failed == total {
            run.chunks = Some(ChunkStats {
                mode: if total > 1 { "chunked" } else { "single" }.into(),
                total,
                done: 0,
                failed,
                parse_failed: 0,
            });
            return Err(first_err.unwrap_or_else(|| {
                DetectorError::ServiceUnavailable("统计 GEC 服务全部窗失败".into())
            }));
        }
        run.chunks = Some(ChunkStats {
            mode: if total > 1 { "chunked" } else { "single" }.into(),
            total,
            done: total - failed,
            failed,
            parse_failed: 0,
        });
        out.sort_by_key(|e| e.span[0]);
        Ok(out)
    }

    async fn detect(&self, text: &str) -> Result<Vec<Error>, DetectorError> {
        self.detect_traced(text, None, &mut crate::quality::RunReport::default())
            .await
    }
}

impl StatisticalGecEngine {
    /// 单窗请求：响应 span 为窗内字节偏移
    async fn post_window(&self, url: &str, send: &str) -> Result<Vec<StatError>, DetectorError> {
        let resp = self
            .client
            .post(url)
            .json(&serde_json::json!({ "text": send, "lang": self.lang }))
            .send()
            .await
            .map_err(|e| DetectorError::ServiceUnavailable(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(DetectorError::ServiceUnavailable(format!(
                "统计 GEC 服务返回 HTTP {}",
                resp.status()
            )));
        }

        let body: StatGecResponse = resp
            .json()
            .await
            .map_err(|e| DetectorError::ServiceUnavailable(e.to_string()))?;
        Ok(body.errors)
    }

    /// 窗内错误 → 原文坐标：span 钳制到窗内并平移到原文字节偏移
    /// （服务偶尔产出越界/字符偏移时保守钳制，不丢整条）
    fn remap_window_error(&self, e: StatError, send: &str, owner_start: usize) -> Error {
        let clamp = |v: usize| -> usize {
            let mut b = v.min(send.len());
            // 钳到字符边界（越界字符偏移按字节下标处理，向后对齐）
            while b > 0 && !send.is_char_boundary(b) {
                b -= 1;
            }
            b
        };
        let mut s = clamp(e.span[0]);
        let mut en = clamp(e.span[1]);
        if en < s {
            std::mem::swap(&mut s, &mut en);
        }
        Error::new(
            &self.lang,
            e.error_type,
            e.confidence,
            [owner_start + s, owner_start + en],
            e.original,
            e.correction,
            if e.message.is_empty() {
                "统计模型检出错误"
            } else {
                &e.message
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 本地 mock 统计 GEC 服务：可指定第 N 次请求失败（None = 恒成功）；
    /// 每次成功响应在窗首回声一条错误（span 为**窗内字节偏移**，契约同 csc_server）
    async fn mock_stat_server_hook(fail_at: Option<usize>, counter: Arc<AtomicUsize>) -> String {
        let app = axum::Router::new().route(
            "/gec",
            axum::routing::post(move |body: axum::body::Bytes| async move {
                let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
                if fail_at == Some(n) {
                    return axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response();
                }
                let req: serde_json::Value = serde_json::from_slice(&body).unwrap();
                let text = req["text"].as_str().unwrap_or("").to_string();
                let original: String = text.chars().take(2).collect();
                let span_end = original.len();
                (
                    axum::http::StatusCode::OK,
                    axum::Json(serde_json::json!({
                        "errors": [
                            {
                                "error_type": "grammar",
                                "confidence": 0.9,
                                "span": [0usize, span_end],
                                "original": original,
                                "correction": "fixed",
                                "message": "echo"
                            }
                        ]
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
        format!("http://{addr}")
    }

    fn stat_config(endpoint: String) -> StatisticalConfig {
        StatisticalConfig {
            endpoint,
            timeout: Duration::from_secs(5),
            model: "mock-model".into(),
        }
    }

    /// 17 字 × N 句：window 120 → target 60 → 每窗约 3 句（块 ≤120 ≤ 窗）
    fn window_test_text(sentences: usize) -> String {
        "这句话用来构造统计臂的滑窗测试。".repeat(sentences)
    }

    #[tokio::test]
    async fn statistical_windows_and_remaps_spans() {
        let counter = Arc::new(AtomicUsize::new(0));
        let endpoint = mock_stat_server_hook(None, counter.clone()).await;
        let engine = StatisticalGecEngine::new("zh", stat_config(endpoint)).with_window_chars(120);
        let text = window_test_text(9); // 9 句 ×17 字 = 153 字 → 多窗
        let mut run = crate::quality::RunReport::default();
        let errors = engine.detect_traced(&text, None, &mut run).await.unwrap();
        assert!(counter.load(Ordering::SeqCst) >= 3, "应逐窗多次请求");
        // span 已平移回原文坐标：每条错误的 span 必须对应原文子串
        for e in &errors {
            assert_eq!(&text[e.span[0]..e.span[1]], e.original, "span 重映射错误");
        }
        // 跨窗分布：末尾窗的检出起点必须远超首窗预算
        assert!(
            errors.last().unwrap().span[0] > 120,
            "后续窗检出应落在文本后段"
        );
        let stats = run.chunks.clone().unwrap();
        assert_eq!(stats.mode, "chunked");
        assert_eq!(stats.done, stats.total);
        assert_eq!(stats.failed, 0);
        assert_eq!(stats.coverage(), Some(1.0));
    }

    #[tokio::test]
    async fn statistical_window_failure_lowers_coverage() {
        let counter = Arc::new(AtomicUsize::new(0));
        let endpoint = mock_stat_server_hook(Some(2), counter).await; // 第 2 窗失败
        let engine = StatisticalGecEngine::new("zh", stat_config(endpoint)).with_window_chars(120);
        let text = window_test_text(9);
        let mut run = crate::quality::RunReport::default();
        let errors = engine.detect_traced(&text, None, &mut run).await.unwrap();
        // 失败窗不阻断其余窗；结果仍来自第 1/3..窗
        assert!(errors.len() >= 2, "其余窗应继续产出: {errors:?}");
        for e in &errors {
            assert_eq!(&text[e.span[0]..e.span[1]], e.original);
        }
        let stats = run.chunks.unwrap();
        assert_eq!(stats.total, stats.done + 1);
        assert_eq!(stats.failed, 1);
        assert!(stats.coverage().unwrap() < 1.0, "coverage 必须下降");
    }

    #[tokio::test]
    async fn statistical_all_windows_fail_is_error() {
        let counter = Arc::new(AtomicUsize::new(0));
        let endpoint = mock_stat_server_hook(Some(1), counter).await; // 单窗全文，唯一窗失败
        let engine = StatisticalGecEngine::new("zh", stat_config(endpoint)).with_window_chars(120);
        let mut run = crate::quality::RunReport::default();
        let err = engine
            .detect_traced("短句。", None, &mut run)
            .await
            .unwrap_err();
        assert!(matches!(err, DetectorError::ServiceUnavailable(_)));
        let stats = run.chunks.unwrap();
        assert_eq!((stats.total, stats.done, stats.failed), (1, 0, 1));
    }

    #[tokio::test]
    async fn statistical_short_text_single_window_full_coverage() {
        let counter = Arc::new(AtomicUsize::new(0));
        let endpoint = mock_stat_server_hook(None, counter.clone()).await;
        let engine = StatisticalGecEngine::new("zh", stat_config(endpoint)).with_window_chars(120);
        let mut run = crate::quality::RunReport::default();
        let errors = engine
            .detect_traced("他昨天报复很大。", None, &mut run)
            .await
            .unwrap();
        assert_eq!(counter.load(Ordering::SeqCst), 1, "短文本应单窗单请求");
        assert_eq!(errors.len(), 1);
        let stats = run.chunks.unwrap();
        assert_eq!(stats.mode, "single");
        assert_eq!(stats.coverage(), Some(1.0));
    }

    #[tokio::test]
    async fn statistical_multiwindow_emits_chunk_events() {
        // 段级进度事件：多窗逐块 begin/end 各恰好一次，序号 1..=total 齐全，
        // stage = rule（stat 臂挂级联 rule 位，进度板 ① 节点）；单窗不推。
        let counter = Arc::new(AtomicUsize::new(0));
        let endpoint = mock_stat_server_hook(None, counter).await;
        let engine = StatisticalGecEngine::new("zh", stat_config(endpoint)).with_window_chars(120);
        let text = window_test_text(9);
        let events: Arc<Mutex<Vec<crate::progress::StageEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let ev = events.clone();
        let sink: crate::progress::StageSink = Arc::new(move |e| ev.lock().unwrap().push(e));
        engine
            .detect_traced(&text, Some(sink), &mut Default::default())
            .await
            .unwrap();
        let total = engine.windows(&text).len() as u32;
        assert!(total > 1, "构造应切多窗: {total}");
        let evs = events.lock().unwrap();
        let mut begins: Vec<u32> = Vec::new();
        let mut ends: Vec<(u32, bool)> = Vec::new();
        for e in evs.iter() {
            match e {
                crate::progress::StageEvent::ChunkBegin {
                    stage,
                    chunk,
                    total: t,
                } => {
                    assert_eq!(*stage, shared_model::sources::RULE);
                    assert_eq!(*t, total);
                    begins.push(*chunk);
                }
                crate::progress::StageEvent::ChunkEnd {
                    stage,
                    chunk,
                    total: t,
                    ok,
                    ..
                } => {
                    assert_eq!(*stage, shared_model::sources::RULE);
                    assert_eq!(*t, total);
                    ends.push((*chunk, *ok));
                }
                _ => {}
            }
        }
        begins.sort_unstable();
        assert_eq!(
            begins,
            (1..=total).collect::<Vec<_>>(),
            "每窗恰好一次 begin"
        );
        assert_eq!(ends.len(), total as usize, "每窗恰好一次 end");
        assert!(ends.iter().all(|(_, ok)| *ok), "全窗成功");
    }

    #[tokio::test]
    async fn statistical_single_window_no_chunk_events() {
        let counter = Arc::new(AtomicUsize::new(0));
        let endpoint = mock_stat_server_hook(None, counter).await;
        let engine = StatisticalGecEngine::new("zh", stat_config(endpoint)).with_window_chars(120);
        let events: Arc<Mutex<Vec<crate::progress::StageEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let ev = events.clone();
        let sink: crate::progress::StageSink = Arc::new(move |e| ev.lock().unwrap().push(e));
        engine
            .detect_traced("他昨天报复很大。", Some(sink), &mut Default::default())
            .await
            .unwrap();
        assert!(events.lock().unwrap().is_empty(), "单窗短文本不推段级事件");
    }

    /// 本地 mock 统计 GEC 服务（同样带 `Connection: close`，理由见
    /// [`mock_stat_server_hook`]）
    async fn mock_stat_server() -> String {
        let app = axum::Router::new().route(
            "/gec",
            axum::routing::post(|body: axum::body::Bytes| async move {
                let req: serde_json::Value = serde_json::from_slice(&body).unwrap();
                let text = req["text"].as_str().unwrap_or("");
                (
                    axum::http::StatusCode::OK,
                    axum::Json(serde_json::json!({
                        "errors": [
                            {
                                "error_type": "grammar",
                                "confidence": 0.95,
                                "span": [0, text.chars().count().min(1)],
                                "original": "mock",
                                "correction": "fixed",
                                "message": "statistical model"
                            }
                        ]
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
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn statistical_roundtrip() {
        let endpoint = mock_stat_server().await;
        let engine = StatisticalGecEngine::new("en", stat_config(endpoint));
        assert_eq!(engine.kind(), GecEngineKind::Statistical);
        assert_eq!(engine.name(), "mock-model");
        let errors = engine.detect("I saw a apple.").await.unwrap();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].lang, "en");
        assert_eq!(errors[0].error_type, "grammar");
        assert_eq!(errors[0].confidence, 0.95);
        assert_eq!(errors[0].correction.as_deref(), Some("fixed"));
        assert_eq!(errors[0].message, "statistical model");
    }

    #[tokio::test]
    async fn statistical_unavailable_when_service_down() {
        let engine = StatisticalGecEngine::new(
            "en",
            StatisticalConfig {
                endpoint: "http://127.0.0.1:1".into(),
                timeout: Duration::from_millis(200),
                model: "dead".into(),
            },
        );
        let err = engine.detect("x").await.unwrap_err();
        assert!(matches!(err, DetectorError::ServiceUnavailable(_)));
    }
}
