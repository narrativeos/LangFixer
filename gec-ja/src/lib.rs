//! LangFixer 日文检测器
//!
//! 主路径调用 Sudachi 分词服务的 `/parse` 端点获取词性标注，
//! 再在 Rust 侧执行 GEC 规则；服务不可达时自动降级为
//! [`offline::OfflineJapaneseDetector`] 本地规则检测。

pub mod config;
pub mod detector;
pub mod offline;
pub mod rules;

pub use config::DetectorConfig;
pub use detector::JapaneseDetector;
pub use offline::OfflineJapaneseDetector;
