//! 检测管线阶段事件实时推送（前端分阶段进度条用）
//!
//! 级联引擎在 [`crate::engine::GecEngine::detect_level_traced`] 执行过程中，把每个
//! 阶段的完成事件推给调用方注入的 [`StageSink`]：api-server 将事件写入请求级进度
//! 登记表（`X-Progress-Id` 标识，`GET /gec/detect/progress` 轮询消费）；桌面端
//! Tauri 命令把事件转成应用事件（`detect:stage`）直接推给前端。
//! 未注入 sink 的调用（后台任务 / 批量作业）零开销。

use serde::Serialize;
use std::sync::Arc;

/// 单个阶段事件。`stage` 复用 [`shared_model::sources`] 的层名
/// （rule / decision / knowledge / feature / targeted / llm），与 `layers` 报告同源，
/// 前端进度条与「本次管线」摘要共用一套标识。
#[derive(Clone, Serialize, Debug)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StageEvent {
    /// 管线开始：声明本次实际参与的阶段列表（进度条据此渲染骨架）
    Start { stages: Vec<&'static str> },
    /// 一个阶段结束。`status`：`ran` 已执行 / `unavailable` 执行失败或不可用 /
    /// `skipped` 被门控跳过；`ms` 为该阶段 wall time（skipped 时无）
    Stage {
        stage: &'static str,
        status: &'static str,
        ms: Option<u64>,
    },
    /// 段级事件（预算层执行循环）：有预算块切块的层（③ LLM 整文档分块 /
    /// stat 滑窗）每块复查**开始**推一条。块间**并发**执行，同层事件互相
    /// 交错，按 `chunk` 序号（1 起）配对聚合，不依赖先后顺序。
    /// 短文本单块路径不推段级事件——整层一条 `Stage` 已足够表达。
    ChunkBegin {
        stage: &'static str,
        chunk: u32,
        total: u32,
    },
    /// 段级事件：该块复查**结束**。`ok=false` = 该块本轮未复查
    /// （网络/服务失败或解析失败），与 `quality` 的 failed/parse_failed 账本同义。
    ChunkEnd {
        stage: &'static str,
        chunk: u32,
        total: u32,
        ok: bool,
        ms: u64,
    },
    /// 句表（块矩阵数据面）：全文一次性切句结果随事件流下发。`spans` 为字节区间
    /// （与 `quality.sentences` 同一句表来源），`origins` 等长同序标注每个右边界的
    /// 切分来源（`punct` 标点 / `marker` 篇章标记 / `pause` 停顿 / `fallback` 字数兜底）。
    /// 先于 `End` 推送；前端「块矩阵」视图据此渲染「块行 × 层列」检测矩阵，
    /// 事件快照落库后历史回放自动带矩阵数据。
    Units {
        spans: Vec<[usize; 2]>,
        origins: Vec<&'static str>,
    },
    /// 管线结束（全程 wall time，与 `DetectTimings::total_ms` 一致）
    End { total_ms: u64 },
}

/// 阶段事件接收器：线程安全回调（级联内并行分支共享同一 sink 的 Arc 克隆）。
/// 实现方必须尽快返回（仅入队 / 转发，不做 IO）。
pub type StageSink = Arc<dyn Fn(StageEvent) + Send + Sync + 'static>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_event_wire_format() {
        let evs = vec![
            StageEvent::Start {
                stages: vec!["rule", "knowledge", "llm"],
            },
            StageEvent::Stage {
                stage: "rule",
                status: "ran",
                ms: Some(12),
            },
            StageEvent::Stage {
                stage: "llm",
                status: "skipped",
                ms: None,
            },
            StageEvent::ChunkBegin {
                stage: "llm",
                chunk: 1,
                total: 2,
            },
            StageEvent::ChunkEnd {
                stage: "llm",
                chunk: 1,
                total: 2,
                ok: true,
                ms: 30,
            },
            StageEvent::Units {
                spans: vec![[0, 6], [6, 12]],
                origins: vec!["punct", "fallback"],
            },
            StageEvent::End { total_ms: 42 },
        ];
        let json = serde_json::to_string(&evs).unwrap();
        assert_eq!(
            json,
            r#"[{"kind":"start","stages":["rule","knowledge","llm"]},{"kind":"stage","stage":"rule","status":"ran","ms":12},{"kind":"stage","stage":"llm","status":"skipped","ms":null},{"kind":"chunk_begin","stage":"llm","chunk":1,"total":2},{"kind":"chunk_end","stage":"llm","chunk":1,"total":2,"ok":true,"ms":30},{"kind":"units","spans":[[0,6],[6,12]],"origins":["punct","fallback"]},{"kind":"end","total_ms":42}]"#
        );
    }
}
