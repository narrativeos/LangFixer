//! 多模态审校设置（`MULTIMODAL.md` §13）
//!
//! 放在 `media-flow`：api-server（HTTP）与 desktop-app（Tauri）跑的是**同一条**载体链路，
//! 设置面必须同构——否则两端会各自漂出不同的默认值与开关名。

use serde::{Deserialize, Serialize};

fn default_true() -> bool {
    true
}

fn env_or_empty(key: &str) -> String {
    std::env::var(key).unwrap_or_default()
}

fn env_flag(key: &str, default: bool) -> bool {
    match std::env::var(key) {
        Ok(v) => v == "1" || v.eq_ignore_ascii_case("true"),
        Err(_) => default,
    }
}

/// 多模态审校设置（`MULTIMODAL.md` §13）
///
/// 环境变量：`GEC_MEDIA_ENABLED` / `GEC_MEDIA_WORKDIR` / `GEC_MEDIA_APPLY_MODE` /
/// `GEC_MEDIA_MAX_BYTES` / `GEC_MEDIA_TIMEOUT_SECS` / `GEC_MEDIA_VISION_TIMEOUT_SECS` /
/// `GEC_MEDIA_VISION_MODEL` / `GEC_MEDIA_VISION_MAX_EDGE` / `GEC_MEDIA_VISION_MAX_BYTES` /
/// `GEC_MEDIA_VISION_JPEG_QUALITY` /
/// `GEC_MEDIA_MIN_CONFIDENCE` / `GEC_MEDIA_CJK_FONT` / `GEC_MEDIA_LEVEL` /
/// `GEC_MEDIA_FORCE_LLM` /
/// `PHOTOCRAFT_CLI` / `PDFCRAFT_CLI` / `WORDCRAFT_CLI`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaSettings {
    /// 总开关（默认关闭）
    #[serde(default)]
    pub enabled: bool,
    /// 默认回写模式：`overlay`（决策 ①）/ `comment` / `in_place` / `list_only`
    #[serde(default = "default_media_apply_mode")]
    pub apply_mode: String,
    /// job 工作目录根（空 = `{数据目录}/media-jobs`；每个 job 一个子目录）
    #[serde(default)]
    pub workdir: String,
    /// 上传载体大小上限（字节）
    #[serde(default = "default_media_max_bytes")]
    pub max_bytes: u64,
    /// 单次协作工具调用超时（秒）
    #[serde(default = "default_media_timeout_secs")]
    pub timeout_secs: u64,
    /// 视觉档单次调用超时（秒）——**故意比协作工具宽**
    ///
    /// 实测（本机 4.9MB 实拍告示 + 视觉模型）：短输出只要 60s，但金集那类整页转录
    /// （每块带 bbox，输出上千 token）会顶到 120s 上限，连接被切断后整个载体降级成
    /// 0 单元（真实重演实测：两个载体全灭、召回 0%）。CLI 调用不该跟着放宽
    /// （挂住的工具要早点失败），所以视觉档单独一个开关。
    #[serde(default = "default_media_vision_timeout_secs")]
    pub vision_timeout_secs: u64,
    /// 视觉档上传前的长边上限（像素）：超过先降采样再送端点
    ///
    /// 实测（本机网关 + 实拍告示）：原图 4.9MB / 3.6MB（长边 3000+）整页转录要 110s+，
    /// 且紧随其后的请求必 503，退避加到 200s 仍救不回第二个载体；降到长边 2000px + q80
    /// 是 0.73MB / 0.52MB（约 7 倍）。视觉 bbox 是归一化坐标，降采样不影响回写定位
    /// （`MULTIMODAL.md` §9「体积/时长」那行的设计落地）。
    #[serde(default = "default_media_vision_max_edge")]
    pub vision_max_edge: u32,
    /// 视觉上传的目标大小（字节）：超过就压到它以下（先降质量，再降边长）
    #[serde(default = "default_media_vision_max_bytes")]
    pub vision_max_bytes: u64,
    /// 视觉上传重编码质量（JPEG 1-100）
    #[serde(default = "default_media_vision_jpeg_quality")]
    pub vision_jpeg_quality: u8,
    /// A 档：读结构层（文字层/文本层/docx run）——真值，默认开
    #[serde(default = "default_true")]
    pub use_structure_layer: bool,
    /// B 档：多模态 LLM 视觉抽取（默认关，P1 开）
    #[serde(default)]
    pub use_vision: bool,
    /// C 档：本地 OCR（默认关；PdfCraft 内置 OCR 仅英文，见 §13）
    #[serde(default)]
    pub use_ocr: bool,
    /// 检测响应级别（语义同 `/gec/detect` 的 level）：**默认 3 = 火力全开**
    ///
    /// 多模态载体的价值取决于「能不能查到」，因此默认把规则 / 决策模型 / L3 /
    /// 知识召回 / LLM 全部打开，识别能力优先；延迟与成本作为后续优化项
    /// （`MULTIMODAL.md` §18）。无凭据/无服务时各层自行降级并在 `layers` 里标
    /// `unavailable`，不会静默当成「没错误」。
    #[serde(default = "default_media_level")]
    pub level: u8,
    /// 强制走 LLM 层（默认 true，与 `level = 3` 同调）
    #[serde(default = "default_true")]
    pub force_llm: bool,
    /// 视觉/OCR 抽取的最低置信度（低于此值只进列表，不画框）
    #[serde(default = "default_media_min_confidence")]
    pub min_confidence: f64,
    /// 视觉档使用的模型（空 = 复用 `llm.model`）
    #[serde(default)]
    pub vision_model: String,
    /// 标注/改字用的 CJK 字体（PhotoCraft 实测默认 Inter 会回退）
    #[serde(default = "default_media_cjk_font")]
    pub cjk_font: String,
    /// 三家 CLI 路径（空 = `$PATH` → 常见安装位置）
    #[serde(default)]
    pub photocraft_bin: String,
    #[serde(default)]
    pub pdfcraft_bin: String,
    #[serde(default)]
    pub wordcraft_bin: String,
    /// 默认工作模式（`MULTIMODAL.md` §15.1）：`simple` = 零外部协作工具；
    /// `advanced` = 交给三家适配器。UI 每次也能按 job 显式传 `mode` 覆盖
    #[serde(default = "default_media_mode")]
    pub mode: String,
}

