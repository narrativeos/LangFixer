//! 文档解析层：书本文件（epub / txt / markdown / docx / 可解析 pdf）→ 结构化正文
//!
//! 知识源导入管线（`attribution_engine::ingest`）的输入前置：把「外部智能体
//! 预处理书本」产品化为内置能力——上传文件即得（标题 / 作者 / 章节 / 正文），
//! 之后走既有的分块 → agent 逐块挖掘 → 草稿 → 发布闭环。
//!
//! ```text
//! parse_document(bytes, filename)
//!   → ParsedDocument { format, title, author, lang_hint,
//!                      chapters[{heading, text}], warnings, char_count }
//! ```

mod docx;
mod epub;
mod error;
mod html;
mod pdf;
mod text;
mod xmlscan;

pub use error::ParseError;
// 多模态审校 A 档入口（保住页/段落边界，`MULTIMODAL.md` §15.2）：
// 与 `parse_document`（知识库导入，按标题切章）用途不同，勿混用。
pub use docx::{DocxParagraphs, parse_docx_paragraphs};
pub use pdf::parse_pdf_pages;

/// 解析出的章节（heading 可为空；text 为纯文本/轻 markdown）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ParsedChapter {
    pub heading: String,
    pub text: String,
}

/// 解析结果
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ParsedDocument {
    /// txt / md / epub / docx / pdf
    pub format: &'static str,
    /// 书名（元数据缺失时为空，由用户填写）
    pub title: String,
    /// 作者
    pub author: String,
    /// 语言提示（如 zh-CN / en；空 = 未知）
    pub lang_hint: String,
    /// 章节（epub=spine 顺序；md/docx=标题切章；txt/pdf=单章）
    pub chapters: Vec<ParsedChapter>,
    /// 解析过程的告警（编码回退、缺条目、超大跳过等）
    pub warnings: Vec<String>,
    /// 正文字符总数
    pub char_count: usize,
}

/// 按文件名扩展名 + 魔数解析文档
pub fn parse_document(bytes: &[u8], filename: &str) -> Result<ParsedDocument, ParseError> {
    let fmt = detect_format(bytes, filename);
    let mut warnings: Vec<String> = Vec::new();
    let mut doc = ParsedDocument {
        format: fmt,
        title: String::new(),
        author: String::new(),
        lang_hint: String::new(),
        chapters: Vec::new(),
        warnings: Vec::new(),
        char_count: 0,
    };
    match fmt {
        "epub" => {
            let e = epub::parse_epub(bytes)?;
            doc.title = e.title;
            doc.author = e.author;
            doc.lang_hint = e.lang_hint;
            doc.chapters = e
                .chapters
                .into_iter()
                .map(|(heading, text)| ParsedChapter { heading, text })
                .collect();
            doc.warnings = e.warnings;
        }
        "docx" => {
            let d = docx::parse_docx(bytes)?;
            doc.title = d.title;
            doc.author = d.author;
            doc.chapters = d
                .chapters
                .into_iter()
                .map(|(heading, text)| ParsedChapter { heading, text })
                .collect();
            doc.warnings = d.warnings;
        }
        "pdf" => {
            let (t, w) = pdf::parse_pdf(bytes)?;
            doc.warnings = w;
            doc.chapters.push(ParsedChapter {
                heading: stem(filename),
                text: t,
            });
        }
        "md" => {
            let t = text::decode_text(bytes, &mut warnings);
            doc.chapters = text::chapters_from_markdown(&t)
                .into_iter()
                .map(|(heading, text)| ParsedChapter { heading, text })
                .collect();
            doc.warnings = warnings;
        }
        "txt" => {
            let t = text::decode_text(bytes, &mut warnings);
            doc.chapters.push(ParsedChapter {
                heading: String::new(),
                text: t.trim().to_string(),
            });
            doc.warnings = warnings;
        }
        other => return Err(ParseError::UnsupportedFormat(other.to_string())),
    }
    doc.chapters.retain(|c| !c.text.trim().is_empty());
    if doc.chapters.is_empty() {
        return Err(ParseError::Empty("文档无有效正文".into()));
    }
    doc.char_count = doc.chapters.iter().map(|c| c.text.chars().count()).sum();
    if doc.title.is_empty() {
        doc.title = stem(filename);
    }
    Ok(doc)
}

