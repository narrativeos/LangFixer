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
/// （rule / decision / knowledge / feature / llm），与 `layers` 报告同源，
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
            StageEvent::End { total_ms: 42 },
        ];
        let json = serde_json::to_string(&evs).unwrap();
        assert_eq!(
            json,
            r#"[{"kind":"start","stages":["rule","knowledge","llm"]},{"kind":"stage","stage":"rule","status":"ran","ms":12},{"kind":"stage","stage":"llm","status":"skipped","ms":null},{"kind":"end","total_ms":42}]"#
        );
    }
}
