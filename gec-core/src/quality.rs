//! 检测质量自描述（Detect Pipeline v2 P1：结果自带 quality / chunks / sentences）
//!
//! 设计契约（`docs/DETECT_PIPELINE.md` §4/§5）：
//! - **降级不得伪装成正常结果**：任何层 unavailable → `degraded`；任何层 partial
//!   或块级覆盖不全 → `partial`；一切正常 → `full`；
//! - `coverage` 是**预算层**（分块传输的层）的已复查块占比；全文单发路径恒 1.0；
//!   `None` = 本路径未做覆盖核算（如缓存命中 / 历史复用的回放结果）；
//! - `unowned_chars` 是所有权图空洞字节数，**必须恒 0**（截断探测器；由
//!   [`crate::docchunk`] 铺满不变式构造保证，异常即 `degraded`）；
//! - 缓存资格：**仅 `status == "full"` 的运行可写 LRU / 历史**（调用方在写入侧判定）。

use serde::{Deserialize, Serialize};

/// 质量报告（`DetectOutcome.quality`；`None` = 未实际检测 / 回放结果）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QualityReport {
    /// `"full"` | `"partial"` | `"degraded"`
    pub status: String,
    /// 已复查块 / 总块（全文路径恒 1.0；`None` = 未核算）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage: Option<f64>,
    /// 所有权图空洞（必须恒 0；异常即告警级完整性回归）
    pub unowned_chars: usize,
    /// 降级原因（`"{layer}_unavailable"` / `"{layer}_partial"` / `"coverage:…"` 等）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub degraded_reasons: Vec<String>,
    /// 检出丢弃账本（Detect Pipeline v2 §7：丢弃必须留痕，不得静默吞噬）。
    /// 各计数默认 0；装配方在 `assemble` 之后按来源直接赋值/合并。
    #[serde(default, skip_serializing_if = "DroppedLedger::is_empty")]
    pub dropped: DroppedLedger,
}

/// 检出丢弃账本（按**原因**分桶；总量非零即完整性告警信号）。
///
/// 来源接线：
/// - `ownership`：③-T 定向复查中 LLM 检出落在任何灰区段之外（定位失败/幻觉位置）；
/// - `pii_mask`：PII 掩码区（`*`）内的检出丢弃（filter_masked_errors）；
/// - `out_of_range`：跨出送审文本边界的检出——stat 臂只钳制不丢弃，恒 0 预留；
/// - `out_of_scope`：分块前缀区双报丢弃（所有权归上一块，remap_chunk_errors）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DroppedLedger {
    /// 所有权图外（不属于任何归属区/灰区段）
    #[serde(default)]
    pub ownership: usize,
    /// PII 掩码区检出
    #[serde(default)]
    pub pii_mask: usize,
    /// 越出送审范围（预留：现行各臂只钳制不丢弃）
    #[serde(default)]
    pub out_of_range: usize,
    /// 域外双报（上下文前缀区，归上一块）
    #[serde(default)]
    pub out_of_scope: usize,
}

impl DroppedLedger {
    /// 四项全零（账本无内容，序列化可省略）
    pub fn is_empty(&self) -> bool {
        self.ownership == 0
            && self.pii_mask == 0
            && self.out_of_range == 0
            && self.out_of_scope == 0
    }

    /// 丢弃总数（非零即告警级完整性信号）
    pub fn total(&self) -> usize {
        self.ownership + self.pii_mask + self.out_of_range + self.out_of_scope
    }

    /// 逐桶累加（多来源合并，如全文 LLM ⊕ 定向复查）
    pub fn merge(&mut self, other: &DroppedLedger) {
        self.ownership += other.ownership;
        self.pii_mask += other.pii_mask;
        self.out_of_range += other.out_of_range;
        self.out_of_scope += other.out_of_scope;
    }
}

impl QualityReport {
    /// 按层状态与覆盖核算组装质量（Detect Pipeline v2 §4 记账表的单一实现）。
    ///
    /// 状态推导（从严到宽）：所有权空洞 / 任何层 unavailable → `degraded`；
    /// 任何层 partial 或 coverage < 1 → `partial`；否则 `full`。
    /// `abstained` / `skipped` / `decided` / `ran` 属正常语义，不记降级。
    pub fn assemble(
        layers: &[crate::engine::LayerReport],
        coverage: Option<f64>,
        unowned: usize,
    ) -> Self {
        let mut status = "full";
        let mut reasons: Vec<String> = Vec::new();
        for l in layers {
            match l.status.as_str() {
                "unavailable" => {
                    reasons.push(format!("{}_unavailable", l.layer));
                    status = "degraded";
                }
                "partial" => {
                    reasons.push(format!("{}_partial", l.layer));
                    if status != "degraded" {
                        status = "partial";
                    }
                }
                _ => {}
            }
        }
        if unowned != 0 {
            reasons.push(format!("unowned_chars:{unowned}"));
            status = "degraded";
        }
        if let Some(c) = coverage
            && c < 1.0
        {
            reasons.push(format!("coverage:{c:.2}"));
            if status == "full" {
                status = "partial";
            }
        }
        Self {
            status: status.to_string(),
            coverage,
            unowned_chars: unowned,
            degraded_reasons: reasons,
            dropped: DroppedLedger::default(),
        }
    }

