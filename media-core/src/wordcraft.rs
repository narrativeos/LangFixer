//! WordCraft（`wordcraft-cli`）适配器：Word 高级模式
//!
//! 实测契约（`wordcraft-cli 0.11.0`，见 `MULTIMODAL.md` §13 与 `scripts/media_smoke_p2.sh`）：
//!
//! ```text
//! info <file>            → {pages, paragraphs, words, comments, trackChanges, encrypted, …}
//! inspect <file>         → blocks[]: {index, len, text, style, type}（段落级；带修订时有 runs[]）
//! text <file>            → 纯文本
//! render <file> <out.png> [--page N] [--scale S]
//! convert <in> <out> [--markup]      ← --markup：批注与修订随页一起出（预览要用）
//! run [--file F] [--script FILE] [--cmd 'id={json}'] [--save OUT] [--print] [--author NAME]
//! commands --json        → [{id, label, mutates, params}]
//! ```
//!
//! 用到的命令（`commands --json` 实测）：
//! `select.text{text, occurrence?}`（**occurrence 实测精确命中第 N 次出现**）、
//! `text.delete{}`、`text.insert{text}`、`review.newComment{text}`、
//! `review.comments`（列表：`{id, author, text, anchor}`）、`review.deleteComment{id}`、
//! `review.trackChanges{value}`。
//!
//! 两条硬约束：
//! 1. Word 没有图片那样的自由标注层 → Overlay 与 Comment 都以**批注**落地（探测时如实说明）；
//! 2. `select.text` 按字面找，重复短语必须给 `occurrence`——
//!    算不出是第几次出现就跳过并说明，**不猜**。

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::process::Command;

use crate::adapter::{
    ApplyRequest, ApplyResult, Collaborator, MediaError, ToolCapability, ToolProbe,
};
use crate::model::{ApplyMode, CarrierInfo, CarrierKind, ExtractSource, Region, TextUnit};
use crate::photo::not_found_msg;
use crate::photo::resolve_tool_bin;
use crate::Annotation;

pub const WORD_APP: &str = "wordcraft";
pub const WORD_CLI: &str = "wordcraft-cli";

pub struct WordCraftConfig {
    pub bin: Option<String>,
    pub timeout_secs: u64,
    /// 批注/修订署名（也是幂等清理的判据）
    pub author: String,
}

impl Default for WordCraftConfig {
    fn default() -> Self {
        Self {
            bin: None,
            timeout_secs: 180,
            author: "LangFixer".to_string(),
        }
    }
}

/// `inspect` 的段落块
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    /// 文档里的段落序号（0 起，`blocks[].index`）
    pub index: usize,
    pub text: String,
    pub style: Option<String>,
}

