//! 契约镜像：宿主侧登记表（`gec_zh::studio::HOST_METRICS`）与 vendored 契约
//! （`contract/studio-metrics.v1.json`，源 narrative-docs）必须逐项一致。
//! 契约加了 owner=langfixer 的 id 而登记表没登记、登记表登记了而契约没有、
//! grade / owner / via_sidecar 漂移——都会在这里变红，先于任何跨仓集成事故。
//!
//! 注意守卫的是**归属**而非检测器本身：登记表对应检测器的落地可以逐步进行，
//! 登记的先行不等于已实现，这是契约表与实现进度的刻意解耦。

use std::collections::BTreeMap;

use serde_json::Value;

fn contract() -> Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../contract/studio-metrics.v1.json");
    let raw = std::fs::read_to_string(path).unwrap_or_else(|e| {
        panic!("无法读取 vendored 契约 {path}: {e}（用 scripts/sync-studio-contract.sh 刷新）")
    });
    serde_json::from_str(&raw).expect("vendored 契约必须是合法 JSON")
}

#[test]
fn contract_version_parses() {
    let c = contract();
    let v = c["contract_version"]
        .as_str()
        .expect("contract_version 应为字符串");
    assert!(v.starts_with("1."), "意外的契约主版本: {v}");
}

#[test]
fn registry_matches_contract_langfixer_slice() {
    let c = contract();
    let metrics = c["metrics"].as_object().expect("metrics 总表");
    let from_contract: BTreeMap<&str, (&str, bool)> = metrics
        .iter()
        .filter(|(_, d)| d["owner"].as_str() == Some("langfixer"))
        .map(|(id, d)| {
            let grade = d["grade"]
                .as_str()
                .unwrap_or_else(|| panic!("owner=langfixer 条目 {id} 必须带 grade"));
            let via = d
                .get("via_sidecar")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            (id.as_str(), (grade, via))
        })
        .collect();
    let from_registry: BTreeMap<&str, &str> =
        gec_zh::studio::HOST_METRICS.iter().copied().collect();

    assert!(!from_contract.is_empty(), "契约里 owner=langfixer 切片为空——契约被误改或 vendored 副本漂移");
    assert_eq!(from_contract.len(), 16, "契约 langfixer 切片条目数变了（契约次版本应已递增）");
    let missing: Vec<&str> = from_contract
        .keys()
        .copied()
        .filter(|id| !from_registry.contains_key(id))
        .collect();
    let extra: Vec<&str> = from_registry
        .keys()
        .copied()
        .filter(|id| !from_contract.contains_key(id))
        .collect();
    assert!(missing.is_empty(), "契约有而登记表缺的 id: {missing:?}");
    assert!(extra.is_empty(), "登记表有而契约缺的 id: {extra:?}");

    for (id, (grade, via)) in &from_contract {
        let reg_grade = from_registry[id];
        assert_eq!(reg_grade, *grade, "{id} 的 grade 漂移（登记表 {reg_grade} vs 契约 {grade}）");
        assert!(!via, "owner=langfixer 的 {id} 不得经 sidecar（via_sidecar 必须为 false）");
    }
}

#[test]
fn studio_owned_ids_stay_out_of_the_registry() {
    let c = contract();
    let metrics = c["metrics"].as_object().expect("metrics 总表");
    for (id, def) in metrics {
        if def["owner"].as_str() == Some("studio") {
            assert!(
                !gec_zh::studio::HOST_METRICS.iter().any(|(i, _)| i == id),
                "studio 的 {id} 混入了宿主登记表"
            );
        }
    }
}

#[test]
fn no_verdicts_rule_present() {
    let c = contract();
    let rule = c["wireRules"]["noVerdicts"]
        .as_str()
        .expect("wireRules.noVerdicts 必须存在");
    assert!(rule.contains("不回判断文案"), "noVerdicts 规则漂移: {rule}");
}
