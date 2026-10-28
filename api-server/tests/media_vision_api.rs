//! B 档（多模态 LLM 视觉抽取）端到端测试（`MULTIMODAL.md` §14 P1）
//!
//! 场景是 P0 打不通的那个：**载体没有文字层**（扁平位图），
//! 结构层取不到文本 → 视觉档补文本 → 检测 → 回映射到图片坐标。
//! 视觉模型用本机 mock 服务（断言请求真的带 `image_url` 且是 png data URL），
//! 因此本测试不依赖任何供应商凭据。
//!
//! 需要 `photocraft-cli`（未安装则跳过）：造一个**无文字层**的 PSD，
//! 走「psd → 导出 png → 送视觉端点」这座桥。

use std::future::IntoFuture;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};

use api_server::{routes, state::AppState};
use axum::response::IntoResponse;

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn env_guard() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn photocraft_bin() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("PHOTOCRAFT_CLI") {
        let pb = PathBuf::from(&p);
        if pb.is_file() {
            return Some(pb);
        }
    }
    for dir in std::env::var("PATH").unwrap_or_default().split(':') {
        let pb = PathBuf::from(dir).join("photocraft-cli");
        if pb.is_file() {
            return Some(pb);
        }
    }
    let home = std::env::var("HOME").unwrap_or_default();
    [
        format!("{home}/.local/bin/photocraft-cli"),
        "/usr/local/bin/photocraft-cli".to_string(),
    ]
    .iter()
    .map(PathBuf::from)
    .find(|p| p.is_file())
}

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "lf_vision_{}_{}",
        tag,
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// mock 视觉端点：断言请求形态，返回给定文本（JSON 数组字符串）
async fn start_mock_vision(reply: String) -> String {
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(move |body: axum::body::Bytes| async move {
            let req: serde_json::Value = serde_json::from_slice(&body).unwrap();
            // 检测层（media.level=3）也会打这个端点：那是纯文本请求，不是转录请求。
            // 本测试测的是「抽取档 + bbox 定位」，所以文本请求给一个空建议即可（不假装 LLM 找到了错）。
            let multimodal = req["messages"][1]["content"].is_array();
            if !multimodal {
                return axum::Json(serde_json::json!({
                    "choices": [{ "message": { "content": "[]" } }]
                }));
            }
            assert_eq!(
                req["model"], "vision-e2e",
                "media.vision_model 必须覆盖 llm.model"
            );
            assert_eq!(req["temperature"], 0.0, "转录必须零温度（要稳定）");
            let system = req["messages"][0]["content"].as_str().unwrap_or("");
            assert!(
                system.contains("逐字原样"),
                "必须用 prompts/media_vision_prompt.json 的 system（不得静默回退内置默认）"
            );
            let parts = req["messages"][1]["content"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let image = parts
                .iter()
                .find(|p| p["type"] == "image_url")
                .expect("视觉请求必须带 image_url part");
            let url = image["image_url"]["url"].as_str().unwrap_or("").to_string();
            assert!(
                url.starts_with("data:image/png;base64,"),
                "psd 必须先导出 png 再送视觉端点，实测前缀：{}",
                url.chars().take(40).collect::<String>()
            );
            axum::Json(serde_json::json!({
                "choices": [{ "message": { "content": reply } }]
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

/// 起服务：多模态 + 视觉档开启，LLM 指向 mock
#[allow(clippy::await_holding_lock)]
async fn start_vision_server(mock_base: &str, workdir: &std::path::Path, vision: bool) -> String {
    let _guard = env_guard();
    unsafe {
        std::env::set_var("LLM_API_KEY", "sk-test");
        std::env::set_var("LLM_BASE_URL", mock_base);
        std::env::set_var("LLM_MODEL", "text-model");
        std::env::set_var("GEC_MEDIA_VISION_MODEL", "vision-e2e");
        std::env::set_var("GEC_MEDIA_ENABLED", "1");
        std::env::set_var("GEC_MEDIA_VISION", if vision { "1" } else { "0" });
        // 排队重试免等待：真实退避是 20s 起，测试不能真等（重试行为由本文件的排队测试覆盖）
        std::env::set_var("GEC_MEDIA_VISION_RETRY_BACKOFF_SECS", "0,0");
        std::env::set_var("GEC_MEDIA_VISION_RETRY_BUDGET_SECS", "60");
        std::env::set_var("GEC_MEDIA_WORKDIR", workdir);
        std::env::remove_var("GEC_ENGINE_ZH");
        std::env::remove_var("GEC_ENGINE_EN");
        std::env::remove_var("GEC_STAT_ENDPOINT");
        // 检测仍走本地规则（en 的 a/an 冠词规则本地可命中）
        std::env::set_var("GEC_OFFLINE", "1");
        std::env::set_var("GEC_LEXICONS_DIR", manifest_dir().join("../lexicons"));
        std::env::set_var("LLM_PROMPTS_DIR", manifest_dir().join("../prompts"));
        std::env::set_var(
            "GEC_SETTINGS_FILE",
            std::env::temp_dir().join(format!(
                "lf_vision_test_{}.json",
                uuid::Uuid::new_v4().simple()
            )),
        );
    }
    let db = std::env::temp_dir().join(format!(
        "lf_vision_test_{}.db",
        uuid::Uuid::new_v4().simple()
    ));
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

/// 造一个**无文字层**的 PSD（900x300 白底，只有一个 Background 像素层）
fn make_flat_psd(cli: &PathBuf, dir: &std::path::Path) -> PathBuf {
    let out = dir.join("flat.psd");
    let st = Command::new(cli)
        .arg("run")
        .arg("--new")
        .arg(r##"{"width":900,"height":300,"background":"white","name":"flat"}"##)
        .arg("--out")
        .arg(&out)
        .output()
        .expect("启动 photocraft-cli 失败");
    assert!(
        st.status.success(),
        "造 PSD 失败：{}",
        String::from_utf8_lossy(&st.stderr)
    );
    out
}

async fn wait_terminal(client: &reqwest::Client, base: &str, id: &str) -> serde_json::Value {
    for _ in 0..120 {
        let j: serde_json::Value = client
            .get(format!("{base}/gec/media/jobs/{id}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let stage = j["stage"].as_str().unwrap_or("");
        if matches!(stage, "ready" | "failed" | "applied") {
            return j;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    panic!("job 未在 60s 内进入终态");
}

#[tokio::test]
async fn flat_carrier_falls_back_to_vision_and_locates_by_bbox() {
    let Some(cli) = photocraft_bin() else {
        eprintln!("SKIP：未安装 photocraft-cli（设 PHOTOCRAFT_CLI 可启用本测试）");
        return;
    };
    // 模型「看到」的文字：故意含 a/an 冠词错（本地规则可命中），bbox 用归一化口径
    let reply = r##"```json
[
 {"text":"I saw a apple.","bbox":[0.06,0.35,0.56,0.45],"confidence":0.94},
 {"text":"This sentence is fine.","bbox":[0.06,0.7,0.56,0.78],"confidence":0.91}
]
```"##
        .to_string();
    let mock = start_mock_vision(reply).await;
    let wd = temp_dir("e2e");
    let base = start_vision_server(&mock, &wd, true).await;
    let psd = make_flat_psd(&cli, &wd);

    let resp = reqwest::Client::new()
        .post(format!("{base}/gec/media/jobs?filename=flat.psd&lang=en"))
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
    let job = wait_terminal(&reqwest::Client::new(), &base, &id).await;
    assert_eq!(job["stage"], "ready", "job 失败：{}", job["error"]);

    // 来源档必须是 vision（结构层为空才允许兜底）
    assert_eq!(
        job["extraction"], "vision",
        "扁平载体必须走 B 档：{:?}",
        job["warnings"]
    );
    assert_eq!(job["extraction_model"], "vision-e2e");

    let units = job["units"].as_array().unwrap();
    assert_eq!(units.len(), 2, "视觉块必须成为单元：{:?}", job["warnings"]);
    for u in units {
        assert_eq!(u["source"], "vision");
        assert_eq!(u["editable"], false, "扁平位图没有可无损改字的原对象");
        assert_eq!(
            u["region"]["region"], "image_box",
            "有 bbox 必须落到图片坐标"
        );
        let (x, y, w, h) = (
            u["region"]["x"].as_f64().unwrap(),
            u["region"]["y"].as_f64().unwrap(),
            u["region"]["w"].as_f64().unwrap(),
            u["region"]["h"].as_f64().unwrap(),
        );
        assert!(x >= 0.0 && y >= 0.0 && w > 0.0 && h > 0.0);
        assert!(
            x + w <= 900.0 && y + h <= 300.0,
            "框不得越出载体：{x},{y},{w},{h}"
        );
    }

    // 检测内核复用：视觉文本喂给同一条级联，错误回映射到 image_box
    let anns = job["annotations"].as_array().unwrap();
    assert!(!anns.is_empty(), "应检出冠词错误，实测：{anns:?}");
    let hit = anns
        .iter()
        .find(|a| a["error"]["error_type"] == "article")
        .expect("冠词错误必须被检出");
    assert_eq!(hit["region"]["region"], "image_box");
    assert_eq!(hit["apply"], "overlay");

    // 字节级不变量在 B 档同样成立（offset 反查表与来源档无关）
    let stitched = job["stitched"]["text"].as_str().unwrap();
    let uid = hit["unit_id"].as_u64().unwrap();
    let range = job["stitched"]["unit_ranges"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r[0].as_u64().unwrap() == uid)
        .unwrap()
        .as_array()
        .unwrap()[1]
        .as_array()
        .unwrap()
        .clone();
    let u0 = range[0].as_u64().unwrap() as usize;
    let ls = hit["local_span"].as_array().unwrap();
    let (l0, l1) = (
        ls[0].as_u64().unwrap() as usize,
        ls[1].as_u64().unwrap() as usize,
    );
    assert_eq!(
        &stitched[u0 + l0..u0 + l1],
        hit["error"]["original"].as_str().unwrap(),
        "B 档回映射字节必须与 error.original 一致"
    );

    // 原始响应落盘（排查「模型到底说了什么」）
    assert!(wd.join(&id).join("vision_raw.txt").is_file());
    let _ = std::fs::remove_dir_all(&wd);
}

#[tokio::test]
async fn flat_carrier_without_vision_reports_honest_empty() {
    let Some(cli) = photocraft_bin() else {
        eprintln!("SKIP：未安装 photocraft-cli（设 PHOTOCRAFT_CLI 可启用本测试）");
        return;
    };
    let mock = start_mock_vision("[]".into()).await;
    let wd = temp_dir("novision");
    let base = start_vision_server(&mock, &wd, false).await;
    let psd = make_flat_psd(&cli, &wd);

    let resp = reqwest::Client::new()
        .post(format!("{base}/gec/media/jobs?filename=flat.psd&lang=en"))
        .body(std::fs::read(&psd).unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let created: serde_json::Value = resp.json().await.unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    let job = wait_terminal(&reqwest::Client::new(), &base, &id).await;

    assert_eq!(job["stage"], "ready");
    assert_eq!(job["extraction"], "none", "未开 B 档时来源档必须是 none");
    assert!(job["units"].as_array().unwrap().is_empty());
    assert!(job["annotations"].as_array().unwrap().is_empty());
    let warnings = job["warnings"].as_array().unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap().contains("视觉档")),
        "必须明确告警「取不到文本」而不是静默返回空：{warnings:?}"
    );
    let _ = std::fs::remove_dir_all(&wd);
}

/// mock 网关：前 `fails` 次**转录请求**回 503（带 `Retry-After: 0`），之后正常回话。
/// 检测层的纯文本请求不受影响——本测试测的是「网关排队时视觉档要等，而不是把载体判死」。
/// 返回 `(base_url, 转录请求次数)`：次数是关键断言。
async fn start_flaky_vision(
    fails: usize,
    reply: String,
) -> (String, Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(move |body: axum::body::Bytes| {
            let counter = counter.clone();
            let reply = reply.clone();
            async move {
                let req: serde_json::Value = serde_json::from_slice(&body).unwrap();
                let multimodal = req["messages"][1]["content"].is_array();
                if !multimodal {
                    return axum::Json(serde_json::json!({
                        "choices": [{ "message": { "content": "[]" } }]
                    }))
                    .into_response();
                }
                let n = counter.fetch_add(1, Ordering::SeqCst);
                if n < fails {
                    let mut headers = axum::http::HeaderMap::new();
                    headers.insert(
                        axum::http::header::RETRY_AFTER,
                        axum::http::HeaderValue::from_static("0"),
                    );
                    return (axum::http::StatusCode::SERVICE_UNAVAILABLE, headers, "")
                        .into_response();
                }
                axum::Json(serde_json::json!({
                    "choices": [{ "message": { "content": reply } }]
                }))
                .into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (format!("http://{addr}/v1"), hits)
}

/// 立场：LLM 服务基本稳定，503 多半只是**排队**。排队两次就把载体判死
/// （`extraction = none` + 0 单元）等于把可用结果丢掉。
#[tokio::test]
async fn flat_carrier_recovers_from_gateway_queueing() {
    use std::sync::atomic::Ordering;
    let Some(cli) = photocraft_bin() else {
        eprintln!("SKIP：未安装 photocraft-cli（设 PHOTOCRAFT_CLI 可启用本测试）");
        return;
    };
    let reply = r##"[{"text":"I saw a apple.","bbox":[0.06,0.35,0.56,0.45],"confidence":0.94}]"##
        .to_string();
    let (mock, hits) = start_flaky_vision(2, reply).await;
    let wd = temp_dir("queue");
    let base = start_vision_server(&mock, &wd, true).await;
    let psd = make_flat_psd(&cli, &wd);

    let resp = reqwest::Client::new()
        .post(format!("{base}/gec/media/jobs?filename=flat.psd&lang=en"))
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
    let job = wait_terminal(&reqwest::Client::new(), &base, &id).await;

    assert_eq!(
        job["stage"], "ready",
        "网关排队两次不该让载体报废：{}",
        job["error"]
    );
    assert_eq!(job["extraction"], "vision", "排队后必须拿到视觉档结果");
    assert_eq!(
        hits.load(Ordering::SeqCst),
        3,
        "必须重试到放行（首次 + 2 次重试）"
    );
    let warnings = job["warnings"].as_array().unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap().contains("网关排队重试 2 次后成功")),
        "重试成功必须留痕（不能静默当成一次就成）：{warnings:?}"
    );
    let _ = std::fs::remove_dir_all(&wd);
}
