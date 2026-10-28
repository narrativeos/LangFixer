//! 正误表导出 API 集成测试（`MULTIMODAL.md` §18）
//!
//! 走真实链路：造含文字层的 PSD → job（A 档结构层）→ 检测 → `POST /gec/media/report`
//! → 断言：6 列与专家表同构、勾选生效、产物落盘、`wordcraft-cli` 转出的 docx 里
//! 表格与中文完好（读回 docx 文本核对）。
//!
//! 依赖 `photocraft-cli`（造载体）与 `wordcraft-cli`（转 docx）；缺失时按 SKIP 处理，
//! 但**已安装就必须产出 docx**——「装了工具却出不了 Word」是真缺陷。

use std::future::IntoFuture;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};

use api_server::{routes, state::AppState};
use media_core::{ReportFile, parse_gold_rows, to_columns};

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn tool_bin(env_key: &str, name: &str) -> Option<PathBuf> {
    if let Ok(p) = std::env::var(env_key) {
        let pb = PathBuf::from(&p);
        if pb.is_file() {
            return Some(pb);
        }
    }
    for dir in std::env::var("PATH").unwrap_or_default().split(':') {
        let pb = PathBuf::from(dir).join(name);
        if pb.is_file() {
            return Some(pb);
        }
    }
    let home = std::env::var("HOME").unwrap_or_default();
    [
        format!("{home}/.local/bin/{name}"),
        format!("/usr/local/bin/{name}"),
        format!("/opt/homebrew/bin/{name}"),
    ]
    .iter()
    .map(PathBuf::from)
    .find(|p| p.is_file())
}

#[allow(clippy::await_holding_lock)]
async fn start_media_server(workdir: &std::path::Path) -> String {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        for k in [
            "LLM_API_KEY",
            "LLM_BASE_URL",
            "LLM_MODEL",
            "GEC_MEDIA_VISION",
        ] {
            std::env::remove_var(k);
        }
        std::env::set_var("GEC_OFFLINE", "1");
        std::env::set_var("GEC_LEXICONS_DIR", manifest_dir().join("../lexicons"));
        std::env::set_var(
            "GEC_SETTINGS_FILE",
            std::env::temp_dir().join(format!("lf_report_{}.json", uuid::Uuid::new_v4().simple())),
        );
        std::env::set_var("GEC_MEDIA_ENABLED", "1");
        std::env::set_var("GEC_MEDIA_WORKDIR", workdir);
    }
    let db = std::env::temp_dir().join(format!("lf_report_{}.db", uuid::Uuid::new_v4().simple()));
    let state = Arc::new(AppState::from_env(
        Some(&manifest_dir().join("../decision-tables")),
        Some(&db),
    ));
    let app = routes::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, app).into_future());
    format!("http://{addr}")
}

