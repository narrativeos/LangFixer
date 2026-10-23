//! 反思引擎：用户「放弃」检测结果后的偏差诊断
//!
//! 反思流程是自学习飞轮的**反向路径**：
//! - 采纳 → 沉淀为规则 / 知识 / 模式（让系统更强）；
//! - 放弃 → 反思判断来源哪里出了偏差——
//!   - **规则层**：哪条规则误报？该规则历史采纳/拒绝比例如何？
//!     → 建议停用规则 / 删除具体示例 / 到规则面板查看；
//!   - **知识层**：哪条知识条目误召回（模糊匹配 / 上下文不符）？
//!     → 建议删除该条目；
//!   - **模型层**（decision / feature / statistical）：偏差在模型本身，
//!     无逐词规则可归因 → 自动汇入 L3 飞轮负样本，积累后建议重训练。
//!
//! 本模块是**确定性核心**（无 LLM 依赖，离线可用）；LLM 深度反思由
//! API 层 / 桌面端在配置了 LLM 时另行调用（`prompts/{lang}_reflect` 模板），
//! 产出自然语言诊断追加到 [`ReflectionReport::llm_analysis`]。
//!
//! 本模块只产出**报告与建议**（纯数据、无副作用）；建议的执行
//! （停用规则 / 排除词 / 删示例 / 删知识 / L3 负样本）由调用方按 [`suggestion_kinds`] 分发。

use serde::{Deserialize, Serialize};

use shared_model::Error;

/// 反思建议类型（`ReflectionSuggestion::kind`）
pub mod suggestion_kinds {
    /// 停用规则（payload：`{lang, rule_id}`）
    pub const DISABLE_RULE: &str = "disable_rule";
    /// 删除规则中的具体示例对（payload：`{lang, rule_id, wrong, right}`）
    pub const REMOVE_EXAMPLE: &str = "remove_example";
    /// 删除知识条目（payload：`{id, text}`）
    pub const REMOVE_KNOWLEDGE: &str = "remove_knowledge";
    /// 汇入 L3 飞轮负样本（payload：`{lang, text, error_type}`）
    pub const L3_NEGATIVE: &str = "l3_negative";
    /// 将某词加入规则排除名单（payload：`{lang, rule_id, word}`）：
    /// 写回词表 `rules[].excludes` + 重建检测器，规则保持启用、仅该词不再触发
    /// （典型：POS 把名词误标为动词，如「靠山」被判缺主语）
    pub const EXCLUDE_WORD: &str = "exclude_word";
    /// 建议重训练（payload：`{lang}`；积累到阈值后由设置界面触发）
    pub const RETRAIN: &str = "retrain";
    /// 到规则面板查看该规则（payload：`{lang, rule_id}`）
    pub const VIEW_RULE: &str = "view_rule";
}

/// 规则误报率诊断阈值（按**去重文本数**计：同一文本反复放弃只算 1 次）：
/// 独立拒绝文本 ≥ 该次数，或（独立反馈 ≥ 2 条时）拒绝占比 ≥ [`REJECT_HIGH_RATIO`]
/// → 判定「误报率偏高，建议停用 / 收紧」；否则视为孤立误判，建议保持启用
pub const REJECT_HIGH_COUNT: i64 = 3;
/// 拒绝占比阈值（拒绝 / (采纳 + 拒绝)；仅当独立反馈总数 ≥ 2 时参与判定，
/// 避免「1 条反馈 = 100% 占比」的样本不足误判）
pub const REJECT_HIGH_RATIO: f64 = 0.5;

/// 可执行建议（前端渲染为一键按钮，经 `/gec/feedback/action` 执行）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReflectionSuggestion {
    /// 建议类型，见 [`suggestion_kinds`]
    pub kind: String,
    /// 按钮文案
    pub label: String,
    /// 执行参数（由调用方按 kind 解释）
    pub payload: serde_json::Value,
}

impl ReflectionSuggestion {
    fn new(kind: &str, label: impl Into<String>, payload: serde_json::Value) -> Self {
        Self {
            kind: kind.to_string(),
            label: label.into(),
            payload,
        }
    }
}

/// 反思报告
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReflectionReport {
    /// 判断来源层（`rule` / `knowledge` / `decision` / `feature` / `statistical` / `llm`）
    pub source: String,
    /// 具体对象（规则 id / 知识条目 / 模型层名）
    pub subject: String,
    /// 一句话诊断：偏差从哪来
    pub diagnosis: String,
    /// 证据列表（规则定义 / 反馈统计 / 知识条目来源 …）
    pub evidence: Vec<String>,
    /// 可执行建议
    pub suggestions: Vec<ReflectionSuggestion>,
    /// LLM 深度反思（可选；API 层 / 桌面端配置 LLM 后追加）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_analysis: Option<String>,
}

