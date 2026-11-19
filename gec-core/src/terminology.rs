//! 术语库（领域术语保护 + 强制更正词）
//!
//! 与 `custom_words`（运行时「放弃」反馈产生的词级白名单，API 层按 `original`
//! 相等过滤）互补：术语库是**部署级外置文件**
//! `{GEC_LEXICONS_DIR}/terminology_{lang}.json`（默认目录 `lexicons`），
//! 在引擎层生效（覆盖 api-server / 桌面端 / 媒体管线全部检测入口与全部来源层），
//! 按 **span 重叠**判定，不随重启丢失、不重新编译即可扩充。
//!
//! 文件格式（缺失或损坏时视为空库，不报错）：
//! ```json
//! {
//!   "protected": ["区块链", {"term": "JavaScript", "note": "大小写敏感术语"}],
//!   "forced": [{"wrong": "既使", "right": "即使", "note": "领域高频错词"}]
//! }
//! ```
//! - `protected`：白名单术语——错误 span 与术语任一出现位置重叠即**丢弃**
//!   （防止规则 / 模型把正确专业词误纠）；纯 ASCII 术语按大小写不敏感匹配；
//! - `forced`：黑名单错词——出现即产出强制更正建议（置信 0.98，来源 `rule`），
//!   出现位置被受保护术语覆盖时跳过。

use std::path::Path;

use async_trait::async_trait;
use serde::Deserialize;

use crate::detector::DetectorError;
use crate::engine::{DetectOutcome, GecEngine, GecEngineKind};
use crate::timings::NlpCall;
use shared_model::{Error, error_types, sources};

/// `protected` 条目：纯字符串或 `{term, note}`
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum ProtectEntry {
    Plain(String),
    Detailed {
        term: String,
        #[serde(default)]
        #[allow(dead_code)]
        note: String,
    },
}

impl ProtectEntry {
    fn term(&self) -> &str {
        match self {
            Self::Plain(s) => s,
            Self::Detailed { term, .. } => term,
        }
    }
}

