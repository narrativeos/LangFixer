//! B 档：多模态 LLM 视觉抽取（`MULTIMODAL.md` §3 / §14 P1）
//!
//! 这里**只负责「模型输出 → TextUnit」**：解析、定位归一化、置信度策略。
//! 真正的网络调用在 api-server（`media_vision.rs`），这样本模块可离线测试，
//! 且 A 档（结构层）优先级不受影响——本模块只在结构层取不到文本时兜底。
//!
//! 两条硬约束：
//! 1. **转录不是编辑**：提示词要求逐字原样，错误必须保留（否则校对无从下手）；
//! 2. **定位不确定就不画框**：bbox 缺失 / 越界 / 置信度低于阈值 → `Region::Unlocated`，
//!    只进列表视图（§7 交互约束：不允许把定位失败伪装成定位成功）。

use serde::{Deserialize, Serialize};

use crate::model::{ExtractSource, Region, TextUnit};

/// 模型未给 confidence 时的默认自评（低于 `min_confidence` 默认值 → 不画框）
pub const DEFAULT_VISION_CONFIDENCE: f64 = 0.6;

/// 默认 system 提示词（`prompts/media_vision_prompt.json` 缺失时使用）
pub const DEFAULT_VISION_SYSTEM: &str = "你是文字转录引擎，不是文字编辑。\n\
任务：把图片里可见的文字**逐字原样**转录出来。\n\
严禁：改正错别字、补/删标点、翻译、改写、合并或拆分语义、补全被截断的句子。\n\
图片里的错误（错别字、标点误用、语义不通）必须原样保留——下游校对系统正是靠它们工作。\n\
只输出数据，不要解释。";

/// 默认 user 指令（`{{lang}}` 为检测语言占位符）
pub const DEFAULT_VISION_USER: &str = "转录这张图片中的全部可见文字（{{lang}}），只输出 JSON 数组，不要 markdown 围栏、不要解释。\n\
每个元素形如：\n\
{\"text\":\"该行原文\",\"bbox\":[x1,y1,x2,y2],\"confidence\":0.95}\n\
- text：一个视觉行/文本块的原样文字，保留原有标点、数字、空格、错别字\n\
- bbox：该块在整张图片中的位置，**归一化到 0..1**（原点左上，x2>x1，y2>y1）；无法给出就省略此字段\n\
- confidence：你对该块转录准确度的自评（0..1）\n\
按从上到下、同一行内从左到右的顺序排列。不要合并不同行，不要输出空 text，不要重复输出同一行。";

/// 模型返回的一个文本块
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VisionBlock {
    pub text: String,
    /// `[x1, y1, x2, y2]`（尺度由 [`bbox_to_region`] 判定：0..1 / 0..100 / 0..1000 / 像素）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bbox: Option<[f64; 4]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
}

/// 取 JSON 数组：容忍 ```json 围栏、前后解释文字、`{"blocks":[…]}` 包装
///
/// 返回 `(blocks, 降级告警)`——解析失败时按行回退（无定位），**不静默吞掉**。
pub fn parse_vision_blocks(raw: &str) -> (Vec<VisionBlock>, Option<String>) {
    let cleaned = strip_fences(raw);
    // 1. 顶层对象包装（响应明确是对象时优先解包，否则内部数组会被当成裸数组）
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&cleaned)
        && v.is_object()
    {
        for key in ["blocks", "lines", "items", "regions", "texts", "results"] {
            if let Some(arr) = v.get(key).and_then(|x| x.as_array())
                && let Some(blocks) = blocks_from_array(arr)
            {
                return (
                    blocks,
                    Some(format!("响应是 {{\"{key}\":[…]}} 包装，已解包")),
                );
            }
        }
    }
    // 2. 裸数组（容忍前后解释文字）
    if let Some(arr) = find_json_array(&cleaned)
        && let Some(blocks) = blocks_from_json(arr)
    {
        return (blocks, None);
    }
    // 回退：按行切（无定位 → 全部 Unlocated）
    let blocks: Vec<VisionBlock> = cleaned
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('{') && !l.starts_with('['))
        .map(|l| VisionBlock {
            text: l.trim_matches('"').to_string(),
            bbox: None,
            confidence: None,
        })
        .collect();
    if blocks.is_empty() {
        (
            Vec::new(),
            Some("视觉响应无法解析为文本块（既不是 JSON 数组也不是可切分文本）".to_string()),
        )
    } else {
        (
            blocks,
            Some("视觉响应不是 JSON 数组，已按行回退：无定位信息（不画框）".to_string()),
        )
    }
}

