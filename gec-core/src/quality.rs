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
        if let Some(c) = coverage {
            if c < 1.0 {
                reasons.push(format!("coverage:{c:.2}"));
                if status == "full" {
                    status = "partial";
                }
            }
        }
        Self {
            status: status.to_string(),
            coverage,
            unowned_chars: unowned,
            degraded_reasons: reasons,
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

    /// 多预算层贡献合并（任一 chunked 即整体记 chunked；各计数相加）。
    /// 单发贡献按 1 块计入 total/done（解析失败记 parse_failed）。
    pub fn merge(&mut self, other: &ChunkStats) {
        if other.mode == "chunked" || self.mode == "chunked" {
            self.mode = "chunked".into();
        }
        self.total += other.total;
        self.done += other.done;
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
    fn chunk_coverage_and_merge() {
        assert_eq!(ChunkStats::single().coverage(), Some(1.0));
        let mut bad = ChunkStats::single();
        bad.done = 0;
        bad.parse_failed = 1;
        assert_eq!(bad.coverage(), Some(0.0));

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
        assert_eq!((a.total, a.done, a.failed), (5, 4, 1));
        assert_eq!(a.coverage(), Some(0.8));

        let none = ChunkStats {
            mode: "single".into(),
            total: 0,
            done: 0,
            failed: 0,
            parse_failed: 0,
        };
        assert_eq!(none.coverage(), None);
    }
}
