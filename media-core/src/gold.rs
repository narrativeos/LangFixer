//! Gold 集评测：把「专家正误表」变成可打分的基准（`MULTIMODAL.md` §15）
//!
//! 数据形态（`住院告知文字质检正误表.docx` 的表格）：
//! `序号 | 页码及位置 | 错误或待核原文 | 正确或处理建议 | 计错数 | 备注及判定依据`
//!
//! 「不计」的条目（表达优化 / 表述不清）仍计入召回，但权重 0.5——
//! 它们是真实缺陷，只是不作为硬差错。

use serde::{Deserialize, Serialize};

use crate::model::Annotation;

/// 一条专家判定
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoldItem {
    pub no: u32,
    /// 「页码及位置」（原样保留，便于人工核对）
    pub location: String,
    /// 「错误或待核原文」——匹配主键
    pub original: String,
    /// 「正确或处理建议」
    pub suggestion: String,
    /// 「计错数」列：「不计」= false（表达优化 / 表述不清）
    pub counted: bool,
    /// 判定标签（【差错】/【表达优化】/【表述不清楚】）
    pub verdict: String,
    pub note: String,
}

/// 正误表行 → GoldItem（跳过表头与空行；列顺序按上表）
pub fn parse_gold_rows(rows: &[Vec<String>]) -> Vec<GoldItem> {
    let cell =
        |row: &[String], i: usize| row.get(i).cloned().unwrap_or_default().trim().to_string();
    rows.iter()
        .filter_map(|row| {
            let no: u32 = cell(row, 0).parse().ok()?;
            let original = cell(row, 2);
            if original.is_empty() {
                return None;
            }
            let note = cell(row, 5);
            let verdict = note
                .split('】')
                .find(|p| p.starts_with('【'))
                .map(|p| p.trim_start_matches('【').to_string())
                .unwrap_or_else(|| {
                    if cell(row, 4) == "不计" {
                        "未定".to_string()
                    } else {
                        "差错".to_string()
                    }
                });
            Some(GoldItem {
                no,
                location: cell(row, 1),
                original,
                suggestion: cell(row, 3),
                counted: cell(row, 4) != "不计",
                verdict,
                note,
            })
        })
        .collect()
}

/// 匹配用归一化：去空白 + 全角标点转半角（中文校对文本的常见噪声）
pub fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace())
        .map(normalize_char)
        .collect()
}

/// 归一化的全角→半角映射表（[`normalize`] 与案例库索引共用）
const NORMALIZE_PAIRS: &[(char, char)] = &[
    ('，', ','),
    ('。', '.'),
    ('：', ':'),
    ('；', ';'),
    ('（', '('),
    ('）', ')'),
    ('！', '!'),
    ('？', '?'),
    ('“', '"'),
    ('”', '"'),
    ('‘', '\''),
    ('’', '\''),
    ('、', ','),
    ('\u{3000}', ' '),
];

/// 单字符归一化（全角标点 → 半角；空白由调用方过滤）
pub fn normalize_char(c: char) -> char {
    NORMALIZE_PAIRS
        .iter()
        .find(|(f, _)| *f == c)
        .map(|(_, t)| *t)
        .unwrap_or(c)
}

/// 字符二元组集合（中文短文本相似度；逐字 Jaccard 会虚高）
fn bigrams(s: &str) -> std::collections::HashSet<String> {
    let cs: Vec<char> = s.chars().collect();
    if cs.len() == 1 {
        return [cs[0].to_string()].into_iter().collect();
    }
    cs.windows(2).map(|w| w.iter().collect()).collect()
}

/// 归一化后的重叠度 0..1（包含关系直接给 1.0——专家常引整句，我们命中其中片段）
///
/// 但**单字符片段除外**：一个半角标点错误（`original = ","`）与任何含逗号的
/// 整句互为包含，那是巧合不是命中（见 [`MIN_CONTAINMENT_CHARS`]）。
pub fn overlap(a: &str, b: &str) -> f64 {
    let (a, b) = (normalize(a), normalize(b));
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    if (a.contains(&b) || b.contains(&a))
        && a.chars().count().min(b.chars().count()) >= MIN_CONTAINMENT_CHARS
    {
        return 1.0;
    }
    let (ba, bb) = (bigrams(&a), bigrams(&b));
    let inter = ba.intersection(&bb).count() as f64;
    let union = ba.union(&bb).count() as f64;
    if union == 0.0 { 0.0 } else { inter / union }
}

