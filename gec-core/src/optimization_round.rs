//! 优化轮次（进化中心 P3）确定性核心
//!
//! 闭环的「决策」环节：自动评估当前积累的数据与训练条件 →
//! **提案**（`proposed`）→ **人工确认门**（`confirmed` / `rejected`）→
//! **执行**（复用 P2 训练作业）→ 测评 A/B 验证（P0 工具）。
//!
//! 与 P1 反思中心的区别：反思中心是「单点诊断」（一次反馈聚合 → 建议队列）；
//! 优化轮次是「周期决策」——综合反馈积累、飞轮池规模、上次重训练时间、
//! 测评证据，判断**现在是否值得开一轮优化**，并规划这轮要做的动作集合。
//!
//! | 动作 kind（复用 [`crate::job_kind`] + 人工项） | 触发条件（确定性阈值） |
//! |---|---|
//! | [`job_kind::L3_RETRAIN`] | 模型层拒绝 ≥ [`L3_ROUND_REJECT_COUNT`]；或飞轮池 ≥ [`L3_ROUND_POOL_SIZE`]；或测评证据 L3 门控 F1 < [`L3_ROUND_GATE_F1_WEAK`] |
//! | [`job_kind::LLM_DATA`] | LLM 层采纳（带修正）≥ [`LLM_ROUND_ADOPT_COUNT`] |
//! | [`job_kind::LEXICON_CONDITION`] | 某规则拒绝 ≥ [`RULE_REJECT_HIGH_COUNT`]（P1 同阈值）且有被拒文本 |
//! | [`crate::reflection::suggestion_kinds::RULE_TUNE`]（人工） | 某规则拒绝 ≥ [`RULE_ROUND_REJECT_COUNT`]，或占比 ≥ [`RULE_ROUND_REJECT_RATIO`]（n≥2） |
//!
//! 全部条件不满足 → [`plan_round`] 返回 `None`（不开轮次；API 层返回
//! 「无需行动」及原因）。
//!
//! 本模块保持纯数据 + 纯函数（不依赖存储 / 网络）；
//! 持久化（`OptimizationRoundStore`）与执行（P2 作业）在存储 / API 层完成。

use serde::{Deserialize, Serialize};

use crate::reflection_center::{FeedbackEntryIn, FeedbackStatIn, suggestion_kinds};
use crate::training_center::job_kind;

/// 轮次状态（状态机：`proposed` → `confirmed` → `running` → `completed` / `failed`；
/// `proposed` → `rejected`（人工门拒绝）。终态不可再迁移。）
pub mod round_status {
    /// 已提案（自动评估产出，等待人工确认）
    pub const PROPOSED: &str = "proposed";
    /// 已确认（人工确认门通过）
    pub const CONFIRMED: &str = "confirmed";
    /// 执行中
    pub const RUNNING: &str = "running";
    /// 已完成（结果在 `result`）
    pub const COMPLETED: &str = "completed";
    /// 失败（原因在 `result.error`）
    pub const FAILED: &str = "failed";
    /// 已拒绝（人工门否决）
    pub const REJECTED: &str = "rejected";

    /// 合法状态集合
    pub const ALL: [&str; 6] = [PROPOSED, CONFIRMED, RUNNING, COMPLETED, FAILED, REJECTED];

    /// 状态迁移是否合法
    pub fn can_transition(from: &str, to: &str) -> bool {
        match (from, to) {
            (PROPOSED, CONFIRMED)
            | (PROPOSED, REJECTED)
            | (PROPOSED, FAILED)
            | (CONFIRMED, RUNNING)
            | (CONFIRMED, FAILED)
            | (RUNNING, COMPLETED)
            | (RUNNING, FAILED) => true,
            _ => false,
        }
    }
}

/// 阈值（与 P1 反思中心共享的阈值直接复用其常量；轮次专属阈值在此）
///
/// 模型层（feature / decision / statistical）自上次重训练后拒绝累计
/// 达到该数 → 建议重训练
pub const L3_ROUND_REJECT_COUNT: i64 = 5;
/// 飞轮确认池规模达到该数（样本够厚）→ 建议重训练
pub const L3_ROUND_POOL_SIZE: i64 = 20;
/// 测评证据：L3 门控 F1 低于该值 → 建议重训练（与 P1 同阈值）
pub const L3_ROUND_GATE_F1_WEAK: f64 = crate::reflection_center::L3_GATE_F1_WEAK;
/// LLM 层采纳（带修正）累计达到该数 → 建议构建 LLM 训练数据
pub const LLM_ROUND_ADOPT_COUNT: i64 = 10;
/// 规则拒绝累计达到该数 → 建议人工调参（停用 / 收紧）
pub const RULE_ROUND_REJECT_COUNT: i64 = 5;
/// 规则拒绝占比阈值（拒绝 / (采纳+拒绝)，仅当独立反馈 ≥ 2 时参与判定）
pub const RULE_ROUND_REJECT_RATIO: f64 = 0.5;

