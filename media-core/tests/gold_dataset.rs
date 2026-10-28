//! 金集回归：`datasets/media_gold.json`（专家正误表 → 可打分基准）
//!
//! 数据来自 `scripts/media_gold_extract.sh`。这里只测**打分管道与数据契约**
//! （条目数 / 计差错数 / 匹配规则）；端到端召回率要等 B 档视觉抽取（P1）落地后
//! 用 `LANGFIXER_MEDIA_DIR` 指向原图目录再测。

use std::path::Path;

use media_core::{Annotation, ApplyMode, GoldItem, MarkStyle, Region, score_gold};
use shared_model::{Error, error_types};

fn gold_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../datasets/media_gold.json")
}

fn load_items() -> Vec<GoldItem> {
    let raw = std::fs::read_to_string(gold_path())
        .expect("datasets/media_gold.json 缺失，先跑 scripts/media_gold_extract.sh");
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    serde_json::from_value(v["items"].clone()).unwrap()
}

fn ann(id: u32, original: &str, correction: Option<&str>) -> Annotation {
    let mut e = Error::new(
        "zh",
        error_types::SPELLING,
        0.9,
        [0, original.len()],
        original,
        correction.map(str::to_string),
        "test",
    );
    e.source = Some("rule".into());
    Annotation {
        id,
        error: e,
        unit_id: 1,
        local_span: [0, original.len()],
        region: Region::LayerRef {
            layer_id: 1,
            kind: media_core::LayerKind::Type,
            bounds: None,
        },
        apply: ApplyMode::Overlay,
        style: MarkStyle::default(),
        locate_warning: None,
    }
}

#[test]
fn gold_dataset_contract() {
    let items = load_items();
    assert_eq!(items.len(), 15, "正误表共 15 项");
    assert_eq!(items.iter().filter(|g| g.counted).count(), 7, "计差错 7 项");
    assert!(items.iter().all(|g| !g.original.trim().is_empty()));
    // 判定标签分布（专家口径：差错 / 表达优化 / 表述不清）
    let mut verdicts: Vec<&str> = items.iter().map(|g| g.verdict.as_str()).collect();
    verdicts.sort_unstable();
    assert!(verdicts.contains(&"差错"));
    assert!(
        verdicts
            .iter()
            .any(|v| v.contains("表达") || v.contains("表述"))
    );
}

#[test]
fn perfect_recall_scores_one() {
    let gold = load_items();
    let anns: Vec<Annotation> = gold.iter().map(|g| ann(g.no, &g.original, None)).collect();
    let s = score_gold(&gold, &anns, 0.5);
    assert_eq!(s.matched, gold.len());
    assert!((s.recall - 1.0).abs() < 1e-9);
    assert!((s.precision - 1.0).abs() < 1e-9);
    assert!(s.unmatched_ours.is_empty());
}

#[test]
fn partial_recall_respects_weights() {
    let gold = load_items();
    // 只命中「计差错」的前 3 项 → 3/11 加权召回（7×1.0 + 8×0.5）
    let hits: Vec<Annotation> = gold
        .iter()
        .filter(|g| g.counted)
        .take(3)
        .map(|g| ann(g.no, &g.original, None))
        .collect();
    let s = score_gold(&gold, &hits, 0.5);
    assert_eq!(s.matched, 3);
    assert!(
        (s.recall - 3.0 / 11.0).abs() < 1e-9,
        "加权召回应为 3/11，实测 {}",
        s.recall
    );
}

#[test]
fn substring_hit_counts_as_contained() {
    // 专家引整句（第 9 项），我们只命中其中片段——必须算命中
    let gold = load_items();
    let item = gold
        .iter()
        .find(|g| g.original.contains("请买下午15点以后的车票"))
        .expect("金集应含第 9 项");
    let s = score_gold(
        std::slice::from_ref(item),
        &[ann(1, "下午15点", Some("15点"))],
        0.5,
    );
    assert_eq!(s.matched, 1);
    assert_eq!(s.per_gold[0].2, media_core::MatchKind::Contained);
}

#[test]
fn unrelated_output_scores_zero() {
    let gold = load_items();
    let s = score_gold(&gold, &[ann(1, "完全无关的一句话", None)], 0.5);
    assert_eq!(s.matched, 0);
    assert!((s.recall - 0.0).abs() < 1e-9);
    assert_eq!(s.unmatched_ours, vec![1]);
}
