//! LLM 出站文本 PII 掩码（大模型复核层隐私防线）
//!
//! 送外部 LLM 端点复查前，把手机号 / 身份证号 / 固定电话 / 长数字串（银行卡号等）/
//! 邮箱本地部分替换为**等长** `*`（ASCII 模式只命中 ASCII 字符，
//! `*` 亦为单字节），因此：
//! - 文本**字节长度不变** → [`shared_model::Error::span`] 字节偏移契约不被破坏，
//!   分块 / 定向复查的 span 映射照旧；
//! - 无需回填映射：LLM 看到的是 `*`，产出的错误若落在被掩区域
//!   （`*` 不可能被正确检出）由 [`filter_masked_errors`] 丢弃。
//!
//! 默认开启；`GEC_LLM_MASK_PII=0|false` 关闭（如需要 LLM 复查含号码文本的
//! 排版错误时）。只作用于**送 LLM 的文本**；规则 / L3 / 本地层仍看原文。

/// 掩码结果：掩码后文本 + 被掩字节区间（与原文 span 空间一致）
#[derive(Debug, Clone)]
pub struct PiiMask {
    pub text: String,
    pub ranges: Vec<[usize; 2]>,
}

thread_local! {
    static PATTERNS: Vec<regex::Regex> = vec![
        // 中国居民身份证（18 位，末位可为 X/x）
        regex::Regex::new(r"\d{17}[\dXx]").unwrap(),
        // 中国大陆手机号
        regex::Regex::new(r"1[3-9]\d{9}").unwrap(),
        // 固定电话（区号-号码）
        regex::Regex::new(r"0\d{2,3}-\d{7,8}").unwrap(),
        // 邮箱（整段匹配，仅掩 `@` 之前的本地部分，保留域名供语法上下文）
        regex::Regex::new(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}").unwrap(),
        // 其余长数字串（银行卡号 / 订单号等 13-19 位）
        regex::Regex::new(r"\d{13,19}").unwrap(),
    ];
}

/// 依次匹配各 PII 模式，把命中字节替换为 `*`（等长；所有模式只命中 ASCII）。
/// `regex` 不支持环视，数字模式的「非数字串内部」边界由 [`digit_bounded`] 手工校验
/// （CJK 紧邻数字是常态，不能用 `\b`——CJK 也是 word 字符）；
/// 邮箱整段匹配但只掩 `@` 之前的本地部分。
/// 先身份证/手机再长数字串，避免短模式被长模式二次覆盖。
pub fn mask_pii(text: &str) -> PiiMask {
    let mut bytes = text.as_bytes().to_vec();
    let mut ranges: Vec<[usize; 2]> = Vec::new();
    for (i, re) in PATTERNS.with(|p| p.clone()).iter().enumerate() {
        // 在已掩文本上继续匹配（先前替换的 `*` 不会匹配数字模式）
        let cur = match std::str::from_utf8(&bytes) {
            Ok(s) => s.to_string(),
            Err(_) => break, // 不应发生：只替换 ASCII 字节
        };
        let hit = i == 3; // 邮箱整段匹配，只掩本地部分
        for m in re.find_iter(&cur) {
            let (s, e) = if hit {
                let at = m.as_str().find('@').unwrap_or(m.end() - m.start());
                (m.start(), m.start() + at)
            } else {
                (m.start(), m.end())
            };
            if !hit && !digit_bounded(&cur, s, e) {
                continue;
            }
            for b in &mut bytes[s..e] {
                *b = b'*';
            }
            ranges.push([s, e]);
        }
    }
    ranges.sort_by_key(|r| r[0]);
    // 只替换过 ASCII 字节（各模式只命中 ASCII，`*` 亦单字节），UTF-8 必然保持有效
    let masked = String::from_utf8(bytes).expect("ASCII-only edits preserve UTF-8");
    PiiMask {
        text: masked,
        ranges,
    }
}