fn strip_fences(s: &str) -> String {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix("```") {
        let inner = rest.split_once("```").map(|(a, _)| a).unwrap_or(rest);
        // 去掉 ```json 这类语言标记行
        return inner
            .split_once('\n')
            .map(|(_, b)| b.trim().to_string())
            .unwrap_or_else(|| inner.trim().to_string());
    }
    s.to_string()
}

/// 扫描出第一个配平的 JSON 数组（跳过字符串内的括号）
fn find_json_array(s: &str) -> Option<&str> {
    let bytes = s.as_bytes();
    let start = s.find('[')?;
    let mut depth = 0usize;
    let mut in_str = false;
    let mut escaped = false;
    for i in start..bytes.len() {
        match bytes[i] {
            b'"' if !escaped => in_str = !in_str,
            b'\\' if in_str => escaped = !escaped,
            b'[' if !in_str => depth += 1,
            b']' if !in_str => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[start..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

fn blocks_from_json(s: &str) -> Option<Vec<VisionBlock>> {
    let v: Vec<serde_json::Value> = serde_json::from_str(s).ok()?;
    blocks_from_array(&v)
}

fn blocks_from_array(arr: &[serde_json::Value]) -> Option<Vec<VisionBlock>> {
    let mut out = Vec::new();
    for item in arr {
        match item {
            serde_json::Value::String(t) => {
                if !t.trim().is_empty() {
                    out.push(VisionBlock {
                        text: t.trim().to_string(),
                        bbox: None,
                        confidence: None,
                    });
                }
            }
            serde_json::Value::Object(o) => {
                let Some(text) = ["text", "content", "line", "原文"]
                    .iter()
                    .filter_map(|k| o.get(*k).and_then(|v| v.as_str()))
                    .find(|t| !t.trim().is_empty())
                else {
                    continue; // 无正文的对象直接跳过，不让它污染单元
                };
                let bbox = ["bbox", "box", "bbox_2d", "rect"]
                    .iter()
                    .find_map(|k| o.get(*k).and_then(|v| v.as_array()))
                    .and_then(|a| {
                        if a.len() != 4 {
                            return None;
                        }
                        let n: Vec<f64> = a.iter().filter_map(|x| x.as_f64()).collect();
                        if n.len() == 4 {
                            Some([n[0], n[1], n[2], n[3]])
                        } else {
                            None
                        }
                    });
                let confidence = ["confidence", "conf", "score"]
                    .iter()
                    .find_map(|k| o.get(*k).and_then(|v| v.as_f64()));
                out.push(VisionBlock {
                    text: text.trim().to_string(),
                    bbox,
                    confidence,
                });
            }
            _ => {}
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

/// bbox → 载体像素坐标（`Region::ImageBox`）
///
/// 各家视觉模型尺度不一（0..1 / 0..100 / Qwen-VL 的 0..1000 / 直接像素），
/// 且响应里通常不声明。这里按 bbox 的**数值量级**排优先级，再逐个试，
/// 取第一个能整体落在载体内的口径；都不成立 → `None`（调用方降级为 `Unlocated`，
/// 绝不画一个越界或错尺度的假框）。
///
/// 已知歧义：0..1000 口径下的小框（如 `[10,20,50,60]`）会被读成百分比。
/// 无法从数据本身消歧——UI 必须把框画出来让人一眼看到（§7），
/// 且视觉单元永远 `editable = false`，错框不会导致错改字。
pub fn bbox_to_region(bbox: &[f64; 4], width: f64, height: f64) -> Option<Region> {
    if !bbox.iter().all(|v| v.is_finite()) || width <= 0.0 || height <= 0.0 {
        return None;
    }
    let maxv = bbox.iter().cloned().fold(f64::MIN, f64::max);
    // (x 因子, y 因子, 该口径的取值上界)：分数 / 千分比 / 百分比 / 像素
    // 上界用于排除「量级根本不可能」的口径（否则 1500 会被千分比缩成 1.5px 的假框）
    let (frac, permille, percent, pixels) = (
        (width, height, 1.02),
        (width / 1000.0, height / 1000.0, 1020.0),
        (width / 100.0, height / 100.0, 102.0),
        (1.0, 1.0, width.max(height) * 1.02),
    );
    let order: [(f64, f64, f64); 4] = if maxv <= 1.5 {
        [frac, permille, percent, pixels]
    } else if maxv <= 100.0 {
        [percent, frac, permille, pixels]
    } else if maxv <= 1000.0 {
        [permille, percent, frac, pixels]
    } else {
        [pixels, permille, percent, frac]
    };
    for (sx, sy, domain) in order {
        if maxv > domain {
            continue; // 该口径下取值越界，不可能是这个尺度
        }
        let (x1, y1, x2, y2) = (bbox[0] * sx, bbox[1] * sy, bbox[2] * sx, bbox[3] * sy);
        if x2 <= x1 || y2 <= y1 {
            continue;
        }
        // 允许 2% 容差（模型常把边界压到略溢出）
        if x1 >= -width * 0.02 && y1 >= -height * 0.02 && x2 <= width * 1.02 && y2 <= height * 1.02
        {
            return Some(Region::ImageBox {
                x: x1.clamp(0.0, width),
                y: y1.clamp(0.0, height),
                w: (x2 - x1).clamp(1.0, width),
                h: (y2 - y1).clamp(1.0, height),
            });
        }
    }
    None
}

/// 视觉块 → 文本单元（`source = vision`、`editable = false`——扁平位图无原对象可改）
///
/// 返回 `(units, 告警)`。排序：有定位的按 `(y1, x1)` 阅读顺序，无定位的按原顺序
/// 附在末尾并告警。**不做去重**——告示里同一句可能真的出现多次（金集第 14 项即如此）。
pub fn vision_units(
    blocks: &[VisionBlock],
    width: Option<u32>,
    height: Option<u32>,
    min_confidence: f64,
) -> (Vec<TextUnit>, Vec<String>) {
    let (w, h) = (width.unwrap_or(0) as f64, height.unwrap_or(0) as f64);
    let size_known = w > 0.0 && h > 0.0;
    let mut warnings = Vec::new();
    let mut rows: Vec<(f64, f64, usize, TextUnit)> = Vec::new();
    for (i, b) in blocks.iter().enumerate() {
        let text = b.text.trim();
        if text.is_empty() {
            continue;
        }
        let conf = b
            .confidence
            .unwrap_or(DEFAULT_VISION_CONFIDENCE)
            .clamp(0.0, 1.0);
        let mut region = if size_known {
            b.bbox
                .as_ref()
                .and_then(|bb| bbox_to_region(bb, w, h))
                .unwrap_or(Region::Unlocated)
        } else {
            if b.bbox.is_some() {
                warnings.push("载体尺寸未知，视觉 bbox 无法换算为像素 → 不画框".to_string());
            }
            Region::Unlocated
        };
        if conf < min_confidence {
            if region.is_locatable() {
                warnings.push(format!(
                    "视觉块 #{} 置信度 {conf:.2} < 阈值 {min_confidence:.2} → 只进列表，不画框",
                    i + 1
                ));
            }
            region = Region::Unlocated;
        } else if !region.is_locatable() {
            warnings.push(format!("视觉块 #{} 无可靠定位 → 只进列表，不画框", i + 1));
        }
        let (sy, sx) = match &region {
            Region::ImageBox { x, y, .. } => (*y, *x),
            _ => (f64::MAX, f64::MAX),
        };
        rows.push((
            sy,
            sx,
            i,
            TextUnit {
                id: 0, // 排序后统一编号
                text: text.to_string(),
                source: ExtractSource::Vision,
                region,
                confidence: conf,
                editable: false,
                page: None,
            },
        ));
    }
    // 稳定排序：有定位在前（按 y1,x1 阅读顺序），无定位按原顺序附后
    rows.sort_by(|a, b| {
        a.0.partial_cmp(&b.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .then(a.2.cmp(&b.2))
    });
    let units = rows
        .into_iter()
        .enumerate()
        .map(|(n, (_, _, _, mut u))| {
            u.id = (n + 1) as u32;
            u
        })
        .collect();
    (units, warnings)
}

/// 图片字节 → `data:` URL（OpenAI 兼容 `image_url.url` 的离线形态：
/// 不必先把载体上传到某个可公网访问的地址，也就不多开一个凭据面）
pub fn image_data_url(mime: &str, bytes: &[u8]) -> String {
    use base64::Engine as _;
    format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

/// 扩展名 → 视觉端点通常可吃的 MIME
///
/// `psd` / `tif` / `heic` 返回 `None`：多数端点不收，必须先由适配器导出 png
/// （PhotoCraft `render_preview` 就是干这个的）
pub fn vision_mime(ext: &str) -> Option<&'static str> {
    match ext.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fenced_array_with_surrounding_prose() {
        let raw = "好的，以下是转录结果：\n```json\n[\n {\"text\":\"住院手续身份核查\",\"bbox\":[0.05,0.02,0.6,0.06],\"confidence\":0.93},\n {\"text\":\"分时段进入病区报道\",\"bbox\":[0.1,0.5,0.5,0.54]}\n]\n```\n以上。";
        let (blocks, warn) = parse_vision_blocks(raw);
        assert!(warn.is_none(), "正常路径不应告警：{warn:?}");
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].text, "住院手续身份核查");
        assert_eq!(blocks[0].confidence, Some(0.93));
        assert_eq!(blocks[1].confidence, None);
        // bbox 里含逗号/括号也不能干扰数组扫描
        assert_eq!(
            parse_vision_blocks("[{\"text\":\"a,b (c)\"}]").0[0].text,
            "a,b (c)"
        );
    }

    #[test]
    fn unwraps_object_wrapper_and_alt_field_names() {
        let raw = "{\"lines\":[{\"line\":\"请买下午15点以后的车票。\",\"box\":[10,20,300,44],\"score\":0.88}]}";
        let (blocks, warn) = parse_vision_blocks(raw);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].text, "请买下午15点以后的车票。");
        assert_eq!(blocks[0].bbox, Some([10.0, 20.0, 300.0, 44.0]));
        assert_eq!(blocks[0].confidence, Some(0.88));
        assert!(warn.unwrap().contains("包装"));
    }

    #[test]
    fn non_json_response_falls_back_to_lines_with_warning() {
        let (blocks, warn) = parse_vision_blocks("住院手续身份核查\n分时段进入病区报道\n");
        assert_eq!(blocks.len(), 2, "按行回退仍要拿到文本");
        assert!(blocks.iter().all(|b| b.bbox.is_none()));
        assert!(warn.unwrap().contains("按行回退"));
        let (none, warn) = parse_vision_blocks("");
        assert!(none.is_empty());
        assert!(warn.unwrap().contains("无法解析"));
    }

    #[test]
    fn bbox_scales_all_land_inside_the_carrier() {
        let (w, h) = (3072.0, 4096.0);
        // 0..1 归一化
        let r1 = bbox_to_region(&[0.1, 0.2, 0.5, 0.25], w, h).unwrap();
        // 千分比（Qwen-VL 口径）
        let r2 = bbox_to_region(&[100.0, 200.0, 500.0, 250.0], w, h).unwrap();
        // 百分比
        let r3 = bbox_to_region(&[10.0, 20.0, 50.0, 25.0], w, h).unwrap();
        // 直接像素
        let r4 = bbox_to_region(&[307.0, 819.0, 1536.0, 1024.0], w, h).unwrap();
        for r in [&r1, &r2, &r3, &r4] {
            let Region::ImageBox { x, y, w: bw, h: bh } = r else {
                panic!("必须是 image_box：{r:?}");
            };
            assert!(*x >= 0.0 && *y >= 0.0 && *bw > 0.0 && *bh > 0.0);
            assert!(x + bw <= w && y + bh <= h, "框不得越出载体：{r:?}");
        }
        // 三种归一化口径应落到同一位置（±1px）
        for r in [&r2, &r3, &r4] {
            let Region::ImageBox { x, .. } = r else {
                unreachable!()
            };
            let Region::ImageBox { x: x0, .. } = r1 else {
                unreachable!()
            };
            assert!((x - x0).abs() < 1.0, "尺度判定不一致：{x} vs {x0}");
        }
    }

    #[test]
    fn impossible_bbox_is_dropped_not_fabricated() {
        // 宽度 1500 > 载体 1000：各种缩放口径都不成立 → None（不画假框）
        assert!(bbox_to_region(&[0.0, 0.0, 1500.0, 10.0], 1000.0, 800.0).is_none());
        // 反向坐标（x2 < x1）
        assert!(bbox_to_region(&[0.5, 0.1, 0.2, 0.3], 1000.0, 800.0).is_none());
        // 非有限值
        assert!(bbox_to_region(&[f64::NAN, 0.0, 0.5, 0.5], 1000.0, 800.0).is_none());
    }

    #[test]
    fn uncertain_blocks_are_listed_but_never_boxed() {
        let blocks = vec![
            VisionBlock {
                text: "分时段进入病区报道".into(),
                bbox: Some([0.1, 0.1, 0.4, 0.13]),
                confidence: Some(0.95),
            },
            VisionBlock {
                text: "模糊的一行".into(),
                bbox: Some([0.1, 0.2, 0.4, 0.23]),
                confidence: Some(0.31), // 低于阈值
            },
            VisionBlock {
                text: "没有框的一行".into(),
                bbox: None,
                confidence: Some(0.9),
            },
        ];
        let (units, warnings) = vision_units(&blocks, Some(3072), Some(4096), 0.6);
        assert_eq!(units.len(), 3);
        assert_eq!(units[0].text, "分时段进入病区报道");
        assert!(units[0].region.is_locatable());
        assert!(
            !units[1].region.is_locatable(),
            "低置信度不得画框（只进列表）"
        );
        assert!(!units[2].region.is_locatable(), "无 bbox 不得画框");
        assert_eq!(units[1].confidence, 0.31, "置信度必须原样上报给 UI");
        assert!(warnings.iter().any(|w| w.contains("置信度")));
        assert!(warnings.iter().any(|w| w.contains("无可靠定位")));
        // 视觉单元一律不可无损改字
        assert!(
            units
                .iter()
                .all(|u| !u.editable && u.source == ExtractSource::Vision)
        );
    }

    #[test]
    fn reading_order_sort_and_duplicates_preserved() {
        let mk = |text: &str, y: f64, x: f64| VisionBlock {
            text: text.into(),
            bbox: Some([x, y, x + 0.3, y + 0.03]),
            confidence: None,
        };
        let blocks = vec![
            mk("第三行", 0.7, 0.1),
            mk("微信小程序：掌上阜外医院", 0.1, 0.1),
            mk("第二行左", 0.4, 0.05),
            mk("第二行右", 0.4, 0.6),
            // 同一句真的出现多次（金集第 14 项）——绝不能去重
            mk("微信小程序：掌上阜外医院", 0.8, 0.1),
        ];
        let (units, _) = vision_units(&blocks, Some(3072), Some(4096), 0.6);
        let texts: Vec<&str> = units.iter().map(|u| u.text.as_str()).collect();
        assert_eq!(
            texts,
            vec![
                "微信小程序：掌上阜外医院",
                "第二行左",
                "第二行右",
                "第三行",
                "微信小程序：掌上阜外医院",
            ]
        );
        assert_eq!(
            units
                .iter()
                .filter(|u| u.text.contains("掌上阜外医院"))
                .count(),
            2,
            "重复出现必须保留"
        );
        let ids: Vec<u32> = units.iter().map(|u| u.id).collect();
        assert_eq!(ids, vec![1, 2, 3, 4, 5], "编号必须连续且与阅读顺序一致");
    }
}

#[test]
fn image_data_url_is_inline_and_mime_gates_unsupported() {
    let url = image_data_url("image/png", b"ab");
    assert_eq!(url, "data:image/png;base64,YWI=");
    // 视觉端点不收的格式必须显式拒绝（让上层去导出 png，而不是发一个必然 400 的请求）
    assert_eq!(vision_mime("JPG"), Some("image/jpeg"));
    assert_eq!(vision_mime("png"), Some("image/png"));
    assert_eq!(vision_mime("psd"), None);
    assert_eq!(vision_mime("tif"), None);
    assert_eq!(vision_mime("heic"), None);
}
