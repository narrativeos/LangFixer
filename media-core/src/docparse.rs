//! 简单模式（A 档）的文档解析：`document-parse` 纯 Rust，**零外部协作工具**
//!
//! 与高级模式的分工（`MULTIMODAL.md` §15.1–15.2）：
//! - 这里只回答「这份文档里有哪些可独立校对的段落，分别在第几页 / 第几段」；
//! - 不做版面恢复、不猜坐标——没有可信几何信息时，落点是 `RunRef`（页/段落 + 字符区间），
//!   预览面走只读文档视图，**不画框**（画框需要 `PageBox`，那是 PdfCraft 的活）；
//! - 回写一律 `ListOnly`（简单模式没有回写原语）。
//!
//! 扫描版 PDF（无文本层）在这里如实报错，由上层给出「请用高级模式或图片审校」的提示。

use crate::adapter::MediaError;
use crate::model::{CarrierInfo, CarrierKind, ExtractSource, Region, TextUnit};

/// 解析结果：载体概况 + 段落级单元（页 / 段落结构保住）
#[derive(Debug)]
pub struct ParsedDoc {
    pub info: CarrierInfo,
    pub units: Vec<TextUnit>,
}

/// PDF：按页取文本层 → 段落单元（带页号）
pub fn parse_pdf(bytes: &[u8]) -> Result<ParsedDoc, MediaError> {
    let pages = document_parse::parse_pdf_pages(bytes).map_err(|e| match e {
        document_parse::ParseError::NoTextLayer => MediaError::Unsupported(
            "这份 PDF 没有可提取的文本层（扫描版 / 图片版）。简单模式读不到文字，\
             请改用高级模式（PdfCraft 有 OCR），或把它当图片走图片审校。"
                .to_string(),
        ),
        other => MediaError::Unsupported(format!("PDF 解析失败：{other}")),
    })?;
    let units = pdf_units(&pages);
    if units.is_empty() {
        return Err(MediaError::Unsupported(
            "PDF 文本层解析成功但没有可校对段落".to_string(),
        ));
    }
    Ok(ParsedDoc {
        info: CarrierInfo {
            kind: CarrierKind::Pdf,
            width: None,
            height: None,
            pages: Some(pages.len() as u32),
            text_units: units.len(),
            editable: true,
            warnings: Vec::new(),
        },
        units,
    })
}

/// Word（docx）：段落级解析（保住段落边界与标题级别）
pub fn parse_docx(bytes: &[u8]) -> Result<ParsedDoc, MediaError> {
    let parsed = document_parse::parse_docx_paragraphs(bytes)
        .map_err(|e| MediaError::Unsupported(format!("DOCX 解析失败：{e}")))?;
    let units = docx_units(&parsed.paragraphs);
    if units.is_empty() {
        return Err(MediaError::Unsupported(
            "DOCX 没有可校对段落".to_string(),
        ));
    }
    Ok(ParsedDoc {
        info: CarrierInfo {
            kind: CarrierKind::Docx,
            width: None,
            height: None,
            // 简单模式没有排版引擎，页数不可知——如实留空，不去猜
            pages: None,
            text_units: units.len(),
            editable: true,
            warnings: parsed.warnings,
        },
        units,
    })
}

/// 页文本 → 段落级单元（页号进 `page` 与落点标签）
pub fn pdf_units(pages: &[String]) -> Vec<TextUnit> {
    let mut units: Vec<TextUnit> = Vec::new();
    for (i, page) in pages.iter().enumerate() {
        let page_no = (i + 1) as u32;
        for para in paragraphs(page) {
            units.push(doc_unit(
                units.len() as u32 + 1,
                para,
                &format!("第{page_no}页"),
                Some(page_no),
            ));
        }
    }
    units
}