/// 包含式匹配要求较短一方至少这么多个字符
///
/// 标点 / 单个错字的 `original` 只有 1~2 字符，按包含匹配会把整条 gold 白送，
/// 召回率虚高且掩盖真实覆盖问题。这类错误要靠 `Exact`（与 gold 片段完全一致）
/// 或二元组重叠度命中。
pub const MIN_CONTAINMENT_CHARS: usize = 3;

/// 命中方式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchKind {
    /// 归一化后完全一致
    Exact,
    /// 一方包含另一方
    Contained,
    /// 二元组重叠度 ≥ 阈值
    Overlap,
    /// **位置重合**：我们的错误 span 落在 gold 原文在载体文本中的区间内（或反之）
    ///
    /// 专家常引整句（「分时段进入病区报道。」），我们标的是词级片段（「报道」）。
    /// 文本口径下 2 字符片段按包含匹配会白送整条 gold（见 [`MIN_CONTAINMENT_CHARS`]），
    /// 于是词级命中被判不命中——但位置上明明是同一处判定。`Positional` 就是这个
    /// 「同一处」的口径（上限评测里 gold 原文的偏移可精确算出）。
    Positional,
    /// 未命中
    None,
}

/// 两个字节区间的重合度 0..1（较短一方被覆盖的比例）；完全不相交 → `None`
///
/// 用于 [`MatchKind::Positional`]：一方包含另一方时恒为 1.0。
pub fn span_relation(a: (usize, usize), b: (usize, usize)) -> Option<f64> {
    let lo = a.0.max(b.0);
    let hi = a.1.min(b.1);
    if hi <= lo {
        return None;
    }
    let shorter = (a.1 - a.0).min(b.1 - b.0);
    if shorter == 0 {
        return None;
    }
    Some((hi - lo) as f64 / shorter as f64)
}

/// 打分结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoldScore {
    pub gold_total: usize,
    pub gold_counted: usize,
    pub ours: usize,
    pub matched: usize,
    /// 加权召回（计差错权重 1.0，不计条目 0.5）
    pub recall: f64,
    /// 我们产出中被 gold 覆盖的比例
    pub precision: f64,
    /// 每条 gold 的命中情况：`(gold_no, 我们的 annotation_id, 命中方式, 重叠度)`
    pub per_gold: Vec<(u32, Option<u32>, MatchKind, f64)>,
    /// 我们产出中未被 gold 覆盖的条目 id（可能多报，也可能是 gold 未覆盖的真错）
    pub unmatched_ours: Vec<u32>,
}

/// gold 集文件（`datasets/media_gold.json`）的最小视图：只认 `items`，其余字段忽略
#[derive(Debug, Clone, Deserialize)]
pub struct GoldSet {
    #[serde(default)]
    pub lang: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub items: Vec<GoldItem>,
}

/// 解析 gold 集 JSON（导出工具与评测 harness 共用同一份读法）
pub fn parse_gold_set(raw: &str) -> Result<GoldSet, serde_json::Error> {
    serde_json::from_str(raw)
}

/// 对 gold 打分：贪心一对一匹配（重叠度降序，每条 gold 最多配一条我们的结果）
pub fn score(gold: &[GoldItem], anns: &[Annotation], threshold: f64) -> GoldScore {
    let ours: Vec<(u32, String)> = anns
        .iter()
        .map(|a| (a.id, a.error.original.clone()))
        .collect();
    score_texts(gold, &ours, threshold)
}

