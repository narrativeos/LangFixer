//! 训练中心集成测试（`/gec/train/*`）
//!
//! 覆盖 P1 → P2 闭环：反思建议队列 → `POST /gec/train/suggestion/:id/run`
//! 分派训练动作（L3 重训练 / 词表条件化 / LLM 训练数据）→ 作业记录 +
//! 建议 `consumed`；以及独立作业（领域池 / LLM 数据 / 作业列表）。
//! 外部服务（feature-model / 测评）指向不可达地址——失败路径优雅降级。

use std::sync::{Arc, Mutex};

use api_server::{middleware, routes, state::AppState};

/// 环境变量互斥锁（与 tests/reflection_api.rs 相同的并行隔离约定）
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn env_guard() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// 临时词表目录（真实 lexicons 的副本——条件化入库不污染仓库文件）
fn lexicons_copy() -> std::path::PathBuf {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../lexicons");
    let dst = std::env::temp_dir().join(format!(
        "langfixer_train_lex_{}",
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
async fn build_server(db_file: Option<&std::path::Path>, lex_dir: &std::path::Path) -> String {
    let tables_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../decision-tables");
    unsafe {
        std::env::set_var(
            "GEC_SETTINGS_FILE",
            std::env::temp_dir()
                .join(format!(
                    "langfixer_train_{}.json",
                    uuid::Uuid::new_v4().simple()
                ))
                .display()
                .to_string(),
        );
        std::env::set_var(
            "GEC_L3_DATA_DIR",
            std::env::temp_dir()
                .join(format!(
                    "langfixer_train_l3_{}",
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
        // 外部服务不可达 → 优雅降级（L3 重训练 / 测评消融）
        std::env::set_var("GEC_EVAL_ENDPOINT", "http://127.0.0.1:1");
        std::env::set_var("GEC_L3_ENDPOINT", "http://127.0.0.1:1");
    }
    let db_file = db_file.map(std::path::Path::to_path_buf).or_else(|| {
        Some(std::env::temp_dir().join(format!(
            "langfixer_train_{}.db",
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
/// 种子反馈：模型层 3 拒（→ l3_sample 建议）+ 规则 1 拒（→ lexicon_condition）
/// + LLM 3 拒（→ llm_arm）+ LLM 1 采纳带修正（→ LLM 训练数据）
async fn seed_feedback(c: &reqwest::Client, base: &str) {
    for i in 0..3 {
        let body = serde_json::json!({
            "lang": "zh",
            "error": error_json("zh", "grammar", &format!("模型误报{i}"), Some("feature"), None, None),
            "action": "reject",
        });
        let (status, res) = post_json(c, &format!("{base}/gec/feedback"), &body).await;
        assert_eq!(status, reqwest::StatusCode::OK, "feature: {res}");
    }
    let body = serde_json::json!({
        "lang": "zh",
        "error": error_json("zh", "grammar", "模型确认句", Some("feature"), None, Some("模型确认修正")),
        "action": "adopt",
    });
    let (status, res) = post_json(c, &format!("{base}/gec/feedback"), &body).await;
    assert_eq!(status, reqwest::StatusCode::OK, "feature adopt: {res}");
    let body = serde_json::json!({
        "lang": "zh",
        "error": error_json("zh", "spelling", "他時常迟到", None, Some("ZH_CONFUSABLE"), None),
        "action": "reject",
    });
    let (status, res) = post_json(c, &format!("{base}/gec/feedback"), &body).await;
    assert_eq!(status, reqwest::StatusCode::OK, "rule: {res}");
    for i in 0..3 {
        let body = serde_json::json!({
            "lang": "zh",
            "error": error_json("zh", "grammar", &format!("LLM误报{i}"), Some("llm"), None, None),
            "action": "reject",
        });
        let (status, res) = post_json(c, &format!("{base}/gec/feedback"), &body).await;
        assert_eq!(status, reqwest::StatusCode::OK, "llm: {res}");
    }
    let body = serde_json::json!({
        "lang": "zh",
        "error": error_json("zh", "grammar", "LLM采纳句", Some("llm"), None, Some("LLM采纳修正")),
        "action": "adopt",
    });
    let (status, res) = post_json(c, &format!("{base}/gec/feedback"), &body).await;
    assert_eq!(status, reqwest::StatusCode::OK, "llm adopt: {res}");
}

/// 生成反思报告 → 返回（建议列表, 建议 id 列表）
async fn run_reflect(c: &reqwest::Client, base: &str) -> (Vec<serde_json::Value>, Vec<i64>) {
    let (status, res) = post_json(
        c,
        &format!("{base}/gec/reflect/run"),
        &serde_json::json!({ "lang": "zh", "include_eval": false }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{res}");
    let suggestions = res["report"]["suggestions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let ids = res["suggestion_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_i64().unwrap())
        .collect();
    (suggestions, ids)
}

/// 建议 → 训练作业：l3_sample（失败路径：服务不可达）+ lexicon_condition（入库）
/// + llm_arm（数据构建）→ 作业记录 + 建议 consumed
#[tokio::test]
async fn suggestion_run_dispatches_and_consumes() {
    let _g = env_guard();
    let lex_dir = lexicons_copy();
    let base = build_server(None, &lex_dir).await;
    let c = client();
    seed_feedback(&c, &base).await;
    let (suggestions, ids) = run_reflect(&c, &base).await;
    assert!(!suggestions.is_empty(), "应有建议产生");

    // ── l3_sample → L3 重训练（feature-model 不可达 → 502 + 作业 failed + 建议保持 pending）
    let l3_idx = suggestions
        .iter()
        .position(|s| s["kind"] == "l3_sample")
        .expect("应有 l3_sample 建议");
    let l3_id = ids[l3_idx];
    let (status, res) = post_json(
        &c,
        &format!("{base}/gec/train/suggestion/{l3_id}/run"),
        &serde_json::json!({}),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::BAD_GATEWAY, "{res}");
    let (status, res) = get_json(&c, &format!("{base}/gec/train/job/1")).await;
    assert_eq!(status, reqwest::StatusCode::OK, "{res}");
    assert_eq!(res["job"]["kind"], "l3_retrain");
    assert_eq!(res["job"]["status"], "failed");
    assert!(res["job"]["result"]["error"].is_string());
    // 建议保持 pending（失败不消费）
    let (status, res) = get_json(
        &c,
        &format!("{base}/gec/reflect/suggestions?status=pending"),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert!(
        res["suggestions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"].as_i64() == Some(l3_id)),
        "失败的 l3_sample 建议应保持 pending"
    );

    // ── lexicon_condition → 词表条件化入库（临时词表副本）
    let lex_idx = suggestions
        .iter()
        .position(|s| s["kind"] == "lexicon_condition")
        .expect("应有 lexicon_condition 建议");
    let lex_id = ids[lex_idx];
    let (status, res) = post_json(
        &c,
        &format!("{base}/gec/train/suggestion/{lex_id}/run"),
        &serde_json::json!({}),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{res}");
    assert_eq!(res["job_kind"], "lexicon_condition");
    assert_eq!(res["suggestion_consumed"], true);
    assert_eq!(res["result"]["applied"][0]["rule_id"], "ZH_CONFUSABLE");
    // 词表文件确实被条件化
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
            .any(|w| w == "他時常迟到"),
        "排除条目应已入库"
    );
    // 建议 → consumed
    let (status, res) = get_json(
        &c,
        &format!("{base}/gec/reflect/suggestions?status=consumed"),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert!(
        res["suggestions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"].as_i64() == Some(lex_id)),
        "lexicon_condition 建议应 consumed"
    );

    // ── llm_arm → LLM 训练数据（1 条 adopt 带修正）
    let llm_idx = suggestions
        .iter()
        .position(|s| s["kind"] == "llm_arm")
        .expect("应有 llm_arm 建议");
    let llm_id = ids[llm_idx];
    let (status, res) = post_json(
        &c,
        &format!("{base}/gec/train/suggestion/{llm_id}/run"),
        &serde_json::json!({}),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{res}");
    assert_eq!(res["job_kind"], "llm_data");
    assert_eq!(res["suggestion_consumed"], true);
    assert_eq!(res["result"]["n_samples"], 1);
    assert!(
        res["result"]["jsonl"]
            .as_str()
            .unwrap()
            .contains("LLM采纳句")
    );

    // ── 作业列表 / 详情
    let (status, res) = get_json(&c, &format!("{base}/gec/train/jobs")).await;
    assert_eq!(status, reqwest::StatusCode::OK, "{res}");
    let jobs = res["jobs"].as_array().unwrap();
    assert_eq!(jobs.len(), 3);
    let kinds: std::collections::HashSet<&str> =
        jobs.iter().map(|j| j["kind"].as_str().unwrap()).collect();
    assert!(kinds.contains("l3_retrain"));
    assert!(kinds.contains("lexicon_condition"));
    assert!(kinds.contains("llm_data"));
    let (_status, res) = get_json(&c, &format!("{base}/gec/train/jobs?status=completed")).await;
    assert_eq!(res["jobs"].as_array().unwrap().len(), 2);
    let (_status, res) = get_json(&c, &format!("{base}/gec/train/jobs?status=failed")).await;
    assert_eq!(res["jobs"].as_array().unwrap().len(), 1);
}

/// 领域池构建 + LLM 数据独立作业
#[tokio::test]
async fn pool_and_llm_data_jobs() {
    let _g = env_guard();
    let lex_dir = lexicons_copy();
    let base = build_server(None, &lex_dir).await;
    let c = client();
    seed_feedback(&c, &base).await;

    // ── 领域池：模型层 3 拒（clean）+ 1 采纳（error）+ 飞轮 1（llm adopt 自动汇入）+ 增强 2
    //     （句级去重：反馈样本与飞轮样本同句只留首个）
    let (status, res) = post_json(
        &c,
        &format!("{base}/gec/train/pool"),
        &serde_json::json!({
            "lang": "zh",
            "clean_sentences": ["增强句A", "增强句B"],
        }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{res}");
    let pool = &res["pool"];
    assert_eq!(pool["total"], 7);
    assert_eq!(pool["error_count"], 2);
    assert_eq!(pool["clean_count"], 5);
    let sources: std::collections::HashSet<&str> = pool["by_source"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p[0].as_str().unwrap())
        .collect();
    assert!(sources.contains("feedback_reject"));
    assert!(sources.contains("feedback_adopt"));
    assert!(sources.contains("flywheel"));
    assert!(sources.contains("augment"));
    assert!(res["job_id"].as_i64().unwrap_or(0) > 0);

    // ── LLM 训练数据
    let (status, res) = post_json(
        &c,
        &format!("{base}/gec/train/llm-data"),
        &serde_json::json!({ "lang": "zh", "arm": "cec3" }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{res}");
    assert_eq!(res["data"]["samples"][0]["sentence"], "LLM采纳句");
    assert_eq!(res["data"]["samples"][0]["correction"], "LLM采纳修正");
    assert_eq!(res["data"]["arm"], "cec3");

    // ── L3 重训练独立入口（不可达 → 502）
    let (status, res) = post_json(
        &c,
        &format!("{base}/gec/train/l3/retrain"),
        &serde_json::json!({ "lang": "zh" }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::BAD_GATEWAY, "{res}");

    // 作业列表：2 个 completed + 1 个 failed
    let (status, res) = get_json(&c, &format!("{base}/gec/train/jobs?limit=200")).await;
    assert_eq!(status, reqwest::StatusCode::OK, "{res}");
    assert_eq!(res["jobs"].as_array().unwrap().len(), 3);
    let (_status, res) = get_json(&c, &format!("{base}/gec/train/jobs?status=failed")).await;
    assert_eq!(res["jobs"].as_array().unwrap().len(), 1);
    assert_eq!(res["jobs"][0]["kind"], "l3_retrain");
}

/// rule_tune 建议不是训练作业 → 400
#[tokio::test]
async fn rule_tune_suggestion_is_rejected() {
    let _g = env_guard();
    let lex_dir = lexicons_copy();
    let base = build_server(None, &lex_dir).await;
    let c = client();
    // 前置：ZH_CONFUSABLE 在仓库词表中默认已停用，而反思中心对「已停用规则」
    // 不再产出 rule_tune 建议（反馈是历史数据，建议无意义）——本测试验证的是
    // 「rule_tune 不是训练作业 → 400」，故先把规则置为启用（临时词表副本，不污染仓库）
    let resp = c
        .put(format!(
            "{base}/gec/rules/ZH_CONFUSABLE/toggle?enabled=true"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    // 规则 3 拒 → rule_tune 建议
    for i in 0..3 {
        let body = serde_json::json!({
            "lang": "zh",
            "error": error_json("zh", "spelling", &format!("规则误报{i}"), None, Some("ZH_CONFUSABLE"), None),
            "action": "reject",
        });
        let (status, res) = post_json(&c, &format!("{base}/gec/feedback"), &body).await;
        assert_eq!(status, reqwest::StatusCode::OK, "{res}");
    }
    let (suggestions, ids) = run_reflect(&c, &base).await;
    let idx = suggestions
        .iter()
        .position(|s| s["kind"] == "rule_tune")
        .expect("应有 rule_tune 建议");
    let (status, res) = post_json(
        &c,
        &format!("{base}/gec/train/suggestion/{}/run", ids[idx]),
        &serde_json::json!({}),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST, "{res}");
}
