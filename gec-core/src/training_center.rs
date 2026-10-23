//! 训练中心（进化中心 P2）确定性核心
//!
//! 反思中心（P1）产出建议队列后，训练中心负责「消费建议 → 执行训练动作」：
//!
//! | 反思建议 kind | 训练作业 kind | 动作 |
//! |---|---|---|
//! | `l3_sample` | [`job_kind::L3_RETRAIN`] | 负样本汇入 L3 飞轮 → 重训练 → dev 评测 → 热切换 |
//! | `lexicon_condition` | [`job_kind::LEXICON_CONDITION`] | 条件化排除条目入库（入库前先过测评消融） |
//! | `llm_arm` | [`job_kind::LLM_DATA`] | 构建 LLM 训练数据（自动训练 / 导出外部训练） |
//! | `rule_tune` | —（`None`） | 规则调参是人工操作，不产生训练作业 |
//!
//! 另有两个不依赖建议的独立作业：
//! - [`job_kind::DOMAIN_POOL`]：领域池构建（反馈 + 飞轮样本 + 增强器，
//!   互补单侧分配——每句只贡献一个标签，避免矛盾对把模型退化为先验）；
//! - [`job_kind::LLM_DATA`]：LLM 训练数据（SFT 格式 JSONL）。
//!
//! 本模块保持纯数据 + 纯函数（不依赖 lexicon-store / 网络）；
//! 持久化（`TrainingCenterStore`）与外部服务调用（feature-model / 测评）
//! 在 api-server / desktop-app 层完成。

use serde::{Deserialize, Serialize};

use crate::reflection_center::ReflectionSuggestion;

/// 训练作业类型
pub mod job_kind {
    /// 领域池构建（反馈 + 飞轮样本 + 增强器）
    pub const DOMAIN_POOL: &str = "domain_pool";
    /// L3 重训练（飞轮：合并确认样本 → 重训练 → dev 评测 → 热切换）
    pub const L3_RETRAIN: &str = "l3_retrain";
    /// 词表条件化入库（条件化排除条目）
    pub const LEXICON_CONDITION: &str = "lexicon_condition";
    /// LLM 训练数据构建（SFT JSONL；自动训练 / 导出外部训练）
    pub const LLM_DATA: &str = "llm_data";

    /// 合法类型集合
    pub const ALL: [&str; 4] = [DOMAIN_POOL, L3_RETRAIN, LEXICON_CONDITION, LLM_DATA];
}

/// 训练作业状态（状态机：`proposed` → `confirmed` → `running` → `completed` / `failed`）
pub mod job_status {
    /// 已提案（作业创建，尚未执行）
    pub const PROPOSED: &str = "proposed";
    /// 已确认（P3 优化轮次的人工确认门；P2 本地作业可直接 running）
    pub const CONFIRMED: &str = "confirmed";
    /// 执行中
    pub const RUNNING: &str = "running";
    /// 已完成（结果在 `result`）
    pub const COMPLETED: &str = "completed";
    /// 失败（原因在 `result.error`）
    pub const FAILED: &str = "failed";

    /// 合法状态集合
    pub const ALL: [&str; 5] = [PROPOSED, CONFIRMED, RUNNING, COMPLETED, FAILED];

    /// 状态迁移是否合法
    ///
    /// `proposed → confirmed / failed`；`confirmed → running / failed`；
    /// `running → completed / failed`。终态（completed / failed）不可再迁移。
    pub fn can_transition(from: &str, to: &str) -> bool {
        match (from, to) {
            (PROPOSED, CONFIRMED)
            | (PROPOSED, FAILED)
            | (CONFIRMED, RUNNING)
            | (CONFIRMED, FAILED)
            | (RUNNING, COMPLETED)
            | (RUNNING, FAILED) => true,
            _ => false,
        }
    }
}
/// 领域池样本
///
/// **互补单侧分配**：每句只贡献一个标签（error 或 clean）。若同一对的
/// 平行句双侧都入池（源句=error / 纠正句=clean），两句近乎重复而标签相反，
/// 聚合特征无法区分 → 模型退化为先验（`feature-model/prepare_l3_pool.py`
/// 已实验验证）。因此同一句文本无论来自哪个来源，只保留第一个标签。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolSample {
    pub sentence: String,
    /// `error`（有错，可带修正）/ `clean`（干净）
    pub label: String,
    /// 错误类型（label=error 时有值）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,
    /// 修正文本（label=error 且来源带修正时有值）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correction: Option<String>,
    /// 来源：`feedback_adopt` / `feedback_reject` / `flywheel` / `augment`
    pub source: String,
}

