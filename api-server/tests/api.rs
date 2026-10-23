use std::future::IntoFuture;
use std::sync::{Arc, Mutex};

use api_server::{middleware, routes, state::AppState};
use axum::response::IntoResponse;
use shared_model::{
    AttributionContext, AttributionResult, DecisionTableMeta, DecisionTableView, SessionMeta,
    SessionRecord,
};

/// 检测响应（与 api_server::routes::detect::DetectResponse 同构）
#[derive(serde::Deserialize)]
struct DetectResp {
    errors: Vec<shared_model::Error>,
    #[serde(default)]
    decision_abstained: bool,
    #[serde(default)]
    from_history: bool,
}

/// 环境变量互斥锁：并行测试会共享进程环境，改 env 的测试必须持锁
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// 取 env 锁（容忍前序测试 panic 导致的锁污染，避免连锁失败）
fn env_guard() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// 词表目录绝对路径（测试 CWD 是 api-server/，相对路径 lexicons/ 不存在）
fn lexicons_abs() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../lexicons")
}

/// 按当前进程环境构建 API server（**调用方须已持 env 锁**；
/// 内部会把 `GEC_SETTINGS_FILE` 覆盖为独立临时文件——每个 server 实例一份，
/// 避免并行测试共用/回退到同一设置文件）
///
/// - `db_file`：显式指定 SQLite 存储文件（持久化测试需要两个 server 实例共享同一文件）；
///   为 `None` 时每个 server 使用独立临时文件（并行测试间状态隔离）
/// - `lexicons_dir`：`Some(p)` 时**强制**设 `GEC_LEXICONS_DIR = p`
///   （并行测试互删/互设该变量，条件设置会引入跨测试竞态）；
///   `None` = 不触碰该变量（变量缺失时服务端回退内置默认 "lexicons"）
///
/// 设置文件始终指向独立临时路径（文件不存在 = 走环境变量）：
/// 避免回退到工作目录的 `langfixer-settings.json`（开发机本地文件若配置了
/// LLM 等参数，会覆盖测试通过环境变量注入的 mock 配置 / "未配置"前提）。
async fn build_server_locked(
    db_file: Option<&std::path::Path>,
    lexicons_dir: Option<&std::path::Path>,
) -> String {
    let tables_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../decision-tables");
    // 每个 server 独立设置文件（构建时捕获路径，避免并行测试间互相干扰）
    unsafe {
        std::env::set_var(
            "GEC_SETTINGS_FILE",
            std::env::temp_dir()
                .join(format!(
                    "langfixer_test_{}.json",
                    uuid::Uuid::new_v4().simple()
                ))
                .display()
                .to_string(),
        );
        // L3 飞轮样本目录指向临时目录（测试封闭性：干净句自动采集不写入仓库）
        std::env::set_var(
            "GEC_L3_DATA_DIR",
            std::env::temp_dir()
                .join(format!(
                    "langfixer_test_l3_{}",
                    uuid::Uuid::new_v4().simple()
                ))
                .display()
                .to_string(),
        );
        // 词表目录：显式传参时**强制**设置（而非「未设置才补设」）——
        // 并行测试会互删/互设 GEC_LEXICONS_DIR，条件设置会让本 server
        // 读到别的测试的目录（或无目录），产生跨测试竞态
        if let Some(p) = lexicons_dir {
            std::env::set_var("GEC_LEXICONS_DIR", p.display().to_string());
        }
    }
    // SQLite 文件：显式传参（持久化测试两实例共享）> 每测试独立临时文件（并行隔离）
    let db_file = db_file.map(std::path::Path::to_path_buf).or_else(|| {
        Some(std::env::temp_dir().join(format!(
            "langfixer_test_{}.db",
            uuid::Uuid::new_v4().simple()
        )))
    });
    let state = Arc::new(AppState::from_env(Some(&tables_dir), db_file.as_deref()));
    let app = routes::router(state).layer(middleware::cors::cors_layer());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, app).into_future());
    format!("http://{addr}")
}

/// 常规测试用：持锁清除 LLM / 引擎选择配置后建 server（避免并行测试间环境变量泄漏）
async fn start_server() -> String {
    let _guard = env_guard();
    unsafe {
        std::env::remove_var("LLM_API_KEY");
        std::env::remove_var("LLM_BASE_URL");
        std::env::remove_var("LLM_MODEL");
        std::env::remove_var("LLM_PROMPTS_DIR");
        std::env::remove_var("GEC_ENGINE_ZH");
        std::env::remove_var("GEC_ENGINE_EN");
        std::env::remove_var("GEC_ENGINE_JA");
        std::env::remove_var("GEC_STAT_ENDPOINT");
        std::env::remove_var("GEC_OFFLINE");
        // 指向本机 HanLP 服务（开发机默认 127.0.0.1 在 CI/离线环境不可达，
        // 会导致每个 detect 请求等满 10s 超时才走离线兜底）
        std::env::set_var("HANLP_ENDPOINT", "http://127.0.0.1:8000");
        std::env::remove_var("GEC_TABLES_DIR");
        // 每个测试独立设置文件（构建时捕获路径，避免并行测试间互相干扰；文件不存在 = 走环境变量）
        std::env::remove_var("GEC_SETTINGS_FILE");
        std::env::set_var(
            "GEC_SETTINGS_FILE",
            std::env::temp_dir()
                .join(format!(
                    "langfixer_test_{}.json",
                    uuid::Uuid::new_v4().simple()
                ))
                .display()
                .to_string(),
        );
        // 词表目录保持默认（"lexicons"）：get_settings 等测试断言默认值；
        // 需要真实词表目录的测试自行持锁设置 GEC_LEXICONS_DIR 绝对路径
        std::env::remove_var("GEC_LEXICONS_DIR");
        std::env::remove_var("GEC_TABLES_UPDATE_TOKEN");
        std::env::remove_var("CORS_ORIGINS");
    }
    build_server_locked(None, Some(&lexicons_abs())).await
}

/// 与 `start_server` 相同，但词表目录保持默认（"lexicons"）：
/// 用于断言目录默认值的测试（`build_server_locked` 传 `None` 不补设绝对路径）
async fn start_server_default_dirs() -> String {
    let _guard = env_guard();
    unsafe {
        std::env::remove_var("LLM_API_KEY");
        std::env::remove_var("LLM_BASE_URL");
        std::env::remove_var("LLM_MODEL");
        std::env::remove_var("LLM_PROMPTS_DIR");
        std::env::remove_var("GEC_ENGINE_ZH");
        std::env::remove_var("GEC_ENGINE_EN");
        std::env::remove_var("GEC_ENGINE_JA");
        std::env::remove_var("GEC_STAT_ENDPOINT");
        std::env::remove_var("GEC_OFFLINE");
        std::env::set_var("HANLP_ENDPOINT", "http://127.0.0.1:8000");
        std::env::remove_var("GEC_TABLES_DIR");
        std::env::remove_var("GEC_SETTINGS_FILE");
        std::env::remove_var("GEC_LEXICONS_DIR");
        std::env::remove_var("GEC_TABLES_UPDATE_TOKEN");
        std::env::remove_var("CORS_ORIGINS");
    }
    build_server_locked(None, None).await
}

