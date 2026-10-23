use std::collections::HashMap;
use std::sync::Arc;

use shared_model::Error;

use crate::detector::DetectorError;
use crate::engine::GecEngine;

/// 语言展示优先级（中文 → 英文 → 日文，其余其后）
fn lang_display_rank(lang: &str) -> u8 {
    match lang {
        "zh" => 0,
        "en" => 1,
        "ja" => 2,
        _ => 3,
    }
}

/// 按语言路由到具体引擎（规则 / 统计 / 组合）
pub struct LanguageRouter {
    engines: HashMap<String, Arc<dyn GecEngine>>,
}

impl LanguageRouter {
    pub fn new() -> Self {
        Self {
            engines: HashMap::new(),
        }
    }

    pub fn register(&mut self, engine: Arc<dyn GecEngine>) {
        let lang = engine.name().to_string();
        self.engines.insert(lang, engine);
    }

    pub fn get(&self, lang: &str) -> Option<&Arc<dyn GecEngine>> {
        self.engines.get(lang)
    }

    /// 语言展示顺序：产品主语言固定为 **中文 → 英文 → 日文**，
    /// 其余语言按字典序排在其后（前端语言切换器与 API 列表共用此顺序）
    pub fn supported_languages(&self) -> Vec<String> {
        let mut v: Vec<String> = self.engines.keys().cloned().collect();
        v.sort_by_key(|lang| (lang_display_rank(lang), lang.clone()));
        v
    }

    /// 按语言执行检测；未注册的语言返回 [`DetectorError::UnsupportedLanguage`]
    pub async fn detect(&self, lang: &str, text: &str) -> Result<Vec<Error>, DetectorError> {
        let engine = self
            .get(lang)
            .ok_or_else(|| DetectorError::UnsupportedLanguage(lang.to_string()))?;
        engine.detect(text).await
    }
}

impl Default for LanguageRouter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detector::GECDetector;
    use async_trait::async_trait;
    use shared_model::Error;

    struct FakeDetector {
        lang: &'static str,
    }

    #[async_trait]
    impl GECDetector for FakeDetector {
        fn language(&self) -> &str {
            self.lang
        }
        async fn detect(&self, text: &str) -> Result<Vec<Error>, DetectorError> {
            Ok(vec![Error::new(
                self.lang,
                "spelling",
                0.9,
                [0, text.len()],
                text,
                None,
                "fake error",
            )])
        }
    }

    #[tokio::test]
    async fn router_routes_by_language() {
        let mut router = LanguageRouter::new();
        router.register(Arc::new(FakeDetector { lang: "zh" }));
        router.register(Arc::new(FakeDetector { lang: "en" }));

        assert_eq!(router.supported_languages(), vec!["zh", "en"]);

        let errors = router.detect("zh", "你好").await.unwrap();
        assert_eq!(errors[0].lang, "zh");

        let err = router.detect("ja", "x").await.unwrap_err();
        assert!(matches!(err, DetectorError::UnsupportedLanguage(_)));
    }

    #[test]
    fn supported_languages_display_order_is_zh_en_ja() {
        // 展示顺序固定：中文 → 英文 → 日文（与注册顺序无关），其余语言字典序在后
        let mut router = LanguageRouter::new();
        router.register(Arc::new(FakeDetector { lang: "fr" }));
        router.register(Arc::new(FakeDetector { lang: "ja" }));
        router.register(Arc::new(FakeDetector { lang: "en" }));
        router.register(Arc::new(FakeDetector { lang: "zh" }));
        assert_eq!(router.supported_languages(), vec!["zh", "en", "ja", "fr"]);
    }
}