/// 领域池（一次构建的完整产出）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainPool {
    pub lang: String,
    pub samples: Vec<PoolSample>,
    pub total: usize,
    pub error_count: usize,
    pub clean_count: usize,
    /// 按来源统计（`(source, count)`，count 降序）
    pub by_source: Vec<(String, usize)>,
}

/// 领域池构建输入（纯数据；调用方从 `FeedbackLogStore` / 飞轮文件 /
/// 用户语料转换——本模块不依赖存储层）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PoolInput {
    pub lang: String,
    /// 反馈明细（模型层 adopt/reject 会转成样本；其余层忽略）
    #[serde(default)]
    pub feedback: Vec<crate::FeedbackEntryIn>,
    /// 飞轮确认样本（`l3_data/<lang>/confirmed.jsonl`，label 已定）
    #[serde(default)]
    pub flywheel: Vec<PoolSample>,
    /// 增强器：干净句（用户自有语料——避免基准集污染）
    #[serde(default)]
    pub clean_sentences: Vec<String>,
}

/// 模型层（L3 门控相关：feature / decision / statistical）
pub fn is_model_source(source: &str) -> bool {
    matches!(source, "feature" | "decision" | "statistical")
}

/// 构建领域池
///
/// 样本来源（按序合并，**句级去重**——互补单侧分配，保留首个标签）：
/// 1. 模型层反馈 `adopt` → `error` 样本（用户确认错误真实；带修正）；
/// 2. 模型层反馈 `reject` → `clean` 样本（用户判定模型误报 → 句子实际干净）；
/// 3. 飞轮确认样本 → 原样（label 已定）；
/// 4. 增强器干净句 → `clean` 样本。
pub fn build_domain_pool(input: &PoolInput) -> DomainPool {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut samples: Vec<PoolSample> = Vec::new();

    let push = |s: PoolSample,
                seen: &mut std::collections::HashSet<String>,
                samples: &mut Vec<PoolSample>| {
        let key = s.sentence.clone();
        if seen.insert(key) {
            samples.push(s);
        }
    };

    for f in &input.feedback {
        if !is_model_source(&f.source) {
            continue;
        }
        if f.original.trim().is_empty() {
            continue;
        }
        match f.action.as_str() {
            "adopt" => push(
                PoolSample {
                    sentence: f.original.clone(),
                    label: "error".into(),
                    error_type: (!f.error_type.is_empty()).then(|| f.error_type.clone()),
                    correction: (!f.correction.is_empty()).then(|| f.correction.clone()),
                    source: "feedback_adopt".into(),
                },
                &mut seen,
                &mut samples,
            ),
            "reject" => push(
                PoolSample {
                    sentence: f.original.clone(),
                    label: "clean".into(),
                    error_type: None,
                    correction: None,
                    source: "feedback_reject".into(),
                },
                &mut seen,
                &mut samples,
            ),
            _ => {}
        }
    }

    for s in &input.flywheel {
        push(s.clone(), &mut seen, &mut samples);
    }

    for c in &input.clean_sentences {
        if c.trim().is_empty() {
            continue;
        }
        push(
            PoolSample {
                sentence: c.clone(),
                label: "clean".into(),
                error_type: None,
                correction: None,
                source: "augment".into(),
            },
            &mut seen,
            &mut samples,
        );
    }

    let error_count = samples.iter().filter(|s| s.label == "error").count();
    let clean_count = samples.len() - error_count;
    let total = samples.len();
    let mut by_source: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for s in &samples {
        *by_source.entry(s.source.as_str()).or_insert(0) += 1;
    }
    let mut by_source: Vec<(String, usize)> = by_source
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    by_source.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    DomainPool {
        lang: input.lang.clone(),
        samples,
        total,
        error_count,
        clean_count,
        by_source,
    }
}

