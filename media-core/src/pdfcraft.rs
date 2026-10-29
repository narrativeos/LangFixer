//! PdfCraft（`pdfcraft-cli`）适配器：PDF 高级模式
//!
//! 实测契约（`pdfcraft-cli 0.14.0`；脚本与输出见 `MULTIMODAL.md` §13 与
//! `scripts/media_smoke_p2.sh`——**不要凭记忆改这里的 flag**）：
//!
//! ```text
//! doc_open{path}            → {doc, pages, editable, encrypted, dirty}
//! doc_info{doc}             → pages[]: {page, width, height, rotation}   ← points
//! text_paragraphs{doc,page} → paragraphs[]: {paragraph, text, rect:[x0,y0,x1,y1], lines}
//! text_find{doc,query}      → matches[]: {page, text, rects:[[x0,y0,x1,y1]]}（原点左上）
//! comment_add{doc,page,type,quads|rect|find,contents,author,color}
//! comment_list{doc}         → comments[]: {id, page, index, type, author, contents, rect}
//! comment_delete{doc,id}    doc_save{doc,path} → {path, bytes}
//! render <file> --page N --dpi D --out x.png（顶层子命令，不是 tool）
//! ```
//!
//! 三条硬约束：
//! 1. **文档 id 是进程内的**——一次操作 = 一个 `run --script … --report` 进程，
//!    所以「取文」与「回写」各自一个脚本，不指望跨进程状态。
//! 2. **归一化靠页尺寸**：工具给 points，`Region::PageBox` 存页内 0..1，
//!    换算需要 `doc_info` 的 width/height；拿不到就落 `Unlocated` 并警告，绝不猜坐标。
//! 3. **幂等**：重跑先删 `author == 我们署名` 的旧批注（`comment_list` → `comment_delete`）；
//!    原件永不改（`doc_save{path}` 写到 `result.pdf`）。

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::process::Command;

use crate::adapter::{
    ApplyRequest, ApplyResult, Collaborator, MediaError, ToolCapability, ToolProbe,
};
use crate::model::{
    Annotation, ApplyMode, CarrierInfo, CarrierKind, ExtractSource, Region, TextUnit,
};
use crate::photo::{CallOut, not_found_msg, resolve_tool_bin};

pub const PDF_APP: &str = "pdfcraft";
pub const PDF_CLI: &str = "pdfcraft-cli";

/// 回写 / 预览参数
pub struct PdfCraftConfig {
    /// 设置里显式配置的可执行路径（空 = 自动解析）
    pub bin: Option<String>,
    /// 单次 CLI 调用超时
    pub timeout_secs: u64,
    /// 批注署名（同时是幂等清理的判据）
    pub author: String,
    /// 批注颜色 `#RRGGBB`
    pub mark_color: String,
    /// 预览渲染 DPI（`render` 上限 600）
    pub preview_dpi: u32,
}

impl Default for PdfCraftConfig {
    fn default() -> Self {
        Self {
            bin: None,
            timeout_secs: 180,
            author: "LangFixer".to_string(),
            mark_color: "#e03131".to_string(),
            preview_dpi: 144,
        }
    }
}

/// 页尺寸（points）——`PageBox` 归一化的唯一依据
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageSize {
    pub page: u32,
    pub width: f64,
    pub height: f64,
}

/// 一页的 `text_paragraphs` 结果
#[derive(Debug, Clone)]
pub struct PageParagraphs {
    pub page: u32,
    pub paragraphs: Vec<Value>,
}

pub struct PdfCraft {
    bin: PathBuf,
    cfg: PdfCraftConfig,
}

