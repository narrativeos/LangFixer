//! PDF 解析：pdf-extract 文本层提取（纯 Rust，无外部二进制）
//!
//! 边界：只处理**有文本层**的 PDF；文本层近空（扫描版）→ [`ParseError::NoTextLayer`]。
//! 章节模型：单章（PDF 版面恢复不在范围内，分块交给 chunk_chapters/chunk_text）。

use crate::error::ParseError;

/// 文本层字符数下限：低于此判为扫描版/图片版
const MIN_TEXT_CHARS: usize = 200;

pub fn parse_pdf(bytes: &[u8]) -> Result<(String, Vec<String>), ParseError> {
    let text = pdf_extract::extract_text_from_mem(bytes)
        .map_err(|e| ParseError::Corrupt(format!("PDF 解析失败: {e}")))?;
    // 页分隔符规整为段落分隔
    let text = text.replace('\x0c', "\n\n").replace("\r\n", "\n");
    let chars = text.chars().filter(|c| !c.is_whitespace()).count();
    if chars < MIN_TEXT_CHARS {
        return Err(ParseError::NoTextLayer);
    }
    Ok((text, Vec::new()))
}