/// 数字命中必须不被更大数字串包围（等价于 `(?<!\d)…(?!\d)`）：
/// 紧邻字节非 ASCII 数字即可（CJK / 空格 / 标点 / 串首尾均合法）
fn digit_bounded(s: &str, start: usize, end: usize) -> bool {
    let before_ok = start == 0 || !s.as_bytes()[start - 1].is_ascii_digit();
    let after_ok = end == s.len() || !s.as_bytes()[end].is_ascii_digit();
    before_ok && after_ok
}

/// 掩码是否启用（默认开；`GEC_LLM_MASK_PII=0|false` 关闭）
pub fn masking_enabled() -> bool {
    match std::env::var("GEC_LLM_MASK_PII") {
        Ok(v) => !(v == "0" || v.eq_ignore_ascii_case("false")),
        Err(_) => true,
    }
}

/// 文本需要送 LLM 时取掩码形态；未启用或未命中时返回 `None`（直接用原文）
pub fn maybe_mask(text: &str) -> Option<PiiMask> {
    if !masking_enabled() {
        return None;
    }
    let m = mask_pii(text);
    if m.ranges.is_empty() { None } else { Some(m) }
}

/// 丢弃 span 与任一被掩区间重叠的错误（`*` 区域产出的检出必为噪声）。
/// 返回丢弃条数（§7 dropped 账本 `pii_mask` 桶计数来源）。
pub fn filter_masked_errors(errors: &mut Vec<shared_model::Error>, mask: &PiiMask) -> usize {
    let before = errors.len();
    errors.retain(|e| {
        !mask
            .ranges
            .iter()
            .any(|r| e.span[0] < r[1] && r[0] < e.span[1])
    });
    before - errors.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared_model::Error;

    #[test]
    fn masks_phone_id_landline_length_preserving() {
        let text = "张三 13800138000 身份证 110101199003077777 电话 010-12345678 结束";
        let m = mask_pii(text);
        // 等长 → span 空间与原文一致
        assert_eq!(m.text.len(), text.len());
        assert!(m.text.contains("13800138000") == false);
        assert!(m.text.contains("110101199003077777") == false);
        assert!(m.text.contains("010-12345678") == false);
        assert!(!m.ranges.is_empty());
    }

    #[test]
    fn keeps_dates_and_short_numbers_intact() {
        let text = "2024-01-01 会议 12 人，编号 1001 无问题";
        let m = mask_pii(text);
        assert_eq!(m.text, text, "日期 / 短数字不应被掩");
        assert!(m.ranges.is_empty());
    }

    #[test]
    fn masks_email_local_part_keeps_domain() {
        let m = mask_pii("联系 zhang.san@example.com 谢谢");
        assert!(m.text.contains("@example.com"));
        assert!(!m.text.contains("zhang.san@"));
        assert_eq!(m.text.len(), "联系 zhang.san@example.com 谢谢".len());
    }

    #[test]
    fn id_card_masked_not_double_counted_as_card() {
        let m = mask_pii("号码11010119900307777X");
        assert!(!m.text.contains('1'));
        // 掩码区域仍是同一段（身份证先掩，卡号模式不再命中 `*`）
        assert_eq!(m.ranges.len(), 1);
    }

    #[test]
    fn filter_drops_errors_over_masked_spans() {
        let text = "电话13800138000有误";
        let m = mask_pii(text);
        // 找到被掩区域做一条越界检出（模拟 LLM 把 `*` 当错误）
        let r = m.ranges[0];
        let mut errors = vec![
            Error::new("zh", "spelling", 0.9, r, "***********", None, "masked"),
            Error::new("zh", "spelling", 0.9, [0, 6], "电话", None, "real"),
        ];
        let dropped = filter_masked_errors(&mut errors, &m);
        assert_eq!(errors.len(), 1);
        assert_eq!(dropped, 1, "丢弃计数必须上报（dropped 账本来源）");
        assert_eq!(errors[0].original, "电话");
    }
}
