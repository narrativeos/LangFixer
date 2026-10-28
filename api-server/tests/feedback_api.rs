//! 统一反馈 + 反思流程集成测试（`/gec/feedback*`）
//!
//! 覆盖：放弃 → 白名单 + 反馈留痕 + 反思报告（规则 / 知识 / 模型层）；
//! 采纳 → 规则确认 / LLM 蒸馏；建议执行（停用规则 / 删示例 / 删知识）；
//! 反馈历史；LLM 深度反思未配置时 503。

use std::sync::Arc;
use std::sync::Mutex;

use api_server::{middleware, routes, state::AppState};

/// 环境变量互斥锁（与 tests/api.rs 相同的并行隔离约定）
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn env_guard() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// 词表目录：**出厂词表的临时副本**（每次调用一份）
///
/// 采纳 / 反馈类路由会把规则开关回写词表文件（`sync_rules_to_file`），
/// 直接指向仓库 `lexicons/` 会让测试改到出厂默认（实测见 `api.rs` 同名函数注释）。
fn lexicons_abs() -> std::path::PathBuf {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../lexicons");
    let tmp = std::env::temp_dir().join(format!(
        "langfixer_fb_lex_{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    for entry in std::fs::read_dir(&src).unwrap() {
        let entry = entry.unwrap();
        if entry.path().is_file() {
            std::fs::copy(entry.path(), tmp.join(entry.path().file_name().unwrap())).unwrap();
        }
    }
    tmp
}

/// 构建 server（调用方须持 env 锁）；`db_file` 显式指定 SQLite 文件
async fn build_server(db_file: Option<&std::path::Path>) -> String {
    let tables_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../decision-tables");
    unsafe {
        std::env::set_var(
            "GEC_SETTINGS_FILE",
            std::env::temp_dir()
                .join(format!(
                    "langfixer_fb_{}.json",
                    uuid::Uuid::new_v4().simple()
                ))
                .display()
                .to_string(),
        );
        std::env::set_var(
            "GEC_L3_DATA_DIR",
            std::env::temp_dir()
                .join(format!("langfixer_fb_l3_{}", uuid::Uuid::new_v4().simple()))
                .display()
                .to_string(),
        );
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
        std::env::set_var("GEC_LEXICONS_DIR", lexicons_abs().display().to_string());
        std::env::remove_var("GEC_TABLES_UPDATE_TOKEN");
        std::env::remove_var("CORS_ORIGINS");
    }
    let db_file = db_file.map(std::path::Path::to_path_buf).or_else(|| {
        Some(
            std::env::temp_dir().join(format!("langfixer_fb_{}.db", uuid::Uuid::new_v4().simple())),
        )
    });
    let state = Arc::new(AppState::from_env(Some(&tables_dir), db_file.as_deref()));
    let app = routes::router(state).layer(middleware::cors::cors_layer());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, app).into_future());
    format!("http://{addr}")
}

fn client() -> reqwest::Client {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(reqwest::header::CONNECTION, "close".parse().unwrap());
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .default_headers(headers)
        .build()
        .unwrap()
}

/// 构造错误 JSON（与 shared_model::Error 同构）
fn error_json(
    lang: &str,
    error_type: &str,
    original: &str,
    correction: Option<&str>,
    source: Option<&str>,
    rule_id: Option<&str>,
) -> serde_json::Value {
    let mut e = serde_json::json!({
        "lang": lang,
        "error_type": error_type,
        "confidence": 0.9,
        "span": [0, original.len()],
        "original": original,
        "message": "测试错误",
    });
    if let Some(c) = correction {
        e["correction"] = serde_json::json!(c);
    }
    if let Some(s) = source {
        e["source"] = serde_json::json!(s);
    }
    if let Some(r) = rule_id {
        e["rule_id"] = serde_json::json!(r);
    }
    e
}

