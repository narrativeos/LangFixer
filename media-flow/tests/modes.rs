//! 简单模式（§15.1）冒烟测试：文档载体**一次外部 CLI 都不起**
//!
//! 这条链路的价值就是「没装三家工具也能跑」，所以测试里把三家 CLI 路径全部
//! 指向不存在的位置——如果哪一步偷偷去调，job 必然失败，测试就会红。

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use serde_json::json;

use media_flow::{
    CarrierDetector, DetectResult, FlowCtx, FlowError, JobOptions, MediaJobStore, MediaStage,
    PreviewKind, flow,
};

/// 手写一份 N 页最小 PDF（Helvetica + `Tj`）——不依赖仓库外的二进制
fn minimal_pdf(lines: &[&str]) -> Vec<u8> {
    let n = lines.len();
    let font_id = 3usize;
    let kids: Vec<String> = (0..n).map(|i| format!(" {} 0 R", 4 + 2 * i)).collect();
    let mut objs: Vec<Vec<u8>> = vec![
        b"<</Type/Catalog/Pages 2 0 R>>".to_vec(),
        format!("<</Type/Pages/Kids[{}]/Count {n}>>", kids.join(" ")).into_bytes(),
        b"<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>".to_vec(),
    ];
    for (i, line) in lines.iter().enumerate() {
        objs.push(
            format!(
                "<</Type/Page/Parent 2 0 R/MediaBox[0 0 300 300]/Contents {} 0 R/Resources<</Font<</F0 {font_id} 0 R>>>>>>",
                5 + 2 * i
            )
            .into_bytes(),
        );
        let stream = format!("BT /F0 12 Tf 20 200 Td ({line}) Tj ET");
        objs.push(format!("<</Length {}>>stream\n{stream}\nendstream", stream.len()).into_bytes());
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (i, body) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    let size = objs.len() + 1;
    out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for off in &offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<</Size {size}/Root 1 0 R>>\nstartxref\n{xref}\n%%EOF").as_bytes(),
    );
    out
}

/// 手写一份最小 docx（zip + word/document.xml）
fn minimal_docx(paragraphs: &[(&str, &str)]) -> Vec<u8> {
    use std::io::Write as _;
    let body: String = paragraphs
        .iter()
        .map(|(style, text)| {
            let pstyle = if style.is_empty() {
                String::new()
            } else {
                format!("<w:pPr><w:pStyle w:val=\"{style}\"/></w:pPr>")
            };
            format!("<w:p>{pstyle}<w:r><w:t>{text}</w:t></w:r></w:p>")
        })
        .collect();
    let document = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body>{body}</w:body></w:document>"
    );
    let buf = std::io::Cursor::new(Vec::new());
    let mut zw = zip::ZipWriter::new(buf);
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    zw.start_file("[Content_Types].xml", opts.clone()).unwrap();
    zw.write_all(br#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#).unwrap();
    zw.start_file("word/document.xml", opts).unwrap();
    zw.write_all(document.as_bytes()).unwrap();
    zw.finish().unwrap().into_inner()
}

/// 检测级联替身：把「a apple → an apple」落在 stitched text 的真实字节区间上
#[derive(Debug, Default)]
struct MockDetector {}

#[async_trait]
impl CarrierDetector for MockDetector {
    async fn detect(
        &self,
        lang: &str,
        text: &str,
        _level: Option<u8>,
        _force_llm: bool,
        _category: Option<&str>,
    ) -> Result<DetectResult, FlowError> {
        let needle = "a apple";
        let mut errors = Vec::new();
        let mut from = 0usize;
        while let Some(rel) = text[from..].find(needle) {
            let start = from + rel;
            errors.push(shared_model::Error {
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
            });
            from = start + needle.len();
        }
        Ok(DetectResult {
            layers: vec![json!({ "layer": "rule", "status": "ran", "found": errors.len() })],
            errors,
        })
    }

    fn llm_ready(&self, _lang: &str) -> bool {
        false
    }
}

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lf_modes_{}_{}", tag, uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// 三家 CLI 全部指向不存在的路径：简单模式必须照样跑通（哪一步偷偷起 CLI 就会红）
fn ctx(wd: &Path) -> FlowCtx {
    ctx_with(wd, "simple", true)
}