/// 格式判定：扩展名优先，未知时按魔数嗅探
fn detect_format(bytes: &[u8], filename: &str) -> &'static str {
    let ext = filename
        .rsplit('.')
        .next()
        .map(|e| e.to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "epub" => return "epub",
        "docx" => return "docx",
        "pdf" => return "pdf",
        "md" | "markdown" => return "md",
        "txt" | "text" | "textplain" => return "txt",
        _ => {}
    }
    // 魔数嗅探
    if bytes.starts_with(b"%PDF") {
        return "pdf";
    }
    if bytes.starts_with(b"PK\x03\x04") {
        // zip 家族：含 mimetype 条目（内容 epub）→ epub；含 word/ → docx
        if let Ok(mut zip) = zip::ZipArchive::new(std::io::Cursor::new(bytes)) {
            if let Ok(mut f) = zip.by_name("mimetype") {
                let mut s = String::new();
                if std::io::Read::read_to_string(&mut f, &mut s).is_ok() && s.contains("epub") {
                    return "epub";
                }
            }
            if zip.file_names().any(|n| n.starts_with("word/")) {
                return "docx";
            }
        }
        return "zip";
    }
    // 文本启发：NUL 字节 = 二进制
    if bytes.iter().take(8192).any(|b| *b == 0) {
        return "binary";
    }
    "txt"
}

fn stem(filename: &str) -> String {
    let name = filename.rsplit('/').next().unwrap_or(filename);
    name.rsplit_once('.')
        .map(|(s, _)| s.to_string())
        .unwrap_or_else(|| name.to_string())
}

