//! LangFixer 教育归因引擎
//!
//! 基于 ZEN Engine（GoRules JDM 标准）决策表，对每个语法错误进行多维度归因：
//! - [`engine::AttributionEngine`]：加载并执行 JDM 决策表
//! - [`loader::TableStore`]：多语言决策表管理（目录加载 + 热更新）
//! - [`knowledge::KnowledgeStore`]：知识库（知识源注册表 + 知识点集合，与规则分离）
//! - [`ingest::StagingStore`]：知识源导入管线（书 → 草稿区 → 归并发布）
//! - [`weiner::default_weiner`]：韦纳三维度默认映射

pub mod dataset;
pub mod engine;
pub mod ingest;
pub mod knowledge;
pub mod loader;
pub mod runtime;
pub mod weiner;

pub use dataset::{
    PACK_FORMAT, PACK_FORMAT_VERSION, PackCleanReport, PackComponents, PackDecisionTable,
    PackImportReport, PackSummary, ResourcePack, aux_is_empty, aux_merge_source, aux_split_source,
    clean_pack, export_pack, import_pack, merge_tagged_rows, remove_tagged_rows, summarize,
    tagged_rule_count, validate_pack,
};
pub use engine::{AttributionEngine, AttributionError};
pub use ingest::{StagingStore, apply_plan, chunk_chapters, chunk_text, plan_merge};
pub use knowledge::KnowledgeStore;
pub use loader::TableStore;
pub use runtime::ZenWorker;
pub use weiner::default_weiner;
