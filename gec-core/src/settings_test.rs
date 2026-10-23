//! 设置界面「测试」：各项服务配置的连通性 / 可用性测试
//!
//! 设置界面（Web 端 `POST /gec/settings/test` / 桌面端 `test_setting` 命令）
//! 对每一项配置发起一次最小化真实调用，验证服务可达 + 响应符合预期：
//!
//! | target         | 测试内容                                                              |
//! |----------------|----------------------------------------------------------------------|
//! | `hanlp`        | `POST {endpoint}/analyze`（HanLP v2 分词，验证返回 tokens）          |
//! | `llm`          | `POST {base_url}/chat/completions`（非流式最小提示，验证 content）  |
//! | `decision`     | `POST {endpoint}/decide` + `GET /health`（mock / 启发式回退模式明确告知） |
//! | `l3`           | `POST {endpoint}/l3-detect`（feature-model 特征模型）                |
//! | `embedding`    | `POST {endpoint}/embeddings`（OpenAI 兼容向量接口）                 |
//! | `tables_dir`   | 决策表目录存在性 + `*_gec_attribution.json` 文件计数                 |
//! | `lexicons_dir` | 规则词表目录存在性 + `*_gec_lexicon.json` 文件计数                   |
//! | `all`          | 并行执行以上全部，逐项返回结果                                       |
//!
//! 所有测试均为只读探测：不修改任何配置 / 数据；服务不可达时返回
//! `ok = false` + 具体原因（连接失败 / HTTP 状态 / 响应解析失败）。

use std::path::Path;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::decision::DecisionConfig;
use crate::embedding::EmbeddingConfig;
use crate::l3::L3FeatureConfig;

/// 单项测试结果（Web 端 `SettingsTestResult` / 桌面端同构）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestResult {
    /// 测试目标（`hanlp` / `llm` / `decision` / `l3` / `embedding` / `tables_dir` / `lexicons_dir`）
    pub target: String,
    /// 是否通过
    pub ok: bool,
    /// 结果说明（通过时为能力摘要，失败时为具体原因）
    pub message: String,
    /// 耗时（毫秒；目录类检查为本地 IO，通常 < 1ms）
    pub latency_ms: u64,
}

fn ok_result(target: &str, message: impl Into<String>, latency: Instant) -> TestResult {
    TestResult {
        target: target.to_string(),
        ok: true,
        message: message.into(),
        latency_ms: latency.elapsed().as_millis() as u64,
    }
}

fn err_result(target: &str, message: impl Into<String>, latency: Instant) -> TestResult {
    TestResult {
        target: target.to_string(),
        ok: false,
        message: message.into(),
        latency_ms: latency.elapsed().as_millis() as u64,
    }
}

/// 测试全部项目（并行执行；顺序固定，便于前端逐项展示）
pub async fn test_all(
    hanlp_endpoint: &str,
    llm_base_url: &str,
    llm_api_key: &str,
    llm_model: &str,
    llm_timeout_secs: u64,
    decision: &DecisionConfig,
    l3: &L3FeatureConfig,
    embedding: &EmbeddingConfig,
    tables_dir: &str,
    lexicons_dir: &str,
) -> Vec<TestResult> {
    let (hanlp, llm, decision, l3, embedding, tables, lexicons) = tokio::join!(
        test_hanlp(hanlp_endpoint),
        test_llm(llm_base_url, llm_api_key, llm_model, llm_timeout_secs),
        test_decision(decision),
        test_l3(l3),
        test_embedding(embedding),
        async { test_tables_dir(tables_dir) },
        async { test_lexicons_dir(lexicons_dir) },
    );
    vec![hanlp, llm, decision, l3, embedding, tables, lexicons]
}