    /// 缓存资格：仅 full 可写 LRU / 历史（`None` 质量 = 回放结果，调用方自行决定）
    pub fn cacheable(&self) -> bool {
        self.status == "full"
    }
}

/// 段级（预算块）执行摘要（`DetectOutcome.chunks`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChunkStats {
    /// `"single"`（全文单发）| `"chunked"`（预算块并行/滑窗）
    pub mode: String,
    pub total: usize,
    pub done: usize,
    /// 网络/服务失败的块数（未复查）
    pub failed: usize,
    /// 响应成功但解析失败的块数（同样未复查，禁止折算为空结果）
    pub parse_failed: usize,
}

impl ChunkStats {
    /// 全文单发完成
    pub fn single() -> Self {
        Self {
            mode: "single".into(),
            total: 1,
            done: 1,
            failed: 0,
            parse_failed: 0,
        }
    }

    /// 多**扫描同一文本的不同层**的合并（任一 chunked 即整体记 chunked）。
    /// R5：coverage 取并集而非分母相加——各层扫的是同一段文本，stat 滑窗
    /// 9/9 叠 LLM 全文 1/1 仍是全文复查，不得变 10/11，否则全链路跑满的
    /// 请求会被无端 demote 出缓存。total 取较大侧为基准，done 按“两侧都
    /// 漏”的补集折算；failed/parse_failed 按层累加仅作诊断展示（重叠区
    /// 可双计，coverage 不从它们派生）。
    pub fn merge(&mut self, other: &ChunkStats) {
        if other.mode == "chunked" || self.mode == "chunked" {
            self.mode = "chunked".into();
        }
        let ca = self.coverage().unwrap_or(0.0);
        let cb = other.coverage().unwrap_or(0.0);
        self.total = self.total.max(other.total);
        self.done = ((1.0 - (1.0 - ca) * (1.0 - cb)) * self.total as f64).round() as usize;
        self.failed += other.failed;
        self.parse_failed += other.parse_failed;
    }

    /// 覆盖核算：chunked 按块比；single 全完成 1.0、未复查（失败/解析失败）0.0。
    /// 无任何执行（total 0）→ None（未核算）。
    pub fn coverage(&self) -> Option<f64> {
        if self.total == 0 {
            return None;
        }
        if self.mode == "chunked" {
            Some(self.done as f64 / self.total as f64)
        } else {
            Some(if self.done == self.total { 1.0 } else { 0.0 })
        }
    }
}

/// 句级（结构切分）执行摘要（`DetectOutcome.sentences`；③-T 定向复查计此处，
/// 不计入 chunks——Detect Pipeline v2 §3.1 两个维度各算各的账）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SentenceStats {
    pub total: usize,
    /// 已被某复查层覆盖的句数（L3 部分定案的灰区句未经 ③-T/③ 复查时扣减）
    pub reviewed: usize,
}

/// 每次调用的**运行态输出**（C4 修复）：stat 滑窗执行摘要与所有权空洞不再
/// 存入引擎共享的 Mutex/Atomic（并发请求互染质量/缓存判定），而是由调用方
/// 创建并随调用栈携带；各并发请求各用各的，结束即丢弃。
#[derive(Debug, Default, Clone)]
pub struct RunReport {
    /// 本次调用的预算块/窗执行摘要（`None` = 本引擎无切块，全文单层路径）
    pub chunks: Option<ChunkStats>,
    /// 所有权图空洞字节数（R4：stat 侧切块空洞写入本字段并进质量帐单，
    /// 非零即 `degraded`，不再只是日志）
    pub unowned_chars: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::LayerReport;

    fn layer(name: &str, status: &str) -> LayerReport {
        LayerReport::new(name, status, 0)
    }

    #[test]
    fn assemble_all_normal_is_full() {
        let q = QualityReport::assemble(
            &[
                layer("rule", "ran"),
                layer("feature", "decided"),
                layer("llm", "skipped"),
            ],
            Some(1.0),
            0,
        );
        assert_eq!(q.status, "full");
        assert!(q.degraded_reasons.is_empty());
        assert!(q.cacheable());
    }

    #[test]
    fn assemble_partial_layer() {
        let q = QualityReport::assemble(&[layer("rule", "ran"), layer("llm", "partial")], None, 0);
        assert_eq!(q.status, "partial");
        assert_eq!(q.degraded_reasons, vec!["llm_partial".to_string()]);
        assert!(!q.cacheable());
    }

