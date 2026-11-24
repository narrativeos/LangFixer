//! Tauri 命令门面：一个子模块一个域（与 api-server routes/ 对应，两端同构）。
//! 命令统一在门面层 re-export，main.rs / media.rs / agent_tools.rs 的命令路径保持不变。

mod agent;
mod anomaly;
mod core;
mod custom_words;
mod dataset;
mod detect;
mod eval;
mod feedback;
mod history;
mod knowledge;
mod l3;
mod optimization;
mod recall;
mod reflect;
mod rules;
mod session;
mod settings;
mod tables;
mod training;
mod util;
mod window;

pub use self::agent::*;
pub use self::core::*;
pub use self::custom_words::*;
pub use self::dataset::*;
pub use self::detect::*;
pub use self::eval::*;
pub use self::feedback::*;
pub use self::history::*;
pub use self::knowledge::*;
pub(crate) use self::l3::l3_collect_sample;
pub use self::l3::*;
pub use self::optimization::*;
pub use self::recall::*;
pub use self::reflect::*;
pub use self::rules::*;
pub use self::session::*;
pub use self::settings::*;
pub use self::tables::*;
pub use self::training::*;
pub(crate) use self::util::pack_atomic_write;
pub(crate) use self::util::tables_file_path;
pub use self::window::*;

#[cfg(test)]
mod tests;
