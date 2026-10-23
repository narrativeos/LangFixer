//! 反思中心（进化中心 P1）：反馈聚合 + 测评证据 → 反思报告 → 建议队列
//!
//! 与 [`super::reflection`]（单条「放弃」的即时偏差诊断）互补：
//! 反思中心做的是**跨反馈聚合**——把 `feedback_log` 的积累（按来源层 /
//! 规则 id）与测评中心的最近报告（层级归因 / 漏斗）聚成一份**反思报告**
//! （findings + 证据 + 建议队列），建议可被 P2 训练中心消费：
//!
//! | 建议类型（[`suggestion_kinds`]） | 触发证据 | P2 消费方式 |
//! |---|---|---|
//! | `rule_tune` | 某规则独立拒绝 ≥ [`RULE_REJECT_HIGH_COUNT`]，或拒绝占比 ≥ [`RULE_REJECT_HIGH_RATIO`]（n≥2） | 停用 / 收紧规则（规则面板 + 词表） |
//! | `lexicon_condition` | 规则存在被拒绝文本（含测评证据：规则层 FP 高） | 条件化条目（`EntryCondition`），先过测评消融再入库 |
//! | `l3_sample` | 模型层（feature/decision/statistical）拒绝 ≥ [`MODEL_RETRAIN_COUNT`]（或测评证据 L3 门控 F1 偏低） | 汇入 L3 飞轮负样本 → 重训练 |
//! | `llm_arm` | LLM 层拒绝 ≥ [`LLM_ARM_REJECT_COUNT`]，或测评证据显示当前臂 Fβ 弱且 FP 高 | 换臂 / 构建 LLM 训练数据 |
//!
//! 本模块是**确定性核心**（无 LLM 依赖，离线可用；测评证据经
//! [`fetch_eval_evidence`] 从测评服务拉取，服务不可达时报告退化为
//! 纯反馈数据，不阻断）。存储（报告 / 建议队列持久化）由
//! `lexicon-store::ReflectionCenterStore` 承载，API 组装见
//! `api-server/src/routes/reflection_center.rs`。

use serde::{Deserialize, Serialize};

/// 反思中心建议类型（`ReflectionSuggestion::kind`）
pub mod suggestion_kinds {
    /// 词表条件化（payload：`{lang, rule_id?, originals}`）
    pub const LEXICON_CONDITION: &str = "lexicon_condition";
    /// 规则调参：停用 / 收紧（payload：`{lang, rule_id}`）
    pub const RULE_TUNE: &str = "rule_tune";
    /// L3 样本：负样本积累 / 重训练（payload：`{lang, source, count, originals}`）
    pub const L3_SAMPLE: &str = "l3_sample";
    /// LLM 臂：换臂 / 训练数据构建（payload：`{lang, current_arm?, count?}`）
    pub const LLM_ARM: &str = "llm_arm";
    /// 改错对补充：知识源发布产出的新点改错对（payload：`{lang, point_id, wrong, right, source_id}`）；
    /// 人工采纳 → 写入学习知识（向量召回层），拒绝即忽略
    pub const LEXICON_ADD: &str = "lexicon_add";
}

/// 规则误报率阈值（与 [`super::reflection`] 单条反思一致：按**去重文本数**计）
pub const RULE_REJECT_HIGH_COUNT: i64 = 3;
/// 规则拒绝占比阈值（拒绝 / (采纳+拒绝)，仅当独立反馈 ≥ 2 时参与判定）
pub const RULE_REJECT_HIGH_RATIO: f64 = 0.5;
/// 模型层拒绝累计达到该数 → 建议重训练（低于该数 → 建议继续积累）
pub const MODEL_RETRAIN_COUNT: i64 = 3;
/// LLM 层拒绝累计达到该数 → 建议评估换臂
pub const LLM_ARM_REJECT_COUNT: i64 = 3;
/// 测评证据：L3 门控 F1 低于该值 → 建议重训练（判别力不足）
pub const L3_GATE_F1_WEAK: f64 = 0.9;
/// 「重训无效」判定：重训后复测 F1 未超过重训前值 + 该裕量 → 视为无效
pub const L3_RETRAIN_EPSILON: f64 = 0.05;
/// 测评证据：LLM 层 Fβ 低于该值且 FP 显著多于 TP → 建议换臂
pub const LLM_ARM_F_BETA_WEAK: f64 = 0.3;

/// 反馈统计输入（由调用方从 `FeedbackLogStore::stats` 转换；
/// 本模块保持纯数据，不依赖 lexicon-store）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FeedbackStatIn {
    /// 来源层（`rule` / `knowledge` / `decision` / `feature` / `statistical` / `llm`）
    pub source: String,
    /// 关联规则 id（规则层来源时有值）
    #[serde(default)]
    pub rule_id: Option<String>,
    #[serde(default)]
    pub adopted: i64,
    #[serde(default)]
    pub rejected: i64,
    /// 最近一次反馈时间
    #[serde(default)]
    pub last_at: String,
}