/// 规则定义摘要（API 层 / 桌面端从词表存储查询后转换；
/// 本模块不依赖 lexicon-store，保持纯数据）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RuleDefInfo {
    pub id: String,
    pub category: String,
    #[serde(default)]
    pub description: String,
    pub enabled: bool,
    /// 该规则示例对总数
    #[serde(default)]
    pub example_count: usize,
}

/// 命中的规则示例（被放弃的 original 正是该规则的某条示例时提供）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExampleInfo {
    pub wrong: String,
    pub right: String,
    #[serde(default)]
    pub source: String,
}

/// 知识条目摘要（知识层反思：定位到具体条目）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KnowledgeInfo {
    pub id: i64,
    pub text: String,
    pub error_type: String,
    #[serde(default)]
    pub correction: String,
    /// 条目沉淀来源（`llm_adopt` / `llm_distill` / `manual` …）
    #[serde(default)]
    pub source: String,
}

/// 反思上下文（调用方按可用存储组装）
pub struct ReflectionContext<'a> {
    /// 被放弃的错误
    pub error: &'a Error,
    /// 用户可选的放弃原因
    pub reason: Option<&'a str>,
    /// 规则层：命中的规则定义（`error.rule_id` 查词表存储）
    pub rule_def: Option<RuleDefInfo>,
    /// 规则层：该规则历史反馈 `(采纳文本数, 拒绝文本数)`（按 original 去重，
    /// 同一文本反复放弃只计 1；含本次）
    pub rule_feedback: Option<(i64, i64)>,
    /// 规则层：被放弃的 original 是否正是该规则的某条示例（定位到具体示例）
    pub matched_example: Option<ExampleInfo>,
    /// 知识层：命中的知识条目（按文本在知识存储中定位）
    pub knowledge_item: Option<KnowledgeInfo>,
}

/// 错误来源归一：规则引擎产出的错误不带 `source` 字段（仅 decision /
/// knowledge / feature / llm 层设置）→ 缺失视为规则层
pub fn normalize_source(error: &Error) -> &str {
    error
        .source
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(shared_model::sources::RULE)
}

/// 执行反思：诊断被放弃错误的判断来源偏差，产出报告 + 可执行建议
///
/// 纯函数（无副作用）：诊断阈值与文案见各分支注释；建议的执行由调用方
/// 按 [`suggestion_kinds`] 分发（停用规则 / 排除词 / 删示例 / 删知识 / L3 负样本）。
pub fn reflect_rejection(ctx: &ReflectionContext) -> ReflectionReport {
    let source = normalize_source(ctx.error).to_string();
    match source.as_str() {
        shared_model::sources::RULE => reflect_rule(ctx),
        shared_model::sources::KNOWLEDGE => reflect_knowledge(ctx),
        shared_model::sources::DECISION
        | shared_model::sources::FEATURE
        | shared_model::sources::STATISTICAL => reflect_model(ctx, &source),
        // LLM 兜底层：无规则 / 知识对象可归因，仅留痕（白名单由调用方完成）
        shared_model::sources::LLM => ReflectionReport {
            source,
            subject: "LLM 兜底层".to_string(),
            diagnosis: format!(
                "该判断由 LLM 兜底层产出，无规则 / 知识条目可归因；「{}」已加入白名单，后续检测不再报告",
                ctx.error.original
            ),
            evidence: vec![format!("错误类型：{}", ctx.error.error_type)],
            suggestions: Vec::new(),
            llm_analysis: None,
        },
        _ => ReflectionReport {
            source,
            subject: "未知来源".to_string(),
            diagnosis: "该错误来源未识别，无法进一步归因；已加入白名单".to_string(),
            evidence: Vec::new(),
            suggestions: Vec::new(),
            llm_analysis: None,
        },
    }
}