/// 造一个含文字层的 PSD（正文含错：I saw a apple.）
fn make_psd(cli: &PathBuf, dir: &std::path::Path) -> PathBuf {
    let out = dir.join("poster.psd");
    let st = Command::new(cli)
        .arg("run")
        .arg("--new")
        .arg(r##"{"width":900,"height":300,"background":"white","name":"poster"}"##)
        .args(["--cmd", "type.create", "--params",
            r##"{"x":60,"y":120,"text":"I saw a apple.","size":48,"color":"#111111","name":"line1"}"##])
        .args(["--cmd", "type.create", "--params",
            r##"{"x":60,"y":220,"text":"This sentence is fine.","size":36,"color":"#111111","name":"line2"}"##])
        .arg("--out")
        .arg(&out)
        .status()
        .expect("启动 photocraft-cli 失败");
    assert!(st.success() && out.is_file(), "造 PSD 失败");
    out
}

async fn wait_ready(base: &str, id: &str) -> serde_json::Value {
    for _ in 0..60 {
        let j: serde_json::Value = reqwest::Client::new()
            .get(format!("{base}/gec/media/jobs/{id}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if matches!(
            j["stage"].as_str().unwrap_or(""),
            "ready" | "applied" | "failed"
        ) {
            return j;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    panic!("job {id} 未进入终态");
}

#[tokio::test]
async fn report_exports_expert_style_table_with_selection() {
    let Some(pc) = tool_bin("PHOTOCRAFT_CLI", "photocraft-cli") else {
        eprintln!("SKIP：未安装 photocraft-cli");
        return;
    };
    let wd = std::env::temp_dir().join(format!("lf_report_{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&wd).unwrap();
    let psd = make_psd(&pc, &wd);
    let base = start_media_server(&wd).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/gec/media/jobs?filename=poster.psd&lang=en"))
        .body(std::fs::read(&psd).unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "建 job 失败：{}",
        resp.text().await.unwrap()
    );
    let created: serde_json::Value = resp.json().await.unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    let job = wait_ready(&base, &id).await;
    assert_eq!(job["stage"], "ready", "结构层链路必须就绪：{job}");
    assert_eq!(job["extraction"], "structure");
    let ann_count = job["annotations"].as_array().map(|a| a.len()).unwrap_or(0);
    assert!(ann_count >= 1, "必须至少检出 1 处（a apple）");

    // —— 全量导出 ——
    let resp = client
        .post(format!("{base}/gec/media/report"))
        .json(&serde_json::json!({ "title": "海报文字核对表" }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "导出失败：{}",
        resp.text().await.unwrap()
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    let rid = body["report_id"].as_str().unwrap().to_string();
    assert_eq!(body["candidates"].as_u64().unwrap() as usize, ann_count);
    assert_eq!(
        body["exported"].as_u64().unwrap() as usize,
        ann_count,
        "默认全勾选"
    );
    let rows = body["rows"].as_array().unwrap();
    assert_eq!(rows.len(), ann_count);
    let loc = rows[0]["location"].as_str().unwrap_or("");
    assert!(
        loc.starts_with("图片1"),
        "「页码及位置」必须带载体标号：{loc}"
    );
    assert!(rows[0]["note"].as_str().unwrap().contains("抽取 structure"));
    assert_eq!(rows[0]["checked"], true);

    // 产物落盘 + 与 gold 解析器互逆（同一张表能被评测读回）
    let dir = wd.join("reports").join(&rid);
    for f in ["report.json", "report.md", "report.html"] {
        assert!(dir.join(f).is_file(), "缺产物 {f}");
    }
    let raw = std::fs::read_to_string(dir.join("report.json")).unwrap();
    let file: ReportFile = serde_json::from_str(&raw).expect("report.json 可回读");
    let cols = to_columns(&file.rows);
    assert_eq!(
        cols[0],
        [
            "序号",
            "页码及位置",
            "错误或待核原文",
            "正确或处理建议",
            "计错数",
            "备注及判定依据"
        ]
    );
    let back = parse_gold_rows(&cols);
    assert_eq!(
        back.len(),
        file.rows.len(),
        "导出表必须能被 gold 解析器读回"
    );
    assert_eq!(back[0].original, file.rows[0].original);

    // —— 勾选：剔除一条 ——
    let drop = rows[0]["annotation_id"].as_u64().unwrap() as u32;
    let resp = client
        .post(format!("{base}/gec/media/report"))
        .json(&serde_json::json!({ "rejected_ids": [drop], "docx": false }))
        .send()
        .await
        .unwrap();
    let body2: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        body2["exported"].as_u64().unwrap() as usize,
        ann_count - 1,
        "剔除必须生效"
    );
    assert_eq!(body2["dropped_ids"].as_array().unwrap().len(), 1);
    assert!(
        body2["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap_or("").contains("未生成 docx")),
        "docx:false 必须说明为什么没有 Word：{}",
        body2["warnings"]
    );

    // —— docx：装了 wordcraft-cli 就必须出得来，且表格 / 中文完好 ——
    let Some(wc) = tool_bin("WORDCRAFT_CLI", "wordcraft-cli") else {
        eprintln!("SKIP：未安装 wordcraft-cli，docx 断言跳过");
        let _ = std::fs::remove_dir_all(&wd);
        return;
    };
    assert!(
        dir.join("report.docx").is_file(),
        "wordcraft 可用时必须有 report.docx"
    );
    let bytes = std::fs::read(dir.join("report.docx")).unwrap();
    assert_eq!(&bytes[..2], b"PK", "docx 必须是 zip（OOXML）");

    let out = Command::new(&wc)
        .arg("text")
        .arg(dir.join("report.docx"))
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(text.contains("页码及位置"), "docx 表头必须存在：{text}");
    assert!(
        text.contains(file.rows[0].original.as_str()),
        "docx 必须含导出行的原文列：{text}"
    );

    // —— 取产物 + 非法 id ——
    let resp = client
        .get(format!("{base}/gec/media/reports/{rid}?format=docx"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
    );
    let resp = client
        .get(format!("{base}/gec/media/reports/{rid}?format=md"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert!(resp.text().await.unwrap().contains("| 序号 |"));
    let resp = client
        .get(format!("{base}/gec/media/reports/..%2Fetc?format=json"))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status() == 400 || resp.status() == 404,
        "非法 report id 必须被挡住：{}",
        resp.status()
    );
    let _ = std::fs::remove_dir_all(&wd);
}
