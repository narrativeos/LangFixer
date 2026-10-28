//! 核心模型：文本单元 + 区域锚点 + 回映射产物（`MULTIMODAL.md` §2 的实现）

use serde::{Deserialize, Serialize};
use shared_model::Error;

/// 载体大类（决定用哪个 adapter 与哪种回写语义）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CarrierKind {
    /// 位图（png / jpg / heic / psd / tif …，由 PhotoCraft 承担）
    Image,
    /// 分页文档（PdfCraft 承担）
    Pdf,
    /// 文档流（WordCraft 承担）
    Docx,
    /// 其它（只出清单）
    Other,
}

/// 图层对象类型（PhotoCraft `info` 的 `kind`：`Type` / `Pixel` / `Group` …）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayerKind {
    /// 文字层（可无损改字）
    Type,
    Shape,
    Pixel,
    Group,
    Other,
}

impl LayerKind {
    /// 解析 PhotoCraft `info` 返回的 `kind` 字符串（未知值归 `Other`，不 panic）
    pub fn from_tool(s: &str) -> Self {
        match s {
            "Type" => Self::Type,
            "Shape" => Self::Shape,
            "Pixel" => Self::Pixel,
            "Group" => Self::Group,
            _ => Self::Other,
        }
    }
}

/// 文本在载体里的落点（回写时由适配器翻译为工具原语）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "region", rename_all = "snake_case")]
pub enum Region {
    /// 扁平位图：像素坐标（原点左上，单位 = 载体像素）
    ImageBox { x: f64, y: f64, w: f64, h: f64 },
    /// 图片里的文字层对象（可无损编辑）；`bounds` = `[x, y, w, h]` 像素
    /// （实测 `photocraft-cli info` 的 `layers[].bounds` 即此形式）
    LayerRef {
        layer_id: u32,
        kind: LayerKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bounds: Option<[f64; 4]>,
    },
    /// 分页文档：页 + **页内归一化坐标 0..1**（避免 DPI 假设；
    /// PdfCraft `text_find` 返回 points，适配器负责按页尺寸归一化）
    PageBox {
        page: u32,
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    },
    /// 文档流内的可编辑对象引用（docx run 区间 / PDF 文本命中）
    RunRef { part: String, run_range: [usize; 2] },
    /// 无法定位（置信度过低 / 跨单元 / 载体不支持）→ 只进列表视图，**不画到载体上**
    Unlocated,
}

impl Region {
    /// 能否在载体上画出来（`Unlocated` 绝不能画假框——见 §7 交互约束）
    pub fn is_locatable(&self) -> bool {
        !matches!(self, Region::Unlocated)
    }

    /// 是否指向可无损编辑的原对象
    pub fn is_editable_object(&self) -> bool {
        matches!(
            self,
            Region::LayerRef {
                kind: LayerKind::Type,
                ..
            } | Region::RunRef { .. }
        )
    }
}

/// 文本来源档（§3 三档）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtractSource {
    /// 结构层（文字层 / 文本层 / docx run）——真值，置信度恒 1.0
    Structure,
    /// 多模态 LLM 视觉抽取
    Vision,
    /// 本地 OCR
    Ocr,
}

impl ExtractSource {
    /// 优先级：structure > vision > ocr（数值小者优先；冲突时绝不用后者文本覆盖前者）
    pub fn priority(&self) -> u8 {
        match self {
            Self::Structure => 0,
            Self::Vision => 1,
            Self::Ocr => 2,
        }
    }
}

/// 一个可独立校对的文本单元（一行 / 一段 / 一个文字层 / 一个 OCR 块）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextUnit {
    pub id: u32,
    pub text: String,
    pub source: ExtractSource,
    pub region: Region,
    /// 抽取置信度（structure 恒 1.0；vision/ocr 由来源给出）
    pub confidence: f64,
    /// 是否可无损回写（图片文字层 / docx run / PDF 文本层 = true；扁平位图 = false）
    pub editable: bool,
    pub page: Option<u32>,
}

/// 拼接产物：检测输入 + 反查表（内核只见 `text`，我们保住 span ↔ unit 双向映射）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stitched {
    pub text: String,
    /// unit_id → stitched 中的字节区间 `[start, end)`
    pub unit_ranges: Vec<(u32, [usize; 2])>,
    /// 单元间分隔符（单字节，保证 offset 可逆；默认 `"\n"`）
    pub separator: String,
}

/// 回写模式（决策 ①：默认 Overlay）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplyMode {
    /// 默认：叠加标注——图片 `mark-*` 层 / PDF squiggly-highlight 批注 / Word 批注
    #[default]
    Overlay,
    /// 批注（PDF `comment_add` / docx comment），正文零改动
    Comment,
    /// 显式开启才用：`type.edit`（图片）/ 修订（docx）/ `text_edit`（PDF，CJK 不可用）
    InPlace,
    /// 只出清单，不回写载体
    ListOnly,
}

impl ApplyMode {
    /// 宽松解析（设置/env/UI 三处输入都走这里；无法识别 → 默认 Overlay）
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "comment" => Self::Comment,
            "in_place" | "inplace" => Self::InPlace,
            "list_only" | "listonly" => Self::ListOnly,
            _ => Self::Overlay,
        }
    }
}

/// 标记样式意图（适配器翻译成各家原语；不支持的 kind 由适配器降级为 Box）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkKind {
    Box,
    Underline,
    Highlight,
    Squiggly,
    Strike,
    Note,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarkStyle {
    pub kind: MarkKind,
    /// `#RRGGBB`
    pub color: String,
}

impl Default for MarkStyle {
    fn default() -> Self {
        Self {
            kind: MarkKind::Box,
            color: "#e03131".to_string(),
        }
    }
}

/// 校对结果 + 落点：检测到的 [`Error`] 回映射后的产物（回写与 UI 的共同输入）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Annotation {
    pub id: u32,
    /// **原样保留**：归因 / 讲解 / 反馈 / 会话历史全都接得上
    pub error: Error,
    pub unit_id: u32,
    /// 单元内字节区间（= `error.span` 减去 unit 在 stitched 中的起始偏移）
    pub local_span: [usize; 2],
    pub region: Region,
    pub apply: ApplyMode,
    pub style: MarkStyle,
    /// 定位告警（跨单元 / 落在分隔符上 / 置信度不足）。UI 必须显示，
    /// 不允许把定位失败伪装成定位成功
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locate_warning: Option<String>,
}

/// 载体探测结果（`inspect` 阶段产物；决定能走哪几档、能否无损回写）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CarrierInfo {
    pub kind: CarrierKind,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub pages: Option<u32>,
    /// 探测到的文本单元数（0 = 无文字层 → 只能走视觉/OCR 档）
    pub text_units: usize,
    /// 载体本身是否可无损编辑（含 Type 文字层 / 文本层 / docx = true）
    pub editable: bool,
    pub warnings: Vec<String>,
}

/// 回写产物（§9：永不改原件；可编辑源 + 扁平预览 + sidecar 三件）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Artifacts {
    /// 只读原件（job 目录内的副本）
    pub source: String,
    /// 可继续编辑的产物（result.psd / result.docx / result.pdf）
    pub editable: Option<String>,
    /// 扁平预览（result.png / 渲染页）
    pub preview: Option<String>,
    /// 标注 sidecar（回写失败也不丢结果）
    pub annotations_json: String,
}