/// HanLP 服务测试：`POST {endpoint}/analyze`（HanLP v2 分词 + POS）
pub async fn test_hanlp(endpoint: &str) -> TestResult {
    let target = "hanlp";
    let started = Instant::now();
    let endpoint = endpoint.trim();
    if endpoint.is_empty() {
        return err_result(
            target,
            "HanLP 服务地址未配置（留空时检测器无法调用远端 NLP 服务）",
            started,
        );
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_default();
    let url = format!("{}/analyze", endpoint.trim_end_matches('/'));
    let resp = client
        .post(&url)
        .json(&serde_json::json!({ "text": "测试", "language": "modern" }))
        .send()
        .await;
    match resp {
        Err(e) => err_result(target, format!("HanLP 服务连接失败（{url}）：{e}"), started),
        Ok(resp) if !resp.status().is_success() => err_result(
            target,
            format!("HanLP 服务返回 HTTP {}（{url}）", resp.status()),
            started,
        ),
        Ok(resp) => match resp.json::<serde_json::Value>().await {
            Err(e) => err_result(target, format!("HanLP 响应解析失败：{e}"), started),
            Ok(body) => match body["content"]["tokens"].as_array().cloned() {
                Some(ts) => ok_result(
                    target,
                    format!("HanLP 服务可达（分词返回 {} 个 token）", ts.len()),
                    started,
                ),
                None => err_result(
                    target,
                    "HanLP 响应缺少 content.tokens 字段（服务版本可能不兼容）",
                    started,
                ),
            },
        },
    }
}

/// LLM 测试：`POST {base_url}/chat/completions`（非流式最小提示）
pub async fn test_llm(base_url: &str, api_key: &str, model: &str, timeout_secs: u64) -> TestResult {
    let target = "llm";
    let started = Instant::now();
    if api_key.trim().is_empty() {
        return err_result(
            target,
            "LLM API Key 未配置（讲解 / LLM 检测层不可用）",
            started,
        );
    }
    let base_url = base_url.trim();
    if base_url.is_empty() {
        return err_result(target, "LLM Base URL 未配置", started);
    }
    // 测试请求用更短的超时（配置超时上限 10 秒），避免慢服务长时间挂起测试
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs.min(10).max(1)))
        .build()
        .unwrap_or_default();
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": model,
        "messages": [
            { "role": "system", "content": "You are a helpful assistant." },
            { "role": "user", "content": "回复 OK" }
        ],
        "temperature": 0,
        "max_tokens": 64,
    });
    let resp = client
        .post(&url)
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await;
    match resp {
        Err(e) => err_result(target, format!("LLM 服务连接失败（{url}）：{e}"), started),
        Ok(resp) if !resp.status().is_success() => err_result(
            target,
            format!(
                "LLM 服务返回 HTTP {}（{url}；请检查 API Key / 模型名）",
                resp.status()
            ),
            started,
        ),
        Ok(resp) => match resp.json::<serde_json::Value>().await {
            Err(e) => err_result(target, format!("LLM 响应解析失败：{e}"), started),
            Ok(body) => {
                // 思考模型（如 qwen3）的推理在 `reasoning_content`，`content` 可能为 null
                // （max_tokens 耗尽于推理阶段）——两者任一非空即视为服务可用
                let content = body["choices"][0]["message"]["content"]
                    .as_str()
                    .map(|s| s.trim().to_string());
                let reasoning = body["choices"][0]["message"]["reasoning_content"]
                    .as_str()
                    .map(|s| s.trim().to_string());
                match (content, reasoning) {
                    (Some(c), _) if !c.is_empty() => ok_result(
                        target,
                        format!("LLM 服务可达（模型 {model} 响应：{}）", truncate(&c, 40)),
                        started,
                    ),
                    (None | Some(_), Some(r)) if !r.is_empty() => ok_result(
                        target,
                        format!(
                            "LLM 服务可达（模型 {model}；思考模型：推理输出 {}，正文为空）",
                            truncate(&r, 40)
                        ),
                        started,
                    ),
                    _ => err_result(
                        target,
                        "LLM 响应缺少 choices[0].message.content / reasoning_content（接口可能不兼容）",
                        started,
                    ),
                }
            }
        },
    }
}

