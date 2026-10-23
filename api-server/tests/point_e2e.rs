//! 端到端：分析器错误 → [`gec_en::classify_point`] 推断知识点
//! → 决策表命中该错误类型兜底规则 → 知识库按点富化详情。
//!
//! 覆盖分析器可达的 14 类错误（article/tense/preposition/agreement + 扩展的
//! adjective/adverb/noun/pronoun/verb/infinitive/gerund/subjunctive/negation），
//! 以及未映射介词落兜底（无知识源引用）。
//!
//! 新架构：决策表只含纯规则（点级规则已移出）；知识点详情
//! （remedial/例句/改错对/知识源引用）由知识库运行时富化，
//! 同一知识点可被多个知识源背书（1:N）。

use std::path::Path;

use attribution_engine::{AttributionEngine, KnowledgeStore};
use shared_model::{AttributionContext, AttributionResult};

fn load_en_engine() -> AttributionEngine {
    let tables_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../decision-tables");
    let content = std::fs::read_to_string(tables_dir.join("en_gec_attribution.json")).unwrap();
    AttributionEngine::new(&content, "en_gec_attribution").unwrap()
}

fn load_kb() -> KnowledgeStore {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../knowledge-base");
    let store = KnowledgeStore::new();
    store.load_from_dir(&dir).unwrap();
    store
}

/// 完整流水线：classify_point → 表级归因 → 知识库富化
async fn run(
    kb: &KnowledgeStore,
    engine: &AttributionEngine,
    error_type: &str,
    original: &str,
    correction: &str,
) -> AttributionResult {
    let point = gec_en::classify_point(error_type, original, correction);
    let ctx = AttributionContext {
        lang: "en".into(),
        error_type: error_type.into(),
        confidence: 0.9,
        session_error_count: 0,
        session_meta: None,
        grammar_point: point,
    };
    let mut result = engine.attribute(&ctx).await.unwrap();
    kb.enrich(&ctx, &mut result);
    result
}

/// 断言富化后的知识源引用：title 已从注册表解析，location 含预期片段
fn assert_source(result: &AttributionResult, expected_location: &str) {
    assert!(!result.sources.is_empty(), "sources 应非空");
    assert!(
        result.sources[0].title.contains("示例英语语法指南"),
        "源标题应从注册表解析: {:?}",
        result.sources[0].title
    );
    assert!(
        result.sources[0].location.contains(expected_location),
        "location 应含 \"{expected_location}\": {}",
        result.sources[0].location
    );
}

#[tokio::test]
async fn en_article_a_to_an_enriched() {
    let engine = load_en_engine();
    let kb = load_kb();
    let result = run(&kb, &engine, "article", "a", "an").await;
    assert_eq!(result.trace.matched_rule, "article_novice");
    assert_source(&result, "Unit 01 · 二.1 不定冠词");
}

#[tokio::test]
async fn en_tense_past_enriched() {
    let engine = load_en_engine();
    let kb = load_kb();
    let result = run(&kb, &engine, "tense", "go", "went").await;
    assert_eq!(result.trace.matched_rule, "tense_novice");
    assert_source(&result, "Unit 09 · 四.2");
}

#[tokio::test]
async fn en_preposition_on_to_in_enriched() {
    let engine = load_en_engine();
    let kb = load_kb();
    let result = run(&kb, &engine, "preposition", "on", "in").await;
    assert_eq!(result.trace.matched_rule, "preposition_any");
    assert_source(&result, "Unit 04 · 五.6");
}

#[tokio::test]
async fn en_agreement_enriched() {
    let engine = load_en_engine();
    let kb = load_kb();
    let result = run(&kb, &engine, "agreement", "He play", "He plays").await;
    assert_eq!(result.trace.matched_rule, "agreement_novice");
    assert_source(&result, "Unit 19 · 一.1");
}

#[tokio::test]
async fn en_adjective_comparative_enriched() {
    let engine = load_en_engine();
    let kb = load_kb();
    let result = run(&kb, &engine, "adjective", "more better", "better").await;
    assert_eq!(result.trace.matched_rule, "adjective_any");
    assert_source(&result, "Unit 05 · 七.4");
}

