//! gec-eval — 规则层离线评测 harness（P0 评测基准与 CI 回归门）
//!
//! 对金集（`datasets/*_gold.json`，统一 schema：`split / src / edits[]`）逐句跑
//! 规则层 `detect_local`（离线、确定性、无 LLM），统计：
//!
//! - **precision / recall / F0.5**（GEC 评测惯例，β=0.5 偏重精确率——纠错产品
//!   误报比漏报更伤用户信任）
//! - **延迟**：每千字符中位数毫秒（release 构建）
//!
//! 命中定义与 `gec-zh/tests/cscd_eval.rs` 一致：预测 span 与 gold 编辑 span
//! 重叠（零宽 gold span 不计，进分母）。
//!
//! 用法：
//! - `cargo run --release -p gec-eval` → 打印 markdown 报告
//! - `--json <out.json>` → 写机器可读报告
//! - `--check <baseline.json> [--tolerance 0.02]` → 与基线对比，任一数据集
//!   precision/recall/F0.5 下降超过容差即退出码 1（CI 回归门）

use std::path::Path;
use std::time::Instant;

use serde::{Deserialize, Serialize};

/// 金集条目（统一 schema，见 datasets/make_gold*.py）
#[derive(Deserialize)]
struct GoldItem {
    src: String,
    edits: Vec<GoldEdit>,
}

#[derive(Deserialize)]
struct GoldEdit {
    span: [usize; 2],
}

/// 单数据集指标
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetMetrics {
    pub name: String,
    pub lang: String,
    pub sentences: usize,
    pub gold_edits: usize,
    pub predictions: usize,
    pub hits: usize,
    pub false_positives: usize,
    pub precision: f64,
    pub recall: f64,
    pub f05: f64,
    /// 每千字符检测耗时中位数（ms，release 构建）
    pub latency_ms_per_1k_chars: f64,
}

/// 完整评测报告
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalReport {
    pub version: u32,
    pub engine: String,
    pub datasets: Vec<DatasetMetrics>,
}

fn overlaps(p: [usize; 2], g: [usize; 2]) -> bool {
    p[0] < g[1] && g[0] < p[1]
}

/// F0.5（GEC 惯例：β=0.5，精确率权重是召回的 2 倍）
fn f_beta(p: f64, r: f64, beta: f64) -> f64 {
    if p + r == 0.0 {
        return 0.0;
    }
    let b2 = beta * beta;
    (1.0 + b2) * p * r / (b2 * p + r)
}

/// 单数据集评测：加载金集 → 逐句 detect → 指标 + 延迟中位数
fn evaluate(
    name: &str,
    lang: &str,
    gold_path: &Path,
    detect: &dyn Fn(&str) -> Vec<shared_model::Error>,
) -> DatasetMetrics {
    let json = std::fs::read_to_string(gold_path).unwrap_or_else(|e| {
        panic!(
            "缺少金集 {}（见 datasets/README.md）: {}",
            gold_path.display(),
            e
        )
    });
    let gold: Vec<GoldItem> = serde_json::from_str(&json).expect("金集解析失败");
    assert!(!gold.is_empty(), "{} 为空", gold_path.display());

    // 预热（词表/页缓存），避免首句冷启动污染延迟
    for item in gold.iter().take(20) {
        let _ = detect(&item.src);
    }

    let mut hits = 0usize;
    let mut gold_edits = 0usize;
    let mut predictions = 0usize;
    let mut false_positives = 0usize;
    let mut latencies: Vec<f64> = Vec::with_capacity(gold.len());

    for item in &gold {
        let t0 = Instant::now();
        let errs = detect(&item.src);
        let chars = item.src.chars().count().max(1);
        latencies.push(t0.elapsed().as_secs_f64() * 1000.0 / chars as f64 * 1000.0);

        predictions += errs.len();
        for e in &item.edits {
            gold_edits += 1;
            if e.span[1] > e.span[0]
                && errs
                    .iter()
                    .any(|p| p.span[1] > p.span[0] && overlaps(p.span, e.span))
            {
                hits += 1;
            }
        }
        for p in &errs {
            if p.span[1] > p.span[0]
                && !item
                    .edits
                    .iter()
                    .any(|g| g.span[1] > g.span[0] && overlaps(p.span, g.span))
            {
                false_positives += 1;
            }
        }
    }

    let precision = if predictions > 0 {
        (predictions - false_positives) as f64 / predictions as f64
    } else {
        0.0
    };
    let recall = if gold_edits > 0 {
        hits as f64 / gold_edits as f64
    } else {
        0.0
    };
    latencies.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = latencies[latencies.len() / 2];

    DatasetMetrics {
        name: name.into(),
        lang: lang.into(),
        sentences: gold.len(),
        gold_edits,
        predictions,
        hits,
        false_positives,
        precision,
        recall,
        f05: f_beta(precision, recall, 0.5),
        latency_ms_per_1k_chars: (median * 100.0).round() / 100.0,
    }
}

