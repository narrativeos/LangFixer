//! 引擎路由抽象：rule / decision / statistical / llm 引擎与组合
//!
//! - [`GecEngineKind`]：引擎类型，按语言经环境变量 `GEC_ENGINE_{LANG}` 选择
//!   （`rule` 默认 / `decision` / `statistical` / `llm` 预留）
//! - [`GecEngine`]：引擎 trait；任何 [`GECDetector`]（语言检测器）默认即 `Rule` 引擎；
//!   [`GecEngine::detect_level`] 提供三级响应入口（1 规则+决策 / 2 弃权 LLM 兜底 /
//!   3 强制 LLM）
//! - [`CompositeDetector`]：主引擎 + 兜底引擎（主引擎失败整体回退；成功则合并去重）
//! - [`CascadeDetector`]：三级级联（规则 + 决策模型弃权信号 + 按需 LLM 兜底）
//! - [`compose_detector`]：按语言组合最终引擎的入口
//!
//! 三级响应（API `level` 参数 1/2/3）：
//! - `rule`（默认，1 级）：纯规则检测器；若决策模型层已**显式**启用
//!   （默认关闭，P0.5 起 opt-in；`GEC_DEC_ENABLED=1`/`true` 或设置文件
//!   `decision.enabled=true` 时开启）则组合为 [`CascadeDetector`]（「规则为主 +
//!   决策模型合并 + 弃权信号」）；
//! - `decision`：显式启用决策模型层（vegaml，处理规则覆盖不到的模糊情况）；
//! - `statistical`：统计 GEC 服务为主 + 规则兜底（见 [`crate::statistical`]）；
//! - `llm`：预留。
//!
//! 响应级别语义（P0.5 重绑：**L3 特征模型是唯一的 pre-LLM 门控**）：
//! - level 1：规则（+ 决策模型，若显式启用）；
//! - level 2：level 1 + L3 门控（L3 启用时：高置信判定直接定案，
//!   弃权 / 服务不可用 → LLM 兜底；L3 未启用时回退旧语义：决策模型弃权 → LLM 兜底）；
//! - level 3：level 1 + L3 门控 + **无条件**全文 LLM（「漏报」兜底）。
//!
//! LLM 兜底层（[`LlmGecEngine`]）不静态挂载：由 API / 桌面端在 level 2/3 请求
//! 到达时按运行时 LLM 配置 + 检测模板现构建，经 `detect_level` 传入。

use std::sync::Arc;

use async_trait::async_trait;

use crate::decision::{DecisionConfig, DecisionGecEngine};
use crate::detector::{DetectorError, GECDetector};
use crate::knowledge_recall::{KnowledgeRecall, knowledge_hit_to_error};
use crate::l3::{L3FeatureConfig, L3FeatureEngine};
use crate::llm::LlmGecEngine;
use crate::statistical::StatisticalGecEngine;
use crate::timings::{DetectTimings, NlpCall};
use shared_model::{Error, sources};

/// 引擎类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GecEngineKind {
    /// 规则引擎（内置规则 + NLP 服务，各语言检测器）
    Rule,
    /// 决策模型引擎（vegaml：句级「是否有语病 + 错误类型」判断，
    /// 校准 + conformal + abstain；处理规则覆盖不到的模糊情况）
    Decision,
    /// 统计引擎（Harper / HuggingFace 序列模型等外部服务）
    Statistical,
    /// LLM 引擎（预留）
    Llm,
}

impl GecEngineKind {
    pub fn as_str(self) -> &'static str {
        match self {
            GecEngineKind::Rule => "rule",
            GecEngineKind::Decision => "decision",
            GecEngineKind::Statistical => "statistical",
            GecEngineKind::Llm => "llm",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "rule" => Some(Self::Rule),
            "decision" | "dec" => Some(Self::Decision),
            "statistical" | "stat" => Some(Self::Statistical),
            "llm" => Some(Self::Llm),
            _ => None,
        }
    }

    /// 从环境变量 `GEC_ENGINE_{LANG}`（如 `GEC_ENGINE_EN`）读取该语言的引擎类型，
    /// 未设置或无法解析时默认 [`GecEngineKind::Rule`]
    pub fn from_env(lang: &str) -> Self {
        let var = format!("GEC_ENGINE_{}", lang.to_uppercase());
        std::env::var(&var)
            .ok()
            .and_then(|v| Self::parse(&v))
            .unwrap_or(Self::Rule)
    }
}

/// GEC 引擎 trait：输入文本、产出错误列表的单个检测引擎
#[async_trait]
pub trait GecEngine: Send + Sync {
    /// 引擎类型（rule / statistical / llm）
    fn kind(&self) -> GecEngineKind;
    /// 引擎 / 模型名（日志与元数据用）
    fn name(&self) -> &str;
    /// 检测文本中的错误
    async fn detect(&self, text: &str) -> Result<Vec<Error>, DetectorError>;
    /// 最近一次 `detect` 中的 NLP 服务调用信息（`None` = 未调用 NLP 服务）。
    /// 组合引擎（级联 / 组合检测器）覆写本方法，从规则层转发。
    fn last_nlp_call(&self) -> Option<NlpCall> {
        None
    }
    /// 本引擎组合中挂载的决策模型引擎（若有；查询模型版本号等元数据用）。
    /// 组合引擎（[`CascadeDetector`]）覆写本方法转发到决策层。
    fn decision_engine(&self) -> Option<&crate::decision::DecisionGecEngine> {
        None
    }
    /// 三级响应检测：
    /// - `level = 1`：规则（+ 决策模型，若引擎组合中挂载）；
    /// - `level = 2`：1 级 + 决策模型**明确弃权**时对全文跑 LLM 兜底；
    /// - `level = 3`：1 级 + **无条件**强制全文 LLM（无论 1 级是否已检出错误）。
    ///
    /// `force_llm`（「漏报」强制兜底）：无视 level 触发条件，规则 + 决策模型
    /// 完成后**无条件**对全文跑一次 LLM 兜底（与 1 级结果合并去重）。
    ///
    /// `llm` 为 LLM 兜底引擎（调用方从运行时 LLM 配置 + 检测模板构建；
    /// `None` 表示未配置）。默认实现：无决策层的引擎在 level 2 无弃权信号
    /// （等价 1 级）；level 3 无条件触发 LLM（未配置 →
    /// [`DetectorError::LlmNotConfigured`]）。支持弃权信号的引擎
    /// （[`CascadeDetector`]）覆写本方法。
    ///
    /// 返回的 [`DetectOutcome::timings`] 含各阶段计时（规则 / NLP（含预热）/
    /// LLM / 总耗时）；未覆写的引擎仅填充规则层与总耗时。
    async fn detect_level(
        &self,
        text: &str,
        level: u8,
        force_llm: bool,
        llm: Option<&LlmGecEngine>,
    ) -> Result<DetectOutcome, DetectorError> {
        if !(1..=3).contains(&level) {
            return Err(DetectorError::DetectionFailed(format!(
                "无效响应级别: {level}（应为 1-3）"
            )));
        }
        let started = std::time::Instant::now();
        // ① 规则检测（计时含 NLP 服务调用；NLP 调用信息事后从引擎读取）
        let t_rule = std::time::Instant::now();
        let mut errors = self.detect(text).await?;
        let rule_ms = t_rule.elapsed().as_millis() as u64;
        let nlp = self.last_nlp_call();
        // 无来源错误补标 `rule`（组合引擎内部已自行补标的此处不再变化）
        tag_source(&mut errors, shared_model::sources::RULE);
        let mut layers = vec![LayerReport::new(
            shared_model::sources::RULE,
            "ran",
            errors.len(),
        )];
        // LLM 兜底：force_llm 无视 level 触发；level 3 无条件触发
        // （无决策层的引擎 level 2 无弃权信号，等价 1 级）
        let mut llm_confirmed_clean = false;
        let mut llm_ms = None;
        if force_llm || level == 3 {
            let llm = llm.ok_or_else(|| {
                DetectorError::LlmNotConfigured(
                    "LLM 兜底检测未配置（请在设置界面配置 LLM 参数，或设置 LLM_API_KEY）"
                        .to_string(),
                )
            })?;
            let t_llm = std::time::Instant::now();
            let extra = llm.detect(text).await?;
            llm_ms = Some(t_llm.elapsed().as_millis() as u64);
            llm_confirmed_clean = extra.is_empty();
            layers.push(LayerReport::new(
                shared_model::sources::LLM,
                "ran",
                extra.len(),
            ));
            merge_errors(&mut errors, extra);
        }
        Ok(DetectOutcome {
            errors,
            decision_abstained: false,
            llm_confirmed_clean,
            timings: DetectTimings {
                rule_ms,
                nlp_ms: nlp.map(|c| c.ms),
                nlp_warmup_ms: nlp.filter(|c| c.cold_start).map(|c| c.ms),
                llm_ms,
                total_ms: started.elapsed().as_millis() as u64,
                ..Default::default()
            },
            layers,
        })
    }
}

/// 任何语言检测器默认即规则引擎
#[async_trait]
impl<T: GECDetector + ?Sized> GecEngine for T {
    fn kind(&self) -> GecEngineKind {
        GecEngineKind::Rule
    }
    fn name(&self) -> &str {
        self.language()
    }
    async fn detect(&self, text: &str) -> Result<Vec<Error>, DetectorError> {
        self.detect(text).await
    }
    fn last_nlp_call(&self) -> Option<NlpCall> {
        <T as GECDetector>::last_nlp_call(self)
    }
}

/// 组合检测器：主引擎 + 兜底引擎
///
/// - 主引擎成功：与兜底引擎结果**合并**（按 span 去重、置信度高者胜出），
///   突破纯规则引擎的召回率上限
/// - 主引擎失败：整体回退到兜底引擎（保持离线可用性）
pub struct CompositeDetector {
    pub lang: String,
    pub primary: Arc<dyn GecEngine>,
    pub fallback: Option<Arc<dyn GecEngine>>,
    /// 对外报告的引擎类型（缺省为 `primary.kind()`；
    /// 如决策层挂载时主引擎是规则引擎，对外报告 `decision`）
    kind_override: Option<GecEngineKind>,
}

impl CompositeDetector {
    pub fn new(
        lang: impl Into<String>,
        primary: Arc<dyn GecEngine>,
        fallback: Option<Arc<dyn GecEngine>>,
    ) -> Self {
        Self {
            lang: lang.into(),
            primary,
            fallback,
            kind_override: None,
        }
    }

    /// 覆盖对外报告的引擎类型（builder）
    pub fn with_kind_override(mut self, kind: GecEngineKind) -> Self {
        self.kind_override = Some(kind);
        self
    }
}

/// 三级响应检测的完整结果（错误列表 + 决策模型弃权信号 + LLM 复查确认干净标记）
///
/// `decision_abstained` 仅在决策层启用时有意义：
/// - `true`：决策模型明确弃权（未命中 / 概率低于阈值），
///   前端据此展示「决策模型弃权 → LLM 兜底已触发」提示；
/// - `false`：决策层未启用，或决策模型做出了明确判断。
///
/// `llm_confirmed_clean`：本次检测中 **LLM 兜底层实际执行过**（level 2 弃权触发 /
/// level 3 / force_llm），且 LLM 未检出任何错误。供 L3 飞轮采集负样本使用——
/// 只有「LLM 复查确认干净」的句子才能作为特征模型训练负样本；
/// level 1（无 LLM 复查）「未检出」可能只是规则 / 决策模型漏报，不可视为干净。
#[derive(Debug, Clone, Default)]
pub struct DetectOutcome {
    /// 检测到的错误列表
    pub errors: Vec<Error>,
    /// 决策模型是否弃权
    pub decision_abstained: bool,
    /// LLM 兜底层是否实际执行且未检出错误（「LLM 复查确认干净」）
    pub llm_confirmed_clean: bool,
    /// 各阶段计时（规则 / NLP（含预热）/ 决策 / L3 / 知识召回 / LLM / 总耗时）；
    /// 全部为零表示未实际执行检测（如缓存命中）
    pub timings: DetectTimings,
    /// 层级贡献摘要：本次检测**实际参与**的每层状态与检出数（前端「检测路径」展示）；
    /// 空 = 未产生（缓存命中 / 历史复用等未实际检测路径）
    pub layers: Vec<LayerReport>,
}