async fn post_json(
    c: &reqwest::Client,
    url: &str,
    body: &serde_json::Value,
) -> (reqwest::StatusCode, serde_json::Value) {
    let resp = c.post(url).json(body).send().await.unwrap();
    let status = resp.status();
    let text = resp.text().await.unwrap();
    (
        status,
        serde_json::from_str(&text).unwrap_or(serde_json::json!({ "raw": text })),
    )
}

#[tokio::test]
async fn reject_rule_error_produces_reflection_and_history() {
    let _g = env_guard();
    let base = build_server(None).await;
    let c = client();
    // 规则层错误（无 source → 归一为 rule）
    let body = serde_json::json!({
        "lang": "zh",
        "error": error_json("zh", "spelling", "报复", Some("抱负"), None, Some("ZH_SPELL")),
        "action": "reject",
        "reason": "这个词在上下文中是正确的",
    });
    let (status, res) = post_json(&c, &format!("{base}/gec/feedback"), &body).await;
    assert_eq!(status, reqwest::StatusCode::OK, "reject 应成功: {}", res);
    assert!(res["word_id"].is_number(), "应返回白名单词 id: {}", res);
    let refl = &res["reflection"];
    assert_eq!(refl["source"], "rule");
    assert_eq!(refl["subject"], "ZH_SPELL");
    assert!(
        refl["evidence"][0]
            .as_str()
            .unwrap()
            .contains("用户放弃原因"),
        "reason 应进入证据: {}",
        refl
    );
    // 反馈历史：该规则应有 1 条 reject
    let hist = c
        .get(format!("{base}/gec/feedback/history?lang=zh&limit=20"))
        .send()
        .await
        .unwrap();
    let hist: serde_json::Value = serde_json::from_str(&hist.text().await.unwrap()).unwrap();
    assert_eq!(hist["lang"], "zh");
    let rule_stat = hist["stats"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["rule_id"] == "ZH_SPELL");
    assert!(rule_stat.is_some(), "历史统计应含 ZH_SPELL: {}", hist);
    assert_eq!(rule_stat.unwrap()["rejected"], 1);
    assert_eq!(hist["recent"][0]["action"], "reject");
    assert_eq!(hist["recent"][0]["source"], "rule");
}