/// [`score_texts`] 的**带位置**版本：gold 与我们的条目都带载体文本中的字节区间
///
/// 两个口径取其一：
/// - 文本口径（`Exact` / `Contained` / `Overlap`）能匹配的用文本口径；
/// - 文本口径判不上（词级片段 vs 专家整句引用）但**位置重合**的记 `Positional`。
///
/// `gold_spans` 必须与 `gold` 同序同长（上限评测里由构造直接给出）；
/// `None` = 该条 gold 引用的原文没在载体文本中找到（抽取保真度问题），
/// 此时只能走文本口径，**不许**用假 span 蒙命中。
pub fn score_spanned(
    gold: &[GoldItem],
    gold_spans: &[Option<(usize, usize)>],
    ours: &[(u32, String, (usize, usize))],
    threshold: f64,
) -> GoldScore {
    debug_assert_eq!(
        gold.len(),
        gold_spans.len(),
        "gold_spans 必须与 gold 同序同长"
    );
    let mut pairs: Vec<(f64, usize, usize, MatchKind)> = Vec::new();
    for (gi, g) in gold.iter().enumerate() {
        for (ai, (_, text, _)) in ours.iter().enumerate() {
            let Some((r, kind)) = text_match(&g.original, text, threshold) else {
                continue;
            };
            pairs.push((r, gi, ai, kind));
        }
        // `None` = gold 引用的原文没在载体文本里找到（抽取保真度问题）→ 只能走文本口径，
        // 绝不能编个 (0,0) 假装定位（那会把 offset 0 附近的命中白算成复现）
        if let Some(gs) = gold_spans.get(gi).copied().flatten() {
            for (ai, (_, _, asp)) in ours.iter().enumerate() {
                if let Some(pr) = span_relation(gs, *asp) {
                    pairs.push((pr, gi, ai, MatchKind::Positional));
                }
            }
        }
    }
    let ids: Vec<u32> = ours.iter().map(|(id, ..)| *id).collect();
    assemble(gold, &ids, pairs)
}

/// 文本口径匹配：归一化一致 → `Exact`；一方包含另一方（且较短方足够长）→ `Contained`；
/// 二元组重叠度达阈 → `Overlap`；否则 `None`
fn text_match(gold: &str, ours: &str, threshold: f64) -> Option<(f64, MatchKind)> {
    let (gn, an) = (normalize(gold), normalize(ours));
    let r = overlap(gold, ours);
    let kind = if gn == an {
        MatchKind::Exact
    } else if (gn.contains(&an) || an.contains(&gn))
        && gn.chars().count().min(an.chars().count()) >= MIN_CONTAINMENT_CHARS
    {
        MatchKind::Contained
    } else if r >= threshold {
        MatchKind::Overlap
    } else {
        return None;
    };
    Some((r, kind))
}

/// 命中方式的排序偏好（同重叠度下文本口径优先于位置口径）
fn kind_rank(kind: MatchKind) -> u8 {
    match kind {
        MatchKind::Exact => 0,
        MatchKind::Contained => 1,
        MatchKind::Overlap => 2,
        MatchKind::Positional => 3,
        MatchKind::None => 4,
    }
}

/// 加权召回（计差错 1.0，不计 0.5）——[`assemble`] 与 [`merge_scores`] 共用同一把尺子
fn weighted_recall(gold: &[GoldItem], hits: impl Iterator<Item = usize>) -> f64 {
    let weight = |g: &GoldItem| if g.counted { 1.0 } else { 0.5 };
    let w_total: f64 = gold.iter().map(weight).sum();
    if w_total == 0.0 {
        return 0.0;
    }
    let w_hit: f64 = hits.map(|gi| weight(&gold[gi])).sum();
    w_hit / w_total
}

