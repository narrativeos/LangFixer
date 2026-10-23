use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, RwLock};

use shared_model::DecisionTableMeta;

use crate::engine::{AttributionEngine, AttributionError};

const TABLE_SUFFIX: &str = "_gec_attribution";

fn table_key(lang: &str) -> String {
    format!("{lang}{TABLE_SUFFIX}")
}

/// 决策表存储：支持目录批量加载与按语言热更新
#[derive(Default)]
pub struct TableStore {
    engines: RwLock<HashMap<String, Arc<AttributionEngine>>>,
    raw: RwLock<HashMap<String, String>>,
    /// 真实元数据（规则数 + 加载/更新时间；替代旧的假 version/updated_at）
    meta: RwLock<HashMap<String, DecisionTableMeta>>,
}

impl TableStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 从目录加载所有 `*_gec_attribution.json` 决策表（忽略同目录其他数据文件），
    /// 返回成功加载的数量
    pub fn load_from_dir(&self, dir: &Path) -> Result<usize, Box<dyn std::error::Error>> {
        let mut count = 0;
        if !dir.exists() {
            tracing::warn!(dir = %dir.display(), "决策表目录不存在，跳过加载");
            return Ok(0);
        }
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            // 仅加载 JDM 决策表（`*_gec_attribution.json`），
            // 跳过同目录的非决策表数据文件（如 en_points.json / en_exercises.json）
            let is_table = path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.ends_with("_gec_attribution.json"))
                .unwrap_or(false);
            if !is_table {
                continue;
            }
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string();
            let content = std::fs::read_to_string(&path)?;
            match AttributionEngine::new(&content, &name) {
                Ok(engine) => {
                    let lang = name.strip_suffix(TABLE_SUFFIX).unwrap_or(&name).to_string();
                    let updated_at = std::fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .and_then(|d| {
                            chrono::DateTime::from_timestamp(d.as_secs() as i64, d.subsec_nanos())
                        })
                        .map(|dt| dt.to_rfc3339())
                        .unwrap_or_else(now_iso);
                    let meta = DecisionTableMeta {
                        lang: lang.clone(),
                        version: "1.0.0".into(),
                        updated_at,
                        rule_count: count_rules(&content),
                    };
                    self.engines
                        .write()
                        .unwrap()
                        .insert(name.clone(), Arc::new(engine));
                    self.raw.write().unwrap().insert(name.clone(), content);
                    self.meta.write().unwrap().insert(name, meta);
                    count += 1;
                }
                Err(e) => tracing::error!(table = %name, error = %e, "决策表加载失败"),
            }
        }
        Ok(count)
    }

    /// 按语言获取引擎（`zh` → `zh_gec_attribution`）
    pub fn get(&self, lang: &str) -> Option<Arc<AttributionEngine>> {
        self.engines.read().unwrap().get(&table_key(lang)).cloned()
    }

    /// 获取决策表原始 JSON
    pub fn get_raw(&self, lang: &str) -> Option<String> {
        self.raw.read().unwrap().get(&table_key(lang)).cloned()
    }

    /// 获取决策表真实元数据（规则数 + 加载/更新时间）
    pub fn get_meta(&self, lang: &str) -> Option<DecisionTableMeta> {
        self.meta.read().unwrap().get(&table_key(lang)).cloned()
    }

    /// 列出所有决策表元数据
    pub fn list(&self) -> Vec<DecisionTableMeta> {
        let meta = self.meta.read().unwrap();
        let mut metas: Vec<DecisionTableMeta> = meta.values().cloned().collect();
        metas.sort_by(|a, b| a.lang.cmp(&b.lang));
        metas
    }

    /// 热更新：校验通过后原子替换，失败时保留旧表
    pub fn update(
        &self,
        lang: &str,
        table_json: &str,
    ) -> Result<DecisionTableMeta, AttributionError> {
        AttributionEngine::validate(table_json)?;
        let name = table_key(lang);
        let engine = AttributionEngine::new(table_json, &name)?;
        self.engines
            .write()
            .unwrap()
            .insert(name.clone(), Arc::new(engine));
        self.raw
            .write()
            .unwrap()
            .insert(name.clone(), table_json.to_string());
        let meta = DecisionTableMeta {
            lang: lang.to_string(),
            version: "1.0.0".into(),
            updated_at: now_iso(),
            rule_count: count_rules(table_json),
        };
        self.meta.write().unwrap().insert(name, meta.clone());
        tracing::info!(lang = lang, "决策表已热更新");
        Ok(meta)
    }
}

/// 统计 JDM 决策表中的规则行数（解析失败返回 0）
fn count_rules(content: &str) -> usize {
    let v: serde_json::Value = match serde_json::from_str(content) {
        Ok(v) => v,
        Err(_) => return 0,
    };
    v.pointer("/nodes")
        .and_then(|n| n.as_array())
        .and_then(|nodes| {
            nodes
                .iter()
                .find(|n| n.get("type").and_then(|t| t.as_str()) == Some("decisionTableNode"))
        })
        .and_then(|n| n.pointer("/content/rules"))
        .and_then(|r| r.as_array())
        .map(|r| r.len())
        .unwrap_or(0)
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_and_update_zh_table() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../decision-tables");
        let store = TableStore::new();
        let n = store.load_from_dir(&dir).unwrap();
        assert!(n >= 3, "应加载 zh/en/ja 三张决策表，实际 {n}");
        assert!(store.get("zh").is_some());
        assert!(store.get_raw("zh").is_some());
        assert_eq!(store.list().len(), n);

        // 热更新：修改后仍可读取
        let mut raw = store.get_raw("zh").unwrap();
        raw = raw.replace("spelling_high_conf_old_hand", "spelling_hot_reloaded");
        let meta = store.update("zh", &raw).unwrap();
        assert_eq!(meta.lang, "zh");
        assert!(
            store
                .get_raw("zh")
                .unwrap()
                .contains("spelling_hot_reloaded")
        );
    }

    #[test]
    fn update_rejects_invalid_table() {
        let store = TableStore::new();
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../decision-tables");
        let _ = store.load_from_dir(&dir);
        let err = store.update("zh", "{ not valid json").unwrap_err();
        assert!(matches!(err, AttributionError::LoadFailed(_)));
    }
}
