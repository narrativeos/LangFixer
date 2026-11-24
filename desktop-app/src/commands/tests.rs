use super::optimization::report_field_matches;
use serde_json::json;

/// 回归：前端选「split 为 null 的报告组」时会显式传 split: null，
/// 旧实现把 Some(Null) 当「必须匹配字符串」→ 恒 false → 误报「无匹配测评报告」
#[test]
fn null_want_means_no_filter() {
    let r = json!({ "benchmark": "mucgec", "split": null, "arm": "cec3", "mode": "capability" });
    assert!(report_field_matches(&r, "split", Some(&json!(null))));
    assert!(report_field_matches(&r, "split", None));
    assert!(report_field_matches(&r, "absent", Some(&json!(null))));
}

#[test]
fn string_want_matches_exactly() {
    let r = json!({ "benchmark": "mucgec", "split": "dev", "arm": "cec3", "mode": "funnel" });
    assert!(report_field_matches(
        &r,
        "benchmark",
        Some(&json!("mucgec"))
    ));
    assert!(!report_field_matches(
        &r,
        "benchmark",
        Some(&json!("nacgec"))
    ));
    assert!(!report_field_matches(
        &r,
        "mode",
        Some(&json!("capability"))
    ));
    // 报告字段为 null 而 want 为具体值 → 不匹配
    let r2 = json!({ "split": null });
    assert!(!report_field_matches(&r2, "split", Some(&json!("dev"))));
}