/// `forced` 条目：出现即强制更正
#[derive(Debug, Clone, Deserialize)]
pub struct ForcedEntry {
    pub wrong: String,
    pub right: String,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct TerminologyFile {
    #[serde(default)]
    protected: Vec<ProtectEntry>,
    #[serde(default)]
    forced: Vec<ForcedEntry>,
}

/// 该语言的术语库（加载后不可变；随设置热更新重建引擎时一并重载）
#[derive(Debug, Clone, Default)]
pub struct Terminology {
    protected: Vec<String>,
    forced: Vec<ForcedEntry>,
}

impl Terminology {
    /// 从 `{dir}/terminology_{lang}.json` 加载；文件缺失/损坏返回空库（debug 日志）
    pub fn load(dir: impl AsRef<Path>, lang: &str) -> Self {
        let path = dir.as_ref().join(format!("terminology_{lang}.json"));
        match std::fs::read_to_string(&path) {
            Ok(s) => match serde_json::from_str::<TerminologyFile>(&s) {
                Ok(f) => Self {
                    protected: f
                        .protected
                        .into_iter()
                        .map(|e| e.term().to_string())
                        .filter(|t| !t.is_empty())
                        .collect(),
                    forced: f
                        .forced
                        .into_iter()
                        .filter(|e| !e.wrong.is_empty() && !e.right.is_empty())
                        .collect(),
                },
                Err(e) => {
                    tracing::debug!(path = %path.display(), error = %e, "术语库解析失败，视为空库");
                    Self::default()
                }
            },
            Err(_) => Self::default(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.protected.is_empty() && self.forced.is_empty()
    }

    /// 术语库目录（env `GEC_LEXICONS_DIR`，默认 `lexicons`——与各语言检测器同口径）
    pub fn dir_from_env() -> std::path::PathBuf {
        std::path::PathBuf::from(
            std::env::var("GEC_LEXICONS_DIR").unwrap_or_else(|_| "lexicons".to_string()),
        )
    }

    /// 受保护术语在文本中的全部出现位置（字节区间；ASCII 术语大小写不敏感）
    fn protected_occurrences(&self, text: &str) -> Vec<[usize; 2]> {
        let mut out = Vec::new();
        for term in &self.protected {
            out.extend(occurrences(text, term));
        }
        out.sort_by_key(|s| s[0]);
        out
    }

    /// 对错误列表应用术语库：
    /// ① 丢弃与受保护术语出现位置重叠的错误；
    /// ② 注入强制更正词建议（与既有错误重叠时按 [`crate::engine::merge_errors`]
    ///    置信度规则合并）。
    pub fn apply(&self, lang: &str, text: &str, errors: &mut Vec<Error>) {
        if self.is_empty() {
            return;
        }
        let occurrences = self.protected_occurrences(text);
        if !occurrences.is_empty() {
            let before = errors.len();
            errors.retain(|e| !overlaps_any(e.span, &occurrences));
            if errors.len() != before {
                tracing::debug!(
                    lang,
                    dropped = before - errors.len(),
                    "术语库保护：重叠错误已过滤"
                );
            }
        }
        let mut forced: Vec<Error> = Vec::new();
        for f in &self.forced {
            for occ in occurrences_of(text, &f.wrong) {
                if overlaps_any(occ, &occurrences) {
                    continue;
                }
                let mut e = Error::new(
                    lang,
                    error_types::SPELLING,
                    0.98,
                    occ,
                    &f.wrong,
                    Some(f.right.clone()),
                    if f.note.is_empty() {
                        format!("术语库强制更正：{} → {}", f.wrong, f.right)
                    } else {
                        f.note.clone()
                    },
                );
                e.source = Some(sources::RULE.to_string());
                e.rule_id = Some("terminology".to_string());
                e.category = Some("terminology".to_string());
                forced.push(e);
            }
        }
        crate::engine::merge_errors(errors, forced);
    }
}

/// `needle` 在 `haystack` 中的全部出现位置（字节区间，不重叠推进）。
/// 纯 ASCII needle 按大小写不敏感匹配（`to_ascii_lowercase` 保持字节长度，
/// 区间与原文字节偏移一致）；含非 ASCII 时按原文精确匹配。
pub(crate) fn occurrences(haystack: &str, needle: &str) -> Vec<[usize; 2]> {
    if needle.is_empty() {
        return Vec::new();
    }
    if needle.is_ascii() {
        let hay = haystack.as_bytes().to_ascii_lowercase();
        let ndl = needle.as_bytes().to_ascii_lowercase();
        find_in_bytes(&hay, &ndl)
    } else {
        find_in_bytes(haystack.as_bytes(), needle.as_bytes())
    }
}

fn occurrences_of(haystack: &str, needle: &str) -> Vec<[usize; 2]> {
    occurrences(haystack, needle)
}

fn find_in_bytes(hay: &[u8], ndl: &[u8]) -> Vec<[usize; 2]> {
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(pos) = find_subslice(hay, ndl, from) {
        out.push([pos, pos + ndl.len()]);
        from = pos + ndl.len();
    }
    out
}

fn find_subslice(hay: &[u8], ndl: &[u8], from: usize) -> Option<usize> {
    if from >= hay.len() || ndl.len() > hay.len() {
        return None;
    }
    hay[from..]
        .windows(ndl.len())
        .position(|w| w == ndl)
        .map(|p| from + p)
}

pub(crate) fn overlaps_any(span: [usize; 2], ranges: &[[usize; 2]]) -> bool {
    ranges.iter().any(|r| span[0] < r[1] && r[0] < span[1])
}

/// 术语库守卫引擎：包装任意引擎，在其全部产出（含 LLM 兜底合并后）上应用术语库。
/// 用于不经 [`crate::engine::CascadeDetector`] 的挂载形态（纯规则默认路径、
/// 统计 GEC 组合形态）；级联形态直接持有 [`Terminology`] 字段，不经此包装
/// （保留 `decision_engine()` 转发）。
pub struct TerminologyEngine {
    inner: std::sync::Arc<dyn GecEngine>,
    terminology: std::sync::Arc<Terminology>,
}

impl TerminologyEngine {
    pub fn new(
        inner: std::sync::Arc<dyn GecEngine>,
        terminology: std::sync::Arc<Terminology>,
    ) -> Self {
        Self { inner, terminology }
    }
}

#[async_trait]
impl GecEngine for TerminologyEngine {
    fn kind(&self) -> GecEngineKind {
        self.inner.kind()
    }
    fn name(&self) -> &str {
        self.inner.name()
    }
    async fn detect(&self, text: &str) -> Result<Vec<Error>, DetectorError> {
        let lang = self.inner.name().to_string();
        let mut errors = self.inner.detect(text).await?;
        self.terminology.apply(&lang, text, &mut errors);
        Ok(errors)
    }
    async fn detect_level(
        &self,
        text: &str,
        level: u8,
        force_llm: bool,
        llm: Option<&crate::llm::LlmGecEngine>,
    ) -> Result<DetectOutcome, DetectorError> {
        let lang = self.inner.name().to_string();
        let mut outcome = self.inner.detect_level(text, level, force_llm, llm).await?;
        self.terminology.apply(&lang, text, &mut outcome.errors);
        Ok(outcome)
    }
    /// 运行态与阶段事件透传：内层引擎把预算窗覆盖写入 `run`，
    /// 术语过滤在其后（保护词剔除不影响覆盖核算）。
    async fn detect_traced(
        &self,
        text: &str,
        sink: Option<crate::progress::StageSink>,
        run: &mut crate::quality::RunReport,
    ) -> Result<Vec<Error>, DetectorError> {
        let lang = self.inner.name().to_string();
        let mut errors = self.inner.detect_traced(text, sink, run).await?;
        self.terminology.apply(&lang, text, &mut errors);
        Ok(errors)
    }
    /// 阶段事件透传：不过滤事件直接交给内层引擎（术语处理本身在 End 之后，不单独设阶段）
    async fn detect_level_traced(
        &self,
        text: &str,
        level: u8,
        force_llm: bool,
        llm: Option<&crate::llm::LlmGecEngine>,
        progress: Option<crate::progress::StageSink>,
    ) -> Result<DetectOutcome, DetectorError> {
        let lang = self.inner.name().to_string();
        let mut outcome = self
            .inner
            .detect_level_traced(text, level, force_llm, llm, progress)
            .await?;
        self.terminology.apply(&lang, text, &mut outcome.errors);
        Ok(outcome)
    }
    fn last_nlp_call(&self) -> Option<NlpCall> {
        self.inner.last_nlp_call()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn termbase(protected: &[&str], forced: &[(&str, &str)]) -> Terminology {
        Terminology {
            protected: protected.iter().map(|s| s.to_string()).collect(),
            forced: forced
                .iter()
                .map(|(w, r)| ForcedEntry {
                    wrong: w.to_string(),
                    right: r.to_string(),
                    note: String::new(),
                })
                .collect(),
        }
    }

    fn err(lang: &str, span: [usize; 2], original: &str) -> Error {
        Error::new(
            lang,
            error_types::SPELLING,
            0.9,
            span,
            original,
            None,
            "test",
        )
    }

    #[test]
    fn protected_term_suppresses_overlapping_errors() {
        let text = "区块链技术创新";
        // 「区块链」字节区间 [0, 9)
        let t = termbase(&["区块链"], &[]);
        let mut errors = vec![err("zh", [0, 9], "区块链"), err("zh", [9, 12], "技")];
        t.apply("zh", text, &mut errors);
        assert_eq!(errors.len(), 1, "受保护术语上的错误应被过滤");
        assert_eq!(errors[0].original, "技");
    }

    #[test]
    fn ascii_protected_term_is_case_insensitive() {
        let text = "write JavaScript code";
        let t = termbase(&["javascript"], &[]);
        let mut errors = vec![err("en", [6, 16], "Javascript")];
        t.apply("en", text, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn forced_terms_inject_corrections_and_respect_protection() {
        let text = "既使区块链也既使不了";
        let t = termbase(&[], &[("既使", "即使")]);
        let mut errors: Vec<Error> = Vec::new();
        t.apply("zh", text, &mut errors);
        assert_eq!(errors.len(), 2, "两处「既使」应各产出一条强制更正");
        for e in &errors {
            assert_eq!(e.correction.as_deref(), Some("即使"));
            assert_eq!(e.rule_id.as_deref(), Some("terminology"));
        }
        // 强制词被受保护术语覆盖时跳过
        let t2 = termbase(&["既使"], &[("既使", "即使")]);
        let mut errors2: Vec<Error> = Vec::new();
        t2.apply("zh", text, &mut errors2);
        assert!(errors2.is_empty(), "受保护术语覆盖强制词时不产出");
    }

    #[test]
    fn missing_file_is_empty_noop() {
        let t = Terminology::load("/nonexistent-dir-xyz", "zh");
        assert!(t.is_empty());
        let mut errors = vec![err("zh", [0, 3], "测试")];
        t.apply("zh", "测试", &mut errors);
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn loads_full_file_format() {
        let dir = std::env::temp_dir().join(format!("lf_term_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("terminology_zh.json"),
            r#"{"protected":["区块链",{"term":"向量数据库","note":"术语"}],"forced":[{"wrong":"既使","right":"即使"}]}"#,
        )
        .unwrap();
        let t = Terminology::load(&dir, "zh");
        assert_eq!(t.protected.len(), 2);
        assert_eq!(t.forced.len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }
}