/// LLM 训练数据样本（SFT 格式：句子 + 错误类型 + 修正）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmTrainingSample {
    pub sentence: String,
    pub error_type: String,
    pub correction: String,
}

/// LLM 训练数据（一次构建的完整产出；`jsonl` 可直接落盘 / 导出外部训练）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmTrainingData {
    pub lang: String,
    /// 当前 LLM 臂（`cec3` / `cec4` / `custom`；未知时为空）
    pub arm: String,
    pub samples: Vec<LlmTrainingSample>,
    /// JSONL（每行一个 JSON 对象）
    pub jsonl: String,
}

/// 构建 LLM 训练数据
///
/// 取 LLM 层 `adopt` 反馈（用户确认 LLM 检出的错误真实、且带修正）→
/// SFT 样本。`reject` 反馈不进训练数据（那是换臂 / 评估的证据，不是
/// 训练信号）；无修正的 adopt 跳过（SFT 需要目标输出）。
pub fn build_llm_training_data(
    lang: &str,
    arm: &str,
    feedback: &[crate::FeedbackEntryIn],
) -> LlmTrainingData {
    let mut samples: Vec<LlmTrainingSample> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for f in feedback {
        if f.source != "llm" || f.action != "adopt" {
            continue;
        }
        if f.original.trim().is_empty() || f.correction.trim().is_empty() {
            continue;
        }
        if !seen.insert(f.original.clone()) {
            continue;
        }
        samples.push(LlmTrainingSample {
            sentence: f.original.clone(),
            error_type: f.error_type.clone(),
            correction: f.correction.clone(),
        });
    }
    let jsonl = samples
        .iter()
        .map(|s| {
            serde_json::to_string(&serde_json::json!({
                "sentence": s.sentence,
                "error_type": s.error_type,
                "correction": s.correction,
            }))
            .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join("\n");
    LlmTrainingData {
        lang: lang.to_string(),
        arm: arm.to_string(),
        samples,
        jsonl,
    }
}

/// 训练作业（一行 = 一次训练动作的完整记录）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrainingJob {
    /// 作业类型，见 [`job_kind`]
    pub kind: String,
    pub lang: String,
    /// 状态，见 [`job_status`]（初始 `proposed`）
    pub status: String,
    /// 来源反思建议 id（`None` = 手动发起）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_suggestion: Option<i64>,
    /// 执行参数（按 kind 解释；来自建议 payload 或请求体）
    #[serde(default)]
    pub payload: serde_json::Value,
    /// 执行结果（completed / failed 后填充；failed 时含 `error` 字段）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    pub created_at: String,
    pub updated_at: String,
}

/// 反思建议 → 训练作业（P1 → P2 桥）
///
/// - `l3_sample` → [`job_kind::L3_RETRAIN`]（payload 透传：source / count / originals）
/// - `lexicon_condition` → [`job_kind::LEXICON_CONDITION`]（payload 透传：rule_id / originals）
/// - `llm_arm` → [`job_kind::LLM_DATA`]（payload 透传：current_arm / count）
/// - `rule_tune` → `None`（规则调参是人工操作，不产生训练作业）
pub fn plan_from_suggestion(suggestion: &ReflectionSuggestion) -> Option<TrainingJob> {
    let kind = match suggestion.kind.as_str() {
        "l3_sample" => job_kind::L3_RETRAIN,
        "lexicon_condition" => job_kind::LEXICON_CONDITION,
        "llm_arm" => job_kind::LLM_DATA,
        _ => return None,
    };
    Some(TrainingJob {
        kind: kind.to_string(),
        lang: suggestion
            .payload
            .get("lang")
            .and_then(|v| v.as_str())
            .unwrap_or(&suggestion.target)
            .to_string(),
        status: job_status::PROPOSED.to_string(),
        source_suggestion: None, // 由调用方（存储层）回填
        payload: suggestion.payload.clone(),
        result: None,
        // 时间戳由存储层落库时填充（核心保持纯函数）
        created_at: String::new(),
        updated_at: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fb(source: &str, action: &str, original: &str, correction: &str) -> crate::FeedbackEntryIn {
        crate::FeedbackEntryIn {
            source: source.into(),
            rule_id: None,
            error_type: if source == "llm" {
                "grammar".into()
            } else {
                String::new()
            },
            original: original.into(),
            correction: correction.into(),
            action: action.into(),
            reason: String::new(),
            created_at: "2026-10-17T00:00:00Z".into(),
        }
    }

    #[test]
    fn job_status_transitions() {
        assert!(job_status::can_transition("proposed", "confirmed"));
        assert!(job_status::can_transition("proposed", "failed"));
        assert!(job_status::can_transition("confirmed", "running"));
        assert!(job_status::can_transition("confirmed", "failed"));
        assert!(job_status::can_transition("running", "completed"));
        assert!(job_status::can_transition("running", "failed"));
        // 终态不可再迁移
        assert!(!job_status::can_transition("completed", "running"));
        assert!(!job_status::can_transition("failed", "proposed"));
        // 跳跃不合法
        assert!(!job_status::can_transition("proposed", "running"));
        assert!(!job_status::can_transition("proposed", "completed"));
        assert!(!job_status::can_transition("running", "confirmed"));
    }

    #[test]
    fn pool_from_model_feedback() {
        let input = PoolInput {
            lang: "zh".into(),
            feedback: vec![
                fb("feature", "adopt", "这句话有错", "这句话有错误"),
                fb("feature", "reject", "这句没问题", ""),
                fb("decision", "reject", "另一句", ""),
                // 非模型层 → 不进池
                fb("rule", "adopt", "规则句", "修正"),
                fb("llm", "adopt", "llm句", "llm修正"),
            ],
            flywheel: vec![],
            clean_sentences: vec![],
        };
        let pool = build_domain_pool(&input);
        assert_eq!(pool.total, 3);
        assert_eq!(pool.error_count, 1);
        assert_eq!(pool.clean_count, 2);
        let adopt = pool
            .samples
            .iter()
            .find(|s| s.source == "feedback_adopt")
            .unwrap();
        assert_eq!(adopt.label, "error");
        assert_eq!(adopt.correction.as_deref(), Some("这句话有错误"));
        let reject = pool
            .samples
            .iter()
            .find(|s| s.source == "feedback_reject")
            .unwrap();
        assert_eq!(reject.label, "clean");
        assert!(reject.correction.is_none());
        assert_eq!(pool.by_source[0].0, "feedback_reject"); // clean 2 > error 1
        assert_eq!(pool.by_source[0].1, 2);
    }

    #[test]
    fn pool_dedup_complementary_one_sided() {
        // 同一句：adopt（error）先到 → 后来的 reject（clean）被去重丢弃
        let input = PoolInput {
            lang: "zh".into(),
            feedback: vec![
                fb("feature", "adopt", "重复句", "修正后"),
                fb("feature", "reject", "重复句", ""),
            ],
            flywheel: vec![],
            clean_sentences: vec![],
        };
        let pool = build_domain_pool(&input);
        assert_eq!(pool.total, 1);
        assert_eq!(pool.samples[0].label, "error");
        assert_eq!(pool.samples[0].source, "feedback_adopt");
    }

    #[test]
    fn pool_flywheel_and_augment() {
        let input = PoolInput {
            lang: "en".into(),
            feedback: vec![],
            flywheel: vec![
                PoolSample {
                    sentence: "flywheel err".into(),
                    label: "error".into(),
                    error_type: Some("collocation".into()),
                    correction: Some("fixed".into()),
                    source: "flywheel".into(),
                },
                PoolSample {
                    sentence: "flywheel clean".into(),
                    label: "clean".into(),
                    error_type: None,
                    correction: None,
                    source: "flywheel".into(),
                },
            ],
            clean_sentences: vec!["augment one".into(), "augment two".into(), "  ".into()],
        };
        let pool = build_domain_pool(&input);
        assert_eq!(pool.total, 4); // 空字符串增强被跳过
        assert_eq!(pool.error_count, 1);
        assert_eq!(pool.clean_count, 3);
        assert_eq!(
            pool.by_source
                .iter()
                .filter(|(k, _)| k == "flywheel")
                .next()
                .unwrap()
                .1,
            2
        );
        assert_eq!(
            pool.by_source
                .iter()
                .filter(|(k, _)| k == "augment")
                .next()
                .unwrap()
                .1,
            2
        );
    }

    #[test]
    fn pool_empty_input() {
        let pool = build_domain_pool(&PoolInput::default());
        assert_eq!(pool.total, 0);
        assert!(pool.samples.is_empty());
        assert!(pool.by_source.is_empty());
    }

    #[test]
    fn llm_data_from_adopted_llm_feedback() {
        let feedback = vec![
            fb("llm", "adopt", "句子A", "修正A"),
            fb("llm", "reject", "句子B", "不该有修正"),
            fb("llm", "adopt", "句子C", ""),          // 无修正 → 跳过
            fb("feature", "adopt", "句子D", "修正D"), // 非 llm 层 → 跳过
            fb("llm", "adopt", "句子A", "重复"),      // 去重
        ];
        let data = build_llm_training_data("zh", "cec3", &feedback);
        assert_eq!(data.samples.len(), 1);
        assert_eq!(data.samples[0].sentence, "句子A");
        assert_eq!(data.samples[0].correction, "修正A");
        assert_eq!(data.arm, "cec3");
        let v: serde_json::Value = serde_json::from_str(&data.jsonl).unwrap();
        assert_eq!(v["sentence"], "句子A");
        assert_eq!(v["correction"], "修正A");
    }

    #[test]
    fn llm_data_empty() {
        let data = build_llm_training_data("zh", "", &[]);
        assert!(data.samples.is_empty());
        assert!(data.jsonl.is_empty());
    }

    fn suggestion(kind: &str, payload: serde_json::Value) -> ReflectionSuggestion {
        ReflectionSuggestion {
            kind: kind.into(),
            target: "feature".into(),
            summary: "测试建议".into(),
            evidence: vec![],
            payload,
            priority: 1,
        }
    }

    #[test]
    fn plan_maps_suggestion_kinds() {
        let job = plan_from_suggestion(&suggestion(
            "l3_sample",
            serde_json::json!({ "lang": "zh", "source": "feature", "count": 3 }),
        ))
        .unwrap();
        assert_eq!(job.kind, job_kind::L3_RETRAIN);
        assert_eq!(job.lang, "zh");
        assert_eq!(job.status, job_status::PROPOSED);
        assert_eq!(job.payload["count"], 3);

        let job = plan_from_suggestion(&suggestion(
            "lexicon_condition",
            serde_json::json!({ "lang": "zh", "rule_id": "ZH_HOT", "originals": ["a"] }),
        ))
        .unwrap();
        assert_eq!(job.kind, job_kind::LEXICON_CONDITION);

        let job = plan_from_suggestion(&suggestion(
            "llm_arm",
            serde_json::json!({ "lang": "en", "current_arm": "cec3" }),
        ))
        .unwrap();
        assert_eq!(job.kind, job_kind::LLM_DATA);
        assert_eq!(job.lang, "en");
    }

    #[test]
    fn plan_rule_tune_is_none() {
        assert!(
            plan_from_suggestion(&suggestion(
                "rule_tune",
                serde_json::json!({ "lang": "zh", "rule_id": "ZH_HOT" })
            ))
            .is_none()
        );
    }

    #[test]
    fn plan_lang_falls_back_to_target() {
        let job = plan_from_suggestion(&suggestion("l3_sample", serde_json::json!({}))).unwrap();
        assert_eq!(job.lang, "feature"); // payload 无 lang → 用 target
    }

    #[test]
    fn job_roundtrip() {
        let job = plan_from_suggestion(&suggestion(
            "l3_sample",
            serde_json::json!({ "lang": "zh" }),
        ))
        .unwrap();
        let s = serde_json::to_string(&job).unwrap();
        let back: TrainingJob = serde_json::from_str(&s).unwrap();
        assert_eq!(back.kind, job_kind::L3_RETRAIN);
        assert!(back.source_suggestion.is_none());
        assert!(back.result.is_none());
    }
}
