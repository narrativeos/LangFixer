//! 宽容 XML/HTML 词法器（document-parse 内部共用）
//!
//! 不追求 XML 合规校验（epub/docx 的现实文件常有未闭合标签），只做
//! 「扫标签 → 取属性 → 取文本」的最小解析：
//! - `Start`：`<name attr="v" ...>`（含自闭合标记）
//! - `End`：`</name>`
//! - `Text`：标签间原始文本（实体解码由消费方调 [`text_content`]）
//! - 注释 / CDATA / 处理指令 / DOCTYPE 一律跳过
//!
//! docx 的 `word/document.xml`、epub 的 OPF / container.xml / XHTML 全部
//! 走这一套解析，避免引入 XML 库依赖。

#[derive(Debug, Clone, PartialEq)]
pub enum Token<'a> {
    Start {
        name: &'a str,
        attrs: Vec<(&'a str, &'a str)>,
        self_closing: bool,
    },
    End {
        name: &'a str,
    },
    Text(&'a str),
}

/// 扫描 XML/HTML 片段为 token 序列（输入为已解码的 &str）
pub fn scan(src: &str) -> Vec<Token<'_>> {
    let bytes = src.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0usize;
    let mut text_start = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            if i > text_start {
                tokens.push(Token::Text(&src[text_start..i]));
            }
            // 注释 / CDATA / PI / DOCTYPE：跳到对应终止符
            if src[i..].starts_with("<!--") {
                i = src[i..]
                    .find("-->")
                    .map(|p| i + p + 3)
                    .unwrap_or(bytes.len());
                text_start = i;
                continue;
            }
            if src[i..].starts_with("<![CDATA[") {
                let end = src[i..]
                    .find("]]>")
                    .map(|p| i + p + 3)
                    .unwrap_or(bytes.len());
                let inner = &src[i + 9..end.saturating_sub(3)];
                if !inner.is_empty() {
                    tokens.push(Token::Text(inner));
                }
                i = end;
                text_start = i;
                continue;
            }
            if src[i..].starts_with("<?") || src[i..].starts_with("<!") {
                i = src[i..].find('>').map(|p| i + p + 1).unwrap_or(bytes.len());
                text_start = i;
                continue;
            }
            // 普通标签：找到闭合 '>'（引号内的 '>' 不算）
            let mut j = i + 1;
            let mut quote = 0u8;
            while j < bytes.len() {
                let c = bytes[j];
                if quote != 0 {
                    if c == quote {
                        quote = 0;
                    }
                } else if c == b'"' || c == b'\'' {
                    quote = c;
                } else if c == b'>' {
                    break;
                }
                j += 1;
            }
            if j >= bytes.len() {
                // 残缺标签：余下全部按文本
                tokens.push(Token::Text(&src[i..]));
                i = bytes.len();
                text_start = i;
                continue;
            }
            let inner = &src[i + 1..j];
            let closing = inner.starts_with('/');
            let body = inner
                .trim_end_matches('/')
                .trim_start_matches('/')
                .trim_end();
            let (name, attrs, self_closing) = parse_tag_body(body, inner.ends_with('/'));
            if closing {
                tokens.push(Token::End { name });
            } else {
                tokens.push(Token::Start {
                    name,
                    attrs,
                    self_closing,
                });
            }
            i = j + 1;
            text_start = i;
            continue;
        }
        i += 1;
    }
    if bytes.len() > text_start {
        tokens.push(Token::Text(&src[text_start..]));
    }
    tokens
}

fn parse_tag_body(body: &str, self_closing: bool) -> (&str, Vec<(&str, &str)>, bool) {
    // name：到第一个空白
    let name_end = body.find(|c: char| c.is_whitespace()).unwrap_or(body.len());
    let name = &body[..name_end];
    let mut attrs = Vec::new();
    let rest = &body[name_end..];
    let b = rest.as_bytes();
    let mut i = 0usize;
    while i < b.len() {
        while i < b.len() && (b[i] as char).is_whitespace() {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        // 属性名
        let start = i;
        while i < b.len() && b[i] != b'=' && !(b[i] as char).is_whitespace() {
            i += 1;
        }
        let key = &rest[start..i];
        if key.is_empty() {
            i += 1;
            continue;
        }
        // 可选 =value（带引号 / 裸值 / 布尔属性）
        let mut k = i;
        while k < b.len() && (b[k] as char).is_whitespace() {
            k += 1;
        }
        if k < b.len() && b[k] == b'=' {
            k += 1;
            while k < b.len() && (b[k] as char).is_whitespace() {
                k += 1;
            }
            if k < b.len() && (b[k] == b'"' || b[k] == b'\'') {
                let q = b[k];
                k += 1;
                let vstart = k;
                while k < b.len() && b[k] != q {
                    k += 1;
                }
                attrs.push((key, &rest[vstart..k]));
                k += 1;
            } else {
                let vstart = k;
                while k < b.len() && !(b[k] as char).is_whitespace() && b[k] != b'>' {
                    k += 1;
                }
                attrs.push((key, &rest[vstart..k]));
            }
            i = k;
        } else {
            attrs.push((key, ""));
            i += 1;
        }
    }
    (name, attrs, self_closing)
}

/// 取属性值（首个匹配；OPF/OOXML 属性名大小写固定）
pub fn attr<'a>(attrs: &[(&'a str, &'a str)], name: &str) -> Option<&'a str> {
    attrs.iter().find(|(k, _)| *k == name).map(|(_, v)| *v)
}

/// 文本内容实体解码（&amp; &lt; &gt; &quot; &apos; &nbsp; 常用命名实体 + &#NN; &#xHH;）
pub fn text_content(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        let tail = &rest[pos + 1..];
        if let Some(semi) = tail.find(';')
            && semi <= 8
        {
            let ent = &tail[..semi];
            let decoded = if let Some(hex) = ent.strip_prefix('#') {
                let num = if let Some(h) = hex.strip_prefix('x').or(hex.strip_prefix('X')) {
                    u32::from_str_radix(h, 16).ok()
                } else {
                    hex.parse::<u32>().ok()
                };
                num.and_then(char::from_u32).map(|c| c.to_string())
            } else {
                match ent {
                    "amp" => Some("&".to_string()),
                    "lt" => Some("<".to_string()),
                    "gt" => Some(">".to_string()),
                    "quot" => Some("\"".to_string()),
                    "apos" => Some("'".to_string()),
                    "nbsp" => Some(" ".to_string()),
                    "mdash" => Some("—".to_string()),
                    "ndash" => Some("–".to_string()),
                    "hellip" => Some("…".to_string()),
                    "lsquo" => Some("‘".to_string()),
                    "rsquo" => Some("’".to_string()),
                    "ldquo" => Some("“".to_string()),
                    "rdquo" => Some("”".to_string()),
                    _ => None,
                }
            };
            if let Some(d) = decoded {
                out.push_str(&d);
                rest = &tail[semi + 1..];
                continue;
            }
        }
        out.push('&');
        rest = tail;
    }
    out.push_str(rest);
    out
}