/// 决策模型（vegaml）服务测试：`POST {endpoint}/decide` + `GET /health`
///
/// 除连通性外，还会经 `/health` 的 `mode` 字段识别服务运行模式，
/// **mock / 启发式回退模式（非真实模型）会在结果中明确告知用户**：
/// - `mock`：`VEGAML_MOCK=1` 启动的启发式模式（非真实模型，仅开发/测试）
/// - `heuristic-fallback`：真实模式但未加载到任何模型（非真实模型）
/// - `sklearn` / `vegaml`：真实微调模型
pub async fn test_decision(config: &DecisionConfig) -> TestResult {
    let target = "decision";
    let started = Instant::now();
    if config.endpoint.trim().is_empty() {
        return err_result(
            target,
            "决策模型服务地址未配置（留空时仅环境变量 GEC_DEC_ENDPOINT 可启用）",
            started,
        );
    }
    match crate::decision::DecisionGecEngine::new("zh", config.clone())
        .decide("测试句子")
        .await
    {
        Ok(outcome) => {
            let detail = if outcome.abstained {
                "模型弃权"
            } else if !outcome.errors.is_empty() {
                "检出错误"
            } else {
                "判定无错"
            };
            // 查询服务运行模式，在结果中明确区分「真实模型」与「mock / 启发式回退」
            let (mode, versions) = decision_service_health(config).await;
            let mode_note = match mode.as_deref() {
                Some("mock") => "mock 启发式模式，非真实模型，仅用于开发/测试",
                Some("heuristic-fallback") => "未加载到任何模型，启发式回退，非真实模型",
                Some("sklearn") => "真实模型：sklearn 句级分类器（微调自 gold 数据）",
                Some("vegaml") => "真实模型：vegaml",
                Some(m) => m,
                None => "",
            };
            let version_note = versions
                .iter()
                .map(|(l, v)| format!("{l}={v}"))
                .collect::<Vec<_>>()
                .join(", ");
            let message = match (mode_note.is_empty(), version_note.is_empty()) {
                (true, true) => format!(
                    "决策模型服务可达（模型 {}；测试句判定：{}）",
                    config.model, detail
                ),
                (true, false) => format!(
                    "决策模型服务可达（模型 {}；模型版本 {}；测试句判定：{}）",
                    config.model, version_note, detail
                ),
                (false, true) => {
                    format!("决策模型服务可达（{}；测试句判定：{}）", mode_note, detail)
                }
                (false, false) => format!(
                    "决策模型服务可达（{}；模型版本 {}；测试句判定：{}）",
                    mode_note, version_note, detail
                ),
            };
            ok_result(target, message, started)
        }
        Err(e) => err_result(target, format!("决策模型服务不可用：{e}"), started),
    }
}