#[tokio::test]
async fn reject_rule_high_reject_rate_diagnoses() {
    let _g = env_guard();
    let base = build_server(None).await;
    let c = client();
    // 先采纳一次（让规则入库，反思上下文可定位到规则定义）
    let body = serde_json::json!({
        "lang": "zh",
        "error": error_json("zh", "spelling", "热词0", Some("正词0"), None, Some("ZH_HOT")),
        "action": "adopt",
    });
    post_json(&c, &format!("{base}/gec/feedback"), &body).await;
    // 连续放弃同一规则 4 次 → 误报率偏高诊断
    let mut last = serde_json::Value::Null;
    for i in 0..4 {
        let body = serde_json::json!({
            "lang": "zh",
            "error": error_json("zh", "spelling", &format!("误词{i}"), Some(format!("正词{i}").as_str()), None, Some("ZH_HOT")),
            "action": "reject",
        });
        let (status, res) = post_json(&c, &format!("{base}/gec/feedback"), &body).await;
        assert_eq!(status, reqwest::StatusCode::OK);
        last = res;
    }
    let diag = last["reflection"]["diagnosis"].as_str().unwrap();
    assert!(
        diag.contains("误报率偏高"),
        "4 次放弃应判误报率偏高: {diag}"
    );
    let kinds: Vec<&str> = last["reflection"]["suggestions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"disable_rule"), "应建议停用规则: {kinds:?}");
    assert!(kinds.contains(&"view_rule"));
}

#[tokio::test]
async fn reject_model_error_reflects_model_layer() {
    let _g = env_guard();
    let base = build_server(None).await;
    let c = client();
    let body = serde_json::json!({
        "lang": "zh",
        "error": error_json("zh", "grammar", "这个句子有问题", None, Some("feature"), None),
        "action": "reject",
    });
    let (status, res) = post_json(&c, &format!("{base}/gec/feedback"), &body).await;
    assert_eq!(status, reqwest::StatusCode::OK, "{}", res);
    let refl = &res["reflection"];
    assert_eq!(refl["source"], "feature");
    assert!(refl["diagnosis"].as_str().unwrap().contains("L3 特征模型"));
    let kinds: Vec<&str> = refl["suggestions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["kind"].as_str().unwrap())
        .collect();
    assert!(
        kinds.contains(&"l3_negative"),
        "模型层应建议 L3 负样本: {kinds:?}"
    );
    assert!(kinds.contains(&"retrain"));
}

#[tokio::test]
async fn reject_knowledge_error_locates_item() {
    let _g = env_guard();
    let db = std::env::temp_dir().join(format!(
        "langfixer_fb_kn_{}.db",
        uuid::Uuid::new_v4().simple()
    ));
    let base = build_server(Some(&db)).await;
    let c = client();
    // 直接向知识存储写入条目（与服务端共用同一 db 文件）
    let store = lexicon_store::KnowledgeStore::open(&db).unwrap();
    let id = store
        .insert(
            "zh",
            "他昨天吃了昨天。",
            "grammar",
            "他昨天吃了昨天的饭。",
            &[0.9, 0.1, 0.0, 0.0],
            "llm_adopt",
        )
        .unwrap();
    assert!(id > 0);
    // 放弃知识层错误（original 与条目文本一致 → 精确定位）
    let body = serde_json::json!({
        "lang": "zh",
        "error": error_json("zh", "grammar", "他昨天吃了昨天。", Some("他昨天吃了昨天的饭。"), Some("knowledge"), None),
        "action": "reject",
    });
    let (status, res) = post_json(&c, &format!("{base}/gec/feedback"), &body).await;
    assert_eq!(status, reqwest::StatusCode::OK, "{}", res);
    let refl = &res["reflection"];
    assert_eq!(refl["source"], "knowledge");
    assert_eq!(refl["subject"], format!("知识条目 #{id}"), "{}", refl);
    let sug = &refl["suggestions"][0];
    assert_eq!(sug["kind"], "remove_knowledge");
    assert_eq!(sug["payload"]["id"], id);
    // 执行建议：删除知识条目
    let act = serde_json::json!({
        "action": "remove_knowledge",
        "payload": { "id": id, "text": "他昨天吃了昨天。" },
    });
    let (status, res) = post_json(&c, &format!("{base}/gec/feedback/action"), &act).await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert!(res["ok"] == true, "{}", res);
    let items = store.list("zh").unwrap();
    assert!(items.is_empty(), "知识条目应已删除");
    let _ = std::fs::remove_file(&db);
}
#[tokio::test]
async fn adopt_rule_error_confirms_and_reinforces() {
    let _g = env_guard();
    let base = build_server(None).await;
    let c = client();
    let body = serde_json::json!({
        "lang": "zh",
        "error": error_json("zh", "spelling", "报复", Some("抱负"), None, Some("ZH_SPELL")),
        "action": "adopt",
    });
    let (status, res) = post_json(&c, &format!("{base}/gec/feedback"), &body).await;
    assert_eq!(status, reqwest::StatusCode::OK, "{}", res);
    assert_eq!(res["via"], "confirm");
    assert!(res["message"].as_str().unwrap().contains("ZH_SPELL"));
    // 规则应已入库（自动创建，默认停用）
    let rules = c
        .get(format!("{base}/gec/rules?lang=zh"))
        .send()
        .await
        .unwrap();
    let rules: serde_json::Value = serde_json::from_str(&rules.text().await.unwrap()).unwrap();
    let row = rules["rules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "ZH_SPELL");
    assert!(row.is_some(), "采纳后规则应入库: {}", rules);
    assert_eq!(row.unwrap()["example_count"], 1, "示例补强应入库");
}

#[tokio::test]
async fn feedback_action_disable_and_remove_example() {
    let _g = env_guard();
    let base = build_server(None).await;
    let c = client();
    // 先采纳一条规则错误（入库示例）
    let body = serde_json::json!({
        "lang": "zh",
        "error": error_json("zh", "spelling", "报复", Some("抱负"), None, Some("ZH_SPELL")),
        "action": "adopt",
    });
    post_json(&c, &format!("{base}/gec/feedback"), &body).await;
    // 启用规则
    let tog = c
        .put(format!("{base}/gec/rules/ZH_SPELL/toggle?enabled=true"))
        .send()
        .await
        .unwrap();
    assert_eq!(tog.status(), reqwest::StatusCode::OK);
    // 执行 disable_rule 建议
    let act = serde_json::json!({
        "action": "disable_rule",
        "payload": { "lang": "zh", "rule_id": "ZH_SPELL" },
    });
    let (status, res) = post_json(&c, &format!("{base}/gec/feedback/action"), &act).await;
    assert_eq!(status, reqwest::StatusCode::OK, "{}", res);
    assert!(res["ok"] == true);
    // 规则应已停用
    let rules = c
        .get(format!("{base}/gec/rules?lang=zh"))
        .send()
        .await
        .unwrap();
    let rules: serde_json::Value = serde_json::from_str(&rules.text().await.unwrap()).unwrap();
    let row = rules["rules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "ZH_SPELL")
        .unwrap();
    assert_eq!(row["enabled"], false, "disable_rule 后规则应停用");
    // 执行 remove_example 建议
    let act = serde_json::json!({
        "action": "remove_example",
        "payload": { "lang": "zh", "rule_id": "ZH_SPELL", "wrong": "报复", "right": "抱负" },
    });
    let (status, res) = post_json(&c, &format!("{base}/gec/feedback/action"), &act).await;
    assert_eq!(status, reqwest::StatusCode::OK, "{}", res);
    assert!(res["ok"] == true, "{}", res);
    // 重复删除 → false（幂等提示）
    let (status, res) = post_json(&c, &format!("{base}/gec/feedback/action"), &act).await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert!(res["ok"] == false, "重复删除应返回 false: {}", res);
}

#[tokio::test]
async fn reject_llm_error_is_terminal_reflection() {
    let _g = env_guard();
    let base = build_server(None).await;
    let c = client();
    let body = serde_json::json!({
        "lang": "zh",
        "error": error_json("zh", "grammar", "这个句子有问题", None, Some("llm"), None),
        "action": "reject",
    });
    let (status, res) = post_json(&c, &format!("{base}/gec/feedback"), &body).await;
    assert_eq!(status, reqwest::StatusCode::OK, "{}", res);
    let refl = &res["reflection"];
    assert_eq!(refl["source"], "llm");
    assert!(refl["diagnosis"].as_str().unwrap().contains("白名单"));
    assert!(refl["suggestions"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn reflect_llm_unconfigured_returns_503() {
    let _g = env_guard();
    let base = build_server(None).await;
    let c = client();
    let body = serde_json::json!({
        "lang": "zh",
        "error": error_json("zh", "spelling", "报复", Some("抱负"), None, Some("ZH_SPELL")),
        "reason": "误报",
    });
    let (status, res) = post_json(&c, &format!("{base}/gec/feedback/reflect-llm"), &body).await;
    assert_eq!(
        status,
        reqwest::StatusCode::SERVICE_UNAVAILABLE,
        "LLM 未配置应 503: {}",
        res
    );
}

#[tokio::test]
async fn invalid_action_returns_400() {
    let _g = env_guard();
    let base = build_server(None).await;
    let c = client();
    let body = serde_json::json!({
        "lang": "zh",
        "error": error_json("zh", "spelling", "报复", None, None, None),
        "action": "maybe",
    });
    let (status, _) = post_json(&c, &format!("{base}/gec/feedback"), &body).await;
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
}

// ─────────────────────────────────────────────────────────────────────────────
// anomaly 采纳回归：采纳不得关掉产生该结果的规则（词表文件 → store 回填）
// ─────────────────────────────────────────────────────────────────────────────

/// mock LLM（非流式）：返回固定的蒸馏模式 JSON（anomaly 蒸馏回归测试用）
async fn start_distill_mock() -> String {
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(|body: axum::extract::Json<serde_json::Value>| async move {
            let _ = body;
            axum::Json(serde_json::json!({
                "choices": [{
                    "message": {
                        "content": "{\"pattern\": \"^需要我再微调\", \"strong\": true, \"note\": \"服务性问句（回归测试）\"}"
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
    format!("http://{addr}/v1")
}

/// 构建 anomaly 采纳回归测试 server（调用方须持 env 锁）：
/// - `lexicons`：词表临时副本目录（采纳 sync 写回临时文件，不动仓库文件）；
/// - `db_file`：预置过规则的库（播种只在库空时执行 → anomaly 规则不在 store，
///   模拟「建库早于规则入文件」）；
/// - LLM 指向 mock 端点（返回固定蒸馏模式），提示词模板用仓库 `prompts/`。
async fn build_server_anomaly(
    db_file: &std::path::Path,
    lexicons: &std::path::Path,
    llm_base_url: &str,
) -> String {
    let tables_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../decision-tables");
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    unsafe {
        std::env::set_var(
            "GEC_SETTINGS_FILE",
            std::env::temp_dir()
                .join(format!(
                    "langfixer_fb_an_{}.json",
                    uuid::Uuid::new_v4().simple()
                ))
                .display()
                .to_string(),
        );
        std::env::set_var(
            "GEC_L3_DATA_DIR",
            std::env::temp_dir()
                .join(format!(
                    "langfixer_fb_an_l3_{}",
                    uuid::Uuid::new_v4().simple()
                ))
                .display()
                .to_string(),
        );
        std::env::set_var("LLM_BASE_URL", llm_base_url);
        std::env::set_var("LLM_API_KEY", "test-key");
        std::env::set_var("LLM_MODEL", "test-model");
        std::env::set_var(
            "LLM_PROMPTS_DIR",
            repo.join("prompts").display().to_string(),
        );
        std::env::set_var(
            "GEC_ANOMALY_GOLD",
            repo.join("datasets/anomaly_gold.json")
                .display()
                .to_string(),
        );
        std::env::remove_var("GEC_ENGINE_ZH");
        std::env::remove_var("GEC_ENGINE_EN");
        std::env::remove_var("GEC_ENGINE_JA");
        std::env::remove_var("GEC_STAT_ENDPOINT");
        std::env::remove_var("GEC_OFFLINE");
        std::env::set_var("HANLP_ENDPOINT", "http://127.0.0.1:8000");
        std::env::remove_var("GEC_TABLES_DIR");
        std::env::set_var("GEC_LEXICONS_DIR", lexicons.display().to_string());
        std::env::remove_var("GEC_TABLES_UPDATE_TOKEN");
        std::env::remove_var("CORS_ORIGINS");
    }
    let state = Arc::new(AppState::from_env(Some(&tables_dir), Some(db_file)));
    let app = routes::router(state).layer(middleware::cors::cors_layer());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, app).into_future());
    format!("http://{addr}")
}

/// 回归：采纳 anomaly 结果不得关掉产生该结果的规则。
///
/// 场景：`ZH_ANOMALY_ASSISTANT_REPLY` 是词表文件里的种子规则（`enabled: true`），
/// 但 DB 建库早于规则入文件（播种只在库空时执行 → 规则不在 store）——旧路径
/// `upsert_example` 自动创建「默认停用」影子定义，`sync_rules_to_file` 把停用状态
/// 写回文件，规则就此关闭。
/// 修复后：upsert 前按文件回填定义（保留文件 enabled / category / priority）→
/// 采纳后规则保持启用、文件不被覆盖，且采纳写入反馈日志留痕。
#[tokio::test]
async fn adopt_anomaly_keeps_file_seeded_rule_enabled() {
    let _g = env_guard();
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    // ① 词表复制到临时目录（采纳 sync 写回临时文件，不动仓库文件）
    let lexicons = std::env::temp_dir().join(format!(
        "langfixer_fb_an_lex_{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&lexicons).unwrap();
    for entry in std::fs::read_dir(repo.join("lexicons")).unwrap() {
        let entry = entry.unwrap();
        if entry.path().is_file() {
            std::fs::copy(
                entry.path(),
                lexicons.join(entry.path().file_name().unwrap()),
            )
            .unwrap();
        }
    }
    // ② DB 预置一条占位规则 → 播种被跳过 → anomaly 规则不在 store
    let db = std::env::temp_dir().join(format!(
        "langfixer_fb_an_{}.db",
        uuid::Uuid::new_v4().simple()
    ));
    {
        let store = lexicon_store::LexiconStore::open(&db, None).unwrap();
        store
            .upsert_example("zh", "ZH_TEST_PREEXISTING", "占位词", "占位正词", "manual")
            .unwrap();
    }
    // ③ mock LLM（固定蒸馏模式）+ 启动 server
    let mock = start_distill_mock().await;
    let base = build_server_anomaly(&db, &lexicons, &mock).await;
    let c = client();
    let original = "需要我再微调部分语句，让整体文风更贴合悬疑灵异的氛围吗?";
    let body = serde_json::json!({
        "lang": "zh",
        "error": error_json("zh", "anomaly", original, None, Some("rule"), Some("ZH_ANOMALY_ASSISTANT_REPLY")),
        "action": "adopt",
    });
    let (status, res) = post_json(&c, &format!("{base}/gec/feedback"), &body).await;
    assert_eq!(status, reqwest::StatusCode::OK, "{}", res);
    // ④ 蒸馏过门槛：模式沉淀 + 规则保持启用
    assert_eq!(res["via"], "anomaly", "{}", res);
    assert_eq!(res["anomaly"]["accepted"], true, "{}", res);
    // ⑤ store：规则定义按文件回填（enabled / category / priority 不被影子定义冲掉）
    let store = lexicon_store::LexiconStore::open(&db, None).unwrap();
    let def = store
        .get_rule("zh", "ZH_ANOMALY_ASSISTANT_REPLY")
        .unwrap()
        .expect("规则应在 store");
    assert!(def.enabled, "采纳不得关掉该规则: {:?}", def);
    assert_eq!(def.category, "anomaly");
    assert_eq!(def.priority, 20);
    // ⑥ 文件：enabled / category 不被 sync 覆盖；新模式已追加
    let v: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(lexicons.join("zh_gec_lexicon.json")).unwrap(),
    )
    .unwrap();
    let rule = v["rules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "ZH_ANOMALY_ASSISTANT_REPLY")
        .unwrap();
    assert_eq!(rule["enabled"], true, "文件 enabled 状态不得被 sync 覆盖");
    assert_eq!(rule["category"], "anomaly");
    let patterns: Vec<String> = rule["patterns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["pattern"].as_str().unwrap().to_string())
        .collect();
    assert!(
        patterns.contains(&"^需要我再微调".to_string()),
        "新模式应已追加: {patterns:?}"
    );
    // ⑦ 采纳留痕：反馈历史应含该规则 1 条 adopt
    let hist = c
        .get(format!("{base}/gec/feedback/history?lang=zh&limit=20"))
        .send()
        .await
        .unwrap();
    let hist: serde_json::Value = serde_json::from_str(&hist.text().await.unwrap()).unwrap();
    assert_eq!(hist["recent"][0]["action"], "adopt");
    assert_eq!(hist["recent"][0]["rule_id"], "ZH_ANOMALY_ASSISTANT_REPLY");
    let stat = hist["stats"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["rule_id"] == "ZH_ANOMALY_ASSISTANT_REPLY");
    assert!(stat.is_some(), "历史统计应含该规则: {}", hist);
    assert_eq!(stat.unwrap()["adopted"], 1);
    let _ = std::fs::remove_dir_all(&lexicons);
    let _ = std::fs::remove_file(&db);
}
