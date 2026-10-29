//! 拼接与回映射：`TextUnit[] ⇄ stitched text ⇄ Error.span ⇄ Annotation[]`
//!
//! 这是多模态层唯一的「坐标学」。检测内核只看到 stitched text，
//! `Error.span` 仍是字节偏移——本模块保证这个语义在往返后不失真。

use std::collections::HashMap;

use shared_model::{Error, error_types};

use crate::model::{Annotation, ApplyMode, MarkKind, MarkStyle, Region, Stitched, TextUnit};

/// 单元间分隔符。**必须单字节**，否则 offset 映射不可逆。
pub const DEFAULT_SEPARATOR: char = '\n';

/// 清洗单元文本：内嵌换行/回车/制表压成空格，保证「分隔符只出现在单元之间」
/// 这一不变量（range 在清洗后重算，所以单元内偏移仍自洽）
fn sanitize(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '\n' | '\r' | '\t' => ' ',
            c => c,
        })
        .collect()
}

/// 拼接为检测输入，同时产出 `unit_id → 字节区间` 反查表
pub fn stitch(units: &[TextUnit]) -> Stitched {
    let sep = DEFAULT_SEPARATOR;
    let mut text = String::new();
    let mut unit_ranges = Vec::with_capacity(units.len());
    for u in units {
        if !text.is_empty() {
            text.push(sep);
        }
        let start = text.len();
        text.push_str(&sanitize(&u.text));
        unit_ranges.push((u.id, [start, text.len()]));
    }
    Stitched {
        text,
        unit_ranges,
        separator: sep.to_string(),
    }
}

/// 便捷版：返回 `(Stitched, 清洗后的单元)`——回写要用清洗后的文本与偏移，
/// 两者必须成对，否则局部 span 会错位
pub fn stitch_units(units: &[TextUnit]) -> (Stitched, Vec<TextUnit>) {
    let cleaned: Vec<TextUnit> = units
        .iter()
        .map(|u| TextUnit {
            text: sanitize(&u.text),
            ..u.clone()
        })
        .collect();
    (stitch(&cleaned), cleaned)
}

/// 字节位置落在哪个单元（含边界；`None` = 落在分隔符外/越界）
pub fn unit_at(st: &Stitched, byte: usize) -> Option<(u32, [usize; 2])> {
    st.unit_ranges
        .iter()
        .find(|(_, r)| r[0] <= byte && byte <= r[1])
        .map(|(id, r)| (*id, *r))
}

/// 与 `[s, en)` 相交的单元（空 span 退化为「包含该点的单元」）
fn overlapping(st: &Stitched, s: usize, en: usize) -> Vec<(u32, [usize; 2])> {
    if s == en {
        return unit_at(st, s).map(|x| vec![x]).unwrap_or_default();
    }
    st.unit_ranges
        .iter()
        .filter(|(_, r)| r[0] < en && s < r[1])
        .map(|(id, r)| (*id, *r))
        .collect()
}

/// 按错误类型给标记样式（视觉一致性：同类错误长得一样）
pub fn style_for(error: &Error) -> MarkStyle {
    let kind = match error.error_type.as_str() {
        error_types::SPELLING => MarkKind::Squiggly,
        error_types::PUNCTUATION => MarkKind::Underline,
        _ => MarkKind::Box,
    };
    MarkStyle {
        kind,
        color: "#e03131".to_string(),
    }
}

/// 计划回写模式：**能力探测优先于用户偏好**
/// （不可定位 → ListOnly；载体不可无损编辑 → InPlace 降级为 Overlay）
pub fn plan_apply(unit: Option<&TextUnit>, region: &Region, pref: ApplyMode) -> ApplyMode {
    if !region.is_locatable() {
        return ApplyMode::ListOnly;
    }
    match pref {
        ApplyMode::InPlace => {
            if unit.is_some_and(|u| u.editable) && region.is_editable_object() {
                ApplyMode::InPlace
            } else {
                ApplyMode::Overlay
            }
        }
        m => m,
    }
}