/// `doc_info` → 页尺寸表
pub fn parse_page_sizes(info: &Value) -> Vec<PageSize> {
    info.get("pages")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|p| {
                    let page = p.get("page").and_then(Value::as_u64)? as u32;
                    let width = p.get("width").and_then(Value::as_f64)?;
                    let height = p.get("height").and_then(Value::as_f64)?;
                    if width > 0.0 && height > 0.0 {
                        Some(PageSize {
                            page,
                            width,
                            height,
                        })
                    } else {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn page_size<'a>(sizes: &'a [PageSize], page: u32) -> Option<&'a PageSize> {
    sizes.iter().find(|s| s.page == page)
}

/// points 矩形 `[x0,y0,x1,y1]` → 页内归一化 `PageBox`
///
/// 页尺寸缺失 / 矩形不合法 → `None`（调用方必须落 `Unlocated`，不许猜）
pub fn rect_to_page_box(page: u32, rect: [f64; 4], size: &PageSize) -> Option<Region> {
    let [x0, y0, x1, y1] = rect;
    if !(x0.is_finite() && y0.is_finite() && x1.is_finite() && y1.is_finite()) {
        return None;
    }
    let (w, h) = (x1 - x0, y1 - y0);
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    Some(Region::PageBox {
        page,
        x: x0 / size.width,
        y: y0 / size.height,
        w: w / size.width,
        h: h / size.height,
    })
}

/// 归一化 `PageBox` → points 矩形（回写时反算）
pub fn page_box_to_rect(region: &Region, size: &PageSize) -> Option<[f64; 4]> {
    match region {
        Region::PageBox { x, y, w, h, .. } => Some([
            x * size.width,
            y * size.height,
            (x + w) * size.width,
            (y + h) * size.height,
        ]),
        _ => None,
    }
}

/// points 矩形 → `comment_add` 的 `quads`
/// （实测：`squiggly` 只吃 `quads` 或 `find`，给 `rect` 会报「squiggly needs `find` … or `quads`」）
/// points 矩形 → `comment_add` 的 `quads`
/// （实测：`squiggly` 只吃 `quads` 或 `find`，给 `rect` 会被拒）
pub fn rect_to_quad(rect: [f64; 4]) -> Vec<f64> {
    let [x0, y0, x1, y1] = rect;
    vec![x0, y0, x1, y0, x1, y1, x0, y1]
}

/// `text_paragraphs` → 段落级单元（结构层，置信度 1.0）
///
/// 返回 `(units, warnings)`：几何缺失的段落**照样进列表**（文本是真的），
/// 但落点写 `Unlocated` 并警告——「不画假框」与「不漏检」是两条独立约束。
pub fn paragraph_units(
    pages: &[PageParagraphs],
    sizes: &[PageSize],
) -> (Vec<TextUnit>, Vec<String>) {
    let mut units: Vec<TextUnit> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    for pp in pages {
        let size = page_size(sizes, pp.page);
        for p in &pp.paragraphs {
            let text = p.get("text").and_then(Value::as_str).unwrap_or("").trim();
            if text.is_empty() {
                continue;
            }
            let rect = p
                .get("rect")
                .and_then(Value::as_array)
                .filter(|a| a.len() == 4)
                .and_then(|a| {
                    a.iter()
                        .map(|v| v.as_f64())
                        .collect::<Option<Vec<f64>>>()
                        .map(|v| [v[0], v[1], v[2], v[3]])
                });
            let para_no = p.get("paragraph").and_then(Value::as_u64).unwrap_or(0);
            let region = match (rect, size) {
                (Some(r), Some(s)) => rect_to_page_box(pp.page, r, s).unwrap_or(Region::Unlocated),
                _ => {
                    warnings.push(format!(
                        "第{}页段落#{para_no} 读不到几何信息（页尺寸或 rect 缺失）→ 只进列表，不画框",
                        pp.page
                    ));
                    Region::Unlocated
                }
            };
            units.push(TextUnit {
                id: units.len() as u32 + 1,
                text: text.to_string(),
                source: ExtractSource::Structure,
                region,
                confidence: 1.0,
                editable: true,
                page: Some(pp.page),
            });
        }
    }
    (units, warnings)
}

/// `comment_list` → 属于我们的批注 id（幂等清理的输入）
pub fn our_comment_ids(list: &Value, author: &str) -> Vec<String> {
    list.get("comments")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter(|c| {
                    c.get("author")
                        .and_then(Value::as_str)
                        .map(|a| a.eq_ignore_ascii_case(author))
                        .unwrap_or(false)
                })
                .filter_map(|c| c.get("id").and_then(Value::as_str).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// 批注正文：与清单/报告同一套措辞（实现在 [`Annotation::mark_text`]）
pub fn comment_text(a: &Annotation) -> String {
    a.mark_text()
}

/// 一条 annotation → `comment_add` 步骤；画不出来的返回 `None` + 原因
pub fn comment_step(
    doc: i64,
    a: &Annotation,
    sizes: &[PageSize],
    cfg: &PdfCraftConfig,
    mode: ApplyMode,
) -> (Option<Value>, Option<String>) {
    let page = match a.region {
        Region::PageBox { page, .. } => page,
        Region::Unlocated => {
            return (
                None,
                Some("定位失败（未定位）→ 不画假框，只进清单".to_string()),
            );
        }
        _ => {
            return (
                None,
                Some("该落点类型 PdfCraft 不支持（非页内矩形）".to_string()),
            );
        }
    };
    let Some(size) = page_size(sizes, page) else {
        return (
            None,
            Some(format!("第{page}页尺寸读不到 → 无法换算坐标，不猜")),
        );
    };
    let Some(rect) = page_box_to_rect(&a.region, size) else {
        return (None, Some("坐标换算失败 → 不猜".to_string()));
    };
    let mut args = json!({
        "doc": doc,
        "page": page,
        "contents": comment_text(a),
        "author": cfg.author,
        "color": cfg.mark_color,
    });
    // Overlay = 波浪线（校对语义），Comment = 便签；两者都不改正文
    if mode == ApplyMode::Comment {
        args["type"] = json!("note");
        args["rect"] = json!([rect[0], rect[1], rect[0] + 24.0, rect[1] + 24.0]);
    } else {
        args["type"] = json!("squiggly");
        args["quads"] = json!([rect_to_quad(rect)]);
    }
    (Some(json!({ "tool": "comment_add", "args": args })), None)
}

// ---------------------------------------------------------------------------
// 进程层：一次操作 = 一个脚本（文档 id 只在进程内有效）
// ---------------------------------------------------------------------------

/// 脚本里引用文档的固定 id：`doc_open` 是新进程里的第一个 open，实测恒为 1。
/// 不是 1 就如实报错，不去猜。
const DOC: i64 = 1;

/// stdout 里取第一个 JSON 值（`--report` 之后可能还跟着错误行；`tools` 是数组不是对象）
fn first_json(s: &str) -> Option<Value> {
    let start = s.find(|c: char| c == '{' || c == '[')?;
    let mut iter = serde_json::Deserializer::from_str(&s[start..]).into_iter::<Value>();
    iter.next()?.ok()
}

fn tail(s: &str, chars: usize) -> String {
    let t = s.trim();
    if t.chars().count() <= chars {
        return t.to_string();
    }
    t.chars().take(chars).collect()
}

impl PdfCraft {
    pub fn new(cfg: PdfCraftConfig) -> Result<Self, MediaError> {
        let bin = resolve_tool_bin(cfg.bin.as_deref(), PDF_CLI)
            .ok_or_else(|| MediaError::NotReady(not_found_msg(PDF_CLI, cfg.bin.as_deref())))?;
        Ok(Self { bin, cfg })
    }

    pub fn bin_path(&self) -> &Path {
        &self.bin
    }

    pub fn config(&self) -> &PdfCraftConfig {
        &self.cfg
    }

    /// 执行 CLI（超时保护同 PhotoCraft）；返回 (输出, 退出码是否为 0)
    async fn call_soft(&self, args: &[String]) -> Result<(CallOut, bool), MediaError> {
        let mut cmd = Command::new(&self.bin);
        cmd.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let fut = cmd.output();
        let out = tokio::time::timeout(Duration::from_secs(self.cfg.timeout_secs), fut)
            .await
            .map_err(|_| MediaError::Timeout {
                secs: self.cfg.timeout_secs,
            })?
            .map_err(|e| MediaError::Internal(format!("启动 {} 失败：{e}", self.bin.display())))?;
        Ok((
            CallOut {
                stdout: String::from_utf8_lossy(&out.stdout).to_string(),
                stderr: String::from_utf8_lossy(&out.stderr).to_string(),
            },
            out.status.success(),
        ))
    }

    /// 严格要求退出码为 0
    async fn call(&self, args: &[String]) -> Result<CallOut, MediaError> {
        let (out, ok) = self.call_soft(args).await?;
        if !ok {
            return Err(MediaError::ToolFailed {
                tool: PDF_CLI.into(),
                detail: if out.stderr.trim().is_empty() {
                    tail(&out.stdout, 300)
                } else {
                    tail(&out.stderr, 300)
                },
            });
        }
        Ok(out)
    }

    fn open_step(file: &Path) -> Value {
        json!({ "tool": "doc_open", "args": { "path": file.display().to_string() } })
    }

    /// 跑一串 tool（单进程）。返回每步 `result[0]`（无结果 → `Null`）
    ///
    /// 实测：某步失败时后续步骤 `status: "skipped"`，且进程退出码非 0——
    /// 所以这里不能只看退出码，必须逐步看 status，否则错误信息会被吞掉。
    async fn run_script(&self, root: &Path, steps: &[Value]) -> Result<Vec<Value>, MediaError> {
        let script = root.join(format!(".langfixer_pdfcraft_{}.json", std::process::id()));
        let body = serde_json::to_vec(steps)
            .map_err(|e| MediaError::Internal(format!("脚本序列化失败：{e}")))?;
        tokio::fs::write(&script, body)
            .await
            .map_err(|e| MediaError::Internal(format!("写入脚本失败：{e}")))?;
        let r = self
            .call_soft(&[
                "run".into(),
                "--script".into(),
                script.display().to_string(),
                "--root".into(),
                root.display().to_string(),
                "--report".into(),
            ])
            .await;
        let _ = tokio::fs::remove_file(&script).await;
        let (out, _ok) = r?;
        let Some(report) = first_json(&out.stdout) else {
            return Err(MediaError::ToolFailed {
                tool: PDF_CLI.into(),
                detail: format!("--report 无 JSON 输出：{}", tail(&out.stderr, 300)),
            });
        };
        let Some(rows) = report.get("steps").and_then(Value::as_array).cloned() else {
            return Err(MediaError::ToolFailed {
                tool: PDF_CLI.into(),
                detail: "报告里没有 steps".into(),
            });
        };
        let mut results = Vec::new();
        for row in rows {
            let tool = row.get("tool").and_then(Value::as_str).unwrap_or("?");
            let status = row.get("status").and_then(Value::as_str).unwrap_or("?");
            if status != "ok" {
                let detail = row
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or(status)
                    .to_string();
                return Err(MediaError::ToolFailed {
                    tool: format!("{PDF_CLI} · {tool}"),
                    detail: format!("{detail}｜{}", tail(&out.stderr, 300)),
                });
            }
            results.push(
                row.get("result")
                    .and_then(Value::as_array)
                    .and_then(|a| a.first().cloned())
                    .unwrap_or(Value::Null),
            );
        }
        Ok(results)
    }

    /// `[doc_open, doc_info]` → (document, 页尺寸表)
    pub async fn open_info(&self, file: &Path) -> Result<(Value, Vec<PageSize>), MediaError> {
        let root = file.parent().unwrap_or(Path::new("."));
        let rows = self
            .run_script(
                root,
                &[
                    Self::open_step(file),
                    json!({ "tool": "doc_info", "args": { "doc": DOC } }),
                ],
            )
            .await?;
        let opened = rows.first().cloned().unwrap_or(Value::Null);
        let got = opened.get("doc").and_then(Value::as_i64).unwrap_or(0);
        if got != DOC {
            return Err(MediaError::ToolFailed {
                tool: PDF_CLI.into(),
                detail: format!(
                    "doc_open 返回 doc={got}，与脚本假设的 {DOC} 不符 → 拒绝继续，不猜"
                ),
            });
        }
        let info = rows.get(1).cloned().unwrap_or(Value::Null);
        Ok((opened, parse_page_sizes(&info)))
    }

    /// `[doc_open, text_paragraphs×pages]` → 每页段落
    pub async fn paragraphs_of(
        &self,
        file: &Path,
        pages: &[u32],
    ) -> Result<Vec<PageParagraphs>, MediaError> {
        let root = file.parent().unwrap_or(Path::new("."));
        let mut steps = vec![Self::open_step(file)];
        for p in pages {
            steps.push(json!({ "tool": "text_paragraphs", "args": { "doc": DOC, "page": p } }));
        }
        let rows = self.run_script(root, &steps).await?;
        Ok(pages
            .iter()
            .enumerate()
            .map(|(i, p)| PageParagraphs {
                page: *p,
                paragraphs: rows
                    .get(i + 1)
                    .and_then(|v| v.get("paragraphs"))
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
            })
            .collect())
    }

    /// 一次拿齐：载体概况 + 段落单元
    pub async fn inspect_with_units(
        &self,
        file: &Path,
    ) -> Result<(CarrierInfo, Vec<TextUnit>), MediaError> {
        let (opened, sizes) = self.open_info(file).await?;
        let count = opened.get("pages").and_then(Value::as_u64).unwrap_or(0) as u32;
        let pages: Vec<u32> = (1..=count.max(1)).collect();
        let paragraphs = self.paragraphs_of(file, &pages).await?;
        let (units, mut warnings) = paragraph_units(&paragraphs, &sizes);
        if opened.get("encrypted").and_then(Value::as_bool) == Some(true) {
            warnings.push("PDF 已加密：文本可读，但回写可能被拒".to_string());
        }
        if opened.get("editable").and_then(Value::as_bool) == Some(false) {
            warnings.push(format!(
                "PDF 不可编辑：{}",
                opened
                    .get("read_only_reason")
                    .and_then(Value::as_str)
                    .unwrap_or("未知原因")
            ));
        }
        if units.is_empty() {
            warnings.push("没有可读的文本层段落（扫描版？）→ 简单模式无法校对".to_string());
        }
        let info = CarrierInfo {
            kind: CarrierKind::Pdf,
            width: None,
            height: None,
            pages: Some(count),
            text_units: units.len(),
            editable: opened
                .get("editable")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            warnings,
        };
        Ok((info, units))
    }

    /// `text_find` → 命中词的 points 矩形（把框从「整段」收到「命中词」）
    pub async fn text_find(
        &self,
        file: &Path,
        query: &str,
    ) -> Result<Vec<(u32, [f64; 4])>, MediaError> {
        let root = file.parent().unwrap_or(Path::new("."));
        let rows = self
            .run_script(
                root,
                &[
                    Self::open_step(file),
                    json!({ "tool": "text_find", "args": { "doc": DOC, "query": query } }),
                ],
            )
            .await?;
        let matches = rows
            .get(1)
            .and_then(|v| v.get("matches"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(matches
            .iter()
            .filter_map(|m| {
                let page = m.get("page").and_then(Value::as_u64)? as u32;
                let rect = m
                    .get("rects")
                    .and_then(Value::as_array)?
                    .first()
                    .and_then(Value::as_array)
                    .filter(|a| a.len() == 4)
                    .and_then(|a| {
                        a.iter()
                            .map(|v| v.as_f64())
                            .collect::<Option<Vec<f64>>>()
                            .map(|v| [v[0], v[1], v[2], v[3]])
                    })?;
                Some((page, rect))
            })
            .collect())
    }

    /// 渲染某一页为 png（顶层子命令 `render`）
    pub async fn render_page(&self, file: &Path, page: u32, out: &Path) -> Result<(), MediaError> {
        let o = self
            .call(&[
                "render".into(),
                file.display().to_string(),
                "--page".into(),
                page.to_string(),
                "--dpi".into(),
                self.cfg.preview_dpi.to_string(),
                "--out".into(),
                out.display().to_string(),
            ])
            .await?;
        if out.exists() {
            Ok(())
        } else {
            Err(MediaError::ToolFailed {
                tool: PDF_CLI.into(),
                detail: format!("render 没有产出文件：{}", tail(&o.stderr, 300)),
            })
        }
    }

    /// `tools` → 工具名列表（能力面靠探测，不靠版本号猜）
    pub async fn tool_names(&self) -> Vec<String> {
        match self.call(&["tools".into()]).await {
            Ok(o) => first_json(&o.stdout)
                .and_then(|v| {
                    let arr = match v {
                        Value::Array(a) => a,
                        _ => v.get("tools").and_then(Value::as_array).cloned()?,
                    };
                    Some(
                        arr.iter()
                            .filter_map(|t| t.get("name").and_then(Value::as_str).map(String::from))
                            .collect(),
                    )
                })
                .unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    }

    /// 只读探测：版本 + 能力面（缺什么原语就说什么，不猜）
    pub async fn probe(&self) -> Result<ToolProbe, MediaError> {
        let version = self.call(&["--version".into()]).await?.first_line();
        let names = self.tool_names().await;
        let has = |n: &str| names.iter().any(|x| x == n);
        let mut capabilities = Vec::new();
        let mut missing: Vec<&str> = Vec::new();
        if has("text_paragraphs") || has("text_extract") {
            capabilities.push(ToolCapability::InspectText);
        } else {
            missing.push("text_paragraphs");
        }
        if has("comment_add") && has("comment_list") {
            capabilities.push(ToolCapability::Comments);
            capabilities.push(ToolCapability::OverlayMarks);
        } else {
            missing.push("comment_add/comment_list");
        }
        if has("page_render") {
            capabilities.push(ToolCapability::Preview);
        } else {
            missing.push("page_render");
        }
        let mut notes = vec![
            "PDF 原位改字（text_edit）对中日韩不可靠 → 高级模式只做批注与叠加，不改正文"
                .to_string(),
        ];
        if has("ocr_status") || has("ocr_recognize") {
            capabilities.push(ToolCapability::Ocr);
            // OCR 语言是硬事实，必须如实上报（实测本机只有 en）
            if let Ok(langs) = self.ocr_languages().await {
                if langs.is_empty() {
                    notes.push("OCR 已安装但没有可用语言包".to_string());
                } else {
                    notes.push(format!("OCR 可用语言：{}", langs.join(", ")));
                }
            }
        } else {
            notes.push("没有 OCR 原语 → 扫描版 PDF 无法处理".to_string());
        }
        Ok(ToolProbe {
            app: PDF_APP.into(),
            bin: self.bin.display().to_string(),
            version,
            ready: missing.is_empty(),
            capabilities,
            degraded_reason: if missing.is_empty() {
                None
            } else {
                Some(format!("缺少原语：{}", missing.join(", ")))
            },
            notes,
        })
    }

    /// `ocr_status` → 语言代码（探测用，失败不致命）
    async fn ocr_languages(&self) -> Result<Vec<String>, MediaError> {
        let root = std::env::temp_dir();
        let rows = self
            .run_script(&root, &[json!({ "tool": "ocr_status", "args": {} })])
            .await?;
        Ok(rows
            .first()
            .and_then(|v| v.get("languages"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|l| l.get("code").and_then(Value::as_str).map(String::from))
            .collect())
    }

    /// 回写：Overlay = 波浪线，Comment = 便签。原件永不改（写到 `target`）。
    ///
    /// 幂等：先删「同署名」的旧批注——重跑不会越堆越多。
    pub async fn apply(&self, req: &ApplyRequest<'_>) -> Result<ApplyResult, MediaError> {
        let mut res = ApplyResult::default();
        match req.mode {
            ApplyMode::ListOnly => {
                res.warnings.push("ListOnly：不回写载体".to_string());
                return Ok(res);
            }
            ApplyMode::InPlace => {
                for a in req.annotations {
                    res.skipped.push((
                        a.id,
                        "PDF 原位改字（text_edit）对中日韩不可靠 → 未执行，不改正文".to_string(),
                    ));
                }
                res.warnings
                    .push("PDF 高级模式不提供 InPlace：请改用批注或叠加".to_string());
                return Ok(res);
            }
            _ => {}
        }
        let root = req.source.parent().unwrap_or(Path::new("."));
        let (opened, sizes) = self.open_info(req.source).await?;
        if opened.get("editable").and_then(Value::as_bool) == Some(false) {
            return Err(MediaError::NotWritable(
                opened
                    .get("read_only_reason")
                    .and_then(Value::as_str)
                    .unwrap_or("PDF 只读")
                    .to_string(),
            ));
        }
        // 幂等：先读旧批注（同署名 = 我们上次写的）
        let rows = self
            .run_script(
                root,
                &[
                    Self::open_step(req.source),
                    json!({ "tool": "comment_list", "args": { "doc": DOC } }),
                ],
            )
            .await?;
        let old = our_comment_ids(rows.get(1).unwrap_or(&Value::Null), &req.author);
        let mut steps = vec![Self::open_step(req.source)];
        for id in &old {
            steps.push(json!({ "tool": "comment_delete", "args": { "doc": DOC, "id": id } }));
        }
        if !old.is_empty() {
            res.warnings.push(format!(
                "幂等清理：先删除上次 {} 的批注 {} 条",
                req.author,
                old.len()
            ));
        }
        let mut first_page: Option<u32> = None;
        for a in req.annotations {
            let (step, why) = comment_step(DOC, a, &sizes, &self.cfg, req.mode);
            match (step, why) {
                (Some(s), _) => {
                    steps.push(s);
                    res.applied.push(a.id);
                    if first_page.is_none()
                        && let Region::PageBox { page, .. } = a.region
                    {
                        first_page = Some(page);
                    }
                }
                (None, Some(why)) => res.skipped.push((a.id, why)),
                (None, None) => res.skipped.push((a.id, "未知原因".to_string())),
            }
        }
        if res.applied.is_empty() {
            res.warnings
                .push("没有可落点的标注 → 不生成 result.pdf".to_string());
            return Ok(res);
        }
        steps.push(json!({
            "tool": "doc_save",
            "args": { "doc": DOC, "path": req.target.display().to_string() }
        }));
        steps.push(json!({ "tool": "doc_close", "args": { "doc": DOC } }));
        self.run_script(root, &steps).await?;
        res.artifacts.push(req.target.to_path_buf());
        // 预览：渲染第一条已标注所在页（多页时用户点列表会换页，见 §15.3）
        match self
            .render_page(req.target, first_page.unwrap_or(1), req.preview)
            .await
        {
            Ok(()) => res.artifacts.push(req.preview.to_path_buf()),
            Err(e) => res
                .warnings
                .push(format!("批注已写入，但预览渲染失败：{e}")),
        }
        Ok(res)
    }
}

#[async_trait]
impl Collaborator for PdfCraft {
    fn kind(&self) -> &'static str {
        "pdf"
    }
    async fn probe(&self) -> Result<ToolProbe, MediaError> {
        PdfCraft::probe(self).await
    }
    async fn inspect(&self, file: &Path) -> Result<CarrierInfo, MediaError> {
        Ok(self.inspect_with_units(file).await?.0)
    }
    async fn extract_units(&self, file: &Path) -> Result<Vec<TextUnit>, MediaError> {
        Ok(self.inspect_with_units(file).await?.1)
    }
    async fn apply(&self, req: &ApplyRequest<'_>) -> Result<ApplyResult, MediaError> {
        PdfCraft::apply(self, req).await
    }
    async fn render_preview(&self, file: &Path, out: &Path) -> Result<(), MediaError> {
        self.render_page(file, 1, out).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MarkStyle;
    use shared_model::Error;

    /// 真机输出（pdfcraft-cli 0.14.0，`note.pdf`）——测试锁契约，不是编数据
    const DOC_INFO: &str = r#"{"pages":[{"height":841.8900146484375,"label":"1","page":1,"rotation":0,"width":595.280029296875}],
        "document":{"doc":1,"pages":1,"editable":true,"encrypted":false}}"#;
    const PARAGRAPHS: &str = r#"{"count":1,"page":1,"paragraphs":[{"paragraph":1,"rect":[72.0,90.56,520.48,152.22],"size":20.0,
        "text":"Proof The He'llo wrold sample paragraph for LangFixer round trip."}]}"#;
    const COMMENT_LIST: &str = r##"{"comments":[
        {"author":"LangFixer","contents":"#1 typo: wrold → world","id":"ec16c442-afbb","index":1,"page":1,"type":"Squiggly"},
        {"author":"张三","contents":"这里改一下","id":"other-1","index":2,"page":1,"type":"Note"}],"count":2}"##;

    fn annotation(region: Region) -> Annotation {
        Annotation {
            id: 7,
            error: Error {
                lang: "en".into(),
                error_type: "spelling".into(),
                confidence: 0.9,
                span: [0, 5],
                original: "wrold".into(),
                correction: Some("world".into()),
                message: "拼写".into(),
                category: Some("spelling".into()),
                rule_id: None,
                suggestions: vec![],
                issue_type: None,
                source: None,
                also_found_by: vec![],
            },
            unit_id: 1,
            local_span: [0, 5],
            region,
            apply: ApplyMode::Overlay,
            style: MarkStyle::default(),
            locate_warning: None,
        }
    }

    #[test]
    fn page_sizes_come_from_doc_info() {
        let info: Value = serde_json::from_str(DOC_INFO).unwrap();
        let sizes = parse_page_sizes(&info);
        assert_eq!(sizes.len(), 1);
        assert!((sizes[0].width - 595.28).abs() < 0.01);
        assert_eq!(sizes[0].page, 1);
    }

    #[test]
    fn points_rect_normalizes_by_page_size_and_round_trips() {
        let info: Value = serde_json::from_str(DOC_INFO).unwrap();
        let sizes = parse_page_sizes(&info);
        let region = rect_to_page_box(1, [72.0, 90.56, 520.48, 152.22], &sizes[0]).unwrap();
        match region {
            Region::PageBox { page, x, y, w, h } => {
                assert_eq!(page, 1);
                assert!((x - 72.0 / 595.28).abs() < 1e-6);
                assert!((h - (152.22 - 90.56) / 841.89).abs() < 1e-6);
                let _ = (y, w);
            }
            other => panic!("应是 PageBox：{other:?}"),
        }
        // 归一化后再反算必须回到原 points（回写要用）
        let back = page_box_to_rect(&region, &sizes[0]).unwrap();
        assert!((back[0] - 72.0).abs() < 1e-6);
        assert!((back[3] - 152.22).abs() < 1e-6);
    }

    #[test]
    fn missing_geometry_lands_unlocated_with_warning_never_a_guess() {
        let sizes = parse_page_sizes(&Value::Null); // 没有页尺寸
        let pp: Value = serde_json::from_str(PARAGRAPHS).unwrap();
        let pages = vec![PageParagraphs {
            page: 1,
            paragraphs: pp["paragraphs"].as_array().unwrap().clone(),
        }];
        let (units, warnings) = paragraph_units(&pages, &sizes);
        assert_eq!(units.len(), 1, "文本是真的，照样进列表（不漏检）");
        assert_eq!(units[0].region, Region::Unlocated, "但没有坐标就不画框");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
    }

    #[test]
    fn paragraph_units_with_geometry_get_pagebox() {
        let info: Value = serde_json::from_str(DOC_INFO).unwrap();
        let pp: Value = serde_json::from_str(PARAGRAPHS).unwrap();
        let pages = vec![PageParagraphs {
            page: 1,
            paragraphs: pp["paragraphs"].as_array().unwrap().clone(),
        }];
        let (units, warnings) = paragraph_units(&pages, &parse_page_sizes(&info));
        assert!(units[0].region.is_locatable());
        assert!(warnings.is_empty());
        assert_eq!(units[0].page, Some(1));
        assert_eq!(units[0].confidence, 1.0);
    }

    #[test]
    fn idempotent_cleanup_targets_only_our_comments() {
        let list: Value = serde_json::from_str(COMMENT_LIST).unwrap();
        assert_eq!(our_comment_ids(&list, "LangFixer"), vec!["ec16c442-afbb"]);
        assert!(
            our_comment_ids(&list, "别人").is_empty(),
            "别人写的批注不能动"
        );
    }

    #[test]
    fn overlay_uses_squiggly_quads_comment_uses_note() {
        // 实测：squiggly 给 rect 会被拒（"squiggly needs `find` … or `quads`"）
        let info: Value = serde_json::from_str(DOC_INFO).unwrap();
        let sizes = parse_page_sizes(&info);
        let cfg = PdfCraftConfig::default();
        let region = rect_to_page_box(1, [72.0, 120.0, 520.0, 136.0], &sizes[0]).unwrap();
        let a = annotation(region);
        let (step, why) = comment_step(DOC, &a, &sizes, &cfg, ApplyMode::Overlay);
        assert!(why.is_none());
        let args = step.unwrap()["args"].clone();
        assert_eq!(args["type"], "squiggly");
        assert_eq!(args["author"], "LangFixer");
        let quad = args["quads"][0].as_array().unwrap();
        assert_eq!(quad.len(), 8);
        assert_eq!(quad[0], json!(72.0));
        let (step, _) = comment_step(DOC, &a, &sizes, &cfg, ApplyMode::Comment);
        assert_eq!(step.unwrap()["args"]["type"], "note");
    }

    #[test]
    fn unlocated_annotation_is_skipped_with_a_reason() {
        let info: Value = serde_json::from_str(DOC_INFO).unwrap();
        let sizes = parse_page_sizes(&info);
        let a = annotation(Region::Unlocated);
        let (step, why) = comment_step(
            DOC,
            &a,
            &sizes,
            &PdfCraftConfig::default(),
            ApplyMode::Overlay,
        );
        assert!(step.is_none());
        assert!(why.unwrap().contains("不画假框"));
    }

    #[test]
    fn comment_text_matches_the_list_wording() {
        let a = annotation(Region::Unlocated);
        assert_eq!(comment_text(&a), "#7 spelling：wrold → world");
    }

    #[test]
    fn report_json_survives_trailing_error_lines() {
        let s = r#"{"ok":true,"steps":[{"tool":"doc_open","status":"ok","result":[{}]}]}
pdfcraft-cli: step 2 (comment_add): squiggly needs `find`"#;
        let v = first_json(s).unwrap();
        assert_eq!(v["steps"][0]["status"], "ok");
        assert!(first_json("not json at all").is_none());
        // `tools` 输出是数组（实测 179 个 tool）
        let arr = first_json("[{\"name\":\"doc_open\"}]").unwrap();
        assert_eq!(arr[0]["name"], "doc_open");
    }

    /// 真机契约测试（需要本机 pdfcraft-cli 与一份带文本层的 PDF）：
    /// `LANGFIXER_TEST_PDF=/tmp/note.pdf cargo test -p media-core -- --ignored`
    #[tokio::test]
    #[ignore = "需要本机 pdfcraft-cli 与 LANGFIXER_TEST_PDF"]
    async fn real_cli_contract() {
        let pdf =
            std::env::var("LANGFIXER_TEST_PDF").expect("设置 LANGFIXER_TEST_PDF=/path/to.pdf");
        let file = PathBuf::from(&pdf);
        let tool = PdfCraft::new(PdfCraftConfig::default()).unwrap();
        let probe = tool.probe().await.unwrap();
        assert!(probe.ready, "{:?}", probe.degraded_reason);
        let (info, units) = tool.inspect_with_units(&file).await.unwrap();
        assert!(info.text_units > 0, "{:?}", info.warnings);
        assert_eq!(units.len(), info.text_units);
        let hits = tool.text_find(&file, "the").await.unwrap();
        assert!(!hits.is_empty(), "text_find 应能命中");
        let png = std::env::temp_dir().join("langfixer_contract_preview.png");
        tool.render_page(&file, 1, &png).await.unwrap();
        assert!(png.exists());
    }
}