/// 轮次动作（一个 = 一次训练动作 / 人工操作）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoundAction {
    /// 动作类型（[`job_kind::L3_RETRAIN`] / [`job_kind::LEXICON_CONDITION`] /
    /// [`job_kind::LLM_DATA`] / `rule_tune`（人工））
    pub kind: String,
    /// 目标（层名 / 规则 id / 臂名）
    pub target: String,
    /// 触发原因（一句话，含关键数字）
    pub reason: String,
    /// 执行参数（P2 按 kind 解释；与反思建议 payload 同构）
    #[serde(default)]
    pub payload: serde_json::Value,
}

/// 优化轮次提案（[`plan_round`] 的产出）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoundPlan {
    pub lang: String,
    /// 轮次总述（一句话：为什么现在开轮）
    pub reason: String,
    /// 动作集合（按触发强度排序；空 = 不应开轮，`plan_round` 不会返回空动作）
    pub actions: Vec<RoundAction>,
    /// 证据列表（反馈统计 / 池规模 / 测评数字）
    #[serde(default)]
    pub evidence: Vec<String>,
}

/// 自动评估输入（调用方从存储层转换；本模块不依赖 lexicon-store）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RoundInput {
    pub lang: String,
    /// 按（来源层, 规则 id）聚合的反馈统计（**上次轮次 / 重训练之后**的增量）
    #[serde(default)]
    pub feedback_stats: Vec<FeedbackStatIn>,
    /// 反馈明细（同上增量；词表 / LLM 数据动作需要原文与修正）
    #[serde(default)]
    pub feedback_entries: Vec<FeedbackEntryIn>,
    /// 飞轮确认池规模（`l3_data/<lang>/confirmed.jsonl` 行数）
    #[serde(default)]
    pub pool_size: i64,
    /// 上次 L3 重训练时间（`None` = 从未重训练）
    #[serde(default)]
    pub last_retrain_at: Option<String>,
    /// 测评证据（`None` = 无可用报告 / 服务不可达 → 纯反馈判定）
    #[serde(default)]
    pub eval_evidence: Option<crate::reflection_center::EvalEvidence>,
}

