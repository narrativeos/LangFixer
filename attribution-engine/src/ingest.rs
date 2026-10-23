//! 知识源导入管线核心（书 → 数据集的一键导入闭环，P1）
//!
//! 把「一本书从人工提炼到入库」产品化为可重复执行的管线：
//!
//! ```text
//! ① 注册知识源（元数据 + license_scope）        → open_session
//! ② 书本正文分块                                → chunk_text
//! ③ agent 逐块抽取草稿（点/例句/改错对）          → stage（草稿区，逐条校验）
//! ④ 归并对齐（确定性）：既有点背书(1:N) / 造新点   → plan_merge
//! ⑤ 质量门 + 人工一键发布（热更新知识库）          → apply_plan
//! ```
//!
//! 设计约束（与知识库既有原则一致，见 [`crate::knowledge`]）：
//! - **点 ID 不随书变**：草稿优先匹配既有点（agent 显式 `match_point_id`
//!   或确定性标题/小节匹配），命中只追加 `sources[]` 引用（1:N 背书）；
//!   只有真正新概念才造新点（`{error_type}.{max+1}`）；
//! - **LLM 只做抽取，代码做校验/归并/ID 分配**：草稿逐条 schema 校验，
//!   非法条目拒收并回报原因，防幻觉污染数据集；
//! - **草稿区（staging）与生效知识库分离**：发布前不影响运行时归因；
//!   发布 = `plan_merge + apply_plan`（经 [`KnowledgeStore`] 热更新通道，
//!   校验失败保留旧数据）。

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::RwLock;

use serde::{Deserialize, Serialize};

use shared_model::{CorrectionPair, KnowledgePoint, KnowledgeSource, KnowledgeSourceRef};

use crate::knowledge::KnowledgeStore;

/// CEFR 合法值（草稿校验用）
const CEFR_LEVELS: [&str; 6] = ["A1", "A2", "B1", "B2", "C1", "C2"];

/// 草稿区单会话错误记录上限
const MAX_SESSION_ERRORS: usize = 20;

/// 会话状态：挖掘进行中
pub const SESSION_RUNNING: &str = "running";
/// 会话状态：全部块挖掘完成（等待人工审阅发布）
pub const SESSION_MINED: &str = "mined";
/// 会话状态：熔断终止（连续块失败 / 重启中断），需放弃或重新导入
pub const SESSION_FAILED: &str = "failed";

/// 连续块失败熔断阈值（重试后仍失败的连续块数）
pub const MAX_CONSECUTIVE_FAILURES: usize = 3;

fn default_session_status() -> String {
    SESSION_RUNNING.to_string()
}

// ---------------------------------------------------------------------------
// 草稿（agent 抽取产物，尚未分配 point_id）
// ---------------------------------------------------------------------------

/// 抽取草稿点：与 [`KnowledgePoint`] 同构但**不含 point_id**（发布时归并分配），
/// 并携带对齐信息（`match_point_id` / `location`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DraftPoint {
    /// 错误类型（对应决策表 errorType 分类法，如 article / tense）
    pub error_type: String,
    /// 点标题（书中规则表述）
    pub title: String,
    /// 小节（如 "一.1"）
    #[serde(default)]
    pub section: String,
    /// 补救讲解
    #[serde(default)]
    pub remedial: String,
    /// CEFR 难度（A1-C2；空 = 未判定，富化时回退表级）
    #[serde(default)]
    pub cefr: String,
    /// 例句
    #[serde(default)]
    pub examples: Vec<String>,
    /// 改错对（wrong → right）
    #[serde(default)]
    pub corrections: Vec<CorrectionPair>,
    /// 源内位置（发布时写入 sources[].location；缺省用 "section title" 兜底）
    #[serde(default)]
    pub location: String,
    /// agent 指定的既有匹配点（如 `article.3`）：存在则只追加背书，不造新点
    #[serde(default)]
    pub match_point_id: String,
}

/// 草稿校验（确定性质量门）：非法草稿拒收并回报原因。
pub fn validate_draft(draft: &DraftPoint, lang: &str) -> Result<(), String> {
    if draft.error_type.trim().is_empty() {
        return Err("error_type 不能为空".into());
    }
    if !draft
        .error_type
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return Err(format!(
            "error_type 只允许小写字母/数字/下划线: {}",
            draft.error_type
        ));
    }
    if draft.title.trim().is_empty() {
        return Err("title 不能为空".into());
    }
    if draft.title.chars().count() > 1000 {
        return Err("title 过长（> 1000 字符）".into());
    }
    if draft.remedial.chars().count() > 4000 {
        return Err("remedial 过长（> 4000 字符）".into());
    }
    if !draft.cefr.is_empty() && !CEFR_LEVELS.contains(&draft.cefr.as_str()) {
        return Err(format!("cefr 非法（应为 A1-C2 之一）: {}", draft.cefr));
    }
    if draft.examples.len() > 100 {
        return Err(format!("examples 过多（> 100）: {}", draft.examples.len()));
    }
    if draft.corrections.len() > 200 {
        return Err(format!(
            "corrections 过多（> 200）: {}",
            draft.corrections.len()
        ));
    }
    for c in &draft.corrections {
        if c.wrong.trim().is_empty() && c.right.trim().is_empty() {
            return Err("改错对 wrong / right 不能同时为空".into());
        }
    }
    if lang.trim().is_empty() {
        return Err("lang 不能为空".into());
    }
    Ok(())
}

/// 已入草稿区的草稿（带序号与入区时间）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StagedDraft {
    pub seq: u64,
    pub draft: DraftPoint,
    pub staged_at: String,
}

/// 导入会话（一个知识源一条）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestSession {
    pub source: KnowledgeSource,
    pub chunks_total: usize,
    pub chunks_done: usize,
    pub chunks_failed: usize,
    pub created_at: String,
    pub drafts: Vec<StagedDraft>,
    /// 抽取/分块过程的错误记录（上限 MAX_SESSION_ERRORS 条）
    pub errors: Vec<String>,
    /// 会话状态：running / mined / failed（见 SESSION_*）
    #[serde(default = "default_session_status")]
    pub status: String,
    /// 连续失败块数（重试后仍失败计 1，成功清零；达阈值熔断）
    #[serde(default)]
    pub consecutive_failures: usize,
    /// 归因规则草稿（决策表行候选；发布时随知识点一并应用）
    #[serde(default)]
    pub rule_drafts: Vec<StagedRuleDraft>,
    /// 规则派生状态："" 未派生 / running / mined / failed
    #[serde(default)]
    pub rules_status: String,
    /// 规则派生批次进度（按 error_type 分批）
    #[serde(default)]
    pub rules_total: usize,
    #[serde(default)]
    pub rules_done: usize,
    /// 规则派生错误记录
    #[serde(default)]
    pub rules_errors: Vec<String>,
}

/// 会话摘要（列表展示）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestSummary {
    pub source_id: String,
    pub title: String,
    pub lang: String,
    pub chunks_total: usize,
    pub chunks_done: usize,
    pub chunks_failed: usize,
    pub staged_points: usize,
    pub error_count: usize,
    pub created_at: String,
    pub status: String,
    /// 规则草稿数（派生规则候选；0 = 未派生）
    #[serde(default)]
    pub rule_drafts: usize,
    /// 规则派生状态（"" / running / mined / failed）
    #[serde(default)]
    pub rules_status: String,
}

