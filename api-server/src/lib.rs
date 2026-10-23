//! LangFixer API 服务（库入口，供集成测试使用）

/// 程序版本号（crate 版本；写入检测历史，便于跨版本对比改进 / 按版本筛选训练数据）
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod agent_tools;
pub mod cache;
pub mod custom_words;
pub mod detect_history;
pub mod knowledge_recall;
pub mod middleware;
pub mod routes;
pub mod settings;
pub mod state;
pub mod table_history;
