//! 解析错误

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("不支持的文件格式: {0}")]
    UnsupportedFormat(String),
    #[error("文件损坏或无法解析: {0}")]
    Corrupt(String),
    #[error("未提取到正文文本: {0}")]
    Empty(String),
    #[error("PDF 无文本层（扫描版），无法解析：请改用 OCR 预处理或文本版 PDF")]
    NoTextLayer,
}