/// 自动评估 → 轮次提案
///
/// 综合反馈增量 / 池规模 / 测评证据，按上表阈值产出动作集合；
/// 全部条件不满足 → `None`（无需开轮）。
pub fn plan_round(input: &RoundInput) -> Option<RoundPlan> {
    let mut actions: Vec<RoundAction> = Vec::new();
    let mut evidence: Vec<String> = Vec::new();

    // ── L3 重训练 ────────────────────────────────────────────────────────────
    let model_rejects: i64 = input
        .feedback_stats
        .iter()
        .filter(|s| crate::training_center::is_model_source(&s.source))
        .map(|s| s.rejected)
        .sum();
    let gate_weak = input
        .eval_evidence
        .as_ref()
        .and_then(|ev| {
            ev.layers
                .iter()
                .filter(|l| crate::training_center::is_model_source(&l.name))
                .filter_map(|l| l.gate_f1)
                .min_by(|a, b| a.total_cmp(b))
        })
        .filter(|f1| *f1 < L3_ROUND_GATE_F1_WEAK);

    if model_rejects >= L3_ROUND_REJECT_COUNT {
        evidence.push(format!(
            "模型层拒绝累计 {model_rejects}（阈值 {L3_ROUND_REJECT_COUNT}）"
        ));
    }
    if input.pool_size >= L3_ROUND_POOL_SIZE {
        evidence.push(format!(
            "飞轮确认池 {pool} 条（阈值 {L3_ROUND_POOL_SIZE}）",
            pool = input.pool_size
        ));
    }
    if let Some(f1) = gate_weak {
        evidence.push(format!(
            "测评证据：L3 门控 F1 = {f1:.3}（阈值 {L3_ROUND_GATE_F1_WEAK}）"
        ));
    }
    if !evidence.is_empty() {
        actions.push(RoundAction {
            kind: job_kind::L3_RETRAIN.to_string(),
            target: "feature".into(),
            reason: "模型层负样本 / 确认池积累达到重训练条件".into(),
            payload: serde_json::json!({
                "lang": input.lang,
                "source": "feature",
                "count": model_rejects,
            }),
        });
    }

    // ── LLM 训练数据 ────────────────────────────────────────────────────────
    let llm_adopts: i64 = input
        .feedback_stats
        .iter()
        .filter(|s| s.source == "llm")
        .map(|s| s.adopted)
        .sum();
    if llm_adopts >= LLM_ROUND_ADOPT_COUNT {
        evidence.push(format!(
            "LLM 层采纳（带修正）累计 {llm_adopts}（阈值 {LLM_ROUND_ADOPT_COUNT}）"
        ));
        actions.push(RoundAction {
            kind: job_kind::LLM_DATA.to_string(),
            target: "llm".into(),
            reason: "LLM 层采纳样本积累达到训练数据构建条件".into(),
            payload: serde_json::json!({ "lang": input.lang }),
        });
    }

    // ── 词表条件化 / 规则调参（按规则聚合）──────────────────────────────────
    let mut rule_actions: Vec<RoundAction> = Vec::new();
    let mut seen_rules: std::collections::HashSet<String> = std::collections::HashSet::new();
    for s in &input.feedback_stats {
        let rule_id = match &s.rule_id {
            Some(r) if !r.is_empty() => r.clone(),
            _ => continue,
        };
        if !seen_rules.insert(rule_id.clone()) {
            continue;
        }
        let n = s.adopted + s.rejected;
        let ratio = if n > 0 {
            s.rejected as f64 / n as f64
        } else {
            0.0
        };

        // 词表条件化：拒绝 ≥ P1 阈值且有被拒文本
        if s.rejected >= crate::reflection_center::RULE_REJECT_HIGH_COUNT {
            let mut seen_orig: std::collections::HashSet<String> = std::collections::HashSet::new();
            let originals: Vec<String> = input
                .feedback_entries
                .iter()
                .filter(|f| f.action == "reject" && f.rule_id.as_deref() == Some(&rule_id))
                .filter(|f| !f.original.trim().is_empty())
                .filter(|f| seen_orig.insert(f.original.clone()))
                .map(|f| f.original.clone())
                .collect();
            if !originals.is_empty() {
                evidence.push(format!(
                    "规则 {rule_id} 拒绝 {rej} 次（阈值 {th}），有 {n} 条被拒文本",
                    rej = s.rejected,
                    th = crate::reflection_center::RULE_REJECT_HIGH_COUNT,
                    n = originals.len()
                ));
                rule_actions.push(RoundAction {
                    kind: job_kind::LEXICON_CONDITION.to_string(),
                    target: rule_id.clone(),
                    reason: format!("规则 {rule_id} 误报集中 → 条件化排除"),
                    payload: serde_json::json!({
                        "lang": input.lang,
                        "rule_id": rule_id.clone(),
                        "originals": originals,
                    }),
                });
            }
        }
        // 规则调参（人工）：拒绝 ≥ 阈值，或占比 ≥ 阈值（n≥2）
        if s.rejected >= RULE_ROUND_REJECT_COUNT || (n >= 2 && ratio >= RULE_ROUND_REJECT_RATIO) {
            evidence.push(format!(
                "规则 {rule_id} 拒绝 {rej}/{n}（占比 {pct}%）",
                rej = s.rejected,
                pct = (ratio * 100.0).round() as i64
            ));
            rule_actions.push(RoundAction {
                kind: suggestion_kinds::RULE_TUNE.to_string(),
                target: rule_id.clone(),
                reason: "规则误报率偏高 → 人工停用 / 收紧".into(),
                payload: serde_json::json!({ "lang": input.lang, "rule_id": rule_id }),
            });
        }
    }
    actions.extend(rule_actions);

    if actions.is_empty() {
        return None;
    }
    Some(RoundPlan {
        lang: input.lang.clone(),
        reason: format!(
            "自动评估：{} 类动作触发（{}）",
            actions.len(),
            actions
                .iter()
                .map(|a| a.kind.as_str())
                .collect::<Vec<_>>()
                .join(" + ")
        ),
        actions,
        evidence,
    })
}

