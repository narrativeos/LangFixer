//! 知识源资源包 API 端到端（盘点 / 导出 / 导入 / 删除）：
//! - 盘点组成（点数 / 规则行数 / aux 存在性）
//! - 导出包结构完整（format/checksum/知识组件/决策表组件/aux 组件）
//! - 删除资源包 → 知识库 + 决策表规则行 + aux 文件 + 归因行为联动
//! - 导入资源包 → 完整恢复（幂等）；篡改包拒收
//!
//! 注意：clean/import 会写回决策表目录——必须用临时副本，避免污染仓库资产。

use std::sync::{Arc, Mutex};

use api_server::{middleware, routes, state::AppState};
use attribution_engine::dataset::ResourcePack;
use shared_model::{AttributionContext, AttributionResult};

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn env_guard() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn copy_dir_tree(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        let dest = to.join(path.file_name().unwrap());
        if path.is_dir() {
            copy_dir_tree(&path, &dest);
        } else {
            std::fs::copy(&path, &dest).unwrap();
        }
    }
}

/// 建 server：知识库与决策表目录均用临时副本（资源包操作会写回两处）。
/// 返回（base_url, 临时知识库目录, 临时决策表目录）
async fn build_server() -> (String, std::path::PathBuf, std::path::PathBuf) {
    let _guard = env_guard();
    let root = std::env::temp_dir().join(format!(
        "langfixer_packtest_{}",
        uuid::Uuid::new_v4().simple()
    ));
    let kb_dir = root.join("knowledge-base");
    let tables_dir = root.join("decision-tables");
    copy_dir_tree(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../knowledge-base"),
        &kb_dir,
    );
    copy_dir_tree(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../decision-tables"),
        &tables_dir,
    );
    unsafe {
        std::env::set_var("GEC_KNOWLEDGE_DIR", kb_dir.display().to_string());
        std::env::set_var(
            "GEC_SETTINGS_FILE",
            root.join("settings.json").display().to_string(),
        );
        std::env::remove_var("GEC_TABLES_UPDATE_TOKEN");
    }
    let db_file = root.join("langfixer.db");
    let state = Arc::new(AppState::from_env(Some(&tables_dir), Some(&db_file)));
    let app = routes::router(state).layer(middleware::cors::cors_layer());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, app).into_future());
    (format!("http://{addr}"), kb_dir, tables_dir)
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

fn ctx_article() -> AttributionContext {
    AttributionContext {
        lang: "en".into(),
        error_type: "article".into(),
        confidence: 0.9,
        session_error_count: 0,
        session_meta: None,
        grammar_point: None,
    }
}

async fn matched_rule(base: &str) -> String {
    let res: AttributionResult = client()
        .post(format!("{base}/gec/attribute"))
        .json(&ctx_article())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    res.trace.matched_rule
}

