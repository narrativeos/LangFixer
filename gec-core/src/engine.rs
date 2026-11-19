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
//!   弃权 / 服务不可用 → LLM 兜底；L3 **显式关闭** → **无条件** LLM 兜底，
//!   与 l3.rs「回退无条件 LLM」口径一致；从未配置 L3 时回退旧语义：决策模型弃权触发）；
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
    /// 本次调用的**运行态输出通道**（C4 修复）：预算块/窗执行摘要与所有权空洞
    /// 写入调用方自带的 [`RunReport`](crate::quality::RunReport)，经调用栈携带而非
    /// 存入引擎共享槽——并发请求各用各的，质量/缓存核算不再互染。
    /// 默认实现委托 [`detect`](Self::detect)（无预算块循环的引擎不写入）；有切块的
    /// 引擎（stat 滑窗）覆写本方法。
    async fn detect_run(
        &self,
        text: &str,
        _run: &mut crate::quality::RunReport,
    ) -> Result<Vec<Error>, DetectorError> {
        self.detect(text).await
    }
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
    /// 带可选段级事件接收器与运行态输出的底层检测：默认实现委托
    /// [`detect_run`](Self::detect_run)（无预算块循环的引擎零事件）；执行循环内
    /// 有切块的引擎（stat 滑窗）覆写本方法逐块推
    /// [`crate::progress::StageEvent::ChunkBegin`] /
    /// [`crate::progress::StageEvent::ChunkEnd`]。级联的 rule 分支经本方法注入 sink，
    /// 使 stat 臂的窗进度同样上屏。
    async fn detect_traced(
        &self,
        text: &str,
        _sink: Option<crate::progress::StageSink>,
        run: &mut crate::quality::RunReport,
    ) -> Result<Vec<Error>, DetectorError> {
        self.detect_run(text, run).await
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
        // ① 规则检测（计时含 NLP 服务调用；NLP 调用信息事后从引擎读取）；
        // 预算窗执行摘要经 RunReport 随调用栈携带（C4：不落引擎共享槽）
        let mut run = crate::quality::RunReport::default();
        let t_rule = std::time::Instant::now();
        let mut errors = self.detect_traced(text, None, &mut run).await?;
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
        let mut llm_chunk: Option<crate::quality::ChunkStats> = None;
        let mut llm_ran = false;
        // ⑦ dropped 账本（§7）：本路径来源只有 LLM 层；out_of_range 恒 0 预留
        let mut dropped = crate::quality::DroppedLedger::default();
        if force_llm || level == 3 {
            let llm = llm.ok_or_else(|| {
                DetectorError::LlmNotConfigured(
                    "LLM 兜底检测未配置（请在设置界面配置 LLM 参数，或设置 LLM_API_KEY）"
                        .to_string(),
                )
            })?;
            let t_llm = std::time::Instant::now();
            // 长文本自动走分块并行路径（llm_layer_detect）；解析失败记 `partial`，
            // 不当作「复查确认干净」。运行时失败（超时/排队耗尽/熔断开断）→ 降级：
            // 保留规则层结果，层记 `unavailable`（与定向复查、L3 失败同一先例），
            // 不把整次 detect 打回 502。未配置 LLM 仍走上面的 LlmNotConfigured——
            // 那是要人改设置，不是运行时故障。
            match llm_layer_detect(llm, text, None).await {
                Ok((extra, llm_status, clean, chunk, ledger)) => {
                    llm_ms = Some(t_llm.elapsed().as_millis() as u64);
                    llm_confirmed_clean = clean;
                    llm_ran = true;
                    llm_chunk = Some(chunk);
                    dropped.merge(&ledger);
                    layers.push(LayerReport::new(
                        shared_model::sources::LLM,
                        llm_status,
                        extra.len(),
                    ));
                    merge_errors(&mut errors, extra);
                }
                Err(e) => {
                    llm_ms = Some(t_llm.elapsed().as_millis() as u64);
                    layers.push(LayerReport::new(
                        shared_model::sources::LLM,
                        "unavailable",
                        0,
                    ));
                    tracing::warn!(error = %e, "LLM 兜底失败：保留规则层结果（本轮 LLM 未参与）");
                }
            }
        }
        // S6 质量自描述（Detect Pipeline v2 §5）：段级预算（本层 LLM + 成员引擎的
        // 预算窗）进 chunks；layers → 降级项；句级单层引擎不核算
        let mut chunk_stats: Option<crate::quality::ChunkStats> = run.chunks.take();
        if let Some(c) = &llm_chunk {
            chunk_stats = Some(match chunk_stats {
                Some(mut a) => {
                    a.merge(c);
                    a
                }
                None => c.clone(),
            });
        }
        let chunks = match chunk_stats {
            Some(c) => c,
            None if (force_llm || level == 3) && !llm_ran => crate::quality::ChunkStats {
                mode: "single".into(),
                total: 1,
                done: 0,
                failed: 1,
                parse_failed: 0,
            },
            None => crate::quality::ChunkStats::single(),
        };
        let mut quality =
            crate::quality::QualityReport::assemble(&layers, chunks.coverage(), run.unowned_chars);
        quality.dropped = dropped;
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
            quality: Some(quality),
            chunks: Some(chunks),
            sentences: None,
        })
    }
    /// 与 [`detect_level`](Self::detect_level) 相同，但附带可选的阶段事件接收器
    /// （[`crate::progress::StageSink`]）：支持流式推送的引擎（级联检测器）在
    /// 每个阶段结束点推送 [`crate::progress::StageEvent`]，供前端分阶段进度条实时渲染。
    /// 默认实现委托 `detect_level`（单层引擎不产生阶段内事件）。
    async fn detect_level_traced(
        &self,
        text: &str,
        level: u8,
        force_llm: bool,
        llm: Option<&LlmGecEngine>,
        _progress: Option<crate::progress::StageSink>,
    ) -> Result<DetectOutcome, DetectorError> {
        self.detect_level(text, level, force_llm, llm).await
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
    /// 质量自描述（Detect Pipeline v2）：`None` = 未实际检测（缓存命中 / 历史回放）；
    /// **仅 `status == "full"` 的运行可写 LRU / 历史**（调用方在写入侧判定）
    pub quality: Option<crate::quality::QualityReport>,
    /// 段级（预算块）执行摘要；`None` = 未实际检测
    pub chunks: Option<crate::quality::ChunkStats>,
    /// 句级（结构切分）执行摘要；`None` = 未实际检测 / 单层引擎未做句级核算
    pub sentences: Option<crate::quality::SentenceStats>,
}