fn default_media_mode() -> String {
    "advanced".to_string()
}

fn default_media_apply_mode() -> String {
    "overlay".to_string()
}
fn default_media_max_bytes() -> u64 {
    64 * 1024 * 1024
}
fn default_media_timeout_secs() -> u64 {
    120
}
/// 视觉档默认 300s：整页转录不是「一次工具调用」的量级（见 `MediaSettings::vision_timeout_secs`）
fn default_media_vision_timeout_secs() -> u64 {
    300
}
/// 视觉上传长边默认 2000px：主流视觉端点的有效分辨率就在这一档，再大只是多花载荷
fn default_media_vision_max_edge() -> u32 {
    2000
}
/// 视觉上传目标 1.5MB：base64 后约 2MB，是实测能稳定过网关的量级
fn default_media_vision_max_bytes() -> u64 {
    1_500_000
}
fn default_media_vision_jpeg_quality() -> u8 {
    80
}
fn default_media_min_confidence() -> f64 {
    0.6
}
fn default_media_cjk_font() -> String {
    "Heiti SC".to_string()
}
/// 多模态检测级别默认值：3 = 全层（火力全开）
fn default_media_level() -> u8 {
    3
}

impl Default for MediaSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            apply_mode: default_media_apply_mode(),
            workdir: String::new(),
            max_bytes: default_media_max_bytes(),
            timeout_secs: default_media_timeout_secs(),
            vision_timeout_secs: default_media_vision_timeout_secs(),
            vision_max_edge: default_media_vision_max_edge(),
            vision_max_bytes: default_media_vision_max_bytes(),
            vision_jpeg_quality: default_media_vision_jpeg_quality(),
            use_structure_layer: true,
            use_vision: false,
            use_ocr: false,
            level: default_media_level(),
            force_llm: true,
            min_confidence: default_media_min_confidence(),
            vision_model: String::new(),
            cjk_font: default_media_cjk_font(),
            photocraft_bin: String::new(),
            pdfcraft_bin: String::new(),
            wordcraft_bin: String::new(),
            mode: default_media_mode(),
        }
    }
}

impl MediaSettings {
    pub fn from_env() -> Self {
        Self {
            enabled: env_flag("GEC_MEDIA_ENABLED", false),
            apply_mode: std::env::var("GEC_MEDIA_APPLY_MODE")
                .unwrap_or_else(|_| default_media_apply_mode()),
            workdir: env_or_empty("GEC_MEDIA_WORKDIR"),
            max_bytes: std::env::var("GEC_MEDIA_MAX_BYTES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or_else(default_media_max_bytes),
            timeout_secs: std::env::var("GEC_MEDIA_TIMEOUT_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or_else(default_media_timeout_secs),
            vision_timeout_secs: std::env::var("GEC_MEDIA_VISION_TIMEOUT_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .filter(|v: &u64| *v > 0)
                .unwrap_or_else(default_media_vision_timeout_secs),
            vision_max_edge: std::env::var("GEC_MEDIA_VISION_MAX_EDGE")
                .ok()
                .and_then(|v| v.parse().ok())
                .filter(|v: &u32| *v >= 64)
                .unwrap_or_else(default_media_vision_max_edge),
            vision_max_bytes: std::env::var("GEC_MEDIA_VISION_MAX_BYTES")
                .ok()
                .and_then(|v| v.parse().ok())
                .filter(|v: &u64| *v >= 10_000)
                .unwrap_or_else(default_media_vision_max_bytes),
            vision_jpeg_quality: std::env::var("GEC_MEDIA_VISION_JPEG_QUALITY")
                .ok()
                .and_then(|v| v.parse().ok())
                .filter(|v: &u8| (30..=100).contains(v))
                .unwrap_or_else(default_media_vision_jpeg_quality),
            use_structure_layer: env_flag("GEC_MEDIA_STRUCTURE", true),
            use_vision: env_flag("GEC_MEDIA_VISION", false),
            use_ocr: env_flag("GEC_MEDIA_OCR", false),
            level: std::env::var("GEC_MEDIA_LEVEL")
                .ok()
                .and_then(|v| v.parse().ok())
                .filter(|v: &u8| (1..=3).contains(v))
                .unwrap_or_else(default_media_level),
            force_llm: env_flag("GEC_MEDIA_FORCE_LLM", true),
            min_confidence: std::env::var("GEC_MEDIA_MIN_CONFIDENCE")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or_else(default_media_min_confidence),
            vision_model: env_or_empty("GEC_MEDIA_VISION_MODEL"),
            cjk_font: std::env::var("GEC_MEDIA_CJK_FONT")
                .unwrap_or_else(|_| default_media_cjk_font()),
            photocraft_bin: env_or_empty("PHOTOCRAFT_CLI"),
            pdfcraft_bin: env_or_empty("PDFCRAFT_CLI"),
            wordcraft_bin: env_or_empty("WORDCRAFT_CLI"),
            mode: std::env::var("GEC_MEDIA_MODE").unwrap_or_else(|_| default_media_mode()),
        }
    }
}
