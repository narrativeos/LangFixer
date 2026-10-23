//! 层级贡献测量（进化中心 P3）：消融 Δ 常态化
//!
//! 测评中心（P0）的消融臂（`ablation: {decision: false} / {l3: false} /
//! 两者皆关`）逐臂各跑一份报告。本模块把**同一（benchmark, split, arm, mode）
//! 下的多份报告**聚成一张**层级贡献表**：每层的边际贡献 =
//! `Fβ(全开) − Fβ(该层关闭)`——即「这一层为最终指标净贡献了多少」。
//!
//! 纯派生分析：不新跑测评，只消费已有报告（`GET /eval/reports` +
//! `GET /eval/report/:id` 的响应体）。缺某消融臂时该层标记 `missing`，
//! 不阻断（优雅降级，与 P1 反思中心的测评证据降级一致）。
//!
//! 本模块保持纯数据 + 纯函数（不依赖存储 / 网络）；
//! API 组装（拉取报告、过滤、落表）见 `api-server/src/routes/optimization.rs`。

use serde::{Deserialize, Serialize};

/// 可消融层（与测评服务 `abl_fp` 的键一致）
pub mod ablation_layer {
    /// L3 门控层（feature / decision / statistical 模型层）
    pub const L3: &str = "l3";
    /// 决策模型层
    pub const DECISION: &str = "decision";
    /// 知识层（知识向量召回 ②.5 + 知识库富化；`knowledge_enabled=false`）
    pub const KNOWLEDGE: &str = "knowledge";
}

/// 拓扑假象判定：自身零定案（TP=0）却 Δ 超过该值 → 标记为门控拓扑效应
/// （与「大 Δ」口径一致的可调常量，勿散写魔数）
pub const TOPOLOGICAL_DELTA_MIN: f64 = 0.05;

/// 消融规格（测评报告 `ablation` 字段：`None` = 全开基线）
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AblationSpec {
    /// L3 门控关闭
    pub l3_off: bool,
    /// 决策模型关闭
    pub decision_off: bool,
    /// 知识层关闭（向量召回 + 知识库富化 + 数据驱动归点）
    pub knowledge_off: bool,
}

impl AblationSpec {
    /// 从测评报告 JSON 的 `ablation` 字段解析（`null` / 缺失 → `None`）
    pub fn from_report_value(v: &serde_json::Value) -> Option<AblationSpec> {
        let obj = v.as_object()?;
        let l3_off = obj.get("l3").and_then(|x| x.as_bool()) == Some(false);
        let decision_off = obj.get("decision").and_then(|x| x.as_bool()) == Some(false);
        let knowledge_off = obj.get("knowledge").and_then(|x| x.as_bool()) == Some(false);
        if l3_off || decision_off || knowledge_off {
            Some(Self {
                l3_off,
                decision_off,
                knowledge_off,
            })
        } else {
            None
        }
    }
}

/// 单份测评报告摘要（贡献测量所需的最小字段集）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContributionReportIn {
    pub report_id: String,
    /// 报告时间（UTC ISO8601；同规格多份时取最新）
    pub ts: String,
    pub benchmark: String,
    #[serde(default)]
    pub split: Option<String>,
    pub arm: String,
    /// `None` = 全开基线
    #[serde(default)]
    pub ablation: Option<AblationSpec>,
    /// 总体 Fβ（严格）
    pub f_beta: f64,
    /// 严格预测数
    pub n_pred: i64,
    /// 严格 FP
    pub fp_strict: i64,
    /// 严格召回
    pub r_strict: f64,
    /// LLM 触达率（funnel 报告有值）
    #[serde(default)]
    pub llm_reach: Option<f64>,
    /// 各层严格 TP（报告 `layers` 的 `tp_strict`；层名 = 报告层名，如 `feature`）
    #[serde(default)]
    pub layer_tp: std::collections::BTreeMap<String, i64>,
}

/// 单层贡献（边际：全开 Fβ − 该层关闭 Fβ）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayerContribution {
    /// 层名（[`ablation_layer::L3`] / [`ablation_layer::DECISION`]）
    pub layer: String,
    /// 全开基线 Fβ
    pub f_beta_full: f64,
    /// 该层关闭时的 Fβ
    pub f_beta_ablated: f64,
    /// 边际贡献（= `f_beta_full − f_beta_ablated`；正 = 该层净增益）
    pub delta: f64,
    /// 该层**自己定案**的 TP 数（全开报告中该层 `tp_strict`；0 = 自身零定案）
    #[serde(default)]
    pub tp_self: i64,
    /// 拓扑假象标记：自身零定案却有大 Δ——Δ 反映的是「该层作为下游
    /// 门控/开关」的拓扑效应（关闭它连带改变了下游层的执行），而非该层
    /// 自身的解决能力。解读贡献表时此类 Δ 不应记在该层名下。
    #[serde(default)]
    pub topological: bool,
}