/// 规则层反思：定位规则 + 反馈统计 → 诊断误报率 + 建议（停用 / 删示例 / 查看）
fn reflect_rule(ctx: &ReflectionContext) -> ReflectionReport {
    let rule_id = ctx
        .error
        .rule_id
        .clone()
        .unwrap_or_else(|| "未知规则".to_string());

    let (mut evidence, suggestions, diagnosis) = match &ctx.rule_def {
        Some(def) => {
            let mut ev = vec![format!(
                "规则 {}（分类 {}，示例 {} 条，当前{}）",
                def.id,
                def.category,
                def.example_count,
                if def.enabled { "启用" } else { "停用" }
            )];
            if !def.description.is_empty() {
                ev.push(format!("规则说明：{}", def.description));
            }
            // 反馈统计（按 original 去重：同一文本反复放弃只算 1 个独立证据）
            let (adopted, rejected) = ctx.rule_feedback.unwrap_or((0, 0));
            let total = adopted + rejected;
            let ratio = if total > 0 {
                rejected as f64 / total as f64
            } else {
                0.0
            };
            ev.push(format!(
                "该规则历史反馈：{} 个文本被采纳 / {} 个文本被放弃（放弃占比 {:.0}%{}）",
                adopted,
                rejected,
                ratio * 100.0,
                if total < REJECT_HIGH_COUNT {
                    "，样本较少仅供参考"
                } else {
                    ""
                }
            ));
            // 被放弃的 original 正是该规则的某条示例 → 定位到具体示例
            if let Some(ex) = &ctx.matched_example {
                ev.push(format!(
                    "被放弃的「{}」正是该规则的一条示例（来源 {}）",
                    ex.wrong,
                    if ex.source.is_empty() {
                        "词表"
                    } else {
                        &ex.source
                    }
                ));
            }
            // 高误报判定：独立拒绝文本 ≥ 阈值；或独立反馈 ≥ 2 条且占比 ≥ 阈值
            // （单条反馈不构成「率」，避免 1 次放弃即判 100% 误报）
            let high = rejected >= REJECT_HIGH_COUNT || (total >= 2 && ratio >= REJECT_HIGH_RATIO);
            let diag = if high {
                format!(
                    "规则 {} 误报率偏高（{} 个不同文本被放弃，占比 {:.0}%）：规则模式可能过宽，建议先停用，再收紧其示例 / 模式",
                    def.id,
                    rejected,
                    ratio * 100.0
                )
            } else {
                format!(
                    "规则 {} 目前仅 {} 个不同文本被放弃，尚属孤立误判（可能是个别上下文 / 词性误标），建议保持启用并观察反馈趋势",
                    def.id, rejected
                )
            };
            // 建议与诊断保持一致：高误报分支才给「停用规则」按钮；
            // 孤立误判分支不给「停用」，但都提供「排除该词」（精准、可逆）
            let mut sug = Vec::new();
            // 规则级排除词：「单个词反复触发规则」的精准对策——规则保持启用，
            // 仅该词不再触发；写回词表 `rules[].excludes` + 重建检测器后即时生效
            sug.push(ReflectionSuggestion::new(
                suggestion_kinds::EXCLUDE_WORD,
                format!("将「{}」加入规则 {} 排除名单", ctx.error.original, def.id),
                serde_json::json!({
                    "lang": ctx.error.lang,
                    "rule_id": def.id,
                    "word": ctx.error.original,
                }),
            ));
            if let Some(ex) = &ctx.matched_example {
                sug.push(ReflectionSuggestion::new(
                    suggestion_kinds::REMOVE_EXAMPLE,
                    "删除该误报示例",
                    serde_json::json!({
                        "lang": ctx.error.lang,
                        "rule_id": def.id,
                        "wrong": ex.wrong,
                        "right": ex.right,
                    }),
                ));
            }
            if high {
                sug.push(ReflectionSuggestion::new(
                    suggestion_kinds::DISABLE_RULE,
                    format!("停用规则 {}", def.id),
                    serde_json::json!({ "lang": ctx.error.lang, "rule_id": def.id }),
                ));
            }
            sug.push(ReflectionSuggestion::new(
                suggestion_kinds::VIEW_RULE,
                "到规则面板查看",
                serde_json::json!({ "lang": ctx.error.lang, "rule_id": def.id }),
            ));
            (ev, sug, diag)
        }
        None => (
            vec![format!(
                "该错误未匹配到词表内的具体规则（rule_id = {}），无法归因到规则定义",
                ctx.error.rule_id.clone().unwrap_or_default()
            )],
            Vec::new(),
            "无法定位到具体规则：已加入白名单止血，若反复出现请在规则面板补充对应规则".to_string(),
        ),
    };
    if let Some(reason) = ctx.reason.filter(|r| !r.trim().is_empty()) {
        evidence.insert(0, format!("用户放弃原因：{reason}"));
    }
    ReflectionReport {
        source: shared_model::sources::RULE.to_string(),
        subject: rule_id,
        diagnosis,
        evidence,
        suggestions,
        llm_analysis: None,
    }
}

