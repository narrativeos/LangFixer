//! LRU 检测缓存：避免对相同文本重复运行检测引擎
//!
//! key = `{lang}:{text_hash}:{规则指纹}:{模型版本}:{层开关}`，value = 检测结果 Vec<Error>。
//! 指纹入键与历史复用门槛（`shared_model::history_reusable`）同一口径：规则侧
//! （词表 / 决策表 / 禁用规则 / 自定义词）、模型侧（决策模型重训练）与层开关的
//! 任何变化都会使旧键失效——变化后同文本必然 miss 重检，不会命中旧结果的
//! 「错误缓存」（旧版 key 只含文本，变化后仍会命中）。
//! 容量默认 256 条，超出时淘汰最久未访问的条目；`clear` 经
//! POST /gec/cache/clear 暴露为显式纠正措施。

use std::collections::HashMap;
use std::sync::Mutex;

use shared_model::{DetectParams, Error};

struct CacheInner {
    map: HashMap<String, (u64, Vec<Error>)>,
    counter: u64,
}

/// 简单 LRU 缓存（Mutex 保护，容量有限）
pub struct LruCache {
    inner: Mutex<CacheInner>,
    capacity: usize,
}

impl LruCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(CacheInner {
                map: HashMap::new(),
                counter: 0,
            }),
            capacity,
        }
    }

    /// 获取缓存（命中时更新访问顺序）
    pub fn get(&self, key: &str) -> Option<Vec<Error>> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.counter += 1;
        let ts = inner.counter;
        if let Some(entry) = inner.map.get_mut(key) {
            entry.0 = ts;
            Some(entry.1.clone())
        } else {
            None
        }
    }

    /// 插入缓存（超容量时淘汰最旧条目）
    pub fn insert(&self, key: String, value: Vec<Error>) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.counter += 1;
        let ts = inner.counter;
        if inner.map.len() >= self.capacity && !inner.map.contains_key(&key) {
            let oldest = inner
                .map
                .iter()
                .min_by_key(|(_, (t, _))| *t)
                .map(|(k, _)| k.clone());
            if let Some(k) = oldest {
                inner.map.remove(&k);
            }
        }
        inner.map.insert(key, (ts, value));
    }

    /// 清空缓存，返回被清除的条目数（显式纠正措施：/gec/cache/clear）
    pub fn clear(&self) -> usize {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let n = inner.map.len();
        inner.map.clear();
        n
    }
}

impl Default for LruCache {
    fn default() -> Self {
        Self::new(256)
    }
}

/// 生成缓存 key：`{lang}:{text_hash}:{规则指纹}:{模型版本}:{切块版本}:{层开关}`
///
/// 参数快照逐项入键（规则指纹 / 模型版本 / 切块版本 / 决策层与 L3 开关）：任一
/// 变化都改变 key，使旧缓存条目不可达（自然过期，无需显式失效）。文本哈希用
/// 稳定 fnv1a（与历史层同口径；旧版 DefaultHasher 进程内随机种子无益且难排查）。
pub fn cache_key(lang: &str, text: &str, params: &DetectParams) -> String {
    format!(
        "{lang}:{}:{}:{}:s{}:d{}l{}",
        shared_model::text_hash(text),
        params.rules_fingerprint,
        params.model_version.as_deref().unwrap_or("-"),
        params.splitter,
        u8::from(params.decision_enabled),
        u8::from(params.l3_enabled),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(fp: &str, mv: Option<&str>, decision: bool, l3: bool) -> DetectParams {
        DetectParams {
            level: 1,
            force_llm: false,
            category: None,
            decision_enabled: decision,
            l3_enabled: l3,
            llm_configured: false,
            llm_model: None,
            model_version: mv.map(str::to_string),
            rules_fingerprint: fp.into(),
            splitter: shared_model::SPLITTER_VERSION.into(),
        }
    }

    #[test]
    fn cache_key_stable_for_same_params() {
        assert_eq!(
            cache_key("zh", "同一句话", &params("f1", Some("v1"), true, true)),
            cache_key("zh", "同一句话", &params("f1", Some("v1"), true, true))
        );
    }

    #[test]
    fn cache_key_changes_with_any_invalidating_param() {
        let base = cache_key("zh", "同一句话", &params("f1", Some("v1"), true, true));
        // 规则侧变化（词表 / 决策表 / 禁用规则 / 自定义词）
        assert_ne!(
            base,
            cache_key("zh", "同一句话", &params("f2", Some("v1"), true, true))
        );
        // 决策模型重训练（版本号变化）
        assert_ne!(
            base,
            cache_key("zh", "同一句话", &params("f1", Some("v2"), true, true))
        );
        // 版本变为未知
        assert_ne!(
            base,
            cache_key("zh", "同一句话", &params("f1", None, true, true))
        );
        // 层开关变化（原始输出随之变化）
        assert_ne!(
            base,
            cache_key("zh", "同一句话", &params("f1", Some("v1"), false, true))
        );
        assert_ne!(
            base,
            cache_key("zh", "同一句话", &params("f1", Some("v1"), true, false))
        );
        // 文本变化
        assert_ne!(
            base,
            cache_key("zh", "另一句话", &params("f1", Some("v1"), true, true))
        );
        // 切块器版本变化（分块结果随之变化）
        let mut drift = params("f1", Some("v1"), true, true);
        drift.splitter = "punct-v0".into();
        assert_ne!(base, cache_key("zh", "同一句话", &drift));
    }

    #[test]
    fn clear_reports_and_empties() {
        let c = LruCache::new(8);
        c.insert("k1".into(), vec![]);
        c.insert("k2".into(), vec![]);
        assert_eq!(c.clear(), 2);
        assert!(c.get("k1").is_none());
    }
}
