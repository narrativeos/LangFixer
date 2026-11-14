//! 文档级 eval harness（词典信号版）：与 gec-core `doc_eval_synthetic_mucgec`
//! 同构的三臂评测，但 triage 注册了 **gec-zh 词典信号**（`detect_local`
//! 命中密度）——回答「规则预筛 + 采样地板后，economical 的召回-成本曲线」。
//!
//! 背景：纯统计信号对 mucgec 的语义级错误近乎全盲（economical 召回 0.13，
//! 见 datasets/eval_reports/doc_eval_latest.json）；本 harness 输出
//! datasets/eval_reports/doc_eval_zh_latest.json 供阈值校准。
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, OnceLock};

use gec_core::{
    ChunkRisk, DocBudget, DocJobStore, LlmDetectConfig, LlmGecEngine, run_doc_job, triage,
};
use shared_model::Error;

static GOLD: OnceLock<Vec<String>> = OnceLock::new();

fn config(base_url: &str) -> LlmDetectConfig {
    LlmDetectConfig::from_parts(
        base_url,
        "sk-test",
        "mock-model",
        5,
        "你是校对专家",
        "找出以下文本中的错误：{{text}}",
    )
    .with_retry(Vec::new(), 0)
}

/// 动态 mock（与 gec-core docjob 测试同款）：`respond(user)` None → 400
async fn mock_server(respond: fn(&str) -> Option<String>) -> String {
    use axum::response::IntoResponse;
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(move |body: axum::body::Bytes| async move {
            let req: serde_json::Value = serde_json::from_slice(&body).unwrap();
            let user = req["messages"][1]["content"]
                .as_str()
                .unwrap_or("")
                .to_string();
            match respond(&user) {
                Some(content) => {
                    let resp = serde_json::json!({
                        "choices": [{ "message": { "content": content } }]
                    });
                    (axum::http::StatusCode::OK, axum::Json(resp)).into_response()
                }
                None => axum::http::StatusCode::BAD_REQUEST.into_response(),
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    format!("http://{addr}/v1")
}

/// 词典信号（与 api-server 启动注入同款公式）：detect_local 命中密度
fn lexicon_signal(lex: gec_zh::rules::Lexicon) -> impl Fn(&str) -> ChunkRisk + Send + Sync + 'static {
    move |t: &str| {
        let hits = gec_zh::rules::detect_local(t, &lex).len();
        if hits == 0 {
            ChunkRisk::default()
        } else {
            ChunkRisk {
                score: (0.4 + 0.1 * hits as f64).min(0.7),
                signals: vec![format!("lexicon_hit:{hits}")],
            }
        }
    }
}

fn load_sents() -> (Vec<String>, Vec<String>) {
    let raw = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../datasets/mucgec_gold.json"
    ))
    .expect("mucgec gold 缺失");
    let items: Vec<serde_json::Value> = serde_json::from_str(&raw).unwrap();
    let ok_len = |s: &str| s.chars().count() >= 8 && s.chars().count() <= 60;
    let mut gold: Vec<String> = Vec::new();
    let mut clean: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = Default::default();
    for it in &items {
        if it["split"] != "dev" {
            continue;
        }
        let s = it["src"].as_str().unwrap_or("").trim().to_string();
        if !ok_len(&s) || s.contains('\n') || !seen.insert(s.clone()) {
            continue;
        }
        let has_edits = it["edits"].as_array().is_some_and(|a| !a.is_empty());
        if has_edits && gold.len() < 60 {
            gold.push(s);
        } else if !has_edits && clean.len() < 50 {
            clean.push(s);
        }
    }
    gold.retain(|s| !clean.iter().any(|o| o.contains(s.as_str())));
    clean.retain(|s| !gold.iter().any(|o| o.contains(s.as_str())));
    (gold, clean)
}