/// 知识层反思：定位知识条目 → 诊断误召回（模糊匹配 / 上下文不符）→ 建议删除
fn reflect_knowledge(ctx: &ReflectionContext) -> ReflectionReport {
    let (subject, diagnosis, mut evidence, suggestions) = match &ctx.knowledge_item {
        Some(item) => {
            let from = if item.source.is_empty() {
                "未知来源"
            } else {
                &item.source
            };
            (
                format!("知识条目 #{}", item.id),
                format!(
                    "该错误来自 ②.5 知识召回命中条目 #{}（沉淀自 {}）：知识条目是「已确认错误」的向量沉淀，本次误报可能源于模糊匹配或上下文不符，建议删除该条目",
                    item.id, from
                ),
                vec![
                    format!(
                        "条目内容：「{}」→「{}」（类型 {}）",
                        item.text, item.correction, item.error_type
                    ),
                    format!("条目沉淀来源：{from}"),
                ],
                vec![ReflectionSuggestion::new(
                    suggestion_kinds::REMOVE_KNOWLEDGE,
                    format!("删除知识条目 #{}", item.id),
                    serde_json::json!({ "id": item.id, "text": item.text }),
                )],
            )
        }
        None => (
            "知识召回层".to_string(),
            "该错误来自知识召回层，但知识存储中未找到对应条目（可能已被删除）；已加入白名单止血"
                .to_string(),
            Vec::new(),
            Vec::new(),
        ),
    };
    if let Some(reason) = ctx.reason.filter(|r| !r.trim().is_empty()) {
        evidence.insert(0, format!("用户放弃原因：{reason}"));
    }
    ReflectionReport {
        source: shared_model::sources::KNOWLEDGE.to_string(),
        subject,
        diagnosis,
        evidence,
        suggestions,
        llm_analysis: None,
    }
}