/// stage 回报（逐条校验结果）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageReport {
    pub accepted: usize,
    /// 与本会话已入区草稿重复（error_type + 归一化标题相同）
    pub duplicates: usize,
    pub rejected: usize,
    /// 被拒草稿的错误原因（含序号定位）
    pub errors: Vec<String>,
    /// 会话当前草稿总数
    pub total_staged: usize,
}

impl From<&IngestSession> for IngestSummary {
    fn from(s: &IngestSession) -> Self {
        Self {
            source_id: s.source.source_id.clone(),
            title: s.source.title.clone(),
            lang: s.source.lang.clone(),
            chunks_total: s.chunks_total,
            chunks_done: s.chunks_done,
            chunks_failed: s.chunks_failed,
            staged_points: s.drafts.len(),
            error_count: s.errors.len(),
            created_at: s.created_at.clone(),
            status: s.status.clone(),
            rule_drafts: s.rule_drafts.len(),
            rules_status: s.rules_status.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// 草稿区存储（进程内 + 目录持久化；与生效知识库分离）
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct StagingStore {
    sessions: RwLock<HashMap<String, IngestSession>>,
}

impl StagingStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 开启导入会话（source_id 非空且当前无同名会话）
    pub fn open_session(
        &self,
        source: KnowledgeSource,
        chunks_total: usize,
    ) -> Result<IngestSummary, String> {
        if source.source_id.trim().is_empty() {
            return Err("source_id 不能为空".into());
        }
        if source.title.trim().is_empty() {
            return Err("知识源 title 不能为空".into());
        }
        if source.lang.trim().is_empty() {
            return Err("知识源 lang 不能为空".into());
        }
        let mut sessions = self.sessions.write().unwrap();
        if sessions.contains_key(&source.source_id) {
            return Err(format!(
                "导入会话已存在（先发布或放弃）: {}",
                source.source_id
            ));
        }
        let session = IngestSession {
            source,
            chunks_total,
            chunks_done: 0,
            chunks_failed: 0,
            created_at: chrono::Utc::now().to_rfc3339(),
            drafts: Vec::new(),
            errors: Vec::new(),
            status: SESSION_RUNNING.to_string(),
            consecutive_failures: 0,
            rule_drafts: Vec::new(),
            rules_status: String::new(),
            rules_total: 0,
            rules_done: 0,
            rules_errors: Vec::new(),
        };
        let summary = IngestSummary::from(&session);
        sessions.insert(summary.source_id.clone(), session);
        Ok(summary)
    }

    pub fn session(&self, source_id: &str) -> Option<IngestSession> {
        self.sessions.read().unwrap().get(source_id).cloned()
    }

    pub fn list(&self) -> Vec<IngestSummary> {
        let mut out: Vec<IngestSummary> = self
            .sessions
            .read()
            .unwrap()
            .values()
            .map(IngestSummary::from)
            .collect();
        out.sort_by(|a, b| a.source_id.cmp(&b.source_id));
        out
    }

    /// 一块抽取结束（成功或失败）；失败记错误（截留上限内）并参与熔断。
    /// 返回 `true` = 会话仍应继续挖掘（`false` = 已熔断，调用方应停止）。
    pub fn mark_chunk_done(&self, source_id: &str, error: Option<String>) -> Result<bool, String> {
        let mut sessions = self.sessions.write().unwrap();
        let session = sessions
            .get_mut(source_id)
            .ok_or_else(|| format!("导入会话不存在: {source_id}"))?;
        session.chunks_done += 1;
        match error {
            Some(e) => {
                session.chunks_failed += 1;
                session.consecutive_failures += 1;
                if session.errors.len() < MAX_SESSION_ERRORS {
                    session.errors.push(e);
                }
                if session.consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                    session.status = SESSION_FAILED.to_string();
                    if session.errors.len() < MAX_SESSION_ERRORS {
                        session.errors.push(format!(
                            "连续 {} 块失败，挖掘已熔断（可放弃会话或重新导入）",
                            session.consecutive_failures
                        ));
                    }
                    return Ok(false);
                }
            }
            None => session.consecutive_failures = 0,
        }
        if session.chunks_done >= session.chunks_total {
            session.status = SESSION_MINED.to_string();
        }
        Ok(session.status == SESSION_RUNNING)
    }

    /// 会话是否仍在挖掘（UI 轮询与后台循环共用）
    pub fn is_active(&self, source_id: &str) -> bool {
        self.sessions
            .read()
            .unwrap()
            .get(source_id)
            .map(|s| s.status == SESSION_RUNNING)
            .unwrap_or(false)
    }

    /// 入区一批草稿：逐条校验 + 会话内去重（error_type + 归一化标题）
    pub fn stage(&self, source_id: &str, drafts: Vec<DraftPoint>) -> Result<StageReport, String> {
        let mut sessions = self.sessions.write().unwrap();
        let session = sessions
            .get_mut(source_id)
            .ok_or_else(|| format!("导入会话不存在: {source_id}"))?;
        let lang = session.source.lang.clone();
        let mut seen: HashSet<(String, String)> = session
            .drafts
            .iter()
            .map(|d| (d.draft.error_type.clone(), normalize_title(&d.draft.title)))
            .collect();
        let mut report = StageReport {
            accepted: 0,
            duplicates: 0,
            rejected: 0,
            errors: Vec::new(),
            total_staged: session.drafts.len(),
        };
        let now = chrono::Utc::now().to_rfc3339();
        for (i, draft) in drafts.into_iter().enumerate() {
            if let Err(e) = validate_draft(&draft, &lang) {
                report.rejected += 1;
                if report.errors.len() < 20 {
                    report.errors.push(format!("#{i}: {e}"));
                }
                continue;
            }
            let key = (draft.error_type.clone(), normalize_title(&draft.title));
            if !seen.insert(key) {
                report.duplicates += 1;
                continue;
            }
            session.drafts.push(StagedDraft {
                seq: session.drafts.len() as u64 + 1,
                draft,
                staged_at: now.clone(),
            });
            report.accepted += 1;
        }
        report.total_staged = session.drafts.len();
        Ok(report)
    }

    /// 开启规则派生批次（知识点挖掘完成后、发布前；按 error_type 分批跑 agent）。
    /// `total` = 批次数；重复调用需先完成或失败（running 中拒绝）
    pub fn begin_rules(&self, source_id: &str, total: usize) -> Result<(), String> {
        let mut sessions = self.sessions.write().unwrap();
        let session = sessions
            .get_mut(source_id)
            .ok_or_else(|| format!("导入会话不存在: {source_id}"))?;
        if session.status == SESSION_RUNNING {
            return Err("知识点挖掘尚未完成，先等待挖掘结束".into());
        }
        if session.rules_status == SESSION_RUNNING {
            return Err("规则派生已在进行中".into());
        }
        session.rules_status = SESSION_RUNNING.to_string();
        session.rules_total = total;
        session.rules_done = 0;
        session.rules_errors.clear();
        Ok(())
    }

    /// 规则派生一批结束（成功或失败）；返回 `false` = 已熔断
    pub fn mark_rule_batch_done(
        &self,
        source_id: &str,
        error: Option<String>,
    ) -> Result<bool, String> {
        let mut sessions = self.sessions.write().unwrap();
        let session = sessions
            .get_mut(source_id)
            .ok_or_else(|| format!("导入会话不存在: {source_id}"))?;
        session.rules_done += 1;
        if let Some(e) = error {
            if session.rules_errors.len() < MAX_SESSION_ERRORS {
                session.rules_errors.push(e);
            }
        }
        if session.rules_done >= session.rules_total {
            session.rules_status = if session.rules_errors.len() >= session.rules_total {
                SESSION_FAILED.to_string()
            } else {
                SESSION_MINED.to_string()
            };
            return Ok(false);
        }
        if session.rules_errors.len() >= MAX_CONSECUTIVE_FAILURES {
            session.rules_status = SESSION_FAILED.to_string();
            return Ok(false);
        }
        Ok(true)
    }

    /// 规则派生是否进行中
    pub fn is_rules_active(&self, source_id: &str) -> bool {
        self.sessions
            .read()
            .unwrap()
            .get(source_id)
            .map(|s| s.rules_status == SESSION_RUNNING)
            .unwrap_or(false)
    }

    /// 入区一批规则草稿：逐条校验 + 会话内去重（按目标行 `_id`）
    pub fn stage_rules(
        &self,
        source_id: &str,
        drafts: Vec<DraftRule>,
    ) -> Result<RuleStageReport, String> {
        let mut sessions = self.sessions.write().unwrap();
        let session = sessions
            .get_mut(source_id)
            .ok_or_else(|| format!("导入会话不存在: {source_id}"))?;
        let mut seen: HashSet<String> = session
            .rule_drafts
            .iter()
            .map(|d| rule_row_id(&d.draft))
            .collect();
        let mut report = RuleStageReport {
            accepted: 0,
            duplicates: 0,
            rejected: 0,
            errors: Vec::new(),
            total_staged: session.rule_drafts.len(),
        };
        let now = chrono::Utc::now().to_rfc3339();
        for (i, draft) in drafts.into_iter().enumerate() {
            if let Err(e) = validate_rule_draft(&draft) {
                report.rejected += 1;
                if report.errors.len() < 20 {
                    report.errors.push(format!("#{i}: {e}"));
                }
                continue;
            }
            if !seen.insert(rule_row_id(&draft)) {
                report.duplicates += 1;
                continue;
            }
            session.rule_drafts.push(StagedRuleDraft {
                seq: session.rule_drafts.len() as u64 + 1,
                draft,
                staged_at: now.clone(),
            });
            report.accepted += 1;
        }
        report.total_staged = session.rule_drafts.len();
        Ok(report)
    }

    /// 放弃会话（丢弃全部草稿；生效知识库不受影响）
    pub fn discard(&self, source_id: &str) -> Result<usize, String> {
        let removed = self.sessions.write().unwrap().remove(source_id);
        removed
            .map(|s| s.drafts.len())
            .ok_or_else(|| format!("导入会话不存在: {source_id}"))
    }

    /// 无同 ID 会话时恢复一个导入会话（资源包导入携带草稿时用；
    /// 检查与插入同锁完成，杜绝与并发 ingest 启动之间的 TOCTOU）。
    /// 返回 true = 已插入；false = 已有同 ID 会话（不覆盖，不打断进行中的挖掘）
    pub fn put_session_if_absent(&self, session: IngestSession) -> bool {
        if session.source.source_id.trim().is_empty() {
            return false;
        }
        let mut sessions = self.sessions.write().unwrap();
        if sessions.contains_key(&session.source.source_id) {
            return false;
        }
        sessions.insert(session.source.source_id.clone(), session);
        true
    }

    /// 持久化草稿区到目录（每会话一个 JSON；重启后可恢复草稿）
    pub fn save_to_dir(&self, dir: &Path) -> Result<usize, String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建草稿目录失败: {e}"))?;
        let sessions = self.sessions.read().unwrap();
        let mut n = 0;
        for (source_id, session) in sessions.iter() {
            let path = dir.join(format!("{source_id}.json"));
            let content = serde_json::to_string_pretty(session)
                .map_err(|e| format!("草稿序列化失败: {e}"))?;
            std::fs::write(&path, content).map_err(|e| format!("草稿写入失败: {e}"))?;
            n += 1;
        }
        Ok(n)
    }

