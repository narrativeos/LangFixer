//! 知识库存储：知识源注册表 + 知识点集合（按语言）。
//!
//! 与决策表（[`crate::TableStore`]）分离：
//! - 决策表 = 可提取的规则（可随产品发布）；
//! - 知识库 = 知识内容 + 知识源关联（涉及知识产权，**不随产品内置发布**，
//!   仅在部署环境提供 `GEC_KNOWLEDGE_DIR` 目录时加载；缺失时归因结果
//!   优雅降级为纯表级结果）。
//!
//! 目录布局：
//! ```text
//! knowledge-base/
//! ├── sources.json        # Vec<KnowledgeSource>（知识源注册表，仅元数据）
//! └── points/{lang}.json  # Vec<KnowledgePoint>（知识点；每个点带 sources 关联，1:N）
//! ```

use std::collections::HashMap;
use std::path::Path;
use std::sync::RwLock;

use shared_model::{
    AttributionContext, AttributionResult, KnowledgePoint, KnowledgeSource, KnowledgeSourceRef,
};

/// 知识库存储（进程内；支持目录批量加载与热更新）
#[derive(Default)]
pub struct KnowledgeStore {
    /// source_id → 源元数据
    sources: RwLock<HashMap<String, KnowledgeSource>>,
    /// lang → 知识点列表
    points: RwLock<HashMap<String, Vec<KnowledgePoint>>>,
}