/// 「无需开轮」原因（各指标离阈值还差多少；API 在 `plan_round` 返回 `None` 时返回）
pub fn no_action_reasons(input: &RoundInput) -> Vec<String> {
    let model_rejects: i64 = input
        .feedback_stats
        .iter()
        .filter(|s| crate::training_center::is_model_source(&s.source))
        .map(|s| s.rejected)
        .sum();
    let llm_adopts: i64 = input
        .feedback_stats
        .iter()
        .filter(|s| s.source == "llm")
        .map(|s| s.adopted)
        .sum();
    let gate_f1 = input.eval_evidence.as_ref().and_then(|ev| {
        ev.layers
            .iter()
            .filter(|l| crate::training_center::is_model_source(&l.name))
            .filter_map(|l| l.gate_f1)
            .min_by(|a, b| a.total_cmp(b))
    });
    let max_rule_reject: i64 = input
        .feedback_stats
        .iter()
        .filter(|s| s.rule_id.is_some())
        .map(|s| s.rejected)
        .max()
        .unwrap_or(0);

    let mut out = vec![format!(
        "模型层拒绝 {model_rejects} < {L3_ROUND_REJECT_COUNT}；飞轮池 {pool} < {L3_ROUND_POOL_SIZE}；LLM 采纳 {llm_adopts} < {LLM_ROUND_ADOPT_COUNT}",
        pool = input.pool_size
    )];
    match gate_f1 {
        Some(f1) => out.push(format!(
            "L3 门控 F1 = {f1:.3} ≥ {L3_ROUND_GATE_F1_WEAK}（测评证据）"
        )),
        None => out.push("无测评证据（或无模型层门控数据）".to_string()),
    }
    out.push(format!(
        "规则拒绝峰值 {max_rule_reject} < {RULE_ROUND_REJECT_COUNT}（且无占比 ≥ {pct}% 的规则）",
        pct = (RULE_ROUND_REJECT_RATIO * 100.0).round() as i64
    ));
    out
}