    /// 从目录恢复草稿区（损坏文件跳过；返回恢复会话数）
    pub fn load_from_dir(&self, dir: &Path) -> Result<usize, String> {
        if !dir.exists() {
            return Ok(0);
        }
        let mut sessions = self.sessions.write().unwrap();
        let mut n = 0;
        for entry in std::fs::read_dir(dir).map_err(|e| format!("读取草稿目录失败: {e}"))? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            match std::fs::read_to_string(&path)
                .ok()
                .and_then(|c| serde_json::from_str::<IngestSession>(&c).ok())
            {
                Some(mut s) if !s.source.source_id.is_empty() => {
                    // 重启恢复：挖掘中且未完成的会话无法续跑（agent 任务不持久化），
                    // 显式标记熔断，UI 可提示放弃或重新导入
                    if s.status == SESSION_RUNNING && s.chunks_done < s.chunks_total {
                        s.status = SESSION_FAILED.to_string();
                        if s.errors.len() < MAX_SESSION_ERRORS {
                            s.errors
                                .push("服务重启导致挖掘中断（可放弃会话或重新导入）".to_string());
                        }
                    }
                    sessions.insert(s.source.source_id.clone(), s);
                    n += 1;
                }
                _ => tracing::warn!(path = %path.display(), "草稿文件解析失败，已跳过"),
            }
        }
        Ok(n)
    }
}

// ---------------------------------------------------------------------------
// 正文分块（按空行分段，贪心装填至 max_chars；超长段按字符边界硬切）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextChunk {
    pub index: usize,
    /// 块首行（截断，展示 / 定位用）
    pub heading: String,
    pub text: String,
}

fn chunk_heading(body: &str) -> String {
    body.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .chars()
        .take(80)
        .collect()
}

/// 分块：段落（空行分隔）贪心装填；单段超限按字符边界硬切。
pub fn chunk_text(text: &str, max_chars: usize) -> Vec<TextChunk> {
    let max_chars = max_chars.max(200);
    let mut chunks: Vec<TextChunk> = Vec::new();
    let mut current = String::new();
    for para in text.split("\n\n") {
        // 单段超限：先冲掉已装填部分，再对该段按字符边界硬切
        if para.chars().count() > max_chars {
            if !current.trim().is_empty() {
                let body = current.trim().to_string();
                chunks.push(TextChunk {
                    index: chunks.len(),
                    heading: chunk_heading(&body),
                    text: body,
                });
            }
            current.clear();
            let mut piece = String::new();
            for ch in para.chars() {
                piece.push(ch);
                if piece.chars().count() >= max_chars {
                    chunks.push(TextChunk {
                        index: chunks.len(),
                        heading: chunk_heading(&piece),
                        text: std::mem::take(&mut piece),
                    });
                }
            }
            current = piece;
            continue;
        }
        // 装填超限：先冲块
        if current.chars().count() + para.chars().count() + 2 > max_chars {
            if !current.trim().is_empty() {
                let body = current.trim().to_string();
                chunks.push(TextChunk {
                    index: chunks.len(),
                    heading: chunk_heading(&body),
                    text: body,
                });
            }
            current.clear();
        }
        if !current.is_empty() {
            current.push_str("\n\n");
        }
        current.push_str(para);
    }
    if !current.trim().is_empty() {
        let body = current.trim().to_string();
        chunks.push(TextChunk {
            index: chunks.len(),
            heading: chunk_heading(&body),
            text: body,
        });
    }
    chunks
}

/// 章节输入（document-parse 解析产物喂入导入管线）
#[derive(Debug, Clone)]
pub struct ChapterInput {
    pub heading: String,
    pub text: String,
}