/// 层级贡献报告：本次检测实际参与的每一层各一条
/// （`rule` / `decision` / `knowledge` / `feature` / `llm` / `statistical`）
///
/// `status` 取值：
/// - `ran`：已执行且结果参与合并；
/// - `decided`：特征模型高置信定案（LLM 收缩未触发）；
/// - `abstained`：模型明确弃权（长尾交由 LLM 兜底）；
/// - `unavailable`：该层服务调用失败（按策略回退，不阻断检测）；
/// - `skipped`：该层被评估但未执行（如 L3 已定案 → LLM 跳过）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LayerReport {
    /// 层名（与 [`shared_model::sources`] 常量对齐）
    pub layer: String,
    /// 层状态（见 [`LayerReport`] 文档）
    pub status: String,
    /// 该层检出的错误条数（`abstained` / `skipped` 通常为 0）
    pub found: usize,
}

impl LayerReport {
    pub fn new(layer: &str, status: &str, found: usize) -> Self {
        Self {
            layer: layer.to_string(),
            status: status.to_string(),
            found,
        }
    }
}

/// 为无来源标记的错误补标来源：规则层错误在检测器侧不带 `source`，
/// 合并前统一补标，使前端可展示来源徽章、[`merge_errors`] 可记录多层共识
fn tag_source(errors: &mut [Error], src: &str) {
    for e in errors {
        if e.source.is_none() {
            e.source = Some(src.to_string());
        }
    }
}

/// 字符偏移 → 字节偏移（外部模型服务 span 换算）
///
/// Python 服务（decision-model / feature-model）返回的是**字符偏移**
/// （Python `re` / `len` 语义），而 [`Error::span`] 约定为**字节偏移**
/// （Rust 字符串字节下标）——CJK 文本两者相差约 3 倍，不换算会导致
/// [`merge_errors`] 的 span 重叠去重对中文误判（假重叠 → 真实错误被吞；
/// 漏重叠 → UI 重复条目）。本函数借助原文把字符 span 换算为字节 span
/// （越界钳制到文本长度；`start > end` 时按 `start = end` 收敛为零长）。
pub fn char_span_to_bytes(text: &str, span: [usize; 2]) -> [usize; 2] {
    let n = text.chars().count();
    let s = span[0].min(n);
    let e = span[1].max(s).min(n);
    let byte_s = text
        .char_indices()
        .nth(s)
        .map(|(b, _)| b)
        .unwrap_or(text.len());
    let byte_e = if e >= n {
        text.len()
    } else {
        text.char_indices()
            .nth(e)
            .map(|(b, _)| b)
            .unwrap_or(text.len())
    };
    [byte_s, byte_e]
}

/// 两个 span 是否重叠（半开区间 [start, end)；零长区间或仅端点相邻不算重叠）
fn spans_overlap(a: [usize; 2], b: [usize; 2]) -> bool {
    a[0] < b[1] && b[0] < a[1]
}

/// 合并两路检测结果：
/// - **按 span 重叠去重**：新错误 span 与已有错误 span **相交**（精确相等是特例）
///   时保留置信度高者——不同层（决策模型句级 span / L3 / LLM span 级）对同一错误
///   切出的 span 往往不完全一致，重叠合并避免同一错误在 UI 出现重复条目；
/// - 否则追加；最后按 span 起始偏移排序。
pub fn merge_errors(primary: &mut Vec<Error>, extra: impl IntoIterator<Item = Error>) {
    for mut e in extra {
        if let Some(p) = primary.iter_mut().find(|p| spans_overlap(p.span, e.span)) {
            // 重叠 → 合并：保留置信度高者；置信度相同保留 span 更窄（更精确）者
            let e_wins = if (e.confidence - p.confidence).abs() < f64::EPSILON {
                e.span[1] - e.span[0] < p.span[1] - p.span[0]
            } else {
                e.confidence > p.confidence
            };
            // 多层共识：落选方的来源记入胜出者的 `also_found_by`
            // （同层重复命中由 `add_also_found_by` 按 source 去重，不会自标共识）
            if e_wins {
                if let Some(src) = p.source.clone() {
                    e.add_also_found_by(&src);
                }
                let carried = std::mem::take(&mut p.also_found_by);
                for src in carried {
                    e.add_also_found_by(&src);
                }
                *p = e;
            } else {
                if let Some(src) = e.source.clone() {
                    p.add_also_found_by(&src);
                }
                let carried = e.also_found_by;
                for src in carried {
                    p.add_also_found_by(&src);
                }
            }
        } else {
            primary.push(e);
        }
    }
    primary.sort_by_key(|e| e.span[0]);
}

/// 定向 LLM 复查（L3 部分定案的灰区句）：
/// 把灰区 span 拼成摘录 → LLM 检测摘录 → 返回 span 映射回原文。
///
/// - span 为**字节偏移**（与 [`Error::span`] 同语义，来自 [`crate::l3`] 的
///   字符→字节换算，保证 UTF-8 边界安全）；重叠/相邻 span 先合并；
/// - 摘录以 `\n` 分隔各段；LLM 返回的 span 若落在分隔符或跨段，
///   钳制到最近重叠段（LLM 偶发跨句编辑时保守收缩，不丢整条）；
/// - 无法对应任何段的 LLM 错误（span 越界）丢弃并记 warn。
pub async fn targeted_llm_review(
    llm: &LlmGecEngine,
    text: &str,
    spans: &[[usize; 2]],
) -> Result<Vec<Error>, DetectorError> {
    // ① 越界钳制 + 排序 + 合并重叠/相邻 span
    let mut sp: Vec<[usize; 2]> = spans
        .iter()
        .map(|s| [s[0].min(text.len()), s[1].min(text.len())])
        .filter(|s| s[0] < s[1])
        .collect();
    sp.sort_by_key(|s| s[0]);
    let mut merged: Vec<[usize; 2]> = Vec::new();
    for s in sp {
        match merged.last_mut() {
            Some(last) if s[0] <= last[1] + 1 => last[1] = last[1].max(s[1]),
            _ => merged.push(s),
        }
    }
    if merged.is_empty() {
        return Ok(Vec::new());
    }
    // ② 摘录 + 段表 [excerpt_start, excerpt_end, orig_start]
    let mut excerpt = String::new();
    let mut segs: Vec<[usize; 3]> = Vec::with_capacity(merged.len());
    for [s, e] in merged {
        if !excerpt.is_empty() {
            excerpt.push('\n');
        }
        let es = excerpt.len();
        excerpt.push_str(&text[s..e]);
        segs.push([es, excerpt.len(), s]);
    }
    // ③ LLM 检测摘录（span 相对摘录），映射回原文
    let found = llm.detect(&excerpt).await?;
    let mut out = Vec::with_capacity(found.len());
    for mut err in found {
        let seg = segs
            .iter()
            .find(|g| err.span[0] < g[1] && g[0] < err.span[1]);
        let Some(&[gs, ge, os]) = seg else {
            tracing::warn!(
                original = %err.original,
                "定向 LLM 复查：错误 span 不在任何灰区段内，丢弃"
            );
            continue;
        };
        let shift = os - gs;
        err.span = [
            err.span[0].clamp(gs, ge) + shift,
            err.span[1].clamp(gs, ge) + shift,
        ];
        out.push(err);
    }
    Ok(out)
}

#[async_trait]
impl GecEngine for CompositeDetector {
    fn kind(&self) -> GecEngineKind {
        self.kind_override.unwrap_or_else(|| self.primary.kind())
    }
    fn name(&self) -> &str {
        &self.lang
    }
    async fn detect(&self, text: &str) -> Result<Vec<Error>, DetectorError> {
        match self.primary.detect(text).await {
            Ok(mut errors) => {
                tag_source(&mut errors, self.primary.kind().as_str());
                if let Some(fb) = &self.fallback {
                    match fb.detect(text).await {
                        Ok(mut rule_errors) => {
                            tag_source(&mut rule_errors, fb.kind().as_str());
                            merge_errors(&mut errors, rule_errors);
                        }
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                engine = self.primary.name(),
                                "兜底引擎检测失败，仅保留主引擎结果"
                            );
                        }
                    }
                }
                Ok(errors)
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    engine = self.primary.name(),
                    "主引擎不可用，回退兜底引擎"
                );
                match &self.fallback {
                    Some(fb) => {
                        let mut errors = fb.detect(text).await?;
                        tag_source(&mut errors, fb.kind().as_str());
                        Ok(errors)
                    }
                    None => Err(e),
                }
            }
        }
    }
    fn last_nlp_call(&self) -> Option<NlpCall> {
        self.primary
            .last_nlp_call()
            .or_else(|| self.fallback.as_ref().and_then(|fb| fb.last_nlp_call()))
    }
}

/// 三级级联检测器：规则（主）+ 决策模型（弃权信号）+ 知识向量召回 + LLM 兜底（按需）
///
/// - `detect()`（= 1 级）：规则为主 + 决策模型合并（决策服务宕机自动回退纯规则，
///   与 [`CompositeDetector`] 行为一致）；
/// - `detect_level(2)`：1 级 + **L3 门控**（P0.5 重绑：L3 特征模型是唯一的
///   pre-LLM 门控）——L3 启用时高置信判定直接定案（不跑 LLM），弃权 / 服务
///   不可用时对全文跑 LLM 兜底；L3 未启用时回退旧语义：决策模型**明确弃权**
///   （`DecideOutcome::abstained`）时跑 LLM 兜底；
/// - `detect_level(3)`：1 级 + L3 门控 + **无条件**强制全文 LLM（「漏报」兜底）。
///
/// ②.5 知识召回（句级）：在 ②（决策模型）之后、③（LLM 兜底）之前，
/// 文本按句切分、每句一个向量在知识库中余弦匹配；命中后两级定位
/// （精确子串 → 精确 span；高相似非子串 → 句级粗定位），修正结果合并到错误列表。
///
/// L3 特征模型（level 2/3 门控层）：高置信判定直接定案，LLM 收缩到纯长尾；
/// L3 判「明确无错」但决策模型弃权时视为模型冲突 → 保留 LLM 长尾兜底（不静默覆盖）。
///
/// LLM 引擎由调用方按请求传入（运行时 LLM 配置 + 检测模板现构建，支持设置热更新）；
/// 触发条件命中但 LLM 未配置 → [`DetectorError::LlmNotConfigured`]（API 层映射 503）。
pub struct CascadeDetector {
    pub lang: String,
    pub rule: Arc<dyn GecEngine>,
    pub decision: Option<Arc<DecisionGecEngine>>,
    /// 知识向量召回（可选；未配置嵌入引擎时为 None，静默跳过）
    pub knowledge_recall: Option<Arc<dyn KnowledgeRecall>>,
    /// L3 特征模型（三期：level 3 先跑特征模型，LLM 收缩到纯长尾；
    /// 未启用时为 None，level 3 回退「无条件 LLM」）
    pub l3: Option<Arc<L3FeatureEngine>>,
    /// 对外报告的引擎类型（决策层挂载时报告 `decision`）
    kind_override: Option<GecEngineKind>,
}

impl CascadeDetector {
    pub fn new(
        lang: impl Into<String>,
        rule: Arc<dyn GecEngine>,
        decision: Option<Arc<DecisionGecEngine>>,
    ) -> Self {
        Self {
            lang: lang.into(),
            rule,
            decision,
            knowledge_recall: None,
            l3: None,
            kind_override: None,
        }
    }