/// 本地 mock OpenAI 兼容服务（非流式 + SSE 流式）
async fn start_mock_llm() -> String {
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(|body: axum::body::Bytes| async move {
            let req: serde_json::Value = serde_json::from_slice(&body).unwrap();
            if req["stream"] == true {
                let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"讲解A\"}}]}\n\n\
                         data: {\"choices\":[{\"delta\":{\"content\":\"【练习】\"}}]}\n\n\
                         data: {\"choices\":[{\"delta\":{\"content\":\"练习B\"}}]}\n\n\
                         data: [DONE]\n\n";
                sse.into_response()
            } else {
                let resp = serde_json::json!({
                    "choices": [{ "message": { "content": "完整讲解。\n【练习】1. 练习题" } }]
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

fn client() -> reqwest::Client {
    // 默认带 `Connection: close`：禁用 keep-alive 连接复用。
    // 本地 axum 测试 server 在 keep-alive 下偶发直接关闭连接（hyper 1.12 起
    // Connection 头处理变更），复用连接会随机得到 IncompleteMessage；
    // 每请求独立连接可完全规避（见 tests/flake_repro.rs 的排查记录）。
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(reqwest::header::CONNECTION, "close".parse().unwrap());
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .default_headers(headers)
        .build()
        .unwrap()
}

#[tokio::test]
async fn list_tables() {
    let base = start_server().await;
    let res: Vec<DecisionTableMeta> = client()
        .get(format!("{base}/gec/tables"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let langs: Vec<&str> = res.iter().map(|m| m.lang.as_str()).collect();
    assert!(langs.contains(&"zh"));
    assert!(langs.contains(&"en"));
    assert!(langs.contains(&"ja"));
}

#[tokio::test]
async fn get_table_and_404() {
    let base = start_server().await;
    let view: DecisionTableView = client()
        .get(format!("{base}/gec/tables/zh"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(view.raw.contains("decisionTableNode"));

    let status = client()
        .get(format!("{base}/gec/tables/xx"))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, http::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn attribute_zh() {
    let base = start_server().await;
    let ctx = AttributionContext {
        lang: "zh".into(),
        error_type: "spelling".into(),
        confidence: 0.95,
        session_error_count: 4,
        session_meta: None,
        grammar_point: None,
    };
    let result: AttributionResult = client()
        .post(format!("{base}/gec/attribute"))
        .json(&ctx)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(result.attribution, "habit_solidified");
    assert_eq!(result.weiner.locus, "internal");
    assert_eq!(result.trace.matched_rule, "spelling_high_conf_old_hand");
    assert!(!result.remedial.is_empty());
}

#[tokio::test]
async fn hot_reload_table() {
    let base = start_server().await;
    let c = client();

    let view: DecisionTableView = c
        .get(format!("{base}/gec/tables/zh"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mut raw = view.raw;
    raw = raw.replace("spelling_high_conf_old_hand", "spelling_hot_reloaded");

    let meta: DecisionTableMeta = c
        .put(format!("{base}/gec/tables/zh"))
        .header("content-type", "application/json")
        .body(raw)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(meta.lang, "zh");

    // 热更新后归因命中新规则 ID
    let ctx = AttributionContext {
        lang: "zh".into(),
        error_type: "spelling".into(),
        confidence: 0.95,
        session_error_count: 4,
        session_meta: None,
        grammar_point: None,
    };
    let result: AttributionResult = c
        .post(format!("{base}/gec/attribute"))
        .json(&ctx)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(result.trace.matched_rule, "spelling_hot_reloaded");

    // 非法 JSON 应被拒绝（400），且旧表保留
    let status = c
        .put(format!("{base}/gec/tables/zh"))
        .header("content-type", "application/json")
        .body("{ broken")
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, http::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn detect_english_offline_fallback() {
    let base = start_server().await;
    let body = serde_json::json!({ "lang": "en", "text": "I saw a apple." });
    let resp: DetectResp = client()
        .post(format!("{base}/gec/detect"))
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!resp.errors.is_empty(), "应检测到冠词错误（离线兜底）");
    assert!(resp.errors.iter().any(|e| e.error_type == "article"));
}

#[tokio::test]
async fn analyze_one_stop() {
    let base = start_server().await;
    let body = serde_json::json!({
        "lang": "en",
        "text": "I saw a apple.",
        "session_id": "alice",
    });
    let res: api_server::routes::analyze::AnalyzeResponse = client()
        .post(format!("{base}/gec/analyze"))
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!res.errors.is_empty());
    assert!(
        res.errors.iter().all(|e| e.attribution.is_some()),
        "每个错误都应带归因结果"
    );
}

#[tokio::test]
async fn session_get_and_record() {
    let base = start_server().await;
    let c = client();

    // 1. 创建会话
    let body = serde_json::json!({ "lang": "zh" });
    let meta: SessionMeta = c
        .post(format!("{base}/gec/sessions"))
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let sid = &meta.session_id;

    // 2. 获取会话详情
    let record: SessionRecord = c
        .get(format!("{base}/gec/sessions/{sid}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(record.meta.session_id, *sid);
    assert_eq!(record.meta.lang, "zh");

    // 3. 记录犯错
    let body = serde_json::json!({
        "lang": "zh",
        "error_type": "spelling",
    });
    let record: SessionRecord = c
        .post(format!("{base}/gec/sessions/{sid}/record"))
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(record.total_errors(), 1);
}

#[tokio::test]
async fn detect_english_with_statistical_engine() {
    // mock 统计 GEC 服务（Harper / HuggingFace transformers server 风格）
    let app = axum::Router::new().route(
        "/gec",
        axum::routing::post(|body: axum::body::Bytes| async move {
            let req: serde_json::Value = serde_json::from_slice(&body).unwrap();
            let text = req["text"].as_str().unwrap_or("");
            (
                axum::http::StatusCode::OK,
                axum::Json(serde_json::json!({
                    "errors": [
                        { "error_type": "grammar", "confidence": 0.95, "span": [0, text.chars().count().min(1)], "original": "I", "correction": "I am", "message": "statistical model" }
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
    let stat_endpoint = format!("http://{addr}");

    let base = {
        let _guard = env_guard();
        unsafe {
            std::env::set_var("GEC_ENGINE_EN", "statistical");
            std::env::set_var("GEC_STAT_ENDPOINT", &stat_endpoint);
            std::env::set_var("GEC_OFFLINE", "1");
        }
        build_server_locked(None, Some(&lexicons_abs())).await
    };

    let body = serde_json::json!({ "lang": "en", "text": "I saw a apple." });
    let resp: DetectResp = client()
        .post(format!("{base}/gec/detect"))
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    // 统计引擎结果在列
    assert!(
        resp.errors
            .iter()
            .any(|e| e.error_type == "grammar" && (e.confidence - 0.95).abs() < f64::EPSILON),
        "缺少统计引擎结果: {:?}",
        resp.errors
    );
    // 规则引擎（离线兜底）的 article 结果也被合并进来
    assert!(
        resp.errors.iter().any(|e| e.error_type == "article"),
        "缺少规则兜底结果: {:?}",
        resp.errors
    );
}

#[derive(serde::Deserialize)]
struct ExplainOut {
    explanation: String,
    exercises: String,
    model: String,
    template: String,
}

#[tokio::test]
async fn explain_returns_503_when_llm_disabled() {
    // start_server 内部持锁清除 LLM 配置
    let base = start_server().await;
    let body = serde_json::json!({
        "lang": "zh",
        "error_type": "spelling",
        "cefr": "A2",
    });
    let status = client()
        .post(format!("{base}/gec/explain"))
        .json(&body)
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, http::StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn explain_with_mock_llm_and_session() {
    let mock = start_mock_llm().await;
    let base = {
        let _guard = env_guard();
        unsafe {
            std::env::set_var("LLM_API_KEY", "sk-test");
            std::env::set_var("LLM_BASE_URL", &mock);
            std::env::set_var("LLM_PROMPTS_DIR", "../prompts");
        }
        build_server_locked(None, Some(&lexicons_abs())).await
    };
    let c = client();

    // 先给用户建档（母语 en + 一次 spelling 错误）
    let body = serde_json::json!({
        "lang": "zh",
        "error_type": "spelling",
        "native_language": "en",
    });
    let record: SessionRecord = c
        .post(format!("{base}/student/bob1/record"))
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(record.total_errors(), 1);

    // 2. 带 student_id 请求讲解 → 注入画像，返回讲解/练习切分正确
    let body = serde_json::json!({
        "lang": "zh",
        "error_type": "spelling",
        "attribution": "knowledge_gap",
        "cefr": "A2",
        "session_id": "bob1",
    });
    let res: ExplainOut = c
        .post(format!("{base}/gec/explain"))
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(res.template, "zh_A2");
    assert_eq!(res.model, "gpt-4o-mini");
    assert!(!res.explanation.is_empty());
    assert!(
        res.exercises.contains("练习题"),
        "练习切分应包含 mock 返回的练习部分: {}",
        res.exercises
    );
}

#[tokio::test]
async fn update_table_requires_token_when_configured() {
    let base = {
        let _guard = env_guard();
        unsafe {
            std::env::set_var("GEC_TABLES_UPDATE_TOKEN", "secret-token");
        }
        build_server_locked(None, Some(&lexicons_abs())).await
    };
    let c = client();

    // 先取一份合法决策表内容
    let view: DecisionTableView = c
        .get(format!("{base}/gec/tables/zh"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    // 未带 token → 401
    let status = c
        .put(format!("{base}/gec/tables/zh"))
        .header("content-type", "application/json")
        .body(view.raw.clone())
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, http::StatusCode::UNAUTHORIZED);

    // 错误 token → 401
    let status = c
        .put(format!("{base}/gec/tables/zh"))
        .header("content-type", "application/json")
        .header("x-tables-update-token", "wrong")
        .body(view.raw.clone())
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, http::StatusCode::UNAUTHORIZED);

    // 正确 token → 200
    let status = c
        .put(format!("{base}/gec/tables/zh"))
        .header("content-type", "application/json")
        .header("x-tables-update-token", "secret-token")
        .body(view.raw)
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, http::StatusCode::OK);
}

#[tokio::test]
async fn explain_stream_yields_sse_events() {
    let mock = start_mock_llm().await;
    let base = {
        let _guard = env_guard();
        unsafe {
            std::env::set_var("LLM_API_KEY", "sk-test");
            std::env::set_var("LLM_BASE_URL", &mock);
            std::env::set_var("LLM_PROMPTS_DIR", "../prompts");
        }
        build_server_locked(None, Some(&lexicons_abs())).await
    };
    let body = serde_json::json!({
        "lang": "zh",
        "error_type": "spelling",
        "cefr": "A1",
    });
    let text = client()
        .post(format!("{base}/gec/explain/stream"))
        .json(&body)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(text.contains("event: meta"), "应有 meta 事件: {text}");
    assert!(text.contains("event: token"), "应有 token 事件: {text}");
    assert!(text.contains("event: done"), "应有 done 事件: {text}");
    assert!(text.contains("讲解A"), "应逐段产出 token: {text}");
    assert!(text.contains("练习B"), "应逐段产出 token: {text}");
}

// ─────────────────────────────────────────────────────────────────────────────
// Phase 2: Rules API 测试
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn list_rules_zh() {
    let base = start_server().await;
    let resp: api_server::routes::rules::RulesResponse = client()
        .get(format!("{base}/gec/rules?lang=zh"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(resp.lang, "zh");
    assert!(!resp.categories.is_empty(), "zh 词表应有 categories");
    assert!(!resp.rules.is_empty(), "zh 词表应有 rules");
    // 每条规则应有 id 和 category
    for r in &resp.rules {
        assert!(!r.id.is_empty());
        assert!(!r.category.is_empty());
    }
}

#[tokio::test]
async fn list_rules_en() {
    let base = start_server().await;
    let resp: api_server::routes::rules::RulesResponse = client()
        .get(format!("{base}/gec/rules?lang=en"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(resp.lang, "en");
    assert!(
        resp.rules.len() >= 10,
        "en 词表应有 ≥10 条规则, got {}",
        resp.rules.len()
    );
}

#[tokio::test]
async fn list_rules_unknown_lang_404() {
    let base = start_server().await;
    let status = client()
        .get(format!("{base}/gec/rules?lang=xx"))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, http::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn toggle_rule_disable_and_reenable() {
    let base = start_server().await;
    let c = client();

    // 禁用 EN_ARTICLE 规则
    let resp: api_server::routes::rules::ToggleResponse = c
        .put(format!("{base}/gec/rules/EN_ARTICLE/toggle?enabled=false"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(resp.id, "EN_ARTICLE");
    assert!(!resp.enabled);

    // 重新启用
    let resp: api_server::routes::rules::ToggleResponse = c
        .put(format!("{base}/gec/rules/EN_ARTICLE/toggle?enabled=true"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(resp.enabled);
}

#[tokio::test]
async fn toggle_rule_nonexistent_404() {
    let base = start_server().await;
    let status = client()
        .put(format!("{base}/gec/rules/FAKE_RULE/toggle?enabled=false"))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, http::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn detect_with_category_filter() {
    let base = start_server().await;
    // 不带 category → 返回所有错误
    let body = serde_json::json!({ "lang": "en", "text": "I saw a apple." });
    let resp: DetectResp = client()
        .post(format!("{base}/gec/detect"))
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!resp.errors.is_empty());

    // 带 category=grammar → 仅返回 grammar 分类
    let resp2: DetectResp = client()
        .post(format!("{base}/gec/detect?category=grammar"))
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    for e in &resp2.errors {
        assert_eq!(
            e.category.as_deref(),
            Some("grammar"),
            "所有返回错误应为 grammar 分类"
        );
    }
}

#[tokio::test]
async fn detect_lang_variant_route() {
    let base = start_server().await;
    // POST /gec/en/detect — 语言来自路径，body 只需 text
    let body = serde_json::json!({ "text": "I saw a apple." });
    let resp: DetectResp = client()
        .post(format!("{base}/gec/en/detect"))
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!resp.errors.is_empty(), "en 变体路由应检测到错误");
    assert!(resp.errors.iter().any(|e| e.error_type == "article"));

    // 不支持的语言 → 404
    let status = client()
        .post(format!("{base}/gec/xx/detect"))
        .json(&body)
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, http::StatusCode::NOT_FOUND);
}

// ─────────────────────────────────────────────────────────────────────────────
// Phase 3: 生产加固测试
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn issue_type_populated_on_detect() {
    let base = start_server().await;
    let body = serde_json::json!({ "lang": "en", "text": "I saw a apple." });
    let resp: DetectResp = client()
        .post(format!("{base}/gec/detect"))
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    // 所有错误应有 W3C ITS 2.0 issue_type
    for e in &resp.errors {
        assert!(
            e.issue_type.is_some(),
            "每个错误应有 issue_type, got: {e:?}"
        );
    }
}

#[tokio::test]
async fn custom_word_crud_and_filter() {
    let base = start_server().await;
    let c = client();

    // 1. 添加自定义词
    let add_body = serde_json::json!({
        "lang": "en",
        "word": "apple",
        "reason": "品牌名"
    });
    let word: api_server::custom_words::CustomWord = c
        .post(format!("{base}/gec/custom-words"))
        .json(&add_body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(word.word, "apple");
    assert!(!word.id.is_negative());

    // 2. 列表
    let list: api_server::routes::custom_words::ListWordsResponse = c
        .get(format!("{base}/gec/custom-words?lang=en"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(list.words.iter().any(|w| w.word == "apple"));

    // 3. 检测时 "apple" 被白名单过滤
    let body = serde_json::json!({ "lang": "en", "text": "I saw a apple." });
    let resp: DetectResp = c
        .post(format!("{base}/gec/detect"))
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        !resp.errors.iter().any(|e| e.original == "apple"),
        "白名单词 'apple' 不应出现在结果中: {:?}",
        resp.errors
    );

    // 4. 删除自定义词
    let status = c
        .delete(format!("{base}/gec/custom-words/{}", word.id))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, http::StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn analyze_respects_custom_word_whitelist() {
    let base = start_server().await;
    let c = client();

    // 1. 基线：「检测 + 归因」对 "I saw a apple." 报告冠词错误（original = "a"）
    let body = serde_json::json!({
        "lang": "en",
        "text": "I saw a apple.",
        "session_id": "wlist-analyze",
    });
    let res: api_server::routes::analyze::AnalyzeResponse = c
        .post(format!("{base}/gec/analyze"))
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        res.errors.iter().any(|e| e.error.original == "a"),
        "基线：analyze 应报告冠词错误（original = 'a'）: {:?}",
        res.errors
    );

    // 2. 将错误的 original 加入白名单（「放弃」反馈的止血动作：
    //    reject 处理器写入的正是 error.original）
    let add_body = serde_json::json!({
        "lang": "en",
        "word": "a",
        "reason": "测试白名单"
    });
    c.post(format!("{base}/gec/custom-words"))
        .json(&add_body)
        .send()
        .await
        .unwrap();

    // 3. 再次「检测 + 归因」：白名单错误不再报告
    let res: api_server::routes::analyze::AnalyzeResponse = c
        .post(format!("{base}/gec/analyze"))
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        !res.errors.iter().any(|e| e.error.original == "a"),
        "白名单词 'a' 不应出现在 analyze 结果: {:?}",
        res.errors
    );
}

#[tokio::test]
async fn feedback_reflection_dedups_repeated_rejects() {
    let base = start_server().await;
    let c = client();

    /// 反思报告镜像（仅取断言所需字段）
    #[derive(Debug, serde::Deserialize)]
    struct Sug {
        kind: String,
        #[serde(default)]
        payload: serde_json::Value,
    }
    #[derive(Debug, serde::Deserialize)]
    struct Refl {
        diagnosis: String,
        suggestions: Vec<Sug>,
    }
    #[derive(Debug, serde::Deserialize)]
    struct RejectResp {
        action: String,
        #[serde(default)]
        reflection: Option<Refl>,
    }

    /// 放弃指定 original 的规则层错误（ZH_MISSING_SUBJECT）
    async fn reject(c: &reqwest::Client, base: &str, original: &str) -> RejectResp {
        let body = serde_json::json!({
            "lang": "zh",
            "error": {
                "lang": "zh",
                "error_type": "grammar",
                "confidence": 0.5,
                "span": [0, 2],
                "original": original,
                "message": "句子以动词开头，疑似成分残缺（缺主语）",
                "rule_id": "ZH_MISSING_SUBJECT",
            },
            "action": "reject",
        });
        c.post(format!("{base}/gec/feedback"))
            .json(&body)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    // 1) 同一文本「学习」被放弃 4 次（模拟多次重检测同一篇文档）→
    //    去重后仅 1 个独立拒绝 → 判孤立误判，不应给「停用规则」建议
    for _ in 0..3 {
        assert_eq!(reject(&c, &base, "学习").await.action, "reject");
    }
    let r = reject(&c, &base, "学习").await;
    let refl = r.reflection.expect("reject 应返回反思报告");
    assert!(
        refl.diagnosis.contains("孤立误判"),
        "同一文本反复放弃不应判高误报: {}",
        refl.diagnosis
    );
    assert!(
        !refl.suggestions.iter().any(|s| s.kind == "disable_rule"),
        "样本不足时不应给「停用规则」按钮: {:?}",
        refl.suggestions
    );
    // 孤立误判分支也给「排除该词」（精准、可逆的中间档）
    let ex = refl
        .suggestions
        .iter()
        .find(|s| s.kind == "exclude_word")
        .expect("孤立误判应给「排除该词」建议");
    assert_eq!(ex.payload["word"], "学习");
    assert_eq!(ex.payload["rule_id"], "ZH_MISSING_SUBJECT");

    // 2) 放弃第二个不同文本「跑步」→ 2 个独立拒绝、占比 100% → 高误报 → 停用建议
    let r = reject(&c, &base, "跑步").await;
    let refl = r.reflection.expect("reject 应返回反思报告");
    assert!(
        refl.diagnosis.contains("误报率偏高"),
        "2 个不同文本被放弃应判高误报: {}",
        refl.diagnosis
    );
    assert!(
        refl.suggestions.iter().any(|s| s.kind == "disable_rule"),
        "高误报应给「停用规则」建议: {:?}",
        refl.suggestions
    );
    // 高误报分支同样给「排除该词」（word = 本次被放弃的 original）
    let ex = refl
        .suggestions
        .iter()
        .find(|s| s.kind == "exclude_word")
        .expect("高误报分支应给「排除该词」建议");
    assert_eq!(ex.payload["word"], "跑步");
    assert_eq!(ex.payload["rule_id"], "ZH_MISSING_SUBJECT");
}

/// 反思建议执行：`exclude_word` 写回词表 `rules[].excludes`（临时词表副本，
/// 不写仓库文件）；幂等去重；规则不在词表中 → 500
#[tokio::test]
async fn feedback_action_exclude_word_writes_lexicon() {
    let _guard = env_guard();
    // 词表目录复制到临时目录（测试封闭性：仓库词表不被测试修改）
    let dir = std::env::temp_dir().join(format!(
        "langfixer_test_lexicons_{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    for entry in std::fs::read_dir(lexicons_abs()).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), dir.join(entry.file_name())).unwrap();
    }
    // 测试自包含：清空仓库词表中 ZH_MISSING_SUBJECT 的既有 excludes（断言前提是「初始为空」）
    {
        let p = dir.join("zh_gec_lexicon.json");
        let mut v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        if let Some(rules) = v.get_mut("rules").and_then(|x| x.as_array_mut()) {
            for r in rules.iter_mut() {
                if r["id"] == "ZH_MISSING_SUBJECT" {
                    r["excludes"] = serde_json::Value::Null;
                }
            }
        }
        std::fs::write(&p, serde_json::to_string_pretty(&v).unwrap()).unwrap();
    }
    unsafe {
        std::env::remove_var("GEC_LEXICONS_DIR");
    }
    let base = build_server_locked(None, Some(&dir)).await;
    let c = client();

    // 1) 新写入：词表文件出现 excludes 条目
    let resp: serde_json::Value = c
        .post(format!("{base}/gec/feedback/action"))
        .json(&serde_json::json!({
            "action": "exclude_word",
            "payload": { "lang": "zh", "rule_id": "ZH_MISSING_SUBJECT", "word": "靠山" }
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(resp["ok"], true);
    assert!(
        resp["message"].as_str().unwrap().contains("已加入"),
        "{}",
        resp["message"]
    );
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("zh_gec_lexicon.json")).unwrap())
            .unwrap();
    let rule = v["rules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "ZH_MISSING_SUBJECT")
        .unwrap();
    assert_eq!(rule["excludes"], serde_json::json!(["靠山"]));

    // 2) 幂等：重复追加不报错、不重复
    let resp2: serde_json::Value = c
        .post(format!("{base}/gec/feedback/action"))
        .json(&serde_json::json!({
            "action": "exclude_word",
            "payload": { "lang": "zh", "rule_id": "ZH_MISSING_SUBJECT", "word": "靠山" }
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(resp2["ok"], true);
    assert!(
        resp2["message"].as_str().unwrap().contains("已在"),
        "{}",
        resp2["message"]
    );
    let v2: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("zh_gec_lexicon.json")).unwrap())
            .unwrap();
    let rule2 = v2["rules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "ZH_MISSING_SUBJECT")
        .unwrap();
    assert_eq!(rule2["excludes"].as_array().unwrap().len(), 1);

    // 3) 规则不在词表文件中 → 500
    let status = c
        .post(format!("{base}/gec/feedback/action"))
        .json(&serde_json::json!({
            "action": "exclude_word",
            "payload": { "lang": "zh", "rule_id": "NO_SUCH_RULE", "word": "x" }
        }))
        .send()
        .await
        .unwrap()
        .status();
    assert!(status.is_server_error(), "未知规则应报错: {status}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn detect_cache_returns_consistent_results() {
    let base = start_server().await;
    let body = serde_json::json!({ "lang": "en", "text": "I saw a apple." });
    let c = client();

    // 第一次检测
    let r1: DetectResp = c
        .post(format!("{base}/gec/detect"))
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    // 第二次检测（应命中缓存，结果一致）
    let r2: DetectResp = c
        .post(format!("{base}/gec/detect"))
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(r1.errors.len(), r2.errors.len(), "缓存前后结果数量应一致");
    for (a, b) in r1.errors.iter().zip(r2.errors.iter()) {
        assert_eq!(a.error_type, b.error_type);
        assert_eq!(a.original, b.original);
    }
}

/// 重复检测复用历史（端到端）：
/// - 第一个 server 实例检测 → 写历史（含参数快照）；
/// - 第二个实例（同 SQLite、空 LRU）检测同文本 → 命中历史复用（`from_history=true`，结果一致）；
/// - 热更新规则侧（禁用一条规则 → 版本指纹变化）→ 再检测同文本 → 不复用（`from_history=false`）。
#[tokio::test]
async fn detect_history_reuse_and_rules_invalidation() {
    let _g = env_guard();
    unsafe {
        std::env::remove_var("LLM_API_KEY");
        std::env::remove_var("LLM_BASE_URL");
        std::env::remove_var("LLM_MODEL");
        std::env::remove_var("LLM_PROMPTS_DIR");
        std::env::remove_var("GEC_ENGINE_ZH");
        std::env::remove_var("GEC_ENGINE_EN");
        std::env::remove_var("GEC_ENGINE_JA");
        std::env::remove_var("GEC_STAT_ENDPOINT");
        std::env::remove_var("GEC_OFFLINE");
        std::env::set_var("HANLP_ENDPOINT", "http://127.0.0.1:8000");
        std::env::remove_var("GEC_TABLES_DIR");
        std::env::remove_var("GEC_SETTINGS_FILE");
        std::env::remove_var("GEC_LEXICONS_DIR");
        std::env::remove_var("GEC_TABLES_UPDATE_TOKEN");
        std::env::remove_var("CORS_ORIGINS");
    }
    // 两个 server 实例共享同一 SQLite（历史跨实例可见；各自 LRU 独立）
    let db = std::env::temp_dir().join(format!(
        "langfixer_reuse_{}.db",
        uuid::Uuid::new_v4().simple()
    ));
    let _ = std::fs::remove_file(&db);
    // 词表用临时副本（测试封闭性：规则开关回写词表文件，不能触碰仓库文件）
    let lex_dir = std::env::temp_dir().join(format!(
        "langfixer_reuse_lexicons_{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&lex_dir).unwrap();
    for entry in std::fs::read_dir(lexicons_abs()).unwrap() {
        let entry = entry.unwrap();
        if entry.path().is_file() {
            std::fs::copy(entry.path(), lex_dir.join(entry.file_name())).unwrap();
        }
    }
    let c = client();
    let text = "I saw a apple.";
    let body = serde_json::json!({ "lang": "en", "text": text });

    // ① 第一个实例：全新检测（无历史可复用）
    let base1 = build_server_locked(Some(&db), Some(&lex_dir)).await;
    let r1: DetectResp = c
        .post(format!("{base1}/gec/detect"))
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!r1.from_history, "首次检测必须是全新运行");
    assert!(!r1.errors.is_empty(), "应检测到冠词错误");

    // 历史写入是后台任务：轮询直到可见
    let mut seen = false;
    for _ in 0..50 {
        let page: serde_json::Value = c
            .get(format!("{base1}/gec/history?lang=en&limit=20"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let items: Vec<serde_json::Value> = page["items"].as_array().cloned().unwrap_or_default();
        if items.iter().any(|h| h["text_preview"] == text) {
            seen = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(seen, "首次检测的历史应已持久化");

    // ② 第二个实例（空 LRU）：同文本 → 历史复用
    let base2 = build_server_locked(Some(&db), Some(&lex_dir)).await;
    let r2: DetectResp = c
        .post(format!("{base2}/gec/detect"))
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(r2.from_history, "第二实例同文本检测应复用历史");
    assert_eq!(r1.errors.len(), r2.errors.len(), "复用结果应与原结果一致");
    for (a, b) in r1.errors.iter().zip(r2.errors.iter()) {
        assert_eq!(a.error_type, b.error_type);
        assert_eq!(a.original, b.original);
    }

    // ③ 热更新规则侧（禁用一条 en 规则 → 版本指纹变化）→ 不再复用
    let rules: serde_json::Value = c
        .get(format!("{base2}/gec/rules?lang=en"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let rule_id = rules["rules"][0]["id"]
        .as_str()
        .expect("en 词表应有规则")
        .to_string();
    let status = c
        .put(format!("{base2}/gec/rules/en/toggles"))
        .json(&serde_json::json!({ "toggles": { rule_id: false } }))
        .send()
        .await
        .unwrap()
        .status();
    assert!(status.is_success(), "规则禁用应成功: {status}");

    let r3: DetectResp = c
        .post(format!("{base2}/gec/detect"))
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        !r3.from_history,
        "规则侧变化后同文本检测必须全新运行（指纹不匹配）"
    );

    let _ = std::fs::remove_file(&db);
    let _ = std::fs::remove_dir_all(&lex_dir);
}

/// 决策模型重训练（版本号变化）→ 同文本历史不再可复用：
/// v1 服务下首次检测（历史带 model_version=v1）→ v1 服务第二实例复用
/// → 切换到 v2 服务（模拟重训练后重启）→ 版本不匹配，必须全新运行
#[tokio::test]
async fn detect_history_reuse_and_model_version_invalidation() {
    let _g = env_guard();
    unsafe {
        std::env::remove_var("LLM_API_KEY");
        std::env::remove_var("LLM_BASE_URL");
        std::env::remove_var("LLM_MODEL");
        std::env::remove_var("LLM_PROMPTS_DIR");
        std::env::remove_var("GEC_ENGINE_ZH");
        std::env::remove_var("GEC_ENGINE_EN");
        std::env::remove_var("GEC_ENGINE_JA");
        std::env::remove_var("GEC_STAT_ENDPOINT");
        std::env::remove_var("GEC_OFFLINE");
        std::env::set_var("HANLP_ENDPOINT", "http://127.0.0.1:8000");
        std::env::remove_var("GEC_TABLES_DIR");
        std::env::remove_var("GEC_SETTINGS_FILE");
        std::env::remove_var("GEC_LEXICONS_DIR");
        std::env::remove_var("GEC_TABLES_UPDATE_TOKEN");
        std::env::remove_var("CORS_ORIGINS");
        // P0.5 起决策层默认关闭：显式 opt-in（本测试验证决策模型版本对历史复用的影响）
        std::env::set_var("GEC_DEC_ENABLED", "1");
    }
    let db = std::env::temp_dir().join(format!(
        "langfixer_modelver_{}.db",
        uuid::Uuid::new_v4().simple()
    ));
    let _ = std::fs::remove_file(&db);
    let c = client();
    let text = "I saw a apple.";
    let body = serde_json::json!({ "lang": "en", "text": text });

    // ① mock 决策服务 v1：首次检测（历史 params 记录 model_version=v1）
    unsafe {
        std::env::set_var(
            "GEC_DEC_ENDPOINT",
            start_mock_decision(false, Some("v1")).await,
        );
    }
    let base1 = build_server_locked(Some(&db), Some(&lexicons_abs())).await;
    let r1: DetectResp = c
        .post(format!("{base1}/gec/detect"))
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!r1.from_history, "首次检测必须是全新运行");
    assert!(!r1.errors.is_empty(), "应检测到冠词错误");

    // 历史写入是后台任务：轮询直到可见，并确认参数快照携带模型版本
    let mut params_seen: Option<bool> = None;
    for _ in 0..50 {
        let page: serde_json::Value = c
            .get(format!("{base1}/gec/history?lang=en&limit=20"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let items: Vec<serde_json::Value> = page["items"].as_array().cloned().unwrap_or_default();
        if let Some(h) = items.iter().find(|h| h["text_preview"] == text) {
            let p: Option<serde_json::Value> = h["params_json"]
                .as_str()
                .and_then(|s| serde_json::from_str(s).ok());
            if p.as_ref()
                .and_then(|p| p.get("model_version"))
                .and_then(|v| v.as_str())
                == Some("v1")
            {
                params_seen = Some(true);
                break;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(
        params_seen.unwrap_or(false),
        "历史参数快照应记录决策模型版本 v1"
    );

    // ② 同版本（v1）第二实例：同文本 → 历史复用
    let base2 = build_server_locked(Some(&db), Some(&lexicons_abs())).await;
    let r2: DetectResp = c
        .post(format!("{base2}/gec/detect"))
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(r2.from_history, "同版本（v1）同文本检测应复用历史");
    assert_eq!(r1.errors.len(), r2.errors.len(), "复用结果应与原结果一致");

    // ③ 模型重训练：服务切换到 v2 → 版本不匹配 → 必须全新运行
    unsafe {
        std::env::set_var(
            "GEC_DEC_ENDPOINT",
            start_mock_decision(false, Some("v2")).await,
        );
    }
    let base3 = build_server_locked(Some(&db), Some(&lexicons_abs())).await;
    let r3: DetectResp = c
        .post(format!("{base3}/gec/detect"))
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        !r3.from_history,
        "模型版本变化（v1 → v2）后同文本检测必须全新运行"
    );

    let _ = std::fs::remove_file(&db);
}

// ─────────────────────────────────────────────────────────────────────────────
// 设置界面（GET/PUT /gec/settings）
// ─────────────────────────────────────────────────────────────────────────────

/// 发送请求并读取响应文本；连接层瞬态错误（ConnectionReset / IncompleteMessage）
/// 有界重试 3 次（并行测试高负载下本地服务可能瞬时断连；重试不改变测试语义）
async fn send_text(
    c: &reqwest::Client,
    method: &str,
    url: &str,
    body: Option<&serde_json::Value>,
) -> String {
    for attempt in 0..3u32 {
        let m = match method {
            "GET" => reqwest::Method::GET,
            "POST" => reqwest::Method::POST,
            "PUT" => reqwest::Method::PUT,
            _ => panic!("未知方法 {method}"),
        };
        let mut rb = c.request(m, url);
        if let Some(b) = body {
            rb = rb.json(b);
        }
        match rb.send().await {
            Ok(r) => match r.text().await {
                Ok(t) => return t,
                Err(e) if is_transport_err(&e) && attempt < 2 => {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    continue;
                }
                Err(e) => panic!("读取响应失败: {e}"),
            },
            Err(e) if is_transport_err(&e) && attempt < 2 => {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                continue;
            }
            Err(e) => panic!("请求失败（已重试）: {e}"),
        }
    }
    unreachable!()
}

/// 是否为连接层瞬态错误（ConnectionReset / IncompleteMessage 等，reqwest 显示为
/// "error sending request for url"）
fn is_transport_err(e: &reqwest::Error) -> bool {
    e.to_string().contains("error sending request")
}

#[tokio::test]
async fn get_settings_returns_current_config() {
    let base = start_server_default_dirs().await;
    let text = send_text(&client(), "GET", &format!("{base}/gec/settings"), None).await;
    let res: serde_json::Value = serde_json::from_str(&text).unwrap();
    // start_server 已显式将 HANLP_ENDPOINT 指向本机 HanLP 服务
    assert_eq!(res["hanlp_endpoint"], "http://127.0.0.1:8000");
    assert_eq!(res["llm"]["base_url"], "https://api.openai.com/v1");
    assert_eq!(res["llm"]["model"], "gpt-4o-mini");
    assert_eq!(res["llm"]["timeout_secs"], 30);
    assert_eq!(res["llm"]["prompts_dir"], "prompts");
    assert_eq!(res["llm"]["enabled"], false);
    // 离线开关 / 目录配置默认值（start_server_default_dirs 已清除 GEC_OFFLINE / GEC_TABLES_DIR / GEC_LEXICONS_DIR 环境变量）
    assert_eq!(res["offline_mode"], false);
    assert_eq!(res["tables_dir"], "decision-tables");
    assert_eq!(res["lexicons_dir"], "lexicons");
    // 知识层总开关默认开启（测评消融臂 knowledge:false 的基线）
    assert_eq!(res["knowledge_enabled"], true);
    // LLM 请求模式：前端默认流式，外部 API 默认非流式
    assert_eq!(res["llm_modes"]["frontend"], "stream");
    assert_eq!(res["llm_modes"]["external_api"], "non_stream");
}

#[tokio::test]
async fn put_settings_updates_llm_then_explain_works() {
    let mock = start_mock_llm().await;
    let base = {
        let _guard = env_guard();
        unsafe {
            // 独立设置文件：避免沿用并行测试已写入 LLM 配置的共享文件
            std::env::remove_var("GEC_SETTINGS_FILE");
            std::env::set_var("LLM_PROMPTS_DIR", "../prompts");
        }
        build_server_locked(None, Some(&lexicons_abs())).await
    };
    let c = client();

    // 未配置 LLM → 503
    let body = serde_json::json!({ "lang": "zh", "error_type": "spelling", "cefr": "A2" });
    let status = c
        .post(format!("{base}/gec/explain"))
        .json(&body)
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, http::StatusCode::SERVICE_UNAVAILABLE);

    // 设置界面更新 LLM 参数（部分更新：仅 llm）
    let text = send_text(
        &c,
        "PUT",
        &format!("{base}/gec/settings"),
        Some(&serde_json::json!({
            "llm": {
                "base_url": mock,
                "api_key": "sk-test",
                "model": "gpt-4o-mini",
                "timeout_secs": 30,
            }
        })),
    )
    .await;
    let res: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(res["llm"]["enabled"], true);
    // start_server 已将 HANLP_ENDPOINT 指向本机 HanLP 服务，部分更新不应改动它
    assert_eq!(
        res["hanlp_endpoint"], "http://127.0.0.1:8000",
        "未提供的字段应保持不变"
    );

    // 非流式讲解（外部 API 默认模式）立即可用
    let text = send_text(&c, "POST", &format!("{base}/gec/explain"), Some(&body)).await;
    let out: ExplainOut = serde_json::from_str(&text).unwrap();
    assert_eq!(out.model, "gpt-4o-mini");
    assert!(!out.explanation.is_empty());

    // 流式讲解（前端默认模式）同样可用
    let text = send_text(
        &c,
        "POST",
        &format!("{base}/gec/explain/stream"),
        Some(&body),
    )
    .await;
    assert!(text.contains("event: token"), "应有 token 事件: {text}");
}

#[tokio::test]
async fn put_settings_updates_hanlp_endpoint_and_rebuilds_detectors() {
    // mock HanLP 服务：「的」(DEG) + 后续副词 (AD) → 命中 的/地/得 规则
    let app = axum::Router::new().route(
        "/analyze",
        axum::routing::post(|body: axum::body::Bytes| async move {
            let _ = body;
            (
                axum::http::StatusCode::OK,
                axum::Json(serde_json::json!({
                    "content": { "tokens": [
                        { "text": "他", "pos": "PN", "span": [0, 1] },
                        { "text": "的", "pos": "DEG", "span": [1, 2] },
                        { "text": "很快", "pos": "AD", "span": [2, 4] }
                    ] }
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
    let hanlp_url = format!("http://{addr}");

    // 持锁贯穿整个测试：GEC_OFFLINE=1 启动（初始检测器离线，不发网络请求），
    // 且避免并行测试在重建检测器时改动 GEC_ENGINE_ZH 等环境变量
    let _guard = env_guard();
    unsafe {
        std::env::set_var("GEC_OFFLINE", "1");
    }
    // 测试自包含：词表用临时副本（不触碰仓库文件），并强制启用 ZH_DE_DA_DE
    // （生产词表该规则可能已停用；本测试目标是 HanLP 重建路径，不是规则状态）
    let lex_dir = std::env::temp_dir().join(format!(
        "langfixer_test_lexicons_hanlp_{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&lex_dir).unwrap();
    for entry in std::fs::read_dir(lexicons_abs()).unwrap() {
        let entry = entry.unwrap();
        if entry.path().is_file() {
            std::fs::copy(entry.path(), lex_dir.join(entry.file_name())).unwrap();
        }
    }
    {
        let p = lex_dir.join("zh_gec_lexicon.json");
        let mut v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        if let Some(rules) = v.get_mut("rules").and_then(|x| x.as_array_mut()) {
            for r in rules.iter_mut() {
                if r["id"] == "ZH_DE_DA_DE" {
                    r["enabled"] = serde_json::json!(true);
                }
            }
        }
        std::fs::write(&p, serde_json::to_string_pretty(&v).unwrap()).unwrap();
    }
    let base = build_server_locked(None, Some(&lex_dir)).await;
    // 构建时捕获的设置文件路径（后续并行测试可能改写进程级 GEC_SETTINGS_FILE）
    let settings_file = std::env::var("GEC_SETTINGS_FILE").unwrap();
    let c = client();

    // 变更前：离线兜底（的/地/得 规则不依赖 POS，离线不可达）
    let before_text = send_text(
        &c,
        "POST",
        &format!("{base}/gec/detect"),
        Some(&serde_json::json!({ "lang": "zh", "text": "他的很快" })),
    )
    .await;
    let before: DetectResp = serde_json::from_str(&before_text).unwrap();
    assert!(
        !before
            .errors
            .iter()
            .any(|e| e.rule_id.as_deref() == Some("ZH_DE_DA_DE")),
        "离线模式不应命中 的/地/得 规则: {:?}",
        before.errors
    );

    // 设置界面更新 HanLP 地址并关闭离线开关（显式切换 → 重建 zh/en 检测器）
    let text = send_text(
        &c,
        "PUT",
        &format!("{base}/gec/settings"),
        Some(&serde_json::json!({ "hanlp_endpoint": hanlp_url, "offline_mode": false })),
    )
    .await;
    let res: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(res["hanlp_endpoint"], hanlp_url);
    assert_eq!(res["offline_mode"], false, "离线开关应已关闭");

    // 变更后：新地址生效，mock 的 POS 结果命中 的/地/得 规则
    let after_text = send_text(
        &c,
        "POST",
        &format!("{base}/gec/detect"),
        Some(&serde_json::json!({ "lang": "zh", "text": "我跑的很快" })),
    )
    .await;
    let after: DetectResp = serde_json::from_str(&after_text).unwrap();
    assert!(
        after
            .errors
            .iter()
            .any(|e| e.rule_id.as_deref() == Some("ZH_DE_DA_DE")),
        "地址变更重建检测器后应命中 的/地/得 规则: {:?}",
        after.errors
    );

    // 设置已持久化到文件（构建时捕获的 GEC_SETTINGS_FILE 路径）
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(settings_file).unwrap()).unwrap();
    assert_eq!(saved["hanlp_endpoint"], hanlp_url);
    let _ = std::fs::remove_dir_all(&lex_dir);
}

// ─────────────────────────────────────────────────────────────────────────────
// 决策表编辑器后端（validate / try / versions / rollback / toggles）
// ─────────────────────────────────────────────────────────────────────────────

/// dry-run 校验：合法表 ok=true；坏表 ok=false 且给出具体错误（不替换当前表）
#[tokio::test]
async fn validate_table_dry_run() {
    let base = start_server().await;
    let c = client();

    // 合法：当前 zh 表原样通过
    let view: DecisionTableView = c
        .get(format!("{base}/gec/tables/zh"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let res: serde_json::Value = c
        .post(format!("{base}/gec/tables/zh/validate"))
        .json(&serde_json::Value::String(view.raw.clone()))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(res["ok"], true, "合法表应通过校验: {}", res["errors"]);

    // 非法：缺少 decisionTableNode
    let res: serde_json::Value = c
        .post(format!("{base}/gec/tables/zh/validate"))
        .json(&serde_json::json!({ "nodes": [], "edges": [] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(res["ok"], false);
    assert!(!res["errors"].as_array().unwrap().is_empty());

    // 校验是 dry-run：当前表未被替换
    let view2: DecisionTableView = c
        .get(format!("{base}/gec/tables/zh"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(view2.raw, view.raw, "dry-run 校验不应替换当前表");
}

/// try 端点：用给定上下文对当前内存表试算
#[tokio::test]
async fn try_table_evaluates_current_engine() {
    let base = start_server().await;
    let ctx = AttributionContext {
        lang: "zh".into(),
        error_type: "spelling".into(),
        confidence: 0.95,
        session_error_count: 4,
        session_meta: None,
        grammar_point: None,
    };
    let result: AttributionResult = client()
        .post(format!("{base}/gec/tables/zh/try"))
        .json(&ctx)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(result.attribution, "habit_solidified");
    assert_eq!(result.trace.matched_rule, "spelling_high_conf_old_hand");

    // 未加载的语言 → 404
    let status = client()
        .post(format!("{base}/gec/tables/xx/try"))
        .json(&ctx)
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, http::StatusCode::NOT_FOUND);
}

/// 版本历史 + 回滚：两次 PUT 产生两个版本，回滚到较早版本后内容与之一致
///（用 ja 表：其他并行测试只改 zh 表，避免版本列表互相污染）
#[tokio::test]
async fn table_versions_and_rollback() {
    let base = start_server().await;
    let c = client();

    let view: DecisionTableView = c
        .get(format!("{base}/gec/tables/ja"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let original = view.raw.clone();
    let v1 = original.replace("ja", "ja_v1_marker");
    let v2 = original.replace("ja", "ja_v2_marker");

    // 两次 PUT → 两个版本
    let m1: DecisionTableMeta = c
        .put(format!("{base}/gec/tables/ja"))
        .header("content-type", "application/json")
        .body(v1.clone())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(m1.rule_count > 0, "meta 应带真实规则数");
    let m2: DecisionTableMeta = c
        .put(format!("{base}/gec/tables/ja"))
        .header("content-type", "application/json")
        .body(v2.clone())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(m2.rule_count > 0);

    // 版本列表（新 → 旧）：最新两条必为本次 v2、v1（中间无其他 ja 写入）
    let text = send_text(
        &c,
        "GET",
        &format!("{base}/gec/tables/ja/versions?limit=200"),
        None,
    )
    .await;
    let versions: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap();
    assert!(versions.len() >= 2, "至少应有本次两个版本: {versions:?}");
    let v2_id = versions[0]["id"].as_i64().unwrap();
    let v1_id = versions[1]["id"].as_i64().unwrap();
    assert_ne!(v1_id, v2_id);

    // 回滚到 v1 → 当前表内容应与 v1 一致
    let rb: DecisionTableMeta = c
        .post(format!("{base}/gec/tables/ja/rollback"))
        .json(&serde_json::json!({ "version": v1_id }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(rb.lang, "ja");

    let view3: DecisionTableView = c
        .get(format!("{base}/gec/tables/ja"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    // 语义比较：PUT 后服务端按 serde_json::Value 重存（空白规范化），内容须一致
    let a: serde_json::Value = serde_json::from_str(&view3.raw).unwrap();
    let b: serde_json::Value = serde_json::from_str(&v1).unwrap();
    assert_eq!(a, b, "回滚后表内容应与目标版本一致（语义比较）");

    // 回滚本身也产生新版本
    let text = send_text(
        &c,
        "GET",
        &format!("{base}/gec/tables/ja/versions?limit=200"),
        None,
    )
    .await;
    let versions2: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap();
    assert!(
        versions2.len() > versions.len(),
        "回滚后版本数应增加: {} -> {}",
        versions.len(),
        versions2.len()
    );

    // 回滚到不存在的版本 → 404
    let status = c
        .post(format!("{base}/gec/tables/ja/rollback"))
        .json(&serde_json::json!({ "version": 99999999 }))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, http::StatusCode::NOT_FOUND);
}

/// 规则开关批量持久化：PUT toggles → SQLite 落库 → 新进程（同 db 文件）加载后状态一致
#[tokio::test]
async fn put_rule_toggles_persists_and_reloads() {
    // 固定的 db 文件：两个 server 实例共享，验证重启后状态保持
    let db_file = std::env::temp_dir().join(format!(
        "langfixer_test_rules_{}.db",
        uuid::Uuid::new_v4().simple()
    ));
    // 持锁贯穿整个测试，避免并行测试在两个 server 实例间改动进程级环境变量
    let _guard = env_guard();
    let base = build_server_locked(Some(&db_file), Some(&lexicons_abs())).await;
    let c = client();

    // 批量停用两条 zh 规则
    let text = send_text(
        &c,
        "PUT",
        &format!("{base}/gec/rules/zh/toggles"),
        Some(&serde_json::json!({
            "toggles": { "ZH_CONFUSABLE": false, "ZH_PUNCTUATION": true }
        })),
    )
    .await;
    let res: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(res["lang"], "zh");
    assert_eq!(res["toggles"]["ZH_CONFUSABLE"], false);
    assert_eq!(res["toggles"]["ZH_PUNCTUATION"], true);
    assert!(res["count"].as_u64().unwrap() >= 4, "应返回全部规则状态");

    // 列表接口反映运行时状态（而非词表静态值）
    let text = send_text(&c, "GET", &format!("{base}/gec/rules?lang=zh"), None).await;
    let rules: serde_json::Value = serde_json::from_str(&text).unwrap();
    let confusable = rules["rules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "ZH_CONFUSABLE")
        .unwrap();
    assert_eq!(confusable["enabled"], false, "列表应反映运行时停用状态");

    // 新进程（同 db 文件）加载后状态保持
    let base2 = build_server_locked(Some(&db_file), Some(&lexicons_abs())).await;
    let text = send_text(&c, "GET", &format!("{base2}/gec/rules?lang=zh"), None).await;
    let rules2: serde_json::Value = serde_json::from_str(&text).unwrap();
    let confusable2 = rules2["rules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "ZH_CONFUSABLE")
        .unwrap();
    assert_eq!(confusable2["enabled"], false, "重启后停用状态应保持");

    // 未知规则 id → 404；空 toggles → 400
    let status = c
        .put(format!("{base}/gec/rules/zh/toggles"))
        .json(&serde_json::json!({ "toggles": { "NO_SUCH_RULE": false } }))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, http::StatusCode::NOT_FOUND);
    let status = c
        .put(format!("{base}/gec/rules/zh/toggles"))
        .json(&serde_json::json!({ "toggles": {} }))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, http::StatusCode::BAD_REQUEST);

    let _ = std::fs::remove_file(&db_file);
}

// ─────────────────────────────────────────────────────────────────────────────
// 三级响应（level 1/2/3）+ LLM 兜底
// ─────────────────────────────────────────────────────────────────────────────

/// 本地 mock 决策模型服务：`abstain` 参数控制弃权信号；
/// `model_version` 非空时 `/health` 报告各语言模型版本号（重训练版本切换测试用）
async fn start_mock_decision(abstain: bool, model_version: Option<&str>) -> String {
    let versions = match model_version {
        Some(v) => serde_json::json!({ "en": v, "zh": v }),
        None => serde_json::json!({}),
    };
    let app = axum::Router::new()
        .route(
            "/decide",
            axum::routing::post(move |body: axum::body::Bytes| async move {
                let req: serde_json::Value = serde_json::from_slice(&body).unwrap();
                let _ = req["text"].as_str().unwrap_or("");
                let resp = serde_json::json!({
                    "has_error": false,
                    "has_error_prob": 0.3,
                    "error_type": "none",
                    "abstain": abstain,
                    "errors": []
                });
                (axum::http::StatusCode::OK, axum::Json(resp)).into_response()
            }),
        )
        .route(
            "/health",
            axum::routing::get(move || {
                let versions = versions.clone();
                async move {
                    (
                        axum::http::StatusCode::OK,
                        axum::Json(serde_json::json!({
                            "status": "ok",
                            "mode": "sklearn",
                            "versions": versions,
                        })),
                    )
                        .into_response()
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

/// 本地 mock LLM 检测服务：返回 JSON 数组（含一个 source=llm 的兜底错误）
async fn start_mock_llm_detect() -> String {
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(move |body: axum::body::Bytes| async move {
            let req: serde_json::Value = serde_json::from_slice(&body).unwrap();
            let _ = req["model"].as_str().unwrap_or("");
            let resp = serde_json::json!({
                "choices": [{ "message": { "content":
                    "[{\"error_type\":\"grammar\",\"confidence\":0.85,\"original\":\"报复\",\"correction\":\"抱负\",\"message\":\"LLM 兜底检出：同音错别字\"}]"
                } }]
            });
            (axum::http::StatusCode::OK, axum::Json(resp)).into_response()
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    format!("http://{addr}/v1")
}

/// 配置 LLM 检测 + 决策层环境变量（持 env 锁）
///
/// P0.5 起决策层默认关闭：显式 `GEC_DEC_ENABLED=1` opt-in；
/// L3 指向不可达端口保证测试封闭性（开发机 4806 若跑着真实 feature-model
/// 服务，L3 高置信定案会改变 level 2/3 的 LLM 触发行为）
async fn setup_llm_and_decision(abstain: bool) {
    let llm = start_mock_llm_detect().await;
    let dec = start_mock_decision(abstain, None).await;
    unsafe {
        std::env::set_var("LLM_API_KEY", "sk-test");
        std::env::set_var("LLM_BASE_URL", &llm);
        // 检测模板目录（仓库根 prompts/；测试进程 CWD 为 api-server/，需绝对路径）
        std::env::set_var(
            "LLM_PROMPTS_DIR",
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../prompts")
                .display()
                .to_string(),
        );
        std::env::set_var("GEC_DEC_ENABLED", "1");
        std::env::set_var("GEC_DEC_ENDPOINT", &dec);
        std::env::set_var("GEC_L3_ENDPOINT", "http://127.0.0.1:1");
    }
}

/// level 2：决策模型弃权 → 触发 LLM 兜底，结果含 source=llm 的错误
#[tokio::test]
async fn detect_level2_triggers_llm_on_decision_abstain() {
    let _guard = env_guard();
    setup_llm_and_decision(true).await;
    let base = build_server_locked(None, Some(&lexicons_abs())).await;
    let c = client();
    let text = send_text(
        &c,
        "POST",
        &format!("{base}/gec/detect"),
        Some(&serde_json::json!({ "lang": "zh", "text": "他昨天报复很大。", "level": 2 })),
    )
    .await;
    let resp: DetectResp = serde_json::from_str(&text).unwrap();
    assert!(
        resp.errors
            .iter()
            .any(|e| e.source.as_deref() == Some("llm")),
        "决策模型弃权后应含 LLM 兜底错误: {text}"
    );
    assert!(resp.decision_abstained, "弃权信号应为 true");
}

/// level 3：无条件强制 LLM 兜底（含 1 级无错误的情形）
#[tokio::test]
async fn detect_level3_triggers_llm_when_no_errors() {
    let _guard = env_guard();
    // 决策层明确无错（非弃权）→ 1 级结果为空 → level 3 触发 LLM
    setup_llm_and_decision(false).await;
    // L3 特征模型层指向不可达端口：保证测试封闭性
    // （开发机 4806 若跑着真实 feature-model 服务，「明确无错」会放行而不触发 LLM）
    unsafe {
        std::env::set_var("GEC_L3_ENDPOINT", "http://127.0.0.1:1");
    }
    let base = build_server_locked(None, Some(&lexicons_abs())).await;
    let c = client();
    let text = send_text(
        &c,
        "POST",
        &format!("{base}/gec/detect"),
        Some(&serde_json::json!({ "lang": "zh", "text": "这是一个正确的句子。", "level": 3 })),
    )
    .await;
    let resp: DetectResp = serde_json::from_str(&text).unwrap();
    assert!(
        resp.errors
            .iter()
            .any(|e| e.source.as_deref() == Some("llm")),
        "level 3 应无条件触发 LLM 兜底（1 级无错误）: {text}"
    );
    // 决策模型 has_error_prob=0.3 在灰区内 → 弃权信号为 true（正确行为）
    assert!(resp.decision_abstained, "灰区概率应报告弃权");
}

/// level 2 触发但 LLM 未配置 → 503
#[tokio::test]
async fn detect_level2_without_llm_configured_returns_503() {
    let _guard = env_guard();
    unsafe {
        std::env::remove_var("LLM_API_KEY");
        std::env::remove_var("LLM_BASE_URL");
        // P0.5 起决策层默认关闭：显式 opt-in（弃权触发 level 2 LLM 兜底）；
        // L3 指向不可达端口保证封闭性
        std::env::set_var("GEC_DEC_ENABLED", "1");
        std::env::set_var("GEC_L3_ENDPOINT", "http://127.0.0.1:1");
        std::env::set_var("GEC_DEC_ENDPOINT", start_mock_decision(true, None).await);
    }
    let base = build_server_locked(None, Some(&lexicons_abs())).await;
    let c = client();
    let status = c
        .post(format!("{base}/gec/detect"))
        .json(&serde_json::json!({ "lang": "zh", "text": "他昨天报复很大。", "level": 2 }))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(
        status,
        http::StatusCode::SERVICE_UNAVAILABLE,
        "LLM 未配置时 level 2 弃权触发应返回 503"
    );
}

/// 非法 level → 400
#[tokio::test]
async fn detect_invalid_level_returns_400() {
    // 注意：不要在本测试里持有 env_guard 再调 start_server——
    // start_server 内部会取同一把非递归锁，外层持锁调用会自死锁
    let base = start_server().await;
    let c = client();
    for level in [0u8, 4, 99] {
        let status = c
            .post(format!("{base}/gec/detect"))
            .json(&serde_json::json!({ "lang": "zh", "text": "x", "level": level }))
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(
            status,
            http::StatusCode::BAD_REQUEST,
            "level {level} 应返回 400"
        );
    }
}

/// force_llm（「漏报」）：1 级已有错误时，level 1/2 本不触发 LLM，
/// force_llm=true 后无条件对全文跑 LLM 兜底
#[tokio::test]
async fn detect_force_llm_triggers_llm_even_with_errors() {
    let _guard = env_guard();
    // LLM 检测（mock 返回 span 错误）
    let llm = start_mock_llm_detect().await;
    // 决策层：明确无错（prob 0.1 < 灰区下限 0.25 → 非弃权）
    let dec_app = axum::Router::new().route(
        "/decide",
        axum::routing::post(|body: axum::body::Bytes| async move {
            let _ = body;
            (
                axum::http::StatusCode::OK,
                axum::Json(serde_json::json!({
                    "has_error": false,
                    "has_error_prob": 0.1,
                    "error_type": "none",
                    "abstain": false,
                    "errors": []
                })),
            )
                .into_response()
        }),
    );
    let dec_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dec_addr = dec_listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(dec_listener, dec_app).await.ok();
    });
    unsafe {
        std::env::set_var("LLM_API_KEY", "sk-test");
        std::env::set_var("LLM_BASE_URL", &llm);
        std::env::set_var(
            "LLM_PROMPTS_DIR",
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../prompts")
                .display()
                .to_string(),
        );
        std::env::set_var("GEC_DEC_ENDPOINT", format!("http://{dec_addr}"));
        // P0.5：L3 是唯一的 pre-LLM 门控——本测试只验证 force_llm 契约，
        // 显式关闭 L3 保证封闭性（level 2 基线 = 纯规则，不依赖 L3 服务状态）
        std::env::set_var("GEC_L3_ENABLED", "0");
    }
    let base = build_server_locked(None, Some(&lexicons_abs())).await;
    let c = client();

    // 基线：level 2 非弃权 → 不触发 LLM（结果中无 source=llm 错误）
    let text = send_text(
        &c,
        "POST",
        &format!("{base}/gec/detect"),
        Some(&serde_json::json!({ "lang": "zh", "text": "他昨天报复很大。", "level": 2 })),
    )
    .await;
    let resp: DetectResp = serde_json::from_str(&text).unwrap();
    assert!(
        !resp
            .errors
            .iter()
            .any(|e| e.source.as_deref() == Some("llm")),
        "非弃权且未强制时不应含 LLM 错误: {text}"
    );

    // force_llm = true → 无条件 LLM 兜底
    let text = send_text(
        &c,
        "POST",
        &format!("{base}/gec/detect"),
        Some(&serde_json::json!({
            "lang": "zh",
            "text": "他昨天报复很大。",
            "level": 2,
            "force_llm": true
        })),
    )
    .await;
    let resp: DetectResp = serde_json::from_str(&text).unwrap();
    assert!(
        resp.errors
            .iter()
            .any(|e| e.source.as_deref() == Some("llm")),
        "force_llm 应无条件触发 LLM 兜底: {text}"
    );
}

/// force_llm 触发但 LLM 未配置 → 503
///
/// 注意：`build_server_locked` 已把 `GEC_SETTINGS_FILE` 指向独立临时文件，
/// 避免工作目录的 `langfixer-settings.json`（开发机可能配置了真实 LLM）
/// 覆盖本测试通过环境变量构造的"未配置"前提。
#[tokio::test]
async fn detect_force_llm_without_llm_configured_returns_503() {
    let _guard = env_guard();
    unsafe {
        std::env::remove_var("LLM_API_KEY");
        std::env::remove_var("LLM_BASE_URL");
    }
    let base = build_server_locked(None, Some(&lexicons_abs())).await;
    let c = client();
    let status = c
        .post(format!("{base}/gec/detect"))
        .json(&serde_json::json!({
            "lang": "zh",
            "text": "他昨天报复很大。",
            "force_llm": true
        }))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(
        status,
        http::StatusCode::SERVICE_UNAVAILABLE,
        "LLM 未配置时 force_llm 应返回 503"
    );
}