/// 章节感知分块：先按章切（章 = 挖掘上下文边界），超大章再用 chunk_text 细分。
/// 章标题写入块 heading（展示与溯源）；块序号全局重排。
pub fn chunk_chapters(chapters: &[ChapterInput], max_chars: usize) -> Vec<TextChunk> {
    let mut out: Vec<TextChunk> = Vec::new();
    for c in chapters {
        if c.text.trim().is_empty() {
            continue;
        }
        let mut sub = chunk_text(&c.text, max_chars);
        let heading = c.heading.trim();
        if !heading.is_empty() {
            for ch in sub.iter_mut() {
                ch.heading = heading.to_string();
            }
        }
        out.extend(sub);
    }
    for (i, ch) in out.iter_mut().enumerate() {
        ch.index = i;
    }
    out
}

// ---------------------------------------------------------------------------
// 归并对齐（草稿 → 生效知识库：既有点背书 / 新点造点）
// ---------------------------------------------------------------------------

/// 归一化标题（确定性匹配键：小写、去空白与常见中英文标点）
pub fn normalize_title(s: &str) -> String {
    s.chars()
        .filter(|c| {
            !c.is_whitespace()
                && !matches!(
                    c,
                    ',' | '.'
                        | ';'
                        | ':'
                        | '!'
                        | '?'
                        | '-'
                        | '('
                        | ')'
                        | '（'
                        | '）'
                        | '，'
                        | '。'
                        | '；'
                        | '：'
                        | '！'
                        | '？'
                        | '、'
                        | '“'
                        | '”'
                        | '"'
                        | '\''
                        | '·'
                        | '_'
                        | '/'
                )
        })
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// 既有点背书补丁：给 point_id 追加一条源引用
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Endorsement {
    pub point_id: String,
    pub source_ref: KnowledgeSourceRef,
}

/// 逐草稿归并动作（与 plan_merge 输入草稿一一对应，供草稿审阅 UI）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DraftAction {
    /// `endorse` 背书既有点 / `create` 造新点 / `covered` 本源已背书（跳过）
    pub action: String,
    /// 命中或即将分配的 point_id
    pub point_id: String,
}

/// 归并计划（发布预览与执行共用）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MergePlan {
    /// 造新点（已分配 point_id）
    pub new_points: Vec<KnowledgePoint>,
    /// 既有点追加源引用（1:N 背书）
    pub endorsements: Vec<Endorsement>,
    /// 该源已背书的重复草稿（跳过数）
    pub already_covered: usize,
    /// 逐草稿归并预判（与输入草稿同序）
    pub actions: Vec<DraftAction>,
}

/// 从 point_id（`{error_type}.{n}`）解析序号
fn point_seq(point_id: &str) -> Option<usize> {
    point_id.rsplit('.').next()?.parse().ok()
}

fn draft_source_ref(source: &KnowledgeSource, draft: &DraftPoint) -> KnowledgeSourceRef {
    KnowledgeSourceRef {
        source_id: source.source_id.clone(),
        location: if draft.location.trim().is_empty() {
            format!("{} {}", draft.section, draft.title)
                .trim()
                .to_string()
        } else {
            draft.location.clone()
        },
        title: String::new(), // 原始文件中为空，resolve 时从注册表回填
    }
}

/// 归并对齐（确定性质量门 + ID 分配）：
/// 1. 草稿带 `match_point_id` 且既有点存在 → 背书该点；
/// 2. 否则在同 error_type 既有点中做确定性匹配（归一化标题相等，
///    或 小节相等且标题互为包含）→ 背书该点；
/// 3. 否则造新点（`{error_type}.{max+1}`，含批内已分配序号去冲突）。
pub fn plan_merge(
    store: &KnowledgeStore,
    source: &KnowledgeSource,
    drafts: &[DraftPoint],
) -> MergePlan {
    let existing = store.points(&source.lang, None);
    // 各 error_type 现有最大序号（新点从 max+1 起）
    let mut max_seq: HashMap<String, usize> = HashMap::new();
    for p in &existing {
        if let Some(n) = point_seq(&p.point_id) {
            let e = max_seq.entry(p.error_type.clone()).or_insert(0);
            *e = (*e).max(n);
        }
    }
    let mut plan = MergePlan::default();
    // 批内已造点（error_type + 归一化标题 → 新 point_id），批内重复草稿转为该点背书
    let mut batch_new: HashMap<(String, String), String> = HashMap::new();

    for draft in drafts {
        let key = (draft.error_type.clone(), normalize_title(&draft.title));
        // 批内已造同一点 → 只追加背书
        if let Some(pid) = batch_new.get(&key) {
            plan.endorsements.push(Endorsement {
                point_id: pid.clone(),
                source_ref: draft_source_ref(source, draft),
            });
            plan.actions.push(DraftAction {
                action: "endorse".into(),
                point_id: pid.clone(),
            });
            continue;
        }
        // ① agent 显式指定的既有点；② 否则确定性标题/小节匹配
        let matched: Option<&KnowledgePoint> = if !draft.match_point_id.is_empty() {
            existing.iter().find(|p| p.point_id == draft.match_point_id)
        } else {
            let norm = normalize_title(&draft.title);
            existing.iter().find(|p| {
                p.error_type == draft.error_type
                    && (normalize_title(&p.title) == norm
                        || (!draft.section.is_empty()
                            && p.section == draft.section
                            && (normalize_title(&p.title).contains(&norm)
                                || norm.contains(&normalize_title(&p.title)))))
            })
        };
        if let Some(point) = matched {
            if point
                .sources
                .iter()
                .any(|r| r.source_id == source.source_id)
            {
                plan.already_covered += 1;
                plan.actions.push(DraftAction {
                    action: "covered".into(),
                    point_id: point.point_id.clone(),
                });
            } else {
                plan.endorsements.push(Endorsement {
                    point_id: point.point_id.clone(),
                    source_ref: draft_source_ref(source, draft),
                });
                plan.actions.push(DraftAction {
                    action: "endorse".into(),
                    point_id: point.point_id.clone(),
                });
            }
            continue;
        }
        // ③ 造新点
        let next = max_seq.entry(draft.error_type.clone()).or_insert(0);
        *next += 1;
        let point_id = format!("{}.{}", draft.error_type, *next);
        let point = KnowledgePoint {
            point_id: point_id.clone(),
            lang: source.lang.clone(),
            error_type: draft.error_type.clone(),
            title: draft.title.clone(),
            section: draft.section.clone(),
            cefr: draft.cefr.clone(),
            remedial: draft.remedial.clone(),
            examples: draft.examples.clone(),
            corrections: draft.corrections.clone(),
            sources: vec![draft_source_ref(source, draft)],
        };
        batch_new.insert(key, point_id);
        plan.actions.push(DraftAction {
            action: "create".into(),
            point_id: point.point_id.clone(),
        });
        plan.new_points.push(point);
    }
    plan
}

/// 发布回报
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyReport {
    pub source_id: String,
    pub lang: String,
    pub new_points: usize,
    pub endorsed: usize,
    pub already_covered: usize,
    pub total_points: usize,
    /// 决策表规则：新增 / 原地替换 / 冲突跳过（0 = 本会话未派生规则）
    #[serde(default)]
    pub rules_new: usize,
    #[serde(default)]
    pub rules_replaced: usize,
    #[serde(default)]
    pub rules_conflicts: usize,
}

