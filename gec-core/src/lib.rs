//! LangFixer 通用检测接口
//!
//! 定义 [`GECDetector`] trait、[`GecEngine`] 引擎路由抽象
//! （rule / decision / statistical / llm）、[`LanguageRouter`] 语言路由与
//! [`DetectionPipeline`] 检测流水线。
//! 新增语言检测器只需实现 `GECDetector` 并注册到路由器；
//! 统计模型服务经 [`StatisticalGecEngine`] 挂载，
//! 决策模型（vegaml）服务经 [`DecisionGecEngine`] + [`compose_detector`] 挂载，
//! LLM 兜底层经 [`LlmGecEngine`] + 三级响应（`detect_level` level 2/3）按需触发。

pub mod agent;
pub mod circuit;
pub mod contribution;
pub mod decision;
pub mod detector;
pub mod embedding;
pub mod engine;
pub mod knowledge_recall;
pub mod l3;
pub mod language;
pub mod llm;
pub mod models;
pub mod optimization_round;
pub mod pipeline;
pub mod reflection;
pub mod reflection_center;
pub mod settings_test;
pub mod statistical;
pub mod timings;
pub mod training_center;

pub use agent::{
    ActionTier, AgentError, AgentRunResult, AgentToolExecutor, ExecutedAction, ManagedTool,
    ProposedAction, TierGate, TraceKind, TraceStep, agent_timeout, build_rejection_task, run_agent,
};
pub use circuit::CircuitBreaker;
pub use contribution::{
    AblationSpec, ContributionReportIn, ContributionTable, LayerContribution, ablation_layer,
    measure_contributions, parse_report_summary,
};
pub use decision::{DecideOutcome, DecisionConfig, DecisionGecEngine};
pub use detector::{DetectorError, GECDetector};
pub use embedding::{EmbeddingConfig, EmbeddingEngine, cosine_similarity};
pub use engine::{
    CascadeDetector, CompositeDetector, DetectOutcome, GecEngine, GecEngineKind, LayerReport,
    char_span_to_bytes, compose_detector, merge_errors, targeted_llm_review,
};
pub use knowledge_recall::{
    KnowledgeRecall, RecallHit, fuzzy_similarity, knowledge_hit_to_error, split_sentences,
};
pub use l3::{L3FeatureConfig, L3FeatureEngine, L3Outcome, default_l3_data_dir};
pub use language::LanguageRouter;
pub use llm::{LlmDetectConfig, LlmGecEngine};
pub use models::{ModelEntry, ModelRegistry, OUTPUT_JSON, OUTPUT_PLAIN};
pub use optimization_round::{
    RoundAction, RoundInput, RoundPlan, no_action_reasons, plan_round, round_report, round_status,
};
pub use pipeline::DetectionPipeline;
pub use reflection::{
    ExampleInfo, KnowledgeInfo, ReflectionContext, ReflectionReport, ReflectionSuggestion,
    RuleDefInfo, normalize_source, reflect_rejection, suggestion_kinds,
};
pub use reflection_center::{
    EvalEvidence, EvalLayerEvidence, FeedbackEntryIn, FeedbackStatIn, GoldEvidenceIn, GoldMissIn,
    L3RetrainStateIn, MissAction, ReflectionCenterReport, build_report, build_report_with_gold,
    classify_miss, fetch_eval_evidence, gold_findings, gold_suggestions, parse_eval_report,
    select_evidence_report_id,
};
pub use settings_test::TestResult;
pub use statistical::{StatisticalConfig, StatisticalGecEngine};
pub use timings::{DetectTimings, NlpCall};
pub use training_center::{
    DomainPool, LlmTrainingData, LlmTrainingSample, PoolInput, PoolSample, TrainingJob,
    build_domain_pool, build_llm_training_data, is_model_source, job_kind, job_status,
    plan_from_suggestion,
};