/// 层级贡献表（一次测量的完整产出）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContributionTable {
    pub benchmark: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub split: Option<String>,
    pub arm: String,
    /// 全开基线 Fβ（`None` = 输入中无全开报告）
    pub f_beta_full: Option<f64>,
    /// 核心基线 Fβ（两层皆关；`None` = 无该消融报告）
    pub f_beta_core: Option<f64>,
    /// 各层贡献（按 delta 降序；缺消融报告的层不在其中，见 `missing`）
    pub layers: Vec<LayerContribution>,
    /// 缺失的消融臂（层名列表：无对应报告，无法计算该层贡献）
    pub missing: Vec<String>,
    /// 参与测量的报告数
    pub reports: usize,
}

fn fnum(v: &serde_json::Value, key: &str) -> f64 {
    v.get(key).and_then(|x| x.as_f64()).unwrap_or(0.0)
}

fn inum(v: &serde_json::Value, key: &str) -> i64 {
    v.get(key).and_then(|x| x.as_i64()).unwrap_or(0)
}

/// 从测评报告 JSON（`GET /eval/report/:id` 响应体）提取摘要
///
/// 结构不符（缺 `overall`）返回 `None`。
pub fn parse_report_summary(v: &serde_json::Value) -> Option<ContributionReportIn> {
    let overall = v.get("overall")?;
    let split = v
        .get("split")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string());
    Some(ContributionReportIn {
        report_id: v
            .get("id")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        ts: v
            .get("ts")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        benchmark: v
            .get("benchmark")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        split,
        arm: v
            .get("arm")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        ablation: AblationSpec::from_report_value(
            v.get("ablation").unwrap_or(&serde_json::Value::Null),
        ),
        f_beta: fnum(overall, "f_beta"),
        n_pred: inum(overall, "n_pred"),
        fp_strict: inum(overall, "fp_strict"),
        r_strict: fnum(overall, "r_strict"),
        llm_reach: v
            .get("funnel")
            .and_then(|f| f.get("llm_reach"))
            .and_then(|x| x.as_f64()),
        layer_tp: v
            .get("layers")
            .and_then(|l| l.as_object())
            .map(|m| {
                m.iter()
                    .filter_map(|(name, st)| {
                        st.get("tp_strict")
                            .and_then(|x| x.as_i64())
                            .map(|tp| (name.clone(), tp))
                    })
                    .collect()
            })
            .unwrap_or_default(),
    })
}

/// 同规格（ablation 相同）多份报告 → 取 `ts` 最新一份
fn latest_of<'a>(
    reports: &'a [ContributionReportIn],
    spec: &Option<AblationSpec>,
) -> Option<&'a ContributionReportIn> {
    reports
        .iter()
        .filter(|r| &r.ablation == spec)
        .max_by(|a, b| a.ts.cmp(&b.ts))
}

