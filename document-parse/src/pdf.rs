//! PDF 解析：pdf-extract 文本层提取（纯 Rust，无外部二进制）
//!
//! 边界：只处理**有文本层**的 PDF；文本层近空（扫描版）→ [`ParseError::NoTextLayer`]。
//! 章节模型：单章（PDF 版面恢复不在范围内，分块交给 chunk_chapters/chunk_text）。

use crate::error::ParseError;

/// 文本层字符数下限：低于此判为扫描版/图片版
const MIN_TEXT_CHARS: usize = 200;

pub fn parse_pdf(bytes: &[u8]) -> Result<(String, Vec<String>), ParseError> {
    let text = extract_raw(bytes)?;
    // 页分隔符规整为段落分隔
    let text = text.replace('\x0c', "\n\n").replace("\r\n", "\n");
    let chars = text.chars().filter(|c| !c.is_whitespace()).count();
    if chars < MIN_TEXT_CHARS {
        return Err(ParseError::NoTextLayer);
    }
    Ok((text, Vec::new()))
}

/// 按页提取文本层（多模态审校 A 档，`MULTIMODAL.md` §15.2）：
/// 返回 `pages[0]` = 第 1 页…——**页边界必须保住**，正误表「页码及位置」直接依赖页号。
///
/// 与 [`parse_pdf`] 的另一处区别：不套 200 字阈值（一页通知本身可能只有几十字），
/// 只有**完全无文本**才判扫描版；调用方据此给出明确回退提示，不去猜版面。
pub fn parse_pdf_pages(bytes: &[u8]) -> Result<Vec<String>, ParseError> {
    // `extract_text_from_mem_by_pages` 按页返回，页边界不需要再从分隔符里猜
    let raw = pdf_extract::extract_text_from_mem_by_pages(bytes)
        .map_err(|e| ParseError::Corrupt(format!("PDF 解析失败: {e}")))?;
    let pages: Vec<String> = raw
        .iter()
        .map(|p| p.replace("\r\n", "\n").trim_matches('\n').to_string())
        .collect();
    let chars: usize = pages
        .iter()
        .map(|p| p.chars().filter(|c| !c.is_whitespace()).count())
        .sum();
    if chars == 0 {
        return Err(ParseError::NoTextLayer);
    }
    Ok(pages)
}

fn extract_raw(bytes: &[u8]) -> Result<String, ParseError> {
    pdf_extract::extract_text_from_mem(bytes)
        .map_err(|e| ParseError::Corrupt(format!("PDF 解析失败: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 手写一份 N 页最小 PDF（Helvetica + `Tj`），供页边界测试用
    /// （不依赖仓库外的二进制，也不依赖 datasets/ —— 那些是 gitignore 的本机数据）
    fn minimal_pdf(lines: &[&str]) -> Vec<u8> {
        let n = lines.len();
        let font_id = 3usize;
        let kids: Vec<String> = (0..n).map(|i| format!(" {} 0 R", 4 + 2 * i)).collect();
        let mut objs: Vec<Vec<u8>> = vec![
            b"<</Type/Catalog/Pages 2 0 R>>".to_vec(),
            format!("<</Type/Pages/Kids[{}]/Count {n}>>", kids.join(" ")).into_bytes(),
            b"<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>".to_vec(),
        ];
        for (i, line) in lines.iter().enumerate() {
            objs.push(
                format!(
                    "<</Type/Page/Parent 2 0 R/MediaBox[0 0 300 300]/Contents {} 0 R/Resources<</Font<</F0 {font_id} 0 R>>>>>>",
                    5 + 2 * i
                )
                .into_bytes(),
            );
            let stream = format!("BT /F0 12 Tf 20 200 Td ({line}) Tj ET");
            objs.push(format!("<</Length {}>>stream\n{stream}\nendstream", stream.len()).into_bytes());
        }
        let mut out = Vec::new();
        out.extend_from_slice(b"%PDF-1.4\n");
        let mut offsets = Vec::new();
        for (i, body) in objs.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref = out.len();
        let size = objs.len() + 1;
        out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for off in &offsets {
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!("trailer\n<</Size {size}/Root 1 0 R>>\nstartxref\n{xref}\n%%EOF").as_bytes(),
        );
        out
    }

    #[test]
    fn page_boundaries_are_preserved() {
        let pdf = minimal_pdf(&["PAGE ONE TEXT", "PAGE TWO TEXT"]);
        let pages = parse_pdf_pages(&pdf).expect("两页文本层应可解析");
        assert_eq!(pages.len(), 2, "页号是「页码及位置」的依据，页边界不能丢");
        assert!(pages[0].contains("PAGE ONE"), "{pages:?}");
        assert!(!pages[0].contains("PAGE TWO"), "{pages:?}");
        assert!(pages[1].contains("PAGE TWO"), "{pages:?}");
    }

    #[test]
    fn short_documents_stay_readable_even_though_import_threshold_rejects_them() {
        // 一页通知往往只有几十字：知识库导入的 200 字阈值会判扫描版，
        // 但审校不能因此拒绝——两个入口的阈值必须不同
        let pdf = minimal_pdf(&["Notice: the pool closes at 6pm"]);
        assert!(matches!(parse_pdf(&pdf), Err(ParseError::NoTextLayer)));
        let pages = parse_pdf_pages(&pdf).expect("A 档不该因正文短就判扫描版");
        assert!(pages[0].contains("pool closes"));
    }

    #[test]
    fn empty_text_layer_reports_no_text_layer_not_corrupt() {
        let pdf = minimal_pdf(&[""]);
        assert!(matches!(parse_pdf_pages(&pdf), Err(ParseError::NoTextLayer)));
    }
}
