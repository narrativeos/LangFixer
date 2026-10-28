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

/// 专家正误表（金集）里**闭卷未命中**的条目（外部测评证据，离线可算）
///
/// 与 `EvalEvidence`（测评服务的 Fβ / 分层数字）互补：那边说「整体弱」，
/// 这里说**具体哪几条没抓到**——只有具体条目才能变成可执行的靶子（补条件 / 喂样本 / 开 LLM）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GoldMissIn {
    /// 专家条目号（正误表「序号」，用于人工回查）
    pub no: u32,
    /// 「错误或待核原文」
    #[serde(default)]
    pub original: String,
    /// 「正确或处理建议」（专家写法）
    #[serde(default)]
    pub suggestion: String,
    /// 「页码及位置」
    #[serde(default)]
    pub location: String,
    /// 判定标签（`差错` / `表达优化` / `表述不清楚` / `未定` …）
    #[serde(default)]
    pub verdict: String,
    /// 「备注及判定依据」原文（含 `【差错】` 等前缀）
    #[serde(default)]
    pub note: String,
    /// 是否计差错（与金集 `counted` 一致）
    #[serde(default)]
    pub counted: bool,
    /// 已有规则触发但未 credited 时的规则 id（有值 → 靶子是 `rule_tune`，不是新词表条件）
    #[serde(default)]
    pub rule_id: Option<String>,
    /// 规则归因的来源：`positional` = 同载体坐标重合（强）；`text` = 原文子串重叠（弱，
    /// gold 原文在载体文本里定位不到时的兜底）。强弱必须留痕——弱归因要先人工核对
    /// 「是不是同一条差错」，不能与强归因一样直接当「规则不对」处理
    #[serde(default)]
    pub rule_via: Option<String>,
}

/// 金集测评证据（闭卷口径）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GoldEvidenceIn {
    /// 证据来源标识（如 `media_gold@2026-10-26 · 上限评测 · 离线 level 1`）
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub gold_total: usize,
    /// 计差错条目数
    #[serde(default)]
    pub counted_total: usize,
    /// 文本口径命中数
    #[serde(default)]
    pub text_hits: usize,
    /// 位置口径命中数
    #[serde(default)]
    pub positional_hits: usize,
    /// 闭卷未命中条目
    #[serde(default)]
    pub misses: Vec<GoldMissIn>,
}

/// 未命中条目的靶子类型（[`classify_miss`] 的分类结果）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissAction {
    /// 已有规则触发但没 credited → 调规则
    RuleTune,
    /// 局部可形式化（错别字 / 标点 / 时间数字 / 分隔）→ 词表条件或规则模式
    LexiconCondition,
    /// 结构性差错（搭配 / 层次 / 主语）→ 喂 L3 样本（决策模型学不出来就是学不出来）
    L3Sample,
    /// 编辑判断（表达优化 / 表述不清 / 未定）→ LLM 层
    LlmArm,
}

/// 可形式化的关键词（命中 → [`MissAction::LexiconCondition`]）
const MISS_LEXICON_KEYWORDS: [&str; 9] = [
    "错别字",
    "拼写",
    "标点",
    "冒号",
    "分隔",
    "小时制",
    "时间",
    "数字",
    "大小写",
];

/// 只取判定依据的**第一句**：专家常在后面补「具体内容由院方确认」这类无关话，
/// 里面的「时间」等词会把结构性差错误分进词表靶子（实测金集 #8 就是这样）
fn first_clause(s: &str) -> &str {
    match s.find(['。', '；', ';']) {
        Some(i) => &s[..i],
        None => s,
    }
}