/// 应用归并计划：经 [`KnowledgeStore`] 热更新通道原子替换（校验失败保留旧数据）。
pub fn apply_plan(
    store: &KnowledgeStore,
    source: &KnowledgeSource,
    plan: &MergePlan,
) -> Result<ApplyReport, String> {
    // ① 知识源注册表：既有 + 本源（幂等：已存在则覆盖元数据）
    let mut sources = store.sources();
    sources.retain(|s| s.source_id != source.source_id);
    sources.push(source.clone());
    store.update_sources(&serde_json::to_string(&sources).map_err(|e| e.to_string())?)?;

    // ② 知识点集合：既有点（应用背书补丁）+ 新点
    let mut points = store.points(&source.lang, None);
    let mut endorsed = 0usize;
    for e in &plan.endorsements {
        if let Some(p) = points.iter_mut().find(|p| p.point_id == e.point_id) {
            if !p
                .sources
                .iter()
                .any(|r| r.source_id == e.source_ref.source_id)
            {
                p.sources.push(e.source_ref.clone());
                endorsed += 1;
            }
        }
    }
    points.extend(plan.new_points.iter().cloned());
    let total = store.update_points(
        &source.lang,
        &serde_json::to_string(&points).map_err(|e| e.to_string())?,
    )?;

    Ok(ApplyReport {
        source_id: source.source_id.clone(),
        lang: source.lang.clone(),
        new_points: plan.new_points.len(),
        endorsed,
        already_covered: plan.already_covered,
        total_points: total,
        rules_new: 0,
        rules_replaced: 0,
        rules_conflicts: 0,
    })
}

// ---------------------------------------------------------------------------
// 归因规则草稿（决策表行候选：agent 语义草稿 → 宿主确定性构造 JDM 行）
// ---------------------------------------------------------------------------
//
// 与「LLM 只做抽取，代码做校验/归并/ID 分配」同一原则：agent 只提出
// 语义级规则草稿（错误类型 / 新手层或老手层 / 归因 / 讲解 / CEFR），
// JDM 条件表达式、行 ID（`_id`）、溯源标记（`_meta`）全部由代码生成，
// 且只填目标决策表实际存在的列——杜绝 LLM 手写成非法 JDM 污染决策表。

/// 归因分类法（与产品内置决策表 attribution 输出对齐）
pub const ATTRIBUTION_KINDS: [&str; 5] = [
    "rule_missing",
    "habit_solidified",
    "knowledge_gap",
    "rule_forgotten",
    "collocation_weak",
];

/// 韦纳归因维度合法值
const WEINER_LOCUS: [&str; 2] = ["internal", "external"];
const WEINER_STABILITY: [&str; 2] = ["stable", "unstable"];
const WEINER_CONTROLLABILITY: [&str; 2] = ["controllable", "uncontrollable"];

/// 规则草稿（语义级；不含 JDM 语法）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DraftRule {
    /// 错误类型（对齐既有分类法，如 article / tense）
    pub error_type: String,
    /// 学习者层定位：true=新手层（isNovice==true）；false=老手层
    /// （confidence 下限）；None=不限层（兜底行）
    #[serde(default)]
    pub novice: Option<bool>,
    /// 老手层置信度下限（novice=Some(false) 时生效；缺省 0.8）
    #[serde(default)]
    pub min_confidence: Option<f64>,
    /// 归因（ATTRIBUTION_KINDS 之一）
    pub attribution: String,
    /// 韦纳三维度（留空取默认：internal / stable / controllable）
    #[serde(default)]
    pub weiner_locus: String,
    #[serde(default)]
    pub weiner_stability: String,
    #[serde(default)]
    pub weiner_controllability: String,
    /// CEFR 难度（A1-C2；空 = 未判定）
    #[serde(default)]
    pub cefr: String,
    /// 补救讲解（决策表 remedial 输出）
    pub remedial: String,
}

/// 规则草稿的稳定行 ID（`{error_type}_{novice|hand|any}`）
pub fn rule_row_id(draft: &DraftRule) -> String {
    let layer = match draft.novice {
        Some(true) => "novice",
        Some(false) => "hand",
        None => "any",
    };
    format!("{}_{}", draft.error_type.trim(), layer)
}

/// 规则草稿校验（确定性质量门）
pub fn validate_rule_draft(draft: &DraftRule) -> Result<(), String> {
    if draft.error_type.trim().is_empty() {
        return Err("error_type 不能为空".into());
    }
    if !draft
        .error_type
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return Err(format!(
            "error_type 只允许小写字母/数字/下划线: {}",
            draft.error_type
        ));
    }
    if !ATTRIBUTION_KINDS.contains(&draft.attribution.as_str()) {
        return Err(format!(
            "attribution 非法（应为 {} 之一）: {}",
            ATTRIBUTION_KINDS.join("/"),
            draft.attribution
        ));
    }
    let check = |v: &str, allowed: &[&str], name: &str| -> Result<(), String> {
        if v.is_empty() {
            return Ok(());
        }
        if allowed.contains(&v) {
            Ok(())
        } else {
            Err(format!(
                "{name} 非法（应为 {} 之一）: {v}",
                allowed.join("/")
            ))
        }
    };
    check(&draft.weiner_locus, &WEINER_LOCUS, "weiner_locus")?;
    check(
        &draft.weiner_stability,
        &WEINER_STABILITY,
        "weiner_stability",
    )?;
    check(
        &draft.weiner_controllability,
        &WEINER_CONTROLLABILITY,
        "weiner_controllability",
    )?;
    if !draft.cefr.is_empty() && !CEFR_LEVELS.contains(&draft.cefr.as_str()) {
        return Err(format!("cefr 非法（应为 A1-C2 之一）: {}", draft.cefr));
    }
    if draft.remedial.trim().is_empty() {
        return Err("remedial 不能为空".into());
    }
    if draft.remedial.chars().count() > 4000 {
        return Err("remedial 过长（> 4000 字符）".into());
    }
    if let Some(c) = draft.min_confidence {
        if !(0.0..=1.0).contains(&c) {
            return Err(format!("min_confidence 应在 [0,1]: {c}"));
        }
    }
    Ok(())
}

/// 已入区的规则草稿
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StagedRuleDraft {
    pub seq: u64,
    pub draft: DraftRule,
    pub staged_at: String,
}

/// 规则草稿入区回报
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleStageReport {
    pub accepted: usize,
    pub duplicates: usize,
    pub rejected: usize,
    pub errors: Vec<String>,
    pub total_staged: usize,
}

/// JDM 列 ID（三语决策表共用列名；表缺列则跳过对应条件/输出）
const COL_ERROR_TYPE: &str = "errorType";
const COL_CONFIDENCE: &str = "confidence";
const COL_IS_NOVICE: &str = "isNovice";
const COL_ATTRIBUTION: &str = "attribution";
const COL_WEINER_LOCUS: &str = "weinerLocus";
const COL_WEINER_STABILITY: &str = "weinerStability";
const COL_WEINER_CONTROL: &str = "weinerControllability";
const COL_REMEDIAL: &str = "remedial";
const COL_MATCHED: &str = "matchedRule";
const COL_CEFR: &str = "cefrLevel";

/// 定位 JDM 决策表节点的输入/输出列 ID（(input_ids, output_ids)）
fn table_columns(jdm: &serde_json::Value) -> Option<(Vec<String>, Vec<String>)> {
    let node = jdm
        .get("nodes")?
        .as_array()?
        .iter()
        .find(|n| n.get("type").and_then(|t| t.as_str()) == Some("decisionTableNode"))?;
    let content = node.get("content")?;
    let inputs = content
        .get("inputs")?
        .as_array()?
        .iter()
        .filter_map(|i| i.get("id").and_then(|v| v.as_str()))
        .map(String::from)
        .collect();
    let outputs = content
        .get("outputs")?
        .as_array()?
        .iter()
        .filter_map(|o| o.get("id").and_then(|v| v.as_str()))
        .map(String::from)
        .collect();
    Some((inputs, outputs))
}