/// 把 gold 引用的原文定位到一段文本（字节区间）；定位不到 → `None`
///
/// 上限评测（整篇 stitched 全文）与真实重演（各载体自己的 stitched 文本）必须共用这个
/// 定位器，否则「导出表几条 / 评测几条」又会分叉。`candidates[i]` 是第 i 条 gold 的候选
/// 写法（通常就是 `[original]`；「三处出现“X”」这类条目由调用方给出引号内文本）。
/// 定位不到是抽取保真度问题 → `None`，让它只能走文本口径；
/// **绝不**编个 `(0,0)` 假装定位（那会把 offset 0 附近白算成命中）。
///
/// 匹配**只容忍空白**（含 stitched 的单元分隔符 `\n`）：视觉块常把一句话切成两块，
/// 不容忍分隔符就把「抽取其实正确」的整句判成未定位——真实载体实测 15 条 gold，
/// 转录里寻得到 12 条，精确字节匹配只定位到 6 条，位置口径与规则回填随之全灭。
/// 容忍仅此一项：归一化（去空白 + 全半角）后必须**逐字符相等**，且返回的仍是
/// **原文字节区间**（含中间那个分隔符），所以位置比对仍然诚实。
pub fn locate_spans(text: &str, candidates: &[Vec<String>]) -> Vec<Option<(usize, usize)>> {
    // 归一化字符 → 原文字节位置的反查表（归一化会去空白，不留表就换不回去）
    let mut norm = String::new();
    let mut map: Vec<(usize, usize, usize)> = Vec::new(); // (norm 内字节, 原文字节起点, 该字符字节长)
    for (b, c) in text.char_indices() {
        if c.is_whitespace() {
            continue;
        }
        map.push((norm.len(), b, c.len_utf8()));
        norm.push(normalize_char(c));
    }
    candidates
        .iter()
        .map(|cands| {
            cands.iter().find_map(|t| {
                let nt = normalize(t);
                if nt.is_empty() {
                    return None;
                }
                let at = norm.find(&nt)?;
                let first = map.binary_search_by_key(&at, |(n, _, _)| *n).ok()?;
                let last = first + nt.chars().count() - 1;
                let (_, sb, _) = map[first];
                let (_, se, el) = map[last];
                Some((sb, se + el))
            })
        })
        .collect()
}

/// 按载体分别打分后的**汇总**：同一条 gold 只记一次，取口径最优者
///
/// 每个载体一套 stitched 坐标，跨载体的 offset 毫无关系——所以只能分别打分再汇总，
/// 不能把不同载体的 span 塞进同一坐标系比对（那是假命中）。
/// 召回 / 精确率公式与 [`assemble`] 一致（同一把尺子），`parts` 的 `per_gold` 必须与
/// `gold` 同序同长（[`score_texts`] / [`score_spanned`] 的产出天然满足）。
pub fn merge_scores(gold: &[GoldItem], parts: &[GoldScore]) -> GoldScore {
    for p in parts {
        debug_assert_eq!(
            p.per_gold.len(),
            gold.len(),
            "per_gold 必须与 gold 同序同长"
        );
    }
    let ours: usize = parts.iter().map(|p| p.ours).sum();
    let mut per_gold: Vec<(u32, Option<u32>, MatchKind, f64)> = Vec::with_capacity(gold.len());
    let mut hit_idx: Vec<usize> = Vec::new();
    for (gi, g) in gold.iter().enumerate() {
        let mut best: Option<(Option<u32>, MatchKind, f64)> = None;
        for p in parts {
            let Some((_, ann, kind, r)) = p.per_gold.get(gi) else {
                continue;
            };
            if *kind == MatchKind::None {
                continue;
            }
            let better = match &best {
                None => true,
                Some((_, bk, br)) => {
                    r > br || ((r - br).abs() < f64::EPSILON && kind_rank(*kind) < kind_rank(*bk))
                }
            };
            if better {
                best = Some((*ann, *kind, *r));
            }
        }
        match best {
            Some((ann, kind, r)) => {
                hit_idx.push(gi);
                per_gold.push((g.no, ann, kind, r));
            }
            None => per_gold.push((g.no, None, MatchKind::None, 0.0)),
        }
    }
    let matched = hit_idx.len();
    let mut unmatched_ours: Vec<u32> = parts
        .iter()
        .flat_map(|p| p.unmatched_ours.iter().copied())
        .collect();
    unmatched_ours.sort_unstable();
    unmatched_ours.dedup();
    GoldScore {
        gold_total: gold.len(),
        gold_counted: gold.iter().filter(|g| g.counted).count(),
        ours,
        matched,
        recall: weighted_recall(gold, hit_idx.iter().copied()),
        precision: if ours == 0 {
            0.0
        } else {
            matched as f64 / ours as f64
        },
        per_gold,
        unmatched_ours,
    }
}

