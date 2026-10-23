//! LangFixer 英文检测器
//!
//! 主路径调用 HanLP 服务（NarrativeOS `narrative-operator-nlp`）的 `/analyze`
//! 端点获取英文分词 + POS 结果，再在 Rust 侧执行 GEC 规则；
//! 服务不可达时自动降级为 [`offline::OfflineEnglishDetector`] 纯本地检测。

pub mod config;
pub mod detector;
pub mod offline;
pub mod point;
pub mod rules;

pub use config::DetectorConfig;
pub use detector::EnglishDetector;
pub use offline::OfflineEnglishDetector;
pub use point::classify_point;
