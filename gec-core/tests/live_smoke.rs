//! 一次性 live 冒烟（非 CI）：连接真实 feature-model 服务（127.0.0.1:4899）
//! 与 mock OpenAI 服务（127.0.0.1:4900），验证 L3 部分定案 → 定向 LLM 复查全链路。
//! 运行：`cargo test -p gec-core --test live_smoke -- --ignored --nocapture`
use gec_core::{
    L3FeatureConfig, L3FeatureEngine, LlmDetectConfig, LlmGecEngine, targeted_llm_review,
};

#[tokio::test]
#[ignore]
async fn live_partial_accept_targeted_review() {
    let l3 = L3FeatureEngine::new(
        "zh",
        L3FeatureConfig {
            endpoint: "http://127.0.0.1:4899".into(),
            ..Default::default()
        },
    );
    let text = "好。走吧。行。嗯。他昨天报复很大。";
    let o = l3.detect(text).await.unwrap();
    assert!(!o.abstained, "应部分定案而非整体弃权");
    assert!(o.errors.is_empty());
    // 灰区句「他昨天报复很大。」字符 [9,17) → 字节 [27,51)
    assert_eq!(o.unreviewed_spans, vec![[27, 51]]);

    let llm = LlmGecEngine::new(
        "zh",
        LlmDetectConfig::from_parts(
            "http://127.0.0.1:4900/v1",
            "sk-test",
            "mock-llm",
            5,
            "system",
            "{{text}}",
        ),
    );
    let mapped = targeted_llm_review(&llm, text, &o.unreviewed_spans)
        .await
        .unwrap();
    assert_eq!(mapped.len(), 1);
    assert_eq!(mapped[0].original, "报复");
    // 「报复」在原文字符 [12,14) → 字节 [36,42)
    assert_eq!(mapped[0].span, [36, 42]);
    println!(
        "OK: unreviewed={:?} mapped={:?}",
        o.unreviewed_spans, mapped[0].span
    );
}
