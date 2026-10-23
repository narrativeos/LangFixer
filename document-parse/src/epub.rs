//! EPUB 解析：container.xml → OPF（元数据 + manifest + spine）→ 按 spine 顺序抽 XHTML
//!
//! 章节模型 = spine 阅读顺序（跳过 linear="no"），一个文档一章。
//! 解压防护：单条目声明大小上限（zip 炸弹防护，与资源包导入同一策略）。

use std::collections::HashMap;
use std::io::Read;

use crate::error::ParseError;
use crate::html::html_to_text;
use crate::xmlscan::{Token, attr, scan, text_content};

/// 单 zip 条目解压上限（语法书 XHTML 远小于此；防 zip 炸弹）
const MAX_ENTRY_BYTES: u64 = 16 * 1024 * 1024;

pub struct EpubDoc {
    pub title: String,
    pub author: String,
    pub lang_hint: String,
    pub chapters: Vec<(String, String)>,
    pub warnings: Vec<String>,
}

pub fn parse_epub(bytes: &[u8]) -> Result<EpubDoc, ParseError> {
    let cursor = std::io::Cursor::new(bytes);
    let mut zip = zip::ZipArchive::new(cursor)
        .map_err(|e| ParseError::Corrupt(format!("EPUB 解压失败: {e}")))?;

    // 条目索引（精确 + 小写回退）
    let mut index: HashMap<String, usize> = HashMap::new();
    for i in 0..zip.len() {
        if let Ok(name) = zip.by_index(i).map(|f| f.name().to_string()) {
            index.insert(name.clone(), i);
            index.entry(name.to_lowercase()).or_insert(i);
        }
    }
    let mut warnings: Vec<String> = Vec::new();

    // ① container.xml → OPF 路径（回退：任意 .opf 条目）
    let mut opf_path = String::new();
    if let Some(xml) = read_entry(&mut zip, &index, &mut warnings, "META-INF/container.xml") {
        opf_path = find_rootfile(&xml).unwrap_or_default();
    }
    if opf_path.is_empty() {
        opf_path = index
            .keys()
            .find(|n| n.ends_with(".opf"))
            .cloned()
            .unwrap_or_default();
    }
    if opf_path.is_empty() {
        return Err(ParseError::Corrupt("EPUB 缺少 OPF 清单文件".into()));
    }
    let opf = read_entry(&mut zip, &index, &mut warnings, &opf_path)
        .ok_or_else(|| ParseError::Corrupt(format!("EPUB 清单读取失败: {opf_path}")))?;

    // ② OPF：元数据 + manifest + spine
    let meta = parse_opf_meta(&opf);
    let (items, spine) = parse_opf_structure(&opf);
    if spine.is_empty() {
        warnings.push("EPUB spine 为空，按 manifest 顺序抽取".to_string());
    }
    let opf_dir = opf_path
        .rsplit_once('/')
        .map(|(d, _)| d.to_string())
        .unwrap_or_default();

    // ③ 按 spine（或 manifest html 项）顺序抽取正文
    let order: Vec<String> = if spine.is_empty() {
        items
            .iter()
            .filter(|(_, mt, _)| mt.contains("html"))
            .map(|(id, _, _)| id.clone())
            .collect()
    } else {
        spine
    };
    let mut chapters: Vec<(String, String)> = Vec::new();
    for id in order {
        let Some((href, media_type)) = items
            .iter()
            .find(|(i, _, _)| *i == id)
            .map(|(_, mt, h)| (h.clone(), mt.clone()))
        else {
            continue;
        };
        if !media_type.contains("html") {
            continue;
        }
        let path = resolve_href(&opf_dir, &href);
        let Some(html) = read_entry(&mut zip, &index, &mut warnings, &path) else {
            continue;
        };
        let doc = html_to_text(&html);
        if doc.text.chars().count() < 20 {
            continue;
        }
        let heading = if doc.heading.is_empty() {
            path.rsplit('/').next().unwrap_or("").to_string()
        } else {
            doc.heading
        };
        chapters.push((heading, doc.text));
    }
    if chapters.is_empty() {
        return Err(ParseError::Empty("EPUB 未提取到任何正文文本".into()));
    }
    Ok(EpubDoc {
        title: meta.0,
        author: meta.1,
        lang_hint: meta.2,
        chapters,
        warnings,
    })
}

