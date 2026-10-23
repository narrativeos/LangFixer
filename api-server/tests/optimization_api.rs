//! 优化中心集成测试（`/gec/eval/contributions` + `/gec/round*`）
//!
//! 覆盖 P3 闭环：自动评估（反馈增量 + 池规模 + 测评证据 → 阈值判定）→
//! 提案轮次 → 人工确认门（confirm / reject）→ 执行（派生 P2 训练作业）→
//! 轮次终态 + 增量窗口（评估后无新反馈 → 「无需行动」）。
//! 外部服务（feature-model / 测评）指向不可达地址——失败路径优雅降级。

use std::sync::{Arc, Mutex};

use api_server::{middleware, routes, state::AppState};

/// 环境变量互斥锁（与 tests/training_api.rs / reflection_api.rs 相同的并行隔离约定）
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn env_guard() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// 临时词表目录（真实 lexicons 的副本——条件化入库不污染仓库文件）
fn lexicons_copy() -> std::path::PathBuf {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../lexicons");
    let dst = std::env::temp_dir().join(format!(
        "langfixer_round_lex_{}",
        uuid::Uuid::new_v4().simple()
    ));
    copy_dir(&src, &dst).unwrap();
    dst
}

fn copy_dir(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let target = dst.join(entry.file_name());
        std::fs::copy(entry.path(), &target)?;
    }
    Ok(())
}