#[tokio::test]
async fn en_adverb_adj_adv_enriched() {
    let engine = load_en_engine();
    let kb = load_kb();
    let result = run(&kb, &engine, "adverb", "quiet", "quietly").await;
    assert_eq!(result.trace.matched_rule, "adverb_any");
    assert_source(&result, "Unit 06 · 二.9");
}

#[tokio::test]
async fn en_noun_countable_enriched() {
    let engine = load_en_engine();
    let kb = load_kb();
    let result = run(&kb, &engine, "noun", "informations", "information").await;
    assert_eq!(result.trace.matched_rule, "noun_any");
    assert_source(&result, "Unit 07 · 四.2");
}

#[tokio::test]
async fn en_pronoun_reflexive_enriched() {
    let engine = load_en_engine();
    let kb = load_kb();
    let result = run(&kb, &engine, "pronoun", "him", "himself").await;
    assert_eq!(result.trace.matched_rule, "pronoun_any");
    assert_source(&result, "Unit 03 · 五.1");
}

#[tokio::test]
async fn en_verb_transitive_enriched() {
    let engine = load_en_engine();
    let kb = load_kb();
    let result = run(&kb, &engine, "verb", "discuss about", "discuss").await;
    assert_eq!(result.trace.matched_rule, "verb_any");
    assert_source(&result, "Unit 08 · 三.3");
}

#[tokio::test]
async fn en_infinitive_verb_object_enriched() {
    let engine = load_en_engine();
    let kb = load_kb();
    let result = run(&kb, &engine, "infinitive", "want playing", "want to play").await;
    assert_eq!(result.trace.matched_rule, "infinitive_any");
    assert_source(&result, "Unit 11 · 二.2");
}

#[tokio::test]
async fn en_gerund_verb_object_enriched() {
    let engine = load_en_engine();
    let kb = load_kb();
    let result = run(&kb, &engine, "gerund", "enjoy to play", "enjoy playing").await;
    assert_eq!(result.trace.matched_rule, "gerund_any");
    assert_source(&result, "Unit 12 · 二.2");
}

#[tokio::test]
async fn en_subjunctive_wish_enriched() {
    let engine = load_en_engine();
    let kb = load_kb();
    let result = run(&kb, &engine, "subjunctive", "was", "were").await;
    assert_eq!(result.trace.matched_rule, "subjunctive_any");
    assert_source(&result, "Unit 14 · 四.2");
}

#[tokio::test]
async fn en_negation_double_negative_enriched() {
    let engine = load_en_engine();
    let kb = load_kb();
    let result = run(&kb, &engine, "negation", "nothing", "anything").await;
    assert_eq!(result.trace.matched_rule, "negation_any");
    assert_source(&result, "Unit 24 · 二.1");
}

#[tokio::test]
async fn en_unmapped_preposition_falls_back_without_sources() {
    // 目标介词不在映射表内 → classify_point 返回 None → 无知识点 → 无知识源引用
    let engine = load_en_engine();
    let kb = load_kb();
    let result = run(&kb, &engine, "preposition", "on", "across").await;
    assert_eq!(result.trace.matched_rule, "preposition_any");
    assert!(
        result.sources.is_empty(),
        "无知识点时不应有知识源引用: {:?}",
        result.sources
    );
}

#[tokio::test]
async fn en_fine_point_remedial_carries_point_examples() {
    // 富化后 remedial = 点级讲解；点带例句时附 "｜ 例：" 段（示例源自撰例句）
    let engine = load_en_engine();
    let kb = load_kb();
    let result = run(&kb, &engine, "article", "a", "an").await;
    assert!(
        result.remedial.iter().any(|r| r.contains("｜ 例：")),
        "富化 remedial 应含示例例句: {:?}",
        result.remedial
    );
}

#[tokio::test]
async fn en_fine_point_examples_carries_targeted_corrections() {
    // 富化后 examples 应含该点"最相关"的改错对（wrong→right）。
    // preposition.24(in) 的专属改错对 "✗ by → ✓ in" 来自示例源【改正错误】映射。
    let engine = load_en_engine();
    let kb = load_kb();
    let result = run(&kb, &engine, "preposition", "on", "in").await;
    assert!(
        result.examples.iter().any(|e| e.contains("by → ✓ in")),
        "preposition.24 的 examples 应含其专属改错对 '✗ by → ✓ in': {:?}",
        result.examples
    );
}
