//! LRU 检测缓存：避免对相同文本重复运行检测引擎
//!
//! key = `{lang}:{text_hash}`，value = 检测结果 Vec<Error>。
//! 容量默认 256 条，超出时淘汰最久未访问的条目。

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Mutex;

use shared_model::Error;

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

    /// 清空缓存
    pub fn clear(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.map.clear();
    }
}

impl Default for LruCache {
    fn default() -> Self {
        Self::new(256)
    }
}

/// 生成缓存 key：`{lang}:{text_hash}`
pub fn cache_key(lang: &str, text: &str) -> String {
    let mut hasher = std::hash::DefaultHasher::new();
    text.hash(&mut hasher);
    let hash = hasher.finish();
    format!("{lang}:{:016x}", hash)
}
