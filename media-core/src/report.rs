//! 正误表导出（`MULTIMODAL.md` §18）：把「推演出的标注」变成与专家同构的核对表
//!
//! 专家的验收产物是一张 6 列表格（`住院告知文字质检正误表.docx`）：
//!
//! ```text
//! 序号 | 页码及位置 | 错误或待核原文 | 正确或处理建议 | 计错数 | 备注及判定依据
//! ```
//!
//! 本模块**只做纯函数**：从 [`Annotation`] 造行、按人工勾选裁剪、渲染成
//! HTML / Markdown / JSON。docx 由 `wordcraft-cli convert` 生成（在 api-server 侧）。
//!
//! 两条铁律：
//! 1. **列结构必须与 `gold::parse_gold_rows` 互逆**——我们导出的表能被自己的 gold
//!    解析器读回来，否则「重演」与「评测」是两套口径（有回归测试锁死）。
//! 2. **未定位的条目照实写「未定位」**，绝不编造位置（与 §7 同一立场）。

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};

use shared_model::Error;

use crate::gold::{
    GoldItem, GoldScore, MatchKind, locate_spans, merge_scores, normalize, overlap, score_spanned,
    score_texts, span_relation,
};
use crate::model::{Annotation, Region, Stitched, TextUnit};

/// 与专家正误表完全一致的列名（顺序即 Word 表格列序）
pub const REPORT_COLUMNS: [&str; 6] = [
    "序号",
    "页码及位置",
    "错误或待核原文",
    "正确或处理建议",
    "计错数",
    "备注及判定依据",
];

/// 「计错数」列的两种写法（与专家口径一致：非「不计」即计入差错数）
pub const COUNTED_YES: &str = "计1";
pub const COUNTED_NO: &str = "不计";

/// 备注列里的上下文字段截断长度（不能把整页文字塞进表格）
const CONTEXT_CHARS: usize = 48;

/// 表级元信息（表头下方说明行 + JSON 溯源）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportMeta {
    pub title: String,
    #[serde(default)]
    pub lang: String,
    /// 生成时间（ISO-8601；由调用方给出，纯函数不读时钟）
    #[serde(default)]
    pub generated_at: String,
    /// 载体说明（按表格出现顺序，如「图片1：origin_pic_1.jpg（3072×4096，无文字层）」）
    #[serde(default)]
    pub carriers: Vec<String>,
    /// 文本来源档（structure / vision / ocr）
    #[serde(default)]
    pub extractions: Vec<String>,
    /// B/C 档实际模型名
    #[serde(default)]
    pub models: Vec<String>,
    /// 说明行（人读）
    #[serde(default)]
    pub note: String,
}

impl Default for ReportMeta {
    fn default() -> Self {
        Self {
            title: "文字差错核对表".to_string(),
            lang: "zh".to_string(),
            generated_at: String::new(),
            carriers: vec![],
            extractions: vec![],
            models: vec![],
            note: "本表由 LangFixer 推演生成，「计错数」与「正确或处理建议」以人工勾选结果为准。"
                .to_string(),
        }
    }
}

/// 一行核对结果（既是「推演候选」也是「勾选后的导出行」）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportRow {
    /// 「序号」——导出前按载体顺序重排（1..N）
    pub no: u32,
    /// 「页码及位置」
    pub location: String,
    /// 「错误或待核原文」
    pub original: String,
    /// 「正确或处理建议」
    pub suggestion: String,
    /// 「计错数」
    pub counted: String,
    /// 「备注及判定依据」
    pub note: String,
    // —— 溯源（不进 Word 表格列，进 JSON）——
    #[serde(default)]
    pub job_id: String,
    /// 对应标注 id（= 检测结果的稳定 id，勾选 / 回写都用它）
    pub annotation_id: u32,
    #[serde(default)]
    pub unit_id: u32,
    /// 抽取置信度（A 档恒 1.0）
    #[serde(default)]
    pub confidence: f64,
    /// 文本来源档
    #[serde(default)]
    pub extraction: String,
    /// 检测来源层（rule / llm / statistical …）
    #[serde(default)]
    pub detector: String,
    /// 触发规则 id（规则层时有值）
    ///
    /// 反思中心要靠它分靶子：规则触发了但 gold 未被 credited → 靶子是 `rule_tune`
    /// （规则不对），不是「再补一条词典」。只有 `detector` 粒度不够（看不出哪条规则）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_id: Option<String>,
    /// 是否在载体上定位成功（false = 「未定位」）
    #[serde(default)]
    pub located: bool,
    /// 人工勾选（false = 剔除，不进导出表）
    #[serde(default = "default_true")]
    pub checked: bool,
    /// 命中的 gold 条目号（仅评测 / 重演时填；人工核对时可对照专家表）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gold_no: Option<u32>,
    /// 标注在**所属单元内**的字节区间（与 [`Annotation::local_span`] 同源）
    ///
    /// 位置口径要靠它与 gold 原文换算到同一坐标系比对。这里存单元内坐标而不是
    /// stitched 全局坐标：全局坐标取决于 stitched 用哪一批单元（整篇 / 按载体），
    /// 存全局值会埋「两套坐标系对不上」的雷——换算交给调用方，错了看得见。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_span: Option<[usize; 2]>,
    /// gold 命中口径：`"text"`（文本口径）/ `"positional"`（位置口径）
    ///
    /// 位置口径意味着「同一处判定」而字符串并不相同（专家引整句、我们标词级片段），
    /// **绝不能**与文本口径混成一个数，故单独留痕。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gold_match: Option<String>,
}

fn default_true() -> bool {
    true
}

impl ReportRow {
    /// 6 列文本（Word / Markdown 共用；也是与 gold 解析器互逆的接口）
    pub fn columns(&self) -> Vec<String> {
        vec![
            self.no.to_string(),
            self.location.clone(),
            self.original.clone(),
            self.suggestion.clone(),
            self.counted.clone(),
            self.note.clone(),
        ]
    }

    pub fn counted_bool(&self) -> bool {
        self.counted != COUNTED_NO
    }
}

/// 候选表（`candidates.json` 的形态）：推演结果 + 人工勾选位
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportFile {
    pub meta: ReportMeta,
    pub rows: Vec<ReportRow>,
}

/// 一个载体的标注来源（job 视角的切片）
pub struct CarrierSource<'a> {
    pub job_id: &'a str,
    /// 载体标号（「图片1」/「第2页」），决定「页码及位置」前缀
    pub label: &'a str,
    /// 载体像素尺寸（无则位置退化为原始坐标）
    pub size: Option<(f64, f64)>,
    pub extraction: &'a str,
    pub annotations: &'a [Annotation],
    /// 该载体的文本单元（备注列给出上下文，便于人工核对）
    pub units: &'a [TextUnit],
}

/// 位置描述：`图片1 上部·左` / `第2页 中部·右` / `图片1 未定位`
///
/// 未定位**必须**照实写——与「不画假框」是同一条约束（§7）。
pub fn describe_location(region: &Region, label: &str, size: Option<(f64, f64)>) -> String {
    let bands = |label: &str, x: f64, y: f64, w: f64, h: f64, sw: f64, sh: f64| -> String {
        let (cx, cy) = (x + w / 2.0, y + h / 2.0);
        let v = band(cy / sh, ["上部", "中部", "下部"]);
        let hz = band(cx / sw, ["左", "中", "右"]);
        format!("{label} {v}·{hz}")
    };
    match region {
        Region::Unlocated => format!("{label} 未定位"),
        Region::ImageBox { x, y, w, h } => match size {
            Some((sw, sh)) if sw > 0.0 && sh > 0.0 => bands(label, *x, *y, *w, *h, sw, sh),
            _ => format!("{label} 坐标({}, {})", x.round(), y.round()),
        },
        Region::LayerRef {
            layer_id, bounds, ..
        } => match (bounds, size) {
            (Some([x, y, w, h]), Some((sw, sh))) if sw > 0.0 && sh > 0.0 => {
                bands(label, *x, *y, *w, *h, sw, sh)
            }
            _ => format!("{label} 图层#{layer_id}"),
        },
        Region::PageBox { page, x, y, w, h } => {
            // 分页文档：调用方没给标号时按页号自造，坐标本来就是页内归一化的
            let label = if label.is_empty() {
                format!("第{page}页")
            } else {
                label.to_string()
            };
            bands(&label, *x, *y, *w, *h, 1.0, 1.0)
        }
        Region::RunRef { part, run_range } => {
            format!("{label} {part} 正文 {}-{}", run_range[0], run_range[1])
        }
    }
}