async fn summary(base: &str, source_id: &str) -> serde_json::Value {
    client()
        .get(format!("{base}/gec/knowledge/dataset/{source_id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

/// 盘点派生期望值（源内容变动时测试随盘点自适应，不硬编码资产数字）
fn total_points(s: &serde_json::Value) -> u64 {
    s["total_points"].as_u64().unwrap()
}
fn exclusive_points(s: &serde_json::Value) -> u64 {
    s["langs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["exclusive_points"].as_u64().unwrap())
        .sum()
}
fn tagged_rules(s: &serde_json::Value) -> u64 {
    s["rules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["tagged_rules"].as_u64().unwrap())
        .sum()
}

#[tokio::test]
async fn dataset_summary_and_export_pack() {
    let (base, _kb, _tables) = build_server().await;
    let summary: serde_json::Value = client()
        .get(format!("{base}/gec/knowledge/dataset/src-sample-en"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(summary["source"]["source_id"], "src-sample-en");
    assert!(
        summary["total_points"].as_u64().unwrap() > 200,
        "示例源知识点应 > 200"
    );
    assert!(tagged_rules(&summary) > 0, "示例源规则行已按 _meta 溯源");
    assert!(
        summary["rules"][0]["total_rules"].as_u64().unwrap() > tagged_rules(&summary),
        "除示例源标记行外还应含通用兜底行"
    );
    assert_eq!(summary["aux_points_present"], true);
    assert_eq!(summary["aux_index_present"], true);

    let pack: ResourcePack = client()
        .post(format!("{base}/gec/knowledge/dataset/src-sample-en/export"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(pack.format, "langfixer-resource-pack");
    assert_eq!(pack.format_version, 1);
    assert!(!pack.checksum.is_empty());
    assert_eq!(
        pack.components.knowledge_points.len() as u64,
        total_points(&summary)
    );
    let dts = &pack.components.decision_tables;
    assert!(!dts.is_empty(), "应含决策表快照");
    assert_eq!(dts[0].lang, "en");
    assert!(pack.components.aux_points.is_some() && pack.components.aux_index.is_some());
}

#[tokio::test]
async fn dataset_clean_then_import_roundtrip() {
    let (base, kb, tables_dir) = build_server().await;
    // 盘点派生期望值（不硬编码源资产数字）
    let s0 = summary(&base, "src-sample-en").await;
    // 导出完整资源包
    let pack: serde_json::Value = client()
        .post(format!("{base}/gec/knowledge/dataset/src-sample-en/export"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    // 删除前：article 归因命中示例源规则行
    assert_eq!(matched_rule(&base).await, "article_novice");

    // 删除资源包
    let resp: serde_json::Value = client()
        .post(format!("{base}/gec/knowledge/dataset/src-sample-en/clean"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(resp["knowledge"]["source_removed"], true);
    assert_eq!(resp["knowledge"]["points_removed"], exclusive_points(&s0));
    assert_eq!(resp["rules_removed"], tagged_rules(&s0));
    assert_eq!(resp["aux_removed"], 2);

    // 删除后：盘点 404；归因回落到通用 default 行；aux 文件已删；表文件不再含该源标记
    let res = client()
        .get(format!("{base}/gec/knowledge/dataset/src-sample-en"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 404, "源已不存在");
    assert_eq!(matched_rule(&base).await, "default_knowledge_gap");
    assert!(
        !tables_dir.join("en_points.json").exists(),
        "aux 文件应已删除"
    );
    let table_json = std::fs::read_to_string(tables_dir.join("en_gec_attribution.json")).unwrap();
    assert!(
        !table_json.contains("src-sample-en"),
        "表文件不应再含该源规则行"
    );
    let kb_json = std::fs::read_to_string(kb.join("points/en.json")).unwrap();
    assert_eq!(kb_json.trim(), "[]", "示例源知识点应已清空");

    // 导入资源包 → 完整恢复
    let resp: serde_json::Value = client()
        .post(format!("{base}/gec/knowledge/dataset/import"))
        .json(&pack)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(resp["knowledge"]["points_created"], total_points(&s0));
    assert_eq!(resp["rules_added"], tagged_rules(&s0));
    assert_eq!(resp["aux_written"], 2);

    // 恢复后：盘点/归因/文件全部回来
    let s1 = summary(&base, "src-sample-en").await;
    assert_eq!(total_points(&s1), total_points(&s0));
    assert_eq!(tagged_rules(&s1), tagged_rules(&s0));
    assert_eq!(matched_rule(&base).await, "article_novice");
    assert!(
        tables_dir.join("en_points.json").exists(),
        "aux 文件应已写回"
    );

    // 幂等：再导一次 → 全部 skipped，不重复造点
    let resp: serde_json::Value = client()
        .post(format!("{base}/gec/knowledge/dataset/import"))
        .json(&pack)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(resp["knowledge"]["points_skipped"], total_points(&s0));
    assert_eq!(resp["knowledge"]["points_created"], 0);
    let s2 = summary(&base, "src-sample-en").await;
    assert_eq!(total_points(&s2), total_points(&s0), "重复导入不重复造点");
}

#[tokio::test]
async fn dataset_export_zip_and_import_zip_bytes() {
    use attribution_engine::dataset;

    let (base, _kb, _tables) = build_server().await;
    // 盘点派生期望值（不硬编码源资产数字）
    let s0 = summary(&base, "src-sample-en").await;

    // 导出 zip 压缩包：application/zip 附件 + 明确文件名
    let resp = client()
        .post(format!(
            "{base}/gec/knowledge/dataset/src-sample-en/export?format=zip"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["content-type"], "application/zip");
    let disposition = resp.headers()["content-disposition"]
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        disposition.contains("attachment"),
        "应为附件下载: {disposition}"
    );
    assert!(disposition.contains("src-sample-en-resource-pack.zip"));
    let zip = resp.bytes().await.unwrap();
    assert!(dataset::is_zip_bytes(&zip), "应以 PK magic 开头");

    // zip 内容完整：解出资源包、校验和通过、组件齐备；且明显小于裸 JSON
    let pack = dataset::pack_from_bytes(&zip).expect("zip 应可解出资源包");
    dataset::validate_pack(&pack).unwrap();
    assert_eq!(
        pack.components.knowledge_points.len() as u64,
        total_points(&s0)
    );
    assert!(!pack.components.decision_tables.is_empty());
    assert!(pack.components.aux_points.is_some());
    let raw_json = serde_json::to_vec(&pack).unwrap();
    assert!(
        zip.len() * 3 < raw_json.len(),
        "zip 压缩应显著：zip {} B vs JSON {} B",
        zip.len(),
        raw_json.len()
    );

    // 删除资源包后，把 zip 字节原样上传导入（application/zip）→ 完整恢复
    client()
        .post(format!("{base}/gec/knowledge/dataset/src-sample-en/clean"))
        .send()
        .await
        .unwrap();
    let resp: serde_json::Value = client()
        .post(format!("{base}/gec/knowledge/dataset/import"))
        .header(reqwest::header::CONTENT_TYPE, "application/zip")
        .body(zip.clone())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(resp["knowledge"]["points_created"], total_points(&s0));
    assert_eq!(resp["rules_added"], tagged_rules(&s0));
    assert_eq!(resp["aux_written"], 2);

    // 幂等：zip 再导一次 → 全部 skipped
    let resp: serde_json::Value = client()
        .post(format!("{base}/gec/knowledge/dataset/import"))
        .header(reqwest::header::CONTENT_TYPE, "application/zip")
        .body(zip.clone())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(resp["knowledge"]["points_skipped"], total_points(&s0));
    assert_eq!(resp["knowledge"]["points_created"], 0);

    // 垃圾字节 → 400 拒收
    let res = client()
        .post(format!("{base}/gec/knowledge/dataset/import"))
        .header(reqwest::header::CONTENT_TYPE, "application/zip")
        .body("这不是资源包")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400, "无法解析的请求体应拒收");
}

#[tokio::test]
async fn dataset_import_rejects_tampered_pack() {
    let (base, _kb, _tables) = build_server().await;
    let mut pack: serde_json::Value = client()
        .post(format!("{base}/gec/knowledge/dataset/src-sample-en/export"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    pack["components"]["knowledge_points"][0]["title"] = serde_json::json!("篡改标题");
    let res = client()
        .post(format!("{base}/gec/knowledge/dataset/import"))
        .json(&pack)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400, "校验和不符应拒收");
    // 非资源包 JSON → 4xx 拒收（axum JSON 提取失败 422 / 校验失败 400）
    let res = client()
        .post(format!("{base}/gec/knowledge/dataset/import"))
        .json(&serde_json::json!({"foo": "bar"}))
        .send()
        .await
        .unwrap();
    assert!(res.status().is_client_error(), "非资源包 JSON 应拒收");
}