/// `mode` = 设置里的默认档；`broken_cli` = 把三家 CLI 路径指向不存在的位置
fn ctx_with(wd: &Path, mode: &str, broken_cli: bool) -> FlowCtx {
    let (photo, pdf, word) = if broken_cli {
        (
            "/nonexistent/photocraft-cli",
            "/nonexistent/pdfcraft-cli",
            "/nonexistent/wordcraft-cli",
        )
    } else {
        ("", "", "") // 留空 = 按 $PATH / 常见安装位置自动找
    };
    FlowCtx {
        store: Arc::new(RwLock::new(MediaJobStore::new(wd))),
        media: serde_json::from_value(json!({
            "enabled": true,
            "mode": mode,
            "photocraft_bin": photo,
            "pdfcraft_bin": pdf,
            "wordcraft_bin": word,
            "level": 1,
            "force_llm": false,
        }))
        .unwrap(),
        llm: langfixer_llm::LlmConfig {
            base_url: "http://127.0.0.1:1/v1".to_string(),
            api_key: String::new(),
            model: "x".to_string(),
            timeout_secs: 5,
            prompts_dir: PathBuf::from("prompts"),
        },
        prompts: langfixer_llm::templates::TemplateStore::default(),
    }
}

async fn wait_terminal(ctx: &FlowCtx, id: &str) -> media_flow::MediaJob {
    for _ in 0..60 {
        let job = ctx.store.read().unwrap().get(id).cloned().unwrap();
        if job.stage.is_terminal() {
            return job;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    panic!("job 未在 12s 内进入终态：{id}");
}

/// PDF 简单模式：零 CLI → 段落单元带页号 → 只读文档视图 → 正误表位置写「第N页」
#[tokio::test]
async fn pdf_simple_mode_needs_no_cli_and_keeps_page_numbers() {
    let wd = temp_dir("pdf");
    let c = ctx(&wd);
    let pdf = minimal_pdf(&[
        "Notice: the pool closes at 6pm. I saw a apple on the table.",
        "Second page says a apple too, and that is enough for recall here.",
    ]);
    let created = flow::create_job(
        &c,
        Arc::new(MockDetector::default()),
        "note.pdf",
        &pdf,
        JobOptions {
            lang: Some("en".to_string()),
            ..Default::default()
        },
    )
    .expect("PDF 简单模式必须能建 job（不依赖任何 CLI）");
    assert_eq!(created.mode, media_flow::JobMode::Simple);
    assert_eq!(
        created.preview_kind,
        PreviewKind::DocText,
        "简单模式没有像素坐标 → 只读文档视图，不给画布"
    );
    let job = wait_terminal(&c, &created.id).await;
    assert_eq!(job.stage, MediaStage::Ready, "job 失败：{:?}", job.error);
    assert_eq!(job.tool.as_deref(), Some("document-parse"));
    assert_eq!(job.extraction, "structure");
    assert_eq!(job.carrier.as_ref().unwrap().pages, Some(2), "页号必须保住");
    assert_eq!(
        job.annotations.len(),
        2,
        "两页各一处 → 两条标注：{:?}",
        job.annotations
    );
    let mut parts: Vec<String> = Vec::new();
    for a in &job.annotations {
        match &job.unit(a.unit_id).map(|u| (&a.region, u)) {
            Some((media_core::Region::RunRef { part, run_range, .. }, u)) => {
                // 落点必须是「第N页 + 该页正文的真实字符区间」——简单模式没有假像素框
                parts.push(part.clone());
                let text = u.text.clone();
                assert_eq!(
                    &text[run_range[0]..run_range[1]],
                    "a apple",
                    "落点必须落在该页正文的真实字符区间上：{run_range:?}"
                );
            }
            other => panic!("简单模式落点是段落+字符区间，不是假像素框：{other:?}"),
        }
    }
    assert_eq!(parts, vec!["第1页", "第2页"], "页号必须按载体顺序");
    assert!(job.artifacts.preview.is_none(), "只读文档视图没有位图预览可取");
    // 简单模式没有回写原语 → 必须明确拒绝并给出 remedy，而不是静默 ListOnly
    let err = flow::apply_job(&c, &created.id, Some("overlay"))
        .await
        .expect_err("简单模式不回写载体，必须报错");
    let msg = err.to_string();
    assert!(msg.contains("简单模式") && msg.contains("advanced"), "{msg}");

    let report = flow::build_report(&c, Default::default())
        .await
        .expect("正误表导出失败");
    assert_eq!(report["exported"].as_u64().unwrap(), 2, "{report}");
    let locations: Vec<&str> = report["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["location"].as_str().unwrap_or(""))
        .collect();
    assert!(
        locations.iter().any(|l| l.contains("第1页"))
            && locations.iter().any(|l| l.contains("第2页")),
        "位置必须写真实页号：{locations:?}"
    );
    let _ = std::fs::remove_dir_all(&wd);
}

/// Word 简单模式：段落号用原始序号（空段落跳过但不改号）
#[tokio::test]
async fn docx_simple_mode_labels_real_paragraph_indexes() {
    let wd = temp_dir("docx");
    let c = ctx(&wd);
    let docx = minimal_docx(&[
        ("Heading1", "Proof"),
        ("", ""),
        ("", "I saw a apple on the table here."),
    ]);
    let created = flow::create_job(
        &c,
        Arc::new(MockDetector::default()),
        "note.docx",
        &docx,
        JobOptions {
            lang: Some("en".to_string()),
            ..Default::default()
        },
    )
    .expect("docx 简单模式必须能建 job");
    let job = wait_terminal(&c, &created.id).await;
    assert_eq!(job.stage, MediaStage::Ready, "job 失败：{:?}", job.error);
    assert_eq!(job.tool.as_deref(), Some("document-parse"));
    assert!(
        job.carrier.as_ref().unwrap().pages.is_none(),
        "没有排版引擎 → 页数留空，不猜"
    );
    let hit = job
        .annotations
        .iter()
        .find(|a| a.error.original == "a apple")
        .expect("判定必须落成标注");
    match &hit.region {
        media_core::Region::RunRef { part, .. } => {
            assert_eq!(part, "段落3", "空段落跳过但段落号不能跟着漂：{part}");
        }
        other => panic!("落点必须是段落引用：{other:?}"),
    }
    let _ = std::fs::remove_dir_all(&wd);
}

/// 高级模式缺 CLI：报错必须说明「为什么不行 + 可以怎么改」，不能静默换档
#[tokio::test]
async fn advanced_mode_without_cli_says_why_and_offers_simple() {
    let wd = temp_dir("gate");
    let c = ctx(&wd);
    let err = flow::create_job(
        &c,
        Arc::new(MockDetector::default()),
        "note.pdf",
        &minimal_pdf(&["Notice: the pool closes at 6pm."]),
        JobOptions {
            mode: Some("advanced".to_string()),
            ..Default::default()
        },
    )
    .expect_err("pdfcraft-cli 不在位，高级模式必须被挡住");
    let msg = err.to_string();
    assert!(msg.contains("pdfcraft-cli"), "要点名缺哪个工具：{msg}");
    assert!(msg.contains("mode=simple"), "要给出可执行的退路：{msg}");
    let _ = std::fs::remove_dir_all(&wd);
}

/// 探测面：两种载体都要回模式卡（前端模式卡与后端 gate 同一口径）
#[tokio::test]
async fn inspect_carrier_reports_modes_for_documents() {
    let wd = temp_dir("inspect");
    let c = ctx(&wd);
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("note.pdf", minimal_pdf(&["Notice: the pool closes at 6pm."])),
        (
            "note.docx",
            minimal_docx(&[("", "Notice: the pool closes at 6pm.")]),
        ),
    ];
    for (name, bytes) in cases {
        let v = flow::inspect_carrier(&c, name, &bytes)
            .await
            .unwrap_or_else(|e| panic!("{name} 探测失败：{e}"));
        assert_eq!(v["modes"]["simple"]["available"], json!(true), "{name}: {v}");
        assert_eq!(
            v["modes"]["advanced"]["available"],
            json!(false),
            "{name}: {v}"
        );
        assert!(
            v["modes"]["advanced"]["reason"]
                .as_str()
                .unwrap_or("")
                .contains("cli"),
            "高级模式不可用必须给原因：{v}"
        );
        assert!(
            v["simple"]["text_units"].as_u64().unwrap_or(0) >= 1,
            "{name} 简单模式必须探到段落：{v}"
        );
    }
    let _ = std::fs::remove_dir_all(&wd);
}

/// 真机全链路 · 高级模式 PDF：适配器读结构层 → 落点可定位 → 回写出可编辑产物
///
/// 跑法：`LANGFIXER_TEST_PDF=/tmp/note.pdf cargo test -p media-flow --test modes -- --ignored`
/// （样本需带文本层，并含「a apple」——检测替身按它下判定）
#[tokio::test]
#[ignore = "需要本机 pdfcraft-cli 与 LANGFIXER_TEST_PDF"]
async fn pdf_advanced_end_to_end() {
    let src = PathBuf::from(
        std::env::var("LANGFIXER_TEST_PDF").expect("设置 LANGFIXER_TEST_PDF=/path/to.pdf"),
    );
    let wd = temp_dir("pdf_adv");
    let c = ctx_with(&wd, "advanced", false);
    let bytes = std::fs::read(&src).expect("读样本 PDF");
    let created = flow::create_job(
        &c,
        Arc::new(MockDetector::default()),
        "note.pdf",
        &bytes,
        JobOptions {
            lang: Some("en".to_string()),
            ..Default::default()
        },
    )
    .expect("高级模式 PDF 建 job");
    assert_eq!(created.mode, media_flow::JobMode::Advanced);
    assert_eq!(created.preview_kind, PreviewKind::ImageCanvas);
    let job = wait_terminal(&c, &created.id).await;
    assert_eq!(job.stage, MediaStage::Ready, "job 失败：{:?}", job.error);
    assert_eq!(job.tool.as_deref(), Some("pdfcraft"));
    assert_eq!(job.extraction, "structure");
    let stitched = job.stitched.as_ref().map(|s| s.text.clone()).unwrap_or_default();
    assert!(
        stitched.contains("a apple"),
        "样本必须带文本层且含「a apple」：{stitched:?}"
    );
    assert!(!job.annotations.is_empty(), "判定必须落成标注");
    assert!(
        job.annotations.iter().any(|a| a.region.is_locatable()),
        "高级模式的落点必须能画到载体上：{:?}",
        job.annotations.iter().map(|a| &a.region).collect::<Vec<_>>()
    );
    let applied = flow::apply_job(&c, &created.id, Some("overlay"))
        .await
        .expect("高级模式回写");
    assert_eq!(applied.stage, MediaStage::Applied);
    let out = applied.artifacts.editable.clone().expect("可编辑产物路径");
    assert!(std::path::Path::new(&out).exists(), "产物必须落盘：{out}");
    let _ = std::fs::remove_dir_all(&wd);
}

/// 真机全链路 · 高级模式 Word：段落块 → run 引用落点 → 修订/批注回写
///
/// 跑法：`LANGFIXER_TEST_DOCX=/tmp/in.docx cargo test -p media-flow --test modes -- --ignored`
#[tokio::test]
#[ignore = "需要本机 wordcraft-cli 与 LANGFIXER_TEST_DOCX"]
async fn docx_advanced_end_to_end() {
    let src = PathBuf::from(
        std::env::var("LANGFIXER_TEST_DOCX").expect("设置 LANGFIXER_TEST_DOCX=/path/to.docx"),
    );
    let wd = temp_dir("docx_adv");
    let c = ctx_with(&wd, "advanced", false);
    let bytes = std::fs::read(&src).expect("读样本 docx");
    let created = flow::create_job(
        &c,
        Arc::new(MockDetector::default()),
        "note.docx",
        &bytes,
        JobOptions {
            lang: Some("en".to_string()),
            ..Default::default()
        },
    )
    .expect("高级模式 docx 建 job");
    let job = wait_terminal(&c, &created.id).await;
    assert_eq!(job.stage, MediaStage::Ready, "job 失败：{:?}", job.error);
    assert_eq!(job.tool.as_deref(), Some("wordcraft"));
    let stitched = job.stitched.as_ref().map(|s| s.text.clone()).unwrap_or_default();
    assert!(
        stitched.contains("a apple"),
        "样本必须含「a apple」：{stitched:?}"
    );
    let applied = flow::apply_job(&c, &created.id, Some("inplace"))
        .await
        .expect("高级模式回写（InPlace 走修订）");
    assert_eq!(applied.stage, MediaStage::Applied);
    let out = applied.artifacts.editable.clone().expect("可编辑产物路径");
    assert!(std::path::Path::new(&out).exists(), "产物必须落盘：{out}");
    let _ = std::fs::remove_dir_all(&wd);
}