/// 语义草稿 → JDM 规则行（只填该表实际存在的列；`_id`/`_meta` 由代码生成）
pub fn build_rule_row(
    draft: &DraftRule,
    source_id: &str,
    inputs: &[String],
    outputs: &[String],
) -> serde_json::Value {
    let row_id = rule_row_id(draft);
    // serde_json 开 preserve_order：键序 _id/_meta → 输入列 → 输出列（与手写表一致）
    let mut row = serde_json::Map::new();
    row.insert("_id".into(), serde_json::json!(row_id));
    row.insert("_meta".into(), serde_json::json!(source_id));
    if inputs.iter().any(|c| c == COL_ERROR_TYPE) {
        row.insert(
            COL_ERROR_TYPE.into(),
            serde_json::json!(format!("== \"{}\"", draft.error_type.trim())),
        );
    }
    if inputs.iter().any(|c| c == COL_IS_NOVICE) {
        if let Some(n) = draft.novice {
            row.insert(COL_IS_NOVICE.into(), serde_json::json!(format!("== {n}")));
        }
    }
    if inputs.iter().any(|c| c == COL_CONFIDENCE) && draft.novice == Some(false) {
        let c = draft.min_confidence.unwrap_or(0.8);
        row.insert(COL_CONFIDENCE.into(), serde_json::json!(format!(">= {c}")));
    }
    let has = |id: &str| outputs.iter().any(|c| c == id);
    let mut put = |id: &str, value: String| {
        if has(id) {
            row.insert(id.into(), serde_json::json!(format!("\"{value}\"")));
        }
    };
    put(COL_ATTRIBUTION, draft.attribution.clone());
    let locus = if draft.weiner_locus.is_empty() {
        "internal"
    } else {
        &draft.weiner_locus
    };
    put(COL_WEINER_LOCUS, locus.to_string());
    let stab = if draft.weiner_stability.is_empty() {
        "stable"
    } else {
        &draft.weiner_stability
    };
    put(COL_WEINER_STABILITY, stab.to_string());
    let ctrl = if draft.weiner_controllability.is_empty() {
        "controllable"
    } else {
        &draft.weiner_controllability
    };
    put(COL_WEINER_CONTROL, ctrl.to_string());
    put(COL_REMEDIAL, draft.remedial.trim().to_string());
    put(COL_MATCHED, row_id.clone());
    if !draft.cefr.is_empty() {
        put(COL_CEFR, draft.cefr.clone());
    }
    serde_json::Value::Object(row)
}

/// 规则归并动作：新增 / 本源重派生原地替换 / 冲突（内置或其他源占用，保守跳过）
pub const RULE_NEW: &str = "new";
pub const RULE_REPLACE: &str = "replace";
pub const RULE_CONFLICT: &str = "conflict";

/// 逐草稿归并预判
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RulePlanRow {
    pub rule_id: String,
    /// new / replace / conflict
    pub action: String,
    /// 构造出的 JDM 行（conflict 时为说明对象）
    pub row: serde_json::Value,
}

/// 规则归并计划（发布预览与执行共用；单语言表）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RulePlan {
    pub lang: String,
    pub rows: Vec<RulePlanRow>,
    pub new: usize,
    pub replaced: usize,
    pub conflicts: usize,
}