/// 归一化坐标 → 三档位置词（非有限值不猜，落到中间档）
fn band(ratio: f64, names: [&'static str; 3]) -> &'static str {
    if !ratio.is_finite() {
        return names[1];
    }
    if ratio < 1.0 / 3.0 {
        names[0]
    } else if ratio < 2.0 / 3.0 {
        names[1]
    } else {
        names[2]
    }
}

/// 错误类别 → 中文「错误类别」标签（备注列的 `【…】` 前缀）
pub fn category_label(err: &Error) -> String {
    let key = err
        .category
        .clone()
        .or_else(|| err.issue_type.clone())
        .unwrap_or_else(|| err.error_type.clone())
        .to_ascii_lowercase();
    let zh = match key.as_str() {
        "spelling" | "misspelling" => {
            if err.lang.starts_with("zh") {
                "错别字"
            } else {
                "拼写"
            }
        }
        "punctuation" | "typography" => "标点",
        "grammar" | "agreement" | "conjugation" | "tense" | "article" | "word_form" => "语法",
        "word_choice" | "collocation" | "lexical" => "用词",
        "style" | "clarity" | "expression" | "fluency" => "表达优化",
        "missing_component" | "missing_subject" | "omission" => "成分残缺",
        "format" | "spacing" | "capitalization" => "格式",
        "untranslated" => "漏译",
        "inconsistent" => "不一致",
        _ => "其他",
    };
    zh.to_string()
}

/// 「计错数」默认值：表达优化 / 其他 → 不计；硬差错 → 计
///
/// 这只是**默认**——最终由人工勾选改写（专家的「不计」条目也是真实缺陷，
/// 只是不计硬差错，见 `gold.rs` 的 0.5 权重）。
pub fn counted_default(err: &Error) -> bool {
    !matches!(category_label(err).as_str(), "表达优化" | "其他")
}

/// 「正确或处理建议」列文本（无建议时照实说没有，不编造）
fn suggestion_of(err: &Error) -> String {
    match err.correction.as_deref() {
        Some(c) if c.trim().is_empty() => format!("删除「{}」", err.original),
        Some(c) => format!("改为「{c}」"),
        None => "未给出建议，待人工核定".to_string(),
    }
}

fn unit_context(units: &[TextUnit], unit_id: u32) -> Option<String> {
    units
        .iter()
        .find(|u| u.id == unit_id)
        .map(|u| truncate_chars(u.text.trim(), CONTEXT_CHARS))
        .filter(|s| !s.is_empty())
}

fn truncate_chars(s: &str, n: usize) -> String {
    let cs: Vec<char> = s.chars().collect();
    if cs.len() <= n {
        s.to_string()
    } else {
        format!("{}…", cs[..n].iter().collect::<String>())
    }
}

/// 标注 → 核对行（按载体顺序 → 载体内阅读顺序排序，未定位沉底，序号 1..N）
pub fn candidates(sources: &[CarrierSource<'_>]) -> Vec<ReportRow> {
    let mut rows: Vec<ReportRow> = Vec::new();
    for src in sources {
        let mut part: Vec<ReportRow> = src
            .annotations
            .iter()
            .map(|a| {
                let err = &a.error;
                let category = category_label(err);
                let located = a.region.is_locatable();
                let location = describe_location(&a.region, src.label, src.size);
                let det = err.source.clone().unwrap_or_else(|| err.error_type.clone());
                let mut note = format!("【{category}】{}", err.message.trim());
                if let Some(rid) = err.rule_id.as_deref() {
                    note.push_str(&format!(" · 规则 {rid}"));
                }
                note.push_str(&format!(
                    " · 检测 {det} · 置信度 {:.2} · 抽取 {}",
                    err.confidence, src.extraction
                ));
                if !located {
                    note.push_str(" · ⚠ 未定位（仅列表，不在载体上标记）");
                }
                if let Some(ctx) = unit_context(src.units, a.unit_id) {
                    note.push_str(&format!(" · 上下文「{ctx}」"));
                }
                ReportRow {
                    no: 0,
                    location,
                    original: err.original.clone(),
                    suggestion: suggestion_of(err),
                    counted: if counted_default(err) {
                        COUNTED_YES
                    } else {
                        COUNTED_NO
                    }
                    .to_string(),
                    note,
                    job_id: src.job_id.to_string(),
                    annotation_id: a.id,
                    unit_id: a.unit_id,
                    confidence: err.confidence.clamp(0.0, 1.0),
                    extraction: src.extraction.to_string(),
                    detector: det,
                    rule_id: err.rule_id.clone(),
                    located,
                    checked: true,
                    gold_no: None,
                    // 文本坐标与载体定位是两回事：`未定位` 只说明不能在图上画框，
                    // 单元内字节区间仍可用于位置口径比对
                    local_span: Some(a.local_span),
                    gold_match: None,
                }
            })
            .collect();
        part.sort_by(|a, b| match (a.located, b.located) {
            // 未定位沉底；其余保持标注 id 序（= 阅读顺序，见 `vision::vision_units` 编号）
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.annotation_id.cmp(&b.annotation_id),
        });
        rows.append(&mut part);
    }
    renumber(&mut rows);
    rows
}

fn renumber(rows: &mut [ReportRow]) {
    for (i, r) in rows.iter_mut().enumerate() {
        r.no = (i + 1) as u32;
    }
}

/// 人工勾选的覆盖项（按标注 id 定位；UI / 脚本 / 评测都走这里）
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RowOverride {
    pub annotation_id: u32,
    #[serde(default)]
    pub job_id: Option<String>,
    #[serde(default)]
    pub original: Option<String>,
    #[serde(default)]
    pub suggestion: Option<String>,
    /// 「错误类别」——改写备注列的 `【…】` 前缀
    #[serde(default)]
    pub category: Option<String>,
    /// `"计"` / `"计1"` / `"不计"`（宽松）
    #[serde(default)]
    pub counted: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub checked: Option<bool>,
}

/// 应用勾选：`accepted` 为白名单（`None` = 按 `checked` 位并剔除 `rejected`）
pub fn apply_selection(
    rows: &[ReportRow],
    accepted: Option<&[u32]>,
    rejected: &[u32],
    overrides: &[RowOverride],
) -> Vec<ReportRow> {
    let mut out: Vec<ReportRow> = rows
        .iter()
        .filter(|r| match accepted {
            Some(ids) => ids.contains(&r.annotation_id),
            None => r.checked && !rejected.contains(&r.annotation_id),
        })
        .cloned()
        .collect();
    // 白名单模式里「被点名」就等于勾选：否则 UI 勾了却因 candidates.json 里的
    // checked=false 被丢掉，是两套勾选语义打架
    if accepted.is_some() {
        for r in out.iter_mut() {
            r.checked = true;
        }
    }
    for o in overrides {
        if let Some(row) = out.iter_mut().find(|r| {
            r.annotation_id == o.annotation_id
                && o.job_id.as_deref().map(|j| j == r.job_id).unwrap_or(true)
        }) {
            if let Some(v) = &o.original {
                if v != &row.original {
                    // 人工改写「错误或待核原文」后，机器判定的命中口径与文本坐标都不再成立：
                    // 清掉 `gold_match` / `local_span`，绝不留着旧口径冒充「机器验证过」。
                    // `gold_no` 保留作人工对照（读者要的是「对应专家表第几条」），
                    // 但它现在只是人工线索，不是打分依据。
                    row.gold_match = None;
                    row.local_span = None;
                }
                row.original = v.clone();
            }
            if let Some(v) = &o.suggestion {
                row.suggestion = v.clone();
            }
            if let Some(v) = &o.counted {
                row.counted = normalize_counted(v);
            }
            if let Some(v) = &o.category {
                row.note = replace_bracket_prefix(&row.note, v);
            }
            if let Some(v) = &o.note {
                row.note = v.clone();
            }
            if let Some(false) = o.checked {
                row.checked = false;
            }
        }
    }
    out.retain(|r| r.checked);
    renumber(&mut out);
    out
}

fn normalize_counted(v: &str) -> String {
    let t = v.trim();
    if t == COUNTED_NO || t.eq_ignore_ascii_case("no") || t == "否" || t == "0" {
        COUNTED_NO.to_string()
    } else {
        COUNTED_YES.to_string()
    }
}

/// 换掉备注列开头的 `【…】`（人工改类别时保留后面的依据）
fn replace_bracket_prefix(note: &str, category: &str) -> String {
    let tail = match note.split_once('】') {
        Some((_, rest)) => rest.to_string(),
        None => String::new(),
    };
    format!("【{category}】{tail}")
}

/// 与 gold 对齐：命中条目回填 `gold_no`，并返回与评测**完全同口径**的分数
pub fn attach_gold(rows: &mut [ReportRow], gold: &[GoldItem], threshold: f64) -> GoldScore {
    let ours: Vec<(u32, String)> = rows
        .iter()
        .map(|r| (r.annotation_id, r.original.clone()))
        .collect();
    let score = score_texts(gold, &ours, threshold);
    for (no, ann_id) in score
        .per_gold
        .iter()
        .filter_map(|(no, hit, ..)| Some((*no, (*hit)?)))
    {
        if let Some(row) = rows.iter_mut().find(|r| r.annotation_id == ann_id) {
            row.gold_no = Some(no);
        }
    }
    score
}

/// [`attach_gold`] 的**位置口径**版：gold 原文与系统标注换算到同一坐标系后比对
///
/// - `gold_spans[i]`：`gold[i].original` 在该坐标系中的字节区间；定位不到必须传 `None`
///   （抽取保真度问题），**不许**用 `(0,0)` 蒙命中；
/// - `row_spans[i]`：与 `rows[i]` 一一对应，同一坐标系；`None` = 该行没有可用坐标，
///   它就不参与位置口径（不参与 ≠ 判不上，`ours` 里根本不出现）。
///
/// 坐标系由调用方决定（上限评测用整篇 stitched 全文，真实重演用各载体自己的 stitched 文本），
/// 这里不做任何换算——**换算错位比不比对更危险**，所以宁可把换算留在看得见的地方。
///
/// 命中口径写回 `row.gold_match`（`"text"` / `"positional"`）：位置口径是「同一处判定」
/// 而字符串不同，必须与文本口径分开报，不能混成一个召回数。
pub fn attach_gold_spanned(
    rows: &mut [ReportRow],
    gold: &[GoldItem],
    gold_spans: &[Option<(usize, usize)>],
    row_spans: &[Option<(usize, usize)>],
    threshold: f64,
) -> GoldScore {
    assert_eq!(
        gold_spans.len(),
        gold.len(),
        "gold_spans 必须与 gold 一一对应"
    );
    assert_eq!(
        row_spans.len(),
        rows.len(),
        "row_spans 必须与 rows 一一对应"
    );
    let ours: Vec<(u32, String, (usize, usize))> = rows
        .iter()
        .zip(row_spans)
        .filter_map(|(r, sp)| {
            let sp = (*sp)?;
            Some((r.annotation_id, r.original.clone(), sp))
        })
        .collect();
    let score = score_spanned(gold, gold_spans, &ours, threshold);
    for (no, ann, kind, _) in &score.per_gold {
        let Some(ann) = ann else { continue };
        if let Some(row) = rows.iter_mut().find(|r| r.annotation_id == *ann) {
            row.gold_no = Some(*no);
            row.gold_match = Some(
                match kind {
                    MatchKind::Positional => "positional",
                    _ => "text",
                }
                .to_string(),
            );
        }
    }
    score
}

/// 一个载体的重演上下文（真实重演：**每载体一套 stitched 坐标系**）
///
/// `text` 与 `stitched` 必须来自同一批单元（`stitch()` / `stitch_units()` 的成对产出），
/// `rows` 只放该载体的行。把不同载体的行塞进同一套坐标 = **假命中**：
/// A 载体的 offset 100 与 B 载体的 offset 100 毫无关系。
pub struct CarrierReplay<'a> {
    /// 载体标签（日志用，如「图片1」）
    pub label: &'a str,
    /// 该载体 stitched 文本（定位 gold 原文用）
    pub text: &'a str,
    /// 该载体 stitched 布局（把行内 `local_span` 换算成该载体坐标）
    pub stitched: &'a Stitched,
    /// 该载体的导出行（就地写 `gold_no` / `gold_match`）
    pub rows: &'a mut [ReportRow],
}

/// 每载体的口径明细（「表内命中数 == 评测命中数」的断言逐载体核对靠它）
#[derive(Debug, Clone)]
pub struct CarrierScore {
    pub label: String,
    /// 参与位置口径的行数（无坐标的行**不参与**，不是判不上）
    pub ours: usize,
    /// 该载体上被 credited 的 gold 条数
    pub matched: usize,
    /// 其中靠位置口径（而非文本口径）命中的条数
    pub positional: usize,
    /// 该载体上定位不到坐标的 gold 条数（抽取保真度问题，不是漏报）
    pub gold_unlocated: usize,
}

/// 按载体分别 attach 的产出
#[derive(Debug, Clone)]
pub struct CarrierAttach {
    /// 文本口径汇总（各载体分别打分后合并）
    pub text: GoldScore,
    /// 文本 + 位置口径汇总
    pub spanned: GoldScore,
    /// 逐载体明细（与入参同序）
    pub per_carrier: Vec<CarrierScore>,
}

/// 单元内 span → 该 stitched 布局下的字节区间
///
/// 返回 `None` 只发生在**真的没有坐标**时：该行没有 `local_span`、所属单元不在这套布局里
/// （跨载体拿错布局）、`local_span` 超出该单元在此布局中的长度，或该区间里的文本与行原文
/// 对不上（坐标系错配）。最后一条是硬校验：不同载体的 `unit_id` 可能恰好相同、span 也放得下，
/// 这时算出来的坐标是**假的**——宁可不算，不能算成命中。
pub fn stitched_spans(st: &Stitched, rows: &[ReportRow]) -> Vec<Option<(usize, usize)>> {
    let base: HashMap<u32, [usize; 2]> = st.unit_ranges.iter().copied().collect();
    rows.iter()
        .map(|r| {
            let local = r.local_span?;
            let range = *base.get(&r.unit_id)?;
            let len = range[1].checked_sub(range[0])?;
            if local[0] > local[1] || local[1] > len {
                return None;
            }
            let (s, e) = (range[0] + local[0], range[0] + local[1]);
            let slice = st.text.get(s..e)?;
            if normalize(slice) != normalize(&r.original) {
                return None;
            }
            Some((s, e))
        })
        .collect()
}

/// 按载体分别做文本口径 + 位置口径 attach，再汇总
///
/// 为什么必须按载体：①每载体一套 stitched 坐标（见 [`CarrierReplay`]）；
/// ②标注 id 是**每载体各自编号**的，跨载体合并打分时 `annotation_id` 会撞号，
/// gold 可能被挂到另一个载体的行上。分别处理天然避开这两类错。
///
/// 汇总口径（[`merge_scores`]）：同一条 gold 只记一次，取口径最优者——
/// 同一处判定不会因为出现在两个载体的表里而被算两次命中。
pub fn attach_gold_by_carrier(
    carriers: &mut [CarrierReplay<'_>],
    gold: &[GoldItem],
    gold_candidates: &[Vec<String>],
    threshold: f64,
) -> CarrierAttach {
    assert_eq!(
        gold_candidates.len(),
        gold.len(),
        "gold_candidates 必须与 gold 一一对应"
    );
    let mut text_parts: Vec<GoldScore> = Vec::with_capacity(carriers.len());
    let mut span_parts: Vec<GoldScore> = Vec::with_capacity(carriers.len());
    let mut per_carrier: Vec<CarrierScore> = Vec::with_capacity(carriers.len());
    for c in carriers.iter_mut() {
        // 先文本口径（与上限评测同一顺序：文本优先，位置口径补漏）
        let text_part = attach_gold(c.rows, gold, threshold);
        let gold_spans = locate_spans(c.text, gold_candidates);
        let row_spans = stitched_spans(c.stitched, c.rows);
        let span_part = attach_gold_spanned(c.rows, gold, &gold_spans, &row_spans, threshold);
        per_carrier.push(CarrierScore {
            label: c.label.to_string(),
            ours: span_part.ours,
            matched: span_part.matched,
            positional: span_part
                .per_gold
                .iter()
                .filter(|(_, _, kind, _)| *kind == MatchKind::Positional)
                .count(),
            gold_unlocated: gold_spans.iter().filter(|s| s.is_none()).count(),
        });
        text_parts.push(text_part);
        span_parts.push(span_part);
    }
    CarrierAttach {
        text: merge_scores(gold, &text_parts),
        spanned: merge_scores(gold, &span_parts),
        per_carrier,
    }
}

/// 规则归因的来源——**强弱不同，必须留痕**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleVia {
    /// gold 与该行在**同一载体坐标系**里位置重合（强：同一处判定）
    Positional,
    /// gold 原文在此坐标系定位不到（视觉转录改写/漏字），退一步用「该行原文是 gold 原文的
    /// 子串」归因（弱：可能是同一条差错，也可能只是撞词）→ 人工核对优先
    Text,
}

/// 文本重叠归因的下限。
///
/// 真实载体实测给出的数：下限取 2 字符时，`探视` / `办理` / `避免` 这类**两字动词片段**
/// 会撞进 5 条专家整句（「病房工作电话不咨询病情及商讨探视。」里的「探视」≠ 该行那条差错），
/// 于是反思靶子被指向一个毫不相关的规则触发点。3 字符起才够具体；gold 原文太短（< 6 字符）
/// 同样不兜底——那是撞词不是归因。
const MIN_RULE_TEXT_EVIDENCE_CHARS: usize = 3;
const MIN_RULE_GOLD_EVIDENCE_CHARS: usize = 6;

/// [`missed_rule_evidence`] 的便捷版：只要规则 id
pub fn missed_rule_ids(
    rows: &[ReportRow],
    row_spans: &[Option<(usize, usize)>],
    gold: &[GoldItem],
    gold_spans: &[Option<(usize, usize)>],
) -> BTreeMap<u32, String> {
    missed_rule_evidence(rows, row_spans, gold, gold_spans)
        .into_iter()
        .map(|(no, (rid, _))| (no, rid))
        .collect()
}

/// 未命中的 gold 条目 → **触发过但没 credited** 的规则 id（含归因来源）
///
/// 两级判据（强→弱）：
/// 1. **位置重合**：该 gold 原文在此坐标系中有坐标，且存在某行（有坐标、带 `rule_id`、
///    未被 credited 到任何 gold）与它位置重合 → `Positional`；
/// 2. **原文重叠**（兜底）：gold 原文定位不到（视觉转录改写/漏字，属抽取保真度问题），
///    但某行（带 `rule_id`、未 credited）的原文归一化后是该 gold 原文的子串 → `Text`。
///    这一级只在「坐标根本没有」时启用，且标 `Text` 供人工先核对；
///    太短的行原文（< 3 字符）与太短的 gold 原文（< 6 字符）都不兜底——那是撞词不是归因。
///
/// 这类漏报在反思中心的靶子是 `rule_tune`（规则不对），不是「再补一条词典」。
///
/// 单坐标系接口：真实重演按载体分别调用再合并（跨载体坐标毫无关系）。
pub fn missed_rule_evidence(
    rows: &[ReportRow],
    row_spans: &[Option<(usize, usize)>],
    gold: &[GoldItem],
    gold_spans: &[Option<(usize, usize)>],
) -> BTreeMap<u32, (String, RuleVia)> {
    assert_eq!(
        row_spans.len(),
        rows.len(),
        "row_spans 必须与 rows 一一对应"
    );
    assert_eq!(
        gold_spans.len(),
        gold.len(),
        "gold_spans 必须与 gold 一一对应"
    );
    let mut out: BTreeMap<u32, (String, RuleVia)> = BTreeMap::new();
    for (gi, g) in gold.iter().enumerate() {
        if rows.iter().any(|r| r.gold_no == Some(g.no)) {
            continue;
        }
        // ① 位置重合（强）
        if let Some(gs) = gold_spans[gi] {
            for (r, sp) in rows.iter().zip(row_spans) {
                let Some(rs) = *sp else { continue };
                if r.gold_no.is_some() {
                    continue;
                }
                let Some(rid) = r.rule_id.as_deref() else {
                    continue;
                };
                if span_relation(gs, rs).is_some() {
                    out.insert(g.no, (rid.to_string(), RuleVia::Positional));
                    break;
                }
            }
            if out.contains_key(&g.no) {
                continue;
            }
        }
        // ② 原文重叠（弱，兜底）：坐标根本没有时才用，取原文最长（上下文最多）的那行
        let gn = normalize(&g.original);
        if gn.chars().count() < MIN_RULE_GOLD_EVIDENCE_CHARS {
            continue;
        }
        let mut best: Option<(usize, u32, &str)> = None; // (归一化长度, annotation_id, rule_id)
        for r in rows.iter() {
            if r.gold_no.is_some() {
                continue;
            }
            let Some(rid) = r.rule_id.as_deref() else {
                continue;
            };
            let rn = normalize(&r.original);
            let len = rn.chars().count();
            if len < MIN_RULE_TEXT_EVIDENCE_CHARS || !gn.contains(&rn) {
                continue;
            }
            match &best {
                Some((bl, ba, _)) if *bl > len || (*bl == len && *ba <= r.annotation_id) => {}
                _ => best = Some((len, r.annotation_id, rid)),
            }
        }
        if let Some((_, _, rid)) = best {
            out.insert(g.no, (rid.to_string(), RuleVia::Text));
        }
    }
    out
}

/// 一个载体的坐标视图（按载体算四维用；坐标由调用方按该载体布局算好）
pub struct CarrierSpans<'a> {
    pub rows: &'a [ReportRow],
    pub gold_spans: &'a [Option<(usize, usize)>],
    pub row_spans: &'a [Option<(usize, usize)>],
}

/// [`reproduction_spanned`] 的**按载体**版：每载体一套坐标，分别算四维再汇总
///
/// 汇总规则：一条 gold 只算一次（在任一载体上 credited 即算命中），四维按
/// 「任一载体达标」取或——同一处判定不会因为出现在两个载体的表里而被算两次命中。
/// 不能把所有载体的 span 塞进一个坐标系算：那是假命中（见 [`CarrierReplay`]）。
pub fn reproduction_by_carrier(
    carriers: &[CarrierSpans<'_>],
    gold: &[GoldItem],
    threshold: f64,
) -> ReproScore {
    let mut credited: BTreeSet<u32> = BTreeSet::new();
    let mut dims: BTreeMap<u32, (bool, bool, bool, bool)> = BTreeMap::new();
    for c in carriers {
        let ours: Vec<(u32, String, (usize, usize))> = c
            .rows
            .iter()
            .zip(c.row_spans)
            .filter_map(|(r, sp)| {
                let sp = (*sp)?;
                Some((r.annotation_id, r.original.clone(), sp))
            })
            .collect();
        let score = score_spanned(gold, c.gold_spans, &ours, threshold);
        let repro = reproduction_spanned(c.rows, gold, c.gold_spans, c.row_spans, threshold);
        for (no, _, kind, _) in &score.per_gold {
            if *kind == MatchKind::None {
                continue;
            }
            credited.insert(*no);
            let Some((_, o, l, ct, sg)) = repro.per_gold.iter().find(|(n, ..)| *n == *no) else {
                continue;
            };
            let e = dims.entry(*no).or_insert((false, false, false, false));
            e.0 |= *o;
            e.1 |= *l;
            e.2 |= *ct;
            e.3 |= *sg;
        }
    }
    let per_gold: Vec<(u32, bool, bool, bool, bool)> = gold
        .iter()
        .map(|g| {
            let d = dims
                .get(&g.no)
                .copied()
                .unwrap_or((false, false, false, false));
            (g.no, d.0, d.1, d.2, d.3)
        })
        .collect();
    let mut s = ReproScore {
        gold_total: gold.len(),
        matched: credited.len(),
        original_ok: 0,
        location_ok: 0,
        counted_ok: 0,
        suggestion_ok: 0,
        full_ok: 0,
        per_gold,
    };
    for (_, o, l, ct, sg) in &s.per_gold {
        s.original_ok += usize::from(*o);
        s.location_ok += usize::from(*l);
        s.counted_ok += usize::from(*ct);
        s.suggestion_ok += usize::from(*sg);
        s.full_ok += usize::from(*o && *l && *ct && *sg);
    }
    s
}

/// 「复现专家报告」的**四维**口径（逐维给数，不合成一个含糊的百分比）
///
/// 只看「命中没有」会掩盖真实的复现质量：命中了位置却改了不该改的内容、
/// 计错数与专家不一致、建议南辕北辙，都不算复现。四维分别是：
/// ① 原文对齐、② 位置一致、③ 计错数一致、④ 建议接近。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReproScore {
    pub gold_total: usize,
    /// 命中的 gold 条数
    pub matched: usize,
    /// ① 原文对齐：命中行「错误或待核原文」与 gold 原文重叠度 ≥ [`REPRO_ORIGINAL_OK`]
    pub original_ok: usize,
    /// ② 位置一致：命中行与 gold 指向同一载体；「未定位」一律不达标（不谎报位置）
    pub location_ok: usize,
    /// ③ 计错数一致：命中行「计错数」与 gold `counted` 一致
    pub counted_ok: usize,
    /// ④ 建议接近：命中行「正确或处理建议」与 gold 建议重叠度 ≥ [`REPRO_SUGGESTION_OK`]
    ///
    /// 这是**近似**口径（措辞等价的最终判定交人工），不要当成「建议完全一致」。
    pub suggestion_ok: usize,
    /// 四维全达标（严格复现）
    pub full_ok: usize,
    /// 每条 gold 的四维结果：`(no, 原文, 位置, 计错, 建议)`；未命中四项全 false
    pub per_gold: Vec<(u32, bool, bool, bool, bool)>,
}

/// 原文对齐的重叠度门槛
pub const REPRO_ORIGINAL_OK: f64 = 0.6;
/// 建议接近的重叠度门槛（近似口径）
pub const REPRO_SUGGESTION_OK: f64 = 0.35;

/// 复现率打分：先按 gold 对齐（与 [`attach_gold`] 完全同口径），再逐维判定
pub fn reproduction(rows: &[ReportRow], gold: &[GoldItem], threshold: f64) -> ReproScore {
    let ours: Vec<(u32, String)> = rows
        .iter()
        .map(|r| (r.annotation_id, r.original.clone()))
        .collect();
    score_dims(gold, &score_texts(gold, &ours, threshold), rows)
}

/// [`reproduction`] 的**位置口径**版：坐标可用时，位置口径的命中也进四维
///
/// 差别很实际：#13「报道」在文本口径下不命中 → 完全不进四维（计错数维白少一条）；
/// 位置口径命中后它进四维，但「原文对齐」仍不达标（词级片段 vs 专家整句）。
/// 这正是想要的效果：**同一处判定计入「计错数一致」，但不假装「原文一致」**。
/// 坐标口径与 [`attach_gold_spanned`] 完全一致（同一坐标系、同样不参与的行不蒙命中）。
pub fn reproduction_spanned(
    rows: &[ReportRow],
    gold: &[GoldItem],
    gold_spans: &[Option<(usize, usize)>],
    row_spans: &[Option<(usize, usize)>],
    threshold: f64,
) -> ReproScore {
    assert_eq!(
        gold_spans.len(),
        gold.len(),
        "gold_spans 必须与 gold 一一对应"
    );
    assert_eq!(
        row_spans.len(),
        rows.len(),
        "row_spans 必须与 rows 一一对应"
    );
    let ours: Vec<(u32, String, (usize, usize))> = rows
        .iter()
        .zip(row_spans)
        .filter_map(|(r, sp)| {
            let sp = (*sp)?;
            Some((r.annotation_id, r.original.clone(), sp))
        })
        .collect();
    score_dims(
        gold,
        &score_spanned(gold, gold_spans, &ours, threshold),
        rows,
    )
}

/// 四维判定主体：命中关系由调用方给的 [`GoldScore`] 决定（两套口径共用同一套维度判据）
fn score_dims(gold: &[GoldItem], score: &GoldScore, rows: &[ReportRow]) -> ReproScore {
    let mut s = ReproScore {
        gold_total: gold.len(),
        matched: 0,
        original_ok: 0,
        location_ok: 0,
        counted_ok: 0,
        suggestion_ok: 0,
        full_ok: 0,
        per_gold: Vec::new(),
    };
    for g in gold {
        let hit = score
            .per_gold
            .iter()
            .find(|(no, ..)| *no == g.no)
            .and_then(|(_, ann, kind, _)| if *kind == MatchKind::None { None } else { *ann })
            .and_then(|ann| rows.iter().find(|r| r.annotation_id == ann));
        let Some(row) = hit else {
            s.per_gold.push((g.no, false, false, false, false));
            continue;
        };
        s.matched += 1;
        let dims = (
            overlap(&row.original, &g.original) >= REPRO_ORIGINAL_OK,
            same_carrier(&row.location, &g.location),
            row.counted_bool() == g.counted,
            overlap(&row.suggestion, &g.suggestion) >= REPRO_SUGGESTION_OK,
        );
        for (flag, counter) in [
            (dims.0, &mut s.original_ok),
            (dims.1, &mut s.location_ok),
            (dims.2, &mut s.counted_ok),
            (dims.3, &mut s.suggestion_ok),
        ] {
            if flag {
                *counter += 1;
            }
        }
        if dims.0 && dims.1 && dims.2 && dims.3 {
            s.full_ok += 1;
        }
        s.per_gold.push((g.no, dims.0, dims.1, dims.2, dims.3));
    }
    s
}

/// 「同一载体」判定：两侧都能解析出「图片N」且 N 相同
///
/// 任一侧写着「未定位」直接判不达标——位置没把握就不能算复现（与 §7 同一立场）。
pub fn same_carrier(ours: &str, gold: &str) -> bool {
    fn carrier_index(s: &str) -> Option<u32> {
        if s.contains("未定位") {
            return None;
        }
        let pos = s.find("图片")?;
        let digits: String = s[pos + "图片".len()..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        digits.parse().ok()
    }
    match (carrier_index(ours), carrier_index(gold)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// 6 列矩阵（含表头）——可直接喂给 `gold::parse_gold_rows` 读回（互逆性有测试锁死）
pub fn to_columns(rows: &[ReportRow]) -> Vec<Vec<String>> {
    let mut out = vec![REPORT_COLUMNS.iter().map(|s| s.to_string()).collect()];
    out.extend(rows.iter().map(|r| r.columns()));
    out
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Word 友好的 HTML（`wordcraft-cli convert report.html report.docx`）
///
/// 实测：带边框的 `<table>` + 中文经 WordCraft 转 docx 后，表格结构与中文完好。
pub fn render_html(rows: &[ReportRow], meta: &ReportMeta) -> String {
    let row = |cells: &[String], tag: &str| -> String {
        let inner = cells
            .iter()
            .map(|c| format!("<{tag}>{}</{tag}>", html_escape(c).replace('\n', "<br/>")))
            .collect::<Vec<_>>()
            .join("");
        format!("<tr>{inner}</tr>")
    };
    let head = row(
        &REPORT_COLUMNS
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>(),
        "th",
    );
    let body = rows
        .iter()
        .map(|r| row(&r.columns(), "td"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut meta_lines = vec![format!("载体：{}", meta.carriers.join(" · "))];
    let mut ex = format!("文本来源：{}", meta.extractions.join(" / "));
    if !meta.models.is_empty() {
        ex.push_str(&format!("（模型 {}）", meta.models.join(", ")));
    }
    meta_lines.push(ex);
    meta_lines.push(format!("生成时间：{}", meta.generated_at));
    format!(
        "<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\"/><title>{title}</title>\n<style>\n\
body{{font-family:\"Songti SC\",\"SimSun\",serif;font-size:10.5pt}}\n\
h2{{text-align:center;font-size:14pt}}\n\
p.meta{{font-size:9pt;color:#333}}\n\
table{{border-collapse:collapse;width:100%}}\n\
th,td{{border:0.5pt solid #000;padding:4pt;vertical-align:top;font-size:9pt}}\n\
th{{background:#f0f0f0}}\n\
</style></head>\n<body>\n<h2>{title}</h2>\n<p class=\"meta\">{note}<br/>{meta_lines}</p>\n\
<table>\n{head}\n{body}\n</table>\n</body></html>\n",
        title = html_escape(&meta.title),
        note = html_escape(&meta.note),
        meta_lines = meta_lines.join("<br/>"),
        head = head,
        body = body,
    )
}

/// Markdown 版（人读 + diff 友好）
pub fn render_markdown(rows: &[ReportRow], meta: &ReportMeta) -> String {
    let cell = |s: &str| s.replace('|', "\\|").replace('\n', "<br/>");
    let mut out = format!("# {}\n\n{}\n\n", meta.title, meta.note);
    if !meta.carriers.is_empty() {
        out.push_str(&format!("载体：{}\n\n", meta.carriers.join(" · ")));
    }
    out.push_str(&format!("| {} |\n", REPORT_COLUMNS.join(" | ")));
    out.push_str(&format!(
        "|{}|\n",
        vec!["---"; REPORT_COLUMNS.len()].join("|")
    ));
    for r in rows {
        out.push_str(&format!(
            "| {} |\n",
            r.columns()
                .iter()
                .map(|c| cell(c))
                .collect::<Vec<_>>()
                .join(" | ")
        ));
    }
    out
}

/// JSON 版（含全部溯源字段；就是 `candidates.json` 的形态）
pub fn render_json(rows: &[ReportRow], meta: &ReportMeta) -> String {
    serde_json::to_string_pretty(&ReportFile {
        meta: meta.clone(),
        rows: rows.to_vec(),
    })
    .unwrap_or_else(|_| "{{}}".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gold::{GoldItem, MatchKind, parse_gold_rows};
    use crate::model::{ApplyMode, ExtractSource, MarkStyle, Region, TextUnit};
    use crate::stitch::stitch;
    use shared_model::{Error, error_types};

    fn ann(
        id: u32,
        unit_id: u32,
        original: &str,
        correction: Option<&str>,
        category: &str,
        region: Region,
    ) -> Annotation {
        let mut e = Error::new(
            "zh",
            error_types::SPELLING,
            0.9,
            [0, original.len()],
            original,
            correction.map(str::to_string),
            "示例说明",
        );
        e.category = Some(category.to_string());
        e.rule_id = Some("ZH_TEST".into());
        e.source = Some("rule".into());
        Annotation {
            id,
            error: e,
            unit_id,
            local_span: [0, original.len()],
            region,
            apply: ApplyMode::Overlay,
            style: MarkStyle::default(),
            locate_warning: None,
        }
    }

    fn unit(id: u32, text: &str) -> TextUnit {
        TextUnit {
            id,
            text: text.to_string(),
            source: ExtractSource::Vision,
            region: Region::Unlocated,
            confidence: 0.9,
            editable: false,
            page: None,
        }
    }

    fn box_(x: f64, y: f64) -> Region {
        Region::ImageBox {
            x,
            y,
            w: 100.0,
            h: 40.0,
        }
    }

    #[test]
    fn location_uses_reading_bands_and_admits_being_unlocated() {
        let size = Some((300.0, 300.0));
        assert_eq!(
            describe_location(&box_(0.0, 0.0), "图片1", size),
            "图片1 上部·左"
        );
        assert_eq!(
            describe_location(&box_(120.0, 150.0), "图片1", size),
            "图片1 中部·中"
        );
        assert_eq!(
            describe_location(&box_(250.0, 260.0), "图片1", size),
            "图片1 下部·右"
        );
        assert_eq!(
            describe_location(&Region::Unlocated, "图片1", size),
            "图片1 未定位",
            "定位失败必须照实写，不能编造位置"
        );
        // 没有尺寸信息时退回原始坐标，而不是瞎猜三档
        assert_eq!(
            describe_location(&box_(70.0, 90.0), "图片1", None),
            "图片1 坐标(70, 90)"
        );
    }

    #[test]
    fn category_and_counted_defaults_follow_zh_vocabulary() {
        let mk = |cat: &str| {
            let mut e = Error::new("zh", error_types::GRAMMAR, 0.8, [0, 2], "原文", None, "t");
            e.category = Some(cat.to_string());
            category_label(&e)
        };
        assert_eq!(mk("spelling"), "错别字");
        assert_eq!(mk("punctuation"), "标点");
        assert_eq!(mk("style"), "表达优化");
        assert_eq!(mk("nonexistent_class"), "其他");
        let mut style = Error::new("zh", "style", 0.5, [0, 2], "原文", None, "t");
        style.category = Some("style".into());
        assert!(!counted_default(&style), "表达优化默认不计差错");
        let mut punct = Error::new("zh", error_types::PUNCTUATION, 0.9, [0, 1], ",", None, "t");
        punct.category = Some("punctuation".into());
        assert!(counted_default(&punct), "标点硬差错默认计入");
    }

    #[test]
    fn candidates_put_unlocated_last_and_renumber_from_one() {
        let units = vec![unit(1, "当日手术13点来院，或等候医生电话通知。")];
        let src = CarrierSource {
            job_id: "j1",
            label: "图片1",
            size: Some((3072.0, 4096.0)),
            extraction: "vision",
            annotations: &[
                ann(
                    1,
                    1,
                    "13点",
                    Some("13:00"),
                    "punctuation",
                    Region::Unlocated,
                ),
                ann(2, 1, "等候医生电话通知", None, "style", box_(100.0, 200.0)),
            ],
            units: &units,
        };
        let rows = candidates(&[src]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].annotation_id, 2, "已定位的必须排在未定位之前");
        assert_eq!(rows[1].annotation_id, 1);
        assert_eq!(rows.iter().map(|r| r.no).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(rows[0].counted, COUNTED_NO, "表达优化默认不计");
        assert!(rows[1].note.contains("⚠ 未定位"), "备注必须暴露定位失败");
        assert!(
            rows[0].note.contains("上下文「当日手术"),
            "备注给出上下文便于人工核对"
        );
        assert!(rows[0].note.contains("抽取 vision"), "备注暴露文本来源档");
    }

    #[test]
    fn selection_keeps_checked_rows_and_applies_human_overrides() {
        let units: Vec<TextUnit> = vec![];
        let src = CarrierSource {
            job_id: "j1",
            label: "图片1",
            size: Some((100.0, 100.0)),
            extraction: "structure",
            annotations: &[
                ann(1, 1, "报到", Some("报道"), "spelling", box_(0.0, 0.0)),
                ann(2, 1, "他说的", Some("他说的话"), "style", box_(0.0, 90.0)),
                ann(3, 1, "无关", None, "grammar", box_(0.0, 50.0)),
            ],
            units: &units,
        };
        let mut rows = candidates(&[src]);
        rows[1].checked = false; // 人工取消勾选
        let kept = apply_selection(
            &rows,
            None,
            &[],
            &[RowOverride {
                annotation_id: 1,
                counted: Some("不计".into()),
                suggestion: Some("改为「报道」（人工核定）".into()),
                ..Default::default()
            }],
        );
        assert_eq!(
            kept.iter().map(|r| r.annotation_id).collect::<Vec<_>>(),
            vec![1, 3]
        );
        assert_eq!(
            kept.iter().map(|r| r.no).collect::<Vec<_>>(),
            vec![1, 2],
            "序号必须重排"
        );
        assert_eq!(kept[0].counted, COUNTED_NO, "人工可改「计错数」");
        assert_eq!(kept[0].suggestion, "改为「报道」（人工核定）");
        // 白名单模式：显式 accepted 覆盖 checked 位
        let only = apply_selection(&rows, Some(&[2]), &[], &[]);
        assert_eq!(
            only.iter().map(|r| r.annotation_id).collect::<Vec<_>>(),
            vec![2]
        );
    }

    /// 人工改写「错误或待核原文」→ 机器口径与坐标必须失效，不能留着冒充「机器验证过」
    #[test]
    fn overriding_original_invalidates_gold_channel() {
        let marked = || {
            let mut r = row("图片1 中部", "报道", "改为「报到」", COUNTED_YES, 9);
            r.gold_no = Some(13);
            r.gold_match = Some("positional".into());
            r.local_span = Some([21, 27]);
            r
        };
        let out = apply_selection(
            &[marked()],
            None,
            &[],
            &[RowOverride {
                annotation_id: 9,
                original: Some("分时段进入病区报道。".into()),
                ..Default::default()
            }],
        );
        assert_eq!(out[0].original, "分时段进入病区报道。");
        assert_eq!(out[0].gold_match, None, "改写原文后机器口径不再成立");
        assert_eq!(out[0].local_span, None, "旧文本坐标与新原文不再对应");
        assert_eq!(out[0].gold_no, Some(13), "条目号保留作人工对照");
        // 只改建议不动原文：口径仍成立
        let kept = apply_selection(
            &[marked()],
            None,
            &[],
            &[RowOverride {
                annotation_id: 9,
                suggestion: Some("改为「报到」（人工核定）".into()),
                ..Default::default()
            }],
        );
        assert_eq!(kept[0].gold_match.as_deref(), Some("positional"));
        assert_eq!(kept[0].local_span, Some([21, 27]));
    }

    #[test]
    fn exported_table_reads_back_through_the_gold_parser() {
        // 互逆性：我们导出的 6 列必须能被 gold 解析器读回来——
        // 否则「重演专家结果」与「评测召回」就是两套口径
        let units: Vec<TextUnit> = vec![];
        let src = CarrierSource {
            job_id: "j1",
            label: "图片1",
            size: Some((3072.0, 4096.0)),
            extraction: "vision",
            annotations: &[
                ann(1, 1, "报到", Some("报道"), "spelling", box_(10.0, 20.0)),
                ann(2, 1, "血压心率", None, "punctuation", Region::Unlocated),
            ],
            units: &units,
        };
        let rows = candidates(&[src]);
        let back = parse_gold_rows(&to_columns(&rows));
        assert_eq!(back.len(), rows.len(), "表头必须被跳过，行数必须一致");
        for (g, r) in back.iter().zip(rows.iter()) {
            assert_eq!(g.no, r.no);
            assert_eq!(g.original, r.original);
            assert_eq!(g.location, r.location);
            assert_eq!(g.suggestion, r.suggestion);
            assert_eq!(g.counted, r.counted_bool(), "「计错数」口径必须一致");
        }
        assert_eq!(
            back[0].verdict, "错别字",
            "备注列的【…】必须被读回成判定标签"
        );
        assert_eq!(back[1].verdict, "标点");
    }

    #[test]
    fn html_output_escapes_injected_markup() {
        let units: Vec<TextUnit> = vec![];
        let src = CarrierSource {
            job_id: "j1",
            label: "图片1",
            size: None,
            extraction: "structure",
            annotations: &[ann(
                1,
                1,
                "<script>alert(1)</script>",
                None,
                "spelling",
                Region::Unlocated,
            )],
            units: &units,
        };
        let rows = candidates(&[src]);
        let html = render_html(&rows, &ReportMeta::default());
        assert!(html.contains("&lt;script&gt;"), "原文里的尖括号必须转义");
        assert!(!html.contains("<script>"), "不得把原文当标记输出");
        assert!(html.contains("<table>") && html.contains("页码及位置"));
        let md = render_markdown(&rows, &ReportMeta::default());
        assert!(md.contains("页码及位置") && md.contains("序号"));
    }

    #[test]
    fn attach_gold_backfills_row_numbers_with_the_same_scoring_as_annotations() {
        let mut rows = vec![ReportRow {
            no: 1,
            location: "图片1 上部·左".into(),
            original: "血压心率".into(),
            suggestion: "改为「血压、心率」".into(),
            counted: COUNTED_YES.into(),
            note: "【标点】并列指标未分隔".into(),
            job_id: "j1".into(),
            annotation_id: 7,
            unit_id: 1,
            confidence: 0.9,
            extraction: "vision".into(),
            detector: "rule".into(),
            rule_id: None,
            located: true,
            checked: true,
            gold_no: None,
            local_span: Some([0, 4]),
            gold_match: None,
        }];
        let gold = vec![GoldItem {
            no: 3,
            location: "图片1术前签字及探视第2项".into(),
            original: "或血压心率增高影响手术及康复".into(),
            suggestion: String::new(),
            counted: true,
            verdict: "差错".into(),
            note: String::new(),
        }];
        let s = attach_gold(&mut rows, &gold, 0.5);
        assert_eq!(s.matched, 1);
        assert_eq!(s.per_gold[0].2, MatchKind::Contained);
        assert_eq!(rows[0].gold_no, Some(3), "命中条目必须回填专家表的序号");
    }

    fn row(location: &str, original: &str, suggestion: &str, counted: &str, ann: u32) -> ReportRow {
        ReportRow {
            no: 1,
            location: location.into(),
            original: original.into(),
            suggestion: suggestion.into(),
            counted: counted.into(),
            note: String::new(),
            job_id: "j".into(),
            annotation_id: ann,
            unit_id: 1,
            confidence: 1.0,
            extraction: "structure".into(),
            detector: "rule".into(),
            rule_id: None,
            located: !location.contains("未定位"),
            checked: true,
            gold_no: None,
            local_span: None,
            gold_match: None,
        }
    }

    /// 位置口径必须能把「专家引整句、系统标词级片段」判成同一处，并在行上留痕
    #[test]
    fn attach_gold_spanned_marks_positional_channel() {
        let gold = vec![GoldItem {
            no: 13,
            location: "图片2 第3行".into(),
            original: "分时段进入病区报道。".into(),
            suggestion: "改为「报到」".into(),
            counted: true,
            note: String::new(),
            verdict: "差错".into(),
        }];
        let mut rows = vec![row("图片2 未定位", "报道", "改为「报到」", COUNTED_YES, 4)];
        // 文本口径：2 字符片段不该白送整条 gold（MIN_CONTAINMENT_CHARS 拦住了）
        let text_only = attach_gold(&mut rows, &gold, 0.5);
        assert_eq!(text_only.matched, 0, "文本口径不得靠包含匹配白送");
        assert_eq!(rows[0].gold_no, None);
        // 位置口径：同一坐标系里字节区间重合 → 命中，并标出口径
        rows[0].gold_no = None;
        let gold_span = (0usize, "分时段进入病区报道。".len());
        let score = attach_gold_spanned(
            &mut rows,
            &gold,
            &[Some(gold_span)],
            &[Some((gold_span.1 - "报道".len(), gold_span.1))],
            0.5,
        );
        assert_eq!(score.matched, 1, "位置口径应认出同一处判定");
        assert_eq!(rows[0].gold_no, Some(13));
        assert_eq!(
            rows[0].gold_match.as_deref(),
            Some("positional"),
            "口径必须留痕，不能与文本口径混成一个数"
        );
    }

    /// 位置口径命中必须进四维：计入「计错数一致」，但不假装「原文对齐」
    #[test]
    fn reproduction_spanned_counts_positional_hit() {
        let gold = vec![GoldItem {
            no: 13,
            location: "图片2 第3行".into(),
            original: "分时段进入病区报道。".into(),
            suggestion: "改为「报到」".into(),
            counted: true,
            note: String::new(),
            verdict: "差错".into(),
        }];
        let rows = vec![row("图片2 第3行", "报道", "改为「报到」", COUNTED_YES, 4)];
        let text = reproduction(&rows, &gold, 0.5);
        assert_eq!(text.matched, 0, "文本口径下这条完全不进四维");
        let gold_span = (0usize, "分时段进入病区报道。".len());
        let s = reproduction_spanned(
            &rows,
            &gold,
            &[Some(gold_span)],
            &[Some((gold_span.1 - "报道".len(), gold_span.1))],
            0.5,
        );
        assert_eq!(s.matched, 1);
        assert_eq!(s.counted_ok, 1, "同一处判定应计入「计错数一致」");
        assert_eq!(s.suggestion_ok, 1);
        assert_eq!(s.location_ok, 1, "两侧都指向图片2 且已定位");
        assert_eq!(s.original_ok, 0, "词级片段不得假装与专家整句原文一致");
        assert_eq!(s.full_ok, 0, "原文维不达标 → 不算严格复现");
        assert_eq!(s.per_gold[0], (13, false, true, true, true));
    }

    /// 没有坐标的行不参与位置口径（不参与 ≠ 判不上，也绝不蒙命中）
    #[test]
    fn attach_gold_spanned_skips_rows_without_coordinates() {
        let gold = vec![GoldItem {
            no: 13,
            location: "图片2 第3行".into(),
            original: "分时段进入病区报道。".into(),
            suggestion: String::new(),
            counted: true,
            note: String::new(),
            verdict: "差错".into(),
        }];
        let mut rows = vec![row("图片2 未定位", "报道", "改为「报到」", COUNTED_YES, 4)];
        let score = attach_gold_spanned(&mut rows, &gold, &[Some((0, 30))], &[None], 0.5);
        assert_eq!(score.matched, 0);
        assert_eq!(rows[0].gold_no, None);
        assert_eq!(rows[0].gold_match, None);
    }

    #[test]
    fn reproduction_reports_four_dimensions_separately() {
        let gold = vec![GoldItem {
            no: 3,
            location: "图片1术前签字及探视第2项".into(),
            original: "或血压心率增高影响手术及康复".into(),
            suggestion: "改为「血压、心率加快」".into(),
            counted: true,
            verdict: "差错".into(),
            note: String::new(),
        }];
        // 同一处、原文对齐、建议接近，但「计错数」与专家不一致 → 计错维不达标
        let rows = vec![row(
            "图片1 上部·左",
            "血压心率",
            "改为「血压、心率」",
            COUNTED_NO,
            7,
        )];
        let r = reproduction(&rows, &gold, 0.5);
        assert_eq!(r.matched, 1);
        assert_eq!(
            (r.original_ok, r.location_ok, r.counted_ok, r.suggestion_ok),
            (1, 1, 0, 1)
        );
        assert_eq!(r.full_ok, 0, "四维未全达标不得算严格复现");
        assert_eq!(r.per_gold[0], (3, true, true, false, true));
    }

    #[test]
    fn unlocated_row_never_passes_location_dimension() {
        let gold = vec![GoldItem {
            no: 9,
            location: "图片1办理出院第1项".into(),
            original: "请买下午15点以后的车票。".into(),
            suggestion: "请买15点以后的车票。".into(),
            counted: true,
            verdict: "差错".into(),
            note: String::new(),
        }];
        let rows = vec![row(
            "图片1 未定位",
            "下午15点",
            "删去「下午」，或改为「下午3点」",
            COUNTED_YES,
            1,
        )];
        let r = reproduction(&rows, &gold, 0.5);
        assert_eq!(r.matched, 1, "词级命中在文本口径下按包含计分（片段足够长）");
        assert_eq!(r.original_ok, 1);
        assert_eq!(r.location_ok, 0, "未定位就是没定位，不能算位置一致");
        assert_eq!(r.counted_ok, 1);
        assert_eq!(r.full_ok, 0);
    }

    #[test]
    fn same_carrier_compares_image_index_only() {
        assert!(same_carrier("图片2 (120,300)", "图片2办理出院第1项"));
        assert!(!same_carrier("图片1 (120,300)", "图片2办理出院第1项"));
        assert!(!same_carrier("图片1 未定位", "图片1办理出院第1项"));
        assert!(!same_carrier("", "图片1"));
    }

    fn gold_at(no: u32, location: &str, original: &str, suggestion: &str) -> GoldItem {
        GoldItem {
            no,
            location: location.into(),
            original: original.into(),
            suggestion: suggestion.into(),
            counted: true,
            verdict: "差错".into(),
            note: format!("【差错】{original}"),
        }
    }

    fn row_local(
        location: &str,
        original: &str,
        suggestion: &str,
        ann: u32,
        unit_id: u32,
        local: [usize; 2],
        rule: Option<&str>,
    ) -> ReportRow {
        let mut r = row(location, original, suggestion, COUNTED_YES, ann);
        r.unit_id = unit_id;
        r.local_span = Some(local);
        r.rule_id = rule.map(str::to_string);
        r
    }

    #[test]
    fn per_carrier_attach_scores_each_carrier_in_its_own_coordinates() {
        // 两个载体各自的 stitched 坐标系；两边标注 id 都是 1（每载体各自编号）
        let gold = vec![
            gold_at(1, "图片1 下部", "分时段进入病区报道。", "改为「报到」"),
            gold_at(2, "图片2 上部", "血压心率增高", "改为「血压、心率增高」"),
        ];
        let st1 = stitch(&[unit(1, "分时段进入病区报道。")]);
        let st2 = stitch(&[unit(1, "血压心率增高影响手术及康复")]);
        let mut rows1 = vec![row_local(
            "图片1 上部",
            "报道",
            "改为「报到」",
            1,
            1,
            [21, 27],
            Some("ZH_CONFUSABLE"),
        )];
        let mut rows2 = vec![row_local(
            "图片2 上部",
            "血压心率",
            "改为「血压、心率增高」",
            1,
            1,
            [0, 12],
            Some("ZH_PARALLEL"),
        )];
        let cands: Vec<Vec<String>> = gold.iter().map(|g| vec![g.original.clone()]).collect();
        let mut carriers = vec![
            CarrierReplay {
                label: "图片1",
                text: &st1.text,
                stitched: &st1,
                rows: &mut rows1,
            },
            CarrierReplay {
                label: "图片2",
                text: &st2.text,
                stitched: &st2,
                rows: &mut rows2,
            },
        ];
        let att = attach_gold_by_carrier(&mut carriers, &gold, &cands, 0.5);
        assert_eq!(
            att.text.matched, 1,
            "词级片段走不了文本口径：{:?}",
            att.text.per_gold
        );
        assert_eq!(
            att.spanned.matched, 2,
            "位置口径补上同一处判定：{:?}",
            att.spanned.per_gold
        );
        // 标注 id 撞号：跨载体合并打分时 gold 会被挂到别的载体的行上；按载体天然避开
        assert_eq!(rows1[0].gold_no, Some(1));
        assert_eq!(rows2[0].gold_no, Some(2));
        assert_eq!(rows1[0].gold_match.as_deref(), Some("positional"));
        assert_eq!(rows2[0].gold_match.as_deref(), Some("text"));
        assert_eq!(att.per_carrier[0].positional, 1);
        assert_eq!(att.per_carrier[1].positional, 0);
        // 每载体都只定位到一条 gold 的原文，另一条在这张图上根本不存在
        assert_eq!(att.per_carrier[0].gold_unlocated, 1);
        assert_eq!(att.per_carrier[1].gold_unlocated, 1);

        let gs1 = locate_spans(&st1.text, &cands);
        let rs1 = stitched_spans(&st1, &rows1);
        let gs2 = locate_spans(&st2.text, &cands);
        let rs2 = stitched_spans(&st2, &rows2);
        let repro = reproduction_by_carrier(
            &[
                CarrierSpans {
                    rows: &rows1,
                    gold_spans: &gs1,
                    row_spans: &rs1,
                },
                CarrierSpans {
                    rows: &rows2,
                    gold_spans: &gs2,
                    row_spans: &rs2,
                },
            ],
            &gold,
            0.5,
        );
        assert_eq!(repro.matched, 2);
        assert_eq!(repro.counted_ok, 2, "两条都计差错，表里也都计错");
        assert_eq!(repro.location_ok, 2);
        assert_eq!(
            repro.original_ok, 1,
            "词级片段 ≠ 专家整句：只有 gold2 原文维达标"
        );
        assert_eq!(repro.full_ok, 1, "gold1 原文维不达标，严格复现只剩 gold2");
    }

    #[test]
    fn stitched_spans_rejects_mismatched_layout() {
        let right = stitch(&[unit(1, "分时段进入病区报道。")]);
        // 拿错布局：unit id 恰好相同、span 也放得下，但那个区间里的文本不是「报道」
        let wrong = stitch(&[unit(1, "血压心率增高影响手术及康复")]);
        let rows = vec![row_local(
            "图片1 上部",
            "报道",
            "改为「报到」",
            1,
            1,
            [21, 27],
            None,
        )];
        assert_eq!(stitched_spans(&right, &rows), vec![Some((21, 27))]);
        assert_eq!(
            stitched_spans(&wrong, &rows),
            vec![None],
            "坐标系对不上就是没有坐标，不能算命中"
        );
    }

    #[test]
    fn missed_gold_items_with_a_firing_rule_point_at_the_rule() {
        let st = stitch(&[unit(1, "或血压心率增高影响手术及康复")]);
        let gold = vec![gold_at(
            3,
            "图片1 中部",
            "血压心率增高",
            "改为「血压、心率增高」",
        )];
        let cands = vec![vec!["血压心率增高".to_string()]];
        let gold_spans = locate_spans(&st.text, &cands);
        let mut rows = vec![row_local(
            "图片1 中部",
            "血压心率",
            "加顿号",
            1,
            1,
            [3, 15],
            Some("ZH_PARALLEL"),
        )];
        let row_spans = stitched_spans(&st, &rows);
        let missed = missed_rule_ids(&rows, &row_spans, &gold, &gold_spans);
        assert_eq!(
            missed.get(&3).map(String::as_str),
            Some("ZH_PARALLEL"),
            "规则触发了却没 credited → 靶子是规则本身"
        );
        attach_gold_spanned(&mut rows, &gold, &gold_spans, &row_spans, 0.5);
        assert_eq!(rows[0].gold_no, Some(3));
        assert!(
            missed_rule_ids(&rows, &row_spans, &gold, &gold_spans).is_empty(),
            "已 credited 就不再是漏报"
        );
    }

    #[test]
    fn missed_rule_falls_back_to_text_overlap_with_provenance() {
        // 视觉转录改写了整句（「分时段进入病区报道」→「请您分时间段办理住院，病区报道」）：
        // gold 原文定位不到 = 没有坐标，此时只能用「行原文是 gold 原文子串」归因，
        // 且必须标成 Text（弱归因），不能冒充位置重合
        let st = stitch(&[unit(1, "请您分时间段办理住院，病区报道。")]);
        let gold = vec![gold_at(
            13,
            "图片2 中部",
            "分时段进入病区报道。",
            "改为「分时段进入病区报到」",
        )];
        let gold_spans = locate_spans(&st.text, &[vec!["分时段进入病区报道。".to_string()]]);
        assert_eq!(gold_spans, vec![None], "转录改写 → 没有坐标");
        // 两条候选行：词级片段（4 字符）与两字片段（2 字符）。取上下文更多的前者
        let rows = vec![
            row_local(
                "图片2 中部",
                "病区报道",
                "改为「病区报到」",
                4,
                1,
                [33, 45],
                Some("ZH_CONFUSABLE"),
            ),
            row_local(
                "图片2 中部",
                "报道",
                "改为「报到」",
                5,
                1,
                [39, 45],
                Some("ZH_PUNCTUATION"),
            ),
        ];
        let row_spans = stitched_spans(&st, &rows);
        let ev = missed_rule_evidence(&rows, &row_spans, &gold, &gold_spans);
        assert_eq!(
            ev.get(&13).map(|(r, _)| r.as_str()),
            Some("ZH_CONFUSABLE"),
            "没有坐标时仍应把漏报归到触发过的规则（取原文更具体的那行）"
        );
        assert_eq!(ev.get(&13).map(|(_, v)| *v), Some(RuleVia::Text));

        // 只有两字片段时**不归因**：真实载体实测里 `探视` / `办理` 就是这样撞进 5 条专家整句的
        assert!(
            missed_rule_evidence(&rows[1..], &row_spans[1..], &gold, &gold_spans).is_empty(),
            "2 字符片段不得做文本归因（实测会造出假靶子）"
        );

        // gold 原文本身太短（< 6 字符）→ 不做文本兜底
        let short = vec![gold_at(14, "图片2 中部", "病区报道", "改为「病区报到」")];
        assert!(
            missed_rule_evidence(&rows, &row_spans, &short, &[None]).is_empty(),
            "短 gold 原文不得靠子串归因"
        );
    }

    #[test]
    fn merge_scores_credits_a_gold_at_most_once_across_carriers() {
        let gold = vec![gold_at(1, "图片1", "血压心率", "血压、心率")];
        let a = score_texts(&gold, &[(1, "血压心率".into())], 0.5);
        let b = score_texts(&gold, &[(1, "血压心率".into())], 0.5);
        let m = merge_scores(&gold, &[a, b]);
        assert_eq!(m.matched, 1, "同一处判定不能因为在两个载体出现而算两次");
        assert_eq!(m.ours, 2);
        assert_eq!(m.per_gold.len(), 1);
        assert_ne!(m.per_gold[0].2, MatchKind::None);
    }
}