/// 层级贡献报告：本次检测实际参与的每一层各一条
/// （`rule` / `decision` / `knowledge` / `feature` / `llm` / `statistical`）
///
/// `status` 取值：
/// - `ran`：已执行且结果参与合并；
/// - `decided`：特征模型高置信定案（LLM 收缩未触发）；
/// - `abstained`：模型明确弃权（长尾交由 LLM 兜底）；
/// - `unavailable`：该层服务调用失败（按策略回退，不阻断检测）；
/// - `partial`：该层**部分完成**——响应解析失败（全文单发）或部分块失败/
///   解析失败（文档级分块复查）；此时该层的空结果**不得**解读为
///   「复查确认干净」（`llm_confirmed_clean=false`）；
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
///
/// 返回（错误列表，dropped 账本）：账本桶 `pii_mask`（掩码区检出丢弃）、
/// `ownership`（落在任何灰区段外的丢弃）；§7 完整性告警数据源。
pub async fn targeted_llm_review(
    llm: &LlmGecEngine,
    text: &str,
    spans: &[[usize; 2]],
) -> Result<(Vec<Error>, crate::quality::DroppedLedger), DetectorError> {
    // ① 越界钓制 + 排序 + 合并重叠/相邻 span（钓制对齐字符边界，摘录切片不得 panic）
    let mut sp: Vec<[usize; 2]> = spans
        .iter()
        .map(|s| {
            let mut a = s[0].min(text.len());
            let mut b = s[1].min(text.len());
            while a > 0 && !text.is_char_boundary(a) {
                a -= 1;
            }
            while b < text.len() && !text.is_char_boundary(b) {
                b += 1;
            }
            [a, b]
        })
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
        return Ok((Vec::new(), crate::quality::DroppedLedger::default()));
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
    // ②.5 PII 掩码：摘录送 LLM 前等长掩码（span 空间不变），
    // 与被掩区域重叠的检出丢弃（`*` 区不可能产出有效检出）
    let mask = crate::mask::maybe_mask(&excerpt);
    let excerpt = match &mask {
        Some(m) => m.text.clone(),
        None => excerpt,
    };
    let mut found = llm.detect(&excerpt).await?;
    let mut dropped = crate::quality::DroppedLedger::default();
    if let Some(m) = &mask {
        dropped.pii_mask = crate::mask::filter_masked_errors(&mut found, m);
    }
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
            dropped.ownership += 1;
            continue;
        };
        let shift = os - gs;
        err.span = [
            err.span[0].clamp(gs, ge) + shift,
            err.span[1].clamp(gs, ge) + shift,
        ];
        out.push(err);
    }
    Ok((out, dropped))
}

/// 全文 LLM 层统一执行入口：短文本单发、长文本**分块并行复查**
/// （文档级架构 P1：10 万字+ 不再可能一次调用，且长文单发召回塌陷）。
///
/// 阈值与并发（环境变量，运行时设置后续接入）：
/// - `GEC_LLM_CHUNK_CHARS`：走分块路径的字符阈值（默认 12000；0 = 恒单发）；
/// - `GEC_LLM_CHUNK_TARGET`：目标块大小字符数（默认 1500）；
/// - `GEC_LLM_CONCURRENCY`：块并发（默认 8）。
///
/// 返回（错误列表，层状态，`llm_confirmed_clean`，段级执行摘要，dropped 账本）：
/// - 解析失败（单发）或块失败/解析失败（分块）→ 层状态 `partial`，
///   且 `confirmed_clean` 必为 `false`——**解析失败不得伪装成「无错误」**；
/// - 分块路径全部块失败 → [`Err`]（与单发网络失败同语义，由调用方降级）；
/// - 第四位 [`ChunkStats`] 供调用方织入 quality.coverage / chunks（Detect Pipeline v2）；
/// - 第五位 [`DroppedLedger`](crate::quality::DroppedLedger)：`pii_mask`（掩码区检出）
///   与 `out_of_scope`（分块前缀区双报）；`out_of_range` 恒 0 预留（§7）。
pub async fn llm_layer_detect(
    llm: &LlmGecEngine,
    text: &str,
    progress: Option<crate::progress::StageSink>,
) -> Result<
    (
        Vec<Error>,
        &'static str,
        bool,
        crate::quality::ChunkStats,
        crate::quality::DroppedLedger,
    ),
    DetectorError,
