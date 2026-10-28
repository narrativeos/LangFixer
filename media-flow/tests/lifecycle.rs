//! 载体链路冒烟测试（两端共用：api-server 与桌面端跑的是这同一套编排）
//!
//! 覆盖 `create_job → 视觉兜底 → detect → remap → apply → report` 整条链，
//! 但把两个外部依赖换成本地替身：
//! * 检测级联 = [`MockDetector`]（断言宿主给的 level / force_llm，返回固定判定）；
//! * 视觉网关 = 本机 mock OpenAI 兼容端点（断言请求形态，返回带 bbox 的转录）。
//!
//! 需要 `photocraft-cli`（未安装则跳过）：inspect / apply / 预览导出都要它。

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use image::ImageEncoder as _;
use serde_json::json;

use media_flow::{
    CarrierDetector, DetectResult, FlowCtx, FlowError, JobOptions, MediaJobStore, MediaStage,
    ReportRequest, flow,
};

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
    let d = std::env::temp_dir().join(format!("lf_flow_{}_{}", tag, uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// 扁平位图（无文字层）：结构层取不到文本，只能靠视觉档
fn flat_png(w: u32, h: u32) -> Vec<u8> {
    let mut img = image::RgbImage::new(w, h);
    for (x, y, px) in img.enumerate_pixels_mut() {
        *px = image::Rgb([(255 - x % 256) as u8, (y % 256) as u8, 180]);
    }
    let mut buf: Vec<u8> = Vec::new();
    {
        let enc = image::codecs::png::PngEncoder::new(&mut buf);
        enc.write_image(img.as_raw(), w, h, image::ExtendedColorType::Rgb8)
            .unwrap();
    }
    buf
}

/// mock 视觉端点：断言请求形态，返回带 bbox 的转录
async fn start_mock_vision(reply: String) -> String {
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(move |body: axum::body::Bytes| async move {
            let req: serde_json::Value = serde_json::from_slice(&body).unwrap();
            let multimodal = req["messages"][1]["content"].is_array();
            if !multimodal {
                return axum::Json(json!({ "choices": [{ "message": { "content": "[]" } }] }));
            }
            assert_eq!(req["temperature"], 0.0, "转录必须零温度（要稳定）");
            let parts = req["messages"][1]["content"].as_array().cloned().unwrap();
            let image = parts
                .iter()
                .find(|p| p["type"] == "image_url")
                .expect("视觉请求必须带 image_url part");
            assert!(
                image["image_url"]["url"]
                    .as_str()
                    .unwrap_or("")
                    .starts_with("data:image/")
            );
            axum::Json(json!({ "choices": [{ "message": { "content": reply } }] }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    format!("http://{addr}/v1")
}

/// 宿主注入的检测级联替身：记录调用参数，返回「a apple → an apple」这一条判定
#[derive(Debug, Default)]
struct MockDetector {
    calls: RwLock<Vec<(String, Option<u8>, bool)>>,
}

#[async_trait]
impl CarrierDetector for MockDetector {
    async fn detect(
        &self,
        lang: &str,
        text: &str,
        level: Option<u8>,
        force_llm: bool,
        _category: Option<&str>,
    ) -> Result<DetectResult, FlowError> {
        self.calls
            .write()
            .unwrap()
            .push((lang.to_string(), level, force_llm));
        // 判定必须落在 stitched text 的真实字节区间上：回映射靠它，编不出假框
        let needle = "a apple";
        let Some(start) = text.find(needle) else {
            return Ok(DetectResult::default());
        };
        let err = shared_model::Error {
            lang: lang.to_string(),
            error_type: "article".to_string(),
            confidence: 0.9,
            span: [start, start + needle.len()],
            original: needle.to_string(),
            correction: Some("an apple".to_string()),
            message: "冠词与后接词不一致".to_string(),
            category: Some("grammar".to_string()),
            rule_id: Some("EN_ARTICLE_A_AN".to_string()),
            suggestions: vec![shared_model::Suggestion::new("an apple", 0.9)],
            issue_type: Some("grammar".to_string()),
            source: Some("rule".to_string()),
            also_found_by: Vec::new(),
        };
        Ok(DetectResult {
            errors: vec![err],
            layers: vec![json!({ "layer": "rule", "status": "ran", "found": 1 })],
        })
    }

    /// 故意报「LLM 未就绪」：建 job 就必须降级为 level 1 并留痕，而不是运行时 503
    fn llm_ready(&self, _lang: &str) -> bool {
        false
    }
}

async fn wait_terminal(ctx: &FlowCtx, id: &str) -> media_flow::MediaJob {
    for _ in 0..120 {
        let job = ctx.store.read().unwrap().get(id).cloned().unwrap();
        if job.stage.is_terminal() {
            return job;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    panic!("job 未在 60s 内进入终态：{id}");
}

/// 全链路：扁平位图 → 视觉兜底 → 检测（宿主降级留痕）→ 回映射带框 → 回写 → 正误表
#[tokio::test]
async fn flat_carrier_full_lifecycle_with_mock_detector_and_mock_gateway() {
    let Some(cli) = photocraft_bin() else {
        eprintln!("SKIP：未安装 photocraft-cli（设 PHOTOCRAFT_CLI 可启用本测试）");
        return;
    };
    let reply = r##"[{"text":"I saw a apple.","bbox":[0.06,0.35,0.56,0.45],"confidence":0.94}]"##
        .to_string();
    let gateway = start_mock_vision(reply).await;

    let wd = temp_dir("lifecycle");
    let ctx = FlowCtx {
        store: Arc::new(RwLock::new(MediaJobStore::new(&wd))),
        media: serde_json::from_value(json!({
            "enabled": true,
            "use_structure_layer": false,
            "use_vision": true,
            "photocraft_bin": cli.display().to_string(),
            "vision_model": "smoke-vision",
            "level": 3,
            "force_llm": true,
        }))
        .unwrap(),
        llm: langfixer_llm::LlmConfig {
            base_url: gateway,
            api_key: "sk-smoke".to_string(),
            model: "smoke-vision".to_string(),
            timeout_secs: 30,
            prompts_dir: PathBuf::from("prompts"),
        },
        prompts: langfixer_llm::templates::TemplateStore::default(),
    };

    let detector = Arc::new(MockDetector::default());
    let created = flow::create_job(
        &ctx,
        detector.clone(),
        "smoke.png",
        &flat_png(1200, 1600),
        JobOptions {
            lang: Some("en".to_string()),
            ..Default::default()
        },
    )
    .expect("建 job 失败");
    assert_eq!(created.stage, MediaStage::Copying);
    assert!(
        created
            .warnings
            .iter()
            .any(|w| w.contains("降级为规则 + 决策模型")),
        "LLM 未就绪必须在建 job 时就留痕降级，而不是等运行时报错：{:?}",
        created.warnings
    );

    let job = wait_terminal(&ctx, &created.id).await;
    assert_eq!(job.stage, MediaStage::Ready, "job 失败：{:?}", job.error);
    assert_eq!(job.extraction, "vision", "扁平位图必须由视觉档补文本");
    assert!(!job.units.is_empty(), "视觉转录必须落成文本单元");
    assert_eq!(job.annotations.len(), 1, "一条判定 → 一条标注");
    let a = &job.annotations[0];
    assert!(
        matches!(a.region, media_core::Region::ImageBox { .. }),
        "视觉块必须落到像素框（不能假装定位成功）：{:?}",
        a.region
    );
    assert_eq!(a.error.correction.as_deref(), Some("an apple"));

    // 宿主拿到的 level / force_llm 必须是降级后的那组（不是请求里的 3 / true）
    let calls = detector.calls.read().unwrap().clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "en");
    assert_eq!(calls[0].1, Some(1), "LLM 未就绪 → 实际只跑 level 1");
    assert!(!calls[0].2, "LLM 未就绪 → force_llm 必须关掉");

    // 回写：只改副本，原件字节必须没动
    let source = wd.join(&created.id).join(&created.filename);
    let before = std::fs::read(&source).unwrap();
    let applied = flow::apply_job(&ctx, &created.id, None)
        .await
        .expect("回写失败");
    assert_eq!(applied.stage, MediaStage::Applied);
    assert_eq!(
        std::fs::read(&source).unwrap(),
        before,
        "**永不改原件**：副本被改了"
    );
    let preview = applied.artifacts.preview.clone().expect("预览产物路径");
    assert!(Path::new(&preview).is_file(), "预览必须落盘：{preview}");

    // 正误表：勾选后的结果汇成表，导出产物落盘可取
    let report = flow::build_report(&ctx, ReportRequest::default())
        .await
        .expect("导出正误表失败");
    assert_eq!(report["candidates"], 1);
    assert_eq!(report["exported"], 1);
    let rid = report["report_id"].as_str().unwrap().to_string();
    for fmt in ["json", "md", "html"] {
        let (path, _ctype) = flow::report_file_path(&ctx, &rid, fmt).expect("导出产物缺失");
        assert!(path.is_file(), "{fmt} 导出产物不存在：{}", path.display());
    }
    // 告警必须跟着进表：降级、结构层关闭都不能被悄悄抹掉
    let warnings = report["warnings"].as_array().unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap().contains("结构层")),
        "关闭结构层这条告警必须进表：{warnings:?}"
    );

    let _ = std::fs::remove_dir_all(&wd);
}

/// 未启用时不得静默开工：错误里必须带可执行的 remedy
#[tokio::test]
async fn disabled_media_gives_actionable_error() {
    let ctx = FlowCtx {
        store: Arc::new(RwLock::new(MediaJobStore::new(temp_dir("disabled")))),
        media: serde_json::from_value(json!({})).unwrap(),
        llm: langfixer_llm::LlmConfig {
            base_url: "http://127.0.0.1:1/v1".to_string(),
            api_key: String::new(),
            model: "x".to_string(),
            timeout_secs: 5,
            prompts_dir: PathBuf::from("prompts"),
        },
        prompts: langfixer_llm::templates::TemplateStore::default(),
    };
    let err = flow::create_job(
        &ctx,
        Arc::new(MockDetector::default()),
        "x.png",
        &[1, 2, 3],
        JobOptions::default(),
    )
    .expect_err("未启用必须报错");
    let msg = err.to_string();
    assert!(
        msg.contains("media.enabled") || msg.contains("GEC_MEDIA_ENABLED"),
        "报错必须给出可执行的开关：{msg}"
    );
}