fn repo_root() -> std::path::PathBuf {
    // tools/gec-eval/ 的上两级 = 仓库根
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .map(Into::into)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

fn run_all(root: &std::path::Path) -> EvalReport {
    let lex_zh = gec_zh::rules::Lexicon::load(&root.join("lexicons"));
    assert!(
        !lex_zh.confusables.is_empty(),
        "中文外置词表加载异常：confusables 为空"
    );
    let lex_en = gec_en::rules::Lexicon::load(&root.join("lexicons"));

    let gold = |name: &str| root.join(format!("datasets/{name}_gold.json"));
    EvalReport {
        version: 1,
        engine: "rules-offline".into(),
        datasets: vec![
            evaluate("cscd", "zh", &gold("cscd"), &|t| {
                gec_zh::rules::detect_local(t, &lex_zh)
            }),
            evaluate("mucgec", "zh", &gold("mucgec"), &|t| {
                gec_zh::rules::detect_local(t, &lex_zh)
            }),
            evaluate("nacgec", "zh", &gold("nacgec"), &|t| {
                gec_zh::rules::detect_local(t, &lex_zh)
            }),
            evaluate("jfleg", "en", &gold("jfleg"), &|t| {
                gec_en::rules::detect_local(t, &lex_en)
            }),
        ],
    }
}

fn print_markdown(report: &EvalReport) {
    println!(
        "| 数据集 | 语言 | 句子 | gold 编辑 | 预测 | 命中 | 误报 | 精确率 | 召回率 | F0.5 | 延迟 ms/千字符 |"
    );
    println!("|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|");
    for d in &report.datasets {
        println!(
            "| {} | {} | {} | {} | {} | {} | {} | {:.1}% | {:.1}% | {:.3} | {} |",
            d.name,
            d.lang,
            d.sentences,
            d.gold_edits,
            d.predictions,
            d.hits,
            d.false_positives,
            d.precision * 100.0,
            d.recall * 100.0,
            d.f05,
            d.latency_ms_per_1k_chars,
        );
    }
}

/// 与基线对比：任一数据集 precision/recall/F0.5 下降超过容差 → 失败
fn check(report: &EvalReport, baseline_path: &Path, tolerance: f64) -> bool {
    let json = match std::fs::read_to_string(baseline_path) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("无法读取基线 {}: {}", baseline_path.display(), e);
            return false;
        }
    };
    let baseline: EvalReport = match serde_json::from_str(&json) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("基线解析失败: {e}");
            return false;
        }
    };
    let mut ok = true;
    for base in &baseline.datasets {
        let Some(cur) = report.datasets.iter().find(|d| d.name == base.name) else {
            eprintln!("❌ 基线中的数据集 {} 未出现在本次评测", base.name);
            ok = false;
            continue;
        };
        for (metric, cur_v, base_v) in [
            ("precision", cur.precision, base.precision),
            ("recall", cur.recall, base.recall),
            ("f05", cur.f05, base.f05),
        ] {
            if cur_v + tolerance < base_v {
                eprintln!(
                    "❌ {}/{} 回归: {metric} {:.3} → {:.3}（容差 {tolerance}）",
                    cur.lang, cur.name, base_v, cur_v
                );
                ok = false;
            }
        }
    }
    if ok {
        println!("✅ 评测回归门通过（容差 {tolerance}）");
    }
    ok
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut json_out: Option<String> = None;
    let mut check_base: Option<String> = None;
    let mut tolerance = 0.02f64;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => {
                json_out = args.get(i + 1).cloned();
                i += 2;
            }
            "--check" => {
                check_base = args.get(i + 1).cloned();
                i += 2;
            }
            "--tolerance" => {
                tolerance = args.get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(0.02);
                i += 2;
            }
            _ => i += 1,
        }
    }

    let root = repo_root();
    let report = run_all(&root);
    print_markdown(&report);

    if let Some(path) = &json_out {
        let full = root.join(path);
        std::fs::write(&full, serde_json::to_string_pretty(&report).unwrap())
            .unwrap_or_else(|e| panic!("写报告失败 {}: {e}", full.display()));
        println!("报告已写入 {}", full.display());
    }

    if let Some(base) = &check_base {
        let full = root.join(base);
        if !check(&report, &full, tolerance) {
            std::process::exit(1);
        }
    }
}