/// 查询决策服务 `GET /health`：返回 `(mode, versions)`。
/// - `mode`：服务运行模式（mock / heuristic-fallback / vegaml / sklearn / mixed）
/// - `versions`：各语言模型版本号（每次重训练自增 `v1, v2, ...`；
///   该语言无模型 / 旧服务无 `versions` 字段时缺省）
async fn decision_service_health(
    config: &DecisionConfig,
) -> (Option<String>, Vec<(String, String)>) {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
    {
        Ok(c) => c,
        Err(_) => return (None, Vec::new()),
    };
    let url = format!("{}/health", config.endpoint.trim_end_matches('/'));
    let body = match client.get(&url).send().await {
        Ok(r) => r,
        Err(_) => return (None, Vec::new()),
    };
    let body: serde_json::Value = match body.json().await {
        Ok(b) => b,
        Err(_) => return (None, Vec::new()),
    };
    let mode = body["mode"].as_str().map(|s| s.to_string());
    let versions = body
        .get("versions")
        .and_then(|v| v.as_object())
        .map(|m| {
            m.iter()
                .filter_map(|(l, v)| v.as_str().map(|s| (l.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();
    (mode, versions)
}

/// L3 特征模型服务测试：`POST {endpoint}/l3-detect`
pub async fn test_l3(config: &L3FeatureConfig) -> TestResult {
    let target = "l3";
    let started = Instant::now();
    if config.endpoint.trim().is_empty() {
        return err_result(
            target,
            "特征模型服务地址未配置（留空时仅环境变量 GEC_L3_ENDPOINT 可启用）",
            started,
        );
    }
    match crate::l3::L3FeatureEngine::new("zh", config.clone())
        .detect("测试句子")
        .await
    {
        Ok(outcome) => {
            let detail = if outcome.abstained {
                "模型弃权"
            } else if !outcome.errors.is_empty() {
                "检出错误"
            } else {
                "判定无错"
            };
            ok_result(
                target,
                format!(
                    "特征模型服务可达（模型 {}；测试句判定：{}）",
                    config.model, detail
                ),
                started,
            )
        }
        Err(e) => err_result(target, format!("特征模型服务不可用：{e}"), started),
    }
}

/// 嵌入引擎测试：`POST {endpoint}/embeddings`（OpenAI 兼容）
pub async fn test_embedding(config: &EmbeddingConfig) -> TestResult {
    let target = "embedding";
    let started = Instant::now();
    if !config.is_configured() {
        return err_result(
            target,
            "嵌入引擎未配置（endpoint 为空，向量召回禁用）",
            started,
        );
    }
    match crate::embedding::EmbeddingEngine::new(config.clone())
        .embed("测试")
        .await
    {
        Ok(vec) => ok_result(
            target,
            format!(
                "嵌入服务可达（模型 {}，向量维度 {}）",
                config.model,
                vec.len()
            ),
            started,
        ),
        Err(e) => err_result(target, format!("嵌入服务不可用：{e}"), started),
    }
}

/// 决策表目录测试：目录存在 + 含 `*_gec_attribution.json` 文件
pub fn test_tables_dir(dir: &str) -> TestResult {
    test_dir(dir, "_gec_attribution.json", "决策表")
}

/// 规则词表目录测试：目录存在 + 含 `*_gec_lexicon.json` 文件
pub fn test_lexicons_dir(dir: &str) -> TestResult {
    test_dir(dir, "_gec_lexicon.json", "规则词表")
}

fn test_dir(dir: &str, suffix: &str, label: &str) -> TestResult {
    let target = if suffix == "_gec_attribution.json" {
        "tables_dir"
    } else {
        "lexicons_dir"
    };
    let started = Instant::now();
    let dir = dir.trim();
    if dir.is_empty() {
        return err_result(target, format!("{label}目录未配置"), started);
    }
    let path = Path::new(dir);
    if !path.is_dir() {
        return err_result(
            target,
            format!("{label}目录不存在：{dir}（相对路径以服务端工作目录为基准）"),
            started,
        );
    }
    let mut names = Vec::new();
    let entries = match std::fs::read_dir(path) {
        Ok(e) => e,
        Err(e) => return err_result(target, format!("{label}目录读取失败：{e}"), started),
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().ends_with(suffix) {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    if names.is_empty() {
        err_result(
            target,
            format!("{label}目录存在但未找到 *{suffix} 文件（{dir}）"),
            started,
        )
    } else {
        ok_result(
            target,
            format!(
                "{label}目录正常：找到 {} 个文件（{}）",
                names.len(),
                names.join("、")
            ),
            started,
        )
    }
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max_chars).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;

    #[test]
    fn dir_missing_reports_error() {
        let r = test_dir(
            "definitely-not-exists-xyz",
            "_gec_attribution.json",
            "决策表",
        );
        assert!(!r.ok);
        assert!(r.message.contains("不存在"));
    }

    #[test]
    fn dir_empty_reports_no_files() {
        let dir = std::env::temp_dir().join(format!(
            "gec_settings_test_{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let r = test_dir(dir.to_str().unwrap(), "_gec_attribution.json", "决策表");
        assert!(!r.ok);
        assert!(r.message.contains("未找到"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dir_with_files_reports_count() {
        let dir = std::env::temp_dir().join(format!(
            "gec_settings_test_{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("zh_gec_attribution.json"), "{}").unwrap();
        std::fs::write(dir.join("en_gec_attribution.json"), "{}").unwrap();
        std::fs::write(dir.join("other.json"), "{}").unwrap();
        let r = test_dir(dir.to_str().unwrap(), "_gec_attribution.json", "决策表");
        assert!(r.ok, "message: {}", r.message);
        assert!(r.message.contains("2 个文件"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn llm_unconfigured_reports_error() {
        let r = test_llm("http://127.0.0.1:9/v1", "", "m", 5).await;
        assert!(!r.ok);
        assert!(r.message.contains("API Key 未配置"));
    }

    #[tokio::test]
    async fn hanlp_unreachable_reports_error() {
        // 127.0.0.1:9 必然不可达（连接拒绝；部分环境有透明代理拦截返回 503）
        // 两种情况下测试都应判为失败
        let r = test_hanlp("http://127.0.0.1:9").await;
        assert!(!r.ok, "不可达服务应测试失败: {}", r.message);
    }

    #[tokio::test]
    async fn hanlp_mock_server_ok() {
        // 本地 mock HanLP /analyze
        let app = axum::Router::new().route(
            "/analyze",
            axum::routing::post(|body: axum::body::Bytes| async move {
                let _ = body;
                let resp = serde_json::json!({ "content": { "tokens": [
                    { "word": "测试", "pos": "n" }
                ]}});
                (axum::http::StatusCode::OK, axum::Json(resp)).into_response()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        let r = test_hanlp(&format!("http://{addr}")).await;
        assert!(r.ok, "message: {}", r.message);
        assert!(r.message.contains("1 个 token"));
    }

    #[tokio::test]
    async fn llm_mock_server_ok() {
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(|body: axum::body::Bytes| async move {
                let req: serde_json::Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(req["model"], "m-test");
                let resp = serde_json::json!({
                    "choices": [{ "message": { "content": "OK" } }]
                });
                (axum::http::StatusCode::OK, axum::Json(resp)).into_response()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        let r = test_llm(&format!("http://{addr}/v1"), "sk-test", "m-test", 5).await;
        assert!(r.ok, "message: {}", r.message);
        assert!(r.message.contains("OK"));
    }

    #[tokio::test]
    async fn decision_mock_server_warns_not_real_model() {
        // mock 服务（/decide 弃权 + /health 报告 mode=mock）→ 结果必须明确告知「非真实模型」
        let app = axum::Router::new()
            .route(
                "/decide",
                axum::routing::post(|body: axum::body::Bytes| async move {
                    let _ = body;
                    let resp = serde_json::json!({
                        "has_error": false, "has_error_prob": 0.2, "error_type": "",
                        "abstain": true, "errors": []
                    });
                    (axum::http::StatusCode::OK, axum::Json(resp)).into_response()
                }),
            )
            .route(
                "/health",
                axum::routing::get(|| async {
                    (
                        axum::http::StatusCode::OK,
                        axum::Json(serde_json::json!({
                            "status": "ok", "mode": "mock", "backends": null
                        })),
                    )
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        let decision = DecisionConfig {
            endpoint: format!("http://{addr}"),
            ..Default::default()
        };
        let r = test_decision(&decision).await;
        assert!(r.ok, "message: {}", r.message);
        assert!(r.message.contains("mock"), "message: {}", r.message);
        assert!(r.message.contains("非真实模型"), "message: {}", r.message);
    }

    #[tokio::test]
    async fn decision_sklearn_server_reports_real_model() {
        // 真实模式（mode=sklearn）+ 检出错误（errors 用旧字段名 type → 验证 alias 兼容）
        let app = axum::Router::new()
            .route(
                "/decide",
                axum::routing::post(|body: axum::body::Bytes| async move {
                    let _ = body;
                    let resp = serde_json::json!({
                        "has_error": true, "has_error_prob": 0.9, "error_type": "word_order",
                        "abstain": false,
                        "errors": [{
                            "span": [0, 4], "type": "word_order",
                            "original": "测试句子", "message": "时态不一致"
                        }]
                    });
                    (axum::http::StatusCode::OK, axum::Json(resp)).into_response()
                }),
            )
            .route(
                "/health",
                axum::routing::get(|| async {
                    (
                        axum::http::StatusCode::OK,
                        axum::Json(serde_json::json!({
                            "status": "ok", "mode": "sklearn", "backends": {"zh": "sklearn"}
                        })),
                    )
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        let decision = DecisionConfig {
            endpoint: format!("http://{addr}"),
            ..Default::default()
        };
        let r = test_decision(&decision).await;
        assert!(r.ok, "message: {}", r.message);
        assert!(r.message.contains("真实模型"), "message: {}", r.message);
        assert!(r.message.contains("检出错误"), "message: {}", r.message);
    }

    #[tokio::test]
    async fn decision_without_health_keeps_model_name() {
        // 旧服务无 /health（404）→ 不报模式，回退显示配置的模型名
        let app = axum::Router::new().route(
            "/decide",
            axum::routing::post(|body: axum::body::Bytes| async move {
                let _ = body;
                let resp = serde_json::json!({
                    "has_error": false, "has_error_prob": 0.1, "error_type": "",
                    "abstain": false, "errors": []
                });
                (axum::http::StatusCode::OK, axum::Json(resp)).into_response()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        let decision = DecisionConfig {
            endpoint: format!("http://{addr}"),
            model: "my-model".into(),
            ..Default::default()
        };
        let r = test_decision(&decision).await;
        assert!(r.ok, "message: {}", r.message);
        assert!(r.message.contains("my-model"), "message: {}", r.message);
    }

    #[tokio::test]
    async fn test_all_collects_seven_results() {
        // 全部指向不可用配置（127.0.0.1:9 必然拒绝连接）→ 全部失败
        let decision = DecisionConfig {
            endpoint: "http://127.0.0.1:9".into(),
            ..Default::default()
        };
        let l3 = L3FeatureConfig {
            endpoint: "http://127.0.0.1:9".into(),
            ..Default::default()
        };
        let embedding = EmbeddingConfig::default();
        let results = test_all(
            "http://127.0.0.1:9",
            "http://127.0.0.1:9/v1",
            "",
            "m",
            1,
            &decision,
            &l3,
            &embedding,
            "definitely-not-exists",
            "definitely-not-exists",
        )
        .await;
        assert_eq!(results.len(), 7);
        let targets: Vec<&str> = results.iter().map(|r| r.target.as_str()).collect();
        assert_eq!(
            targets,
            vec![
                "hanlp",
                "llm",
                "decision",
                "l3",
                "embedding",
                "tables_dir",
                "lexicons_dir"
            ]
        );
        // 全部指向不可用配置 → 全部失败
        assert!(results.iter().all(|r| !r.ok));
    }
}
