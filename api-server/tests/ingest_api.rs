//! 知识源导入管线 API 端到端：
//! - 一键导入开启（LLM 未配置 → 400，主链路不受影响）
//! - 草稿区 → 发布预览（plan）→ 一键发布（既有点 1:N 背书 / 新点造点）
//! - 发布后知识源注册表 / 知识点列表立即可见；放弃会话丢弃草稿

use std::sync::{Arc, Mutex};

use api_server::{middleware, routes, state::AppState};
use attribution_engine::ingest::DraftPoint;
use shared_model::KnowledgeSource;

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn env_guard() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// 建 server：把仓库 knowledge-base/ 与 decision-tables/ 复制到临时目录并指向它们
/// （测试 CWD 为 api-server/）。发布/热更新会写回知识库与决策表目录——必须用
/// 临时副本，避免污染仓库资产；同时返回 state（草稿区装配在 state 上，测试可直接播种草稿）
async fn build_server() -> (String, Arc<AppState>, std::path::PathBuf) {
    let _guard = env_guard();
    let root = std::env::temp_dir().join(format!(
        "langfixer_ingest_kb_{}",
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
    let app = routes::router(state.clone()).layer(middleware::cors::cors_layer());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, app).into_future());
    (format!("http://{addr}"), state, tables_dir)
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
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(reqwest::header::CONNECTION, "close".parse().unwrap());
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .default_headers(headers)
        .build()
        .unwrap()
}

fn test_source() -> KnowledgeSource {
    KnowledgeSource {
        source_id: "src-ingest-test".into(),
        title: "导入测试语法书".into(),
        author: "测试".into(),
        edition: "第一版".into(),
        lang: "en".into(),
        license_scope: "internal_test".into(),
        point_count: 0,
    }
}

fn test_drafts() -> Vec<DraftPoint> {
    vec![
        // 背书既有点（agent 显式匹配 article.3；发布后该点应有两个源）
        DraftPoint {
            error_type: "article".into(),
            title: "不定冠词 a/an 的选择（导入测试）".into(),
            section: String::new(),
            remedial: "规则讲解：元音音素前用 an".into(),
            cefr: "A2".into(),
            examples: vec![],
            corrections: vec![shared_model::CorrectionPair {
                wrong: "a hour".into(),
                right: "an hour".into(),
            }],
            location: "第三章 · 2.1".into(),
            match_point_id: "article.3".into(),
        },
        // 新概念 → 造新点
        DraftPoint {
            error_type: "article".into(),
            title: "导入测试专属新概念".into(),
            section: "9.9".into(),
            remedial: "规则讲解：测试专用".into(),
            cefr: "B2".into(),
            examples: vec![],
            corrections: vec![shared_model::CorrectionPair {
                wrong: "a informations".into(),
                right: "much information".into(),
            }],
            location: "第三章 · 9.9".into(),
            match_point_id: String::new(),
        },
        // 非法草稿 → stage 拒收
        DraftPoint {
            error_type: "article".into(),
            title: String::new(),
            section: String::new(),
            remedial: String::new(),
            cefr: String::new(),
            examples: vec![],
            corrections: vec![],
            location: String::new(),
            match_point_id: String::new(),
        },
    ]
}

#[tokio::test]
async fn start_ingest_requires_llm() {
    let (base, _state, _tables) = build_server().await;
    let text = "x".repeat(500);
    let res = client()
        .post(format!("{base}/gec/knowledge/ingest"))
        .json(&serde_json::json!({
            "source_id": "src-no-llm",
            "title": "无 LLM 测试",
            "lang": "en",
            "text": text
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400, "LLM 未配置应 400");
    let body: serde_json::Value = res.json().await.unwrap();
    let msg = body
        .as_str()
        .or_else(|| body.get("error").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string();
    assert!(msg.contains("LLM"), "错误信息应指向 LLM 未配置: {body}");
}

/// 读取决策表目录全部文件（IP 守卫基线：导入/发布只动知识库，绝不动决策表）
fn tables_snapshot() -> Vec<(String, Vec<u8>)> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../decision-tables");
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) == Some("json") {
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            out.push((name, std::fs::read(&path).unwrap()));
        }
    }
    out.sort();
    out
}