    #[test]
    fn assemble_unavailable_beats_partial() {
        let q = QualityReport::assemble(
            &[layer("llm", "partial"), layer("feature", "unavailable")],
            Some(0.5),
            0,
        );
        assert_eq!(q.status, "degraded");
        assert!(
            q.degraded_reasons
                .contains(&"feature_unavailable".to_string())
        );
        assert!(q.degraded_reasons.contains(&"llm_partial".to_string()));
    }

    #[test]
    fn assemble_coverage_gap_is_partial() {
        let q = QualityReport::assemble(&[layer("rule", "ran")], Some(0.75), 0);
        assert_eq!(q.status, "partial");
        assert!(
            q.degraded_reasons
                .iter()
                .any(|r| r.starts_with("coverage:"))
        );
    }

    #[test]
    fn assemble_unowned_gap_is_degraded() {
        let q = QualityReport::assemble(&[layer("rule", "ran")], Some(1.0), 7);
        assert_eq!(q.status, "degraded");
        assert_eq!(q.unowned_chars, 7);
    }

    #[test]
    fn dropped_ledger_defaults_empty_and_merges() {
        let mut a = DroppedLedger::default();
        assert!(a.is_empty());
        assert_eq!(a.total(), 0);
        a.pii_mask = 2;
        a.merge(&DroppedLedger {
            pii_mask: 1,
            out_of_scope: 3,
            ..Default::default()
        });
        assert_eq!(a.pii_mask, 3);
        assert_eq!(a.out_of_scope, 3);
        assert_eq!(a.total(), 6);
        assert!(!a.is_empty());
    }

    #[test]
    fn assemble_keeps_dropped_zero_field() {
        let q = QualityReport::assemble(&[layer("rule", "ran")], Some(1.0), 0);
        assert!(q.dropped.is_empty());
    }

    #[test]
    fn chunk_coverage_and_merge() {
        assert_eq!(ChunkStats::single().coverage(), Some(1.0));
        let mut bad = ChunkStats::single();
        bad.done = 0;
        bad.parse_failed = 1;
        assert_eq!(bad.coverage(), Some(0.0));

        // R5 并集语义：单层扫全文成功（coverage 1.0）叠另一层 3/4 →并集 1.0，
        // 失败计数仅作诊断保留
        let mut a = ChunkStats::single();
        let b = ChunkStats {
            mode: "chunked".into(),
            total: 4,
            done: 3,
            failed: 1,
            parse_failed: 0,
        };
        a.merge(&b);
        assert_eq!(a.mode, "chunked");
        assert_eq!((a.total, a.done, a.failed), (4, 4, 1));
        assert_eq!(a.coverage(), Some(1.0));

        // 双侧都未全覆盖：4/8 ⊕ 4/8 并集 = 6/8 = 75%，不得报 8/16。
        // 并集 done 按独立失败概率折算后四舍五入（至多半块误差，如 5/10 ⊕ 5/10 → 8/10）
        let mut c = ChunkStats {
            mode: "chunked".into(),
            total: 8,
            done: 4,
            failed: 4,
            parse_failed: 0,
        };
        c.merge(&ChunkStats {
            mode: "chunked".into(),
            total: 8,
            done: 4,
            failed: 4,
            parse_failed: 0,
        });
        assert_eq!((c.total, c.done), (8, 6));
        assert_eq!(c.coverage(), Some(0.75));

        let none = ChunkStats {
            mode: "single".into(),
            total: 0,
            done: 0,
            failed: 0,
            parse_failed: 0,
        };
        assert_eq!(none.coverage(), None);
    }

    /// R5：stat 9/9 ⊕ LLM 全文单发 1/1 = 全文复查（1.0），不是 10/11；
    /// 否则全链路跑满的整次运行被无端排除在缓存外。
    #[test]
    fn merge_coverage_is_union_not_sum() {
        let stat = ChunkStats {
            mode: "chunked".into(),
            total: 9,
            done: 9,
            failed: 0,
            parse_failed: 0,
        };
        let mut merged = stat;
        merged.merge(&ChunkStats::single());
        assert_eq!((merged.total, merged.done), (9, 9));
        assert_eq!(merged.coverage(), Some(1.0));

        // stat 有一窗失败但 LLM 全文成功：全文仍被至少一层复查 → 1.0
        let stat_bad = ChunkStats {
            mode: "chunked".into(),
            total: 9,
            done: 8,
            failed: 1,
            parse_failed: 0,
        };
        let mut merged2 = stat_bad;
        merged2.merge(&ChunkStats::single());
        assert_eq!(merged2.coverage(), Some(1.0));
    }
}
