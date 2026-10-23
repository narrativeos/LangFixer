//! 知识库 API 端到端：
//! - 知识源注册表 / 知识点列表（含 1:N 源过滤）
//! - 决策表 IP 回归守护（en 表不得含知识源内容 / grammarPoint）
//! - /gec/attribute 富化（表级归因 + 知识源引用）
//! - 热更新鉴权与校验

use std::sync::{Arc, Mutex};

use api_server::{middleware, routes, state::AppState};
use shared_model::{AttributionContext, AttributionResult, KnowledgeSource};

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn env_guard() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// 建 server：把仓库 knowledge-base/ 复制到临时目录并指向它（测试 CWD 为 api-server/）。
/// 热更新（PUT）会写回知识库目录——必须用临时副本，避免污染仓库资产。
/// 返回（base_url, 临时知识库目录）
async fn build_server() -> (String, std::path::PathBuf) {
    let _guard = env_guard();
    let kb_dir = std::env::temp_dir().join(format!(
        "langfixer_kbtest_kb_{}",
        uuid::Uuid::new_v4().simple()
    ));
    copy_dir_tree(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../knowledge-base"),
        &kb_dir,
    );
    unsafe {
        std::env::set_var("GEC_KNOWLEDGE_DIR", kb_dir.display().to_string());
        std::env::set_var(
            "GEC_SETTINGS_FILE",
            std::env::temp_dir()
                .join(format!(
                    "langfixer_kbtest_{}.json",
                    uuid::Uuid::new_v4().simple()
                ))
                .display()
                .to_string(),
        );
        std::env::remove_var("GEC_TABLES_UPDATE_TOKEN");
    }
    let tables_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../decision-tables");
    // 每个测试独立 SQLite 文件（与 api.rs 并行运行，避免共享默认 langfixer.db）
    let db_file = std::env::temp_dir().join(format!(
        "langfixer_kbtest_{}.db",
        uuid::Uuid::new_v4().simple()
    ));
    let state = Arc::new(AppState::from_env(Some(&tables_dir), Some(&db_file)));
    let app = routes::router(state).layer(middleware::cors::cors_layer());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, app).into_future());
    (format!("http://{addr}"), kb_dir)
}

/// 递归复制目录（测试用知识库临时副本）
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

fn client() -> reqwest::Client {
    // 默认带 `Connection: close`：禁用 keep-alive 连接复用，规避本地 axum 测试
    // server 偶发关闭 keep-alive 连接导致的 IncompleteMessage（见 api.rs 说明）。
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(reqwest::header::CONNECTION, "close".parse().unwrap());
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .default_headers(headers)
        .build()
        .unwrap()
}

