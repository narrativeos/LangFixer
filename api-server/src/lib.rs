//! LangFixer API 服务（库入口，供集成测试使用）

/// 程序版本号（crate 版本；写入检测历史，便于跨版本对比改进 / 按版本筛选训练数据）
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod agent_tools;
pub mod cache;
pub mod custom_words;
pub mod detect_history;
pub mod knowledge_recall;
// 载体（图片 / PDF / Word）链路的实现已抽到 `media-flow`（api-server 与桌面端共用同一条
// 链路）；这里按原名转发，既有测试与 `media-report` 二进制的引用不变。
pub use media_flow::{jobs as media_jobs, report as media_report, vision as media_vision};
pub mod middleware;
pub mod routes;
pub mod settings;
pub mod state;
pub mod table_history;