/// 反馈明细输入（由调用方从 `FeedbackLogStore::list` 转换）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FeedbackEntryIn {
    pub source: String,
    #[serde(default)]
    pub rule_id: Option<String>,
    #[serde(default)]
    pub error_type: String,
    /// 被反馈的原文片段
    #[serde(default)]
    pub original: String,
    #[serde(default)]
    pub correction: String,
    /// `adopt` / `reject`
    pub action: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub created_at: String,
}

/// 测评证据：单层归因摘要（从测评报告 `layers.{name}` 提取）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EvalLayerEvidence {
    /// 层名（`rule` / `decision` / `knowledge` / `feature` / `statistical` / `llm`）
    pub name: String,
    #[serde(default)]
    pub n_pred: i64,
    #[serde(default)]
    pub f_beta: f64,
    #[serde(default)]
    pub tp_overlap: i64,
    #[serde(default)]
    pub fp_overlap: i64,
    /// 召回份额（该层「最先解决」的 gold 编辑占比）
    #[serde(default)]
    pub recall_share: f64,
    /// 句级门控 F1（无门控数据时为 None）
    #[serde(default)]
    pub gate_f1: Option<f64>,
}

/// 测评证据（从测评中心最近一份报告提取；`None` = 无可用报告）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EvalEvidence {
    /// 测评报告 id（如 `20261016T155142Z_mucgec_cec3_capability`）
    pub report_id: String,
    #[serde(default)]
    pub benchmark: String,
    #[serde(default)]
    pub arm: String,
    /// 总体 Fβ（严格）
    #[serde(default)]
    pub f_beta: f64,
    /// LLM 触达率（funnel 模式报告有值）
    #[serde(default)]
    pub llm_reach: Option<f64>,
    #[serde(default)]
    pub layers: Vec<EvalLayerEvidence>,
}

/// L3 重训练状态（「重训无效」判定用；调用方从训练作业历史 + 复测报告组装）
///
/// 反思中心 `l3_sample` 建议被消费（重训练执行）后，若 L3 门控 F1 复测
/// 无改善，下一次反思不应再提重训练（避免「提议→重训→无变化→再提议」
/// 的循环假阳性）；有新数据进来时重新评估。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct L3RetrainStateIn {
    /// 最近一次 l3_retrain 作业创建时对应的测评报告 id（空 = 未知）
    #[serde(default)]
    pub last_job_report_id: Option<String>,
    /// 该报告的 L3 门控 F1（「前」值）
    #[serde(default)]
    pub f1_before: Option<f64>,
    /// 当前（复测）报告的 L3 门控 F1（「后」值；None = 尚未复测）
    #[serde(default)]
    pub f1_after: Option<f64>,
}

/// 可执行建议（P2 训练中心消费；队列状态见 `ReflectionCenterStore`）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReflectionSuggestion {
    /// 建议类型，见 [`suggestion_kinds`]
    pub kind: String,
    /// 目标（规则 id / 模型层名 / 臂名）
    pub target: String,
    /// 一句话建议
    pub summary: String,
    /// 证据列表（反馈统计 / 测评数字）
    #[serde(default)]
    pub evidence: Vec<String>,
    /// 执行参数（P2 按 kind 解释）
    #[serde(default)]
    pub payload: serde_json::Value,
    /// 优先级（0 最高；同 kind 内按触发强度排序）
    #[serde(default)]
    pub priority: u8,
}

/// 反思报告（一次「生成反思报告」的完整产出）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReflectionCenterReport {
    pub lang: String,
    /// 生成时间（UTC ISO8601）
    pub created_at: String,
    /// 反馈总量（采纳 + 放弃）
    #[serde(default)]
    pub feedback_total: i64,
    /// 按（来源层, 规则 id）聚合的反馈统计
    #[serde(default)]
    pub stats: Vec<FeedbackStatIn>,
    /// 发现（自然语言：哪层弱、证据是什么）
    #[serde(default)]
    pub findings: Vec<String>,
    /// 测评证据（`None` = 未请求 / 测评服务不可达 / 无匹配报告）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eval_evidence: Option<EvalEvidence>,
    /// 建议队列（按 priority 升序）
    #[serde(default)]
    pub suggestions: Vec<ReflectionSuggestion>,
}

fn num(v: &serde_json::Value, key: &str) -> i64 {
    v.get(key).and_then(|x| x.as_i64()).unwrap_or(0)
}

fn fnum(v: &serde_json::Value, key: &str) -> f64 {
    v.get(key).and_then(|x| x.as_f64()).unwrap_or(0.0)
}