/// 把未命中条目分到最合适的靶子（**确定性**映射）
///
/// 立场：分类错了不要紧——建议进队列是 `pending`，由人工 / P2 判定（`dismissed` 也是合法结局）；
/// 但**不能不提**：闭卷漏掉的计差错条目必须至少产出一条建议，否则反思中心就是装饰。
pub fn classify_miss(m: &GoldMissIn) -> MissAction {
    if m.rule_id.is_some() {
        return MissAction::RuleTune;
    }
    // 不计差错 = 专家自己都没定性为差错（表达优化 / 表述不清 / 未定）——
    // 那是编辑判断，词典与决策模型都不该硬来，靶子在 LLM 层
    if !m.counted {
        return MissAction::LlmArm;
    }
    let hay = format!("{} {}", first_clause(&m.verdict), first_clause(&m.note));
    if MISS_LEXICON_KEYWORDS.iter().any(|k| hay.contains(k)) {
        MissAction::LexiconCondition
    } else {
        MissAction::L3Sample
    }
}

/// 金集证据 → findings（写进报告的「发现」，数字必须与评测同一把尺子）
pub fn gold_findings(gold: &GoldEvidenceIn) -> Vec<String> {
    let mut out = Vec::new();
    let counted_miss = gold.misses.iter().filter(|m| m.counted).count();
    out.push(format!(
        "金集闭卷复现（{}）：{} 条 gold 中文本口径命中 {}、位置口径命中 {}；\
         未命中 {} 条（其中计差错 {counted_miss} 条）",
        gold.source,
        gold.gold_total,
        gold.text_hits,
        gold.positional_hits,
        gold.misses.len(),
    ));
    if !gold.misses.is_empty() && counted_miss == 0 {
        out.push("未命中的都不是计差错条目 → 硬差错已全覆盖，剩余是编辑判断".to_string());
    }
    if counted_miss > 0 {
        let nos: Vec<String> = gold
            .misses
            .iter()
            .filter(|m| m.counted)
            .map(|m| format!("#{}", m.no))
            .collect();
        out.push(format!(
            "未命中的计差错条目：{}（必须逐条给靶子）",
            nos.join(" ")
        ));
    }
    out
}