/// 检测 span 是否**跨 stitched 单元边界**
///
/// 跨单元的 span 意味着判定文本把两个独立文本对象当成了连续文本：视觉转录把「强电间」
/// 切成两块时，`电` + `间` 在载体上并不相邻连续，基于它得出的「句末缺句号」之类判定
/// 是**假相邻**，不是真差错。与 [`remap`] 的「跨单元 → Unlocated」同源，但这是**判定入口**
/// 的口径：调用方据此丢弃并计数告警，而不是留一条「未定位」的假判定在正误表里充数。
///
/// 注意区分：`false` 不等于「判定成立」，只说明文本连续；`true` 一定是假相邻。
pub fn crosses_unit(st: &Stitched, span: [usize; 2]) -> bool {
    overlapping(st, span[0], span[1]).len() > 1
}

/// 按「判定是否跨文本单元」分区：`(可定位的判定, 假相邻判定)`
///
/// 流水线用它把假相邻判定挡在 remap 之前（丢弃 + 计数告警），见 [`crosses_unit`] 的理由。
pub fn partition_cross_unit(st: &Stitched, errors: Vec<Error>) -> (Vec<Error>, Vec<Error>) {
    errors.into_iter().partition(|e| !crosses_unit(st, e.span))
}

/// 回映射：`Error[]`（stitched 字节 span）→ `Annotation[]`（载体落点）
///
/// 规则（不硬猜）：
/// - span 完整落在一个单元内 → 该单元 region + 单元内局部 span；
/// - span 跨单元 → 保留错误，`region = Unlocated` + `locate_warning`
///   （跨文字层的修正无法落到单个原对象，宁可进「未定位」也不画假框）；
/// - span 落在分隔符/越界 → 同上；
/// - 空 span（纯插入建议）→ 挂到包含该点的单元。
/// RunRef 的 `run_range` 语义 = **该标注在单元内的字符区间**。
/// 单元存的是整段（`[0, len]`），标注必须收窄到错误本身——否则正误表的位置与
/// 前端的跳转会指到整段文字上，用户按号去找就找不到。
fn narrow_run_ref(region: &Region, local: [usize; 2]) -> Region {
    match region {
        Region::RunRef { part, .. } => Region::RunRef {
            part: part.clone(),
            run_range: local,
        },
        other => other.clone(),
    }
}