/// 从测评报告 JSON（`GET /eval/report/:id` 响应体）提取证据
///
/// 解析失败（结构不符）返回 `None`——反思报告退化为纯反馈数据。
pub fn parse_eval_report(v: &serde_json::Value) -> Option<EvalEvidence> {
    let layers = v
        .get("layers")
        .and_then(|l| l.as_object())
        .map(|m| {
            m.iter()
                .map(|(name, lv)| EvalLayerEvidence {
                    name: name.clone(),
                    n_pred: num(lv, "n_pred"),
                    f_beta: fnum(lv, "f_beta"),
                    tp_overlap: num(lv, "tp_overlap"),
                    fp_overlap: num(lv, "fp_overlap"),
                    recall_share: fnum(lv, "recall_share"),
                    gate_f1: lv
                        .get("gate")
                        .and_then(|g| g.get("f1"))
                        .and_then(|x| x.as_f64()),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let overall = v.get("overall")?;
    Some(EvalEvidence {
        report_id: v
            .get("id")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        benchmark: v
            .get("benchmark")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        arm: v
            .get("arm")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        f_beta: fnum(overall, "f_beta"),
        llm_reach: v
            .get("funnel")
            .and_then(|f| f.get("llm_reach"))
            .and_then(|x| x.as_f64()),
        layers,
    })
}

/// 选择测评证据报告（纯选择逻辑，可单测）
///
/// 列表按 ts 降序（最新在前）。优先选**非消融**（全开）报告——消融报告的
/// Fβ 是「该层关闭」的测量，不能当作当前系统状态的证据（否则补跑消融期间
/// 反思会引用 Fβ=0 的 l3-off 报告得出「L3 门控 F1=0」的假证据）；
/// 无全开报告时退回最新报告。
pub fn select_evidence_report_id(reports: &[serde_json::Value], lang: &str) -> Option<String> {
    let bmks: &[&str] = if lang == "en" {
        &["jfleg"]
    } else {
        &["mucgec", "cscd", "nacgec"]
    };
    let matched: Vec<&serde_json::Value> = reports
        .iter()
        .filter(|r| bmks.contains(&r.get("benchmark").and_then(|b| b.as_str()).unwrap_or("")))
        .collect();
    let full = matched
        .iter()
        .find(|r| r.get("ablation").map(|a| a.is_null()).unwrap_or(true));
    full.or_else(|| matched.first())
        .and_then(|r| r.get("id")?.as_str())
        .map(|s| s.to_string())
}

/// 从测评服务拉取证据（`GET {endpoint}/eval/reports` 选报告 →
/// `GET {endpoint}/eval/report/:id` 解析）
///
/// - `report_id`：显式指定报告 id；`None` = [`select_evidence_report_id`]
///   （zh → mucgec/cscd/nacgec，en → jfleg；优先非消融报告）
/// - 服务不可达 / 无匹配报告 → `None`（调用方在 findings 中说明）
pub async fn fetch_eval_evidence(
    endpoint: &str,
    lang: &str,
    report_id: Option<&str>,
) -> Option<EvalEvidence> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .ok()?;
    let list: serde_json::Value = client
        .get(format!("{endpoint}/eval/reports"))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    let id = match report_id {
        Some(id) => id.to_string(),
        None => {
            let reports = list.get("reports")?.as_array()?;
            select_evidence_report_id(reports, lang)?
        }
    };
    let report: serde_json::Value = client
        .get(format!("{endpoint}/eval/report/{id}"))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    parse_eval_report(&report)
}

/// 生成反思报告（确定性核心：反馈聚合 + 测评证据 → findings + 建议队列）
///
/// 建议去重：同一 (kind, target) 只保留一条（priority 取高者，证据合并）。
pub fn build_report(
    lang: &str,
    created_at: &str,
    stats: &[FeedbackStatIn],
    recent: &[FeedbackEntryIn],
    eval: Option<EvalEvidence>,
    l3_retrain: &L3RetrainStateIn,
) -> ReflectionCenterReport {
    let mut findings: Vec<String> = Vec::new();
    let mut suggestions: Vec<ReflectionSuggestion> = Vec::new();

    let feedback_total: i64 = stats.iter().map(|s| s.adopted + s.rejected).sum();
    let reject_total: i64 = stats.iter().map(|s| s.rejected).sum();

    // ── 反馈侧 findings ──
    if feedback_total == 0 {
        findings.push(format!(
            "语言 {lang} 暂无反馈积累（采纳 / 放弃均为 0）——建议先在日常检测中使用「采纳 / 放弃」沉淀数据"
        ));
    } else {
        findings.push(format!(
            "语言 {lang} 累计反馈 {feedback_total} 条（放弃 {reject_total} 条）"
        ));
        let by_source: std::collections::HashMap<&str, i64> =
            stats
                .iter()
                .fold(std::collections::HashMap::new(), |mut m, s| {
                    *m.entry(s.source.as_str()).or_insert(0) += s.rejected;
                    m
                });
        if let Some((source, n)) = by_source
            .into_iter()
            .filter(|(_, n)| *n > 0)
            .max_by_key(|(_, n)| *n)
        {
            findings.push(format!("放弃最集中的来源层：{source}（{n} 条）"));
        }
    }

    // ── 反馈侧建议 ──
    for s in stats {
        let rejected = s.rejected;
        if rejected <= 0 {
            continue;
        }
        match s.source.as_str() {
            "rule" => {
                let rule_id = s.rule_id.clone().unwrap_or_else(|| "（未关联规则）".into());
                let total = s.adopted + s.rejected;
                let high_ratio =
                    total >= 2 && rejected as f64 / total as f64 >= RULE_REJECT_HIGH_RATIO;
                if rejected >= RULE_REJECT_HIGH_COUNT || high_ratio {
                    push_suggestion(
                        &mut suggestions,
                        suggestion_kinds::RULE_TUNE,
                        rule_id.clone(),
                        format!(
                            "规则 {rule_id} 误报率偏高（独立拒绝 {rejected} / 反馈 {total}）→ 建议停用或收紧"
                        ),
                        vec![format!(
                            "反馈统计：拒绝 {rejected} / 采纳 {}（最近 {}）",
                            s.adopted, s.last_at
                        )],
                        serde_json::json!({ "lang": lang, "rule_id": s.rule_id }),
                        0,
                    );
                }
                let originals =
                    recent_rejected_originals(recent, &s.source, s.rule_id.as_deref(), 5);
                if !originals.is_empty() {
                    push_suggestion(
                        &mut suggestions,
                        suggestion_kinds::LEXICON_CONDITION,
                        rule_id,
                        format!(
                            "规则有 {} 条被拒绝文本 → 建议条件化排除条目（入库前先过测评消融）",
                            originals.len()
                        ),
                        vec![format!("被拒绝文本：{}", originals.join("、"))],
                        serde_json::json!({ "lang": lang, "rule_id": s.rule_id, "originals": originals }),
                        2,
                    );
                }
            }
            "feature" | "decision" | "statistical" => {
                let originals = recent_rejected_originals(recent, &s.source, None, 5);
                let evidence = vec![format!(
                    "被放弃样本：{}",
                    if originals.is_empty() {
                        "（见反馈明细）".into()
                    } else {
                        originals.join("、")
                    }
                )];
                if rejected >= MODEL_RETRAIN_COUNT {
                    push_suggestion(
                        &mut suggestions,
                        suggestion_kinds::L3_SAMPLE,
                        s.source.clone(),
                        format!(
                            "模型层 {} 累计误报 {rejected} 条 → 建议汇入 L3 飞轮负样本并重训练",
                            s.source
                        ),
                        evidence,
                        serde_json::json!({ "lang": lang, "source": s.source, "count": rejected, "originals": originals }),
                        1,
                    );
                } else {
                    push_suggestion(
                        &mut suggestions,
                        suggestion_kinds::L3_SAMPLE,
                        s.source.clone(),
                        format!(
                            "模型层 {} 误报 {rejected} 条（未达重训练阈值 {MODEL_RETRAIN_COUNT}）→ 继续积累 L3 负样本",
                            s.source
                        ),
                        evidence,
                        serde_json::json!({ "lang": lang, "source": s.source, "count": rejected, "originals": originals }),
                        2,
                    );
                }
            }
            "llm" => {
                if rejected >= LLM_ARM_REJECT_COUNT {
                    let originals = recent_rejected_originals(recent, "llm", None, 5);
                    push_suggestion(
                        &mut suggestions,
                        suggestion_kinds::LLM_ARM,
                        "llm".into(),
                        format!(
                            "LLM 层累计误报 {rejected} 条 → 建议评估换臂（cec3 / cec4 / custom）或构建训练数据"
                        ),
                        vec![format!(
                            "被放弃样本：{}",
                            if originals.is_empty() {
                                "（见反馈明细）".into()
                            } else {
                                originals.join("、")
                            }
                        )],
                        serde_json::json!({ "lang": lang, "current_arm": null, "count": rejected }),
                        2,
                    );
                }
            }
            // knowledge 层：逐条删除已由单条反思流程覆盖（remove_knowledge），中心层仅记 findings
            _ => {}
        }
    }

    // ── 测评证据侧 ──
    if let Some(ev) = &eval {
        if !ev.report_id.is_empty() {
            findings.push(format!(
                "测评证据（报告 {}，{}/{}）：总体 Fβ={:.3}，LLM 触达率 {}",
                ev.report_id,
                ev.benchmark,
                ev.arm,
                ev.f_beta,
                ev.llm_reach
                    .map(|r| format!("{:.1}%", r * 100.0))
                    .unwrap_or_else(|| "—".into())
            ));
        }
        for l in &ev.layers {
            if l.recall_share > 0.0 {
                findings.push(format!(
                    "层 {} 召回份额 {:.1}%（Fβ={:.3}，FP 重叠 {}）",
                    l.name,
                    l.recall_share * 100.0,
                    l.f_beta,
                    l.fp_overlap
                ));
            }
            match l.name.as_str() {
                "llm" => {
                    // 当前臂弱（Fβ 低且 FP 显著多于 TP）→ 换臂
                    if l.f_beta < LLM_ARM_F_BETA_WEAK && l.fp_overlap > 3 * l.tp_overlap.max(1) {
                        push_suggestion(
                            &mut suggestions,
                            suggestion_kinds::LLM_ARM,
                            if ev.arm.is_empty() {
                                "llm".into()
                            } else {
                                ev.arm.clone()
                            },
                            format!(
                                "测评：当前臂 {} LLM 层 Fβ={:.3}（FP 重叠 {} ≫ TP {}）→ 建议换臂或构建 LLM 训练数据",
                                if ev.arm.is_empty() {
                                    "（未标注）"
                                } else {
                                    &ev.arm
                                },
                                l.f_beta,
                                l.fp_overlap,
                                l.tp_overlap
                            ),
                            vec![format!(
                                "报告 {}（{}/{}）",
                                ev.report_id, ev.benchmark, ev.arm
                            )],
                            serde_json::json!({ "lang": lang, "current_arm": value_or_null(&ev.arm), "report_id": ev.report_id }),
                            1,
                        );
                    }
                }
                "feature" => {
                    // L3 门控判别力不足 → 重训练
                    if let Some(f1) = l.gate_f1 {
                        if f1 < L3_GATE_F1_WEAK {
                            // 「重训无效」检查：已重训过且复测 F1 无改善 → 不再提议
                            // （避免「提议→重训→无变化→再提议」的循环假阳性；
                            //  有新数据进来时重新评估）
                            let ineffective = l3_retrain
                                .f1_before
                                .zip(l3_retrain.f1_after)
                                .map(|(before, after)| {
                                    let re_measured = l3_retrain
                                        .last_job_report_id
                                        .as_deref()
                                        .is_some_and(|rid| !rid.is_empty() && rid != ev.report_id);
                                    re_measured && after <= before + L3_RETRAIN_EPSILON
                                })
                                .unwrap_or(false);
                            if ineffective {
                                findings.push(format!(
                                    "L3 门控 F1={:.3} 仍偏低，但已执行过重训练（报告 {}）且复测无改善（{:.3}→{:.3}）→ 标记重训无效，不再重复提议（有新数据时重新评估）",
                                    f1,
                                    l3_retrain
                                        .last_job_report_id
                                        .as_deref()
                                        .unwrap_or("?"),
                                    l3_retrain.f1_before.unwrap_or(0.0),
                                    l3_retrain.f1_after.unwrap_or(0.0),
                                ));
                            } else {
                                push_suggestion(
                                    &mut suggestions,
                                    suggestion_kinds::L3_SAMPLE,
                                    "feature".into(),
                                    format!(
                                        "测评：L3 门控 F1={:.3}（< {L3_GATE_F1_WEAK}）→ 建议重训练提升判别力（当前弃权偏保守）",
                                        f1
                                    ),
                                    vec![format!(
                                        "报告 {}（{}/{}）",
                                        ev.report_id, ev.benchmark, ev.arm
                                    )],
                                    serde_json::json!({ "lang": lang, "source": "feature", "gate_f1": f1, "report_id": ev.report_id }),
                                    1,
                                );
                            }
                        }
                    }
                }
                "rule" => {
                    // 规则层 FP 显著多于 TP → 条件化 / 调参
                    if l.n_pred > 0 && l.fp_overlap > 3 * l.tp_overlap.max(1) {
                        push_suggestion(
                            &mut suggestions,
                            suggestion_kinds::LEXICON_CONDITION,
                            "rule".into(),
                            format!(
                                "测评：规则层 FP 重叠 {} ≫ 命中 {}（预测 {}）→ 建议条件化条目 / 规则调参",
                                l.fp_overlap, l.tp_overlap, l.n_pred
                            ),
                            vec![format!(
                                "报告 {}（{}/{}）",
                                ev.report_id, ev.benchmark, ev.arm
                            )],
                            serde_json::json!({ "lang": lang, "rule_id": null, "report_id": ev.report_id }),
                            1,
                        );
                    }
                }
                _ => {}
            }
        }
    }

    // 建议按 priority 升序（同 priority 保持生成序）
    suggestions.sort_by_key(|s| s.priority);

    ReflectionCenterReport {
        lang: lang.to_string(),
        created_at: created_at.to_string(),
        feedback_total,
        stats: stats.to_vec(),
        findings,
        eval_evidence: eval,
        suggestions,
    }
}

fn value_or_null(s: &str) -> serde_json::Value {
    if s.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::Value::String(s.to_string())
    }
}

/// 去重合并：同一 (kind, target) 保留 priority 高者，证据合并
fn push_suggestion(
    out: &mut Vec<ReflectionSuggestion>,
    kind: &str,
    target: String,
    summary: String,
    evidence: Vec<String>,
    payload: serde_json::Value,
    priority: u8,
) {
    if let Some(existing) = out
        .iter_mut()
        .find(|s| s.kind == kind && s.target == target)
    {
        if priority < existing.priority {
            existing.priority = priority;
            existing.summary = summary;
            existing.payload = payload;
        }
        for e in evidence {
            if !existing.evidence.contains(&e) {
                existing.evidence.push(e);
            }
        }
    } else {
        out.push(ReflectionSuggestion {
            kind: kind.to_string(),
            target,
            summary,
            evidence,
            payload,
            priority,
        });
    }
}

/// 某来源层（可选规则 id）被拒绝的原文（去重、截断到 `limit` 条）
fn recent_rejected_originals(
    recent: &[FeedbackEntryIn],
    source: &str,
    rule_id: Option<&str>,
    limit: usize,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for e in recent.iter().rev() {
        if e.action != "reject" || e.source != source {
            continue;
        }
        if let Some(rid) = rule_id {
            if e.rule_id.as_deref() != Some(rid) {
                continue;
            }
        }
        if !e.original.is_empty() && !out.contains(&e.original) {
            out.push(e.original.clone());
            if out.len() >= limit {
                break;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stat(source: &str, rule_id: Option<&str>, adopted: i64, rejected: i64) -> FeedbackStatIn {
        FeedbackStatIn {
            source: source.into(),
            rule_id: rule_id.map(str::to_string),
            adopted,
            rejected,
            last_at: "2026-10-17T00:00:00Z".into(),
        }
    }

    fn entry(source: &str, rule_id: Option<&str>, original: &str, action: &str) -> FeedbackEntryIn {
        FeedbackEntryIn {
            source: source.into(),
            rule_id: rule_id.map(str::to_string),
            error_type: "grammar".into(),
            original: original.into(),
            correction: String::new(),
            action: action.into(),
            reason: String::new(),
            created_at: "2026-10-17T00:00:00Z".into(),
        }
    }

    #[test]
    fn empty_feedback_yields_guidance_only() {
        let r = build_report("zh", "t", &[], &[], None, &L3RetrainStateIn::default());
        assert!(r.findings.iter().any(|f| f.contains("暂无反馈积累")));
        assert!(r.suggestions.is_empty());
    }

    #[test]
    fn rule_high_reject_rate_triggers_tune_and_condition() {
        let stats = vec![stat("rule", Some("ZH_SPELL"), 1, 3)];
        let recent = vec![
            entry("rule", Some("ZH_SPELL"), "报复", "reject"),
            entry("rule", Some("ZH_SPELL"), "靠山", "reject"),
            entry("rule", Some("ZH_SPELL"), "处理", "reject"),
        ];
        let r = build_report(
            "zh",
            "t",
            &stats,
            &recent,
            None,
            &L3RetrainStateIn::default(),
        );
        let tune = r
            .suggestions
            .iter()
            .find(|s| s.kind == suggestion_kinds::RULE_TUNE)
            .unwrap();
        assert_eq!(tune.target, "ZH_SPELL");
        assert_eq!(tune.priority, 0);
        let cond = r
            .suggestions
            .iter()
            .find(|s| s.kind == suggestion_kinds::LEXICON_CONDITION)
            .unwrap();
        let originals: Vec<String> = cond.payload["originals"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(originals, vec!["处理", "靠山", "报复"]);
    }

    #[test]
    fn rule_low_reject_no_tune() {
        // 1 拒绝 / 5 采纳 = 17% < 50%，且 < 3 次 → 仅条件化
        let stats = vec![stat("rule", Some("R1"), 5, 1)];
        let recent = vec![entry("rule", Some("R1"), "误报词", "reject")];
        let r = build_report(
            "zh",
            "t",
            &stats,
            &recent,
            None,
            &L3RetrainStateIn::default(),
        );
        assert!(
            !r.suggestions
                .iter()
                .any(|s| s.kind == suggestion_kinds::RULE_TUNE)
        );
        assert!(
            r.suggestions
                .iter()
                .any(|s| s.kind == suggestion_kinds::LEXICON_CONDITION)
        );
    }

    #[test]
    fn model_layer_rejects_accumulate_to_retrain() {
        let stats = vec![stat("feature", None, 0, 3)];
        let recent = vec![
            entry("feature", None, "句子A", "reject"),
            entry("feature", None, "句子B", "reject"),
            entry("feature", None, "句子C", "reject"),
        ];
        let r = build_report(
            "zh",
            "t",
            &stats,
            &recent,
            None,
            &L3RetrainStateIn::default(),
        );
        let s = r
            .suggestions
            .iter()
            .find(|s| s.kind == suggestion_kinds::L3_SAMPLE)
            .unwrap();
        assert_eq!(s.priority, 1, "达到重训练阈值 → 高优先级");
        assert_eq!(s.payload["count"], 3);
        assert_eq!(s.payload["originals"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn model_layer_below_threshold_accumulates() {
        let stats = vec![stat("decision", None, 0, 2)];
        let r = build_report("zh", "t", &stats, &[], None, &L3RetrainStateIn::default());
        let s = r
            .suggestions
            .iter()
            .find(|s| s.kind == suggestion_kinds::L3_SAMPLE)
            .unwrap();
        assert_eq!(s.priority, 2);
        assert!(s.summary.contains("继续积累"));
    }

    #[test]
    fn llm_rejects_trigger_arm_suggestion() {
        let stats = vec![stat("llm", None, 0, 3)];
        let r = build_report("zh", "t", &stats, &[], None, &L3RetrainStateIn::default());
        assert!(
            r.suggestions
                .iter()
                .any(|s| s.kind == suggestion_kinds::LLM_ARM)
        );
    }
    #[test]
    fn eval_evidence_weak_arm_triggers_switch() {
        // 真实弱臂场景：Fβ 低 + FP ≫ TP
        let ev = EvalEvidence {
            report_id: "r1".into(),
            benchmark: "mucgec".into(),
            arm: "weak".into(),
            f_beta: 0.1,
            llm_reach: Some(0.9),
            layers: vec![EvalLayerEvidence {
                name: "llm".into(),
                n_pred: 300,
                f_beta: 0.1,
                tp_overlap: 5,
                fp_overlap: 100,
                recall_share: 0.05,
                gate_f1: Some(0.8),
            }],
        };
        let r = build_report("zh", "t", &[], &[], Some(ev), &L3RetrainStateIn::default());
        let arm = r
            .suggestions
            .iter()
            .find(|s| s.kind == suggestion_kinds::LLM_ARM)
            .unwrap();
        assert_eq!(arm.target, "weak");
        assert_eq!(arm.priority, 1);
        assert!(r.findings.iter().any(|f| f.contains("mucgec")));
    }

    #[test]
    fn eval_evidence_strong_arm_no_suggestion() {
        // TP ≫ FP → 不触发换臂
        let ev = EvalEvidence {
            report_id: "r1b".into(),
            benchmark: "mucgec".into(),
            arm: "cec3".into(),
            f_beta: 0.8,
            llm_reach: Some(0.2),
            layers: vec![EvalLayerEvidence {
                name: "llm".into(),
                f_beta: 0.8,
                tp_overlap: 200,
                fp_overlap: 50,
                ..Default::default()
            }],
        };
        let r = build_report("zh", "t", &[], &[], Some(ev), &L3RetrainStateIn::default());
        assert!(
            !r.suggestions
                .iter()
                .any(|s| s.kind == suggestion_kinds::LLM_ARM)
        );
    }

    #[test]
    fn eval_evidence_weak_l3_gate_triggers_retrain() {
        let ev = EvalEvidence {
            report_id: "r2".into(),
            benchmark: "mucgec".into(),
            arm: "cec3".into(),
            f_beta: 0.8,
            llm_reach: Some(0.1),
            layers: vec![EvalLayerEvidence {
                name: "feature".into(),
                f_beta: 0.5,
                tp_overlap: 10,
                fp_overlap: 4,
                recall_share: 0.2,
                gate_f1: Some(0.72),
                ..Default::default()
            }],
        };
        let r = build_report("zh", "t", &[], &[], Some(ev), &L3RetrainStateIn::default());
        let s = r
            .suggestions
            .iter()
            .find(|s| s.kind == suggestion_kinds::L3_SAMPLE)
            .unwrap();
        assert_eq!(s.target, "feature");
        assert!(s.summary.contains("0.720"));
    }

    #[test]
    fn l3_retrain_ineffective_suppresses_reproposal() {
        // 已重训过（报告 r1，F1 0.72）且复测（报告 r2）无改善（0.72→0.72）
        // → 不再提议 l3_sample，改为 findings 说明
        let ev = EvalEvidence {
            report_id: "r2".into(),
            benchmark: "mucgec".into(),
            arm: "cec3".into(),
            f_beta: 0.8,
            llm_reach: Some(0.1),
            layers: vec![EvalLayerEvidence {
                name: "feature".into(),
                f_beta: 0.5,
                tp_overlap: 10,
                fp_overlap: 4,
                recall_share: 0.2,
                gate_f1: Some(0.72),
                ..Default::default()
            }],
        };
        let state = L3RetrainStateIn {
            last_job_report_id: Some("r1".into()),
            f1_before: Some(0.72),
            f1_after: Some(0.72),
        };
        let r = build_report("zh", "t", &[], &[], Some(ev), &state);
        assert!(
            !r.suggestions
                .iter()
                .any(|s| s.kind == suggestion_kinds::L3_SAMPLE),
            "重训无效时不应再提议: {:?}",
            r.suggestions
        );
        assert!(
            r.findings.iter().any(|f| f.contains("重训无效")),
            "findings 应说明重训无效: {:?}",
            r.findings
        );
    }

    #[test]
    fn l3_retrain_same_report_still_proposes() {
        // 重训作业由当前报告触发（尚未复测）→ 仍正常提议
        let ev = EvalEvidence {
            report_id: "r1".into(),
            benchmark: "mucgec".into(),
            arm: "cec3".into(),
            f_beta: 0.8,
            llm_reach: Some(0.1),
            layers: vec![EvalLayerEvidence {
                name: "feature".into(),
                gate_f1: Some(0.72),
                ..Default::default()
            }],
        };
        let state = L3RetrainStateIn {
            last_job_report_id: Some("r1".into()),
            f1_before: Some(0.72),
            f1_after: Some(0.72),
        };
        let r = build_report("zh", "t", &[], &[], Some(ev), &state);
        assert!(
            r.suggestions
                .iter()
                .any(|s| s.kind == suggestion_kinds::L3_SAMPLE),
            "未复测（同一报告）时仍应提议"
        );
    }

    #[test]
    fn eval_evidence_rule_fp_triggers_condition() {
        let ev = EvalEvidence {
            report_id: "r3".into(),
            benchmark: "mucgec".into(),
            arm: "cec3".into(),
            f_beta: 0.8,
            llm_reach: None,
            layers: vec![EvalLayerEvidence {
                name: "rule".into(),
                n_pred: 136,
                f_beta: 0.0,
                tp_overlap: 2,
                fp_overlap: 124,
                recall_share: 0.002,
                gate_f1: None,
            }],
        };
        let r = build_report("zh", "t", &[], &[], Some(ev), &L3RetrainStateIn::default());
        assert!(
            r.suggestions
                .iter()
                .any(|s| s.kind == suggestion_kinds::LEXICON_CONDITION && s.target == "rule")
        );
    }

    #[test]
    fn dedup_merges_evidence_and_keeps_higher_priority() {
        // 反馈侧（priority 2）+ 测评侧（priority 1）同一 L3 建议 → 合并
        let stats = vec![stat("feature", None, 0, 1)];
        let ev = EvalEvidence {
            report_id: "r4".into(),
            benchmark: "mucgec".into(),
            arm: "cec3".into(),
            f_beta: 0.8,
            llm_reach: None,
            layers: vec![EvalLayerEvidence {
                name: "feature".into(),
                gate_f1: Some(0.5),
                ..Default::default()
            }],
        };
        let r = build_report(
            "zh",
            "t",
            &stats,
            &[],
            Some(ev),
            &L3RetrainStateIn::default(),
        );
        let l3: Vec<&ReflectionSuggestion> = r
            .suggestions
            .iter()
            .filter(|s| s.kind == suggestion_kinds::L3_SAMPLE)
            .collect();
        assert_eq!(l3.len(), 1, "同一 (kind, target) 应去重");
        assert_eq!(l3[0].priority, 1, "保留高优先级");
        assert!(
            l3[0].evidence.len() >= 2,
            "证据应合并: {:?}",
            l3[0].evidence
        );
    }

    #[test]
    fn select_evidence_prefers_non_ablation_report() {
        // 列表 ts 降序：最新的 l3-off 消融报告在前，全开报告在后
        // → 应选全开报告（消融报告的 Fβ 不是当前系统状态的证据）
        let reports = vec![
            serde_json::json!({"id": "l3off-new", "benchmark": "mucgec", "ablation": {"l3": false}}),
            serde_json::json!({"id": "full-old", "benchmark": "mucgec", "ablation": null}),
            serde_json::json!({"id": "jfleg-new", "benchmark": "jfleg"}),
        ];
        assert_eq!(
            select_evidence_report_id(&reports, "zh"),
            Some("full-old".to_string())
        );
        // 无全开报告 → 退回最新
        let only_abl = vec![
            serde_json::json!({"id": "l3off", "benchmark": "mucgec", "ablation": {"l3": false}}),
        ];
        assert_eq!(
            select_evidence_report_id(&only_abl, "zh"),
            Some("l3off".to_string())
        );
        // 语言过滤：en 只看 jfleg
        assert_eq!(
            select_evidence_report_id(&reports, "en"),
            Some("jfleg-new".to_string())
        );
        // 空列表
        assert_eq!(select_evidence_report_id(&[], "zh"), None);
    }

    #[test]
    fn parse_eval_report_extracts_evidence() {
        let v = serde_json::json!({
            "id": "20261016T155142Z_mucgec_cec3_capability",
            "benchmark": "mucgec",
            "arm": "cec3",
            "overall": { "f_beta": 0.8052, "fp_overlap": 100 },
            "funnel": { "llm_reach": 0.1985 },
            "layers": {
                "rule": { "n_pred": 136, "f_beta": 0.1, "tp_overlap": 12, "fp_overlap": 124, "recall_share": 0.0018, "gate": { "f1": 0.9 } },
                "llm": { "n_pred": 300, "f_beta": 0.8, "tp_overlap": 200, "fp_overlap": 50, "recall_share": 0.5, "gate": { "f1": 0.99 } }
            }
        });
        let ev = parse_eval_report(&v).unwrap();
        assert_eq!(ev.report_id, "20261016T155142Z_mucgec_cec3_capability");
        assert_eq!(ev.arm, "cec3");
        assert!((ev.f_beta - 0.8052).abs() < 1e-9);
        assert_eq!(ev.llm_reach, Some(0.1985));
        assert_eq!(ev.layers.len(), 2);
        let rule = ev.layers.iter().find(|l| l.name == "rule").unwrap();
        assert_eq!(rule.fp_overlap, 124);
        assert_eq!(rule.gate_f1, Some(0.9));
    }

    #[test]
    fn parse_eval_report_rejects_malformed() {
        assert!(parse_eval_report(&serde_json::json!({ "foo": 1 })).is_none());
        assert!(parse_eval_report(&serde_json::Value::Null).is_none());
    }
}