/// 轮次报告（聚合视图：背景 + 各动作完整结果 + 执行后测评证据）
///
/// 纯聚合，无网络 / 存储依赖：
/// - `round`：轮次完整行（序列化 JSON；`result.actions[].job_id` 关联作业）
/// - `jobs`：本轮派生的训练作业完整行（任意顺序；按 `job_id` 关联）
/// - `eval_after`：执行后该语言最新测评报告（服务不可达 → `None`，优雅降级）
///
/// 产出 `{round, summary, actions, evidence, eval_after}`：
/// `summary` 是人类可读的中文摘要（各动作一行），`actions` 每项附完整作业
/// （含 `result` 细节——池规模 / SFT 样本数 / 入库条目 / L3 指标）。
pub fn round_report(
    round: &serde_json::Value,
    jobs: &[serde_json::Value],
    eval_after: Option<&crate::reflection_center::EvalEvidence>,
) -> serde_json::Value {
    let id = round.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
    let lang = round.get("lang").and_then(|v| v.as_str()).unwrap_or("");
    let status = round.get("status").and_then(|v| v.as_str()).unwrap_or("");
    let reason = round.get("reason").and_then(|v| v.as_str()).unwrap_or("");

    // 各动作聚合（附完整作业 + 一行摘要）
    //
    // 注意：计划 `actions` 只有 `kind` / `target` / `reason` / `payload`；
    // `job_id` 在执行结果 `result.actions` 里（按 kind 匹配）。
    let mut action_lines: Vec<String> = Vec::new();
    let result_actions = round
        .get("result")
        .and_then(|v| v.get("actions"))
        .and_then(|v| v.as_array());
    let actions_out = round
        .get("actions")
        .and_then(|v| v.as_array())
        .map(|list| {
            list.iter()
                .map(|a| {
                    let kind = a.get("kind").and_then(|v| v.as_str()).unwrap_or("");
                    let mut item = serde_json::to_value(a).unwrap_or_default();
                    // 执行结果（按 kind 匹配）→ job_id → 完整作业
                    let ra = result_actions.and_then(|list| {
                        list.iter()
                            .find(|x| x.get("kind").and_then(|v| v.as_str()) == Some(kind))
                    });
                    if let Some(x) = ra {
                        item["exec"] = x.clone();
                    }
                    let job = ra
                        .and_then(|x| x.get("job_id"))
                        .and_then(|v| v.as_i64())
                        .and_then(|jid| {
                            jobs.iter()
                                .find(|j| j.get("id").and_then(|v| v.as_i64()) == Some(jid))
                        });
                    if let Some(j) = job {
                        item["job"] = j.clone();
                    }
                    // 一行摘要（从作业 result 提取关键数字；无作业 → 未执行）
                    let line = match kind {
                        "l3_retrain" => match job {
                            Some(j)
                                if j.get("status").and_then(|v| v.as_str())
                                    == Some("completed") =>
                            {
                                let extra = j
                                    .get("result")
                                    .and_then(|r| r.get("dev_f05"))
                                    .and_then(|v| v.as_f64())
                                    .map(|f| format!("，dev F0.5 = {f:.3}"))
                                    .unwrap_or_default();
                                format!("L3 重训练完成{extra}")
                            }
                            Some(j)
                                if j.get("status").and_then(|v| v.as_str()) == Some("failed") =>
                            {
                                format!(
                                    "L3 重训练失败（{}）",
                                    j.get("result")
                                        .and_then(|r| r.get("error"))
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("未知错误")
                                )
                            }
                            _ => "L3 重训练（未执行）".to_string(),
                        },
                        "llm_data" => match job {
                            Some(j)
                                if j.get("status").and_then(|v| v.as_str())
                                    == Some("completed") =>
                            {
                                let n = j
                                    .get("result")
                                    .and_then(|r| r.get("n_samples"))
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(0);
                                format!("LLM 训练数据完成（{n} 条 SFT 样本）")
                            }
                            Some(j)
                                if j.get("status").and_then(|v| v.as_str()) == Some("failed") =>
                            {
                                format!(
                                    "LLM 训练数据失败（{}）",
                                    j.get("result")
                                        .and_then(|r| r.get("error"))
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("未知错误")
                                )
                            }
                            _ => "LLM 训练数据（未执行）".to_string(),
                        },
                        "lexicon_condition" => match job {
                            Some(j)
                                if j.get("status").and_then(|v| v.as_str())
                                    == Some("completed") =>
                            {
                                let n = j
                                    .get("result")
                                    .and_then(|r| r.get("applied"))
                                    .and_then(|v| v.as_array())
                                    .map(|a| a.len())
                                    .unwrap_or(0);
                                format!("词表条件化完成（{n} 条排除入库）")
                            }
                            Some(j)
                                if j.get("status").and_then(|v| v.as_str()) == Some("failed") =>
                            {
                                format!(
                                    "词表条件化失败（{}）",
                                    j.get("result")
                                        .and_then(|r| r.get("error"))
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("未知错误")
                                )
                            }
                            _ => "词表条件化（未执行）".to_string(),
                        },
                        "rule_tune" => "规则调参（人工操作：规则面板停用 / 收紧）".to_string(),
                        _ => format!("未知动作 {kind}"),
                    };
                    item["summary"] = serde_json::Value::String(line);
                    item
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    action_lines = actions_out
        .iter()
        .filter_map(|v| {
            v.get("summary")
                .and_then(|s| s.as_str())
                .map(|s| s.to_string())
        })
        .collect();
    // 人类可读总述
    let n_failed = action_lines.iter().filter(|l| l.contains("失败")).count();
    let summary = match status {
        "completed" => format!(
            "轮次 #{id}（{lang}）执行完成：{}。开轮原因：{reason}",
            action_lines.join("；")
        ),
        "failed" => format!(
            "轮次 #{id}（{lang}）执行失败（{n_failed} 个动作失败）：{}。开轮原因：{reason}",
            action_lines.join("；")
        ),
        "running" => format!("轮次 #{id}（{lang}）执行中。开轮原因：{reason}"),
        "proposed" => format!(
            "轮次 #{id}（{lang}）已提案，待人工确认（{}）。开轮原因：{reason}",
            action_lines.join("；")
        ),
        "confirmed" => format!(
            "轮次 #{id}（{lang}）已确认，待执行（{}）。开轮原因：{reason}",
            action_lines.join("；")
        ),
        "rejected" => format!("轮次 #{id}（{lang}）已被人工拒绝。开轮原因：{reason}"),
        _ => format!("轮次 #{id}（{lang}）状态 {status}。开轮原因：{reason}"),
    };

    // 执行后测评证据（完整序列化；服务不可达 → Null）
    let eval_after = eval_after.map(|ev| serde_json::to_value(ev).unwrap_or_default());

    serde_json::json!({
        "round": round,
        "summary": summary,
        "n_actions": actions_out.len(),
        "n_failed": n_failed,
        "actions": actions_out,
        "evidence": round.get("evidence").cloned().unwrap_or_default(),
        "eval_after": eval_after,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stat(source: &str, rule_id: Option<&str>, adopted: i64, rejected: i64) -> FeedbackStatIn {
        FeedbackStatIn {
            source: source.into(),
            rule_id: rule_id.map(|s| s.to_string()),
            adopted,
            rejected,
            last_at: String::new(),
        }
    }

    fn entry(source: &str, rule_id: Option<&str>, original: &str, action: &str) -> FeedbackEntryIn {
        FeedbackEntryIn {
            source: source.into(),
            rule_id: rule_id.map(|s| s.to_string()),
            error_type: String::new(),
            original: original.into(),
            correction: String::new(),
            action: action.into(),
            reason: String::new(),
            created_at: String::new(),
        }
    }

    fn input() -> RoundInput {
        RoundInput {
            lang: "zh".into(),
            ..Default::default()
        }
    }

    #[test]
    fn no_action_when_below_thresholds() {
        let mut inp = input();
        inp.feedback_stats = vec![
            stat("feature", None, 0, 2),
            stat("llm", None, 3, 0),
            // 占比 1/3 = 33% < 50% 且拒绝 1 < 3 → 不触发
            stat("rule", Some("ZH_HOT"), 2, 1),
        ];
        assert!(plan_round(&inp).is_none());
    }

    #[test]
    fn l3_retrain_on_model_rejects() {
        let mut inp = input();
        inp.feedback_stats = vec![stat("feature", None, 0, 3), stat("decision", None, 0, 2)];
        let plan = plan_round(&inp).unwrap();
        let a = plan
            .actions
            .iter()
            .find(|a| a.kind == job_kind::L3_RETRAIN)
            .unwrap();
        assert_eq!(a.payload["count"], 5);
        assert!(plan.evidence.iter().any(|e| e.contains("模型层拒绝累计 5")));
    }

    #[test]
    fn l3_retrain_on_pool_size() {
        let mut inp = input();
        inp.pool_size = 25;
        let plan = plan_round(&inp).unwrap();
        assert!(plan.actions.iter().any(|a| a.kind == job_kind::L3_RETRAIN));
        assert!(plan.evidence.iter().any(|e| e.contains("飞轮确认池 25")));
    }

    #[test]
    fn l3_retrain_on_weak_gate() {
        let mut inp = input();
        inp.eval_evidence = Some(crate::reflection_center::EvalEvidence {
            report_id: "r".into(),
            benchmark: "mucgec".into(),
            arm: "cec3".into(),
            f_beta: 0.8,
            llm_reach: None,
            layers: vec![crate::reflection_center::EvalLayerEvidence {
                name: "feature".into(),
                n_pred: 100,
                f_beta: 0.5,
                tp_overlap: 10,
                fp_overlap: 90,
                recall_share: 0.1,
                gate_f1: Some(0.72),
            }],
        });
        let plan = plan_round(&inp).unwrap();
        assert!(plan.actions.iter().any(|a| a.kind == job_kind::L3_RETRAIN));
        assert!(plan.evidence.iter().any(|e| e.contains("0.720")));
    }
    #[test]
    fn llm_data_on_adopts() {
        let mut inp = input();
        inp.feedback_stats = vec![stat("llm", None, 12, 0)];
        let plan = plan_round(&inp).unwrap();
        assert!(
            plan.actions
                .iter()
                .any(|a| a.kind == job_kind::LLM_DATA && a.target == "llm")
        );
    }

    #[test]
    fn lexicon_condition_on_rule_rejects_with_text() {
        let mut inp = input();
        inp.feedback_stats = vec![stat("rule", Some("ZH_HOT"), 0, 3)];
        inp.feedback_entries = vec![
            entry("rule", Some("ZH_HOT"), "句子甲", "reject"),
            entry("rule", Some("ZH_HOT"), "句子乙", "reject"),
            entry("rule", Some("ZH_HOT"), "句子甲", "reject"), // 去重
        ];
        let plan = plan_round(&inp).unwrap();
        let a = plan
            .actions
            .iter()
            .find(|a| a.kind == job_kind::LEXICON_CONDITION)
            .unwrap();
        assert_eq!(a.target, "ZH_HOT");
        assert_eq!(a.payload["originals"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn rule_tune_on_ratio() {
        let mut inp = input();
        // 1 采纳 2 拒绝 → 占比 67% ≥ 50%（n=3 ≥ 2）
        inp.feedback_stats = vec![stat("rule", Some("ZH_CONFUSABLE"), 1, 2)];
        let plan = plan_round(&inp).unwrap();
        let a = plan
            .actions
            .iter()
            .find(|a| a.kind == suggestion_kinds::RULE_TUNE)
            .unwrap();
        assert_eq!(a.target, "ZH_CONFUSABLE");
        // 拒绝 2 < 3 → 不产生词表条件化动作
        assert!(
            !plan
                .actions
                .iter()
                .any(|a| a.kind == job_kind::LEXICON_CONDITION)
        );
    }

    #[test]
    fn multiple_actions_and_dedup_per_rule() {
        let mut inp = input();
        inp.feedback_stats = vec![
            stat("feature", None, 0, 6),
            stat("rule", Some("R1"), 0, 3),
            stat("rule", Some("R1"), 0, 4), // 同规则 → 合并
            stat("rule", Some("R2"), 0, 5),
        ];
        inp.feedback_entries = vec![
            entry("rule", Some("R1"), "甲", "reject"),
            entry("rule", Some("R2"), "乙", "reject"),
        ];
        let plan = plan_round(&inp).unwrap();
        let n_lex: usize = plan
            .actions
            .iter()
            .filter(|a| a.kind == job_kind::LEXICON_CONDITION)
            .count();
        let n_tune: usize = plan
            .actions
            .iter()
            .filter(|a| a.kind == suggestion_kinds::RULE_TUNE)
            .count();
        assert_eq!(n_lex, 2, "每规则至多一个条件化动作");
        assert_eq!(n_tune, 2, "每规则至多一个调参动作");
        assert!(plan.actions.iter().any(|a| a.kind == job_kind::L3_RETRAIN));
    }

    #[test]
    fn no_action_reasons_lists_gaps() {
        let mut inp = input();
        inp.feedback_stats = vec![stat("feature", None, 0, 2), stat("llm", None, 3, 0)];
        inp.pool_size = 8;
        let reasons = no_action_reasons(&inp);
        assert!(reasons.iter().any(|r| r.contains("模型层拒绝 2 < 5")));
        assert!(reasons.iter().any(|r| r.contains("飞轮池 8 < 20")));
        assert!(reasons.iter().any(|r| r.contains("LLM 采纳 3 < 10")));
        assert!(reasons.iter().any(|r| r.contains("无测评证据")));
    }

    #[test]
    fn round_status_machine() {
        use round_status::*;
        assert!(can_transition(PROPOSED, CONFIRMED));
        assert!(can_transition(PROPOSED, REJECTED));
        assert!(can_transition(CONFIRMED, RUNNING));
        assert!(can_transition(RUNNING, COMPLETED));
        assert!(can_transition(RUNNING, FAILED));
        assert!(!can_transition(CONFIRMED, REJECTED), "确认后不能走拒绝门");
        assert!(!can_transition(COMPLETED, RUNNING));
        assert!(!can_transition(REJECTED, CONFIRMED));
        assert!(!can_transition(RUNNING, CONFIRMED));
    }
    fn sample_round(status: &str) -> serde_json::Value {
        serde_json::json!({
            "id": 7,
            "lang": "zh",
            "status": status,
            "reason": "模型层拒绝 5 ≥ 5；LLM 采纳 10 ≥ 10；规则 ZH_CONFUSABLE 拒绝 3 ≥ 3",
            "evidence": [{"source": "feedback", "model_rejects": 5}],
            "actions": [
                {"kind": "l3_retrain", "target": "feature", "reason": "模型层拒绝 5 ≥ 5"},
                {"kind": "llm_data", "target": "llm", "reason": "LLM 采纳 10 ≥ 10"},
                {"kind": "lexicon_condition", "target": "ZH_CONFUSABLE", "reason": "规则拒绝 3 ≥ 3"},
                {"kind": "rule_tune", "target": "ZH_CONFUSABLE", "reason": "规则拒绝 3 ≥ 5 或占比"}
            ],
            "result": null,
            "created_at": "2026-10-17T10:00:00Z",
            "updated_at": "2026-10-17T10:05:00Z"
        })
    }

    fn sample_jobs() -> Vec<serde_json::Value> {
        vec![
            serde_json::json!({
                "id": 11, "lang": "zh", "kind": "l3_retrain", "status": "completed",
                "result": {"dev_f05": 0.812, "model": "v2"},
                "created_at": "2026-10-17T10:01:00Z", "updated_at": "2026-10-17T10:02:00Z"
            }),
            serde_json::json!({
                "id": 12, "lang": "zh", "kind": "llm_data", "status": "completed",
                "result": {"n_samples": 10, "jsonl": "..."},
                "created_at": "2026-10-17T10:01:00Z", "updated_at": "2026-10-17T10:02:00Z"
            }),
            serde_json::json!({
                "id": 13, "lang": "zh", "kind": "lexicon_condition", "status": "failed",
                "result": {"error": "词表文件不存在"},
                "created_at": "2026-10-17T10:01:00Z", "updated_at": "2026-10-17T10:02:00Z"
            }),
        ]
    }

    /// 完成态报告（部分动作失败 → 轮次 failed）：摘要 + 各动作完整作业 + 执行后测评证据
    #[test]
    fn report_completed_with_eval_after() {
        let mut round = sample_round("failed");
        round["result"] = serde_json::json!({"actions": [
            {"kind": "l3_retrain", "job_id": 11, "status": "completed"},
            {"kind": "llm_data", "job_id": 12, "status": "completed"},
            {"kind": "lexicon_condition", "job_id": 13, "status": "failed", "error": "词表文件不存在"},
            {"kind": "rule_tune", "status": "human_required"}
        ]});
        let ev = crate::reflection_center::EvalEvidence {
            report_id: "20261017T100600Z_mucgec_cec3_capability".into(),
            benchmark: "mucgec".into(),
            arm: "cec3".into(),
            f_beta: 0.81,
            ..Default::default()
        };
        let report = round_report(&round, &sample_jobs(), Some(&ev));
        assert_eq!(report["n_actions"], 4);
        assert_eq!(report["n_failed"], 1);
        let s = report["summary"].as_str().unwrap();
        assert!(s.contains("执行失败（1 个动作失败）"), "{s}");
        assert!(s.contains("dev F0.5 = 0.812"), "{s}");
        assert!(s.contains("10 条 SFT 样本"), "{s}");
        assert!(s.contains("词表条件化失败（词表文件不存在）"), "{s}");
        assert!(s.contains("规则调参（人工操作"), "{s}");
        // 各动作附完整作业
        let a0 = &report["actions"][0];
        assert_eq!(a0["job"]["id"], 11);
        assert_eq!(a0["job"]["result"]["dev_f05"], 0.812);
        assert_eq!(a0["summary"], "L3 重训练完成，dev F0.5 = 0.812");
        // 执行后测评证据（完整序列化）
        assert_eq!(
            report["eval_after"]["report_id"],
            "20261017T100600Z_mucgec_cec3_capability"
        );
        assert_eq!(report["eval_after"]["f_beta"], 0.81);
        // 背景保留
        assert_eq!(report["round"]["id"], 7);
        assert_eq!(report["evidence"][0]["model_rejects"], 5);
    }

    /// 提案态报告：待确认摘要 + 无作业 + 无测评证据
    #[test]
    fn report_proposed() {
        let round = sample_round("proposed");
        let report = round_report(&round, &[], None);
        assert_eq!(report["n_actions"], 4);
        assert_eq!(report["n_failed"], 0);
        let s = report["summary"].as_str().unwrap();
        assert!(s.contains("已提案，待人工确认"), "{s}");
        assert!(s.contains("L3 重训练（未执行）"), "{s}");
        // 无 job_id 关联（actions 无作业）
        assert!(report["actions"][0].get("job").is_none() || report["actions"][0]["job"].is_null());
        assert!(report["eval_after"].is_null());
    }

    /// 拒绝态报告
    #[test]
    fn report_rejected() {
        let round = sample_round("rejected");
        let report = round_report(&round, &[], None);
        assert_eq!(
            report["summary"].as_str().unwrap(),
            "轮次 #7（zh）已被人工拒绝。开轮原因：模型层拒绝 5 ≥ 5；LLM 采纳 10 ≥ 10；规则 ZH_CONFUSABLE 拒绝 3 ≥ 3"
        );
    }
}