impl KnowledgeStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 从目录加载 `sources.json` + `points/*.json`，返回知识点总数
    pub fn load_from_dir(&self, dir: &Path) -> Result<usize, Box<dyn std::error::Error>> {
        if !dir.exists() {
            tracing::info!(dir = %dir.display(), "知识库目录不存在，跳过加载（归因结果将不含知识源富化）");
            return Ok(0);
        }
        let mut total = 0;
        let sources_path = dir.join("sources.json");
        if sources_path.exists() {
            let content = std::fs::read_to_string(&sources_path)?;
            self.update_sources(&content)?;
            tracing::info!(
                count = self.sources.read().unwrap().len(),
                "知识源注册表已加载"
            );
        }
        let points_dir = dir.join("points");
        if points_dir.exists() {
            for entry in std::fs::read_dir(&points_dir)? {
                let path = entry?.path();
                let is_points = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.ends_with(".json"))
                    .unwrap_or(false);
                if !is_points {
                    continue;
                }
                let lang = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown")
                    .to_string();
                let content = std::fs::read_to_string(&path)?;
                let n = self.update_points(&lang, &content)?;
                total += n;
                tracing::info!(lang = %lang, count = n, "知识点集合已加载");
            }
        }
        Ok(total)
    }

    /// 知识源列表（含关联知识点数统计；按 source_id 排序）
    pub fn sources(&self) -> Vec<KnowledgeSource> {
        let mut out: Vec<KnowledgeSource> =
            self.sources.read().unwrap().values().cloned().collect();
        let points = self.points.read().unwrap();
        for s in &mut out {
            s.point_count = points
                .get(&s.lang)
                .map(|ps| {
                    ps.iter()
                        .filter(|p| p.sources.iter().any(|r| &r.source_id == &s.source_id))
                        .count()
                })
                .unwrap_or(0);
        }
        out.sort_by(|a, b| a.source_id.cmp(&b.source_id));
        out
    }

    /// 某语言的知识点列表（可按知识源过滤；无该语言集合时返回空）
    pub fn points(&self, lang: &str, source_id: Option<&str>) -> Vec<KnowledgePoint> {
        let points = self.points.read().unwrap();
        let Some(list) = points.get(lang) else {
            return Vec::new();
        };
        let mut out: Vec<KnowledgePoint> = list.clone();
        if let Some(sid) = source_id {
            out.retain(|p| p.sources.iter().any(|r| r.source_id == sid));
        }
        out
    }

    /// 按点 ID 查询知识点
    pub fn get_point(&self, lang: &str, point_id: &str) -> Option<KnowledgePoint> {
        self.points
            .read()
            .unwrap()
            .get(lang)
            .and_then(|ps| ps.iter().find(|p| &p.point_id == point_id).cloned())
    }

    /// 已加载知识点集合的语言列表（排序；数据集按语言盘点/导出用）
    pub fn langs(&self) -> Vec<String> {
        let points = self.points.read().unwrap();
        let mut langs: Vec<String> = points.keys().cloned().collect();
        langs.sort();
        langs
    }

    /// 解析知识源引用：从注册表回填 `title`（未知源保留原样，展示端回退 source_id）
    pub fn resolve(&self, refs: &[KnowledgeSourceRef]) -> Vec<KnowledgeSourceRef> {
        let sources = self.sources.read().unwrap();
        refs.iter()
            .map(|r| {
                let title = sources
                    .get(&r.source_id)
                    .map(|s| s.title.clone())
                    .unwrap_or_default();
                KnowledgeSourceRef {
                    source_id: r.source_id.clone(),
                    location: r.location.clone(),
                    title,
                }
            })
            .collect()
    }

    /// 按 `grammar_point` 用知识点富化归因结果（知识库缺失 / 点不存在时原样返回）：
    /// - `remedial`：点级补救内容覆盖表级通用建议（点带例句时附 "｜ 例：…"）
    /// - `cefr`：点级 CEFR 覆盖
    /// - `examples`：点的改错对（`✗ wrong → ✓ right`）；无改错对时回退点例句
    /// - `sources`：点 → 知识源关联（1:N，title 已解析）
    pub fn enrich(&self, ctx: &AttributionContext, result: &mut AttributionResult) {
        let Some(point_id) = ctx.grammar_point.clone() else {
            return;
        };
        let Some(point) = self.get_point(&ctx.lang, &point_id) else {
            return;
        };
        if !point.remedial.is_empty() {
            let mut remedial = point.remedial.clone();
            if !point.examples.is_empty() {
                remedial = format!("{remedial} ｜ 例：{}", point.examples.join(" "));
            }
            result.remedial = vec![remedial];
        }
        if !point.cefr.is_empty() {
            result.cefr = point.cefr.clone();
        }
        if !point.corrections.is_empty() {
            result.examples = point
                .corrections
                .iter()
                .map(|c| format!("✗ {} → ✓ {}", c.wrong, c.right))
                .collect();
        } else if !point.examples.is_empty() {
            result.examples = point.examples.clone();
        }
        result.sources = self.resolve(&point.sources);
    }

    /// 热更新知识源注册表（校验：source_id 非空且唯一；成功后原子替换）
    pub fn update_sources(&self, json: &str) -> Result<Vec<KnowledgeSource>, String> {
        let sources: Vec<KnowledgeSource> =
            serde_json::from_str(json).map_err(|e| format!("JSON 解析失败: {e}"))?;
        let mut seen = std::collections::HashSet::new();
        for s in &sources {
            if s.source_id.is_empty() {
                return Err("知识源 source_id 不能为空".into());
            }
            if !seen.insert(s.source_id.clone()) {
                return Err(format!("知识源 source_id 重复: {}", s.source_id));
            }
        }
        let map: HashMap<String, KnowledgeSource> = sources
            .iter()
            .cloned()
            .map(|s| (s.source_id.clone(), s))
            .collect();
        *self.sources.write().unwrap() = map;
        Ok(sources)
    }

    /// 热更新某语言的知识点集合（校验：point_id 非空且语言内唯一；成功后原子替换）
    pub fn update_points(&self, lang: &str, json: &str) -> Result<usize, String> {
        let points: Vec<KnowledgePoint> =
            serde_json::from_str(json).map_err(|e| format!("JSON 解析失败: {e}"))?;
        let mut seen = std::collections::HashSet::new();
        for p in &points {
            if p.point_id.is_empty() {
                return Err("知识点 point_id 不能为空".into());
            }
            if !seen.insert(p.point_id.clone()) {
                return Err(format!("知识点 point_id 重复: {}", p.point_id));
            }
        }
        self.points
            .write()
            .unwrap()
            .insert(lang.to_string(), points.clone());
        Ok(points.len())
    }

    /// 把当前内存态写回目录（`sources.json` + `points/{lang}.json`）。
    ///
    /// 发布 / 热更新后调用——**热更新只有写回才跨重启存活**（否则重启回到
    /// 旧文件）。原子写（临时文件 + rename），目录不存在自动创建。
    pub fn save_to_dir(&self, dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(dir.join("points"))
            .map_err(|e| format!("创建目录失败 {}: {e}", dir.display()))?;
        let sources_guard = self.sources.read().unwrap();
        let mut sources: Vec<&KnowledgeSource> = sources_guard.values().collect();
        sources.sort_by(|a, b| a.source_id.cmp(&b.source_id));
        let sources_json =
            serde_json::to_string_pretty(&sources).map_err(|e| format!("序列化知识源失败: {e}"))?;
        atomic_write(&dir.join("sources.json"), &sources_json)?;
        let points = self.points.read().unwrap();
        for (lang, list) in points.iter() {
            let json = serde_json::to_string_pretty(list)
                .map_err(|e| format!("序列化知识点失败 ({lang}): {e}"))?;
            atomic_write(&dir.join("points").join(format!("{lang}.json")), &json)?;
        }
        Ok(())
    }
}