pub fn remap(
    st: &Stitched,
    units: &[TextUnit],
    errors: &[Error],
    pref: ApplyMode,
) -> Vec<Annotation> {
    let by_id: HashMap<u32, &TextUnit> = units.iter().map(|u| (u.id, u)).collect();
    let mut out = Vec::with_capacity(errors.len());
    for (i, e) in errors.iter().enumerate() {
        let (s, en) = (e.span[0], e.span[1]);
        let hits = overlapping(st, s, en);
        let (unit_id, region, local_span, locate_warning) = match hits.len() {
            1 => {
                let (id, r) = hits[0];
                let unit = by_id.get(&id).copied();
                let local = [s - r[0], en - r[0]];
                let region = unit
                    .map(|u| narrow_run_ref(&u.region, local))
                    .filter(|rg| rg.is_locatable())
                    .unwrap_or(Region::Unlocated);
                let warn = unit
                    .filter(|u| u.confidence < 0.6)
                    .map(|u| format!("定位置信度偏低（{:.2}），框仅供参考", u.confidence));
                (id, region, local, warn)
            }
            0 => (
                0,
                Region::Unlocated,
                [0, 0],
                Some("错误位置落在文本单元边界之外，无法定位到载体".to_string()),
            ),
            n => (
                hits[0].0,
                Region::Unlocated,
                [s - hits[0].1[0], en.saturating_sub(hits[0].1[0])],
                Some(format!("错误跨 {n} 个文本单元，无法落到单个可编辑对象")),
            ),
        };
        let unit = by_id.get(&unit_id).copied();
        let apply = plan_apply(unit, &region, pref);
        out.push(Annotation {
            id: (i + 1) as u32,
            error: e.clone(),
            unit_id,
            local_span,
            region,
            apply,
            style: style_for(e),
            locate_warning,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ExtractSource, LayerKind};

    fn unit(id: u32, text: &str) -> TextUnit {
        TextUnit {
            id,
            text: text.to_string(),
            source: ExtractSource::Structure,
            region: Region::LayerRef {
                layer_id: id,
                kind: LayerKind::Type,
                bounds: Some([0.0, (id as f64) * 40.0, 300.0, 32.0]),
            },
            confidence: 1.0,
            editable: true,
            page: None,
        }
    }

    fn err(text: &str, word: &str, corr: &str) -> Error {
        let start = text.find(word).expect("测试词必须在 stitched text 中");
        Error::new(
            "zh",
            error_types::SPELLING,
            0.9,
            [start, start + word.len()],
            word,
            Some(corr.to_string()),
            "test",
        )
    }

    #[test]
    fn stitch_ranges_are_byte_exact_and_reversible() {
        let units = vec![
            unit(1, "He'llo wrold"),
            unit(2, "第二行 文案"),
            unit(3, "tail"),
        ];
        let st = stitch(&units);
        assert_eq!(st.text, "He'llo wrold\n第二行 文案\ntail");
        assert_eq!(st.unit_ranges.len(), 3);
        for (id, r) in &st.unit_ranges {
            let want = units.iter().find(|u| u.id == *id).unwrap();
            assert_eq!(&st.text[r[0]..r[1]], want.text, "unit {id} 切片不符");
        }
    }

    #[test]
    fn remap_narrows_run_ref_to_the_error_itself() {
        // 段落级单元存的是整段范围（[0, len]）；标注必须收窄到错误本身，
        // 否则正误表的位置会指到整段文字上
        let mut units = vec![unit(1, "I saw a apple here")];
        units[0].region = Region::RunRef {
            part: "第1页".to_string(),
            run_range: [0, 18],
        };
        let st = stitch(&units);
        let e = err(&st.text, "a apple", "an apple");
        let ann = remap(&st, &units, &[e], ApplyMode::ListOnly);
        assert_eq!(ann.len(), 1);
        assert_eq!(
            ann[0].region,
            Region::RunRef {
                part: "第1页".to_string(),
                run_range: [6, 13]
            }
        );
        assert_eq!(ann[0].local_span, [6, 13]);
    }

    #[test]
    fn crosses_unit_flags_fake_adjacency() {
        // 「强」「电间」是两块视觉块：跨块的 span 是假相邻，不得当成连续文本判定
        let units = vec![
            unit(1, "强电"),
            unit(2, "间"),
            unit(3, "联系电话：23119456"),
        ];
        let (st, _) = stitch_units(&units);
        // 单元内的 span → 不跨
        let base = st.unit_ranges[2].1[0];
        assert!(
            !crosses_unit(&st, [base, base + 9]),
            "单元内 span 不得判为跨单元"
        );
        // 跨越两块（含分隔符）→ 假相邻
        let b1 = st.unit_ranges[0].1[0];
        let e2 = st.unit_ranges[1].1[1];
        assert!(crosses_unit(&st, [b1, e2]), "跨块的 span 必须判为假相邻");
        // 空 span（纯插入建议）落在单元内 → 不算跨
        assert!(!crosses_unit(&st, [base, base]));
    }

    #[test]
    fn partition_cross_unit_drops_fake_adjacency() {
        // 「强电」+「间」是两块：跨块的判定（原文里带分隔符）出局，单元内的判定留下
        let units = vec![
            unit(1, "强电"),
            unit(2, "间"),
            unit(3, "联系电话：23119456"),
        ];
        let (st, _) = stitch_units(&units);
        let fake = err(&st.text, "电\n间", "改为「强电间」");
        let real = err(&st.text, "23119456", "改为「2311-9456」");
        let (kept, dropped) = partition_cross_unit(&st, vec![real.clone(), fake.clone()]);
        assert_eq!(kept.len(), 1, "单元内判定必须留下");
        assert_eq!(kept[0].original, "23119456");
        assert_eq!(dropped.len(), 1, "跨块判定必须出局并可计数");
        assert_eq!(dropped[0].original, "电\n间");
    }

    #[test]
    fn remap_maps_cjk_span_back_to_unit_local_offset() {
        let units = vec![unit(1, "术前签字及探视"), unit(2, "或血压心率增高影响手术")];
        let (st, cleaned) = stitch_units(&units);
        let errors = vec![err(&st.text, "血压心率", "血压、心率")];
        let anns = remap(&st, &cleaned, &errors, ApplyMode::Overlay);
        assert_eq!(anns.len(), 1);
        let a = &anns[0];
        assert_eq!(a.unit_id, 2);
        // 「或」占 3 字节 → 单元内局部偏移从 3 开始
        assert_eq!(a.local_span, [3, 15]);
        let base = st.unit_ranges[1].1[0];
        assert_eq!(
            &st.text[base + a.local_span[0]..base + a.local_span[1]],
            "血压心率"
        );
        assert!(a.region.is_locatable());
        assert_eq!(a.apply, ApplyMode::Overlay);
        assert_eq!(a.style.kind, MarkKind::Squiggly);
    }

    #[test]
    fn remap_span_crossing_units_is_not_fabricated() {
        let units = vec![unit(1, "第一行 abc"), unit(2, "def 第二行")];
        let (st, cleaned) = stitch_units(&units);
        let mut e = err(&st.text, "abc", "xyz");
        e.span = [st.unit_ranges[0].1[0] + 8, st.unit_ranges[1].1[1] - 4];
        let anns = remap(&st, &cleaned, &[e], ApplyMode::Overlay);
        assert_eq!(anns.len(), 1);
        assert!(!anns[0].region.is_locatable());
        assert_eq!(anns[0].apply, ApplyMode::ListOnly);
        assert!(
            anns[0]
                .locate_warning
                .as_deref()
                .unwrap()
                .contains("跨 2 个")
        );
    }

    #[test]
    fn remap_empty_span_attaches_to_containing_unit() {
        let units = vec![unit(1, "请在门诊乘扶梯")];
        let (st, cleaned) = stitch_units(&units);
        let at = st.unit_ranges[0].1[0] + "请在".len();
        let e = Error::new(
            "zh",
            error_types::GRAMMAR,
            0.7,
            [at, at],
            "",
            Some("补出主语".into()),
            "t",
        );
        let anns = remap(&st, &cleaned, &[e], ApplyMode::Overlay);
        assert_eq!(anns[0].unit_id, 1);
        assert_eq!(anns[0].local_span, [6, 6]);
        assert!(anns[0].region.is_locatable());
    }

    #[test]
    fn inplace_downgrades_when_unit_not_editable() {
        let mut u = unit(1, "扁平位图上的文字");
        u.editable = false;
        u.region = Region::ImageBox {
            x: 0.0,
            y: 0.0,
            w: 100.0,
            h: 20.0,
        };
        let (st, cleaned) = stitch_units(&[u]);
        let errors = vec![err(&st.text, "位图", "bitmap")];
        let anns = remap(&st, &cleaned, &errors, ApplyMode::InPlace);
        assert_eq!(anns[0].apply, ApplyMode::Overlay, "不可编辑载体必须降级");
    }

    #[test]
    fn sanitize_keeps_offsets_consistent() {
        let u = unit(1, "第一行\n第二行");
        let (st, cleaned) = stitch_units(&[u]);
        assert_eq!(st.text, "第一行 第二行");
        assert_eq!(cleaned[0].text, "第一行 第二行");
        assert_eq!(
            &st.text[st.unit_ranges[0].1[0]..st.unit_ranges[0].1[1]],
            cleaned[0].text
        );
    }

    #[test]
    fn low_confidence_unit_warns_but_still_locates() {
        let mut u = unit(1, "He'llo wrold");
        u.confidence = 0.4;
        u.source = ExtractSource::Vision;
        let (st, cleaned) = stitch_units(&[u]);
        let errors = vec![err(&st.text, "wrold", "world")];
        let anns = remap(&st, &cleaned, &errors, ApplyMode::Overlay);
        assert!(anns[0].region.is_locatable());
        assert!(anns[0].locate_warning.is_some());
    }
}
