//! txt / markdown 解析：编码探测（UTF-8 → GBK 兜底）+ markdown 标题切章

/// 解码文本：UTF-8（去 BOM）优先，失败回退 GBK（WHATWG 语义，encoding_rs）
pub fn decode_text(bytes: &[u8], warnings: &mut Vec<String>) -> String {
    let body = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    if let Ok(s) = std::str::from_utf8(body) {
        return normalize(s);
    }
    let (cow, _, had_errors) = encoding_rs::GBK.decode(body);
    if had_errors {
        warnings.push("文本非 UTF-8，已按 GBK 解码（个别字符可能失真）".to_string());
    } else {
        warnings.push("文本非 UTF-8，已按 GBK 解码".to_string());
    }
    normalize(&cow)
}

fn normalize(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n")
}

/// markdown：按 ATX 标题（#..######）切章；无标题 = 单章。
/// 章文本保留标题行本身（利于挖掘时的位置溯源）。
pub fn chapters_from_markdown(text: &str) -> Vec<(String, String)> {
    let mut chapters: Vec<(String, String)> = Vec::new();
    let mut heading = String::new();
    let mut buf: Vec<&str> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix('#') {
            // 必须是 ATX 标题：# 后紧跟空白或行尾，且不超过 6 级
            let hashes = rest.len() - rest.trim_start_matches('#').len();
            let after = &rest[hashes..];
            if hashes < 6 && (after.is_empty() || after.starts_with(' ') || after.starts_with('\t'))
            {
                flush(&mut chapters, &mut heading, &mut buf);
                heading = after.trim().to_string();
                buf.push(line);
                continue;
            }
        }
        buf.push(line);
    }
    flush(&mut chapters, &mut heading, &mut buf);
    if chapters.is_empty() {
        chapters.push((String::new(), text.trim().to_string()));
    }
    chapters.retain(|(_, t)| !t.trim().is_empty());
    chapters
}

fn flush(chapters: &mut Vec<(String, String)>, heading: &mut String, buf: &mut Vec<&str>) {
    if buf.iter().all(|l| l.trim().is_empty()) && heading.is_empty() {
        buf.clear();
        return;
    }
    let text = buf.join("\n").trim().to_string();
    if !text.is_empty() || !heading.is_empty() {
        chapters.push((heading.clone(), text));
    }
    heading.clear();
    buf.clear();
}