#[tokio::test]
async fn stage_plan_publish_end_to_end() {
    let (base, state, _tables) = build_server().await;
    let tables_before = tables_snapshot();
    // 播种草稿区（模拟 agent 挖掘产物；LLM 不在测试链路内）
    state.ingest.open_session(test_source(), 1).unwrap();
    let report = state
        .ingest
        .stage("src-ingest-test", test_drafts())
        .unwrap();
    assert_eq!(
        (report.accepted, report.rejected),
        (2, 1),
        "非法草稿应被拒收"
    );

    // 发布预览
    let plan: serde_json::Value = client()
        .get(format!("{base}/gec/knowledge/ingest/src-ingest-test/plan"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(plan["new_points"].as_array().unwrap().len(), 1);
    assert_eq!(plan["endorsements"].as_array().unwrap().len(), 1);

    // 一键发布
    let apply: serde_json::Value = client()
        .post(format!(
            "{base}/gec/knowledge/ingest/src-ingest-test/publish"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(apply["new_points"], 1);
    assert_eq!(apply["endorsed"], 1);

    // 注册表立即可见新源；新源关联 2 点（1 背书 + 1 新点）
    let sources: Vec<KnowledgeSource> = client()
        .get(format!("{base}/gec/knowledge/sources"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let new_src = sources
        .iter()
        .find(|s| s.source_id == "src-ingest-test")
        .expect("新知识源应在注册表");
    assert_eq!(new_src.point_count, 2);

    // 既有点 article.3 获得 1:N 双源背书
    let points: Vec<serde_json::Value> = client()
        .get(format!(
            "{base}/gec/knowledge/points?lang=en&source_id=src-ingest-test"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(points.len(), 2);
    let article3 = state.knowledge.get_point("en", "article.3").unwrap();
    assert!(
        article3
            .sources
            .iter()
            .any(|s| s.source_id == "src-sample-en")
    );
    assert!(
        article3
            .sources
            .iter()
            .any(|s| s.source_id == "src-ingest-test")
    );

    // 会话已关闭
    let list: Vec<serde_json::Value> = client()
        .get(format!("{base}/gec/knowledge/ingest"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(list.is_empty(), "发布后会话应关闭");

    // 反哺闭环：新点改错对 → lexicon_add 建议队列（pending，待人工采纳）
    let sugg: serde_json::Value = client()
        .get(format!(
            "{base}/gec/reflect/suggestions?status=pending&lang=en&limit=50"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let lex: Vec<&serde_json::Value> = sugg["suggestions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["kind"] == "lexicon_add")
        .collect();
    assert_eq!(lex.len(), 1, "发布新点应产生 1 条改错对建议: {sugg}");
    assert_eq!(lex[0]["payload"]["wrong"], "a informations");
    assert_eq!(lex[0]["payload"]["right"], "much information");

    // 归点数据化：新发布点的改错对自动参与归点（不再是静态映射 article.3）
    let pid = attribution_engine::knowledge::classify_point_en(
        &state.knowledge,
        "article",
        "a informations",
        "much information",
        Some("article.3".into()),
    );
    assert!(
        pid.as_deref()
            .is_some_and(|p| p.starts_with("article.") && p != "article.3"),
        "新发布点应命中归点而非静态兜底: {pid:?}"
    );

    // 持久化写回：发布后知识库目录文件包含新源与新点（跨重启存活）
    let kb = state.knowledge_dir.clone();
    let points_json =
        std::fs::read_to_string(kb.join("points/en.json")).expect("points/en.json 应已写回");
    assert!(
        points_json.contains("a informations"),
        "新点改错对应写回落盘"
    );
    let sources_json =
        std::fs::read_to_string(kb.join("sources.json")).expect("sources.json 应已写回");
    assert!(
        sources_json.contains("src-ingest-test"),
        "新知识源应写回落盘"
    );
    assert!(!kb.join("points/en.json.tmp").exists(), "不应残留临时文件");

    // 模拟重启：从磁盘重新加载知识库 → 数据驱动归点仍命中已发布点（与内存一致）
    let reloaded = attribution_engine::knowledge::KnowledgeStore::new();
    reloaded.load_from_dir(&kb).unwrap();
    let pid2 = attribution_engine::knowledge::classify_point_en(
        &reloaded,
        "article",
        "a informations",
        "much information",
        Some("article.3".into()),
    );
    assert_eq!(pid2, pid, "重启后归点应命中同一已发布点（持久化生效）");
    let _ = std::fs::remove_dir_all(&kb);

    // IP 守卫：发布只动知识库，决策表文件逐字节不变，且草稿内容不落入决策表
    let tables_after = tables_snapshot();
    assert_eq!(tables_before, tables_after, "导入/发布不得改动决策表文件");
    let marker = "导入测试专属新概念";
    for (name, bytes) in &tables_after {
        let content = String::from_utf8_lossy(bytes);
        assert!(!content.contains(marker), "草稿内容泄漏进决策表: {name}");
    }
}

#[tokio::test]
async fn drafts_endpoint_with_merge_preview() {
    let (base, state, _tables) = build_server().await;
    state.ingest.open_session(test_source(), 1).unwrap();
    state
        .ingest
        .stage("src-ingest-test", test_drafts())
        .unwrap();
    let drafts: Vec<serde_json::Value> = client()
        .get(format!(
            "{base}/gec/knowledge/ingest/src-ingest-test/drafts"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(drafts.len(), 2, "非法草稿已拒收，明细只含入区草稿");
    assert_eq!(drafts[0]["action"], "endorse");
    assert_eq!(drafts[0]["point_id"], "article.3");
    assert_eq!(drafts[1]["action"], "create");
    assert!(
        drafts[1]["point_id"]
            .as_str()
            .unwrap_or("")
            .starts_with("article."),
        "新点应为 article.N: {}",
        drafts[1]["point_id"]
    );
    // 会话摘要含状态字段（UI 轮询依据）
    let detail: serde_json::Value = client()
        .get(format!("{base}/gec/knowledge/ingest/src-ingest-test"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(detail["status"], "running");
}

#[tokio::test]
async fn republish_same_source_endorses_incrementally() {
    let (base, state, _tables) = build_server().await;
    // 第一次导入并发布
    state.ingest.open_session(test_source(), 1).unwrap();
    state
        .ingest
        .stage("src-ingest-test", test_drafts())
        .unwrap();
    let res = client()
        .post(format!(
            "{base}/gec/knowledge/ingest/src-ingest-test/publish"
        ))
        .send()
        .await
        .unwrap();
    assert!(res.status().is_success());

    // 重复导入同一知识源：允许开启新会话（增量背书场景）
    state.ingest.open_session(test_source(), 1).unwrap();
    state
        .ingest
        .stage("src-ingest-test", test_drafts())
        .unwrap();
    let plan: serde_json::Value = client()
        .get(format!("{base}/gec/knowledge/ingest/src-ingest-test/plan"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        plan["new_points"].as_array().unwrap().len(),
        0,
        "不应重复造点"
    );
    assert_eq!(plan["endorsements"].as_array().unwrap().len(), 0);
    assert_eq!(plan["already_covered"], 2, "两条草稿均已被本源背书");

    // 再次发布：幂等，注册表不重复
    let apply: serde_json::Value = client()
        .post(format!(
            "{base}/gec/knowledge/ingest/src-ingest-test/publish"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(apply["new_points"], 0);
    assert_eq!(apply["endorsed"], 0);
    assert_eq!(apply["already_covered"], 2);
    let sources: Vec<KnowledgeSource> = client()
        .get(format!("{base}/gec/knowledge/sources"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let n = sources
        .iter()
        .filter(|s| s.source_id == "src-ingest-test")
        .count();
    assert_eq!(n, 1, "注册表幂等（元数据覆盖而非追加）");
}

#[tokio::test]
async fn discard_drops_drafts() {
    let (base, state, _tables) = build_server().await;
    state.ingest.open_session(test_source(), 1).unwrap();
    state
        .ingest
        .stage("src-ingest-test", test_drafts())
        .unwrap();
    let res = client()
        .post(format!(
            "{base}/gec/knowledge/ingest/src-ingest-test/discard"
        ))
        .send()
        .await
        .unwrap();
    assert!(res.status().is_success());
    assert!(state.ingest.session("src-ingest-test").is_none());
    // 发布空会话 → 400
    let res = client()
        .post(format!(
            "{base}/gec/knowledge/ingest/src-ingest-test/publish"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);
}

/// 解析预览：markdown 按标题切章 + 建议 source_id；上传导入未配 LLM → 400
#[tokio::test]
async fn parse_preview_and_upload_endpoint() {
    let (base, _state, _tables) = build_server().await;
    let md = "# 第一讲 冠词\n\n定冠词 the 表特指。\n\n# 第二讲 数词\n\n基数词与序数词。\n";

    let res = client()
        .post(format!(
            "{base}/gec/knowledge/parse-preview?filename=book.md"
        ))
        .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
        .body(md)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let v: serde_json::Value = res.json().await.unwrap();
    assert_eq!(v["format"], "md");
    assert_eq!(v["chapters"].as_array().unwrap().len(), 2);
    assert_eq!(v["chapters"][0]["heading"], "第一讲 冠词");
    assert_eq!(v["suggested_source_id"], "src-book");

    // 上传导入：LLM 未配置 → 400（解析成功但挖掘无法进行）
    let res = client()
        .post(format!(
            "{base}/gec/knowledge/ingest/upload?filename=book.md&source_id=src-md-test&lang=en"
        ))
        .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
        .body(md)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);
    let body: serde_json::Value = res.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains("LLM"));
}

/// 规则草稿 → rules-plan 预览 → 发布一并写决策表（新增 + 冲突跳过 + 文件写回）
#[tokio::test]
async fn rule_drafts_publish_into_decision_table() {
    let (base, state, tables) = build_server().await;
    let src = KnowledgeSource {
        source_id: "src-rulebook".into(),
        title: "规则书".into(),
        author: String::new(),
        edition: String::new(),
        lang: "en".into(),
        license_scope: "internal_test".into(),
        point_count: 0,
    };
    state.ingest.open_session(src, 1).unwrap();
    state
        .ingest
        .stage(
            "src-rulebook",
            vec![DraftPoint {
                error_type: "gerund".into(),
                title: "动名词作宾语".into(),
                section: String::new(),
                remedial: "规则讲解：动名词作宾语".into(),
                cefr: "B1".into(),
                examples: vec![],
                corrections: vec![],
                location: String::new(),
                match_point_id: String::new(),
            }],
        )
        .unwrap();
    let rule = |error_type: &str| attribution_engine::ingest::DraftRule {
        error_type: error_type.into(),
        novice: Some(true),
        min_confidence: None,
        attribution: "rule_missing".into(),
        weiner_locus: String::new(),
        weiner_stability: String::new(),
        weiner_controllability: String::new(),
        cefr: "B1".into(),
        remedial: format!("规则讲解：{error_type} 新手层"),
    };
    // article_novice 已被内置示例源 src-sample-en 占用 → conflict；gerund_novice → new
    state
        .ingest
        .stage_rules("src-rulebook", vec![rule("article"), rule("gerund")])
        .unwrap();

    // rules-plan 预览
    let res = client()
        .get(format!(
            "{base}/gec/knowledge/ingest/src-rulebook/rules-plan"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let plan: serde_json::Value = res.json().await.unwrap();
    let plan = plan.as_object().expect("rules-plan 非空");
    assert_eq!(plan["new"], 1);
    assert_eq!(plan["conflicts"], 1);
    assert_eq!(plan["replaced"], 0);

    // 发布：知识点 + 规则一并应用
    let res = client()
        .post(format!("{base}/gec/knowledge/ingest/src-rulebook/publish"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let report: serde_json::Value = res.json().await.unwrap();
    assert_eq!(report["rules_new"], 1);
    assert_eq!(report["rules_conflicts"], 1);

    // 决策表文件写回：新行存在且带本源溯源；冲突行未被覆盖
    let raw = std::fs::read_to_string(tables.join("en_gec_attribution.json")).unwrap();
    assert!(raw.contains("gerund_novice"), "新规则行应写入决策表文件");
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let rows = v["nodes"][1]["content"]["rules"].as_array().unwrap();
    let article_novice = rows
        .iter()
        .find(|r| r["_id"] == "article_novice")
        .expect("article_novice 应保留");
    assert_eq!(article_novice["_meta"], "src-sample-en", "他源行不得被覆盖");
}
