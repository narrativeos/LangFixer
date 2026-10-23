//! JFLEG 基准评测：对 dev+test 每条源句跑 `detect_local`，
//! 与 `datasets/jfleg_gold.json`（4 个人工修订的词级编辑，
//! 由 `datasets/make_gold.py` 预计算 span 与类别）对齐，统计
//! 按 `cat_rule`（规则可及性分类）的召回率与误报率。
//!
//! 命中定义：预测 span 与 gold 编辑 span 重叠（零宽 gold span 不计）。
use std::collections::HashMap;

use gec_en::rules::{Lexicon, detect_local};
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
    // gec-en/ 的父目录
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
}

fn overlaps(p: [usize; 2], g: [usize; 2]) -> bool {
    p[0] < g[1] && g[0] < p[1]
}

#[test]
fn jfleg_benchmark_report() {
    let lex = Lexicon::load(&repo_root().join("lexicons"));
    // 外置词表应成功加载（含过去式表）
    assert!(
        lex.past_tenses.len() >= 100,
        "外置词表加载异常: {}",
        lex.past_tenses.len()
    );

    let gold_path = repo_root().join("datasets/jfleg_gold.json");
    let json = std::fs::read_to_string(&gold_path).unwrap_or_else(|e| {
        panic!(
            "缺少 {}（先运行 datasets/make_gold.py）: {}",
            gold_path.display(),
            e
        )
    });
    let gold: Vec<GoldItem> = serde_json::from_str(&json).expect("jfleg_gold.json 解析失败");

    // (split, cat_rule) -> (命中, 总数)
    let mut per_cat: HashMap<(String, String), (usize, usize)> = HashMap::new();
    let mut n_sent: HashMap<String, usize> = HashMap::new();
    let mut n_pred: HashMap<String, usize> = HashMap::new();
    let mut n_fp: HashMap<String, usize> = HashMap::new();
    let mut total_hit = 0usize;

    for item in &gold {
        *n_sent.entry(item.split.clone()).or_default() += 1;
        let errs = detect_local(&item.src, &lex);
        *n_pred.entry(item.split.clone()).or_default() += errs.len();
        for e in &item.edits {
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
        let verbose = std::env::var("JFLEG_VERBOSE").is_ok();
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

    println!("\n=== JFLEG 基准（detect_local + 外置词表） ===");
    for split in ["dev", "test"] {
        println!(
            "[{}] 句子 {} / 预测 {} / 误报 {}",
            split,
            n_sent[split],
            n_pred.get(split).copied().unwrap_or(0),
            n_fp.get(split).copied().unwrap_or(0)
        );
        let mut sorted: Vec<(&(String, String), &(usize, usize))> = per_cat
            .iter()
            .filter(|((s, _), _)| s.as_str() == split)
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
    // 冒烟断言：至少命中一部分 gold 编辑
    assert!(total_hit > 0, "无任何命中");
}
