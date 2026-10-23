//! XHTML/HTML → 纯文本 + 章节标题提取
//!
//! 章节模型：一个 XHTML 文档 = 一章；heading 取文档中第一个 h1/h2/h3 的
//! 文本（无标题则空，由上层回退文件名）。正文按块级元素分段（`\n\n`），
//! 与 `chunk_text` 的段落切分约定一致。

use crate::xmlscan::{Token, scan, text_content};

/// 整棵子树跳过的元素（样式/脚本/元数据/导航噪声）
const SKIP_TAGS: [&str; 10] = [
    "head", "script", "style", "title", "meta", "link", "svg", "nav", "iframe", "object",
];

/// 块级元素：前后各插一个段落分隔
const BLOCK_TAGS: [&str; 24] = [
    "p",
    "div",
    "ul",
    "ol",
    "li",
    "blockquote",
    "section",
    "article",
    "table",
    "tr",
    "dl",
    "dd",
    "dt",
    "pre",
    "figure",
    "figcaption",
    "header",
    "footer",
    "aside",
    "caption",
    "form",
    "body",
    "html",
    "tbody",
];

pub struct HtmlDoc {
    pub heading: String,
    pub text: String,
}

/// XHTML → (heading, text)。宽容解析：未闭合标签、缺 body 均可处理。
pub fn html_to_text(html: &str) -> HtmlDoc {
    let tokens = scan(html);
    let mut out = String::new();
    let mut skip_depth = 0usize;
    let mut heading = String::new();
    let mut heading_depth = 0usize;
    let mut heading_buf = String::new();

    for tok in tokens {
        match tok {
            Token::Start {
                name, self_closing, ..
            } => {
                if SKIP_TAGS.contains(&name) && !self_closing {
                    skip_depth += 1;
                    continue;
                }
                if skip_depth > 0 {
                    continue;
                }
                match name {
                    "h1" | "h2" | "h3" => {
                        if !self_closing {
                            heading_depth += 1;
                        }
                        push_sep(&mut out);
                    }
                    "br" => {
                        if !out.ends_with('\n') {
                            out.push('\n');
                        }
                    }
                    _ => {
                        if BLOCK_TAGS.contains(&name) {
                            push_sep(&mut out);
                        }
                    }
                }
            }
            Token::End { name } => {
                if SKIP_TAGS.contains(&name) && skip_depth > 0 {
                    skip_depth -= 1;
                    continue;
                }
                if skip_depth > 0 {
                    continue;
                }
                match name {
                    "h1" | "h2" | "h3" => {
                        heading_depth = heading_depth.saturating_sub(1);
                        if heading.is_empty() {
                            let h = collapse_spaces(&heading_buf);
                            let h = h.trim().to_string();
                            if !h.is_empty() {
                                heading = h;
                            }
                        }
                        heading_buf.clear();
                        push_sep(&mut out);
                    }
                    _ => {
                        if BLOCK_TAGS.contains(&name) {
                            push_sep(&mut out);
                        }
                    }
                }
            }
            Token::Text(t) => {
                if skip_depth > 0 {
                    continue;
                }
                // HTML 语义：连续空白折叠为单空格
                let decoded = collapse_spaces(&text_content(t));
                if decoded.is_empty() {
                    continue;
                }
                if heading_depth > 0 {
                    heading_buf.push_str(&decoded);
                    heading_buf.push(' ');
                }
                out.push_str(&decoded);
            }
        }
    }

    // 规整：3+ 连续换行 → 2；行首尾空白清理；空段剔除
    let text = normalize_paragraphs(&out);
    HtmlDoc { heading, text }
}

fn push_sep(out: &mut String) {
    if out.is_empty() {
        return;
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    if !out.ends_with("\n\n") {
        out.push('\n');
    }
}

/// 连续空白（含换行/制表）折叠为单空格（保留边界空格：inline 标签间
/// 的空格属于正文；行级多余空格由 normalize_paragraphs 的 trim 清理）
fn collapse_spaces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_space = false;
    for ch in s.chars() {
        if ch.is_whitespace() || ch == '\u{a0}' {
            if !in_space {
                out.push(' ');
                in_space = true;
            }
        } else {
            out.push(ch);
            in_space = false;
        }
    }
    out
}

fn normalize_paragraphs(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut nl = 0usize;
    for ch in s.chars() {
        match ch {
            '\n' => {
                nl += 1;
                if nl <= 2 {
                    out.push('\n');
                }
            }
            _ => {
                nl = 0;
                out.push(ch);
            }
        }
    }
    out.lines()
        .map(|l| l.trim())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}
