//! 手工验证工具：`cargo run -p document-parse --example parse_file -- <文件路径>`
use document_parse::parse_document;

fn main() {
    let path = std::env::args().nth(1).expect("用法: parse_file <文件>");
    let bytes = std::fs::read(&path).expect("读取失败");
    let name = path.rsplit('/').next().unwrap_or(&path).to_string();
    match parse_document(&bytes, &name) {
        Ok(doc) => {
            println!(
                "format={} title={} author={} lang={} chapters={} chars={}",
                doc.format,
                doc.title,
                doc.author,
                doc.lang_hint,
                doc.chapters.len(),
                doc.char_count
            );
            for w in &doc.warnings {
                println!("WARN: {w}");
            }
            for (i, c) in doc.chapters.iter().enumerate() {
                let head: String = c.text.chars().take(80).collect();
                println!("#{i} [{}] {}", c.heading, head.replace('\n', "⏎"));
            }
        }
        Err(e) => println!("ERROR: {e}"),
    }
}