/// 从标题/文件名生成建议 source_id（小写字母/数字/-/_；CJK 无法转写时回退 src-book）
pub fn suggest_source_id(title: &str, filename: &str) -> String {
    let src = if title.trim().is_empty() {
        stem(filename)
    } else {
        title.trim().to_string()
    };
    let mut out = String::new();
    for ch in src.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.chars().filter(|c| c.is_ascii_alphanumeric()).count() < 3 {
        return "src-book".to_string();
    }
    format!("src-{out}")
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn zip_of(entries: &[(&str, &str)]) -> Vec<u8> {
        use std::io::{Cursor, Write};
        use zip::write::SimpleFileOptions;
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, content) in entries {
            w.start_file(*name, SimpleFileOptions::default()).unwrap();
            w.write_all(content.as_bytes()).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    fn epub_fixture() -> Vec<u8> {
        let container = r#"<?xml version="1.0"?>
<container><rootfiles><rootfile full-path="OEBPS/content.opf"/></rootfiles></container>"#;
        let opf = r#"<?xml version="1.0"?>
<package>
 <metadata><dc:title>测试语法书</dc:title><dc:creator>测试作者</dc:creator><dc:language>en</dc:language></metadata>
 <manifest>
  <item href="text0.html" id="a" media-type="application/xhtml+xml"/>
  <item href="text1.html" id="b" media-type="application/xhtml+xml"/>
  <item href="img.jpg" id="c" media-type="image/jpeg"/>
 </manifest>
 <spine><itemref idref="b"/><itemref idref="a"/><itemref idref="c"/></spine>
</package>"#;
        let html0 = r#"<html><head><title>零</title></head><body>
<h2>冠词</h2><p>定冠词 the 表<em>特指</em>。</p>
<p>错误示例: <b>the</b> apple &amp; an apple.</p></body></html>"#;
        let html1 = r#"<html><body><h2>名词</h2><p>复数变化规则。</p><ul><li>s 结尾</li><li>es 结尾</li></ul></body></html>"#;
        zip_of(&[
            ("mimetype", "application/epub+zip"),
            ("META-INF/container.xml", container),
            ("OEBPS/content.opf", opf),
            ("OEBPS/text0.html", html0),
            ("OEBPS/text1.html", html1),
        ])
    }

    #[test]
    fn epub_spine_order_and_metadata() {
        let doc = parse_document(&epub_fixture(), "book.epub").unwrap();
        assert_eq!(doc.format, "epub");
        assert_eq!(doc.title, "测试语法书");
        assert_eq!(doc.author, "测试作者");
        assert_eq!(doc.lang_hint, "en");
        // spine 顺序：b(text1) 先于 a(text0)；图片条目跳过
        assert_eq!(doc.chapters.len(), 2);
        assert_eq!(doc.chapters[0].heading, "名词");
        assert_eq!(doc.chapters[1].heading, "冠词");
        // 实体解码 + 内联标签剥离
        assert!(doc.chapters[1].text.contains("the apple & an apple"));
        assert!(doc.char_count > 20);
    }

    fn docx_fixture() -> Vec<u8> {
        zip_of(&[(
            "word/document.xml",
            r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Proof</w:t></w:r></w:p>
<w:p><w:r><w:t>The He'llo wrold paragraph.</w:t></w:r></w:p>
<w:p><w:r><w:t>Second wrold paragraph.</w:t></w:r></w:p>
</w:body></w:document>"#,
        )])
    }

    #[test]
    fn docx_paragraph_entry_keeps_paragraphs_and_heading_levels() {
        // 多模态 A 档要的是段落级结构（列表定位都要）；切章入口不能反过来影响它
        let p = parse_docx_paragraphs(&docx_fixture()).expect("段落级解析");
        assert_eq!(p.paragraphs.len(), 3, "段落边界不能丢");
        assert_eq!(p.paragraphs[0], (Some(1), "Proof".to_string()));
        assert_eq!(p.paragraphs[1], (None, "The He'llo wrold paragraph.".to_string()));

        // 知识库导入入口（按标题切章）行为不变
        let d = crate::docx::parse_docx(&docx_fixture()).expect("切章入口");
        assert_eq!(d.chapters.len(), 1);
        assert_eq!(d.chapters[0].0, "Proof");
        assert_eq!(
            d.chapters[0].1,
            "The He'llo wrold paragraph.\n\nSecond wrold paragraph."
        );
    }

    #[test]
    fn docx_paragraph_indexes_keep_empty_paragraphs() {
        // 段落号是给用户的定位口径：空段落被丢掉，后面的「段落N」就会整体前漂一位
        let docx = zip_of(&[(
            "word/document.xml",
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Proof</w:t></w:r></w:p>
<w:p></w:p>
<w:p><w:r><w:t>Third one.</w:t></w:r></w:p>
</w:body></w:document>"#,
        )]);
        let p = parse_docx_paragraphs(&docx).expect("段落级解析");
        assert_eq!(p.paragraphs.len(), 3, "空段落也要占一个序号");
        assert_eq!(p.paragraphs[1], (None, String::new()));
        assert_eq!(p.paragraphs[2], (None, "Third one.".to_string()));
        // 切章入口照旧跳过空段落
        let d = crate::docx::parse_docx(&docx).expect("切章入口");
        assert_eq!(d.chapters[0].1, "Third one.");
    }

    #[test]
    fn epub_magic_sniff_without_extension() {
        let doc = parse_document(&epub_fixture(), "downloaded-file").unwrap();
        assert_eq!(doc.format, "epub");
    }

    #[test]
    fn txt_gbk_fallback_and_md_headings() {
        let (gbk_bytes, _, _) = encoding_rs::GBK.encode("冠词的用法。定冠词表特指。");
        let doc = parse_document(&gbk_bytes, "notes.txt").unwrap();
        assert_eq!(doc.format, "txt");
        assert_eq!(doc.chapters.len(), 1);
        assert!(doc.chapters[0].text.contains("冠词"));
        assert!(doc.warnings.iter().any(|w| w.contains("GBK")));

        let md = "# 第一章\n\n内容甲。\n\n## 第二章\n\n内容乙。\n";
        let doc = parse_document(md.as_bytes(), "book.md").unwrap();
        assert_eq!(doc.format, "md");
        assert_eq!(doc.chapters.len(), 2);
        assert_eq!(doc.chapters[0].heading, "第一章");
        assert!(doc.chapters[1].text.contains("内容乙"));
    }

    #[test]
    fn docx_paragraphs_and_headings() {
        let core = r#"<?xml version="1.0"?>
<cp:coreProperties><dc:title>Word 语法书</dc:title><dc:creator>王某</dc:creator></cp:coreProperties>"#;
        let document = r#"<?xml version="1.0"?>
<w:document><w:body>
<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>时态</w:t></w:r></w:p>
<w:p><w:r><w:t>现在完成时 &amp; 过去时。</w:t></w:r></w:p>
<w:p><w:r><w:t>第二行正文。</w:t></w:r></w:p>
<w:p><w:pPr><w:pStyle w:val="Heading2"/></w:pPr><w:r><w:t>虚拟语气</w:t></w:r></w:p>
<w:p><w:r><w:t>if 从句。</w:t></w:r></w:p>
</w:body></w:document>"#;
        let bytes = zip_of(&[("docProps/core.xml", core), ("word/document.xml", document)]);
        let doc = parse_document(&bytes, "book.docx").unwrap();
        assert_eq!(doc.format, "docx");
        assert_eq!(doc.title, "Word 语法书");
        assert_eq!(doc.author, "王某");
        assert_eq!(doc.chapters.len(), 2);
        assert_eq!(doc.chapters[0].heading, "时态");
        assert!(doc.chapters[0].text.contains("现在完成时 & 过去时"));
        assert_eq!(doc.chapters[1].heading, "虚拟语气");
    }

    #[test]
    fn pdf_garbage_rejected_and_binary_sniff() {
        let err = parse_document(b"%PDF-1.4 broken", "x.pdf").unwrap_err();
        assert!(matches!(err, ParseError::Corrupt(_)));
        let mut bytes = vec![0u8; 64];
        bytes[3] = 1;
        let err = parse_document(&bytes, "x").unwrap_err();
        assert!(matches!(err, ParseError::UnsupportedFormat(_)));
    }

    #[test]
    fn source_id_suggestions() {
        assert_eq!(suggest_source_id("我的语法书", "book.epub"), "src-book");
        assert_eq!(
            suggest_source_id("English Grammar (5th)", "x"),
            "src-english-grammar-5th"
        );
    }
}