/// 构建 server（调用方须持 env 锁）；外部服务全部指向不可达地址（hermetic）
async fn build_server(lex_dir: &std::path::Path) -> String {
    let tables_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../decision-tables");
    unsafe {
        std::env::set_var(
            "GEC_SETTINGS_FILE",
            std::env::temp_dir()
                .join(format!(
                    "langfixer_round_{}.json",
                    uuid::Uuid::new_v4().simple()
                ))
                .display()
                .to_string(),
        );
        std::env::set_var(
            "GEC_L3_DATA_DIR",
            std::env::temp_dir()
                .join(format!(
                    "langfixer_round_l3_{}",
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
        std::env::set_var("GEC_LEXICONS_DIR", lex_dir.display().to_string());
        std::env::remove_var("GEC_TABLES_UPDATE_TOKEN");
        std::env::remove_var("CORS_ORIGINS");
        // 外部服务不可达 → 优雅降级（L3 重训练失败 / 测评证据缺失）
        std::env::set_var("GEC_EVAL_ENDPOINT", "http://127.0.0.1:1");
        std::env::set_var("GEC_L3_ENDPOINT", "http://127.0.0.1:1");
    }
    let db_file = std::env::temp_dir().join(format!(
        "langfixer_round_{}.db",
        uuid::Uuid::new_v4().simple()
    ));
    let state = Arc::new(AppState::from_env(Some(&tables_dir), Some(&db_file)));
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
    correction: Option<&str>,
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
    if let Some(c) = correction {
        e["correction"] = serde_json::json!(c);
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

/// 种子反馈（触发全部四类轮次动作）：
/// - 模型层 5 拒（feature）→ `l3_retrain`
/// - 规则 3 拒（ZH_CONFUSABLE，3 条不同文本）→ `lexicon_condition` + `rule_tune`
/// - LLM 10 采纳（带修正）→ `llm_data`
async fn seed_feedback(c: &reqwest::Client, base: &str) {
    for i in 0..5 {
        let body = serde_json::json!({
            "lang": "zh",
            "error": error_json("zh", "grammar", &format!("模型误报{i}"), Some("feature"), None, None),
            "action": "reject",
        });
        let (status, res) = post_json(c, &format!("{base}/gec/feedback"), &body).await;
        assert_eq!(status, reqwest::StatusCode::OK, "feature: {res}");
    }
    for i in 0..3 {
        let body = serde_json::json!({
            "lang": "zh",
            "error": error_json("zh", "spelling", &format!("规则误报句{i}"), None, Some("ZH_CONFUSABLE"), None),
            "action": "reject",
        });
        let (status, res) = post_json(c, &format!("{base}/gec/feedback"), &body).await;
        assert_eq!(status, reqwest::StatusCode::OK, "rule: {res}");
    }
    for i in 0..10 {
        let body = serde_json::json!({
            "lang": "zh",
            "error": error_json("zh", "grammar", &format!("LLM采纳句{i}"), Some("llm"), None, Some(&format!("LLM采纳修正{i}"))),
            "action": "adopt",
        });
        let (status, res) = post_json(c, &format!("{base}/gec/feedback"), &body).await;
        assert_eq!(status, reqwest::StatusCode::OK, "llm: {res}");
    }
}

/// 无反馈 → 自动评估「无需行动」（含各指标离阈值的差距）
#[tokio::test]
async fn evaluate_no_action_when_quiet() {
    let _g = env_guard();
    let lex_dir = lexicons_copy();
    let base = build_server(&lex_dir).await;
    let c = client();
    let (status, res) = post_json(
        &c,
        &format!("{base}/gec/round/evaluate"),
        &serde_json::json!({ "lang": "zh" }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{res}");
    assert_eq!(res["action"], "none");
    let reasons = res["reasons"].as_array().unwrap();
    assert!(
        reasons.iter().any(|r| r
            .as_str()
            .map(|s| s.contains("模型层拒绝 0 < 5"))
            .unwrap_or(false)),
        "{reasons:?}"
    );
    assert!(
        reasons.iter().any(|r| r
            .as_str()
            .map(|s| s.contains("LLM 采纳 0 < 10"))
            .unwrap_or(false)),
        "{reasons:?}"
    );
    // 列表为空
    let (status, res) = get_json(&c, &format!("{base}/gec/rounds")).await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert!(res["rounds"].as_array().unwrap().is_empty());
}

/// 完整生命周期：评估 → 提案（4 类动作）→ 确认 → 执行 →
/// 轮次 failed（L3 服务不可达）+ 各动作结果 + 派生作业 + 词表入库
/// → 增量窗口（再评估 → 无需行动）
#[tokio::test]
async fn round_propose_confirm_execute_lifecycle() {
    let _g = env_guard();
    let lex_dir = lexicons_copy();
    let base = build_server(&lex_dir).await;
    let c = client();
    seed_feedback(&c, &base).await;

    // ── 自动评估 → 提案（201）
    let (status, res) = post_json(
        &c,
        &format!("{base}/gec/round/evaluate"),
        &serde_json::json!({ "lang": "zh" }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::CREATED, "{res}");
    let round_id = res["round"]["id"].as_i64().unwrap();
    assert_eq!(res["round"]["status"], "proposed");
    let kinds: Vec<&str> = res["round"]["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"l3_retrain"), "{kinds:?}");
    assert!(kinds.contains(&"lexicon_condition"), "{kinds:?}");
    assert!(kinds.contains(&"llm_data"), "{kinds:?}");
    assert!(kinds.contains(&"rule_tune"), "{kinds:?}");
    assert!(!res["round"]["evidence"].as_array().unwrap().is_empty());

    // ── 未确认不能执行（人工门）
    let (status, res) = post_json(
        &c,
        &format!("{base}/gec/round/{round_id}/execute"),
        &serde_json::json!({}),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST, "{res}");

    // ── 确认 → 执行
    let (status, res) = post_json(
        &c,
        &format!("{base}/gec/round/{round_id}/confirm"),
        &serde_json::json!({}),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{res}");
    assert_eq!(res["round"]["status"], "confirmed");
    // 确认后不能走拒绝门
    let (status, res) = post_json(
        &c,
        &format!("{base}/gec/round/{round_id}/reject"),
        &serde_json::json!({}),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST, "{res}");

    let (status, res) = post_json(
        &c,
        &format!("{base}/gec/round/{round_id}/execute"),
        &serde_json::json!({}),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{res}");
    // L3 服务不可达 → 该动作 failed → 轮次 failed（其余动作成功）
    assert_eq!(res["round"]["status"], "failed", "{res}");
    let actions = res["round"]["result"]["actions"].as_array().unwrap();
    let by_kind = |k: &str| actions.iter().find(|a| a["kind"] == k).unwrap();
    assert_eq!(by_kind("l3_retrain")["status"], "failed");
    assert!(by_kind("l3_retrain")["error"].is_string());
    assert!(by_kind("l3_retrain")["job_id"].as_i64().unwrap() > 0);
    assert_eq!(by_kind("llm_data")["status"], "completed");
    assert_eq!(by_kind("llm_data")["job_id"].as_i64().unwrap() > 0, true);
    assert_eq!(by_kind("lexicon_condition")["status"], "completed");
    assert_eq!(by_kind("rule_tune")["status"], "human_required");

    // ── 派生的 P2 训练作业（3 个自动动作）
    let (status, res) = get_json(&c, &format!("{base}/gec/train/jobs?lang=zh")).await;
    assert_eq!(status, reqwest::StatusCode::OK, "{res}");
    let jobs = res["jobs"].as_array().unwrap();
    assert_eq!(jobs.len(), 3, "{jobs:?}");
    assert!(
        jobs.iter()
            .any(|j| j["kind"] == "l3_retrain" && j["status"] == "failed")
    );
    assert!(
        jobs.iter()
            .any(|j| j["kind"] == "llm_data" && j["status"] == "completed")
    );
    assert!(
        jobs.iter()
            .any(|j| j["kind"] == "lexicon_condition" && j["status"] == "completed")
    );

    // ── 词表条件化确实入库（临时副本）
    let file = std::fs::read_to_string(lex_dir.join("zh_gec_lexicon.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&file).unwrap();
    let rule = v["rules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "ZH_CONFUSABLE")
        .unwrap();
    assert!(
        rule["excludes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str() == Some("规则误报句0")),
        "排除条目应已入库"
    );

    // ── 增量窗口：轮次后无新反馈 → 再评估「无需行动」
    let (status, res) = post_json(
        &c,
        &format!("{base}/gec/round/evaluate"),
        &serde_json::json!({ "lang": "zh" }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{res}");
    assert_eq!(res["action"], "none", "增量窗口应排除已计反馈: {res}");

    // ── 详情 / 列表
    let (status, res) = get_json(&c, &format!("{base}/gec/round/{round_id}")).await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(res["round"]["id"], round_id);
    let (status, res) = get_json(&c, &format!("{base}/gec/rounds?lang=zh&status=failed")).await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(res["rounds"].as_array().unwrap().len(), 1);
    // ── 轮次报告（聚合：背景 + 各动作完整作业 + 执行后测评证据）
    let (status, res) = get_json(&c, &format!("{base}/gec/round/{round_id}/report")).await;
    assert_eq!(status, reqwest::StatusCode::OK, "{res}");
    assert_eq!(res["round"]["id"], round_id);
    assert_eq!(res["n_actions"], 4);
    assert_eq!(res["n_failed"], 1);
    let s = res["summary"].as_str().unwrap().to_string();
    assert!(s.contains("执行失败（1 个动作失败）"), "{s}");
    assert!(s.contains("L3 重训练失败"), "{s}");
    assert!(s.contains("LLM 训练数据完成"), "{s}");
    assert!(s.contains("词表条件化完成"), "{s}");
    assert!(s.contains("规则调参（人工操作"), "{s}");
    // 各动作附完整作业（含 result 细节）
    let r_actions = res["actions"].as_array().unwrap();
    let ra = |k: &str| r_actions.iter().find(|a| a["kind"] == k).unwrap();
    assert_eq!(
        ra("l3_retrain")["job"]["id"],
        by_kind("l3_retrain")["job_id"]
    );
    assert!(ra("l3_retrain")["job"]["result"]["error"].is_string());
    assert!(
        ra("llm_data")["job"]["result"]["n_samples"]
            .as_u64()
            .unwrap_or(0)
            > 0
    );
    assert!(
        ra("lexicon_condition")["job"]["result"]["applied"]
            .as_array()
            .is_some()
    );
    assert!(ra("rule_tune").get("job").is_none() || ra("rule_tune")["job"].is_null());
    // 背景 + 执行后测评证据（服务不可达 → null）
    assert!(!res["evidence"].as_array().unwrap().is_empty());
    assert!(res["eval_after"].is_null());
    // 不存在的轮次 → 404
    let (status, res) = get_json(&c, &format!("{base}/gec/round/99999/report")).await;
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND, "{res}");
}

/// 拒绝门：proposed → rejected；拒绝后不能确认 / 执行
#[tokio::test]
async fn round_reject_gate() {
    let _g = env_guard();
    let lex_dir = lexicons_copy();
    let base = build_server(&lex_dir).await;
    let c = client();
    // 模型层 5 拒 → l3_retrain 动作
    for i in 0..5 {
        let body = serde_json::json!({
            "lang": "zh",
            "error": error_json("zh", "grammar", &format!("拒绝门句{i}"), Some("feature"), None, None),
            "action": "reject",
        });
        let (status, _) = post_json(&c, &format!("{base}/gec/feedback"), &body).await;
        assert_eq!(status, reqwest::StatusCode::OK);
    }
    let (status, res) = post_json(
        &c,
        &format!("{base}/gec/round/evaluate"),
        &serde_json::json!({ "lang": "zh" }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::CREATED, "{res}");
    let round_id = res["round"]["id"].as_i64().unwrap();

    let (status, res) = post_json(
        &c,
        &format!("{base}/gec/round/{round_id}/reject"),
        &serde_json::json!({}),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{res}");
    assert_eq!(res["round"]["status"], "rejected");
    // 拒绝后不能确认 / 执行
    let (status, res) = post_json(
        &c,
        &format!("{base}/gec/round/{round_id}/confirm"),
        &serde_json::json!({}),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST, "{res}");
    let (status, res) = post_json(
        &c,
        &format!("{base}/gec/round/{round_id}/execute"),
        &serde_json::json!({}),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST, "{res}");
}

/// 贡献测量：测评服务不可达 → 503
#[tokio::test]
async fn contributions_service_down() {
    let _g = env_guard();
    let lex_dir = lexicons_copy();
    let base = build_server(&lex_dir).await;
    let c = client();
    let (status, res) = post_json(
        &c,
        &format!("{base}/gec/eval/contributions"),
        &serde_json::json!({}),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::SERVICE_UNAVAILABLE, "{res}");
    assert!(res["error"].as_str().unwrap().contains("测评服务"));
}