/// 段落流 → 段落级单元（`_level` = 标题级别 1-6；`None` = 正文）
///
/// 标签用**原始段落序号**（空段落跳过但不改号），否则 UI 上的「段落7」会和文档里的位置对不上。
pub fn docx_units(paragraphs: &[(Option<u8>, String)]) -> Vec<TextUnit> {
    let mut units: Vec<TextUnit> = Vec::new();
    for (i, (_, text)) in paragraphs.iter().enumerate() {
        if text.trim().is_empty() {
            continue;
        }
        units.push(doc_unit(
            units.len() as u32 + 1,
            text.trim().to_string(),
            &format!("段落{}", i + 1),
            None,
        ));
    }
    units
}

fn doc_unit(id: u32, text: String, part: &str, page: Option<u32>) -> TextUnit {
    let chars = text.chars().count();
    TextUnit {
        id,
        text,
        source: ExtractSource::Structure,
        // 段落内字符区间（不是像素）：只读预览的高亮用它，画框不用它
        region: Region::RunRef {
            part: part.to_string(),
            run_range: [0, chars],
        },
        confidence: 1.0,
        editable: true,
        page,
    }
}

/// 页文本切段：空行分段；没有空行就整页一段
/// （不去猜版面——按行切会把句子劈开，反而制造漏检）
fn paragraphs(page: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    for line in page.lines() {
        if line.trim().is_empty() {
            if !cur.is_empty() {
                out.push(cur.join("\n"));
                cur.clear();
            }
        } else {
            cur.push(line.trim_end());
        }
    }
    if !cur.is_empty() {
        out.push(cur.join("\n"));
    }
    out.into_iter().filter(|t| !t.trim().is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pdf_units_keep_page_numbers_and_paragraph_bounds() {
        let pages = vec![
            "Title of page one\n\nFirst para line one\nFirst para line two".to_string(),
            "Page two heading".to_string(),
        ];
        let units = pdf_units(&pages);
        assert_eq!(units.len(), 3, "空行分段：第1页两段、第2页一段");
        assert_eq!(units[0].page, Some(1));
        assert_eq!(units[1].page, Some(1));
        assert_eq!(units[2].page, Some(2));
        // 段落内换行保住（句子不能被劈开）
        assert!(units[1].text.contains("line one\nFirst para line two"));
        assert_eq!(
            units[2].region,
            Region::RunRef {
                part: "第2页".to_string(),
                run_range: [0, 16]
            }
        );
    }

    #[test]
    fn no_blank_lines_means_one_unit_per_page_not_one_per_line() {
        let pages = vec!["a\nb\nc".to_string()];
        let units = pdf_units(&pages);
        assert_eq!(units.len(), 1, "按行切会把句子劈开 → 整页一段");
        assert_eq!(units[0].text, "a\nb\nc");
    }

    #[test]
    fn docx_units_label_by_original_paragraph_index() {
        let units = docx_units(&[
            (Some(1), "Proof".to_string()),
            (None, "  ".to_string()),
            (None, "The He'llo wrold paragraph.".to_string()),
        ]);
        assert_eq!(units.len(), 2);
        assert_eq!(units[0].text, "Proof");
        assert_eq!(units[1].text, "The He'llo wrold paragraph.");
        match &units[1].region {
            Region::RunRef { part, run_range } => {
                assert_eq!(part, "段落3", "跳过空段落不能改段落号");
                assert_eq!(run_range, &[0, 27]);
            }
            other => panic!("落点应是段落字符区间，不是像素框：{other:?}"),
        }
        assert!(units.iter().all(|u| u.confidence == 1.0));
    }

    #[test]
    fn unreadable_input_says_so_instead_of_returning_empty_success() {
        let err = parse_pdf(b"not a pdf at all").unwrap_err();
        assert!(matches!(err, MediaError::Unsupported(_)), "{err}");
        let err = parse_docx(b"not a docx at all").unwrap_err();
        assert!(matches!(err, MediaError::Unsupported(_)), "{err}");
    }
}
