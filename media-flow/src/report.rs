//! 正误表导出（`MULTIMODAL.md` §18）：把勾选后的核对行写成与专家同构的 Word 表格
//!
//! 分工：行的构造 / 勾选 / 渲染全在 `media_core::report`（纯函数、离线可测）；
//! 这里只负责**落盘**与**调用 WordCraft** 把 HTML 转成真 docx。
//!
//! 实测契约（`wordcraft-cli 0.x`）：
//! `wordcraft-cli convert report.html report.docx` 生成的 docx 里，带边框的
//! `<table>` 保持为表格、中文不乱码（`wordcraft-cli text` 读回是制表符分隔的行）。

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use media_core::{
    MediaError, ReportFile, ReportMeta, ReportRow, not_found_msg, render_html, render_json,
    render_markdown, resolve_tool_bin,
};

/// WordCraft 适配器（只做一件事：HTML → docx）
pub struct WordCraft {
    bin: PathBuf,
    timeout_secs: u64,
}

impl WordCraft {
    /// `explicit` = 设置里的 `media.wordcraft_bin`（空 = `$PATH` → 常见安装位置）
    pub fn new(explicit: Option<&str>, timeout_secs: u64) -> Result<Self, MediaError> {
        let bin = resolve_tool_bin(explicit, "wordcraft-cli")
            .ok_or_else(|| MediaError::NotReady(not_found_msg("wordcraft-cli", explicit)))?;
        Ok(Self { bin, timeout_secs })
    }

    pub fn bin_path(&self) -> &Path {
        &self.bin
    }

    /// HTML → docx（覆盖写；失败返回带 stderr 的错误，不静默）
    pub async fn html_to_docx(&self, html: &Path, docx: &Path) -> Result<(), MediaError> {
        let mut cmd = tokio::process::Command::new(&self.bin);
        cmd.arg("convert")
            .arg(html)
            .arg(docx)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let out = tokio::time::timeout(Duration::from_secs(self.timeout_secs), cmd.output())
            .await
            .map_err(|_| MediaError::Timeout {
                secs: self.timeout_secs,
            })?
            .map_err(|e| MediaError::Internal(format!("启动 {} 失败：{e}", self.bin.display())))?;
        if !out.status.success() {
            let detail = String::from_utf8_lossy(&out.stderr).trim().to_string();
            let detail = if detail.is_empty() {
                format!("退出码 {:?}", out.status.code())
            } else {
                detail
            };
            return Err(MediaError::ToolFailed {
                tool: "wordcraft-cli".into(),
                detail,
            });
        }
        if !docx.is_file() {
            return Err(MediaError::ToolFailed {
                tool: "wordcraft-cli".into(),
                detail: format!("转换成功但没写出 {}", docx.display()),
            });
        }
        Ok(())
    }
}

/// 一次导出的产物路径
#[derive(Debug, Clone, Default)]
pub struct ReportFiles {
    pub json: PathBuf,
    pub markdown: PathBuf,
    pub html: PathBuf,
    pub docx: Option<PathBuf>,
}

/// 写三件套（json / md / html）；docx 由 [`WordCraft::html_to_docx`] 追加
pub fn write_artifacts(
    dir: &Path,
    rows: &[ReportRow],
    meta: &ReportMeta,
) -> Result<ReportFiles, MediaError> {
    std::fs::create_dir_all(dir)
        .map_err(|e| MediaError::Internal(format!("创建报告目录失败：{e}")))?;
    let json = dir.join("report.json");
    let markdown = dir.join("report.md");
    let html = dir.join("report.html");
    std::fs::write(&json, render_json(rows, meta))
        .map_err(|e| MediaError::Internal(format!("写 report.json 失败：{e}")))?;
    std::fs::write(&markdown, render_markdown(rows, meta))
        .map_err(|e| MediaError::Internal(format!("写 report.md 失败：{e}")))?;
    std::fs::write(&html, render_html(rows, meta))
        .map_err(|e| MediaError::Internal(format!("写 report.html 失败：{e}")))?;
    Ok(ReportFiles {
        json,
        markdown,
        html,
        docx: None,
    })
}

/// 读回 `candidates.json`（人工在文件里勾完 `checked` / 改完列之后再导出）
pub fn read_report_file(path: &Path) -> Result<ReportFile, MediaError> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| MediaError::Unsupported(format!("读 {} 失败：{e}", path.display())))?;
    serde_json::from_str(&raw).map_err(|e| {
        MediaError::Unsupported(format!("{} 不是合法 candidates.json：{e}", path.display()))
    })
}

/// 写「推演候选」文件（人工勾选的载体：勾 `checked`、改各列内容都直接改这个文件）
pub fn write_candidates(path: &Path, file: &ReportFile) -> Result<(), MediaError> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let raw = serde_json::to_string_pretty(file)
        .map_err(|e| MediaError::Internal(format!("序列化 candidates.json 失败：{e}")))?;
    std::fs::write(path, raw)
        .map_err(|e| MediaError::Internal(format!("写 {} 失败：{e}", path.display())))
}
