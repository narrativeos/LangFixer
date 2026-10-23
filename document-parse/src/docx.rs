//! DOCX 解析：zip → word/document.xml（段落 + 标题样式）+ docProps/core.xml（元数据）
//!
//! 章节模型：按标题样式（HeadingN / 标题N）切章；无标题 = 单章。

use std::io::Read;

use crate::error::ParseError;
use crate::xmlscan::{Token, attr, scan, text_content};

pub struct DocxDoc {
    pub title: String,
    pub author: String,
    pub chapters: Vec<(String, String)>,
    pub warnings: Vec<String>,
}

pub fn parse_docx(bytes: &[u8]) -> Result<DocxDoc, ParseError> {
    let cursor = std::io::Cursor::new(bytes);
    let mut zip = zip::ZipArchive::new(cursor)
        .map_err(|e| ParseError::Corrupt(format!("DOCX 解压失败: {e}")))?;
    let mut warnings: Vec<String> = Vec::new();

    // 元数据（缺失不算错误）
    let (title, author) = match read_entry(&mut zip, &mut warnings, "docProps/core.xml") {
        Some(xml) => read_core_props(&xml),
        None => (String::new(), String::new()),
    };

    let doc_xml = read_entry(&mut zip, &mut warnings, "word/document.xml")
        .ok_or_else(|| ParseError::Corrupt("DOCX 缺少 word/document.xml".into()))?;

    // 段落流：w:p 边界；w:pStyle 判标题；w:t 取文；w:br/w:cr 换行；w:tab 空格
    let mut paragraphs: Vec<(Option<u8>, String)> = Vec::new(); // (标题级别, 文本)
    let mut cur = String::new();
    let mut cur_level: Option<u8> = None;
    let mut in_t = false;
    let mut in_skip = 0usize; // w:instrText（域代码）跳过
    for tok in scan(&doc_xml) {
        match tok {
            Token::Start {
                name,
                attrs,
                self_closing,
            } => match name {
                "w:p" => {
                    cur_level = None;
                    let _ = self_closing;
                }
                "w:pStyle" => {
                    if let Some(v) = attr(&attrs, "w:val") {
                        cur_level = heading_level(v);
                    }
                }
                "w:t" => in_t = !self_closing,
                "w:instrText" => in_skip += if self_closing { 0 } else { 1 },
                "w:br" | "w:cr" => cur.push('\n'),
                "w:tab" => cur.push(' '),
                _ => {}
            },
            Token::End { name } => match name {
                "w:t" => in_t = false,
                "w:instrText" => in_skip = in_skip.saturating_sub(1),
                "w:p" => {
                    let text = text_content(&cur).trim().to_string();
                    if !text.is_empty() || cur_level.is_some() {
                        paragraphs.push((cur_level, text));
                    }
                    cur.clear();
                    cur_level = None;
                }
                _ => {}
            },
            Token::Text(t) => {
                if in_t && in_skip == 0 {
                    cur.push_str(t);
                }
            }
        }
    }
    let tail = text_content(&cur).trim().to_string();
    if !tail.is_empty() {
        paragraphs.push((cur_level, tail));
    }

    // 按标题切章（级别 1-6 均开新章；正文并入当前章）
    let mut chapters: Vec<(String, String)> = Vec::new();
    let mut heading = String::new();
    let mut buf: Vec<String> = Vec::new();
    for (level, text) in paragraphs {
        if let Some(_l) = level {
            if !buf.is_empty() || !heading.is_empty() {
                chapters.push((heading.clone(), buf.join("\n\n")));
            }
            heading = text;
            buf.clear();
        } else if !text.is_empty() {
            buf.push(text);
        }
    }
    if !buf.is_empty() || !heading.is_empty() {
        chapters.push((heading, buf.join("\n\n")));
    }
    chapters.retain(|(_, t)| !t.trim().is_empty());
    if chapters.is_empty() {
        return Err(ParseError::Empty("DOCX 未提取到任何正文文本".into()));
    }
    Ok(DocxDoc {
        title,
        author,
        chapters,
        warnings,
    })
}

/// 读取 zip 条目文本（单条目解压上限）
fn read_entry(
    zip: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>,
    warnings: &mut Vec<String>,
    name: &str,
) -> Option<String> {
    let mut f = zip.by_name(name).ok()?;
    if f.size() > 64 * 1024 * 1024 {
        warnings.push(format!("跳过超大条目 {name}"));
        return None;
    }
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// 标题样式判定：HeadingN / heading N / 标题N / TbN(题注) 之外一律正文
fn heading_level(style: &str) -> Option<u8> {
    let s = style.trim();
    let digits: String = s
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>();
    let digits: String = digits.chars().rev().collect();
    if digits.is_empty() {
        return None;
    }
    let prefix = &s[..s.len() - digits.len()];
    let lower = prefix.to_lowercase();
    if lower.starts_with("heading")
        || lower.starts_with("标题")
        || prefix == "T"
        || prefix == "标题"
    {
        digits.parse::<u8>().ok().filter(|n| (1..=6).contains(n))
    } else {
        None
    }
}

/// docProps/core.xml：dc:title / dc:creator
fn read_core_props(xml: &str) -> (String, String) {
    let mut title = String::new();
    let mut author = String::new();
    let mut capture: Option<&str> = None;
    for tok in scan(xml) {
        match tok {
            Token::Start {
                name, self_closing, ..
            } => {
                if capture.is_none()
                    && !self_closing
                    && (name == "dc:title" || name == "dc:creator")
                {
                    capture = Some(name);
                }
            }
            Token::Text(t) => {
                if let Some(tag) = capture {
                    let v = text_content(t).trim().to_string();
                    if tag == "dc:title" && title.is_empty() {
                        title = v;
                    } else if tag == "dc:creator" && author.is_empty() {
                        author = v;
                    }
                }
            }
            Token::End { .. } => capture = None,
        }
    }
    (title, author)
}
