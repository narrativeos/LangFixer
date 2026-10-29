//! 多模态审校 API 集成测试（`MULTIMODAL.md` §14 P0）
//!
//! 两类断言：
//! 1. **不依赖外部工具**：开关关闭 → 503；未知 job → 404；job 目录隔离（走 PDF +
//!    `mode=simple`——模式门下唯一零 CLI / 零 LLM 的档，见 §15.1）；
//! 2. **依赖 PhotoCraft CLI**（`PHOTOCRAFT_CLI` 或已安装才跑，否则跳过）：
//!    造含文字层的 PSD → 上传 → 全链路（取文本 / 检测 / 回映射）→ 回写 →
//!    **重跑幂等**（标注图层不翻倍）。

use std::future::IntoFuture;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};

use api_server::{routes, state::AppState};

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn env_guard() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// PhotoCraft CLI 解析（与 media-core 同样的三级顺序）
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

/// 起一个多模态已开启的 server（每测试独立设置文件 / db / job 目录）
///
/// env 锁跨 await 是既有测试的既定模式（并行的 `std::env` 改动会互相污染，
/// 正确性优先于 lint）
#[allow(clippy::await_holding_lock)]
async fn start_media_server(enabled: bool, workdir: &std::path::Path) -> String {
    let _guard = env_guard();
    unsafe {
        for k in [
            "LLM_API_KEY",
            "LLM_BASE_URL",
            "LLM_MODEL",
            "GEC_ENGINE_ZH",
            "GEC_ENGINE_EN",
            "GEC_STAT_ENDPOINT",
        ] {
            std::env::remove_var(k);
        }
        // 离线：只用本地规则（en 的 a/an 冠词规则本地可命中）
        std::env::set_var("GEC_OFFLINE", "1");
        std::env::set_var("GEC_LEXICONS_DIR", manifest_dir().join("../lexicons"));
        std::env::set_var(
            "GEC_SETTINGS_FILE",
            std::env::temp_dir().join(format!(
                "lf_media_test_{}.json",
                uuid::Uuid::new_v4().simple()
            )),
        );
        std::env::set_var("GEC_MEDIA_ENABLED", if enabled { "1" } else { "0" });
        std::env::set_var("GEC_MEDIA_WORKDIR", workdir);
    }
    let db = std::env::temp_dir().join(format!(
        "lf_media_test_{}.db",
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

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "lf_media_{}_{}",
        tag,
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[tokio::test]
async fn media_disabled_returns_503() {
    let wd = temp_dir("off");
    let base = start_media_server(false, &wd).await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/gec/media/jobs?filename=a.psd"))
        .body(vec![0u8; 16])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 503, "未开启多模态时必须 503，而不是静默接受");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("GEC_MEDIA_ENABLED")
    );
    let _ = std::fs::remove_dir_all(&wd);
}

#[tokio::test]
async fn unknown_job_is_404() {
    let wd = temp_dir("404");
    let base = start_media_server(true, &wd).await;
    let resp = reqwest::Client::new()
        .get(format!("{base}/gec/media/jobs/nope"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let _ = std::fs::remove_dir_all(&wd);
}

#[tokio::test]
async fn probes_report_vision_and_ocr_readiness() {
    let wd = temp_dir("probes");
    let base = start_media_server(true, &wd).await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/gec/media/probes"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "probes 失败：{}",
        resp.text().await.unwrap()
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["photocraft"].is_object(),
        "必须上报 PhotoCraft 探测结果"
    );
    // B/C 档就绪状态：未开启时必须给出「怎么开」的可行动原因（UI 直接显示）
    let vision = &body["vision"];
    assert_eq!(vision["enabled"], false, "B 档默认关闭");
    assert_eq!(vision["ready"], false, "未配置 LLM 时不得谎报就绪");
    assert!(
        vision["degraded_reason"]
            .as_str()
            .unwrap_or("")
            .contains("use_vision"),
        "降级原因必须告诉用户怎么开：{vision}"
    );
    assert!(
        ["media_vision", "builtin"].contains(&vision["template"].as_str().unwrap_or("")),
        "必须上报提示词来源：{vision}"
    );
    assert_eq!(body["ocr"]["ready"], false, "C 档未实现，不得谎报就绪");
    // 总开关必须在探测里可见：`probes` 不 gate，UI 靠它提前说「为什么不能开工」，
    // 而不是等用户点「开始审校」吃一句 503
    assert_eq!(body["enabled"], true, "必须上报多模态总开关：{body}");
    // 上传前压缩口径要能探测到（UI 显示「≤2000px / ≤1.5MB JPEG q80」，
    // 用户才知道画框对应的分辨率是哪一档）
    assert_eq!(
        vision["max_edge"], 2000,
        "必须上报视觉压缩长边上限：{vision}"
    );
    assert_eq!(
        vision["max_bytes"], 1_500_000,
        "必须上报视觉压缩字节上限：{vision}"
    );
    assert_eq!(
        vision["jpeg_quality"], 80,
        "必须上报视觉 JPEG 质量：{vision}"
    );
    let _ = std::fs::remove_dir_all(&wd);
}

#[tokio::test]
async fn job_is_isolated_in_its_own_dir() {
    let wd = temp_dir("iso");
    let base = start_media_server(true, &wd).await;
    // 必须用 PDF + `mode=simple`：PDF 简单模式是纯 Rust 解析，零 CLI、零 LLM，任何机器
    // （含 CI runner）都能建单。换成图片载体就出环境依赖——advanced 档要 `photocraft-cli`、
    // simple 档要视觉模型 + LLM，裸机上两档都被模式门挡掉，建单直接 503（§15.1）
    let resp = reqwest::Client::new()
        .post(format!(
            "{base}/gec/media/jobs?filename=poster.pdf&lang=en&mode=simple"
        ))
        .body(b"not-a-real-pdf".to_vec())
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let job: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(status, 200, "PDF 简单模式零依赖，必须能建单：{job}");
    let id = job["id"].as_str().unwrap().to_string();
    assert_eq!(job["stage"], "copying");
    assert_eq!(job["mode"], "simple", "必须按请求选档，不静默换档");
    // 副本落在 job 专属目录，文件名可控（无目录穿越）
    assert!(wd.join(&id).join("poster.pdf").is_file());
    // 轮询到终态（坏文件必须 failed，不能卡住）
    let mut stage = String::new();
    for _ in 0..60 {
        let j: serde_json::Value = reqwest::Client::new()
            .get(format!("{base}/gec/media/jobs/{id}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        stage = j["stage"].as_str().unwrap().to_string();
        if matches!(stage.as_str(), "ready" | "failed" | "applied") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    assert_eq!(stage, "failed", "非载体文件必须失败并给出原因");
    let _ = std::fs::remove_dir_all(&wd);
}

/// 递归统计图层名（含组内子层）
fn count_layers_named_prefix(v: &serde_json::Value, prefix: &str, out: &mut usize) {
    if let Some(layers) = v
        .get("layers")
        .or(v.get("children"))
        .and_then(|x| x.as_array())
    {
        for l in layers {
            if l.get("name")
                .and_then(|n| n.as_str())
                .is_some_and(|n| n.starts_with(prefix))
            {
                *out += 1;
            }
            count_layers_named_prefix(l, prefix, out);
        }
    }
}

/// 造一个含文字层的 PSD（正文故意含错：I saw a apple.）
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

async fn apply_overlay(base: &str, id: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{base}/gec/media/jobs/{id}/apply"))
        .json(&serde_json::json!({ "apply_mode": "overlay" }))
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn photo_job_detects_remaps_and_applies_idempotently() {
    let Some(cli) = photocraft_bin() else {
        eprintln!("SKIP：未安装 photocraft-cli（设 PHOTOCRAFT_CLI 可启用本测试）");
        return;
    };
    let wd = temp_dir("e2e");
    let base = start_media_server(true, &wd).await;
    let psd = make_psd(&cli, &wd);
    let bytes = std::fs::read(&psd).unwrap();

    let resp = reqwest::Client::new()
        .post(format!("{base}/gec/media/jobs?filename=poster.psd&lang=en"))
        .body(bytes)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let created: serde_json::Value = resp.json().await.unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    let job = wait_terminal(&reqwest::Client::new(), &base, &id).await;
    assert_eq!(job["stage"], "ready", "job 失败：{}", job["error"]);
    assert_eq!(job["extraction"], "structure", "含文字层载体必须走 A 档");

    // A 档取文本：两个文字层 = 两个单元（`info` 按图层面板自下而上列出，故不断言顺序）
    let units = job["units"].as_array().unwrap();
    assert_eq!(units.len(), 2, "应取到 2 个文字层单元");
    let mut texts: Vec<&str> = units.iter().map(|u| u["text"].as_str().unwrap()).collect();
    texts.sort_unstable();
    assert_eq!(texts, vec!["I saw a apple.", "This sentence is fine."]);
    let stitched = job["stitched"]["text"].as_str().unwrap();
    assert!(stitched.contains("I saw a apple.") && stitched.contains("This sentence is fine."));
    // 单元落点指向可编辑文字层对象
    assert!(
        units
            .iter()
            .all(|u| u["region"]["region"] == "layer_ref" && u["region"]["layer_id"].is_number()),
        "结构层单元的落点必须是 layer_ref"
    );

    // 检测内核复用：冠词错误被检出并回映射到含该句的单元
    let anns = job["annotations"].as_array().unwrap();
    assert!(!anns.is_empty(), "应检出冠词错误，实测：{anns:?}");
    let want_unit = units
        .iter()
        .find(|u| u["text"].as_str().unwrap().contains("a apple"))
        .unwrap();
    let hit = anns
        .iter()
        .find(|a| a["error"]["error_type"] == "article" && a["unit_id"] == want_unit["id"])
        .unwrap_or_else(|| panic!("冠词错误应回映射到含该句的单元，实测：{anns:?}"));
    assert_eq!(
        hit["apply"], "overlay",
        "默认回写模式必须是 Overlay（决策 ①）"
    );
    assert_eq!(hit["region"]["region"], "layer_ref");
    assert_eq!(hit["region"]["layer_id"], want_unit["region"]["layer_id"]);

    // 字节级不变量：stitched[unit_start + local_span] == error.original
    // （span↔unit 反查表若错位，这里一定炸）
    let ranges = job["stitched"]["unit_ranges"].as_array().unwrap();
    let range = ranges
        .iter()
        .find(|r| r[0] == want_unit["id"])
        .expect("unit_ranges 必须含该单元")
        .as_array()
        .unwrap()[1]
        .as_array()
        .unwrap()
        .clone();
    let (u0, u1) = (
        range[0].as_u64().unwrap() as usize,
        range[1].as_u64().unwrap() as usize,
    );
    let ls = hit["local_span"].as_array().unwrap();
    let (l0, l1) = (
        ls[0].as_u64().unwrap() as usize,
        ls[1].as_u64().unwrap() as usize,
    );
    let orig = hit["error"]["original"].as_str().unwrap();
    assert_eq!(
        &stitched[u0 + l0..u0 + l1],
        orig,
        "回映射字节必须与 error.original 一致（unit 起点 {u0}，局部 [{l0},{l1})）"
    );
    assert!(l1 <= u1 - u0, "局部 span 不得越出单元边界");
    // sidecar 已落盘
    assert!(wd.join(&id).join("annotations.json").is_file());

    // 回写 + 幂等：重跑不翻倍标注图层
    let r1 = apply_overlay(&base, &id).await;
    assert_eq!(r1.status(), 200, "回写失败：{}", r1.text().await.unwrap());
    let result = wd.join(&id).join("result.psd");
    assert!(result.is_file(), "必须产出可编辑 result.psd");
    let info1: serde_json::Value = serde_json::from_str(
        String::from_utf8_lossy(
            &Command::new(&cli)
                .args(["info"])
                .arg(&result)
                .arg("--compact")
                .output()
                .unwrap()
                .stdout,
        )
        .trim(),
    )
    .unwrap();
    let mut n1 = 0usize;
    count_layers_named_prefix(&info1, "mark-", &mut n1);
    assert!(n1 >= 2, "Overlay 应产出框 + 编号文字层，实测 {n1}");

    let r2 = apply_overlay(&base, &id).await;
    assert_eq!(r2.status(), 200);
    let info2: serde_json::Value = serde_json::from_str(
        String::from_utf8_lossy(
            &Command::new(&cli)
                .args(["info"])
                .arg(&result)
                .arg("--compact")
                .output()
                .unwrap()
                .stdout,
        )
        .trim(),
    )
    .unwrap();
    let mut n2 = 0usize;
    count_layers_named_prefix(&info2, "mark-", &mut n2);
    assert_eq!(
        n1, n2,
        "重复回写必须幂等（先清历史 mark-* 图层）：{n1} → {n2}"
    );

    // 预览产物
    let prev = reqwest::Client::new()
        .get(format!("{base}/gec/media/jobs/{id}/preview"))
        .send()
        .await
        .unwrap();
    assert_eq!(prev.status(), 200);
    assert_eq!(prev.headers()["content-type"], "image/png");

    let _ = std::fs::remove_dir_all(&wd);
}