/// 模型层反思（decision / feature / statistical）：偏差在模型本身
///
/// 模型输出无逐词规则可归因 → 建议汇入 L3 飞轮负样本（调用方自动执行）+
/// 积累到阈值后重训练。
fn reflect_model(ctx: &ReflectionContext, source: &str) -> ReflectionReport {
    let layer_name = match source {
        shared_model::sources::DECISION => "决策模型（vegaml）",
        shared_model::sources::FEATURE => "L3 特征模型",
        shared_model::sources::STATISTICAL => "统计引擎",
        _ => "模型层",
    };
    let mut evidence = vec![format!(
        "该判断来自{layer_name}：模型输出无逐词规则可归因，偏差在模型本身"
    )];
    evidence.push(format!(
        "错误类型：{}，置信度 {:.2}",
        ctx.error.error_type, ctx.error.confidence
    ));
    if let Some(reason) = ctx.reason.filter(|r| !r.trim().is_empty()) {
        evidence.insert(0, format!("用户放弃原因：{reason}"));
    }
    let suggestions = vec![
        ReflectionSuggestion::new(
            suggestion_kinds::L3_NEGATIVE,
            "汇入 L3 飞轮负样本",
            serde_json::json!({
                "lang": ctx.error.lang,
                "text": ctx.error.original,
                "error_type": ctx.error.error_type,
            }),
        ),
        ReflectionSuggestion::new(
            suggestion_kinds::RETRAIN,
            "积累后重训练（设置界面）",
            serde_json::json!({ "lang": ctx.error.lang }),
        ),
    ];
    ReflectionReport {
        source: source.to_string(),
        subject: layer_name.to_string(),
        diagnosis: format!(
            "偏差在{layer_name}本身：该模型对本句误判。已加入白名单止血，并建议把本句汇入 L3 飞轮负样本——积累到阈值后重训练可系统性修正此类误判"
        ),
        evidence,
        suggestions,
        llm_analysis: None,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use shared_model::error_types;

    fn rule_error() -> Error {
        Error::new_with_meta(
            "zh",
            error_types::SPELLING,
            0.9,
            [0, 2],
            "报复",
            Some("抱负".to_string()),
            "疑似同音错别字",
            Some("spelling".to_string()),
            Some("ZH_SPELL".to_string()),
            Vec::new(),
        )
    }

    fn ctx_for(error: &Error) -> ReflectionContext<'_> {
        ReflectionContext {
            error,
            reason: None,
            rule_def: None,
            rule_feedback: None,
            matched_example: None,
            knowledge_item: None,
        }
    }

    #[test]
    fn rule_first_reject_is_isolated() {
        let e = rule_error();
        let mut ctx = ctx_for(&e);
        ctx.rule_def = Some(RuleDefInfo {
            id: "ZH_SPELL".into(),
            category: "spelling".into(),
            description: "同音错别字".into(),
            enabled: true,
            example_count: 12,
        });
        ctx.rule_feedback = Some((5, 1)); // 5 采纳 1 拒绝 → 占比 17% → 孤立
        let r = reflect_rejection(&ctx);
        assert_eq!(r.source, "rule");
        assert_eq!(r.subject, "ZH_SPELL");
        assert!(
            r.diagnosis.contains("孤立"),
            "低误报率应判孤立案例: {}",
            r.diagnosis
        );
        // 孤立误判分支不给「停用规则」按钮（诊断与建议保持一致）
        assert!(
            !r.suggestions
                .iter()
                .any(|s| s.kind == suggestion_kinds::DISABLE_RULE),
            "孤立误判不应建议停用: {:?}",
            r.suggestions
        );
        assert!(
            r.suggestions
                .iter()
                .any(|s| s.kind == suggestion_kinds::VIEW_RULE)
        );
        // 两个分支都给「排除该词」（精准、可逆的中间档）
        let ex = r
            .suggestions
            .iter()
            .find(|s| s.kind == suggestion_kinds::EXCLUDE_WORD)
            .expect("应建议将误报词加入规则排除名单");
        assert_eq!(
            ex.payload.get("word").and_then(|v| v.as_str()),
            Some(e.original.as_str())
        );
        assert_eq!(
            ex.payload.get("rule_id").and_then(|v| v.as_str()),
            Some("ZH_SPELL")
        );
    }

    #[test]
    fn single_distinct_reject_is_isolated_even_at_100_percent() {
        let e = rule_error();
        let mut ctx = ctx_for(&e);
        ctx.rule_def = Some(RuleDefInfo {
            id: "ZH_MISSING_SUBJECT".into(),
            category: "grammar".into(),
            ..Default::default()
        });
        // 同一文本被反复放弃 4 次 → 去重后仅 1 个独立拒绝（占比 100% 但样本不足）
        ctx.rule_feedback = Some((0, 1));
        let r = reflect_rejection(&ctx);
        assert!(
            r.diagnosis.contains("孤立误判"),
            "单一文本反复放弃不应判高误报: {}",
            r.diagnosis
        );
        assert!(
            !r.suggestions
                .iter()
                .any(|s| s.kind == suggestion_kinds::DISABLE_RULE),
            "样本不足时不应建议停用规则: {:?}",
            r.suggestions
        );
        assert!(r.evidence.iter().any(|ev| ev.contains("样本较少")));
    }

    #[test]
    fn three_distinct_rejects_suggest_disable() {
        let e = rule_error();
        let mut ctx = ctx_for(&e);
        ctx.rule_def = Some(RuleDefInfo {
            id: "ZH_MISSING_SUBJECT".into(),
            category: "grammar".into(),
            ..Default::default()
        });
        // 3 个不同文本被放弃 → 独立证据足够 → 高误报
        ctx.rule_feedback = Some((0, 3));
        let r = reflect_rejection(&ctx);
        assert!(
            r.diagnosis.contains("误报率偏高"),
            "3 个独立文本放弃应判高误报: {}",
            r.diagnosis
        );
        assert!(
            r.suggestions
                .iter()
                .any(|s| s.kind == suggestion_kinds::DISABLE_RULE)
        );
    }

    #[test]
    fn rule_high_reject_rate_suggests_disable() {
        let e = rule_error();
        let mut ctx = ctx_for(&e);
        ctx.rule_def = Some(RuleDefInfo {
            id: "ZH_SPELL".into(),
            category: "spelling".into(),
            ..Default::default()
        });
        ctx.rule_feedback = Some((1, 3)); // 占比 75% → 高误报
        let r = reflect_rejection(&ctx);
        assert!(
            r.diagnosis.contains("误报率偏高"),
            "高误报率应建议停用: {}",
            r.diagnosis
        );
    }

    #[test]
    fn rule_matched_example_gets_remove_suggestion() {
        let e = rule_error();
        let mut ctx = ctx_for(&e);
        ctx.rule_def = Some(RuleDefInfo {
            id: "ZH_SPELL".into(),
            ..Default::default()
        });
        ctx.matched_example = Some(ExampleInfo {
            wrong: "报复".into(),
            right: "抱负".into(),
            source: "manual".into(),
        });
        let r = reflect_rejection(&ctx);
        let sug = r
            .suggestions
            .iter()
            .find(|s| s.kind == suggestion_kinds::REMOVE_EXAMPLE)
            .expect("应含删除示例建议");
        assert_eq!(sug.payload["wrong"], "报复");
        assert_eq!(sug.payload["rule_id"], "ZH_SPELL");
        assert!(
            r.evidence
                .iter()
                .any(|x| x.contains("正是该规则的一条示例"))
        );
    }
    #[test]
    fn rule_without_def_falls_back() {
        let e = rule_error();
        let ctx = ctx_for(&e);
        let r = reflect_rejection(&ctx);
        assert!(r.diagnosis.contains("无法定位"));
        assert!(r.suggestions.is_empty());
    }

    #[test]
    fn knowledge_with_item() {
        let mut e = Error::new(
            "en",
            error_types::ARTICLE,
            0.6,
            [0, 9],
            "I saw a apple.",
            Some("I saw an apple.".to_string()),
            "知识召回命中",
        );
        e.source = Some(shared_model::sources::KNOWLEDGE.to_string());
        let mut ctx = ctx_for(&e);
        ctx.knowledge_item = Some(KnowledgeInfo {
            id: 7,
            text: "I saw a apple.".into(),
            error_type: "article".into(),
            correction: "I saw an apple.".into(),
            source: "llm_adopt".into(),
        });
        let r = reflect_rejection(&ctx);
        assert_eq!(r.source, "knowledge");
        assert_eq!(r.subject, "知识条目 #7");
        assert!(r.diagnosis.contains("模糊匹配"));
        let sug = &r.suggestions[0];
        assert_eq!(sug.kind, suggestion_kinds::REMOVE_KNOWLEDGE);
        assert_eq!(sug.payload["id"], 7);
    }

    #[test]
    fn knowledge_without_item() {
        let mut e = Error::new(
            "en",
            error_types::ARTICLE,
            0.6,
            [0, 9],
            "I saw a apple.",
            None,
            "知识召回命中",
        );
        e.source = Some(shared_model::sources::KNOWLEDGE.to_string());
        let ctx = ctx_for(&e);
        let r = reflect_rejection(&ctx);
        assert!(r.diagnosis.contains("未找到对应条目"));
        assert!(r.suggestions.is_empty());
    }

    #[test]
    fn model_layer_feature() {
        let mut e = Error::new(
            "zh",
            error_types::GRAMMAR,
            0.8,
            [0, 6],
            "这个句子有问题",
            None,
            "L3 特征模型判定",
        );
        e.source = Some(shared_model::sources::FEATURE.to_string());
        let ctx = ctx_for(&e);
        let r = reflect_rejection(&ctx);
        assert_eq!(r.source, "feature");
        assert!(r.diagnosis.contains("L3 特征模型"));
        assert!(
            r.suggestions
                .iter()
                .any(|s| s.kind == suggestion_kinds::L3_NEGATIVE)
        );
        assert!(
            r.suggestions
                .iter()
                .any(|s| s.kind == suggestion_kinds::RETRAIN)
        );
    }

    #[test]
    fn llm_source_is_terminal() {
        let mut e = Error::new(
            "zh",
            error_types::GRAMMAR,
            0.8,
            [0, 6],
            "这个句子有问题",
            None,
            "LLM 兜底",
        );
        e.source = Some(shared_model::sources::LLM.to_string());
        let ctx = ctx_for(&e);
        let r = reflect_rejection(&ctx);
        assert_eq!(r.source, "llm");
        assert!(r.diagnosis.contains("白名单"));
    }

    #[test]
    fn missing_source_normalized_to_rule() {
        let e = rule_error(); // 无 source 字段
        let ctx = ctx_for(&e);
        let r = reflect_rejection(&ctx);
        assert_eq!(r.source, "rule");
        assert_eq!(normalize_source(&e), "rule");
    }

    #[test]
    fn reason_prefixed_to_evidence() {
        let e = rule_error();
        let mut ctx = ctx_for(&e);
        ctx.reason = Some("这个词在上下文中是正确的");
        let r = reflect_rejection(&ctx);
        assert!(
            r.evidence
                .first()
                .is_some_and(|x| x.contains("用户放弃原因"))
        );
    }
}