/// 原子写文件：先写 `{name}.tmp` 再 rename（崩溃不留半截文件）
fn atomic_write(path: &Path, content: &str) -> Result<(), String> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, content).map_err(|e| format!("写入失败 {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("替换失败 {}: {e}", path.display()))?;
    Ok(())
}
/// 数据驱动的点级归点（英文）：已发布点的 `corrections[]` 与
/// `(wrong → right)` 精确一致（忽略大小写/首尾空白）即命中该点；
/// 未命中时回退调用方给定的静态映射（`gec_en::classify_point`）。
///
/// 意义：新导入并发布的知识源**自动参与归点**——无需改代码，
/// 书中改错对即归点候选；点 ID 仍由知识库数据决定（不随代码硬编码漂移）。
pub fn classify_point_en(
    store: &KnowledgeStore,
    error_type: &str,
    original: &str,
    correction: &str,
    fallback: Option<String>,
) -> Option<String> {
    let o = original.trim();
    let c = correction.trim();
    if !o.is_empty() && !c.is_empty() {
        if let Some(p) = store.points("en", None).into_iter().find(|p| {
            p.error_type == error_type
                && p.corrections.iter().any(|x| {
                    x.wrong.trim().eq_ignore_ascii_case(o) && x.right.trim().eq_ignore_ascii_case(c)
                })
        }) {
            return Some(p.point_id);
        }
    }
    fallback
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kb_dir() -> std::path::PathBuf {
        let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        p.push("../knowledge-base");
        p
    }

    fn load_real() -> KnowledgeStore {
        let store = KnowledgeStore::new();
        store.load_from_dir(&kb_dir()).unwrap();
        store
    }

    fn base_result() -> AttributionResult {
        AttributionResult {
            attribution: "knowledge_gap".into(),
            weiner: shared_model::WeinerDimensions::new("internal", "unstable", "controllable"),
            remedial: vec!["表级通用建议".into()],
            trace: shared_model::AttributionTrace {
                matched_rule: "article_novice".into(),
                decision_table: "en_gec_attribution".into(),
            },
            cefr: "A2".into(),
            source: String::new(),
            sources: Vec::new(),
            examples: Vec::new(),
        }
    }

    fn ctx_with(point: Option<&str>) -> AttributionContext {
        AttributionContext {
            lang: "en".into(),
            error_type: "article".into(),
            confidence: 0.9,
            session_error_count: 0,
            session_meta: None,
            grammar_point: point.map(|s| s.to_string()),
        }
    }

    #[test]
    fn load_real_kb() {
        let store = load_real();
        let sources = store.sources();
        assert!(!sources.is_empty(), "应至少登记 1 个知识源");
        let en = sources
            .iter()
            .find(|s| s.lang == "en")
            .expect("应有 en 知识源");
        assert!(
            en.point_count > 200,
            "en 知识源应关联足量示例知识点，实际 {}",
            en.point_count
        );
        assert!(
            store.points("en", None).len() > 200,
            "en 知识点集合应完整加载"
        );
    }

    #[test]
    fn enrich_article_point_with_sources() {
        let store = load_real();
        let mut result = base_result();
        store.enrich(&ctx_with(Some("article.3")), &mut result);
        assert_eq!(result.sources.len(), 1, "article.3 应关联 1 个知识源");
        assert!(
            result.sources[0].title.contains("示例英语语法指南"),
            "title 应从注册表解析: {:?}",
            result.sources[0].title
        );
        assert!(
            result.sources[0].location.contains("二.1"),
            "location 应含小节号: {:?}",
            result.sources[0].location
        );
        assert!(
            !result.remedial[0].contains("表级通用建议"),
            "remedial 应被点级内容覆盖"
        );
        assert!(
            result
                .examples
                .iter()
                .any(|e| e.contains("a ancient source")),
            "examples 应含该点改错对: {:?}",
            result.examples
        );
    }

    #[test]
    fn enrich_noop_without_point() {
        let store = load_real();
        let mut result = base_result();
        store.enrich(&ctx_with(None), &mut result);
        assert!(result.sources.is_empty());
        assert_eq!(result.remedial, vec!["表级通用建议".to_string()]);
    }

    #[test]
    fn enrich_unknown_point_noop() {
        let store = load_real();
        let mut result = base_result();
        store.enrich(&ctx_with(Some("article.9999")), &mut result);
        assert!(result.sources.is_empty());
        assert_eq!(result.remedial, vec!["表级通用建议".to_string()]);
    }

    #[test]
    fn multi_source_point_resolves_all_refs() {
        // 同一知识点被两个知识源背书（1:N 关联）
        let store = KnowledgeStore::new();
        store
            .update_sources(
                r#"[
                    { "source_id": "src-a", "title": "教材 A", "lang": "en" },
                    { "source_id": "src-b", "title": "教材 B", "lang": "en" }
                ]"#,
            )
            .unwrap();
        store
            .update_points(
                "en",
                r#"[
                    {
                        "point_id": "noun.1",
                        "lang": "en",
                        "error_type": "noun",
                        "title": "可数与不可数",
                        "cefr": "A2",
                        "remedial": "规则讲解：可数/不可数名词",
                        "sources": [
                            { "source_id": "src-a", "location": "第一章 · 1.1" },
                            { "source_id": "src-b", "location": "第三章 · 2.4" }
                        ]
                    }
                ]"#,
            )
            .unwrap();
        let mut ctx = ctx_with(Some("noun.1"));
        ctx.error_type = "noun".into();
        let mut result = base_result();
        store.enrich(&ctx, &mut result);
        assert_eq!(result.sources.len(), 2, "同一规则/点可映射多个知识源");
        assert_eq!(result.sources[0].title, "教材 A");
        assert_eq!(result.sources[1].title, "教材 B");
        assert_eq!(store.points("en", Some("src-a")).len(), 1);
        assert_eq!(store.points("en", Some("src-unknown")).len(), 0);
        let sources = store.sources();
        assert_eq!(
            sources
                .iter()
                .find(|s| s.source_id == "src-a")
                .unwrap()
                .point_count,
            1
        );
    }

    #[test]
    fn classify_point_en_prefers_published_data() {
        let store = KnowledgeStore::new();
        store
            .update_points(
                "en",
                r#"[{ "point_id": "subjunctive.9", "lang": "en", "error_type": "subjunctive",
                      "title": "wish 后虚拟语气（新书）",
                      "corrections": [{ "wrong": "I was", "right": "I were" }] }]"#,
            )
            .unwrap();
        // 已发布点改错对精确命中（忽略大小写/首尾空白）→ 优先数据点
        assert_eq!(
            classify_point_en(
                &store,
                "subjunctive",
                " i was ",
                "I WERE ",
                Some("subjunctive.8".into())
            )
            .as_deref(),
            Some("subjunctive.9")
        );
        // 未命中 → 回退静态映射
        assert_eq!(
            classify_point_en(
                &store,
                "subjunctive",
                "was",
                "were",
                Some("subjunctive.8".into())
            )
            .as_deref(),
            Some("subjunctive.8")
        );
        // error_type 不一致不命中
        assert_eq!(
            classify_point_en(&store, "tense", "I was", "I were", None).as_deref(),
            None
        );
    }

    #[test]
    fn save_to_dir_roundtrip_survives_reload() {
        // 发布/热更新 → 写回目录 → 重新加载（模拟重启）→ 数据与归点均存活
        let dir = std::env::temp_dir().join(format!(
            "langfixer_kb_save_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = KnowledgeStore::new();
        store
            .update_sources(
                r#"[{ "source_id": "src-new", "title": "新导入书", "lang": "en", "license_scope": "public" }]"#,
            )
            .unwrap();
        store
            .update_points(
                "en",
                r#"[{ "point_id": "article.7", "lang": "en", "error_type": "article",
                      "title": "不可数名词限量（新导入）",
                      "corrections": [{ "wrong": "a informations", "right": "much information" }],
                      "sources": [{ "source_id": "src-new", "location": "第一章 · 1.2" }] }]"#,
            )
            .unwrap();
        store.save_to_dir(&dir).unwrap();
        // 落盘文件存在且无残留临时文件
        assert!(dir.join("sources.json").exists());
        assert!(dir.join("points/en.json").exists());
        assert!(!dir.join("points/en.json.tmp").exists());

        let reloaded = KnowledgeStore::new();
        reloaded.load_from_dir(&dir).unwrap();
        assert!(reloaded.get_point("en", "article.7").is_some());
        assert_eq!(reloaded.sources()[0].source_id, "src-new");
        assert_eq!(
            classify_point_en(
                &reloaded,
                "article",
                "a informations",
                "much information",
                None
            )
            .as_deref(),
            Some("article.7"),
            "写回后重启，数据驱动归点仍命中新发布点"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn update_rejects_invalid() {
        let store = KnowledgeStore::new();
        assert!(store.update_sources("{ bad").is_err());
        assert!(
            store
                .update_sources(r#"[{ "source_id": "", "title": "x", "lang": "en" }]"#)
                .is_err()
        );
        assert!(
            store
                .update_sources(
                    r#"[
                    { "source_id": "s", "title": "a", "lang": "en" },
                    { "source_id": "s", "title": "b", "lang": "en" }
                ]"#
                )
                .is_err()
        );
        assert!(
            store
                .update_points(
                    "en",
                    r#"[
                    { "point_id": "p", "lang": "en", "error_type": "t", "title": "x" },
                    { "point_id": "p", "lang": "en", "error_type": "t", "title": "y" }
                ]"#
                )
                .is_err()
        );
    }
}
