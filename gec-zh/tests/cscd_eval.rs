//! CEC 评测基准（CSCD / NaCGEC）规则层评测：对每条源句跑 `detect_local`，
//! 与 `datasets/cscd_gold.json`（CSCD + SIGHAN-2015/EC-Med/EC-LAW 泛化性
//! 测试集，由 `datasets/prepare_cscd.py` 生成）和 `datasets/nacgec_gold.json`
//!（NaCGEC 语法基准，由 `datasets/prepare_nacgec.py` 生成）对齐，统计按
//! `cat_rule`（规则可及性分类）的召回率与误报率。
//!
//! 命中定义与 mucgec_eval 一致：预测 span 与 gold 编辑 span 重叠（零宽 gold
//! span 不计）。LLM/L3 路径的 F0.5/F1 评测用 `datasets/score_f05.py`。
use std::collections::HashMap;

use gec_zh::rules::{Lexicon, detect_local};
use serde::Deserialize;

#[derive(Deserialize)]
struct GoldItem {
    split: String,
    src: String,
    edits: Vec<GoldEdit>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct GoldEdit {
    op: char,
    #[serde(rename = "src")]
    src_word: String,
    #[serde(rename = "ref")]
    ref_word: String,
    span: [usize; 2],
    #[serde(default)]
    cat: String,
    #[serde(default)]
    cat_rule: String,
}

fn repo_root() -> &'static std::path::Path {
    // gec-zh/ 的父目录
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
}

fn overlaps(p: [usize; 2], g: [usize; 2]) -> bool {
    p[0] < g[1] && g[0] < p[1]
}

/// 通用评测：加载 gold、逐句跑 detect_local、按 (split, cat_rule) 统计召回与误报。
/// 返回 (总命中数, 总 gold 编辑数)。
fn run_eval(gold_name: &str, verbose_env: &str) -> (usize, usize) {
    let lex = Lexicon::load(&repo_root().join("lexicons"));
    // 外置词表应成功加载
    assert!(
        !lex.confusables.is_empty(),
        "外置词表加载异常：confusables 为空"
    );

    let gold_path = repo_root().join(format!("datasets/{gold_name}"));
    let json = std::fs::read_to_string(&gold_path).unwrap_or_else(|e| {
        panic!(
            "缺少 {}（先运行 datasets/prepare_cscd.py / prepare_nacgec.py）: {}",
            gold_path.display(),
            e
        )
    });
    let gold: Vec<GoldItem> = serde_json::from_str(&json).expect("gold 解析失败");
    assert!(!gold.is_empty(), "{gold_name} 为空");

    // (split, cat_rule) -> (命中, 总数)
    let mut per_cat: HashMap<(String, String), (usize, usize)> = HashMap::new();
    let mut n_sent: HashMap<String, usize> = HashMap::new();
    let mut n_pred: HashMap<String, usize> = HashMap::new();
    let mut n_fp: HashMap<String, usize> = HashMap::new();
    let verbose = std::env::var(verbose_env).is_ok();
    let mut total_hit = 0usize;
    let mut total_edit = 0usize;

    for item in &gold {
        *n_sent.entry(item.split.clone()).or_default() += 1;
        let errs = detect_local(&item.src, &lex);
        *n_pred.entry(item.split.clone()).or_default() += errs.len();
        for e in &item.edits {
            total_edit += 1;
            let entry = per_cat
                .entry((item.split.clone(), e.cat_rule.clone()))
                .or_insert((0, 0));
            entry.1 += 1;
            // 零宽 gold span（纯插入）无法用 span 重叠判定，跳过
            if e.span[1] > e.span[0]
                && errs
                    .iter()
                    .any(|p| p.span[1] > p.span[0] && overlaps(p.span, e.span))
            {
                entry.0 += 1;
                total_hit += 1;
            }
        }
        // 误报：未命中任何 gold 编辑的预测
        for p in &errs {
            if p.span[1] > p.span[0]
                && !item
                    .edits
                    .iter()
                    .any(|g| g.span[1] > g.span[0] && overlaps(p.span, g.span))
            {
                *n_fp.entry(item.split.clone()).or_default() += 1;
                if verbose {
                    println!(
                        "  [FP {}] {} | '{}' -> {:?} | {}",
                        item.split, p.error_type, p.original, p.correction, item.src
                    );
                }
            }
        }
    }

    println!("\n=== {gold_name}（detect_local + 外置词表） ===");
    let mut splits: Vec<&String> = n_sent.keys().collect();
    splits.sort();
    for split in splits {
        println!(
            "[{}] 句子 {} / 预测 {} / 误报 {}",
            split,
            n_sent[split],
            n_pred.get(split).copied().unwrap_or(0),
            n_fp.get(split).copied().unwrap_or(0)
        );
        let mut sorted: Vec<(&(String, String), &(usize, usize))> = per_cat
            .iter()
            .filter(|((s, _), _)| s.as_str() == split.as_str())
            .collect();
        sorted.sort_by(|a, b| b.1.1.cmp(&a.1.1));
        for ((_, cat), (hit, total)) in sorted {
            println!(
                "  {:<12} 召回 {}/{} = {:.1}%",
                cat,
                hit,
                total,
                100.0 * (*hit as f64) / (*total as f64)
            );
        }
    }
    (total_hit, total_edit)
}

#[test]
fn cscd_benchmark_report() {
    let (hit, total) = run_eval("cscd_gold.json", "CSCD_VERBOSE");
    // 冒烟断言：gold 非空且规则层至少命中一条（CSCD 拼写基准）
    assert!(total > 0, "CSCD gold 无编辑");
    assert!(hit > 0, "CSCD 无任何命中");
}

#[test]
fn nacgec_benchmark_report() {
    let (hit, total) = run_eval("nacgec_gold.json", "NACGEC_VERBOSE");
    // NaCGEC 以语法编辑（other）为主，规则层命中预期低；只断言 gold 完整
    assert!(total > 0, "NaCGEC gold 无编辑");
    let _ = hit;
}