#[tokio::test]
async fn list_knowledge_sources() {
    let (base, _kb) = build_server().await;
    let res: Vec<KnowledgeSource> = client()
        .get(format!("{base}/gec/knowledge/sources"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!res.is_empty(), "知识源注册表应非空");
    let en = res.iter().find(|s| s.lang == "en").expect("应有 en 知识源");
    assert_eq!(en.license_scope, "public", "内置示例源应为公开许可");
    assert!(
        en.point_count > 200,
        "en 源关联知识点数应 > 200，实际 {}",
        en.point_count
    );
}

#[tokio::test]
async fn list_knowledge_points_with_source_filter() {
    let (base, _kb) = build_server().await;
    let all: Vec<serde_json::Value> = client()
        .get(format!("{base}/gec/knowledge/points?lang=en"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(all.len() > 200, "en 知识点应完整: {}", all.len());
    let sid = all[0]["sources"][0]["source_id"]
        .as_str()
        .unwrap()
        .to_string();
    let filtered: Vec<serde_json::Value> = client()
        .get(format!(
            "{base}/gec/knowledge/points?lang=en&source_id={sid}"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(filtered.len(), all.len(), "全部点均关联该源");
    let none: Vec<serde_json::Value> = client()
        .get(format!(
            "{base}/gec/knowledge/points?lang=en&source_id=src-unknown"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(none.is_empty(), "未知源应过滤为空");
    let other: Vec<serde_json::Value> = client()
        .get(format!("{base}/gec/knowledge/points?lang=xx"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(other.is_empty(), "无该语言集合时返回空");
}

#[tokio::test]
async fn en_table_ip_guard() {
    // IP 回归守护：决策表只含可提取规则，不得含知识源内容
    let (base, _kb) = build_server().await;
    let view: shared_model::DecisionTableView = client()
        .get(format!("{base}/gec/tables/en"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    for frag in [
        "徐广联",
        "大学英语语法",
        "第一讲",
        "第二讲",
        "讲座与测试",
        "grammarPoint",
        "LangFixer 示例英语语法指南",
    ] {
        assert!(!view.raw.contains(frag), "en 决策表不应包含: {frag}");
    }
}
#[tokio::test]
async fn attribute_enriched_via_api() {
    let (base, _kb) = build_server().await;
    let ctx = AttributionContext {
        lang: "en".into(),
        error_type: "article".into(),
        confidence: 0.9,
        session_error_count: 0,
        session_meta: None,
        grammar_point: Some("article.3".into()),
    };
    let res: AttributionResult = client()
        .post(format!("{base}/gec/attribute"))
        .json(&ctx)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(res.trace.matched_rule, "article_novice");
    assert!(!res.sources.is_empty(), "应富化知识源引用");
    assert!(
        res.sources[0].title.contains("示例英语语法指南"),
        "title 应从注册表解析: {:?}",
        res.sources[0].title
    );
    assert!(
        res.sources[0].location.contains("二.1"),
        "location: {}",
        res.sources[0].location
    );
    assert!(
        res.examples.iter().any(|e| e.contains("a ancient source")),
        "examples 应含该点改错对: {:?}",
        res.examples
    );
}

#[tokio::test]
async fn knowledge_ablation_toggle_gates_enrichment() {
    // 测评消融臂 `knowledge: false`：PUT /gec/settings 关闭知识层 →
    // /gec/attribute 的知识库富化旁路；恢复开启 → 富化回来
    let (base, _kb) = build_server().await;
    let ctx = AttributionContext {
        lang: "en".into(),
        error_type: "article".into(),
        confidence: 0.9,
        session_error_count: 0,
        session_meta: None,
        grammar_point: Some("article.3".into()),
    };
    let c = client();

    // 默认开启：富化含知识源引用
    let res: AttributionResult = c
        .post(format!("{base}/gec/attribute"))
        .json(&ctx)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!res.sources.is_empty(), "知识层开启时应富化知识源引用");

    // 关闭知识层（部分更新；GET/PUT 响应回显开关状态）
    let s: serde_json::Value = c
        .put(format!("{base}/gec/settings"))
        .json(&serde_json::json!({ "knowledge_enabled": false }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(s["knowledge_enabled"], false);

    let res: AttributionResult = c
        .post(format!("{base}/gec/attribute"))
        .json(&ctx)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(res.sources.is_empty(), "知识层关闭：富化应被旁路");
    assert_eq!(
        res.trace.matched_rule, "article_novice",
        "表级归因不受知识层开关影响"
    );

    // 恢复开启 → 富化回来（消融臂跑完必须能恢复）
    let s: serde_json::Value = c
        .put(format!("{base}/gec/settings"))
        .json(&serde_json::json!({ "knowledge_enabled": true }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(s["knowledge_enabled"], true);
    let res: AttributionResult = c
        .post(format!("{base}/gec/attribute"))
        .json(&ctx)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!res.sources.is_empty(), "恢复开启后富化应回来");
}

#[tokio::test]
async fn put_knowledge_rejects_invalid() {
    let (base, _kb) = build_server().await;
    // 重复 source_id → 400
    let res = client()
        .put(format!("{base}/gec/knowledge/sources"))
        .json(&serde_json::json!([
            { "source_id": "s", "title": "a", "lang": "en" },
            { "source_id": "s", "title": "b", "lang": "en" }
        ]))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);
    // 非法知识点结构 → 400
    let res = client()
        .put(format!("{base}/gec/knowledge/points/en"))
        .json(&serde_json::json!([{ "point_id": "p" }]))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);
}

#[tokio::test]
async fn put_knowledge_points_hot_reload() {
    let (base, kb) = build_server().await;
    // 热更新 en 知识点集合（测试进程内替换；成功后 GET 立即可见）
    let body = serde_json::json!([{
        "point_id": "noun.1",
        "lang": "en",
        "error_type": "noun",
        "title": "测试点",
        "cefr": "A2",
        "remedial": "测试补救",
        "sources": [{ "source_id": "src-x", "location": "某书 · 1.1" }]
    }]);
    let res = client()
        .put(format!("{base}/gec/knowledge/points/en"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let resp: serde_json::Value = res.json().await.unwrap();
    assert_eq!(resp["count"], 1);
    let pts: Vec<serde_json::Value> = client()
        .get(format!("{base}/gec/knowledge/points?lang=en"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(pts.len(), 1, "热更新后 GET 应立即可见");
    assert_eq!(pts[0]["point_id"], "noun.1");

    // 写回落盘：热更新结果持久化到知识库目录（跨重启存活）
    let on_disk =
        std::fs::read_to_string(kb.join("points/en.json")).expect("points/en.json 应已写回");
    assert!(on_disk.contains("noun.1"), "热更新点应写回落盘");
    let reloaded = attribution_engine::knowledge::KnowledgeStore::new();
    reloaded.load_from_dir(&kb).unwrap();
    assert!(
        reloaded.get_point("en", "noun.1").is_some(),
        "重启后应读到热更新点"
    );
    let _ = std::fs::remove_dir_all(&kb);
}