/// 规则归并对齐（确定性）：
/// 1. 表内无同 `_id` 行 → new；
/// 2. 同 `_id` 且 `_meta` = 本源 → replace（重派生原地替换）；
/// 3. 同 `_id` 但属内置行或其他源 → conflict（跳过，不覆盖）。
pub fn plan_rules(
    table_json: &str,
    lang: &str,
    source_id: &str,
    drafts: &[DraftRule],
) -> Result<RulePlan, String> {
    let jdm: serde_json::Value =
        serde_json::from_str(table_json).map_err(|e| format!("决策表解析失败: {e}"))?;
    let (inputs, outputs) = table_columns(&jdm).ok_or("决策表缺少 decisionTableNode")?;
    let existing: Vec<&serde_json::Value> = jdm
        .get("nodes")
        .and_then(|n| n.as_array())
        .and_then(|nodes| {
            nodes
                .iter()
                .find(|n| n.get("type").and_then(|t| t.as_str()) == Some("decisionTableNode"))
        })
        .and_then(|n| n.pointer("/content/rules"))
        .and_then(|r| r.as_array())
        .map(|r| r.iter().collect())
        .unwrap_or_default();
    let mut plan = RulePlan {
        lang: lang.to_string(),
        rows: Vec::new(),
        new: 0,
        replaced: 0,
        conflicts: 0,
    };
    for draft in drafts {
        let row = build_rule_row(draft, source_id, &inputs, &outputs);
        let rule_id = row
            .get("_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let hit = existing
            .iter()
            .find(|r| r.get("_id").and_then(|v| v.as_str()) == Some(rule_id.as_str()));
        let action = match hit {
            None => {
                plan.new += 1;
                RULE_NEW
            }
            Some(h) => {
                if h.get("_meta").and_then(|v| v.as_str()) == Some(source_id) {
                    plan.replaced += 1;
                    RULE_REPLACE
                } else {
                    plan.conflicts += 1;
                    RULE_CONFLICT
                }
            }
        };
        if action == RULE_CONFLICT {
            plan.rows.push(RulePlanRow {
                rule_id,
                action: action.to_string(),
                row: serde_json::json!({"note": "既有同 ID 行属产品内置或其他知识源，保守跳过"}),
            });
            continue;
        }
        plan.rows.push(RulePlanRow {
            rule_id,
            action: action.to_string(),
            row,
        });
    }
    Ok(plan)
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(error_type: &str, title: &str) -> DraftPoint {
        DraftPoint {
            error_type: error_type.into(),
            title: title.into(),
            section: String::new(),
            remedial: format!("规则讲解：{title}"),
            cefr: "B1".into(),
            examples: vec![],
            corrections: vec![],
            location: String::new(),
            match_point_id: String::new(),
        }
    }

    fn source() -> KnowledgeSource {
        KnowledgeSource {
            source_id: "src-new-book".into(),
            title: "新语法书".into(),
            author: "作者".into(),
            edition: "第一版".into(),
            lang: "en".into(),
            license_scope: "internal_test".into(),
            point_count: 0,
        }
    }

    #[test]
    fn validate_rejects_bad_drafts() {
        assert!(validate_draft(&draft("article", "定冠词"), "en").is_ok());
        let mut d = draft("article", "");
        assert!(validate_draft(&d, "en").is_err(), "title 为空应拒收");
        d = draft("Bad Type!", "x");
        assert!(validate_draft(&d, "en").is_err(), "error_type 非法应拒收");
        d = draft("article", "标题");
        d.cefr = "X9".into();
        assert!(validate_draft(&d, "en").is_err(), "cefr 非法应拒收");
        d.cefr = String::new();
        assert!(validate_draft(&d, "en").is_ok(), "cefr 空应通过");
    }

    #[test]
    fn chunk_text_packs_and_hard_splits() {
        let para = "a".repeat(500);
        let text = vec![para.clone(); 10].join("\n\n");
        let chunks = chunk_text(&text, 1200);
        assert!(!chunks.is_empty());
        for c in &chunks {
            assert!(
                c.text.chars().count() <= 1200,
                "块超限: {}",
                c.text.chars().count()
            );
        }
        let total: usize = chunks.iter().map(|c| c.text.chars().count()).sum();
        assert!(total >= 5000, "内容不应丢失: {total}");
        // 超长单段硬切
        let big = "x".repeat(3000);
        let chunks = chunk_text(&big, 1000);
        assert!(chunks.len() >= 3);
        assert!(chunks.iter().all(|c| c.text.chars().count() <= 1000));
    }

    #[test]
    fn staging_validates_dedups_and_tracks_chunks() {
        let store = StagingStore::new();
        store.open_session(source(), 3).unwrap();
        // 重复 source_id 拒绝
        assert!(store.open_session(source(), 1).is_err());
        let report = store
            .stage(
                "src-new-book",
                vec![
                    draft("article", "定冠词的使用"),
                    draft("article", "定冠词的使用"), // 会话内重复
                    draft("article", ""),             // 非法
                ],
            )
            .unwrap();
        assert_eq!(
            (report.accepted, report.duplicates, report.rejected),
            (1, 1, 1)
        );
        assert_eq!(report.total_staged, 1);
        store.mark_chunk_done("src-new-book", None).unwrap();
        store
            .mark_chunk_done("src-new-book", Some("LLM 超时".into()))
            .unwrap();
        let s = store.session("src-new-book").unwrap();
        assert_eq!((s.chunks_done, s.chunks_failed), (2, 1));
        assert_eq!(s.errors.len(), 1);
        assert_eq!(store.discard("src-new-book").unwrap(), 1);
        assert!(store.session("src-new-book").is_none());
    }

    #[test]
    fn plan_merge_endorses_existing_and_creates_new() {
        let kb = KnowledgeStore::new();
        kb.update_points(
            "en",
            r#"[{
                "point_id": "article.3",
                "lang": "en",
                "error_type": "article",
                "title": "不定冠词的使用",
                "section": "二.1",
                "sources": [{ "source_id": "src-old", "location": "第一讲" }]
            }]"#,
        )
        .unwrap();
        let drafts = vec![
            // ① agent 显式匹配既有点
            DraftPoint {
                match_point_id: "article.3".into(),
                ..draft("article", "a/an 的选择")
            },
            // ② 确定性标题匹配（归一化后相等）
            draft("article", "不定冠词的使用"),
            // ③ 新概念造新点（既有最大序号 3 → article.4）
            draft("article", "冠词省略的特殊情形"),
            // 批内重复 → 对新点的背书而非再造点
            draft("article", "冠词省略的特殊情形！"),
        ];
        let plan = plan_merge(&kb, &source(), &drafts);
        assert_eq!(plan.new_points.len(), 1, "仅新概念造新点");
        assert_eq!(plan.new_points[0].point_id, "article.4");
        assert_eq!(plan.endorsements.len(), 3, "既有点 2 条 + 批内 1 条背书");
        assert!(
            plan.endorsements
                .iter()
                .all(|e| e.source_ref.source_id == "src-new-book")
        );
        // 幂等：本源已背书的点再归并 → already_covered
        let kb2 = KnowledgeStore::new();
        kb2.update_points(
            "en",
            r#"[{
                "point_id": "article.3",
                "lang": "en",
                "error_type": "article",
                "title": "不定冠词的使用",
                "sources": [{ "source_id": "src-new-book", "location": "x" }]
            }]"#,
        )
        .unwrap();
        let plan2 = plan_merge(&kb2, &source(), &[draft("article", "不定冠词的使用")]);
        assert_eq!(plan2.already_covered, 1);
        assert!(plan2.endorsements.is_empty());
    }

    #[test]
    fn apply_plan_updates_store_and_resolves_sources() {
        let kb = KnowledgeStore::new();
        kb.update_sources(r#"[{ "source_id": "src-old", "title": "旧书", "lang": "en" }]"#)
            .unwrap();
        kb.update_points(
            "en",
            r#"[{ "point_id": "article.3", "lang": "en", "error_type": "article",
                  "title": "不定冠词的使用", "sources": [{ "source_id": "src-old" }] }]"#,
        )
        .unwrap();
        let drafts = vec![
            DraftPoint {
                match_point_id: "article.3".into(),
                ..draft("article", "a/an 的选择")
            },
            draft("tense", "过去时的构成"),
        ];
        let plan = plan_merge(&kb, &source(), &drafts);
        let report = apply_plan(&kb, &source(), &plan).unwrap();
        assert_eq!(report.new_points, 1);
        assert_eq!(report.endorsed, 1);
        assert_eq!(report.total_points, 2);
        // 注册表含两个源
        assert_eq!(kb.sources().len(), 2);
        // 既有点获得 1:N 双源背书，resolve 回填标题
        let p = kb.get_point("en", "article.3").unwrap();
        assert_eq!(p.sources.len(), 2);
        let resolved = kb.resolve(&p.sources);
        assert!(resolved.iter().any(|r| r.title == "新语法书"));
        // 新点可被点级富化命中
        let new = kb.get_point("en", "tense.1").expect("新点应存在");
        assert_eq!(new.cefr, "B1");
    }

    #[test]
    fn staging_persists_and_restores() {
        let dir = std::env::temp_dir().join(format!(
            "ingest_stage_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = StagingStore::new();
        store.open_session(source(), 2).unwrap();
        store
            .stage("src-new-book", vec![draft("article", "定冠词")])
            .unwrap();
        store.save_to_dir(&dir).unwrap();
        let restored = StagingStore::new();
        assert_eq!(restored.load_from_dir(&dir).unwrap(), 1);
        let s = restored.session("src-new-book").unwrap();
        assert_eq!(s.drafts.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_status_transitions_and_circuit_breaker() {
        let store = StagingStore::new();
        store.open_session(source(), 10).unwrap();
        assert!(store.is_active("src-new-book"));
        assert_eq!(
            store.session("src-new-book").unwrap().status,
            SESSION_RUNNING
        );

        // 失败-成功交替：连续计数清零，不熔断
        for _ in 0..3 {
            assert!(
                store
                    .mark_chunk_done("src-new-book", Some("块失败".into()))
                    .unwrap()
            );
            assert!(store.mark_chunk_done("src-new-book", None).unwrap());
        }
        let s = store.session("src-new-book").unwrap();
        assert_eq!((s.chunks_done, s.chunks_failed), (6, 3));
        assert_eq!(s.consecutive_failures, 0);

        // 连续失败达阈值 → 熔断（返回 false，status=failed，停止轮询/挖掘）
        assert!(
            store
                .mark_chunk_done("src-new-book", Some("x".into()))
                .unwrap()
        );
        assert!(
            store
                .mark_chunk_done("src-new-book", Some("x".into()))
                .unwrap()
        );
        assert!(
            !store
                .mark_chunk_done("src-new-book", Some("x".into()))
                .unwrap()
        );
        assert!(!store.is_active("src-new-book"));
        let s = store.session("src-new-book").unwrap();
        assert_eq!(s.status, SESSION_FAILED);
        assert!(s.errors.iter().any(|e| e.contains("熔断")));
    }

    #[test]
    fn all_chunks_done_marks_session_mined() {
        let store = StagingStore::new();
        store.open_session(source(), 2).unwrap();
        assert!(store.mark_chunk_done("src-new-book", None).unwrap());
        assert!(
            !store
                .mark_chunk_done("src-new-book", Some("最后一块失败".into()))
                .unwrap()
        );
        // 最后一块失败但 done==total 且未达熔断阈值 → mined（部分产出可审阅）
        let s = store.session("src-new-book").unwrap();
        assert_eq!(s.status, SESSION_MINED);
        assert_eq!(s.chunks_failed, 1);
    }

    #[test]
    fn plan_merge_records_per_draft_actions() {
        let kb = KnowledgeStore::new();
        kb.update_points(
            "en",
            r#"[{ "point_id": "article.3", "lang": "en", "error_type": "article",
                  "title": "不定冠词的使用", "sources": [] }]"#,
        )
        .unwrap();
        let drafts = vec![
            DraftPoint {
                match_point_id: "article.3".into(),
                ..draft("article", "a/an 的选择")
            },
            draft("article", "冠词省略的特殊情形"),
            draft("article", "冠词省略的特殊情形！"), // 批内重复 → 背书新点
        ];
        let plan = plan_merge(&kb, &source(), &drafts);
        assert_eq!(plan.actions.len(), 3, "动作与草稿一一对应");
        assert_eq!(plan.actions[0].action, "endorse");
        assert_eq!(plan.actions[0].point_id, "article.3");
        assert_eq!(plan.actions[1].action, "create");
        assert_eq!(plan.actions[1].point_id, "article.4");
        assert_eq!(plan.actions[2].action, "endorse");
        assert_eq!(plan.actions[2].point_id, "article.4");
    }

    // ---- 规则草稿 / 章节分块 ----

    fn rule_draft(error_type: &str, novice: Option<bool>) -> DraftRule {
        DraftRule {
            error_type: error_type.into(),
            novice,
            min_confidence: None,
            attribution: "rule_missing".into(),
            weiner_locus: String::new(),
            weiner_stability: String::new(),
            weiner_controllability: String::new(),
            cefr: "B1".into(),
            remedial: format!("规则讲解：{error_type}"),
        }
    }

    #[test]
    fn chunk_chapters_keeps_chapter_bounds_and_headings() {
        let big = "a".repeat(5000);
        let chapters = vec![
            ChapterInput {
                heading: "第一讲 冠词".into(),
                text: "短章正文。".into(),
            },
            ChapterInput {
                heading: "第九讲 时态".into(),
                text: big.clone(),
            },
            ChapterInput {
                heading: String::new(),
                text: "   ".into(),
            },
        ];
        let chunks = chunk_chapters(&chapters, 1200);
        assert!(chunks.len() >= 6, "超大章应细分: {}", chunks.len());
        assert_eq!(chunks[0].heading, "第一讲 冠词");
        assert!(chunks.iter().skip(1).all(|c| c.heading == "第九讲 时态"));
        for (i, c) in chunks.iter().enumerate() {
            assert_eq!(c.index, i, "序号全局重排");
            assert!(c.text.chars().count() <= 1200);
        }
    }

    #[test]
    fn rule_draft_validation_and_staging() {
        assert!(validate_rule_draft(&rule_draft("article", Some(true))).is_ok());
        let mut d = rule_draft("Bad Type", None);
        assert!(validate_rule_draft(&d).is_err(), "error_type 非法应拒收");
        d = rule_draft("tense", None);
        d.attribution = "made_up".into();
        assert!(validate_rule_draft(&d).is_err(), "attribution 非法应拒收");
        d = rule_draft("tense", None);
        d.remedial = String::new();
        assert!(validate_rule_draft(&d).is_err(), "remedial 空应拒收");

        let store = StagingStore::new();
        store.open_session(source(), 1).unwrap();
        let report = store
            .stage_rules(
                "src-new-book",
                vec![
                    rule_draft("article", Some(true)),
                    rule_draft("article", Some(true)),
                    rule_draft("tense", Some(false)),
                ],
            )
            .unwrap();
        assert_eq!(
            (report.accepted, report.duplicates, report.rejected),
            (2, 1, 0)
        );
    }

    #[test]
    fn plan_rules_new_replace_conflict_with_real_en_table() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../decision-tables");
        let raw = std::fs::read_to_string(dir.join("en_gec_attribution.json")).unwrap();

        // 新源：article_novice 已被 src-sample-en 占用 → conflict；新 error_type → new
        let drafts = vec![
            rule_draft("article", Some(true)),
            rule_draft("gerund", Some(true)),
        ];
        let plan = plan_rules(&raw, "en", "src-new-book", &drafts).unwrap();
        assert_eq!((plan.new, plan.conflicts), (1, 1));
        assert_eq!(plan.rows[0].action, RULE_CONFLICT);
        assert_eq!(plan.rows[1].action, RULE_NEW);
        assert_eq!(plan.rows[1].rule_id, "gerund_novice");

        // 示例源重派生：src-sample-en 再提 article_novice → replace
        let plan2 = plan_rules(
            &raw,
            "en",
            "src-sample-en",
            &[rule_draft("article", Some(true))],
        )
        .unwrap();
        assert_eq!((plan2.replaced, plan2.new, plan2.conflicts), (1, 0, 0));

        // 构造行可被 merge_tagged_rows 合并并通过引擎校验
        let rows: Vec<serde_json::Value> = plan
            .rows
            .iter()
            .filter(|r| r.action == RULE_NEW)
            .map(|r| r.row.clone())
            .collect();
        let mut jdm: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let (replaced, added) = crate::dataset::merge_tagged_rows(&mut jdm, &rows, "src-new-book");
        assert_eq!((replaced, added), (0, 1));
        let json = serde_json::to_string(&jdm).unwrap();
        crate::engine::AttributionEngine::validate(&json).unwrap();
        // 新行落在通用兜底行之前（行序不变量）
        let rows_after = jdm["nodes"][1]["content"]["rules"].as_array().unwrap();
        let pos_new = rows_after
            .iter()
            .position(|r| r.get("_id").and_then(|v| v.as_str()) == Some("gerund_novice"))
            .unwrap();
        let pos_generic = rows_after
            .iter()
            .position(|r| r.get("_meta").and_then(|v| v.as_str()).unwrap_or("") == "")
            .unwrap();
        assert!(pos_new < pos_generic, "源派生行须在通用行前");
    }

    #[test]
    fn build_rule_row_fills_jdm_literals() {
        let inputs: Vec<String> = ["errorType", "confidence", "history", "isNovice"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let outputs: Vec<String> = [
            "attribution",
            "weinerLocus",
            "weinerStability",
            "weinerControllability",
            "remedial",
            "matchedRule",
            "cefrLevel",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let d = DraftRule {
            error_type: "article".into(),
            novice: Some(false),
            min_confidence: Some(0.7),
            attribution: "habit_solidified".into(),
            weiner_locus: String::new(),
            weiner_stability: String::new(),
            weiner_controllability: String::new(),
            cefr: "A2".into(),
            remedial: "规则讲解：冠词用法回顾".into(),
        };
        let row = build_rule_row(&d, "src-x", &inputs, &outputs);
        assert_eq!(row["_id"], "article_hand");
        assert_eq!(row["_meta"], "src-x");
        assert_eq!(row["errorType"], "== \"article\"");
        assert_eq!(row["isNovice"], "== false");
        assert_eq!(row["confidence"], ">= 0.7");
        assert_eq!(row["attribution"], "\"habit_solidified\"");
        assert_eq!(row["weinerLocus"], "\"internal\"");
        assert_eq!(row["matchedRule"], "\"article_hand\"");
        assert_eq!(row["cefrLevel"], "\"A2\"");
        assert!(row.get("history").is_none(), "无条件列留空（通配）");
    }
}