    /// 覆盖对外报告的引擎类型（builder）
    pub fn with_kind_override(mut self, kind: GecEngineKind) -> Self {
        self.kind_override = Some(kind);
        self
    }
}

#[async_trait]
impl GecEngine for CascadeDetector {
    fn kind(&self) -> GecEngineKind {
        self.kind_override.unwrap_or_else(|| self.rule.kind())
    }
    fn name(&self) -> &str {
        &self.lang
    }
    fn decision_engine(&self) -> Option<&crate::decision::DecisionGecEngine> {
        self.decision.as_deref()
    }
    async fn detect(&self, text: &str) -> Result<Vec<Error>, DetectorError> {
        self.detect_level(text, 1, false, None)
            .await
            .map(|o| o.errors)
    }

    async fn detect_level(
        &self,
        text: &str,
        level: u8,
        force_llm: bool,
        llm: Option<&LlmGecEngine>,
    ) -> Result<DetectOutcome, DetectorError> {
        if !(1..=3).contains(&level) {
            return Err(DetectorError::DetectionFailed(format!(
                "无效响应级别: {level}（应为 1-3）"
            )));
        }
        let started = std::time::Instant::now();
        // ①+②(+③-F) 规则 / 决策模型 / L3 特征模型**并行**执行：
        // 三者都是全文级判断、互不依赖对方结果，总延迟 = 各层 max
        // （此前为串行求和）；规则不可用时回退决策模型（保持既有故障回退行为，
        // 且决策服务只调一次——旧实现在回退路径会再调一次 `decide`）。
        // 各分支独立计时（wall time），用于 [`DetectTimings`]
        let ((rule_res, rule_ms), (dec_res, dec_ms), (l3_res, l3_ms)) = tokio::join!(
            async {
                let t = std::time::Instant::now();
                let r = self.rule.detect(text).await;
                (r, t.elapsed().as_millis() as u64)
            },
            async {
                match &self.decision {
                    Some(d) => {
                        let t = std::time::Instant::now();
                        let r = d.decide(text).await.map(Some);
                        (r, t.elapsed().as_millis() as u64)
                    }
                    None => (Ok(None), 0),
                }
            },
            async {
                if level >= 2 && !force_llm {
                    match &self.l3 {
                        Some(l3) => {
                            let t = std::time::Instant::now();
                            let r = l3.detect(text).await;
                            (Some(r), t.elapsed().as_millis() as u64)
                        }
                        None => (None, 0),
                    }
                } else {
                    (None, 0)
                }
            },
        );
        // ① 中 NLP 服务调用信息（首次调用含冷启动预热 → 单独记 nlp_warmup_ms）
        let nlp = self.rule.last_nlp_call();
        // 决策模型结果：`dec = None` 表示未启用或调用失败（`dec_err` 带错误）
        let (dec, dec_err) = match dec_res {
            Ok(o) => (o, None),
            Err(e) => (None, Some(e)),
        };
        let rule_ok = rule_res.is_ok();
        let rule_found = rule_res.as_ref().map(|e| e.len()).unwrap_or(0);
        let mut errors = match rule_res {
            Ok(e) => e,
            Err(e) => match &dec {
                Some(o) => {
                    tracing::warn!(
                        error = %e,
                        engine = self.rule.name(),
                        "主引擎（规则）不可用，回退决策模型"
                    );
                    o.errors.clone()
                }
                None => {
                    // 规则不可用且决策模型失败 / 未启用 →
                    // 优先返回决策层错误（与旧 `d.detect(text).await?` 语义一致），
                    // 否则返回规则错误
                    return Err(dec_err.unwrap_or(e));
                }
            },
        };
        // 规则层错误补标来源 `rule`（决策 / L3 / 召回 / LLM 层各自产出时已自标）
        tag_source(&mut errors, sources::RULE);
        let mut layers = vec![LayerReport::new(
            sources::RULE,
            if rule_ok { "ran" } else { "unavailable" },
            rule_found,
        )];
        // ② 决策模型（若启用）：合并 + 弃权信号
        let abstained = match &dec {
            Some(o) => {
                merge_errors(&mut errors, o.errors.clone());
                o.abstained
            }
            None => {
                if let Some(e) = &dec_err {
                    tracing::warn!(
                        error = %e,
                        engine = self.rule.name(),
                        "决策模型检测失败，仅保留规则结果"
                    );
                }
                false
            }
        };
        if self.decision.is_some() {
            let status = if dec_err.is_some() {
                "unavailable"
            } else if abstained {
                "abstained"
            } else {
                "ran"
            };
            layers.push(LayerReport::new(
                sources::DECISION,
                status,
                dec.as_ref().map(|o| o.errors.len()).unwrap_or(0),
            ));
        }
        // ②.5 知识召回（句级）：文本按句切分、每句一个向量余弦匹配知识库；
        // 命中后两级定位：精确子串 → 精确 span；高相似非子串 → 句级粗定位（模糊召回）。
        // 嵌入服务不可用时实现侧静默返回空，不阻断检测
        let mut recall_ms = None;
        if let Some(kb) = &self.knowledge_recall {
            let t = std::time::Instant::now();
            let hits = kb.recall(&self.lang, text).await;
            recall_ms = Some(t.elapsed().as_millis() as u64);
            let mut recall_found = 0usize;
            for hit in &hits {
                if let Some(err) = knowledge_hit_to_error(hit) {
                    recall_found += 1;
                    merge_errors(&mut errors, vec![err]);
                }
            }
            layers.push(LayerReport::new(sources::KNOWLEDGE, "ran", recall_found));
            match hits.first().map(|h| h.sim) {
                Some(top) => tracing::debug!(
                    lang = %self.lang,
                    count = hits.len(),
                    top_similarity = top,
                    "知识召回（句级）命中（已合并到错误列表）"
                ),
                None => tracing::debug!(
                    lang = %self.lang,
                    "知识召回（句级）未命中（无句子达到阈值，或知识库为空）"
                ),
            }
        }
        // ③ LLM 兜底：force_llm 无条件触发 / level 2 门控触发（P0.5：L3 是唯一的
        // pre-LLM 门控——L3 弃权 / 服务不可用 → 长尾兜底；L3 未启用时回退旧语义：
        // 决策模型弃权触发）/ level 3 无条件
        let l3_gate_open = match &l3_res {
            Some(Ok(o)) => o.abstained,
            Some(Err(_)) => true,
            None => false,
        };
        let mut need_llm = force_llm
            || match level {
                1 => false,
                2 => abstained || l3_gate_open,
                _ => true,
            };
        // ③-F 特征模型（level 2/3 门控层，非 force_llm）：
        // 已在上方与 ①② **并行**跑完（`l3_res`）；
        // 高置信判定直接定案（有错 → 合并错误项；明确无错 → 放行），
        // LLM 收缩到纯长尾：仅特征模型弃权 / 服务不可用时才调用。
        // **部分定案**（`unreviewed_spans` 非空）：定案部分保留，灰区句不再全文
        // 兜底，改由 ③-T 定向 LLM 复查（成本 ≈ 灰区句长度）
        let mut targeted_spans: Vec<[usize; 2]> = Vec::new();
        if let Some(l3_res) = l3_res {
            match l3_res {
                Ok(o) if !o.abstained => {
                    layers.push(LayerReport::new(
                        sources::FEATURE,
                        "decided",
                        o.errors.len(),
                    ));
                    if !o.unreviewed_spans.is_empty() {
                        targeted_spans = o.unreviewed_spans;
                    }
                    let has_errors = !o.errors.is_empty();
                    if has_errors {
                        merge_errors(&mut errors, o.errors);
                    }
                    if !targeted_spans.is_empty() {
                        // 部分定案：灰区句待定向复查（不整体放行，也不全文兜底）
                    } else if !has_errors {
                        // L3「明确无错」：若决策模型已弃权，两个模型意见冲突
                        // （决策模型不确定、L3 却自信）——不静默覆盖：
                        // 保留 LLM 长尾兜底并记 warn（L3 负样本源自 LLM 判定，
                        // 存在系统性盲区时此处是最后防线）
                        if abstained {
                            tracing::warn!(
                                "L3 特征模型判「明确无错」但决策模型弃权：模型冲突，保留 LLM 长尾兜底"
                            );
                        } else {
                            need_llm = false;
                        }
                    } else {
                        need_llm = false;
                    }
                }
                Ok(o) => {
                    // 弃权 → 保持 need_llm = true（长尾）
                    layers.push(LayerReport::new(
                        sources::FEATURE,
                        "abstained",
                        o.errors.len(),
                    ));
                }
                Err(e) => {
                    layers.push(LayerReport::new(sources::FEATURE, "unavailable", 0));
                    tracing::warn!(
                        error = %e,
                        engine = "l3-feature",
                        "L3 特征模型调用失败，回退 LLM 兜底"
                    );
                }
            }
        }
        let mut llm_confirmed_clean = false;
        let mut llm_ms = None;
        let mut llm_ran = false;
        if need_llm {
            let llm = llm.ok_or_else(|| {
                DetectorError::LlmNotConfigured(
                    "LLM 兜底检测未配置（请在设置界面配置 LLM 参数，或设置 LLM_API_KEY）"
                        .to_string(),
                )
            })?;
            let t = std::time::Instant::now();
            let extra = llm.detect(text).await?;
            llm_ms = Some(t.elapsed().as_millis() as u64);
            llm_confirmed_clean = extra.is_empty();
            llm_ran = true;
            layers.push(LayerReport::new(sources::LLM, "ran", extra.len()));
            merge_errors(&mut errors, extra);
        } else if !targeted_spans.is_empty() {
            // ③-T 定向 LLM 复查：L3 部分定案的灰区句单独送 LLM，
            // 返回 span 映射回原文后合并（全文其余部分已由 L3 定案，不再送 LLM）
            match llm {
                Some(l) => {
                    let t = std::time::Instant::now();
                    match targeted_llm_review(l, text, &targeted_spans).await {
                        Ok(extra) => {
                            llm_ms = Some(t.elapsed().as_millis() as u64);
                            llm_confirmed_clean = extra.is_empty();
                            llm_ran = true;
                            layers.push(LayerReport::new(sources::LLM, "ran", extra.len()));
                            merge_errors(&mut errors, extra);
                        }
                        Err(e) => {
                            layers.push(LayerReport::new(sources::LLM, "unavailable", 0));
                            tracing::warn!(
                                error = %e,
                                "定向 LLM 复查失败：保留 L3 定案结果，灰区句本轮未复查"
                            );
                        }
                    }
                }
                None => {
                    tracing::debug!("L3 部分定案存在灰区句但未配置 LLM → 本轮放行（灰区句未复查）");
                }
            }
        }
        // LLM 层被评估但未触发（L3 高置信定案 / 无弃权信号）→ 显式记 skipped，
        // 前端「检测路径」据此区分「LLM 未触发」与「LLM 未参与（level 1）」
        // （定向复查失败已记 `unavailable`，不再重复记 skipped）
        if (force_llm || level >= 2)
            && !llm_ran
            && !(need_llm || (!targeted_spans.is_empty() && llm.is_some()))
        {
            layers.push(LayerReport::new(sources::LLM, "skipped", 0));
        }
        let timings = DetectTimings {
            rule_ms,
            nlp_ms: nlp.map(|c| c.ms),
            nlp_warmup_ms: nlp.filter(|c| c.cold_start).map(|c| c.ms),
            decision_ms: if self.decision.is_some() {
                Some(dec_ms)
            } else {
                None
            },
            l3_ms: if level >= 2 && !force_llm && self.l3.is_some() {
                Some(l3_ms)
            } else {
                None
            },
            recall_ms,
            llm_ms,
            total_ms: started.elapsed().as_millis() as u64,
        };
        tracing::debug!(
            lang = %self.lang,
            level,
            rule_ms,
            nlp_ms = timings.nlp_ms,
            nlp_warmup_ms = timings.nlp_warmup_ms,
            decision_ms = timings.decision_ms,
            l3_ms = timings.l3_ms,
            recall_ms = timings.recall_ms,
            llm_ms = timings.llm_ms,
            total_ms = timings.total_ms,
            "检测各阶段计时"
        );
        Ok(DetectOutcome {
            errors,
            decision_abstained: abstained,
            llm_confirmed_clean,
            timings,
            layers,
        })
    }
}