#[tokio::test]
async fn doc_eval_with_lexicon_signal() {
    let lex = gec_zh::rules::Lexicon::load(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../lexicons"),
    );
    let _ = triage::set_extra_signal(Arc::new(lexicon_signal(lex)));

    let (gold, clean) = load_sents();
    assert!(gold.len() >= 50 && clean.len() >= 30, "gold 集供给不足");
    GOLD.set(gold.clone()).ok();

    let mut doc = String::new();
    let mut gold_ranges: Vec<[usize; 2]> = Vec::new();
    for (i, s) in gold.iter().enumerate() {
        let st = doc.len();
        doc.push_str(s);
        doc.push('\n');
        doc.push_str(&clean[i % clean.len()]);
        doc.push('\n');
        gold_ranges.push([st, st + s.len()]);
    }

    // 理想复查 mock：送端文本含 gold 句即报该句（所有权 remap 负责去重）
    let respond: fn(&str) -> Option<String> = |user| {
        let mut arr: Vec<serde_json::Value> = Vec::new();
        for s in GOLD.get()? {
            if user.contains(s.as_str()) {
                arr.push(serde_json::json!({
                    "error_type": "grammar",
                    "confidence": 0.9,
                    "original": s,
                    "correction": "（gold 探针）",
                    "message": "doc_eval gold probe",
                }));
            }
        }
        Some(serde_json::to_string(&arr).unwrap())
    };
    let endpoint = mock_server(respond).await;
    let llm = Arc::new(LlmGecEngine::new("zh", config(&endpoint)));
    let no_cancel = || Arc::new(AtomicBool::new(false));

    let metrics = |errs: &[Error]| -> (usize, usize) {
        let mut hits = 0usize;
        let mut fp = 0usize;
        for r in &gold_ranges {
            if errs.iter().any(|e| e.span[0] < r[1] && r[0] < e.span[1]) {
                hits += 1;
            }
        }
        for e in errs {
            if !gold_ranges.iter().any(|r| e.span[0] < r[1] && r[0] < e.span[1]) {
                fp += 1;
            }
        }
        (hits, fp)
    };

    // 臂 A：full（cache 预热）
    let store_a = Arc::new(DocJobStore::open_in_memory().unwrap());
    store_a.create_job("full", "zh", &doc, None, 400).unwrap();
    let (pa, ea) = run_doc_job(
        store_a.clone(),
        llm.clone(),
        "full",
        8,
        true,
        DocBudget::All,
        None,
        no_cancel(),
    )
    .await
    .unwrap();
    let (hits_a, fp_a) = metrics(&ea);

    // 臂 B：economical（词典信号 + 采样地板，默认 fast 阈值）
    let store_b = Arc::new(DocJobStore::open_in_memory().unwrap());
    store_b.create_job("econ", "zh", &doc, None, 400).unwrap();
    let (pb, eb) = run_doc_job(
        store_b.clone(),
        llm.clone(),
        "econ",
        8,
        false,
        DocBudget::SkipLowRisk,
        None,
        no_cancel(),
    )
    .await
    .unwrap();
    let (hits_b, fp_b) = metrics(&eb);
    // 阈值扫描：fast_threshold 越高 → 跳得越多、成本越低、召回应单调下降
    let mut sweep = Vec::new();
    for th in [0.2, 0.4, 0.6] {
        let store = Arc::new(DocJobStore::open_in_memory().unwrap());
        store.create_job("sweep", "zh", &doc, None, 400).unwrap();
        let (p, es) = run_doc_job(
            store.clone(),
            llm.clone(),
            "sweep",
            8,
            false,
            DocBudget::SkipLowRisk,
            Some(th),
            no_cancel(),
        )
        .await
        .unwrap();
        let (h, f) = metrics(&es);
        sweep.push(serde_json::json!({
            "fast_threshold": th,
            "coverage": p.coverage(),
            "recall": h as f64 / gold_ranges.len() as f64,
            "skipped": p.skipped,
            "llm_calls": p.done + p.cached,
            "false_positives": f,
        }));
    }

    // 臂 C：修订文档（复制一句 → 受影响块重跑，其余命中缓存）
    let dup = clean[7].clone();
    let revised = format!("{doc}{dup}\n");
    store_a.create_job("rev", "zh", &revised, None, 400).unwrap();
    let (pc, ec) = run_doc_job(
        store_a.clone(),
        llm.clone(),
        "rev",
        8,
        true,
        DocBudget::All,
        None,
        no_cancel(),
    )
    .await
    .unwrap();
    let (hits_c, fp_c) = metrics(&ec);
    let cache_hit_rate = if pc.cached + pc.done > 0 {
        pc.cached as f64 / (pc.cached + pc.done) as f64
    } else {
        0.0
    };

    let n_gold = gold_ranges.len();
    let report = serde_json::json!({
        "doc_chars": doc.chars().count(),
        "chunks": pa.total,
        "gold_sentences": n_gold,
        "signal": "lexicon_hit(detect_local) + statistical",
        "sample_every": 3,
        "full": {
            "coverage": pa.coverage(),
            "recall": hits_a as f64 / n_gold as f64,
            "false_positives": fp_a,
            "llm_calls": pa.done,
        },
        "economical": {
            "coverage": pb.coverage(),
            "recall": hits_b as f64 / n_gold as f64,
            "false_positives": fp_b,
            "skipped": pb.skipped,
            "llm_calls": pb.done + pb.cached,
        },
        "revised": {
            "recall": hits_c as f64 / n_gold as f64,
            "false_positives": fp_c,
            "cached": pc.cached,
            "done": pc.done,
            "cache_hit_rate": cache_hit_rate,
        },
        "sweep": sweep,
    });
    let out_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../datasets/eval_reports");
    std::fs::create_dir_all(out_dir).ok();
    std::fs::write(
        format!("{out_dir}/doc_eval_zh_latest.json"),
        serde_json::to_string_pretty(&report).unwrap(),
    )
    .ok();

    // 回归阈值
    assert_eq!(pa.coverage(), 1.0);
    assert_eq!(hits_a, n_gold, "full 必须零漏检: {report}");
    assert_eq!(fp_a, 0);
    // 词典信号 + 地板：economical 召回必须显著高于纯统计基线（0.13）——
    // 地板给 ≈1/3 下限，词典信号在其上叠加，取 0.35 为回归线
    assert!(
        hits_b as f64 / n_gold as f64 >= 0.35,
        "economical（词典信号+地板）召回过低: {report}"
    );
    assert!(pb.skipped >= 1, "economical 应有跳过: {report}");
    assert!(pb.coverage() < 1.0);
    assert_eq!(fp_b, 0);
    // 阈值扫描单调：th 增大时 llm_calls 不减、召回不增
    for w in sweep.windows(2) {
        assert!(
            w[1]["llm_calls"].as_u64().unwrap() <= w[0]["llm_calls"].as_u64().unwrap(),
            "阈值升高成本应下降: {report}"
        );
        assert!(
            w[1]["recall"].as_f64().unwrap() <= w[0]["recall"].as_f64().unwrap() + 1e-9,
            "阈值升高召回不应上升: {report}"
        );
    }
    // revised：缓存命中且零漏检
    assert!(cache_hit_rate >= 0.5, "修订文档缓存命中过低: {report}");
    assert_eq!(hits_c, n_gold, "缓存 remap 不应丢 gold: {report}");
    assert_eq!(fp_c, 0);
}