> {
    // 隐私防线：送 LLM（可能为外部端点）前掩码 PII（手机号 / 身份证 / 固话 /
    // 长数字串 / 邮箱本地部分；等长替换，span 字节偏移空间不变）；
    // 与被掩区域重叠的 LLM 检出丢弃。GEC_LLM_MASK_PII=0 可关
    let mask = crate::mask::maybe_mask(text);
    let text: &str = mask.as_ref().map(|m| m.text.as_str()).unwrap_or(text);
    let threshold = env_usize("GEC_LLM_CHUNK_CHARS", 12000);
    if threshold == 0 || text.chars().count() < threshold {
        let (mut errors, parse_failed) = llm.detect_tracked(text).await?;
        let mut dropped = crate::quality::DroppedLedger::default();
        if let Some(m) = &mask {
            dropped.pii_mask = crate::mask::filter_masked_errors(&mut errors, m);
        }
        let clean = errors.is_empty() && !parse_failed;
        let stats = crate::quality::ChunkStats {
            mode: "single".into(),
            total: 1,
            done: if parse_failed { 0 } else { 1 },
            failed: 0,
            parse_failed: parse_failed as usize,
        };
        Ok((
            errors,
            if parse_failed { "partial" } else { "ran" },
            clean,
            stats,
            dropped,
        ))
    } else {
        let target = env_usize("GEC_LLM_CHUNK_TARGET", crate::docchunk::DEFAULT_CHUNK_CHARS);
        let concurrency = env_usize("GEC_LLM_CONCURRENCY", 8);
        let mut o = llm
            .detect_document(text, target, concurrency, progress)
            .await?;
        let mut dropped = crate::quality::DroppedLedger {
            out_of_scope: o.prefix_dropped,
            ..Default::default()
        };
        if let Some(m) = &mask {
            dropped.pii_mask = crate::mask::filter_masked_errors(&mut o.errors, m);
        }
        let bad = o.failed_chunks + o.parse_failed_chunks;
        let clean = bad == 0 && o.errors.is_empty();
        let status = if bad == 0 {
            "ran"
        } else if bad >= o.chunks {
            "unavailable"
        } else {
            "partial"
        };
        tracing::info!(
            chunks = o.chunks,
            failed = o.failed_chunks,
            parse_failed = o.parse_failed_chunks,
            coverage = %o.coverage(),
            "文档级 LLM 复查完成（分块并行）"
        );
        let stats = crate::quality::ChunkStats {
            mode: "chunked".into(),
            total: o.chunks,
            done: o.chunks - bad.min(o.chunks),
            failed: o.failed_chunks,
            parse_failed: o.parse_failed_chunks,
        };
        Ok((o.errors, status, clean, stats, dropped))
    }
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(default)
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
    async fn detect_run(
        &self,
        text: &str,
        run: &mut crate::quality::RunReport,
    ) -> Result<Vec<Error>, DetectorError> {
        match self.primary.detect_run(text, run).await {
            Ok(mut errors) => {
                tag_source(&mut errors, self.primary.kind().as_str());
                if let Some(fb) = &self.fallback {
                    // 兜底引擎另用一份局部 RunReport：仅当主引擎无切块摘要时采用，
                    // 空洞不重叠双计（与历史转发语义 primary or fallback 一致）
                    let mut fb_run = crate::quality::RunReport::default();
                    match fb.detect_run(text, &mut fb_run).await {
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
                    run.unowned_chars += fb_run.unowned_chars;
                    if run.chunks.is_none() {
                        run.chunks = fb_run.chunks;
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
                        let mut errors = fb.detect_run(text, run).await?;
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
    /// L3 特征模型（三期门控层：level 2 先跑特征模型，LLM 收缩到纯长尾；
    /// level 3 定案仍合并但不终结全文 LLM（漏报兜底臂）；未启用时为 None）
    pub l3: Option<Arc<L3FeatureEngine>>,
    /// L3 **配置存在但显式关闭**（`enabled=false`）：level 2 门控视同「门控缺席」
    /// 开闸，回退无条件 LLM（[`crate::l3::L3FeatureConfig`] enabled 字段的文档口径）；
    /// 从未配置 L3 的部署保持旧语义（决策模型弃权门控）
    pub l3_disabled: bool,
    /// 术语库（可选；`lexicons/terminology_{lang}.json` 非空时挂载：
    /// 受保护术语上的错误过滤 + 强制更正词注入，作用于全部来源层合并后）
    pub terminology: Option<Arc<crate::terminology::Terminology>>,
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
            l3_disabled: false,
            terminology: None,
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
        self.detect_level_traced(text, level, force_llm, llm, None)
            .await
    }

    /// 级联主链路（同 `detect_level`，另把各阶段完成事件推给可选 `progress` sink）。
    /// 阶段计划 = 挂载层 + 级别；rule / decision / L3 并行段各自结束时推送，
    /// 召回与 LLM 门控在其后；未触发的 LLM 记 skipped，管线结束推 End（总耗时）。
    async fn detect_level_traced(
        &self,
        text: &str,
        level: u8,
        force_llm: bool,
        llm: Option<&LlmGecEngine>,
        progress: Option<crate::progress::StageSink>,
    ) -> Result<DetectOutcome, DetectorError> {
        if !(1..=3).contains(&level) {
            return Err(DetectorError::DetectionFailed(format!(
                "无效响应级别: {level}（应为 1-3）"
            )));
        }
        let started = std::time::Instant::now();
        // 阶段事件：开局声明本次参与的阶段计划（与 timings 各字段的挂载条件一致；
        // LLM 只要门控存在就入计划，实际未触发时在尾部补记 skipped）
        if let Some(sink) = &progress {
            let mut plan: Vec<&'static str> = vec![shared_model::sources::RULE];
            if self.decision.is_some() {
                plan.push(shared_model::sources::DECISION);
            }
            if self.l3.is_some() && level >= 2 && !force_llm {
                plan.push(shared_model::sources::FEATURE);
            }
            if self.knowledge_recall.is_some() {
                plan.push(shared_model::sources::KNOWLEDGE);
            }
            if force_llm || level >= 2 {
                plan.push(shared_model::sources::LLM);
            }
            sink(crate::progress::StageEvent::Start { stages: plan });
        }
        // 并行三分支各自持 sink 克隆（Arc 浅拷贝，分支结束时推送自身完成事件）
        let p_rule = progress.clone();
        let p_dec = progress.clone();
        let p_l3 = progress.clone();
        // ① 臂的本次调用运行态（窗摘要/空洞）：随调用栈携带，不落引擎共享槽（C4）
        let mut rule_run = crate::quality::RunReport::default();
        // ①＋②（＋③-F）规则 / 决策模型 / L3 特征模型**并行**执行：
        // 三者都是全文级判断、互不依赖对方结果，总延迟 = 各层 max
        // （此前为串行求和）；规则不可用时回退决策模型（保持既有故障回退行为，
        // 且决策服务只调一次——旧实现在回退路径会再调一次 `decide`）。
        // 各分支独立计时（wall time），用于 [`DetectTimings`]
        // 三者都是全文级判断、互不依赖对方结果，总延迟 = 各层 max
        // （此前为串行求和）；规则不可用时回退决策模型（保持既有故障回退行为，
        // 且决策服务只调一次——旧实现在回退路径会再调一次 `decide`）。
        // 各分支独立计时（wall time），用于 [`DetectTimings`]
        let ((rule_res, rule_ms), (dec_res, dec_ms), (l3_res, l3_ms)) = tokio::join!(
            async {
                let t = std::time::Instant::now();
                // sink 透传到 rule 成员执行循环（stat 臂滑窗逐块推段级事件；
                // 其余引擎经 trait 默认实现零事件）；窗执行摘要写入 rule_run
                let r = self
                    .rule
                    .detect_traced(text, p_rule.clone(), &mut rule_run)
                    .await;
                let ms = t.elapsed().as_millis() as u64;
                if let Some(sink) = &p_rule {
                    sink(crate::progress::StageEvent::Stage {
                        stage: shared_model::sources::RULE,
                        status: if r.is_ok() { "ran" } else { "unavailable" },
                        ms: Some(ms),
                    });
                }
                (r, ms)
            },
            async {
                match &self.decision {
                    Some(d) => {
                        let t = std::time::Instant::now();
                        let r = d.decide(text).await.map(Some);
                        let ms = t.elapsed().as_millis() as u64;
                        if let Some(sink) = &p_dec {
                            sink(crate::progress::StageEvent::Stage {
                                stage: shared_model::sources::DECISION,
                                status: if r.is_ok() { "ran" } else { "unavailable" },
                                ms: Some(ms),
                            });
                        }
                        (r, ms)
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
                            let ms = t.elapsed().as_millis() as u64;
                            if let Some(sink) = &p_l3 {
                                sink(crate::progress::StageEvent::Stage {
                                    stage: shared_model::sources::FEATURE,
                                    status: if r.is_ok() { "ran" } else { "unavailable" },
                                    ms: Some(ms),
                                });
                            }
                            (Some(r), ms)
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
            if let Some(sink) = &progress {
                sink(crate::progress::StageEvent::Stage {
                    stage: shared_model::sources::KNOWLEDGE,
                    status: "ran",
                    ms: recall_ms,
                });
            }
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
        // pre-LLM 门控——L3 弃权 / 服务不可用 → 开闸；L3 **显式关闭**（l3_disabled）
        // → 同样开闸回退无条件 LLM，这是 l3.rs enabled 字段的文档口径，防止门控层
        // 缺席叠加待发布决策层后 level 2 静默退化为纯规则；从未配置 L3 的旧式部署
        // 保持旧语义：决策模型弃权触发）/ level 3 无条件
        let l3_gate_open = match &l3_res {
            Some(Ok(o)) => o.abstained,
            Some(Err(_)) => true,
            None => self.l3_disabled,
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
        // **level 3 例外**：深度扫描的存在意义就是「漏报」兜底，句级门控
        // （dev R≈0.45）不得终结全文 LLM——定案仍合并，但 need_llm 保持为真。
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
                        } else if level <= 2 {
                            need_llm = false;
                        }
                    } else if level <= 2 {
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
        // 段级预算块执行摘要（③ LLM）与灰区句未复查标记（③-T 句级核算）
        let mut llm_chunk: Option<crate::quality::ChunkStats> = None;
        let mut gray_unresolved = false;
        // ⑦ dropped 账本（§7）：来③ 全文/分块 LLM ⊕ ③-T 定向复查；out_of_range 恒 0 预留
        let mut dropped = crate::quality::DroppedLedger::default();
        if need_llm {
            let llm = llm.ok_or_else(|| {
                DetectorError::LlmNotConfigured(
                    "LLM 兜底检测未配置（请在设置界面配置 LLM 参数，或设置 LLM_API_KEY）"
                        .to_string(),
                )
            })?;
            let t = std::time::Instant::now();
            // 长文本自动走分块并行路径（llm_layer_detect）；解析失败记 `partial`，
            // 不当作「复查确认干净」。运行时失败降级同默认实现：保留规则/L3 结果，
            // 层记 `unavailable`，不把整次 detect 打回 502。
            // 长文本分块路径的段级事件经同一 sink 透传（chunk_begin/chunk_end）
            match llm_layer_detect(llm, text, progress.clone()).await {
                Ok((extra, llm_status, clean, chunk, ledger)) => {
                    llm_ms = Some(t.elapsed().as_millis() as u64);
                    llm_confirmed_clean = clean;
                    llm_ran = true;
                    llm_chunk = Some(chunk);
                    dropped.merge(&ledger);
                    if let Some(sink) = &progress {
                        sink(crate::progress::StageEvent::Stage {
                            stage: shared_model::sources::LLM,
                            status: llm_status,
                            ms: llm_ms,
                        });
                    }
                    layers.push(LayerReport::new(sources::LLM, llm_status, extra.len()));
                    merge_errors(&mut errors, extra);
                }
                Err(e) => {
                    llm_ms = Some(t.elapsed().as_millis() as u64);
                    // R6：全文 LLM 未参与时，若 L3 部分定案留有灰区句且尚未
                    // 定向复查（③-T 不跑）——灰区句本轮同样未复查，句级
                    // 账本必须扣减，不得把待复查的灰区句计为已复查。
                    if !targeted_spans.is_empty() {
                        gray_unresolved = true;
                    }
                    if let Some(sink) = &progress {
                        sink(crate::progress::StageEvent::Stage {
                            stage: shared_model::sources::LLM,
                            status: "unavailable",
                            ms: llm_ms,
                        });
                    }
                    layers.push(LayerReport::new(sources::LLM, "unavailable", 0));
                    tracing::warn!(error = %e, "LLM 兜底失败：保留规则/L3 结果（本轮 LLM 未参与）");
                }
            }
        } else if !targeted_spans.is_empty() {
            // ③-T 定向 LLM 复查：L3 部分定案的灰区句单独送 LLM，
            // 返回 span 映射回原文后合并（全文其余部分已由 L3 定案，不再送 LLM）
            match llm {
                Some(l) => {
                    let t = std::time::Instant::now();
                    match targeted_llm_review(l, text, &targeted_spans).await {
                        Ok((extra, ledger)) => {
                            dropped.merge(&ledger);
                            llm_ms = Some(t.elapsed().as_millis() as u64);
                            llm_confirmed_clean = extra.is_empty();
                            llm_ran = true;
                            if let Some(sink) = &progress {
                                sink(crate::progress::StageEvent::Stage {
                                    stage: shared_model::sources::LLM,
                                    status: "ran",
                                    ms: llm_ms,
                                });
                            }
                            layers.push(LayerReport::new(sources::LLM, "ran", extra.len()));
                            merge_errors(&mut errors, extra);
                        }
                        Err(e) => {
                            gray_unresolved = true;
                            layers.push(LayerReport::new(sources::LLM, "unavailable", 0));
                            tracing::warn!(
                                error = %e,
                                "定向 LLM 复查失败：保留 L3 定案结果，灰区句本轮未复查"
                            );
                        }
                    }
                }
                None => {
                    gray_unresolved = true;
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
            if let Some(sink) = &progress {
                sink(crate::progress::StageEvent::Stage {
                    stage: shared_model::sources::LLM,
                    status: "skipped",
                    ms: None,
                });
            }
            layers.push(LayerReport::new(sources::LLM, "skipped", 0));
        }
        // ④ 术语库（最后一步，作用于全部来源层合并后的错误）：
        // 受保护术语上的错误过滤 + 强制更正词注入
        if let Some(term) = &self.terminology {
            term.apply(&self.lang, text, &mut errors);
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
        if let Some(sink) = &progress {
            sink(crate::progress::StageEvent::End {
                total_ms: timings.total_ms,
            });
        }
        // S6 质量自描述（Detect Pipeline v2 §5）：
        // - chunks：③ LLM 段级摘要 ⊕ 成员引擎预算窗摘要（stat 滑窗）；
        //   无任何预算层且未触发 LLM → 全文单发；触发但未成功 → 单发未复查；
        // - quality：layers 非 ran 项 → 降级项；coverage 由 chunks 核算；
        //   unowned_chars：stat 臂窗切块空洞经 rule_run 入帐（R4：非零即降级，
        //   不再只是日志）；
        // - sentences：句级恒切一次；③-T 未定案时灰区句不计入已复查（③ 全文
        //   复查覆盖全文，不扣；③ 失败且有灰区句同样扣减，见 R6）。
        let mut chunk_stats: Option<crate::quality::ChunkStats> = rule_run.chunks.take();
        if let Some(c) = &llm_chunk {
            chunk_stats = Some(match chunk_stats {
                Some(mut a) => {
                    a.merge(c);
                    a
                }
                None => c.clone(),
            });
        }
        let chunks = match chunk_stats {
            Some(c) => c,
            None if need_llm && !llm_ran => crate::quality::ChunkStats {
                mode: "single".into(),
                total: 1,
                done: 0,
                failed: 1,
                parse_failed: 0,
            },
            None => crate::quality::ChunkStats::single(),
        };
        let mut quality = crate::quality::QualityReport::assemble(
            &layers,
            chunks.coverage(),
            rule_run.unowned_chars,
        );
        quality.dropped = dropped;
        let s_spans = crate::docchunk::sentence_spans(text);
        let unreviewed_sentences = if gray_unresolved {
            s_spans
                .iter()
                .filter(|s| targeted_spans.iter().any(|t| spans_overlap(**s, *t)))
                .count()
        } else {
            0
        };
        let sentences = crate::quality::SentenceStats {
            total: s_spans.len(),
            reviewed: s_spans.len() - unreviewed_sentences,
        };
        Ok(DetectOutcome {
            errors,
            decision_abstained: abstained,
            llm_confirmed_clean,
            timings,
            layers,
            quality: Some(quality),
            chunks: Some(chunks),
            sentences: Some(sentences),
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
    // 术语库（`lexicons/terminology_{lang}.json`，空/缺失 = 不挂载，零开销）：
    // 级联形态持有字段（保留 decision_engine 转发）；其余形态经守卫包装
    let terminology = {
        let t = Arc::new(crate::terminology::Terminology::load(
            crate::terminology::Terminology::dir_from_env(),
            lang,
        ));
        if t.is_empty() { None } else { Some(t) }
    };
    // L3 特征模型引擎（三期：level 3 先跑特征模型，LLM 收缩到纯长尾）
    let l3_engine: Option<Arc<L3FeatureEngine>> = l3
        .as_ref()
        .filter(|c| c.enabled())
        .map(|c| Arc::new(L3FeatureEngine::new(lang, c.clone())));
    // 显式关闭（配置在但 enabled=false）与从未配置区分：前者 level 2 开闸回退无条件 LLM
    let l3_disabled = l3.as_ref().is_some_and(|c| !c.enabled());
    match GecEngineKind::from_env(lang) {
        GecEngineKind::Statistical => {
            let primary = Arc::new(StatisticalGecEngine::from_env(lang));
            let fallback: Arc<dyn GecEngine> = rule;
            tracing::info!(
                lang,
                model = primary.name(),
                "统计 GEC 引擎已启用（规则引擎兜底）"
            );
            wrap_terminology(
                Arc::new(CompositeDetector::new(lang, primary, Some(fallback))),
                terminology.clone(),
            )
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
            det.l3_disabled = l3_disabled;
            det.terminology = terminology.clone();
            if let Some(e) = l3_engine {
                det.l3 = Some(e);
                tracing::info!(
                    lang,
                    "L3 特征模型层已启用（level 2 门控；level 3 定案合并但不终结全文 LLM）"
                );
            }
            Arc::new(det)
        }
        GecEngineKind::Llm => {
            tracing::debug!(lang, "LLM 引擎为预留类型，当前使用规则引擎");
            wrap_terminology(rule, terminology)
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
                det.l3_disabled = l3_disabled;
                det.terminology = terminology.clone();
                if let Some(e) = l3_engine {
                    det.l3 = Some(e);
                    tracing::info!(
                        lang,
                        "L3 特征模型层已启用（level 2 门控；level 3 定案合并但不终结全文 LLM）"
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
                    det.l3_disabled = l3_disabled;
                    det.terminology = terminology.clone();
                    if let Some(e) = l3_engine {
                        det.l3 = Some(e);
                        tracing::info!(
                            lang,
                            "L3 特征模型层已启用（level 2 门控；level 3 定案合并但不终结全文 LLM）"
                        );
                    }
                    Arc::new(det)
                }
                None => wrap_terminology(rule, terminology),
            },
        },
    }
}

/// 术语库非空时把引擎包一层守卫（空则原样返回，零开销）；
/// 级联形态不经此函数（持有字段以保留 `decision_engine()` 转发）
fn wrap_terminology(
    engine: Arc<dyn GecEngine>,
    terminology: Option<Arc<crate::terminology::Terminology>>,
) -> Arc<dyn GecEngine> {
    match terminology {
        Some(t) => Arc::new(crate::terminology::TerminologyEngine::new(engine, t)),
        None => engine,
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
        // （L3 **从未配置**的旧式部署语义；若是显式关闭 enabled=false 则开闸，
        // 见 cascade_level2_with_l3_disabled_triggers_llm）
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

    #[tokio::test]
    async fn cascade_level2_with_l3_disabled_triggers_llm() {
        // L3 显式关闭（配置存在但 enabled=false，引擎未挂载）→ level 2 门控开闸，
        // 回退**无条件** LLM（l3.rs「回退无条件 LLM」口径）。若关门，叠加待发布
        // 决策层后 level 2 会静默退化为纯规则——level 2 流量丢失 LLM 覆盖率
        let llm_endpoint = mock_llm_service().await;
        let llm = llm_engine(&llm_endpoint);
        let mut det = CascadeDetector::new(
            "zh",
            Arc::new(FakeRule {
                lang: "zh",
                fail: false,
            }),
            None,
        );
        det.l3_disabled = true;
        let outcome = det
            .detect_level("正确句子", 2, false, Some(&llm))
            .await
            .unwrap();
        let llm_layer = outcome
            .layers
            .iter()
            .find(|l| l.layer == "llm")
            .expect("level 2 应报告 llm 层状态");
        assert_eq!(llm_layer.status, "ran", "显式关闭 L3 应开闸回退 LLM");
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
        let errors = composite.detect("abcdef").await.unwrap();
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
    async fn cascade_stat_rule_emits_chunk_events() {
        // 级联 rule 位挂 stat 臂：sink 经 detect_traced 透传到滑窗循环，
        // 段级事件（stage=rule）随窗推进上屏；stage 事件（rule）仍照常。
        let app = axum::Router::new().route(
            "/gec",
            axum::routing::post(|body: axum::body::Bytes| async move {
                let _ = body;
                (
                    axum::http::StatusCode::OK,
                    axum::Json(serde_json::json!({ "errors": [] })),
                )
                    .into_response()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        let stat = Arc::new(
            StatisticalGecEngine::new(
                "zh",
                StatisticalConfig {
                    endpoint: format!("http://{addr}"),
                    timeout: Duration::from_secs(5),
                    model: "mock".into(),
                },
            )
            .with_window_chars(120),
        );
        let cascade = CascadeDetector::new("zh", stat, None);
        let text = "这句话用来构造统计臂的滑窗测试。".repeat(9);
        let events: Arc<std::sync::Mutex<Vec<crate::progress::StageEvent>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let ev = events.clone();
        let sink: crate::progress::StageSink = Arc::new(move |e| ev.lock().unwrap().push(e));
        cascade
            .detect_level_traced(&text, 1, false, None, Some(sink))
            .await
            .unwrap();
        let evs = events.lock().unwrap();
        let begins = evs
            .iter()
            .filter(|e| matches!(e, crate::progress::StageEvent::ChunkBegin { stage, .. } if *stage == shared_model::sources::RULE))
            .count();
        let ends = evs
            .iter()
            .filter(|e| matches!(e, crate::progress::StageEvent::ChunkEnd { .. }))
            .count();
        assert!(begins > 1, "stat 臂多窗应逐窗推段级事件: {begins}");
        assert_eq!(begins, ends, "begin/end 配对");
        assert!(evs
            .iter()
            .any(|e| matches!(e, crate::progress::StageEvent::Stage { stage, .. } if *stage == shared_model::sources::RULE)));
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
    async fn cascade_level3_l3_hit_still_runs_llm() {
        // level 3（深度扫描）= 漏报兜底臂：L3 定案错误照合并，但全文 LLM 仍无条件跑
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
        assert!(
            outcome
                .errors
                .iter()
                .any(|e| e.source.as_deref() == Some("l3"))
                || outcome
                    .layers
                    .iter()
                    .any(|l| l.layer == sources::FEATURE && l.status == "decided"),
            "L3 定案结果应保留: {:?}",
            outcome.errors
        );
        assert!(
            outcome
                .errors
                .iter()
                .any(|e| e.source.as_deref() == Some("llm")),
            "level 3 不得被 L3 门控终结全文 LLM: {:?}",
            outcome.errors
        );
    }

    /// L3 明确无错 + 决策明确：level 3 仍**无条件**跑全文 LLM（漏报兜底）；
    /// level 2 的放行语义见 cascade_level2_l3_confident_clean_skips_llm
    #[tokio::test]
    async fn cascade_level3_l3_confident_clean_still_runs_llm() {
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
                .any(|e| e.source.as_deref() == Some("llm")),
            "特征模型明确无错不得终结 level 3 全文 LLM: {:?}",
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
        let (mapped, ledger) = targeted_llm_review(&llm, text, &[[21, 45]]).await.unwrap();
        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0].original, "报复");
        // 「报复」在原文字符 [10,12) → 字节 [30,36)
        assert_eq!(mapped[0].span, [30, 36]);
        assert!(ledger.is_empty(), "正常映射不应有丢弃记账: {ledger:?}");
    }

    /// dropped 账本：LLM 检出无法对应任何灰区段（定位失败）→ ownership 桶计数
    #[tokio::test]
    async fn targeted_llm_review_ledgers_out_of_segment_drop() {
        let endpoint = mock_llm_content(
            r#"[{"error_type":"spelling","confidence":0.9,"original":"不存在的词","correction":null,"message":"幻觉位置"}]"#,
        )
        .await;
        let llm = llm_engine(&endpoint);
        let text = "今天天气很好。他昨天报复很大。";
        let (mapped, ledger) = targeted_llm_review(&llm, text, &[[21, 45]]).await.unwrap();
        assert!(mapped.is_empty(), "段外检出必须丢弃: {mapped:?}");
        assert_eq!(ledger.ownership, 1, "段外丢弃计入 ownership 桶");
        assert_eq!(ledger.total(), 1);
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

    // ── 文档级重构 P0：LLM 层统一入口（单发 partial 语义 + 长文分块） ──

    /// mock LLM：响应可注入（garbage = 解析失败臂）
    async fn mock_llm_content(content: &'static str) -> String {
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(move |_body: axum::body::Bytes| async move {
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

    #[tokio::test]
    async fn llm_layer_parse_failure_marks_partial_not_clean() {
        // 解析失败：层状态 partial，confirmed_clean 必须为 false
        // （「复查失败」绝不得伪装成「复查确认干净」）
        let endpoint = mock_llm_content("抱歉，我无法完成该任务。").await;
        let llm = llm_engine(&endpoint);
        let (errors, status, confirmed_clean, chunk, dropped) =
            llm_layer_detect(&llm, "正确句子", None).await.unwrap();
        assert!(errors.is_empty());
        assert_eq!(status, "partial");
        assert!(!confirmed_clean, "解析失败不得当作确认干净");
        assert_eq!(chunk.mode, "single");
        assert_eq!((chunk.total, chunk.done, chunk.parse_failed), (1, 0, 1));
        assert!(dropped.is_empty());
    }

    #[tokio::test]
    async fn llm_layer_normal_run_marks_ran() {
        let endpoint = mock_llm_content("[]").await;
        let llm = llm_engine(&endpoint);
        let (errors, status, confirmed_clean, chunk, _dropped) =
            llm_layer_detect(&llm, "正确句子", None).await.unwrap();
        assert!(errors.is_empty());
        assert_eq!(status, "ran");
        assert!(confirmed_clean);
        assert_eq!(chunk.coverage(), Some(1.0));
    }

    #[tokio::test]
    async fn llm_layer_chunked_status_derives_from_outcome() {
        // 分块路径的状态判定（纯逻辑侧）：直接经 detect_document 验证
        // 阈值路由本身由 detect_document 测试覆盖（llm.rs），此处锁定
        // 「全部干净 → ran + confirmed_clean」在长文本入口同样成立
        let endpoint = mock_llm_content("[]").await;
        let llm = llm_engine(&endpoint);
        let text = "第一段是干净的中文叙述，没有任何错误需要检出。\n这是第二段内容，同样干净。\n这是第三段收尾文字，也干净。";
        let o = llm.detect_document(text, 50, 4, None).await.unwrap();
        assert!(o.chunks >= 2, "应切成多块: {}", o.chunks);
        assert_eq!(o.coverage(), 1.0);
        let (errors, _status, _clean, _chunk, _dropped) =
            llm_layer_detect(&llm, text, None).await.unwrap();
        assert!(errors.is_empty());
    }

    #[tokio::test]
    async fn level3_llm_runtime_failure_degrades_to_rule_results() {
        // 演练立场（2026-11）：LLM 层运行时失败不能把整次 detect 打回 502——
        // 规则结果必须保留，层记 `unavailable`；未配置 LLM 则仍是明确报错
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(|| async { axum::http::StatusCode::SERVICE_UNAVAILABLE }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        let llm = LlmGecEngine::new(
            "zh",
            LlmDetectConfig::from_parts(
                format!("http://{addr}/v1"),
                "sk-test",
                "mock-llm",
                5,
                "system",
                "user {{text}}",
            )
            .with_retry(Vec::new(), 0),
        );
        let fake = Arc::new(FakeRule {
            lang: "zh",
            fail: false,
        });
        let outcome = fake
            .detect_level("他昨天报复很大。", 3, false, Some(&llm))
            .await
            .expect("LLM 运行时失败必须降级 Ok，不得打回 Err");
        assert_eq!(outcome.errors.len(), 1, "规则层结果必须保留");
        assert!(
            outcome
                .layers
                .iter()
                .any(|l| l.layer == "llm" && l.status == "unavailable"),
            "llm 层应记 unavailable：{:?}",
            outcome.layers
        );
        // 未配置 ≠ 运行时失败：仍然明确报错（要人改设置）
        let e = fake
            .detect_level("他昨天报复很大。", 3, false, None)
            .await
            .unwrap_err();
        assert!(matches!(e, DetectorError::LlmNotConfigured(_)), "{e:?}");
    }

    // ── 成熟架构补强：术语库（领域术语保护） ──

    struct FakeZhSpan;

    #[async_trait]
    impl GECDetector for FakeZhSpan {
        fn language(&self) -> &str {
            "zh"
        }
        async fn detect(&self, _text: &str) -> Result<Vec<Error>, DetectorError> {
            // 固定产出「测试」[0,6) 上的错误（字节偏移）
            Ok(vec![Error::new(
                "zh",
                "spelling",
                0.9,
                [0, 6],
                "测试",
                None,
                "fake",
            )])
        }
    }

    fn term_zh(protected: &[&str], forced_json: &str) -> crate::terminology::Terminology {
        let dir = std::env::temp_dir().join(format!(
            "lf_term_engine_{}_{:04x}",
            std::process::id(),
            rand_like_seed()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let prot = protected
            .iter()
            .map(|t| format!("\"{t}\""))
            .collect::<Vec<_>>()
            .join(",");
        std::fs::write(
            dir.join("terminology_zh.json"),
            format!(r#"{{"protected":[{prot}],"forced":{forced_json}}}"#),
        )
        .unwrap();
        let t = crate::terminology::Terminology::load(&dir, "zh");
        std::fs::remove_dir_all(&dir).ok();
        t
    }

    fn rand_like_seed() -> u32 {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        N.fetch_add(1, Ordering::Relaxed)
    }

    #[tokio::test]
    async fn cascade_terminology_protects_and_forces() {
        // 级联形态：受保护术语上的错误被过滤，强制词注入
        let rule: Arc<dyn GecEngine> = Arc::new(FakeZhSpan);
        let mut det = CascadeDetector::new("zh", rule, None);
        det.terminology = Some(Arc::new(term_zh(
            &["测试"],
            r#"[{"wrong":"词","right":"词儿"}]"#,
        )));
        let outcome = det.detect_level("测试词", 1, false, None).await.unwrap();
        assert_eq!(
            outcome.errors.len(),
            1,
            "受保护错误被过滤 + 强制词注入：{:?}",
            outcome.errors
        );
        assert_eq!(outcome.errors[0].original, "词");
        assert_eq!(outcome.errors[0].correction.as_deref(), Some("词儿"));
    }

    #[tokio::test]
    async fn terminology_engine_wrapper_applies_on_default_level() {
        // 非级联形态（纯规则默认路径）经守卫包装：detect_level 同样生效
        let inner: Arc<dyn GecEngine> = Arc::new(FakeZhSpan);
        let wrapped = wrap_terminology(inner, Some(Arc::new(term_zh(&["测试"], "[]"))));
        let outcome = wrapped
            .detect_level("测试词", 1, false, None)
            .await
            .unwrap();
        assert!(
            outcome.errors.is_empty(),
            "受保护术语上的错误应被过滤：{:?}",
            outcome.errors
        );
    }

    // ── 成熟架构补强：LLM 出站 PII 掩码 ──

    #[tokio::test]
    async fn llm_layer_masks_pii_and_drops_masked_region_errors() {
        // 送 LLM 前掩码手机号；LLM 对被掩区域的检出（`*` 引用）被丢弃
        let endpoint = mock_llm_content(
            r#"[{"error_type":"spelling","confidence":0.9,"original":"***********","correction":null,"message":"masked noise"}]"#,
        )
        .await;
        let llm = llm_engine(&endpoint);
        let (errors, status, _clean, _chunk, dropped) =
            llm_layer_detect(&llm, "电话13800138000有误", None)
                .await
                .unwrap();
        assert!(
            errors.is_empty(),
            "被掩区域（`*`）的检出应被过滤：{errors:?}"
        );
        assert_eq!(status, "ran");
        assert_eq!(dropped.pii_mask, 1, "掩码区丢弃必须计入 dropped 账本");
    }

    #[tokio::test]
    async fn llm_layer_keeps_errors_outside_masked_regions() {
        // 掩码不影响被掩区域之外的检出
        let endpoint = mock_llm_content(
            r#"[{"error_type":"spelling","confidence":0.9,"original":"报负","correction":"抱负","message":"x"}]"#,
        )
        .await;
        let llm = llm_engine(&endpoint);
        let (errors, status, _, _chunk, dropped) =
            llm_layer_detect(&llm, "他13800138000说报负很大", None)
                .await
                .unwrap();
        assert_eq!(errors.len(), 1, "掩码区外检出应保留：{errors:?}");
        assert_eq!(errors[0].correction.as_deref(), Some("抱负"));
        assert_eq!(status, "ran");
        assert!(
            dropped.is_empty(),
            "区外检出保留时不应有丢弃记账: {dropped:?}"
        );
    }
}