/// `inspect` → 段落块（跳过没有文本的块）
pub fn parse_blocks(inspect: &Value) -> Vec<Block> {
    inspect
        .get("blocks")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|b| {
                    let text = b.get("text").and_then(Value::as_str).unwrap_or("");
                    if text.trim().is_empty() {
                        return None;
                    }
                    Some(Block {
                        index: b
                            .get("index")
                            .and_then(Value::as_u64)
                            .unwrap_or(0) as usize,
                        text: text.to_string(),
                        style: b
                            .get("style")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .or_else(|| {
                                b.get("props")
                                    .and_then(|p| p.get("style"))
                                    .and_then(Value::as_str)
                                    .map(str::to_string)
                            }),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 段落块 → 文本单元（落点 = 段落 + 段落内字符区间，不是像素）
pub fn block_units(blocks: &[Block]) -> Vec<TextUnit> {
    blocks
        .iter()
        .map(|b| TextUnit {
            id: b.index as u32 + 1,
            text: b.text.clone(),
            source: ExtractSource::Structure,
            region: Region::RunRef {
                part: format!("段落{}", b.index + 1),
                run_range: [0, b.text.chars().count()],
            },
            confidence: 1.0,
            editable: true,
            page: None,
        })
        .collect()
}

/// 段落块拼成文档正文（`select.text` 是在整篇文档里找字面串）
pub fn doc_text(blocks: &[Block]) -> String {
    blocks
        .iter()
        .map(|b| b.text.as_str())
        .collect::<Vec<&str>>()
        .join("\n")
}

/// 某条标注的原文是全文第几次出现（1 起）
///
/// 定位失败（原文与段落对不上 / 段落里没有该原文）→ `None`，
/// 调用方必须跳过并说明，不能默认「第一次」。
pub fn occurrence_of(
    blocks: &[Block],
    block_index: usize,
    local_span: [usize; 2],
    original: &str,
) -> Option<usize> {
    if original.is_empty() {
        return None;
    }
    let doc = doc_text(blocks);
    // 该段落在 doc 里的字节起点（段落之间插 "\n"，与 doc_text 一致）
    let mut offset = 0usize;
    let mut block_start: Option<usize> = None;
    for b in blocks {
        if b.index == block_index {
            block_start = Some(offset);
            break;
        }
        offset += b.text.len() + 1;
    }
    let start = block_start?;
    let block = blocks.iter().find(|b| b.index == block_index)?;
    let local = local_span[0].min(block.text.len());
    let target = start + local;
    if !doc[target..].starts_with(original) {
        // 段落内偏移处不是这段原文（跨段落 / 修订已改）→ 不猜
        return None;
    }
    let before = &doc[..target];
    let count = before.match_indices(original).count();
    Some(count + 1)
}

/// `run --print` 的输出（`review.comments: [{…}]`）→ JSON
pub fn parse_print_json(stdout: &str) -> Option<Value> {
    let start = stdout.find(|c: char| c == '[' || c == '{')?;
    let mut iter = serde_json::Deserializer::from_str(&stdout[start..]).into_iter::<Value>();
    iter.next()?.ok()
}

/// 批注列表里属于我们的 id（幂等清理）
pub fn our_comment_ids(comments: &[Value], author: &str) -> Vec<i64> {
    comments
        .iter()
        .filter(|c| {
            c.get("author")
                .and_then(Value::as_str)
                .map(|a| a.eq_ignore_ascii_case(author))
                .unwrap_or(false)
        })
        .filter_map(|c| c.get("id").and_then(Value::as_i64))
        .collect()
}

/// 标注 → WordCraft 命令序列（纯函数，可单测）
///
/// 定位不了的标注进 `skipped` 并带原因——不能默认「第一次出现」。
pub fn apply_commands(
    annotations: &[Annotation],
    blocks: &[Block],
    mode: ApplyMode,
) -> (Vec<Value>, Vec<(u32, String)>) {
    let mut cmds: Vec<Value> = Vec::new();
    let mut skipped: Vec<(u32, String)> = Vec::new();
    let mut tracking = false;
    for a in annotations {
        let Some(block_index) = a.unit_id.checked_sub(1) else {
            skipped.push((a.id, "没有对应段落（unit_id 无效）".to_string()));
            continue;
        };
        let block_index = block_index as usize;
        if !blocks.iter().any(|b| b.index == block_index) {
            skipped.push((a.id, format!("段落{}不在文档里", block_index + 1)));
            continue;
        }
        let original = a.error.original.trim();
        if original.is_empty() {
            skipped.push((a.id, "原文为空 → select.text 无法定位".to_string()));
            continue;
        }
        let Some(occurrence) = occurrence_of(blocks, block_index, a.local_span, original) else {
            skipped.push((
                a.id,
                format!("「{original}」在文档里的第几次出现算不出来 → 不猜，只进清单"),
            ));
            continue;
        };
        cmds.push(json!({
            "command": "select.text",
            "params": { "text": original, "occurrence": occurrence }
        }));
        match mode {
            ApplyMode::InPlace => {
                let fix = a.error.correction.clone().unwrap_or_default();
                if fix.trim().is_empty() {
                    // 没有建议值就别删人原文
                    cmds.push(json!({
                        "command": "review.newComment",
                        "params": { "text": a.mark_text() }
                    }));
                    continue;
                }
                tracking = true;
                cmds.push(json!({ "command": "text.delete", "params": {} }));
                cmds.push(json!({ "command": "text.insert", "params": { "text": fix } }));
            }
            // Word 没有自由标注层：Overlay 与 Comment 都以批注落地
            _ => {
                cmds.push(json!({
                    "command": "review.newComment",
                    "params": { "text": a.mark_text() }
                }));
            }
        }
    }
    if tracking {
        let mut with = vec![json!({ "command": "review.trackChanges", "params": { "value": true } })];
        with.append(&mut cmds);
        with.push(json!({ "command": "review.trackChanges", "params": { "value": false } }));
        cmds = with;
    }
    (cmds, skipped)
}

pub struct WordCraft {
    bin: PathBuf,
    cfg: WordCraftConfig,
}

impl WordCraft {
    pub fn new(cfg: WordCraftConfig) -> Result<Self, MediaError> {
        let bin = resolve_tool_bin(cfg.bin.as_deref(), WORD_CLI)
            .ok_or_else(|| MediaError::NotReady(not_found_msg(WORD_CLI, cfg.bin.as_deref())))?;
        Ok(Self { bin, cfg })
    }

    pub fn bin_path(&self) -> &Path {
        &self.bin
    }

    pub fn config(&self) -> &WordCraftConfig {
        &self.cfg
    }

    async fn call_soft(&self, args: &[String]) -> Result<(String, String, bool), MediaError> {
        let mut cmd = Command::new(&self.bin);
        cmd.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let out = tokio::time::timeout(Duration::from_secs(self.cfg.timeout_secs), cmd.output())
            .await
            .map_err(|_| MediaError::Timeout {
                secs: self.cfg.timeout_secs,
            })?
            .map_err(|e| MediaError::Internal(format!("启动 {} 失败：{e}", self.bin.display())))?;
        Ok((
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
            out.status.success(),
        ))
    }

    async fn call(&self, args: &[String]) -> Result<String, MediaError> {
        let (stdout, stderr, ok) = self.call_soft(args).await?;
        if !ok {
            return Err(MediaError::ToolFailed {
                tool: WORD_CLI.into(),
                detail: if stderr.trim().is_empty() {
                    stdout.trim().to_string()
                } else {
                    stderr.trim().to_string()
                },
            });
        }
        Ok(stdout)
    }

    /// `info <file>`
    pub async fn info(&self, file: &Path) -> Result<Value, MediaError> {
        let out = self
            .call(&["info".into(), file.display().to_string()])
            .await?;
        parse_print_json(&out)
            .filter(|v| v.is_object())
            .ok_or_else(|| MediaError::ToolFailed {
                tool: WORD_CLI.into(),
                detail: "info 输出不是 JSON".into(),
            })
    }

    /// `inspect <file>`
    pub async fn inspect_json(&self, file: &Path) -> Result<Value, MediaError> {
        let out = self
            .call(&["inspect".into(), file.display().to_string()])
            .await?;
        parse_print_json(&out)
            .filter(|v| v.is_object())
            .ok_or_else(|| MediaError::ToolFailed {
                tool: WORD_CLI.into(),
                detail: "inspect 输出不是 JSON".into(),
            })
    }

    /// 跑一串命令（单进程）。`save = Some(target)` 时写产物。
    ///
    /// 实测：某条命令失败即中止，`--save` 不会执行 → 不会写出半份文档（全有或全无）。
    async fn run_script(
        &self,
        file: &Path,
        commands: &[Value],
        save: Option<&Path>,
        print: bool,
    ) -> Result<String, MediaError> {
        let dir = file.parent().unwrap_or(Path::new("."));
        let script = dir.join(format!(".langfixer_wordcraft_{}.json", std::process::id()));
        let mut doc = json!({
            "file": file.display().to_string(),
            "commands": commands,
        });
        if let Some(target) = save {
            doc["save"] = json!([target.display().to_string()]);
        }
        let body = serde_json::to_vec(&doc)
            .map_err(|e| MediaError::Internal(format!("脚本序列化失败：{e}")))?;
        tokio::fs::write(&script, body)
            .await
            .map_err(|e| MediaError::Internal(format!("写入脚本失败：{e}")))?;
        let mut args = vec![
            "run".into(),
            "--script".into(),
            script.display().to_string(),
            "--author".into(),
            self.cfg.author.clone(),
        ];
        if print {
            args.push("--print".into());
        }
        let r = self.call(&args).await;
        let _ = tokio::fs::remove_file(&script).await;
        r
    }

    /// 现有批注列表（幂等清理的输入）
    pub async fn list_comments(&self, file: &Path) -> Result<Vec<Value>, MediaError> {
        let out = self
            .run_script(file, &[json!({ "command": "review.comments" })], None, true)
            .await?;
        Ok(parse_print_json(&out)
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default())
    }

    /// 格式转换（`--markup` = 批注与修订随页一起出）：报告 HTML→docx、预览 target→png
    pub async fn convert(&self, from: &Path, to: &Path, markup: bool) -> Result<(), MediaError> {
        let mut args = vec![
            "convert".into(),
            from.display().to_string(),
            to.display().to_string(),
        ];
        if markup {
            args.push("--markup".into());
        }
        self.call(&args).await?;
        if to.is_file() {
            Ok(())
        } else {
            Err(MediaError::ToolFailed {
                tool: WORD_CLI.into(),
                detail: format!("转换成功但没写出 {}", to.display()),
            })
        }
    }

    /// 一次拿齐：载体概况 + 段落单元
    pub async fn inspect_with_units(
        &self,
        file: &Path,
    ) -> Result<(CarrierInfo, Vec<TextUnit>), MediaError> {
        let info = self.info(file).await?;
        let inspect = self.inspect_json(file).await?;
        let blocks = parse_blocks(&inspect);
        let units = block_units(&blocks);
        let mut warnings: Vec<String> = Vec::new();
        if info.get("encrypted").and_then(Value::as_bool) == Some(true) {
            warnings.push("DOCX 已加密：文本可读，但回写可能被拒".to_string());
        }
        if info.get("trackChanges").and_then(Value::as_bool) == Some(true) {
            warnings
                .push("文档已有未处理的修订：InPlace 会在其上继续叠加，请先在 Word 里核对".to_string());
        }
        if units.is_empty() {
            warnings.push("没有可校对的段落".to_string());
        }
        Ok((
            CarrierInfo {
                kind: CarrierKind::Docx,
                width: None,
                height: None,
                pages: info.get("pages").and_then(Value::as_u64).map(|p| p as u32),
                text_units: units.len(),
                editable: info.get("encrypted").and_then(Value::as_bool) != Some(true),
                warnings,
            },
            units,
        ))
    }

    /// `commands --json` → 命令 id（能力面靠探测，不靠版本号猜）
    pub async fn command_ids(&self) -> Vec<String> {
        match self.call(&["commands".into(), "--json".into()]).await {
            Ok(o) => parse_print_json(&o)
                .and_then(|v| v.as_array().cloned())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|c| c.get("id").and_then(Value::as_str).map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    }

    /// 只读探测：版本 + 命令面
    pub async fn probe(&self) -> Result<ToolProbe, MediaError> {
        let version = self
            .call(&["--version".into()])
            .await?
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .to_string();
        let ids = self.command_ids().await;
        let has = |n: &str| ids.iter().any(|x| x == n);
        let mut capabilities = Vec::new();
        let mut missing: Vec<&str> = Vec::new();
        if has("select.text") {
            capabilities.push(ToolCapability::InspectText);
        } else {
            missing.push("select.text");
        }
        if has("review.newComment") && has("review.comments") && has("review.deleteComment") {
            capabilities.push(ToolCapability::Comments);
            capabilities.push(ToolCapability::OverlayMarks);
        } else {
            missing.push("review.newComment/review.comments/review.deleteComment");
        }
        if has("review.trackChanges") && has("text.delete") && has("text.insert") {
            capabilities.push(ToolCapability::EditInPlace);
        } else {
            missing.push("review.trackChanges/text.delete/text.insert");
        }
        capabilities.push(ToolCapability::Preview);
        let notes = vec![
            "Word 没有自由标注层：Overlay 与 Comment 都以批注落地".to_string(),
            "InPlace = 修订（Track Changes），会改正文：只在用户显式选择时执行".to_string(),
        ];
        Ok(ToolProbe {
            app: WORD_APP.into(),
            bin: self.bin.display().to_string(),
            version,
            ready: missing.is_empty(),
            capabilities,
            degraded_reason: if missing.is_empty() {
                None
            } else {
                Some(format!("缺少命令：{}", missing.join(", ")))
            },
            notes,
        })
    }

    /// 回写：Overlay/Comment = 批注，InPlace = 修订。原件永不改。
    ///
    /// 幂等：先删同署名批注。修订不能按署名删（Word 的接受/拒绝是全量的），
    /// 但 job 目录里每次都是只读原件的副本，所以重跑天然从原件出发，不会叠加。
    pub async fn apply(&self, req: &ApplyRequest<'_>) -> Result<ApplyResult, MediaError> {
        let mut res = ApplyResult::default();
        if req.mode == ApplyMode::ListOnly {
            res.warnings.push("ListOnly：不回写载体".to_string());
            return Ok(res);
        }
        let info = self.info(req.source).await?;
        if info.get("encrypted").and_then(Value::as_bool) == Some(true) {
            return Err(MediaError::NotWritable("DOCX 已加密".to_string()));
        }
        let inspect = self.inspect_json(req.source).await?;
        let blocks = parse_blocks(&inspect);
        let existing = self.list_comments(req.source).await?;
        let old = our_comment_ids(&existing, &req.author);
        let mut commands: Vec<Value> = old
            .iter()
            .map(|id| json!({ "command": "review.deleteComment", "params": { "id": id } }))
            .collect();
        if !old.is_empty() {
            res.warnings.push(format!(
                "幂等清理：先删除上次 {} 的批注 {} 条",
                req.author,
                old.len()
            ));
        }
        let (cmds, skipped) = apply_commands(req.annotations, &blocks, req.mode);
        commands.extend(cmds);
        res.skipped = skipped;
        if commands.is_empty() {
            res.warnings
                .push("没有可落点的标注 → 不生成 result.docx".to_string());
            return Ok(res);
        }
        self.run_script(req.source, &commands, Some(req.target), false)
            .await?;
        if !req.target.is_file() {
            return Err(MediaError::ToolFailed {
                tool: WORD_CLI.into(),
                detail: format!("命令跑完但没有写出 {}", req.target.display()),
            });
        }
        res.artifacts.push(req.target.to_path_buf());
        res.applied = req
            .annotations
            .iter()
            .map(|a| a.id)
            .filter(|id| !res.skipped.iter().any(|(s, _)| s == id))
            .collect();
        // 预览：带标注渲染（实测 --markup 把批注与修订一起出）
        match self.convert(req.target, req.preview, true).await {
            Ok(()) => res.artifacts.push(req.preview.to_path_buf()),
            Err(e) => res.warnings.push(format!("回写成功，但预览生成失败：{e}")),
        }
        Ok(res)
    }

    /// `render <file> <out.png>`
    pub async fn render(&self, file: &Path, out: &Path) -> Result<(), MediaError> {
        self.call(&[
            "render".into(),
            file.display().to_string(),
            out.display().to_string(),
        ])
        .await?;
        if out.is_file() {
            Ok(())
        } else {
            Err(MediaError::ToolFailed {
                tool: WORD_CLI.into(),
                detail: format!("render 没有产出 {}", out.display()),
            })
        }
    }
}

#[async_trait]
impl Collaborator for WordCraft {
    fn kind(&self) -> &'static str {
        "docx"
    }
    async fn probe(&self) -> Result<ToolProbe, MediaError> {
        WordCraft::probe(self).await
    }
    async fn inspect(&self, file: &Path) -> Result<CarrierInfo, MediaError> {
        Ok(self.inspect_with_units(file).await?.0)
    }
    async fn extract_units(&self, file: &Path) -> Result<Vec<TextUnit>, MediaError> {
        Ok(self.inspect_with_units(file).await?.1)
    }
    async fn apply(&self, req: &ApplyRequest<'_>) -> Result<ApplyResult, MediaError> {
        WordCraft::apply(self, req).await
    }
    async fn render_preview(&self, file: &Path, out: &Path) -> Result<(), MediaError> {
        WordCraft::render(self, file, out).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MarkStyle;
    use shared_model::Error;

    /// 真机输出（wordcraft-cli 0.11.0 `inspect in.docx`）
    const INSPECT: &str = r#"{"blocks":[
        {"index":0,"len":5,"props":{"style":"Heading1"},"style":"Heading1","text":"Proof","type":"paragraph"},
        {"index":1,"len":109,"style":"Normal","text":"The He'llo wrold sample paragraph for LangFixer round trip. Second line with wrold again for occurrence test.","type":"paragraph"}],
        "comments":0,"pages":1,"parts":[],"trackChanges":false,"words":18}"#;
    /// 真机输出（`run --cmd review.comments --print`）
    const COMMENTS: &str =
        "review.comments: [{\"anchor\":{\"off\":4,\"path\":[1],\"story\":\"body\"},\"author\":\"LangFixer\",\"date\":\"2026-10-28T11:43:37Z\",\"id\":0,\"parent\":null,\"resolved\":false,\"text\":\"#2 spelling: He'llo -> Hello\"}]";

    fn blocks() -> Vec<Block> {
        parse_blocks(&parse_print_json(INSPECT).unwrap())
    }

    fn annotation(
        unit_id: u32,
        local_span: [usize; 2],
        original: &str,
        fix: Option<&str>,
    ) -> Annotation {
        Annotation {
            id: 3,
            error: Error {
                lang: "en".into(),
                error_type: "spelling".into(),
                confidence: 0.9,
                span: [0, 5],
                original: original.into(),
                correction: fix.map(str::to_string),
                message: "拼写".into(),
                category: Some("spelling".into()),
                rule_id: None,
                suggestions: vec![],
                issue_type: None,
                source: None,
                also_found_by: vec![],
            },
            unit_id,
            local_span,
            region: Region::RunRef {
                part: format!("段落{}", unit_id + 1),
                run_range: [0, 10],
            },
            apply: ApplyMode::Overlay,
            style: MarkStyle::default(),
            locate_warning: None,
        }
    }

    #[test]
    fn inspect_blocks_keep_paragraph_identity() {
        let b = blocks();
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].index, 0);
        assert_eq!(b[0].style.as_deref(), Some("Heading1"));
        let units = block_units(&b);
        assert_eq!(units[1].id, 2);
        match &units[1].region {
            Region::RunRef { part, run_range } => {
                assert_eq!(part, "段落2");
                assert_eq!(run_range[1], 109, "段落内字符数（不是字节）");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn repeated_phrase_resolves_to_the_right_occurrence() {
        // 真机事实：select.text 带 occurrence 才改得准（实测 occurrence=1 只改第一处）
        let b = blocks();
        let text = &b[1].text;
        let first = text.find("wrold").unwrap();
        let second = text[first + 1..].find("wrold").map(|i| first + 1 + i).unwrap();
        assert_eq!(occurrence_of(&b, 1, [first, first + 5], "wrold"), Some(1));
        assert_eq!(occurrence_of(&b, 1, [second, second + 5], "wrold"), Some(2));
    }

    #[test]
    fn unresolvable_occurrence_is_none_not_one() {
        let b = blocks();
        // 偏移处不是这段原文 → 不能假装是第一次
        assert_eq!(occurrence_of(&b, 1, [0, 5], "wrold"), None);
        assert_eq!(occurrence_of(&b, 9, [0, 5], "wrold"), None);
        assert_eq!(occurrence_of(&b, 1, [0, 0], ""), None);
    }

    #[test]
    fn comment_mode_writes_comments_inplace_writes_tracked_edits() {
        let b = blocks();
        let first = b[1].text.find("wrold").unwrap();
        let a = annotation(2, [first, first + 5], "wrold", Some("world"));
        let one = std::slice::from_ref(&a);
        let (cmds, skipped) = apply_commands(one, &b, ApplyMode::Overlay);
        assert!(skipped.is_empty());
        assert_eq!(cmds[0]["command"], "select.text");
        assert_eq!(cmds[0]["params"]["occurrence"], 1);
        assert_eq!(cmds[1]["command"], "review.newComment");
        let (cmds, _) = apply_commands(one, &b, ApplyMode::InPlace);
        assert_eq!(cmds[0]["command"], "review.trackChanges");
        assert_eq!(cmds[0]["params"]["value"], true);
        assert_eq!(cmds[2]["command"], "text.delete");
        assert_eq!(cmds[3]["params"]["text"], "world");
        assert_eq!(cmds[4]["params"]["value"], false, "跑完要把修订关掉");
    }

    #[test]
    fn no_suggestion_means_no_deletion() {
        // 没有建议值就删人原文 = 破坏性操作，绝不允许
        let b = blocks();
        let first = b[1].text.find("wrold").unwrap();
        let a = annotation(2, [first, first + 5], "wrold", None);
        let (cmds, skipped) =
            apply_commands(std::slice::from_ref(&a), &b, ApplyMode::InPlace);
        assert!(skipped.is_empty());
        assert!(!cmds.iter().any(|c| c["command"] == "text.delete"));
        assert!(cmds.iter().any(|c| c["command"] == "review.newComment"));
    }

    #[test]
    fn unlocatable_annotation_is_skipped_with_reason() {
        let b = blocks();
        let a = annotation(9, [0, 5], "wrold", Some("world"));
        let (cmds, skipped) = apply_commands(std::slice::from_ref(&a), &b, ApplyMode::Overlay);
        assert!(cmds.is_empty());
        assert_eq!(skipped.len(), 1);
    }

    #[test]
    fn idempotent_cleanup_only_touches_our_comments() {
        let v = parse_print_json(COMMENTS).unwrap();
        let list = v.as_array().unwrap();
        assert_eq!(our_comment_ids(list, "LangFixer"), vec![0]);
        assert!(our_comment_ids(list, "别人").is_empty());
    }

    /// 真机契约测试：`LANGFIXER_TEST_DOCX=/tmp/in.docx cargo test -p media-core -- --ignored`
    #[tokio::test]
    #[ignore = "需要本机 wordcraft-cli 与 LANGFIXER_TEST_DOCX"]
    async fn real_cli_contract() {
        let docx = std::env::var("LANGFIXER_TEST_DOCX").expect("设置 LANGFIXER_TEST_DOCX=/path.docx");
        let file = PathBuf::from(&docx);
        let tool = WordCraft::new(WordCraftConfig::default()).unwrap();
        let probe = tool.probe().await.unwrap();
        assert!(probe.ready, "{:?}", probe.degraded_reason);
        let (info, units) = tool.inspect_with_units(&file).await.unwrap();
        assert!(info.text_units > 0, "{:?}", info.warnings);
        assert_eq!(units.len(), info.text_units);
        let png = std::env::temp_dir().join("langfixer_word_preview.png");
        tool.render(&file, &png).await.unwrap();
        assert!(png.exists());
    }
}





