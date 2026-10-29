//! LangFixer 载体（图片 / PDF / Word）审校编排
//!
//! 分工（`MULTIMODAL.md` §8 / §14）：
//! - [`media_core`]：模型、拼接 / 回映射（纯函数）、协作工具适配器（PhotoCraft…）
//! - 本 crate：job 状态机、抽取档位选择、检测级联**接缝**、回写、正误表导出
//! - 宿主（api-server / desktop-app）：传输层 + 自己的检测级联
//!
//! 为什么要单独一层：载体链路有两个宿主——api-server（HTTP，Web UI）与
//! desktop-app（Tauri `invoke`）。两端的检测级联不同（服务端缓存 / 历史 / 白名单 vs
//! 桌面端本地决策表 / 离线模式），但**其余部分必须同一份实现**，否则两端会漂出
//! 不同的判定与留痕口径（同一张图给出不同的框、不同的告警）。

pub mod flow;
pub mod jobs;
pub mod report;
pub mod settings;
pub mod vision;

pub use flow::{
    CarrierDetector, DetectResult, FlowCtx, FlowError, JobOptions, ModeGate, ReportRequest,
    advanced_cli, carrier_kind_of, create_job, ensure_enabled, mode_gate, mode_gate_json,
    pdf_craft, photo_craft, preview_kind_for, probes, safe_name, vision_ready, word_craft,
};
pub use jobs::{MediaJob, MediaJobStore, MediaStage};
pub use media_core::{JobMode, PreviewKind};
pub use settings::MediaSettings;