/// 读取 zip 条目文本（大小写容错 + 单条目解压上限防 zip 炸弹）
fn read_entry(
    zip: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>,
    index: &HashMap<String, usize>,
    warnings: &mut Vec<String>,
    name: &str,
) -> Option<String> {
    let idx = *index
        .get(name)
        .or_else(|| index.get(&name.to_lowercase()))?;
    let mut f = zip.by_index(idx).ok()?;
    if f.size() > MAX_ENTRY_BYTES {
        warnings.push(format!("跳过超大条目 {}（{} 字节）", name, f.size()));
        return None;
    }
    let mut buf = Vec::new();
    if f.read_to_end(&mut buf).is_err() {
        return None;
    }
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// container.xml → rootfile full-path
fn find_rootfile(xml: &str) -> Option<String> {
    for tok in scan(xml) {
        if let Token::Start { name, attrs, .. } = tok
            && name == "rootfile"
            && let Some(p) = attr(&attrs, "full-path")
        {
            return Some(p.trim_start_matches("./").to_string());
        }
    }
    None
}

/// OPF 元数据：(dc:title, dc:creator, dc:language)
fn parse_opf_meta(opf: &str) -> (String, String, String) {
    let mut out = [String::new(), String::new(), String::new()];
    let mut capture: Option<usize> = None;
    for tok in scan(opf) {
        match tok {
            Token::Start {
                name, self_closing, ..
            } => {
                if capture.is_none() && !self_closing {
                    for (i, tag) in ["dc:title", "dc:creator", "dc:language"].iter().enumerate() {
                        if name == *tag {
                            capture = Some(i);
                            break;
                        }
                    }
                }
            }
            Token::Text(t) => {
                if let Some(i) = capture
                    && out[i].is_empty()
                {
                    out[i] = text_content(t).trim().to_string();
                }
            }
            Token::End { .. } => capture = None,
        }
    }
    (out[0].clone(), out[1].clone(), out[2].clone())
}

/// OPF 结构：manifest items (id, media-type, href) + spine itemref 顺序（跳过 linear=no）
fn parse_opf_structure(opf: &str) -> (Vec<(String, String, String)>, Vec<String>) {
    let mut items: Vec<(String, String, String)> = Vec::new();
    let mut refs: Vec<(String, bool)> = Vec::new();
    for tok in scan(opf) {
        if let Token::Start { name, attrs, .. } = tok {
            match name {
                "item" => {
                    let id = attr(&attrs, "id").unwrap_or("").to_string();
                    let mt = attr(&attrs, "media-type").unwrap_or("").to_string();
                    let href = attr(&attrs, "href").unwrap_or("").to_string();
                    if !id.is_empty() {
                        items.push((id, mt, href));
                    }
                }
                "itemref" => {
                    let idref = attr(&attrs, "idref").unwrap_or("").to_string();
                    let linear = attr(&attrs, "linear").unwrap_or("yes");
                    if !idref.is_empty() {
                        refs.push((idref, linear != "no"));
                    }
                }
                _ => {}
            }
        }
    }
    let spine: Vec<String> = refs
        .into_iter()
        .filter(|(_, keep)| *keep)
        .map(|(i, _)| i)
        .collect();
    (items, spine)
}

/// OPF 目录 + href（相对路径归一化 + 百分号解码）
fn resolve_href(opf_dir: &str, href: &str) -> String {
    let href = percent_decode(href);
    if opf_dir.is_empty() {
        return normalize_path(&href);
    }
    normalize_path(&format!("{opf_dir}/{href}"))
}

fn normalize_path(p: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0usize;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
