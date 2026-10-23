use shared_model::{WeinerDimensions, attribution_labels};

/// 决策表未给出韦纳维度时的默认映射
///
/// - 习惯固化 / 规则遗忘 / 搭配薄弱 → 内部 × 稳定 × 可控
/// - 知识缺口 / 规则缺失 → 内部 × 不稳定 × 可控
pub fn default_weiner(attribution: &str) -> WeinerDimensions {
    match attribution {
        attribution_labels::HABIT_SOLIDIFIED
        | attribution_labels::RULE_FORGOTTEN
        | attribution_labels::COLLOCATION_WEAK => {
            WeinerDimensions::new("internal", "stable", "controllable")
        }
        attribution_labels::KNOWLEDGE_GAP | attribution_labels::RULE_MISSING => {
            WeinerDimensions::new("internal", "unstable", "controllable")
        }
        _ => WeinerDimensions::new("unknown", "unknown", "unknown"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_known_labels() {
        let w = default_weiner(attribution_labels::HABIT_SOLIDIFIED);
        assert_eq!(
            (w.locus.as_str(), w.stability.as_str()),
            ("internal", "stable")
        );
        let w = default_weiner(attribution_labels::KNOWLEDGE_GAP);
        assert_eq!(w.stability, "unstable");
        let w = default_weiner("mystery");
        assert_eq!(w.locus, "unknown");
    }
}