/// 合并同靶子建议：证据合并，payload 里的 `gold_nos` / `originals` 求并集
///
/// 不能直接用 [`push_suggestion`]：它只合并证据，payload 会被首条占住，
/// 后来条目的**专家条目号与原文都丢了**——「这条建议对应专家表哪几条」必须可追溯。
fn push_gold_suggestion(
    out: &mut Vec<ReflectionSuggestion>,
    kind: &str,
    target: String,
    summary: String,
    evidence: Vec<String>,
    payload: serde_json::Value,
    priority: u8,
) {
    // 可追溯字段：新建议 payload 里的这些数组必须并入已有同靶子建议
    const MERGE_KEYS: [&str; 2] = ["gold_nos", "originals"];
    fn union(payload: &mut serde_json::Value, key: &str, values: Vec<serde_json::Value>) {
        let Some(obj) = payload.as_object_mut() else {
            return;
        };
        let arr = obj
            .entry(key)
            .or_insert_with(|| serde_json::Value::Array(Vec::new()));
        let Some(list) = arr.as_array_mut() else {
            return;
        };
        for v in values {
            if !list.contains(&v) {
                list.push(v);
            }
        }
    }
    if let Some(existing) = out
        .iter_mut()
        .find(|s| s.kind == kind && s.target == target)
    {
        for e in evidence {
            if !existing.evidence.contains(&e) {
                existing.evidence.push(e);
            }
        }
        for key in MERGE_KEYS {
            let values = payload
                .get(key)
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            union(&mut existing.payload, key, values);
        }
        if priority < existing.priority {
            existing.priority = priority;
            existing.summary = summary;
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

/// 金集证据 → 建议队列（同一 (kind, target) 合并证据；payload 沿用 [`suggestion_kinds`] 约定，
/// 额外带 `gold_nos` 让 P2 / 人工能回到专家表具体条目）
pub fn gold_suggestions(gold: &GoldEvidenceIn) -> Vec<ReflectionSuggestion> {
    let mut out: Vec<ReflectionSuggestion> = Vec::new();
    for m in &gold.misses {
        let evidence = vec![format!(
            "金集 #{}（{}）原文「{}」专家建议「{}」",
            m.no,
            if m.counted { "计差错" } else { "不计" },
            m.original,
            m.suggestion
        )];
        let priority = if m.counted { 0 } else { 2 };
        match classify_miss(m) {
            MissAction::RuleTune => {
                let rid = m.rule_id.clone().unwrap_or_default();
                let via = match m.rule_via.as_deref() {
                    Some("text") => "归因＝原文重叠（弱：gold 原文在载体上定位不到，先核对是不是同一条差错）",
                    Some(_) => "归因＝坐标重合（强：同一处判定）",
                    None => "归因未留痕",
                };
                push_gold_suggestion(
                    &mut out,
                    suggestion_kinds::RULE_TUNE,
                    rid.clone(),
                    format!(
                        "规则 {rid} 触发了但金集 #{} 未被 credited → 核对触发条件与建议文本（是规则不对，不是缺规则）· {via}",
                        m.no
                    ),
                    evidence,
                    serde_json::json!({
                        "rule_id": rid,
                        "rule_via": m.rule_via,
                        "gold_nos": [m.no]
                    }),
                    priority,
                );
            }
            MissAction::LexiconCondition => push_gold_suggestion(
                &mut out,
                suggestion_kinds::LEXICON_CONDITION,
                "（缺条件）".into(),
                format!(
                    "金集 #{} 是局部可形式化差错但没抓到 → 补词表条件 / 规则模式（入库前先过消融）",
                    m.no
                ),
                evidence,
                serde_json::json!({ "originals": [m.original], "gold_nos": [m.no] }),
                priority,
            ),
            MissAction::L3Sample => push_gold_suggestion(
                &mut out,
                suggestion_kinds::L3_SAMPLE,
                "decision".into(),
                "结构性差错（搭配 / 层次 / 主语）规则写不动 → 把专家改法作为正样本汇入 L3 飞轮".into(),
                evidence,
                serde_json::json!({ "source": "decision", "originals": [m.original], "gold_nos": [m.no] }),
                priority,
            ),
            MissAction::LlmArm => push_gold_suggestion(
                &mut out,
                suggestion_kinds::LLM_ARM,
                "llm".into(),
                "编辑判断类条目（表达优化 / 表述不清 / 未定）不该由词典硬判 → 启用 LLM 层并用留出集验证".into(),
                evidence,
                serde_json::json!({ "current_arm": null, "originals": [m.original], "gold_nos": [m.no] }),
                priority,
            ),
        }
    }
    out.sort_by_key(|s| s.priority);
    out
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

/// [`build_report`] + 金集闭卷证据（外部测评证据的一种，离线可算）
///
/// 金集 findings 追加在反馈 findings 之后；建议按 (kind, target) 与反馈侧建议合并去重，
/// 最后按 priority 升序重排。**不改反馈侧任何判定**——金集证据只补充，不覆盖。
pub fn build_report_with_gold(
    lang: &str,
    created_at: &str,
    stats: &[FeedbackStatIn],
    recent: &[FeedbackEntryIn],
    eval: Option<EvalEvidence>,
    l3_retrain: &L3RetrainStateIn,
    gold: Option<&GoldEvidenceIn>,
) -> ReflectionCenterReport {
    let mut report = build_report(lang, created_at, stats, recent, eval, l3_retrain);
    let Some(gold) = gold else { return report };
    report.findings.extend(gold_findings(gold));
    for s in gold_suggestions(gold) {
        push_suggestion(
            &mut report.suggestions,
            &s.kind,
            s.target,
            s.summary,
            s.evidence,
            s.payload,
            s.priority,
        );
    }
    report.suggestions.sort_by_key(|s| s.priority);
    report
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

    fn miss(no: u32, counted: bool, verdict: &str, note: &str, original: &str) -> GoldMissIn {
        GoldMissIn {
            no,
            original: original.into(),
            suggestion: "专家改法".into(),
            location: "图片1 中部".into(),
            verdict: verdict.into(),
            note: note.into(),
            counted,
            rule_id: None,
            rule_via: None,
        }
    }

    #[test]
    fn gold_miss_classification_is_deterministic() {
        // 规则触发了却没 credited → 靶子是规则本身，不是「再补一条词典」
        let mut fired = miss(13, true, "差错", "【差错】错别字：报道/报到", "报道");
        fired.rule_id = Some("ZH_CONFUSABLE".into());
        assert_eq!(classify_miss(&fired), MissAction::RuleTune);
        // 局部可形式化（并列未分隔）→ 词表条件
        assert_eq!(
            classify_miss(&miss(
                3,
                true,
                "差错",
                "【差错】并列指标未作分隔",
                "血压心率"
            )),
            MissAction::LexiconCondition
        );
        // 结构性差错 → L3 样本
        assert_eq!(
            classify_miss(&miss(
                11,
                true,
                "差错",
                "【差错】主谓搭配不当",
                "病房工作电话不咨询病情"
            )),
            MissAction::L3Sample
        );
        // 不计差错 = 编辑判断 → LLM 层（词典硬判就是误报源头）
        assert_eq!(
            classify_miss(&miss(
                1,
                false,
                "表达优化",
                "【表达优化】关系不明",
                "住院手续身份核查"
            )),
            MissAction::LlmArm
        );
    }

    #[test]
    fn gold_suggestions_keep_every_gold_no_when_merged() {
        // 同靶子合并时不能把后来条目的专家条目号丢掉（payload 被首条占住是实测踩过的坑）；
        // 且 #8 那种「补充说明里带『具体时间』」的结构性差错不能被误分进词表靶子
        let gold = GoldEvidenceIn {
            misses: vec![
                miss(
                    8,
                    true,
                    "差错",
                    "【差错】并列条件中混入“需等”这一等待动作，句子层次不清。改文仅理顺条件与先后关系；具体时间由院方确认",
                    "且次日上午10点左右需等",
                ),
                miss(
                    11,
                    true,
                    "差错",
                    "【差错】“电话”不能实施“咨询”动作，原句主谓搭配不当。改文保留原限制性意思",
                    "病房工作电话不咨询病情",
                ),
            ],
            ..Default::default()
        };
        let s = gold_suggestions(&gold);
        assert_eq!(s.len(), 1, "同靶子必须合并");
        assert_eq!(s[0].kind, suggestion_kinds::L3_SAMPLE, "{:?}", s[0]);
        let nos: Vec<u64> = s[0].payload["gold_nos"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap())
            .collect();
        assert_eq!(nos, vec![8, 11], "合并后必须能回到每一条专家条目");
        assert_eq!(s[0].payload["originals"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn rule_tune_suggestion_keeps_attribution_strength() {
        // 规则归因的强弱必须跟着建议走：坐标重合是同一处判定，原文重叠只是兜底猜测，
        // 两者混在一起就会把「先核对是不是同一条差错」的活当成「规则不对」直接改规则
        let weak = {
            let mut m = miss(
                13,
                true,
                "差错",
                "【差错】错别字：报道/报到",
                "分时段进入病区报道。",
            );
            m.rule_id = Some("ZH_CONFUSABLE".into());
            m.rule_via = Some("text".into());
            m
        };
        let s = gold_suggestions(&GoldEvidenceIn {
            misses: vec![weak],
            ..Default::default()
        });
        let s = s
            .iter()
            .find(|s| s.kind == suggestion_kinds::RULE_TUNE)
            .expect("rule_tune 建议");
        assert!(
            s.summary.contains("原文重叠"),
            "弱归因必须写进摘要: {}",
            s.summary
        );
        assert_eq!(s.payload["rule_via"], "text");

        let strong = {
            let mut m = miss(
                13,
                true,
                "差错",
                "【差错】错别字：报道/报到",
                "分时段进入病区报道。",
            );
            m.rule_id = Some("ZH_CONFUSABLE".into());
            m.rule_via = Some("positional".into());
            m
        };
        let s = gold_suggestions(&GoldEvidenceIn {
            misses: vec![strong],
            ..Default::default()
        });
        let s = s
            .iter()
            .find(|s| s.kind == suggestion_kinds::RULE_TUNE)
            .expect("rule_tune 建议");
        assert!(
            s.summary.contains("坐标重合"),
            "强归因要标明: {}",
            s.summary
        );
        assert_eq!(s.payload["rule_via"], "positional");
    }

    #[test]
    fn gold_misses_become_queued_suggestions_with_traceability() {
        let gold = GoldEvidenceIn {
            source: "test".into(),
            gold_total: 6,
            counted_total: 4,
            text_hits: 1,
            positional_hits: 2,
            misses: vec![
                miss(3, true, "差错", "【差错】并列指标未作分隔", "血压心率"),
                miss(
                    8,
                    true,
                    "差错",
                    "【差错】并列条件混入等待动作",
                    "且次日上午10点左右需等",
                ),
                miss(
                    11,
                    true,
                    "差错",
                    "【差错】主谓搭配不当",
                    "病房工作电话不咨询病情",
                ),
                miss(
                    15,
                    false,
                    "表达不清楚",
                    "【表达不清楚】不能认定矛盾",
                    "每天15:00—16:00",
                ),
            ],
        };
        let s = gold_suggestions(&gold);
        // 同类同靶子必须合并：#8 + #11 都是结构性 → 1 条 l3_sample（证据 2 条，不刷屏）
        let l3: Vec<&ReflectionSuggestion> = s
            .iter()
            .filter(|x| x.kind == suggestion_kinds::L3_SAMPLE)
            .collect();
        assert_eq!(l3.len(), 1, "同一靶子必须合并");
        assert_eq!(l3[0].evidence.len(), 2);
        assert_eq!(l3[0].priority, 0, "计差错漏报最高优先级");
        let lex: Vec<&ReflectionSuggestion> = s
            .iter()
            .filter(|x| x.kind == suggestion_kinds::LEXICON_CONDITION)
            .collect();
        assert_eq!(lex.len(), 1, "#3（并列未分隔）应给词表条件靶子");
        assert_eq!(lex[0].evidence.len(), 1);
        let llm = s
            .iter()
            .find(|x| x.kind == suggestion_kinds::LLM_ARM)
            .expect("编辑判断类漏报应给 llm_arm");
        assert_eq!(llm.priority, 2, "编辑判断不得抢计差错漏报的优先级");
        // 每条建议都能回到专家表条目
        assert!(s.iter().all(|x| {
            x.payload
                .get("gold_nos")
                .and_then(|v| v.as_array())
                .map(|a| !a.is_empty())
                .unwrap_or(false)
        }));
        // 报告合并：findings 追加、建议合并去重
        let r = build_report_with_gold(
            "zh",
            "t",
            &[],
            &[],
            None,
            &L3RetrainStateIn::default(),
            Some(&gold),
        );
        assert!(
            r.findings
                .iter()
                .any(|f| f.contains("未命中的计差错条目：#3 #8 #11")),
            "findings：{:?}",
            r.findings
        );
        assert!(r.suggestions.len() >= s.len());
        // 不传金集证据 → 与 build_report 完全一致（金集证据只补充，不覆盖）
        let plain = build_report("zh", "t", &[], &[], None, &L3RetrainStateIn::default());
        let none = build_report_with_gold(
            "zh",
            "t",
            &[],
            &[],
            None,
            &L3RetrainStateIn::default(),
            None,
        );
        assert_eq!(plain.suggestions.len(), none.suggestions.len());
        assert_eq!(plain.findings, none.findings);
    }
}