/// 按 `GEC_ENGINE_{LANG}` 环境变量组合该语言的最终引擎：
/// - `rule`（默认）：纯规则检测器；若决策模型层已启用
///   （`decision` 参数 `enabled`，来自设置文件 / `GEC_DEC_*` 环境变量）
///   → [`CascadeDetector`]（规则为主 + 决策模型合并 + 弃权信号，
///   服务宕机自动回退纯规则；三级响应 level 2/3 的 LLM 兜底按请求触发）；
///   决策层关闭但 `knowledge_recall` 已配置 → 同样挂载 [`CascadeDetector`]
///   （②.5 知识向量召回独立于决策层生效：命中时合并修正结果）
/// - `decision`：显式启用决策模型层（vegaml，处理规则覆盖不到的模糊情况）
/// - `statistical`：统计 GEC 服务为主 + 规则检测器兜底（结果合并）
/// - `llm`：预留，当前等价于 `rule`
///
/// `decision`：决策模型层配置（运行时设置优先）；`None` 表示未显式配置
/// （`decision` 类型时从 `GEC_DEC_*` 环境变量读取，`rule` 类型时决策层关闭）。
pub fn compose_detector<D: GECDetector + 'static>(
    lang: &str,
    rule: Arc<D>,
    decision: Option<DecisionConfig>,
    knowledge_recall: Option<Arc<dyn KnowledgeRecall>>,
    l3: Option<L3FeatureConfig>,
) -> Arc<dyn GecEngine> {
    // L3 特征模型引擎（三期：level 3 先跑特征模型，LLM 收缩到纯长尾）
    let l3_engine: Option<Arc<L3FeatureEngine>> = l3
        .as_ref()
        .filter(|c| c.enabled())
        .map(|c| Arc::new(L3FeatureEngine::new(lang, c.clone())));
    match GecEngineKind::from_env(lang) {
        GecEngineKind::Statistical => {
            let primary = Arc::new(StatisticalGecEngine::from_env(lang));
            let fallback: Arc<dyn GecEngine> = rule;
            tracing::info!(
                lang,
                model = primary.name(),
                "统计 GEC 引擎已启用（规则引擎兜底）"
            );
            Arc::new(CompositeDetector::new(lang, primary, Some(fallback)))
        }
        GecEngineKind::Decision => {
            let cfg = decision.unwrap_or_else(DecisionConfig::from_env);
            let secondary = Arc::new(DecisionGecEngine::new(lang, cfg));
            tracing::info!(
                lang,
                model = secondary.name(),
                "决策模型引擎已启用（规则引擎为主，决策模型处理模糊情况）"
            );
            let mut det = CascadeDetector::new(lang, rule, Some(secondary))
                .with_kind_override(GecEngineKind::Decision);
            det.knowledge_recall = knowledge_recall;
            if let Some(e) = l3_engine {
                det.l3 = Some(e);
                tracing::info!(
                    lang,
                    "L3 特征模型层已启用（level 3 先跑特征模型，LLM 收缩到长尾）"
                );
            }
            Arc::new(det)
        }
        GecEngineKind::Llm => {
            tracing::debug!(lang, "LLM 引擎为预留类型，当前使用规则引擎");
            rule
        }
        GecEngineKind::Rule => match decision {
            Some(cfg) if cfg.enabled() => {
                let secondary = Arc::new(DecisionGecEngine::new(lang, cfg));
                tracing::info!(
                    lang,
                    model = secondary.name(),
                    "决策模型层已启用（规则为主 + 决策模型合并；服务宕机自动回退纯规则）"
                );
                let mut det = CascadeDetector::new(lang, rule, Some(secondary))
                    .with_kind_override(GecEngineKind::Decision);
                det.knowledge_recall = knowledge_recall;
                if let Some(e) = l3_engine {
                    det.l3 = Some(e);
                    tracing::info!(
                        lang,
                        "L3 特征模型层已启用（level 3 先跑特征模型，LLM 收缩到长尾）"
                    );
                }
                Arc::new(det)
            }
            _ => match knowledge_recall {
                // 决策层关闭但知识召回已配置：仍挂载级联检测器，
                // ②.5 知识向量召回独立于决策层生效
                Some(kr) => {
                    tracing::info!(
                        lang,
                        "决策层未启用但知识召回已配置：挂载级联检测器（规则 + 知识向量召回 ②.5）"
                    );
                    let mut det = CascadeDetector::new(lang, rule, None);
                    det.knowledge_recall = Some(kr);
                    if let Some(e) = l3_engine {
                        det.l3 = Some(e);
                        tracing::info!(
                            lang,
                            "L3 特征模型层已启用（level 3 先跑特征模型，LLM 收缩到长尾）"
                        );
                    }
                    Arc::new(det)
                }
                None => rule,
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge_recall::RecallHit;
    use crate::llm::LlmDetectConfig;
    use crate::statistical::StatisticalConfig;
    use async_trait::async_trait;
    use axum::response::IntoResponse;
    use shared_model::LearnedKnowledge;
    use std::time::Duration;

    struct FakeRule {
        lang: &'static str,
        fail: bool,
    }

    #[async_trait]
    impl GECDetector for FakeRule {
        fn language(&self) -> &str {
            self.lang
        }
        async fn detect(&self, text: &str) -> Result<Vec<Error>, DetectorError> {
            if self.fail {
                Err(DetectorError::ServiceUnavailable("rule down".into()))
            } else {
                Ok(vec![Error::new(
                    self.lang,
                    "spelling",
                    0.9,
                    [0, 2],
                    text,
                    Some("fix".into()),
                    "rule error",
                )])
            }
        }
    }

    #[test]
    fn merge_dedup_and_sort() {
        let mut primary = vec![
            Error::new("en", "grammar", 0.6, [5, 9], "b", Some("c".into()), "p1"),
            Error::new("en", "spelling", 0.9, [0, 2], "a", None, "p2"),
        ];
        let extra = vec![
            Error::new("en", "grammar", 0.8, [5, 9], "b", Some("d".into()), "s1"),
            Error::new("en", "tense", 0.7, [10, 12], "e", None, "s2"),
        ];
        merge_errors(&mut primary, extra);
        assert_eq!(primary.len(), 3);
        assert_eq!(primary[0].span, [0, 2]);
        assert_eq!(primary[1].span, [5, 9]);
        assert_eq!(primary[1].confidence, 0.8, "同 span 应保留置信度高者");
        assert_eq!(primary[2].span, [10, 12]);
    }

    fn sourced(conf: f64, span: [usize; 2], src: &str, msg: &str) -> Error {
        let mut e = Error::new("en", "spelling", conf, span, "a", Some("x".into()), msg);
        e.source = Some(src.to_string());
        e
    }

    #[test]
    fn merge_records_multilayer_consensus() {
        // ④ 多层共识：跨层重叠命中时，落选层来源记入胜出者 `also_found_by`
        let mut primary = vec![sourced(0.4, [0, 2], "rule", "r")];
        merge_errors(
            &mut primary,
            vec![sourced(0.95, [0, 2], "statistical", "s")],
        );
        assert_eq!(primary.len(), 1);
        assert_eq!(primary[0].confidence, 0.95);
        assert_eq!(primary[0].source.as_deref(), Some("statistical"));
        assert_eq!(primary[0].also_found_by, vec!["rule".to_string()]);
        // 同层重复命中不算共识（source 相同 → 忽略）
        merge_errors(
            &mut primary,
            vec![sourced(0.3, [0, 2], "statistical", "s2")],
        );
        assert_eq!(primary[0].also_found_by, vec!["rule".to_string()]);
        // 胜出者被替换时共识随迁：rule(0.4) → statistical(0.95) → llm(0.99)
        let mut p2 = vec![sourced(0.4, [0, 2], "rule", "r")];
        merge_errors(&mut p2, vec![sourced(0.95, [0, 2], "statistical", "s")]);
        merge_errors(&mut p2, vec![sourced(0.99, [0, 2], "llm", "l")]);
        assert_eq!(p2.len(), 1);
        assert_eq!(p2[0].source.as_deref(), Some("llm"));
        let mut also: Vec<&str> = p2[0].also_found_by.iter().map(|s| s.as_str()).collect();
        also.sort();
        assert_eq!(also, vec!["rule", "statistical"]);
    }

    #[tokio::test]
    async fn cascade_tags_rule_source_and_reports_layers() {
        // ② 规则层错误补标 source = "rule"；③ layers 报告规则层参与与检出数
        let rule: Arc<dyn GecEngine> = Arc::new(FakeRule {
            lang: "en",
            fail: false,
        });
        let det = CascadeDetector::new("en", rule, None);
        let outcome = det.detect_level("ab", 1, false, None).await.unwrap();
        assert_eq!(outcome.errors[0].source.as_deref(), Some("rule"));
        assert_eq!(outcome.layers.len(), 1);
        assert_eq!(outcome.layers[0].layer, "rule");
        assert_eq!(outcome.layers[0].status, "ran");
        assert_eq!(outcome.layers[0].found, 1);
    }

    #[tokio::test]
    async fn cascade_level2_without_gates_marks_llm_skipped() {
        // level 2 但无决策层 / L3（无弃权信号、无门控）→ LLM 未触发 → layers 记 `skipped`
        let rule: Arc<dyn GecEngine> = Arc::new(FakeRule {
            lang: "en",
            fail: false,
        });
        let det = CascadeDetector::new("en", rule, None);
        let outcome = det.detect_level("ab", 2, false, None).await.unwrap();
        let llm = outcome
            .layers
            .iter()
            .find(|l| l.layer == "llm")
            .expect("level 2 应报告 llm 层状态");
        assert_eq!(llm.status, "skipped");
    }

    #[test]
    fn merge_overlap_keeps_higher_confidence() {
        // 决策模型句级 span [0,4] 与 LLM span 级错误 [1,3] 重叠（非精确相等）
        // → 合并为 1 条，保留置信度高者（LLM 的精确 span）
        let mut primary = vec![Error::new(
            "en",
            "missing",
            0.9,
            [0, 4],
            "text",
            None,
            "句级",
        )];
        let extra = vec![Error::new(
            "en",
            "grammar",
            0.95,
            [1, 3],
            "te",
            Some("tx".into()),
            "span 级",
        )];
        merge_errors(&mut primary, extra);
        assert_eq!(primary.len(), 1, "重叠 span 应合并: {:?}", primary);
        assert_eq!(primary[0].span, [1, 3], "保留置信度高者");
        assert_eq!(primary[0].error_type, "grammar");
    }

    #[test]
    fn char_span_to_bytes_cjk() {
        // 8 个中文字符（24 字节）：字符 [4, 6) → 字节 [12, 18)
        let text = "他昨天吃了昨天。";
        assert_eq!(char_span_to_bytes(text, [4, 6]), [12, 18]);
        // 整句：字符 [0, 8) → 字节 [0, 24)
        assert_eq!(char_span_to_bytes(text, [0, 8]), [0, 24]);
        // 越界钳制：字符 [10, 20) → 字节 [24, 24)
        assert_eq!(char_span_to_bytes(text, [10, 20]), [24, 24]);
        // 倒置 span（start > end）收敛为零长（start = 字符 6 → 字节 18）
        assert_eq!(char_span_to_bytes(text, [6, 4]), [18, 18]);
    }

    #[test]
    fn char_span_to_bytes_ascii_and_mixed() {
        // 纯 ASCII：字符 = 字节
        assert_eq!(char_span_to_bytes("hello world", [0, 5]), [0, 5]);
        assert_eq!(char_span_to_bytes("hello world", [6, 11]), [6, 11]);
        // 混合：a(1B) b(1B) 中(3B) 文(3B) = 8 字节；字符 [2, 4) → 字节 [2, 8)
        assert_eq!(char_span_to_bytes("ab中文", [2, 4]), [2, 8]);
        // 空文本
        assert_eq!(char_span_to_bytes("", [0, 0]), [0, 0]);
        assert_eq!(char_span_to_bytes("", [3, 9]), [0, 0]);
    }

    #[test]
    fn merge_adjacent_and_disjoint_kept() {
        // 端点相邻（[0,2] 与 [2,4]）与不相交（[5,7]）都不算重叠 → 全部保留
        let mut primary = vec![Error::new("en", "a", 0.9, [0, 2], "x", None, "p")];
        let extra = vec![
            Error::new("en", "b", 0.5, [2, 4], "y", None, "s1"),
            Error::new("en", "c", 0.5, [5, 7], "z", None, "s2"),
        ];
        merge_errors(&mut primary, extra);
        assert_eq!(primary.len(), 3, "相邻/不相交 span 应保留: {:?}", primary);
    }

    #[test]
    fn engine_kind_parse() {
        assert_eq!(GecEngineKind::parse("rule"), Some(GecEngineKind::Rule));
        assert_eq!(
            GecEngineKind::parse("statistical"),
            Some(GecEngineKind::Statistical)
        );
        assert_eq!(
            GecEngineKind::parse("STAT"),
            Some(GecEngineKind::Statistical)
        );
        assert_eq!(GecEngineKind::parse("llm"), Some(GecEngineKind::Llm));
        assert_eq!(GecEngineKind::parse("nope"), None);
        // 未设置的变量 → 默认 rule
        assert_eq!(GecEngineKind::from_env("zz"), GecEngineKind::Rule);
    }

    #[test]
    fn engine_kind_from_env() {
        unsafe { std::env::set_var("GEC_ENGINE_TT", "statistical") }
        assert_eq!(GecEngineKind::from_env("tt"), GecEngineKind::Statistical);
        unsafe { std::env::remove_var("GEC_ENGINE_TT") }
        assert_eq!(GecEngineKind::from_env("tt"), GecEngineKind::Rule);
    }

    #[tokio::test]
    async fn composite_falls_back_when_primary_down() {
        let primary = Arc::new(StatisticalGecEngine::new(
            "en",
            StatisticalConfig {
                endpoint: "http://127.0.0.1:1".into(),
                timeout: Duration::from_millis(200),
                model: "dead".into(),
            },
        ));
        let fallback: Arc<dyn GecEngine> = Arc::new(FakeRule {
            lang: "en",
            fail: false,
        });
        let composite = CompositeDetector::new("en", primary, Some(fallback));
        assert_eq!(composite.kind(), GecEngineKind::Statistical);
        let errors = composite.detect("hello").await.unwrap();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].error_type, "spelling");
    }

    #[tokio::test]
    async fn composite_merges_primary_and_fallback() {
        // mock 统计服务：[0,2] 高置信 + [4,6] 低置信
        let app = axum::Router::new().route(
            "/gec",
            axum::routing::post(|body: axum::body::Bytes| async move {
                let _ = body;
                (
                    axum::http::StatusCode::OK,
                    axum::Json(serde_json::json!({
                        "errors": [
                            { "error_type": "grammar", "confidence": 0.95, "span": [0, 2], "original": "a", "correction": "b", "message": "stat" },
                            { "error_type": "tense", "confidence": 0.7, "span": [4, 6], "original": "c", "message": "stat2" }
                        ]
                    })),
                )
                    .into_response()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });

        let primary = Arc::new(StatisticalGecEngine::new(
            "en",
            StatisticalConfig {
                endpoint: format!("http://{addr}"),
                timeout: Duration::from_secs(5),
                model: "mock".into(),
            },
        ));

        // 规则引擎：[0,2] 低置信（应被统计结果覆盖）+ [3,4] 独有
        struct FakeRule2;
        #[async_trait]
        impl GECDetector for FakeRule2 {
            fn language(&self) -> &str {
                "en"
            }
            async fn detect(&self, _text: &str) -> Result<Vec<Error>, DetectorError> {
                Ok(vec![
                    Error::new("en", "spelling", 0.4, [0, 2], "a", Some("x".into()), "low"),
                    Error::new("en", "article", 0.8, [3, 4], "a", None, "rule"),
                ])
            }
        }
        let fallback: Arc<dyn GecEngine> = Arc::new(FakeRule2);
        let composite = CompositeDetector::new("en", primary, Some(fallback));
        let errors = composite.detect("abc").await.unwrap();
        assert_eq!(errors.len(), 3);
        assert_eq!(errors[0].span, [0, 2]);
        assert_eq!(
            errors[0].confidence, 0.95,
            "同 span 统计结果（0.95）应覆盖规则结果（0.4）"
        );
        assert_eq!(errors[1].span, [3, 4]);
        assert_eq!(errors[2].span, [4, 6]);
    }

    /// 假知识召回：返回固定知识条目（模拟 ②.5 句级命中）
    struct FakeRecall;

    #[async_trait]
    impl KnowledgeRecall for FakeRecall {
        async fn recall(&self, _lang: &str, text: &str) -> Vec<RecallHit> {
            vec![RecallHit {
                item: LearnedKnowledge {
                    id: 1,
                    lang: "en".into(),
                    text: "gpus".into(),
                    error_type: "grammar".into(),
                    correction: "GPU".into(),
                    embedding: vec![],
                    source: "test".into(),
                    created_at: String::new(),
                },
                sim: 0.95,
                sentence: text.to_string(),
                sentence_span: (0, text.len()),
            }]
        }
    }

    #[tokio::test]
    async fn cascade_merges_knowledge_recall() {
        // ②.5：知识召回命中 → 修正结果以 source = "knowledge" 合并进错误列表
        let rule: Arc<dyn GecEngine> = Arc::new(FakeRule {
            lang: "en",
            fail: false,
        });
        let mut det = CascadeDetector::new("en", rule, None);
        det.knowledge_recall = Some(Arc::new(FakeRecall));
        let outcome = det
            .detect_level("the gpus are fast", 1, false, None)
            .await
            .unwrap();
        assert!(
            outcome
                .errors
                .iter()
                .any(|e| e.source.as_deref() == Some("knowledge")),
            "知识召回命中应合并进错误列表: {:?}",
            outcome.errors
        );
    }

    #[tokio::test]
    async fn compose_rule_with_recall_mounts_cascade() {
        // 决策层关闭 + 知识召回已配置 → 挂载 CascadeDetector（②.5 独立于决策层）
        let rule = Arc::new(FakeRule {
            lang: "en",
            fail: false,
        });
        let det = compose_detector("zz", rule, None, Some(Arc::new(FakeRecall)), None);
        let errors = det.detect("the gpus are fast").await.unwrap_or_default();
        assert!(
            errors
                .iter()
                .any(|e| e.source.as_deref() == Some("knowledge")),
            "级联检测器挂载后 ②.5 应命中: {:?}",
            errors
        );
    }

    #[test]
    fn engine_kind_parse_decision() {
        assert_eq!(
            GecEngineKind::parse("decision"),
            Some(GecEngineKind::Decision)
        );
        assert_eq!(GecEngineKind::parse("DEC"), Some(GecEngineKind::Decision));
        assert_eq!(GecEngineKind::Decision.as_str(), "decision");
    }

    /// 本地 mock 决策模型服务（句级：has_error + error_type）
    async fn mock_decide_endpoint() -> String {
        let app = axum::Router::new().route(
            "/decide",
            axum::routing::post(|_body: axum::body::Bytes| async move {
                (
                    axum::http::StatusCode::OK,
                    axum::Json(serde_json::json!({
                        "has_error": true,
                        "has_error_prob": 0.9,
                        "error_type": "missing",
                        "abstain": false,
                        "errors": []
                    })),
                )
                    .into_response()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        format!("http://{addr}")
    }

    fn decision_cfg(endpoint: String, enabled: bool) -> DecisionConfig {
        DecisionConfig {
            endpoint,
            timeout_secs: 5,
            model: "mock-vegaml".into(),
            abstain_threshold: 0.5,
            enabled,
        }
    }

    /// 带调用计数的 mock 决策服务（验证回退路径的调用次数）
    async fn mock_decide_endpoint_counted(
        calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) -> String {
        let app = axum::Router::new().route(
            "/decide",
            axum::routing::post(move |_body: axum::body::Bytes| async move {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                (
                    axum::http::StatusCode::OK,
                    axum::Json(serde_json::json!({
                        "has_error": true,
                        "has_error_prob": 0.9,
                        "error_type": "missing",
                        "abstain": false,
                        "errors": []
                    })),
                )
                    .into_response()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn cascade_rule_down_calls_decision_exactly_once() {
        // 规则引擎宕机 → 回退决策模型：结果取自决策层，
        // 且决策服务**恰好被调用一次**（旧实现在回退路径会再调一次 `decide`）
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let endpoint = mock_decide_endpoint_counted(calls.clone()).await;
        let rule = Arc::new(FakeRule {
            lang: "cb",
            fail: true,
        });
        let decision = Arc::new(DecisionGecEngine::new("cb", decision_cfg(endpoint, true)));
        let cascade = CascadeDetector::new("cb", rule, Some(decision));
        let outcome = cascade
            .detect_level("搭配不当的句子", 1, false, None)
            .await
            .unwrap();
        assert_eq!(outcome.errors.len(), 1, "回退路径应产出决策层错误");
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "规则失败回退路径中决策服务应恰好被调用一次"
        );
    }

    #[tokio::test]
    async fn compose_decision_merges_rule_and_model() {
        unsafe { std::env::set_var("GEC_ENGINE_DD", "decision") }
        let endpoint = mock_decide_endpoint().await;
        let rule = Arc::new(FakeRule {
            lang: "dd",
            fail: false,
        });
        let engine = compose_detector("dd", rule, Some(decision_cfg(endpoint, true)), None, None);
        assert_eq!(engine.kind(), GecEngineKind::Decision);
        // 规则错误 [0,2]（conf 0.9）+ 决策模型句级错误 [0,4]（conf 0.9）：
        // span 重叠 → 合并为 1 条（置信度相同保留精确的规则 span）
        let errors = engine.detect("text").await.unwrap();
        assert_eq!(errors.len(), 1, "重叠 span 应合并为 1 条: {:?}", errors);
        assert_eq!(errors[0].span, [0, 2], "置信度相同保留精确的规则 span");
        unsafe { std::env::remove_var("GEC_ENGINE_DD") }
    }

    #[tokio::test]
    async fn compose_decision_falls_back_to_rule_when_model_down() {
        unsafe { std::env::set_var("GEC_ENGINE_FF", "decision") }
        let rule = Arc::new(FakeRule {
            lang: "ff",
            fail: false,
        });
        let engine = compose_detector(
            "ff",
            rule,
            Some(decision_cfg("http://127.0.0.1:1".into(), true)),
            None,
            None,
        );
        assert_eq!(engine.kind(), GecEngineKind::Decision);
        // 决策服务宕机 → 主引擎（规则）成功，结果仅含规则错误
        let errors = engine.detect("hello").await.unwrap();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].error_type, "spelling");
        unsafe { std::env::remove_var("GEC_ENGINE_FF") }
    }

    #[tokio::test]
    async fn compose_rule_opt_in_decision_layer() {
        unsafe { std::env::remove_var("GEC_ENGINE_RR") }
        // 未启用决策层 → 纯规则
        let rule = Arc::new(FakeRule {
            lang: "rr",
            fail: false,
        });
        let engine = compose_detector("rr", rule, None, None, None);
        assert_eq!(engine.kind(), GecEngineKind::Rule);
        // 显式关闭 → 纯规则
        let rule2 = Arc::new(FakeRule {
            lang: "rr",
            fail: false,
        });
        let engine2 = compose_detector(
            "rr",
            rule2,
            Some(decision_cfg("http://127.0.0.1:4804".into(), false)),
            None,
            None,
        );
        assert_eq!(engine2.kind(), GecEngineKind::Rule);
        // 启用决策层 → 规则为主 + 决策合并（对外报告 decision）
        let rule3 = Arc::new(FakeRule {
            lang: "rr",
            fail: false,
        });
        let engine3 = compose_detector(
            "rr",
            rule3,
            Some(decision_cfg("http://127.0.0.1:4804".into(), true)),
            None,
            None,
        );
        assert_eq!(engine3.kind(), GecEngineKind::Decision);
    }

    // ---------- 三级级联（CascadeDetector + LLM 兜底） ----------

    /// 本地 mock 决策服务：按参数返回弃权 / 无错 / 错误
    async fn mock_decision_service(abstain: bool, has_error: bool, prob: f64) -> String {
        let app = axum::Router::new().route(
            "/decide",
            axum::routing::post(move |body: axum::body::Bytes| async move {
                let req: serde_json::Value = serde_json::from_slice(&body).unwrap();
                let text = req["text"].as_str().unwrap_or("").to_string();
                let resp = serde_json::json!({
                    "has_error": has_error,
                    "has_error_prob": prob,
                    "error_type": if has_error { "grammar" } else { "none" },
                    "abstain": abstain,
                    "errors": []
                });
                let _ = text;
                (axum::http::StatusCode::OK, axum::Json(resp)).into_response()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        format!("http://{addr}")
    }

    /// 本地 mock LLM 服务：返回一个 span 级错误
    async fn mock_llm_service() -> String {
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(move |body: axum::body::Bytes| async move {
                let req: serde_json::Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(req["model"], "mock-llm");
                let resp = serde_json::json!({
                    "choices": [{ "message": { "content":
                        "[{\"error_type\":\"grammar\",\"confidence\":0.85,\"original\":\"报复\",\"correction\":\"抱负\",\"message\":\"LLM 兜底检出\"}]"
                    } }]
                });
                (axum::http::StatusCode::OK, axum::Json(resp)).into_response()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        format!("http://{addr}/v1")
    }

    fn llm_engine(endpoint: &str) -> LlmGecEngine {
        LlmGecEngine::new(
            "zh",
            LlmDetectConfig::from_parts(
                endpoint,
                "sk-test",
                "mock-llm",
                5,
                "system",
                "user {{text}}",
            ),
        )
    }

    fn cascade_with_decision(rule: Arc<FakeRule>, decision_endpoint: String) -> CascadeDetector {
        let secondary = Arc::new(DecisionGecEngine::new(
            "zh",
            DecisionConfig {
                endpoint: decision_endpoint,
                ..Default::default()
            },
        ));
        CascadeDetector::new("zh", rule, Some(secondary))
    }

    #[tokio::test]
    async fn cascade_level1_is_rule_plus_decision() {
        let endpoint = mock_decision_service(true, false, 0.3).await;
        let cascade = cascade_with_decision(
            Arc::new(FakeRule {
                lang: "zh",
                fail: false,
            }),
            endpoint,
        );
        let errors = cascade.detect("他昨天报复很大。").await.unwrap();
        // 规则错误 + 决策弃权（无错误产出）
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].error_type, "spelling");
    }

    #[tokio::test]
    async fn cascade_level2_triggers_llm_on_abstain() {
        let endpoint = mock_decision_service(true, false, 0.3).await;
        let llm_endpoint = mock_llm_service().await;
        let llm = llm_engine(&llm_endpoint);
        let cascade = cascade_with_decision(
            Arc::new(FakeRule {
                lang: "zh",
                fail: false,
            }),
            endpoint,
        );
        let outcome = cascade
            .detect_level("他昨天报复很大。", 2, false, Some(&llm))
            .await
            .unwrap();
        assert_eq!(outcome.errors.len(), 2, "规则错误 + LLM 兜底错误");
        assert!(
            outcome
                .errors
                .iter()
                .any(|e| e.source.as_deref() == Some("llm")),
            "应包含 LLM 来源错误"
        );
        assert!(outcome.decision_abstained, "决策模型弃权信号应为 true");
    }

    #[tokio::test]
    async fn cascade_level2_skips_llm_when_not_abstained() {
        // 决策模型明确判断（非弃权）→ 不触发 LLM
        let endpoint = mock_decision_service(false, true, 0.9).await;
        let cascade = cascade_with_decision(
            Arc::new(FakeRule {
                lang: "zh",
                fail: false,
            }),
            endpoint,
        );
        let outcome = cascade
            .detect_level("他昨天报复很大。", 2, false, None)
            .await
            .unwrap();
        // 规则 [0,2] 与决策句级 [0,16] 重叠 → 合并为 1 条（置信度相同保留窄 span）
        assert_eq!(
            outcome.errors.len(),
            1,
            "规则与决策句级错误 span 重叠 → 合并，无 LLM: {:?}",
            outcome.errors
        );
        assert!(
            outcome
                .errors
                .iter()
                .all(|e| e.source.as_deref() != Some("llm")),
            "不应包含 LLM 错误"
        );
        assert!(!outcome.decision_abstained, "明确判断不应弃权");
    }

    #[tokio::test]
    async fn cascade_level2_abstain_without_llm_configured_errors() {
        let endpoint = mock_decision_service(true, false, 0.3).await;
        let cascade = cascade_with_decision(
            Arc::new(FakeRule {
                lang: "zh",
                fail: false,
            }),
            endpoint,
        );
        let err = cascade
            .detect_level("他昨天报复很大。", 2, false, None)
            .await
            .unwrap_err();
        assert!(
            matches!(err, DetectorError::LlmNotConfigured(_)),
            "弃权 + 未配置 LLM 应返回 LlmNotConfigured，实际: {err:?}"
        );
    }

    /// force_llm（「漏报」）：无视 level 触发条件，1 级已有错误也强制全文 LLM 兜底
    #[tokio::test]
    async fn cascade_force_llm_triggers_even_with_level1_errors() {
        // 决策模型明确判断（非弃权）→ level 2 本不应触发 LLM
        let endpoint = mock_decision_service(false, true, 0.9).await;
        let llm_endpoint = mock_llm_service().await;
        let llm = llm_engine(&llm_endpoint);
        let cascade = cascade_with_decision(
            Arc::new(FakeRule {
                lang: "zh",
                fail: false,
            }),
            endpoint,
        );
        // force_llm = false：规则 + 决策错误，无 LLM
        let outcome = cascade
            .detect_level("他昨天报复很大。", 2, false, Some(&llm))
            .await
            .unwrap();
        assert!(
            outcome
                .errors
                .iter()
                .all(|e| e.source.as_deref() != Some("llm")),
            "非弃权 + 未强制时不应含 LLM 错误: {:?}",
            outcome.errors
        );
        // force_llm = true：无条件对全文跑 LLM 兜底
        let outcome = cascade
            .detect_level("他昨天报复很大。", 2, true, Some(&llm))
            .await
            .unwrap();
        assert!(
            outcome
                .errors
                .iter()
                .any(|e| e.source.as_deref() == Some("llm")),
            "force_llm 应无条件触发 LLM 兜底: {:?}",
            outcome.errors
        );
    }

    /// 默认实现（无决策层引擎）：level 3 无条件触发 LLM（即使 1 级已有错误）
    #[tokio::test]
    async fn default_detect_level3_triggers_llm_even_with_errors() {
        let llm_endpoint = mock_llm_service().await;
        let llm = llm_engine(&llm_endpoint);
        let engine = Arc::new(FakeRule {
            lang: "zh",
            fail: false,
        });
        // 1 级有错误 → level 3 仍无条件触发 LLM
        let outcome = engine
            .detect_level("正确句子", 3, false, Some(&llm))
            .await
            .unwrap();
        assert!(
            outcome
                .errors
                .iter()
                .any(|e| e.source.as_deref() == Some("llm")),
            "level 3 应无条件触发 LLM 兜底: {:?}",
            outcome.errors
        );
        // level 3 未配置 LLM → LlmNotConfigured
        let err = engine
            .detect_level("正确句子", 3, false, None)
            .await
            .unwrap_err();
        assert!(matches!(err, DetectorError::LlmNotConfigured(_)));
    }

    /// 默认 trait 实现：level=1 + force_llm=true 也应触发 LLM（「漏报」快捷操作不受 level 限制）
    #[tokio::test]
    async fn default_detect_level1_force_llm_triggers() {
        let llm_endpoint = mock_llm_service().await;
        let llm = llm_engine(&llm_endpoint);
        let engine = Arc::new(FakeRule {
            lang: "zh",
            fail: false,
        });
        // level=1 + force_llm=true → 无条件触发 LLM
        let outcome = engine
            .detect_level("正确句子", 1, true, Some(&llm))
            .await
            .unwrap();
        assert!(
            outcome
                .errors
                .iter()
                .any(|e| e.source.as_deref() == Some("llm")),
            "level=1 + force_llm 应触发 LLM 兜底: {:?}",
            outcome.errors
        );
        // level=1 + force_llm=false → 不触发 LLM
        let outcome = engine
            .detect_level("正确句子", 1, false, Some(&llm))
            .await
            .unwrap();
        assert!(
            outcome
                .errors
                .iter()
                .all(|e| e.source.as_deref() != Some("llm")),
            "level=1 无 force_llm 不应触发 LLM: {:?}",
            outcome.errors
        );
        // level=1 + force_llm=true 但未配置 LLM → LlmNotConfigured
        let err = engine
            .detect_level("正确句子", 1, true, None)
            .await
            .unwrap_err();
        assert!(matches!(err, DetectorError::LlmNotConfigured(_)));
    }

    #[tokio::test]
    async fn cascade_level3_triggers_llm_when_no_errors() {
        // 规则宕机 → 回退决策（明确无错）→ 1 级结果为空 → level 3 触发 LLM
        let endpoint = mock_decision_service(false, false, 0.1).await;
        let cascade = cascade_with_decision(
            Arc::new(FakeRule {
                lang: "zh",
                fail: true,
            }),
            endpoint,
        );
        let llm_endpoint = mock_llm_service().await;
        let llm = llm_engine(&llm_endpoint);
        let outcome = cascade
            .detect_level("他昨天报复很大。", 3, false, Some(&llm))
            .await
            .unwrap();
        assert_eq!(outcome.errors.len(), 1);
        assert_eq!(outcome.errors[0].source.as_deref(), Some("llm"));
    }

    #[tokio::test]
    async fn cascade_level3_triggers_llm_even_with_errors() {
        let endpoint = mock_decision_service(false, true, 0.9).await;
        let cascade = cascade_with_decision(
            Arc::new(FakeRule {
                lang: "zh",
                fail: false,
            }),
            endpoint,
        );
        let llm_endpoint = mock_llm_service().await;
        let llm = llm_engine(&llm_endpoint);
        let outcome = cascade
            .detect_level("他昨天报复很大。", 3, false, Some(&llm))
            .await
            .unwrap();
        assert_eq!(
            outcome.errors.len(),
            2,
            "规则 [0,2] + LLM 兜底 [9,15]（决策句级 [0,16] 与规则重叠已合并）: {:?}",
            outcome.errors
        );
        assert!(
            outcome
                .errors
                .iter()
                .any(|e| e.source.as_deref() == Some("llm")),
            "level 3 应无条件触发 LLM 兜底: {:?}",
            outcome.errors
        );
        assert!(!outcome.decision_abstained);
        // level 3 未配置 LLM → LlmNotConfigured
        let err = cascade
            .detect_level("他昨天报复很大。", 3, false, None)
            .await
            .unwrap_err();
        assert!(matches!(err, DetectorError::LlmNotConfigured(_)));
    }

    #[tokio::test]
    async fn cascade_invalid_level_rejected() {
        let cascade = cascade_with_decision(
            Arc::new(FakeRule {
                lang: "zh",
                fail: false,
            }),
            "http://127.0.0.1:1".into(),
        );
        for level in [0u8, 4, 99] {
            let err = cascade
                .detect_level("x", level, false, None)
                .await
                .unwrap_err();
            assert!(
                matches!(err, DetectorError::DetectionFailed(_)),
                "level {level} 应被拒绝，实际: {err:?}"
            );
        }
    }

    /// 默认 detect_level 实现（无决策层的引擎）：level 3 无条件触发 LLM
    #[tokio::test]
    async fn default_detect_level_triggers_llm_when_empty() {
        struct EmptyRule;
        #[async_trait]
        impl GECDetector for EmptyRule {
            fn language(&self) -> &str {
                "zh"
            }
            async fn detect(&self, _text: &str) -> Result<Vec<Error>, DetectorError> {
                Ok(Vec::new())
            }
        }
        let engine: Arc<dyn GecEngine> = Arc::new(EmptyRule);
        let llm_endpoint = mock_llm_service().await;
        let llm = llm_engine(&llm_endpoint);
        let outcome = engine
            .detect_level("正确句子", 3, false, Some(&llm))
            .await
            .unwrap();
        assert_eq!(outcome.errors.len(), 1);
        assert_eq!(outcome.errors[0].source.as_deref(), Some("llm"));
        // level 2 无弃权信号 → 等价 1 级（不触发 LLM）
        let outcome = engine
            .detect_level("正确句子", 2, false, Some(&llm))
            .await
            .unwrap();
        assert!(outcome.errors.is_empty());
        assert!(!outcome.decision_abstained);
        // level 3 未配置 LLM → LlmNotConfigured
        let err = engine
            .detect_level("正确句子", 3, false, None)
            .await
            .unwrap_err();
        assert!(matches!(err, DetectorError::LlmNotConfigured(_)));
    }
    // ---------- L3 特征模型（三期：LLM 收缩到纯长尾） ----------

    /// 本地 mock 特征模型服务：按参数返回弃权 / 无错 / span 级错误
    async fn mock_l3_service(
        abstain: bool,
        has_error: bool,
        prob: f64,
        span_error: bool,
    ) -> String {
        let app = axum::Router::new().route(
            "/l3-detect",
            axum::routing::post(move |body: axum::body::Bytes| async move {
                let req: serde_json::Value = serde_json::from_slice(&body).unwrap();
                let _ = req["text"].as_str().unwrap_or("");
                let resp = serde_json::json!({
                    "has_error": has_error,
                    "has_error_prob": prob,
                    "error_type": if has_error { "collocation" } else { "none" },
                    "abstain": abstain,
                    "errors": if span_error {
                        vec![serde_json::json!({
                            "error_type": "collocation",
                            "confidence": 0.9,
                            "span": [0, 6],
                            "original": "报复",
                            "correction": "抱负",
                            "message": "特征模型检出同音错别字"
                        })]
                    } else { Vec::new() }
                });
                (axum::http::StatusCode::OK, axum::Json(resp)).into_response()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        format!("http://{addr}")
    }

    /// mock L3 服务（部分定案）：abstain=false + unreviewed_spans（字符偏移）
    async fn mock_l3_partial_service(unreviewed: Vec<[usize; 2]>) -> String {
        let app = axum::Router::new().route(
            "/l3-detect",
            axum::routing::post(move |body: axum::body::Bytes| async move {
                let req: serde_json::Value = serde_json::from_slice(&body).unwrap();
                let _ = req["text"].as_str().unwrap_or("");
                let resp = serde_json::json!({
                    "has_error": false,
                    "has_error_prob": 0.3,
                    "error_type": "none",
                    "abstain": false,
                    "errors": [],
                    "unreviewed_spans": unreviewed
                });
                (axum::http::StatusCode::OK, axum::Json(resp)).into_response()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        format!("http://{addr}")
    }

    /// mock LLM 服务（标记版）：仅当收到的 user 文本**不含**标记子串时返回
    /// `报复→抱负`；含标记（说明送审了不该送的全文）则返回 `天气→气候`，
    /// 测试据此断言 LLM 只收到了灰区摘录而非全文
    async fn mock_llm_service_marker(marker: &'static str) -> String {
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(move |body: axum::body::Bytes| async move {
                let req: serde_json::Value = serde_json::from_slice(&body).unwrap();
                let user = req["messages"][1]["content"].as_str().unwrap_or("");
                let content = if user.contains(marker) {
                    "[{\"error_type\":\"grammar\",\"confidence\":0.85,\"original\":\"天气\",\"correction\":\"气候\",\"message\":\"全文泄漏标记\"}]"
                } else {
                    "[{\"error_type\":\"grammar\",\"confidence\":0.85,\"original\":\"报复\",\"correction\":\"抱负\",\"message\":\"定向复查检出\"}]"
                };
                let resp = serde_json::json!({
                    "choices": [{ "message": { "content": content } }]
                });
                (axum::http::StatusCode::OK, axum::Json(resp)).into_response()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        format!("http://{addr}/v1")
    }

    fn cascade_with_l3(
        rule: Arc<FakeRule>,
        l3_endpoint: String,
        decision_endpoint: String,
    ) -> CascadeDetector {
        let secondary = Arc::new(DecisionGecEngine::new(
            "zh",
            DecisionConfig {
                endpoint: decision_endpoint,
                ..Default::default()
            },
        ));
        let l3_engine = Arc::new(L3FeatureEngine::new(
            "zh",
            L3FeatureConfig {
                endpoint: l3_endpoint,
                ..Default::default()
            },
        ));
        let mut det = CascadeDetector::new("zh", rule, Some(secondary));
        det.l3 = Some(l3_engine);
        det
    }

    /// L3 特征模型高置信命中 → 直接定案，不调用 LLM
    #[tokio::test]
    async fn cascade_level3_l3_hit_skips_llm() {
        let endpoint = mock_decision_service(true, false, 0.3).await;
        let l3ep = mock_l3_service(false, true, 0.9, true).await;
        let cascade = cascade_with_l3(
            Arc::new(FakeRule {
                lang: "zh",
                fail: false,
            }),
            l3ep,
            endpoint,
        );
        let llm = llm_engine(&mock_llm_service().await);
        let outcome = cascade
            .detect_level("他昨天报复很大。", 3, false, Some(&llm))
            .await
            .unwrap();
        assert_eq!(
            outcome.errors.len(),
            1,
            "L3 错误 [0,6] 与规则 [0,2] 重叠 → 合并为 1 条: {:?}",
            outcome.errors
        );
        assert!(
            outcome
                .errors
                .iter()
                .all(|e| e.source.as_deref() != Some("llm")),
            "L3 高置信命中不应调用 LLM: {:?}",
            outcome.errors
        );
    }

    /// L3 特征模型明确无错 + 决策模型也明确（非弃权）→ 放行，不调用 LLM
    #[tokio::test]
    async fn cascade_level3_l3_confident_clean_skips_llm() {
        let endpoint = mock_decision_service(false, false, 0.1).await;
        let l3ep = mock_l3_service(false, false, 0.05, false).await;
        let cascade = cascade_with_l3(
            Arc::new(FakeRule {
                lang: "zh",
                fail: false,
            }),
            l3ep,
            endpoint,
        );
        let llm = llm_engine(&mock_llm_service().await);
        let outcome = cascade
            .detect_level("他昨天报复很大。", 3, false, Some(&llm))
            .await
            .unwrap();
        assert!(
            outcome
                .errors
                .iter()
                .all(|e| e.source.as_deref() != Some("llm")),
            "特征模型明确无错且决策模型一致时不应调用 LLM: {:?}",
            outcome.errors
        );
    }

    /// L3 判「明确无错」但决策模型弃权 → 两个模型冲突：不静默覆盖，保留 LLM 长尾兜底
    #[tokio::test]
    async fn cascade_level3_l3_clean_but_decision_abstain_triggers_llm() {
        let endpoint = mock_decision_service(true, false, 0.3).await;
        let l3ep = mock_l3_service(false, false, 0.05, false).await;
        let cascade = cascade_with_l3(
            Arc::new(FakeRule {
                lang: "zh",
                fail: false,
            }),
            l3ep,
            endpoint,
        );
        let llm = llm_engine(&mock_llm_service().await);
        let outcome = cascade
            .detect_level("他昨天报复很大。", 3, false, Some(&llm))
            .await
            .unwrap();
        assert!(
            outcome
                .errors
                .iter()
                .any(|e| e.source.as_deref() == Some("llm")),
            "L3 明确无错 + 决策弃权 = 模型冲突，应保留 LLM 长尾: {:?}",
            outcome.errors
        );
    }
    /// L3 特征模型弃权 → 触发 LLM 长尾兜底
    #[tokio::test]
    async fn cascade_level3_l3_abstain_triggers_llm() {
        let endpoint = mock_decision_service(true, false, 0.3).await;
        let l3ep = mock_l3_service(true, false, 0.4, false).await;
        let cascade = cascade_with_l3(
            Arc::new(FakeRule {
                lang: "zh",
                fail: false,
            }),
            l3ep,
            endpoint,
        );
        let llm = llm_engine(&mock_llm_service().await);
        let outcome = cascade
            .detect_level("他昨天报复很大。", 3, false, Some(&llm))
            .await
            .unwrap();
        assert!(
            outcome
                .errors
                .iter()
                .any(|e| e.source.as_deref() == Some("llm")),
            "L3 弃权应触发 LLM 长尾: {:?}",
            outcome.errors
        );
    }

    /// L3 服务宕机 → 回退 LLM 兜底（保持 level 3 契约）
    #[tokio::test]
    async fn cascade_level3_l3_down_falls_back_to_llm() {
        let endpoint = mock_decision_service(true, false, 0.3).await;
        let cascade = cascade_with_l3(
            Arc::new(FakeRule {
                lang: "zh",
                fail: false,
            }),
            "http://127.0.0.1:1".into(),
            endpoint,
        );
        let llm = llm_engine(&mock_llm_service().await);
        let outcome = cascade
            .detect_level("他昨天报复很大。", 3, false, Some(&llm))
            .await
            .unwrap();
        assert!(
            outcome
                .errors
                .iter()
                .any(|e| e.source.as_deref() == Some("llm")),
            "L3 服务宕机应回退 LLM: {:?}",
            outcome.errors
        );
    }

    // ---------- level 2 + L3 门控（P0.5 重绑：L3 是唯一的 pre-LLM 门控） ----------

    /// level 2 + L3 高置信「明确无错」+ 决策模型一致 → 直接定案，不调用 LLM
    #[tokio::test]
    async fn cascade_level2_l3_confident_clean_skips_llm() {
        let endpoint = mock_decision_service(false, false, 0.1).await;
        let l3ep = mock_l3_service(false, false, 0.05, false).await;
        let cascade = cascade_with_l3(
            Arc::new(FakeRule {
                lang: "zh",
                fail: false,
            }),
            l3ep,
            endpoint,
        );
        let llm = llm_engine(&mock_llm_service().await);
        let outcome = cascade
            .detect_level("他昨天报复很大。", 2, false, Some(&llm))
            .await
            .unwrap();
        assert!(
            outcome
                .errors
                .iter()
                .all(|e| e.source.as_deref() != Some("llm")),
            "L3 高置信无错 + level 2 不应调用 LLM: {:?}",
            outcome.errors
        );
        assert!(
            outcome.timings.l3_ms.is_some(),
            "level 2 应并行运行 L3 门控（计时可见）"
        );
    }

    /// level 2 + L3 弃权 → 触发 LLM 长尾兜底（P0.5：L3 是唯一的 pre-LLM 门控）
    #[tokio::test]
    async fn cascade_level2_l3_abstain_triggers_llm() {
        let endpoint = mock_decision_service(false, false, 0.1).await;
        let l3ep = mock_l3_service(true, false, 0.4, false).await;
        let cascade = cascade_with_l3(
            Arc::new(FakeRule {
                lang: "zh",
                fail: false,
            }),
            l3ep,
            endpoint,
        );
        let llm = llm_engine(&mock_llm_service().await);
        let outcome = cascade
            .detect_level("他昨天报复很大。", 2, false, Some(&llm))
            .await
            .unwrap();
        assert!(
            outcome
                .errors
                .iter()
                .any(|e| e.source.as_deref() == Some("llm")),
            "L3 弃权 + level 2 应触发 LLM 长尾: {:?}",
            outcome.errors
        );
    }

    /// level 2 + L3 服务宕机 → 门控不可用 = 回退 LLM 兜底
    #[tokio::test]
    async fn cascade_level2_l3_down_falls_back_to_llm() {
        let endpoint = mock_decision_service(false, false, 0.1).await;
        let cascade = cascade_with_l3(
            Arc::new(FakeRule {
                lang: "zh",
                fail: false,
            }),
            "http://127.0.0.1:1".into(),
            endpoint,
        );
        let llm = llm_engine(&mock_llm_service().await);
        let outcome = cascade
            .detect_level("他昨天报复很大。", 2, false, Some(&llm))
            .await
            .unwrap();
        assert!(
            outcome
                .errors
                .iter()
                .any(|e| e.source.as_deref() == Some("llm")),
            "L3 服务宕机 + level 2 应回退 LLM: {:?}",
            outcome.errors
        );
    }

    // ---------- level 2 + L3 部分定案 → 定向 LLM 复查（A 优化） ----------

    /// L3 部分定案（灰区句 span）→ 只把灰区句送 LLM（全文其余部分不送审），
    /// LLM 检出的错误 span 映射回原文坐标
    #[tokio::test]
    async fn cascade_level2_l3_partial_triggers_targeted_llm() {
        let endpoint = mock_decision_service(false, false, 0.1).await;
        // 灰区句 = 第一句（字符 [0,8)）
        let l3ep = mock_l3_partial_service(vec![[0, 8]]).await;
        let cascade = cascade_with_l3(
            Arc::new(FakeRule {
                lang: "zh",
                fail: false,
            }),
            l3ep,
            endpoint,
        );
        // 标记 = 第二句：LLM 若收到全文（含第二句）会返回「天气」错误
        let llm = llm_engine(&mock_llm_service_marker("今天天气很好。").await);
        let outcome = cascade
            .detect_level("他昨天报复很大。今天天气很好。", 2, false, Some(&llm))
            .await
            .unwrap();
        assert!(
            outcome.errors.iter().all(|e| e.original != "天气"),
            "LLM 不应收到全文（第二句不在灰区）: {:?}",
            outcome.errors
        );
        let hit = outcome
            .errors
            .iter()
            .find(|e| e.source.as_deref() == Some("llm"))
            .expect("定向复查应产出 LLM 错误");
        assert_eq!(hit.original, "报复");
        // 「报复」在原文字符 [3,5) → 字节 [9,15)
        assert_eq!(hit.span, [9, 15]);
        assert!(
            outcome.timings.llm_ms.is_some(),
            "定向复查应计入 llm_ms 计时"
        );
    }

    /// 定向复查的 span 映射：灰区句不在文本开头时，偏移正确平移回原文
    #[tokio::test]
    async fn targeted_llm_review_maps_later_segment() {
        let llm = llm_engine(&mock_llm_service().await);
        // 第一句「今天天气很好。」7 字 21 字节；第二句字节 [21,45)
        let text = "今天天气很好。他昨天报复很大。";
        let mapped = targeted_llm_review(&llm, text, &[[21, 45]]).await.unwrap();
        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0].original, "报复");
        // 「报复」在原文字符 [10,12) → 字节 [30,36)
        assert_eq!(mapped[0].span, [30, 36]);
    }

    /// 决策模型弃权 + L3 部分定案 → 全文 LLM（覆盖灰区，不重复定向调用）
    #[tokio::test]
    async fn cascade_level2_decision_abstain_partial_uses_fulltext_llm() {
        let endpoint = mock_decision_service(true, false, 0.3).await;
        let l3ep = mock_l3_partial_service(vec![[0, 8]]).await;
        let cascade = cascade_with_l3(
            Arc::new(FakeRule {
                lang: "zh",
                fail: false,
            }),
            l3ep,
            endpoint,
        );
        // 全文送审 → 标记命中 → LLM 返回「天气」错误
        let llm = llm_engine(&mock_llm_service_marker("今天天气很好。").await);
        let outcome = cascade
            .detect_level("他昨天报复很大。今天天气很好。", 2, false, Some(&llm))
            .await
            .unwrap();
        assert!(
            outcome.errors.iter().any(|e| e.original == "天气"),
            "决策弃权应触发全文 LLM（而非仅定向复查）: {:?}",
            outcome.errors
        );
    }

    /// level 2 不跑特征模型（L3 专属层）：即使 L3 会命中也不产出 feature 错误
    #[tokio::test]
    async fn cascade_level2_ignores_l3() {
        let endpoint = mock_decision_service(true, false, 0.3).await;
        let l3ep = mock_l3_service(false, true, 0.9, true).await;
        let cascade = cascade_with_l3(
            Arc::new(FakeRule {
                lang: "zh",
                fail: false,
            }),
            l3ep,
            endpoint,
        );
        let llm = llm_engine(&mock_llm_service().await);
        let outcome = cascade
            .detect_level("他昨天报复很大。", 2, false, Some(&llm))
            .await
            .unwrap();
        assert!(
            outcome
                .errors
                .iter()
                .all(|e| e.source.as_deref() != Some("feature")),
            "level 2 不应运行特征模型: {:?}",
            outcome.errors
        );
    }

    /// force_llm 绕过特征模型（无条件 LLM，「漏报」快捷操作语义不变）
    #[tokio::test]
    async fn cascade_level3_force_llm_bypasses_l3() {
        let endpoint = mock_decision_service(false, true, 0.9).await;
        let l3ep = mock_l3_service(false, true, 0.9, true).await;
        let cascade = cascade_with_l3(
            Arc::new(FakeRule {
                lang: "zh",
                fail: false,
            }),
            l3ep,
            endpoint,
        );
        let llm = llm_engine(&mock_llm_service().await);
        let outcome = cascade
            .detect_level("他昨天报复很大。", 3, true, Some(&llm))
            .await
            .unwrap();
        assert!(
            outcome
                .errors
                .iter()
                .all(|e| e.source.as_deref() != Some("feature")),
            "force_llm 应绕过特征模型: {:?}",
            outcome.errors
        );
        assert!(
            outcome
                .errors
                .iter()
                .any(|e| e.source.as_deref() == Some("llm")),
            "force_llm 应触发 LLM: {:?}",
            outcome.errors
        );
    }

    // ---------- 各阶段计时（DetectTimings） ----------

    /// 模拟依赖 NLP 服务的规则检测器：每次 detect 记录一次 NLP 调用
    /// （首次 = 冷启动预热，成功调用后视为预热完成）
    struct FakeNlpRule {
        warmed: std::sync::atomic::AtomicBool,
        last: std::sync::Mutex<Option<NlpCall>>,
    }

    #[async_trait]
    impl GECDetector for FakeNlpRule {
        fn language(&self) -> &str {
            "en"
        }
        async fn detect(&self, text: &str) -> Result<Vec<Error>, DetectorError> {
            let ms: u64 = 42;
            let cold_start = !self.warmed.swap(true, std::sync::atomic::Ordering::Relaxed);
            *self.last.lock().unwrap() = Some(NlpCall { ms, cold_start });
            Ok(vec![Error::new(
                "en",
                "spelling",
                0.9,
                [0, 2],
                text,
                Some("fix".into()),
                "nlp rule",
            )])
        }
        fn last_nlp_call(&self) -> Option<NlpCall> {
            *self.last.lock().unwrap()
        }
    }

    /// 级联检测器：首次 NLP 调用 → nlp_warmup_ms 非空（含冷启动预热）；
    /// 预热完成后 → nlp_warmup_ms 为空；各阶段计时字段与执行层一一对应
    #[tokio::test]
    async fn cascade_detect_level_reports_nlp_warmup_timings() {
        let rule: Arc<dyn GecEngine> = Arc::new(FakeNlpRule {
            warmed: std::sync::atomic::AtomicBool::new(false),
            last: std::sync::Mutex::new(None),
        });
        let det = CascadeDetector::new("en", rule, None);
        // 首次调用：NLP 冷启动（含模型加载预热）
        let first = det.detect_level("hello", 1, false, None).await.unwrap();
        assert_eq!(first.timings.nlp_ms, Some(42), "NLP 调用耗时应被记录");
        assert_eq!(
            first.timings.nlp_warmup_ms,
            Some(42),
            "首次 NLP 调用应标记为冷启动预热"
        );
        assert!(first.timings.decision_ms.is_none(), "决策层未启用");
        assert!(first.timings.l3_ms.is_none());
        assert!(first.timings.recall_ms.is_none());
        assert!(first.timings.llm_ms.is_none(), "level 1 不触发 LLM");
        // 第二次调用：已预热 → nlp_warmup_ms 为空
        let second = det.detect_level("hello", 1, false, None).await.unwrap();
        assert_eq!(second.timings.nlp_ms, Some(42));
        assert!(
            second.timings.nlp_warmup_ms.is_none(),
            "预热完成后 nlp_warmup_ms 应为空"
        );
    }

    /// 默认 detect_level 实现（无决策层引擎）：填充规则层 + LLM + 总耗时；
    /// level 3 未配置 LLM → LlmNotConfigured
    #[tokio::test]
    async fn default_detect_level_reports_timings() {
        let det: Arc<dyn GecEngine> = Arc::new(FakeRule {
            lang: "en",
            fail: false,
        });
        let outcome = det.detect_level("hello", 1, false, None).await.unwrap();
        assert!(
            outcome.timings.rule_ms <= outcome.timings.total_ms,
            "规则层耗时应计入总耗时"
        );
        assert!(outcome.timings.nlp_ms.is_none(), "纯本地检测无 NLP 调用");
        assert!(outcome.timings.decision_ms.is_none());
        assert!(outcome.timings.llm_ms.is_none());
        // level 3 无条件 LLM：未配置 → LlmNotConfigured
        let err = det.detect_level("hello", 3, false, None).await.unwrap_err();
        assert!(matches!(err, DetectorError::LlmNotConfigured(_)));
    }

    /// level 3 + L3 弃权 + LLM 兜底：l3_ms / llm_ms 均被记录
    #[tokio::test]
    async fn cascade_level3_reports_l3_and_llm_timings() {
        let endpoint = mock_decision_service(false, true, 0.9).await;
        // L3 弃权（abstain=true）→ 保持 need_llm → 触发 LLM 长尾兜底
        let l3ep = mock_l3_service(true, false, 0.3, false).await;
        let cascade = cascade_with_l3(
            Arc::new(FakeRule {
                lang: "zh",
                fail: false,
            }),
            l3ep,
            endpoint,
        );
        let llm = llm_engine(&mock_llm_service().await);
        let outcome = cascade
            .detect_level("他昨天报复很大。", 3, false, Some(&llm))
            .await
            .unwrap();
        assert!(
            outcome.timings.l3_ms.is_some(),
            "level 3 应运行 L3 特征模型"
        );
        assert!(outcome.timings.llm_ms.is_some(), "L3 弃权路径应触发 LLM");
        assert!(outcome.timings.decision_ms.is_some(), "决策层已启用");
        assert!(
            outcome.timings.decision_ms.unwrap() <= outcome.timings.total_ms,
            "决策层耗时应计入总耗时"
        );
    }
}
