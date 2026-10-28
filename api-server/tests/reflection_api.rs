//! 反思中心集成测试（`/gec/reflect/*`）
//!
//! 覆盖：反馈聚合 → 反思报告（findings + 建议队列）→ 报告历史 / 详情 →
//! 建议队列状态流转（P2 消费入口）；测评服务不可达时优雅降级（不阻断）。

use std::sync::{Arc, Mutex};

use api_server::{middleware, routes, state::AppState};

/// 环境变量互斥锁（与 tests/feedback_api.rs 相同的并行隔离约定）
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn env_guard() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// 词表目录：**出厂词表的临时副本**（每次调用一份）
///
/// 反思中心的采纳动作会走规则开关回写（`sync_rules_to_file`），
/// 直接指向仓库 `lexicons/` 会让测试改到出厂默认（实测见 `api.rs` 同名函数注释）。
fn lexicons_abs() -> std::path::PathBuf {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../lexicons");
    let tmp = std::env::temp_dir().join(format!(
        "langfixer_rf_lex_{}",
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

/// 构建 server（调用方须持 env 锁）；测评服务指向不可达地址（hermetic）
async fn build_server(db_file: Option<&std::path::Path>) -> String {
    let tables_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../decision-tables");
    unsafe {
        std::env::set_var(
            "GEC_SETTINGS_FILE",
            std::env::temp_dir()
                .join(format!(
                    "langfixer_refl_{}.json",
                    uuid::Uuid::new_v4().simple()
                ))
                .display()
                .to_string(),
        );
        std::env::set_var(
            "GEC_L3_DATA_DIR",
            std::env::temp_dir()
                .join(format!(
                    "langfixer_refl_l3_{}",
                    uuid::Uuid::new_v4().simple()
                ))
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
        // 测评服务不可达 → 反思中心优雅降级（纯反馈数据）
        std::env::set_var("GEC_EVAL_ENDPOINT", "http://127.0.0.1:1");
    }
    let db_file = db_file.map(std::path::Path::to_path_buf).or_else(|| {
        Some(std::env::temp_dir().join(format!(
            "langfixer_refl_{}.db",
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

fn client() -> reqwest::Client {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(reqwest::header::CONNECTION, "close".parse().unwrap());
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .default_headers(headers)
        .build()
        .unwrap()
}

fn error_json(
    lang: &str,
    error_type: &str,
    original: &str,
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

async fn get_json(c: &reqwest::Client, url: &str) -> (reqwest::StatusCode, serde_json::Value) {
    let resp = c.get(url).send().await.unwrap();
    let status = resp.status();
    let text = resp.text().await.unwrap();
    (
        status,
        serde_json::from_str(&text).unwrap_or(serde_json::json!({ "raw": text })),
    )
}

async fn put_json(
    c: &reqwest::Client,
    url: &str,
    body: &serde_json::Value,
) -> (reqwest::StatusCode, serde_json::Value) {
    let resp = c.put(url).json(body).send().await.unwrap();
    let status = resp.status();
    let text = resp.text().await.unwrap();
    (
        status,
        serde_json::from_str(&text).unwrap_or(serde_json::json!({ "raw": text })),
    )
}

/// 种子反馈：规则 3 拒 1 采纳 + 模型层 3 拒 + LLM 3 拒
async fn seed_feedback(c: &reqwest::Client, base: &str) {
    for (i, action) in ["adopt", "reject", "reject", "reject"].iter().enumerate() {
        let body = serde_json::json!({
            "lang": "zh",
            "error": error_json("zh", "spelling", &format!("误词{i}"), None, Some("ZH_HOT")),
            "action": action,
        });
        let (status, res) = post_json(c, &format!("{base}/gec/feedback"), &body).await;
        assert_eq!(status, reqwest::StatusCode::OK, "{action}: {res}");
    }
    for i in 0..3 {
        let body = serde_json::json!({
            "lang": "zh",
            "error": error_json("zh", "grammar", &format!("模型误报{i}"), Some("feature"), None),
            "action": "reject",
        });
        let (status, res) = post_json(c, &format!("{base}/gec/feedback"), &body).await;
        assert_eq!(status, reqwest::StatusCode::OK, "feature: {res}");
    }
    for i in 0..3 {
        let body = serde_json::json!({
            "lang": "zh",
            "error": error_json("zh", "grammar", &format!("LLM误报{i}"), Some("llm"), None),
            "action": "reject",
        });
        let (status, res) = post_json(c, &format!("{base}/gec/feedback"), &body).await;
        assert_eq!(status, reqwest::StatusCode::OK, "llm: {res}");
    }
}

#[tokio::test]
async fn reflect_run_aggregates_feedback_and_serves_queue() {
    let _g = env_guard();
    let base = build_server(None).await;
    let c = client();
    seed_feedback(&c, &base).await;

    // 生成反思报告（测评服务不可达 → 优雅降级）
    let (status, res) = post_json(
        &c,
        &format!("{base}/gec/reflect/run"),
        &serde_json::json!({ "lang": "zh", "include_eval": true }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{res}");
    let report = &res["report"];
    assert_eq!(report["lang"], "zh");
    assert!(report["feedback_total"].as_i64().unwrap() >= 10, "{report}");
    assert!(!report["findings"].as_array().unwrap().is_empty());
    let kinds: Vec<&str> = report["suggestions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"rule_tune"), "规则 3 拒 → 调参: {kinds:?}");
    assert!(
        kinds.contains(&"l3_sample"),
        "模型层 3 拒 → L3 样本: {kinds:?}"
    );
    assert!(kinds.contains(&"llm_arm"), "LLM 3 拒 → 换臂: {kinds:?}");
    // 测评降级说明
    assert!(
        report["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f.as_str().unwrap().contains("测评证据不可用")),
        "{:?}",
        report["findings"]
    );
    // 建议队列 id 与建议一一对应
    let n_sug = res["suggestion_ids"].as_array().unwrap().len();
    assert_eq!(n_sug, report["suggestions"].as_array().unwrap().len());

    // 报告历史
    let (status, hist) = get_json(&c, &format!("{base}/gec/reflect/reports?lang=zh")).await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(hist["reports"].as_array().unwrap().len(), 1);
    let report_id: i64 = hist["reports"][0]["id"].as_i64().unwrap();
    assert_eq!(
        hist["reports"][0]["n_suggestions"],
        report["suggestions"].as_array().unwrap().len() as i64
    );

    // 报告详情
    let (status, full) = get_json(&c, &format!("{base}/gec/reflect/report/{report_id}")).await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(full["report"]["lang"], "zh");
    assert!(full["report"]["suggestions"].as_array().is_some());

    // 建议队列（全部 pending）
    let (status, queue) = get_json(
        &c,
        &format!("{base}/gec/reflect/suggestions?status=pending&lang=zh"),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    let rows = queue["suggestions"].as_array().unwrap();
    assert_eq!(rows.len(), n_sug);
    assert!(rows.iter().all(|s| s["status"] == "pending"));

    // 状态流转（P2 消费入口）
    let sug_id: i64 = rows[0]["id"].as_i64().unwrap();
    let (status, up) = put_json(
        &c,
        &format!("{base}/gec/reflect/suggestion/{sug_id}"),
        &serde_json::json!({ "status": "consumed" }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{up}");
    let (status, consumed) = get_json(
        &c,
        &format!("{base}/gec/reflect/suggestions?status=consumed"),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(consumed["suggestions"][0]["id"], sug_id);
    assert!(consumed["suggestions"][0]["status_at"].is_string());

    // 非法状态 / 不存在 id
    let (status, _) = put_json(
        &c,
        &format!("{base}/gec/reflect/suggestion/{sug_id}"),
        &serde_json::json!({ "status": "bogus" }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
    let (status, _) = put_json(
        &c,
        &format!("{base}/gec/reflect/suggestion/99999"),
        &serde_json::json!({ "status": "dismissed" }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND);
}

/// 金集闭卷证据 → **具体漏报条目**变成可执行靶子，并能从建议队列读回（带 `gold_nos`）
#[tokio::test]
async fn reflect_run_with_gold_evidence_queues_targeted_suggestions() {
    let _g = env_guard();
    let base = build_server(None).await;
    let c = client();
    let gold = serde_json::json!({
        "source": "media_gold@test · 上限评测",
        "gold_total": 3,
        "counted_total": 2,
        "text_hits": 0,
        "positional_hits": 0,
        "misses": [
            {
                "no": 3,
                "original": "血压心率",
                "suggestion": "改为「血压、心率」",
                "location": "图片1 未定位",
                "verdict": "差错",
                "note": "【差错】并列指标未作分隔",
                "counted": true
            },
            {
                "no": 11,
                "original": "病房工作电话不咨询病情",
                "suggestion": "改为「…不可用于咨询病情」",
                "location": "图片2 未定位",
                "verdict": "差错",
                "note": "【差错】主谓搭配不当",
                "counted": true
            },
            {
                "no": 1,
                "original": "住院手续身份核查",
                "suggestion": "按栏目实际内容选定标题",
                "location": "图片1 未定位",
                "verdict": "表达优化",
                "note": "【表达优化】关系不明",
                "counted": false
            }
        ]
    });
    let (status, res) = post_json(
        &c,
        &format!("{base}/gec/reflect/run"),
        &serde_json::json!({ "lang": "zh", "include_eval": false, "gold": gold }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{res}");
    let report = &res["report"];
    let kinds: Vec<&str> = report["suggestions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["kind"].as_str().unwrap())
        .collect();
    assert!(
        kinds.contains(&"lexicon_condition")
            && kinds.contains(&"l3_sample")
            && kinds.contains(&"llm_arm"),
        "三类靶子都应出现（局部可形式化 / 结构性 / 编辑判断）：{kinds:?}"
    );
    // findings 必须点出未命中的计差错条目号（与评测同一把尺子，不是含糊的「召回偏低」）
    assert!(
        report["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f.as_str().unwrap_or_default().contains("#3 #11")),
        "{:?}",
        report["findings"]
    );
    // 队列落库可读回，payload 带 gold_nos（能回到专家表具体条目）
    let n_sug = res["suggestion_ids"].as_array().unwrap().len();
    let (status, queue) = get_json(
        &c,
        &format!("{base}/gec/reflect/suggestions?status=pending&lang=zh"),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    let rows = queue["suggestions"].as_array().unwrap();
    assert_eq!(rows.len(), n_sug);
    let gold_rows: Vec<&serde_json::Value> = rows
        .iter()
        .filter(|s| {
            s["payload"]["gold_nos"]
                .as_array()
                .map(|a| !a.is_empty())
                .unwrap_or(false)
        })
        .collect();
    assert!(
        gold_rows.len() >= 3,
        "金集来源的建议必须带 gold_nos：{}",
        serde_json::to_string(&rows).unwrap()
    );
    // 计差错漏报优先级最高（编辑判断不得抢位）
    let min_priority = rows
        .iter()
        .filter(|s| s["payload"]["gold_nos"].is_array())
        .map(|s| s["priority"].as_i64().unwrap())
        .min()
        .unwrap();
    assert_eq!(min_priority, 0, "{:?}", rows);
}

#[tokio::test]
async fn reflect_run_without_eval_and_empty_feedback() {
    let _g = env_guard();
    let base = build_server(None).await;
    let c = client();
    // 空反馈 + 不拉测评 → 引导性 findings，无建议
    let (status, res) = post_json(
        &c,
        &format!("{base}/gec/reflect/run"),
        &serde_json::json!({ "lang": "zh", "include_eval": false }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{res}");
    assert!(
        res["report"]["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f.as_str().unwrap().contains("暂无反馈积累"))
    );
    assert!(res["report"]["suggestions"].as_array().unwrap().is_empty());
    assert!(res["report"].get("eval_evidence").is_none());
}