/// 贪心一对一配对 + 加权召回/精确率（`score_texts` 与 `score_spanned` 共用）
fn assemble(
    gold: &[GoldItem],
    ours_ids: &[u32],
    mut pairs: Vec<(f64, usize, usize, MatchKind)>,
) -> GoldScore {
    pairs.sort_by(|x, y| {
        y.0.partial_cmp(&x.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(kind_rank(x.3).cmp(&kind_rank(y.3)))
    });
    let mut gold_used = vec![false; gold.len()];
    let mut ann_used = vec![false; ours_ids.len()];
    let mut best: Vec<(usize, usize, MatchKind, f64)> = Vec::new();
    for (r, gi, ai, kind) in pairs {
        if gold_used[gi] || ann_used[ai] {
            continue;
        }
        gold_used[gi] = true;
        ann_used[ai] = true;
        best.push((gi, ai, kind, r));
    }
    let per_gold = gold
        .iter()
        .enumerate()
        .map(|(gi, g)| {
            best.iter()
                .find(|(x, ..)| *x == gi)
                .map(|(_, ai, k, r)| (g.no, Some(ours_ids[*ai]), *k, *r))
                .unwrap_or((g.no, None, MatchKind::None, 0.0))
        })
        .collect();
    GoldScore {
        gold_total: gold.len(),
        gold_counted: gold.iter().filter(|g| g.counted).count(),
        ours: ours_ids.len(),
        matched: best.len(),
        recall: weighted_recall(gold, best.iter().map(|(gi, ..)| *gi)),
        precision: if ours_ids.is_empty() {
            0.0
        } else {
            best.len() as f64 / ours_ids.len() as f64
        },
        per_gold,
        unmatched_ours: ours_ids
            .iter()
            .enumerate()
            .filter(|(ai, _)| !ann_used[*ai])
            .map(|(_, id)| *id)
            .collect(),
    }
}

/// [`score`] 的文本版：`(我们的条目 id, 条目原文)`
///
/// 报告条目（`report::ReportRow`）没有 `Annotation` 外壳，但打分口径必须完全一致
/// ——否则「重演出的正误表」与「检测到的标注」会出现两套召回数字。
pub fn score_texts(gold: &[GoldItem], ours: &[(u32, String)], threshold: f64) -> GoldScore {
    let mut pairs: Vec<(f64, usize, usize, MatchKind)> = Vec::new();
    for (gi, g) in gold.iter().enumerate() {
        for (ai, (_, text)) in ours.iter().enumerate() {
            if let Some((r, kind)) = text_match(&g.original, text, threshold) {
                pairs.push((r, gi, ai, kind));
            }
        }
    }
    let ids: Vec<u32> = ours.iter().map(|(id, _)| *id).collect();
    assemble(gold, &ids, pairs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ApplyMode, MarkStyle, Region};
    use shared_model::{Error, error_types};

    fn ann(id: u32, original: &str) -> Annotation {
        let mut e = Error::new(
            "zh",
            error_types::SPELLING,
            0.9,
            [0, 3],
            original,
            None,
            "t",
        );
        e.correction = Some("x".into());
        Annotation {
            id,
            error: e,
            unit_id: 1,
            local_span: [0, 3],
            region: Region::Unlocated,
            apply: ApplyMode::ListOnly,
            style: MarkStyle::default(),
            locate_warning: None,
        }
    }

    fn row(no: &str, loc: &str, orig: &str, sugg: &str, count: &str, note: &str) -> Vec<String> {
        [no, loc, orig, sugg, count, note]
            .map(str::to_string)
            .to_vec()
    }

    #[test]
    fn parses_expert_table_rows_and_skips_header() {
        let rows = vec![
            row(
                "序号",
                "页码及位置",
                "错误或待核原文",
                "正确或处理建议",
                "计错数",
                "备注及判定依据",
            ),
            row(
                "3",
                "图片1\n术前签字及探视\n第2项",
                "或血压心率增高影响手术及康复",
                "改为“或导致患者血压升高、心率加快，影响手术及康复”。",
                "—",
                "【差错】\n“血压”“心率”是并列指标，原文未作分隔。",
            ),
            row(
                "5",
                "图片1",
                "门诊乘扶梯至三层手术家属等候大厅等候",
                "建议改为“请在门诊乘扶梯至三层手术家属等候大厅等候”。",
                "不计",
                "【表达优化】\n补出地点介词。",
            ),
        ];
        let gold = parse_gold_rows(&rows);
        assert_eq!(gold.len(), 2, "表头与无序号行必须被跳过");
        assert!(gold[0].counted && gold[0].verdict == "差错");
        assert!(!gold[1].counted && gold[1].verdict == "表达优化");
        assert!(gold[0].location.contains("术前签字"));
    }

    #[test]
    fn matching_survives_punctuation_and_length_differences() {
        assert_eq!(normalize("血压、心率"), normalize("血压,心率"));
        // 专家引整句、我们只命中片段 → Contained
        let gold = vec![GoldItem {
            no: 3,
            location: "图片1".into(),
            original: "或血压心率增高影响手术及康复".into(),
            suggestion: String::new(),
            counted: true,
            verdict: "差错".into(),
            note: String::new(),
        }];
        let anns = vec![ann(1, "血压心率"), ann(2, "完全无关的文本啊")];
        let s = score(&gold, &anns, 0.5);
        assert_eq!(s.matched, 1);
        assert_eq!(s.per_gold[0].2, MatchKind::Contained);
        assert_eq!(s.per_gold[0].1, Some(1));
        assert_eq!(s.unmatched_ours, vec![2]);
        assert!((s.recall - 1.0).abs() < 1e-9);
        assert!((s.precision - 0.5).abs() < 1e-9);
    }

    #[test]
    fn uncounted_items_weight_half() {
        let mk = |no: u32, counted: bool| GoldItem {
            no,
            location: String::new(),
            original: format!("错误{no}"),
            suggestion: String::new(),
            counted,
            verdict: "差错".into(),
            note: String::new(),
        };
        let gold = vec![mk(1, true), mk(2, false)];
        let s = score(&gold, &[ann(1, "错误1")], 0.5);
        assert!(
            (s.recall - 1.0 / 1.5).abs() < 1e-9,
            "只命中计差错项 → 1/1.5"
        );
    }

    #[test]
    fn single_char_punctuation_hit_does_not_match_whole_sentence() {
        // 半角标点错误的 original 只有一个字符：与整句互为包含是巧合，不能算命中
        let gold = vec![GoldItem {
            no: 2,
            location: "图片1".into(),
            original: "当日手术13点来院，或等候医生电话通知。".into(),
            suggestion: String::new(),
            counted: false,
            verdict: "表述不清楚".into(),
            note: String::new(),
        }];
        let s = score(&gold, &[ann(1, ",")], 0.5);
        assert_eq!(s.matched, 0, "单字符不得按包含计分");
        assert_eq!(s.per_gold[0].2, MatchKind::None);
        assert_eq!(s.unmatched_ours, vec![1]);
        // 但同样的标点错误若 gold 就是引这个字符，必须算 Exact
        let strict = vec![GoldItem {
            no: 14,
            location: "图片2".into(),
            original: ",".into(),
            suggestion: "改为全角逗号".into(),
            counted: true,
            verdict: "差错".into(),
            note: String::new(),
        }];
        let s2 = score(&strict, &[ann(1, ",")], 0.5);
        assert_eq!(s2.matched, 1, "gold 引的就是该字符 → Exact");
        assert_eq!(s2.per_gold[0].2, MatchKind::Exact);
    }

    #[test]
    fn one_to_one_matching_prevents_double_count() {
        let gold = vec![GoldItem {
            no: 1,
            location: String::new(),
            original: "血压心率".into(),
            suggestion: String::new(),
            counted: true,
            verdict: "差错".into(),
            note: String::new(),
        }];
        let s = score(&gold, &[ann(1, "血压心率"), ann(2, "血压心率")], 0.5);
        assert_eq!(s.matched, 1, "一条 gold 只能配一条结果");
        assert_eq!(s.unmatched_ours.len(), 1);
    }

    #[test]
    fn span_relation_measures_overlap_of_shorter_side() {
        assert_eq!(span_relation((0, 3), (3, 6)), None, "相邻不算重合");
        assert_eq!(
            span_relation((0, 6), (1, 3)),
            Some(1.0),
            "完全落在 gold 引用内"
        );
        assert!((span_relation((0, 4), (2, 8)).unwrap() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn positional_channel_counts_word_level_hit_inside_gold_quote() {
        let gold = vec![GoldItem {
            no: 13,
            location: "图片2".into(),
            original: "分时段进入病区报道。".into(),
            suggestion: "将「报道」改为「报到」。".into(),
            counted: true,
            verdict: "差错".into(),
            note: String::new(),
        }];
        let text = "请分时段进入病区报道。";
        let g0 = text.find("分时段进入病区报道。").unwrap();
        let gold_spans = vec![Some((g0, g0 + "分时段进入病区报道。".len()))];
        let a0 = text.find("报道").unwrap();
        let ours = vec![(1u32, "报道".to_string(), (a0, a0 + "报道".len()))];

        // 文本口径：2 字符片段不按包含白送，所以判不中
        let by_text = score_texts(&gold, &[(1u32, "报道".to_string())], 0.5);
        assert_eq!(by_text.matched, 0, "文本口径下词级片段不得按包含计分");

        // 位置口径：gold 引用的就是这一句 → 同一处，算命中（标 Positional）
        let by_pos = score_spanned(&gold, &gold_spans, &ours, 0.5);
        assert_eq!(by_pos.matched, 1, "位置重合必须算命中");
        assert_eq!(by_pos.per_gold[0].2, MatchKind::Positional);
        assert!(by_pos.unmatched_ours.is_empty());
        assert!((by_pos.recall - 1.0).abs() < 1e-9);

        // gold 引用在载体文本里找不到（抽取保真度问题）→ 不许用假 span 蒙命中
        let unlocated = score_spanned(&gold, &[None], &ours, 0.5);
        assert_eq!(unlocated.matched, 0, "定位不了的 gold 只能走文本口径");
    }

    #[test]
    fn positional_channel_keeps_disjoint_spans_unmatched() {
        let gold = vec![GoldItem {
            no: 1,
            location: "图片1".into(),
            original: "血压心率".into(),
            suggestion: String::new(),
            counted: true,
            verdict: "差错".into(),
            note: String::new(),
        }];
        // 同一段文字里：gold 指前半，系统命中后半 → 文本也不够像，位置也不重合
        let ours = vec![(1u32, "心率增高".to_string(), (12, 24))];
        let s = score_spanned(&gold, &[Some((0, 12))], &ours, 0.5);
        assert_eq!(s.matched, 0, "位置不重合不得靠文本相似白送");
        assert_eq!(s.per_gold[0].2, MatchKind::None);
        assert_eq!(s.unmatched_ours.len(), 1);
    }

    #[test]
    fn locate_spans_tolerates_stitch_separators_only() {
        // stitched 用 \n 连接视觉块：一句话被切成两块时，gold 整句仍应定位，
        // 且返回的是原文字节区间（含那个分隔符）——位置比对才换得回去
        let text = "请您按病区提示操作，分\n时段进入病区报道。谢谢配合";
        let spans = locate_spans(text, &[vec!["分时段进入病区报道。".to_string()]]);
        let (s, e) = spans[0].expect("跨分隔符的 gold 整句必须能定位");
        assert_eq!(&text[s..e], "分\n时段进入病区报道。");

        // 容忍只此一项：归一化后不相等就不得定位（不是模糊匹配）
        assert!(locate_spans(text, &[vec!["分时段进入病区通知。".to_string()]])[0].is_none());
        // 全半角仍按 normalize 口径
        let spans2 = locate_spans(text, &[vec!["分时段进入病区报道.".to_string()]]);
        assert!(spans2[0].is_some(), "全半角差异不得影响定位");
        // 空候选不得命中（否则会把 offset 0 白送）
        assert!(locate_spans(text, &[vec!["  ".to_string()]])[0].is_none());
    }
}