/// 测量层级贡献表
///
/// - 全开基线：`ablation = None` 的最新报告；
/// - 每层边际贡献：`Fβ(全开) − Fβ(仅该层关闭)`（同规格多份取最新）；
/// - 两层皆关报告 → `f_beta_core`（核心基线：规则 + 知识 + LLM 之外的「骨架」）；
/// - 缺基线 / 缺消融臂 → 对应层进 `missing`，不阻断。
pub fn measure_contributions(reports: &[ContributionReportIn]) -> ContributionTable {
    let full = latest_of(reports, &None);
    let f_beta_full = full.map(|r| r.f_beta);

    let mut layers: Vec<LayerContribution> = Vec::new();
    let mut missing: Vec<String> = Vec::new();

    for layer in [
        ablation_layer::L3,
        ablation_layer::DECISION,
        ablation_layer::KNOWLEDGE,
    ] {
        let spec = match layer {
            ablation_layer::L3 => AblationSpec {
                l3_off: true,
                ..Default::default()
            },
            ablation_layer::DECISION => AblationSpec {
                decision_off: true,
                ..Default::default()
            },
            _ => AblationSpec {
                knowledge_off: true,
                ..Default::default()
            },
        };
        match (f_beta_full, latest_of(reports, &Some(spec.clone()))) {
            (Some(fb), Some(r)) => {
                // 消融层 → 报告层名映射（L3 门控 = feature 特征模型层；知识层同名）
                let report_layer = match layer {
                    ablation_layer::L3 => "feature",
                    ablation_layer::KNOWLEDGE => "knowledge",
                    _ => "decision",
                };
                let tp_self = full
                    .and_then(|f| f.layer_tp.get(report_layer))
                    .copied()
                    .unwrap_or(0);
                let delta = ((fb - r.f_beta) * 1e4).round() / 1e4;
                layers.push(LayerContribution {
                    layer: layer.to_string(),
                    f_beta_full: fb,
                    f_beta_ablated: r.f_beta,
                    delta,
                    tp_self,
                    // 自身零定案却有大 Δ → 拓扑假象（Δ 来自下游门控效应）
                    topological: tp_self == 0 && delta > TOPOLOGICAL_DELTA_MIN,
                })
            }
            // 缺基线或缺消融臂 → 该层无法测量
            _ => missing.push(layer.to_string()),
        }
    }
    layers.sort_by(|a, b| b.delta.total_cmp(&a.delta));

    let f_beta_core = latest_of(
        reports,
        &Some(AblationSpec {
            l3_off: true,
            decision_off: true,
            ..Default::default()
        }),
    )
    .map(|r| r.f_beta);

    let (benchmark, split, arm) = reports
        .first()
        .map(|r| (r.benchmark.clone(), r.split.clone(), r.arm.clone()))
        .unwrap_or_default();

    ContributionTable {
        benchmark,
        split,
        arm,
        f_beta_full,
        f_beta_core,
        layers,
        missing,
        reports: reports.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(
        id: &str,
        ts: &str,
        ablation: Option<AblationSpec>,
        f_beta: f64,
    ) -> ContributionReportIn {
        ContributionReportIn {
            report_id: id.into(),
            ts: ts.into(),
            benchmark: "mucgec".into(),
            split: Some("dev".into()),
            arm: "cec3".into(),
            ablation,
            f_beta,
            n_pred: 100,
            fp_strict: 10,
            r_strict: 0.8,
            llm_reach: None,
            layer_tp: Default::default(),
        }
    }

    #[test]
    fn full_table_with_all_arms() {
        let reports = vec![
            report("full", "2026-10-16T15:00:00Z", None, 0.80),
            report(
                "l3off",
                "2026-10-16T15:01:00Z",
                Some(AblationSpec {
                    l3_off: true,
                    decision_off: false,
                    knowledge_off: false,
                }),
                0.72,
            ),
            report(
                "decoff",
                "2026-10-16T15:02:00Z",
                Some(AblationSpec {
                    l3_off: false,
                    decision_off: true,
                    knowledge_off: false,
                }),
                0.79,
            ),
            report(
                "both",
                "2026-10-16T15:03:00Z",
                Some(AblationSpec {
                    l3_off: true,
                    decision_off: true,
                    knowledge_off: false,
                }),
                0.70,
            ),
            report(
                "knowoff",
                "2026-10-16T15:04:00Z",
                Some(AblationSpec {
                    knowledge_off: true,
                    ..Default::default()
                }),
                0.75,
            ),
        ];
        let t = measure_contributions(&reports);
        assert_eq!(t.f_beta_full, Some(0.80));
        assert_eq!(t.f_beta_core, Some(0.70));
        assert_eq!(t.layers.len(), 3);
        assert!(t.missing.is_empty());
        // L3 贡献 0.08 > knowledge 贡献 0.05 > decision 贡献 0.01 → 排序
        assert_eq!(t.layers[0].layer, ablation_layer::L3);
        assert!((t.layers[0].delta - 0.08).abs() < 1e-9);
        assert_eq!(t.layers[1].layer, ablation_layer::KNOWLEDGE);
        assert!((t.layers[1].delta - 0.05).abs() < 1e-9);
        assert_eq!(t.layers[2].layer, ablation_layer::DECISION);
        assert!((t.layers[2].delta - 0.01).abs() < 1e-9);
        assert_eq!(t.reports, 5);
    }

    #[test]
    fn zero_self_tp_large_delta_flags_topological() {
        // L3 自身定案 TP = 0，但 Δ = 全开 Fβ（消融实际关掉了下游 LLM 门控）
        // → 标记拓扑假象
        let mut full = report("full", "2026-10-16T15:00:00Z", None, 0.79);
        full.layer_tp.insert("feature".into(), 0); // L3 自身定案 = 0
        let reports = vec![
            full,
            report(
                "l3off",
                "2026-10-16T15:01:00Z",
                Some(AblationSpec {
                    l3_off: true,
                    decision_off: false,
                    knowledge_off: false,
                }),
                0.0,
            ),
        ];
        let t = measure_contributions(&reports);
        let l3 = t
            .layers
            .iter()
            .find(|l| l.layer == ablation_layer::L3)
            .unwrap();
        assert_eq!(l3.tp_self, 0);
        assert!(l3.topological, "自身零定案 + 大 Δ → 拓扑假象");
    }

    #[test]
    fn self_tp_layer_not_topological() {
        // L3 自身定案 TP > 0 → 非拓扑假象
        let mut full = report("full", "2026-10-16T15:00:00Z", None, 0.79);
        full.layer_tp.insert("feature".into(), 42);
        let reports = vec![
            full,
            report(
                "l3off",
                "2026-10-16T15:01:00Z",
                Some(AblationSpec {
                    l3_off: true,
                    decision_off: false,
                    knowledge_off: false,
                }),
                0.10,
            ),
        ];
        let t = measure_contributions(&reports);
        let l3 = t
            .layers
            .iter()
            .find(|l| l.layer == ablation_layer::L3)
            .unwrap();
        assert_eq!(l3.tp_self, 42);
        assert!(!l3.topological);
    }

    #[test]
    fn missing_arm_degrades_gracefully() {
        let reports = vec![
            report("full", "2026-10-16T15:00:00Z", None, 0.80),
            report(
                "l3off",
                "2026-10-16T15:01:00Z",
                Some(AblationSpec {
                    l3_off: true,
                    decision_off: false,
                    knowledge_off: false,
                }),
                0.72,
            ),
        ];
        let t = measure_contributions(&reports);
        assert_eq!(t.layers.len(), 1);
        assert_eq!(t.layers[0].layer, ablation_layer::L3);
        assert_eq!(
            t.missing,
            vec![
                ablation_layer::DECISION.to_string(),
                ablation_layer::KNOWLEDGE.to_string()
            ]
        );
        assert_eq!(t.f_beta_core, None);
    }

    #[test]
    fn no_full_report_marks_all_missing() {
        let reports = vec![report(
            "l3off",
            "2026-10-16T15:01:00Z",
            Some(AblationSpec {
                l3_off: true,
                decision_off: false,
                knowledge_off: false,
            }),
            0.72,
        )];
        let t = measure_contributions(&reports);
        assert_eq!(t.f_beta_full, None);
        assert!(t.layers.is_empty());
        assert_eq!(t.missing.len(), 3);
    }

    #[test]
    fn latest_report_wins_per_spec() {
        let reports = vec![
            report("full-old", "2026-10-15T00:00:00Z", None, 0.60),
            report("full-new", "2026-10-16T00:00:00Z", None, 0.80),
            report(
                "l3off-old",
                "2026-10-15T00:01:00Z",
                Some(AblationSpec {
                    l3_off: true,
                    decision_off: false,
                    knowledge_off: false,
                }),
                0.50,
            ),
            report(
                "l3off-new",
                "2026-10-16T00:01:00Z",
                Some(AblationSpec {
                    l3_off: true,
                    decision_off: false,
                    knowledge_off: false,
                }),
                0.72,
            ),
        ];
        let t = measure_contributions(&reports);
        assert_eq!(t.f_beta_full, Some(0.80), "应取最新全开报告");
        assert!((t.layers[0].delta - 0.08).abs() < 1e-9);
    }

    #[test]
    fn parse_summary_from_report_json() {
        let v = serde_json::json!({
            "id": "r1",
            "ts": "2026-10-16T15:00:00Z",
            "benchmark": "mucgec",
            "split": "dev",
            "arm": "cec3",
            "ablation": { "l3": false },
            "overall": { "f_beta": 0.72, "n_pred": 90, "fp_strict": 12, "r_strict": 0.75 },
            "funnel": { "llm_reach": 0.2 }
        });
        let r = parse_report_summary(&v).unwrap();
        assert_eq!(
            r.ablation,
            Some(AblationSpec {
                l3_off: true,
                decision_off: false,
                knowledge_off: false,
            })
        );
        assert!((r.f_beta - 0.72).abs() < 1e-9);
        assert_eq!(r.llm_reach, Some(0.2));
    }

    #[test]
    fn parse_summary_null_ablation_is_none() {
        let v = serde_json::json!({ "id": "r2", "overall": { "f_beta": 0.8 } });
        let r = parse_report_summary(&v).unwrap();
        assert_eq!(r.ablation, None);
    }

    #[test]
    fn parse_summary_rejects_malformed() {
        assert!(parse_report_summary(&serde_json::json!({ "foo": 1 })).is_none());
    }
}
