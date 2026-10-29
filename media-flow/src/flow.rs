//! 载体审校编排（`MULTIMODAL.md` §8 / §18）：job 状态机 → 全链路 → 回写 → 正误表
//!
//! 检测级联由宿主注入（[`CarrierDetector`]）：api-server 用服务端级联（缓存 / 历史复用 /
//! 规则开关 / 白名单），桌面端用本地引擎（本地决策表 / 离线模式）。
//! 其余（副本隔离、抽取档位选择、拼接与回映射、sidecar 落盘、回写、导出）**只有一份实现**。

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use serde::Deserialize;

use langfixer_llm::LlmConfig;
use langfixer_llm::templates::TemplateStore;
use media_core::{
    ApplyMode, ApplyRequest, CarrierInfo, CarrierKind, Collaborator, JobMode, MediaError, PdfCraft,
    PdfCraftConfig, PhotoCraft, PhotoCraftConfig, PreviewKind, ReportMeta, RowOverride, TextUnit,
    WordCraft, WordCraftConfig, stitch_units,
};
use shared_model::Error as GecError;

use crate::jobs::{MediaJob, MediaJobStore, MediaStage};
use crate::report;
use crate::settings::MediaSettings;
use crate::vision;

/// 编排层错误：宿主各自映射（api-server → HTTP 状态码，桌面端 → `invoke` 错误串）
#[derive(Debug, thiserror::Error)]
pub enum FlowError {
    /// 请求本身不合法（空文件、未实现的载体、非法 id…）
    #[error("{0}")]
    BadRequest(String),
    /// job / 产物不存在
    #[error("{0}")]
    NotFound(String),
    /// 功能未启用或工具未就绪（对应 HTTP 503）
    #[error("{0}")]
    Unavailable(String),
    /// 上游失败（协作工具超时 / 崩溃 / 视觉端点失败）
    #[error("{0}")]
    Upstream(String),
    /// 编排层自己的问题（写盘失败…）
    #[error("{0}")]
    Internal(String),
}

impl From<MediaError> for FlowError {
    fn from(e: MediaError) -> Self {
        match e {
            MediaError::NotReady(m) => FlowError::Unavailable(m),
            MediaError::Unsupported(m) => FlowError::BadRequest(m),
            MediaError::ToolFailed { tool, detail } => {
                FlowError::Upstream(format!("{tool}: {detail}"))
            }
            MediaError::Timeout { secs } => FlowError::Upstream(format!("协作工具超时（{secs}s）")),
            MediaError::NotWritable(m) => FlowError::BadRequest(m),
            MediaError::Internal(m) => FlowError::Internal(m),
        }
    }
}

/// 一次级联检测的结果（载体链路只用这两项：判定 + 层状态）
#[derive(Debug, Clone, Default)]
pub struct DetectResult {
    pub errors: Vec<GecError>,
    /// 已序列化的层状态（宿主原样给出，UI 按文本面板同一口径显示）
    pub layers: Vec<serde_json::Value>,
}

/// 检测级联接缝（宿主实现）
#[async_trait]
pub trait CarrierDetector: Send + Sync {
    /// 对 stitched text 跑一次级联；`level` / `force_llm` 语义同 `/gec/detect`
    async fn detect(
        &self,
        lang: &str,
        text: &str,
        level: Option<u8>,
        force_llm: bool,
        category: Option<&str>,
    ) -> Result<DetectResult, FlowError>;

    /// LLM 兜底层是否可用：不可用时建 job 就要降级并留痕，而不是等运行时 503
    fn llm_ready(&self, lang: &str) -> bool;
}

/// 编排依赖（宿主每次请求构造；`store` 与宿主共享同一份）
#[derive(Clone)]
pub struct FlowCtx {
    pub store: Arc<RwLock<MediaJobStore>>,
    pub media: MediaSettings,
    pub llm: LlmConfig,
    pub prompts: TemplateStore,
}

/// 建 job 的请求参数（语义同 `POST /gec/media/jobs` 的 query）
#[derive(Debug, Clone, Default, Deserialize)]
pub struct JobOptions {
    #[serde(default)]
    pub apply_mode: Option<String>,
    /// 工作模式（`simple` / `advanced`；缺省 = 设置里的 `media.mode`）
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub lang: Option<String>,
    #[serde(default)]
    pub level: Option<u8>,
    #[serde(default)]
    pub force_llm: Option<bool>,
    #[serde(default)]
    pub category: Option<String>,
}

/// 载体大类按扩展名判定（大小写不敏感）
pub fn carrier_kind_of(filename: &str) -> CarrierKind {
    use media_core::CarrierKind as K;
    let ext = filename
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "psd" | "psb" | "png" | "jpg" | "jpeg" | "webp" | "tif" | "tiff" | "heic" | "bmp" => {
            K::Image
        }
        "pdf" => K::Pdf,
        "docx" => K::Docx,
        _ => K::Other,
    }
}

/// 文件名安全化（只取 basename，防目录穿越）
pub fn safe_name(filename: &str) -> String {
    let base = filename.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base
        .chars()
        .map(|c| if c.is_control() || c == ' ' { '_' } else { c })
        .collect();
    if cleaned.is_empty() {
        "carrier".to_string()
    } else {
        cleaned
    }
}

/// 按设置构造图片适配器
pub fn photo_craft(cfg: &MediaSettings) -> Result<PhotoCraft, FlowError> {
    PhotoCraft::new(PhotoCraftConfig {
        bin: Some(cfg.photocraft_bin.clone()),
        timeout_secs: cfg.timeout_secs,
        cjk_font: cfg.cjk_font.clone(),
        group: media_core::DEFAULT_GROUP.to_string(),
        mark_color: "#e03131".to_string(),
    })
    .map_err(FlowError::from)
}

/// 按设置构造 PDF 适配器（高级模式）
pub fn pdf_craft(cfg: &MediaSettings) -> Result<PdfCraft, FlowError> {
    PdfCraft::new(PdfCraftConfig {
        bin: Some(cfg.pdfcraft_bin.clone()),
        timeout_secs: cfg.timeout_secs,
        author: "LangFixer".to_string(),
        mark_color: "#e03131".to_string(),
        preview_dpi: 144,
    })
    .map_err(FlowError::from)
}

/// 按设置构造 Word 适配器（高级模式）
pub fn word_craft(cfg: &MediaSettings) -> Result<WordCraft, FlowError> {
    WordCraft::new(WordCraftConfig {
        bin: Some(cfg.wordcraft_bin.clone()),
        timeout_secs: cfg.timeout_secs,
        author: "LangFixer".to_string(),
    })
    .map_err(FlowError::from)
}

/// 高级模式该载体对应的 CLI 名（`None` = 该载体没有高级模式）
pub fn advanced_cli(kind: CarrierKind) -> Option<&'static str> {
    match kind {
        CarrierKind::Image => Some("photocraft-cli"),
        CarrierKind::Pdf => Some("pdfcraft-cli"),
        CarrierKind::Docx => Some("wordcraft-cli"),
        CarrierKind::Other => None,
    }
}

/// 高级模式所需 CLI 是否就位。返回 `None` = 就位；`Some(原因)` = 缺哪个、怎么补
/// （只做路径解析，不起进程——能力深探测在 [`probes`]）
pub fn advanced_cli_status(kind: CarrierKind, cfg: &MediaSettings) -> Option<String> {
    use media_core::resolve_tool_bin;
    let Some(name) = advanced_cli(kind) else {
        return Some("该格式没有高级模式".to_string());
    };
    let explicit = match kind {
        CarrierKind::Image => cfg.photocraft_bin.as_str(),
        CarrierKind::Pdf => cfg.pdfcraft_bin.as_str(),
        CarrierKind::Docx => cfg.wordcraft_bin.as_str(),
        CarrierKind::Other => "",
    };
    if resolve_tool_bin(Some(explicit), name).is_some() {
        return None;
    }
    Some(if explicit.trim().is_empty() {
        format!("{name} 未找到（装到 ~/.local/bin/{name}，或在设置里给绝对路径）")
    } else {
        format!("{name} 未找到：设置里配的路径「{explicit}」不可用（改正那个路径，或留空自动查找）")
    })
}

/// 高级模式所需 CLI 是否就位
pub fn advanced_cli_found(kind: CarrierKind, cfg: &MediaSettings) -> bool {
    advanced_cli_status(kind, cfg).is_none()
}

/// 视觉档是否可用（只看配置面，不发网络请求）
pub fn vision_ready(ctx: &FlowCtx) -> bool {
    ctx.media.use_vision && ctx.llm.enabled()
}

/// 某载体的模式可用性（§15.1）：UI 的模式卡与后端的 gate 用同一份判定，
/// 否则「按钮能点但后端 503」就会漂出来
#[derive(Debug, Clone)]
pub struct ModeGate {
    pub simple: bool,
    pub simple_reason: Option<String>,
    pub advanced: bool,
    pub advanced_reason: Option<String>,
}

pub fn mode_gate(ctx: &FlowCtx, kind: CarrierKind) -> ModeGate {
    let cfg = &ctx.media;
    let advanced_reason = advanced_cli_status(kind, cfg);
    let advanced = advanced_reason.is_none();
    let (simple, simple_reason) = match kind {
        CarrierKind::Image => {
            if vision_ready(ctx) {
                (true, None)
            } else {
                (
                    false,
                    Some(
                        "图片简单模式靠视觉模型读字：需要 media.use_vision = true 且已配置 LLM（设置 → LLM）"
                            .to_string(),
                    ),
                )
            }
        }
        CarrierKind::Pdf | CarrierKind::Docx => (true, None),
        CarrierKind::Other => (false, Some("该格式不支持审校".to_string())),
    };
    ModeGate {
        simple,
        simple_reason,
        advanced,
        advanced_reason,
    }
}

/// 单个模式可否开工（建 job 的闸）。`None` = 可；`Some(原因)` = 不可，原因里必须带退路
pub fn mode_available(ctx: &FlowCtx, kind: CarrierKind, mode: JobMode) -> Option<String> {
    if mode == JobMode::Simple {
        let g = mode_gate(ctx, kind);
        return g.simple_reason;
    }
    let missing = advanced_cli_status(kind, &ctx.media)?;
    Some(format!(
        "{missing}。高级模式要用它读结构层并回写载体；改用 mode=simple（零 CLI：段落级定位、\
         只读文档视图、正误表位置写真实页号）"
    ))
}

/// 模式卡 JSON（`probes` 与 `inspect_carrier` 都回这个，前端只认一处口径）
pub fn mode_gate_json(ctx: &FlowCtx, kind: CarrierKind) -> serde_json::Value {
    let g = mode_gate(ctx, kind);
    serde_json::json!({
        "simple": { "available": g.simple, "reason": g.simple_reason },
        "advanced": { "available": g.advanced, "reason": g.advanced_reason },
    })
}

/// 未启用就挡住：必须给可执行的 remedy，而不是「未启用」三个字
pub fn ensure_enabled(cfg: &MediaSettings) -> Result<(), FlowError> {
    if !cfg.enabled {
        return Err(FlowError::Unavailable(
            "多模态审校未启用（设置文件里 media.enabled = true，或环境变量 GEC_MEDIA_ENABLED=1）"
                .to_string(),
        ));
    }
    Ok(())
}

fn temp_carrier_dir() -> Result<PathBuf, FlowError> {
    let dir =
        std::env::temp_dir().join(format!("langfixer-media-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).map_err(|e| FlowError::Internal(e.to_string()))?;
    Ok(dir)
}

/// 三家协作工具 + B/C 档就绪探测（只读，不写文件、不发网络请求）
pub async fn probes(ctx: &FlowCtx) -> serde_json::Value {
    let cfg = &ctx.media;
    let mut out = serde_json::Map::new();
    // 总开关必须可探测：`probes` 本身不 gate（未启用时也要能告诉 UI「为什么不能开工」），
    // 否则用户只能在点「开始审校」后吃到一句 503
    out.insert("enabled".into(), serde_json::json!(cfg.enabled));
    let photocraft = match photo_craft(cfg) {
        Ok(pc) => match pc.probe().await {
            Ok(p) => serde_json::to_value(p).unwrap(),
            Err(e) => {
                serde_json::json!({ "app": "photocraft", "ready": false, "degraded_reason": e.to_string() })
            }
        },
        Err(e) => {
            serde_json::json!({ "app": "photocraft", "ready": false, "degraded_reason": e.to_string() })
        }
    };
    out.insert("photocraft".into(), photocraft);
    // PDF / Word：真探测（能力面靠 `tools` / `commands` 输出，不靠版本号猜）
    let pdfcraft = match pdf_craft(cfg) {
        Ok(tc) => match tc.probe().await {
            Ok(p) => serde_json::to_value(p).unwrap(),
            Err(e) => {
                serde_json::json!({ "app": "pdfcraft", "ready": false, "degraded_reason": e.to_string() })
            }
        },
        Err(e) => {
            serde_json::json!({ "app": "pdfcraft", "ready": false, "degraded_reason": e.to_string() })
        }
    };
    out.insert("pdfcraft".into(), pdfcraft);
    let wordcraft = match word_craft(cfg) {
        Ok(tc) => match tc.probe().await {
            Ok(p) => serde_json::to_value(p).unwrap(),
            Err(e) => {
                serde_json::json!({ "app": "wordcraft", "ready": false, "degraded_reason": e.to_string() })
            }
        },
        Err(e) => {
            serde_json::json!({ "app": "wordcraft", "ready": false, "degraded_reason": e.to_string() })
        }
    };
    out.insert("wordcraft".into(), wordcraft);
    // 模式可用性（§15.1）：每种载体一份，前端模式卡与后端 gate 同一口径
    let mut modes = serde_json::Map::new();
    for (key, kind) in [
        ("image", CarrierKind::Image),
        ("pdf", CarrierKind::Pdf),
        ("docx", CarrierKind::Docx),
    ] {
        modes.insert(key.into(), mode_gate_json(ctx, kind));
    }
    out.insert("modes".into(), serde_json::Value::Object(modes));
    out.insert("mode_default".into(), serde_json::json!(cfg.mode));
    // B 档（视觉）就绪状态：只看配置面，不发网络请求（probes 必须保持只读且快）
    let template = ctx.prompts.for_lang_cefr("media", "vision").is_some();
    let model = if cfg.vision_model.trim().is_empty() {
        ctx.llm.model.clone()
    } else {
        cfg.vision_model.clone()
    };
    let reason = if !cfg.use_vision {
        "B 档未开启（media.use_vision / GEC_MEDIA_VISION=1）"
    } else if !ctx.llm.enabled() {
        "未配置 LLM（设置 → LLM，或 LLM_API_KEY）"
    } else {
        ""
    };
    out.insert(
        "vision".into(),
        serde_json::json!({
            "app": "llm_vision",
            "enabled": cfg.use_vision,
            "ready": cfg.use_vision && ctx.llm.enabled(),
            "model": model,
            "template": if template { "media_vision" } else { "builtin" },
            "min_confidence": cfg.min_confidence,
            // 上传前压缩口径（§14）：UI 把它显示出来，用户才知道图上看到的是
            // 「压缩后仍定位准确」的哪一档分辨率，而不是原图
            "max_edge": cfg.vision_max_edge,
            "max_bytes": cfg.vision_max_bytes,
            "jpeg_quality": cfg.vision_jpeg_quality,
            "degraded_reason": reason,
        }),
    );
    out.insert(
        "ocr".into(),
        serde_json::json!({
            "app": "ocr",
            "enabled": cfg.use_ocr,
            "ready": false,
            "degraded_reason": "C 档未实现（PdfCraft 内置 OCR 仅英文，见 MULTIMODAL.md §13）",
        }),
    );
    serde_json::Value::Object(out)
}

/// 载体能力探测（不落 job，用于上传前预判）
///
/// 三种载体 × 两种模式都要能探：`carrier` / `probe` 给「当前可用那一档」的实况
/// （高级模式 CLI 在位就用它，否则用简单模式的读法），`modes` 给模式卡，
/// `simple` 给简单模式单独探到的事实——前端据此决定默认选哪档、按钮为什么禁用。
pub async fn inspect_carrier(
    ctx: &FlowCtx,
    filename: &str,
    bytes: &[u8],
) -> Result<serde_json::Value, FlowError> {
    ensure_enabled(&ctx.media)?;
    if bytes.is_empty() {
        return Err(FlowError::BadRequest("文件为空".into()));
    }
    let kind = carrier_kind_of(filename);
    if kind == CarrierKind::Other {
        return Err(FlowError::BadRequest(format!(
            "{filename} 的格式不支持审校（支持：图片 / PDF / docx）"
        )));
    }
    let modes = mode_gate_json(ctx, kind);
    let dir = temp_carrier_dir()?;
    let path = dir.join(safe_name(filename));
    if let Err(e) = std::fs::write(&path, bytes) {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(FlowError::Internal(e.to_string()));
    }
    // 简单模式：不起任何 CLI（文档 = 纯 Rust 解析；图片 = 只读尺寸）
    let simple: Result<CarrierInfo, FlowError> = match kind {
        CarrierKind::Pdf => Ok(media_core::docparse::parse_pdf(bytes)
            .map_err(FlowError::from)?
            .info),
        CarrierKind::Docx => Ok(media_core::docparse::parse_docx(bytes)
            .map_err(FlowError::from)?
            .info),
        CarrierKind::Image => match vision::image_dims(&path) {
            Some((w, h)) => Ok(CarrierInfo {
                kind,
                width: Some(w),
                height: Some(h),
                pages: None,
                text_units: 0,
                editable: false,
                warnings: vec!["简单模式靠视觉模型读字（扁平位图没有文字层）".to_string()],
            }),
            None => Err(FlowError::BadRequest(
                "这个图片格式（psd / tif / heic…）简单模式读不了：请改用高级模式，或先导出 png / jpg"
                    .to_string(),
            )),
        },
        CarrierKind::Other => unreachable!(),
    };
    // 高级模式：CLI 在位就真探测；不在位不影响简单模式那一路
    let mut out = serde_json::Map::new();
    out.insert("kind".into(), serde_json::json!(kind));
    out.insert("modes".into(), modes);
    match &simple {
        Ok(info) => {
            out.insert("simple".into(), serde_json::to_value(info).unwrap());
        }
        Err(e) => {
            out.insert(
                "simple".into(),
                serde_json::json!({ "available": false, "reason": e.to_string() }),
            );
        }
    }
    if advanced_cli_found(kind, &ctx.media) {
        let r = match kind {
            CarrierKind::Image => {
                let pc = photo_craft(&ctx.media)?;
                let info = pc.inspect(&path).await;
                (info, pc.probe().await)
            }
            CarrierKind::Pdf => {
                let tc = pdf_craft(&ctx.media)?;
                let (info, _units) = tc.inspect_with_units(&path).await?;
                (Ok(info), tc.probe().await)
            }
            CarrierKind::Docx => {
                let tc = word_craft(&ctx.media)?;
                let (info, _units) = tc.inspect_with_units(&path).await?;
                (Ok(info), tc.probe().await)
            }
            CarrierKind::Other => unreachable!(),
        };
        let (info, probe) = r;
        match info {
            Ok(info) => {
                out.insert("carrier".into(), serde_json::to_value(info).unwrap());
                out.insert("advanced_available".into(), serde_json::json!(true));
            }
            Err(e) => {
                out.insert("advanced_error".into(), serde_json::json!(e.to_string()));
            }
        }
        out.insert(
            "probe".into(),
            match probe {
                Ok(p) => serde_json::to_value(p).unwrap(),
                Err(e) => serde_json::json!({ "ready": false, "degraded_reason": e.to_string() }),
            },
        );
    } else {
        let gate = mode_gate(ctx, kind);
        out.insert(
            "probe".into(),
            serde_json::json!({
                "app": advanced_cli(kind).unwrap_or("none"),
                "ready": false,
                "degraded_reason": gate.advanced_reason.unwrap_or_default(),
            }),
        );
        // 高级模式不可用 → `carrier` 就用简单模式探到的那份，前端不至于白屏
        if let Ok(info) = &simple {
            out.insert("carrier".into(), serde_json::to_value(info).unwrap());
        }
        out.insert("advanced_available".into(), serde_json::json!(false));
    }
    let _ = std::fs::remove_dir_all(&dir);
    Ok(serde_json::Value::Object(out))
}

/// 预览面（§15.3）：只有可信像素坐标的才给画布，其余走只读文档视图
///
/// - 图片（两档都有像素）→ 画布；
/// - PDF 高级模式：`PageBox` 是页内归一化坐标，渲染页上能画真框 → 画布；
/// - PDF 简单模式 / Word 两档：落点是段落 + 字符区间，没有像素坐标 → 只读文档视图。
///   （Word 的 `--markup` 渲染图是可下载的凭证，但不是可交互画布，别混为一谈）
pub fn preview_kind_for(kind: CarrierKind, mode: JobMode) -> PreviewKind {
    match (kind, mode) {
        (CarrierKind::Image, _) | (CarrierKind::Pdf, JobMode::Advanced) => PreviewKind::ImageCanvas,
        (CarrierKind::Docx, _) | (CarrierKind::Pdf, JobMode::Simple) => PreviewKind::DocText,
        (CarrierKind::Other, _) => PreviewKind::None,
    }
}

/// 建 job：写副本 → 落状态 → 异步跑全链路（返回创建时的 job 供宿主回传轮询）
pub fn create_job(
    ctx: &FlowCtx,
    detector: Arc<dyn CarrierDetector>,
    filename: &str,
    bytes: &[u8],
    opts: JobOptions,
) -> Result<MediaJob, FlowError> {
    let cfg = ctx.media.clone();
    ensure_enabled(&cfg)?;
    if bytes.is_empty() {
        return Err(FlowError::BadRequest("文件为空".into()));
    }
    if bytes.len() as u64 > cfg.max_bytes {
        return Err(FlowError::BadRequest(format!(
            "文件 {} 字节超过上限 {}（设置 media.max_bytes）",
            bytes.len(),
            cfg.max_bytes
        )));
    }
    let kind = carrier_kind_of(filename);
    if kind == CarrierKind::Other {
        return Err(FlowError::BadRequest(format!(
            "{filename} 的格式不支持审校（支持：图片 / PDF / docx）"
        )));
    }
    // 模式（§15.1）：请求里显式传的优先，否则设置里的 `media.mode`
    let requested = JobMode::parse(opts.mode.as_deref().unwrap_or(&cfg.mode));
    let gate = mode_gate(ctx, kind);
    let mode = match requested {
        JobMode::Advanced if !gate.advanced => {
            // 不静默换档：为什么不能用高级模式、能不能改用简单模式，都要说清
            let reason = gate
                .advanced_reason
                .unwrap_or_else(|| "高级模式不可用".to_string());
            if gate.simple {
                return Err(FlowError::Unavailable(format!(
                    "{reason}｜可以改用简单模式（mode=simple）：读文本与出正误表照样能做，只是不回写载体"
                )));
            }
            return Err(FlowError::Unavailable(reason));
        }
        JobMode::Simple if !gate.simple => {
            return Err(FlowError::Unavailable(
                gate.simple_reason
                    .unwrap_or_else(|| "简单模式不可用".to_string()),
            ));
        }
        m => m,
    };
    // 简单模式没有回写原语 → 强制 ListOnly。这不是「默认值」，是能力边界，
    // 写成默认值会让人以为改了就有回写（改了也白改）。
    let apply_mode = if mode == JobMode::Simple {
        ApplyMode::ListOnly
    } else {
        ApplyMode::parse(opts.apply_mode.as_deref().unwrap_or(&cfg.apply_mode))
    };
    let id = uuid::Uuid::new_v4().simple().to_string();
    let name = safe_name(filename);
    let dir = ctx.store.read().unwrap().job_dir(&id);
    let source = dir.join(&name);
    let lang = opts.lang.clone().unwrap_or_else(|| "zh".to_string());
    // 默认火力全开（`media.level = 3` / `media.force_llm = true`），但 LLM 层不可用时
    // 不能照搬 `/gec/detect` 的 503 契约：载体审校要求「跑完并留痕」，
    // 因此降级为规则 + 决策模型（level 1），并把降级原因写进 `warnings`（不静默）。
    let want_level = opts.level.unwrap_or(cfg.level);
    let want_force_llm = opts.force_llm.unwrap_or(cfg.force_llm);
    let llm_ready = detector.llm_ready(&lang);
    let (level, force_llm, warnings) = if (want_level == 3 || want_force_llm) && !llm_ready {
        (
            1u8,
            false,
            vec![
                "LLM 层未配置：本次降级为规则 + 决策模型（level 1）；配置 LLM 后重跑可得全层结果"
                    .to_string(),
            ],
        )
    } else {
        (want_level, want_force_llm, Vec::new())
    };
    let job = MediaJob {
        id: id.clone(),
        filename: name,
        kind,
        mode,
        preview_kind: preview_kind_for(kind, mode),
        tool: None,
        stage: MediaStage::Copying,
        error: None,
        carrier: None,
        units: Vec::new(),
        extraction: "none".to_string(),
        extraction_model: None,
        stitched: None,
        annotations: Vec::new(),
        layers: Vec::new(),
        apply_mode,
        artifacts: media_core::Artifacts {
            source: source.display().to_string(),
            editable: None,
            preview: None,
            annotations_json: dir.join("annotations.json").display().to_string(),
        },
        warnings,
        created_at: chrono::Utc::now().to_rfc3339(),
        updated_at: chrono::Utc::now().to_rfc3339(),
    };
    ctx.store.write().unwrap().insert(job.clone());
    if let Err(e) = std::fs::write(&source, bytes) {
        ctx.store
            .write()
            .unwrap()
            .fail(&id, &format!("写入副本失败：{e}"));
        return Err(FlowError::Internal(e.to_string()));
    }
    let category = opts.category.clone();
    let run_ctx = ctx.clone();
    tokio::spawn(async move {
        run_job(
            run_ctx,
            detector,
            id,
            lang,
            Some(level),
            force_llm,
            category,
            apply_mode,
            mode,
        )
        .await;
    });
    Ok(job)
}

/// job 阶段推进（写锁内只改内存，不做 IO）
fn set_stage(ctx: &FlowCtx, id: &str, stage: MediaStage) {
    ctx.store.write().unwrap().update(id, |j| j.stage = stage);
}

/// 抽取阶段的结构化结果：模式 × 载体 → 谁来读、读到什么、预览面是什么
struct Extracted {
    carrier: CarrierInfo,
    units: Vec<TextUnit>,
    /// `structure`（结构层，真值）/ `vision`（视觉模型）/ `none`
    extraction: String,
    extraction_model: Option<String>,
    preview_kind: PreviewKind,
    /// 实际读这份载体的是谁（留痕，不给 UI 猜）
    tool: String,
    warnings: Vec<String>,
}

/// 谁来读这份载体（`MULTIMODAL.md` §15.1）
///
/// 简单模式：文档走 `document-parse`（纯 Rust），图片走视觉模型 + `image` 读尺寸——
/// **一次外部 CLI 都不起**；高级模式：三家适配器读结构层。
async fn extract_stage(
    ctx: &FlowCtx,
    cfg: &MediaSettings,
    kind: CarrierKind,
    mode: JobMode,
    dir: &std::path::Path,
    source: &std::path::Path,
    lang: &str,
) -> Result<Extracted, FlowError> {
    let mut warnings: Vec<String> = Vec::new();
    match (kind, mode) {
        // 简单模式 · PDF / Word：纯 Rust 解析，保住页 / 段落结构
        (CarrierKind::Pdf | CarrierKind::Docx, JobMode::Simple) => {
            if !cfg.use_structure_layer {
                return Err(FlowError::BadRequest(
                    "简单模式只有结构层可读：要么打开 media.use_structure_layer，要么改用高级模式"
                        .to_string(),
                ));
            }
            let bytes = std::fs::read(source)
                .map_err(|e| FlowError::Internal(format!("读取副本失败：{e}")))?;
            let parsed = if kind == CarrierKind::Pdf {
                media_core::docparse::parse_pdf(&bytes)?
            } else {
                media_core::docparse::parse_docx(&bytes)?
            };
            warnings.extend(parsed.info.warnings.clone());
            Ok(Extracted {
                carrier: parsed.info,
                units: parsed.units,
                extraction: "structure".to_string(),
                extraction_model: None,
                preview_kind: PreviewKind::DocText,
                tool: "document-parse".to_string(),
                warnings,
            })
        }
        // 简单模式 · 图片：尺寸靠 `image`，文本靠视觉模型；不碰 PhotoCraft
        (CarrierKind::Image, JobMode::Simple) => {
            let (w, h) = vision::image_dims(source).ok_or_else(|| {
                FlowError::BadRequest(
                    "这个图片格式（psd / tif / heic…）简单模式读不了：请改用高级模式，或先导出 png / jpg"
                        .to_string(),
                )
            })?;
            let mut carrier = CarrierInfo {
                kind,
                width: Some(w),
                height: Some(h),
                pages: None,
                text_units: 0,
                editable: false,
                warnings: Vec::new(),
            };
            if !vision_ready(ctx) {
                return Err(FlowError::Unavailable(
                    "图片简单模式靠视觉模型读字：需要 media.use_vision = true 且已配置 LLM"
                        .to_string(),
                ));
            }
            let out =
                vision::extract_vision_units(&ctx.llm, cfg, &ctx.prompts, lang, source, &carrier)
                    .await
                    .map_err(|e| FlowError::Upstream(e.to_string()))?;
            // 原始响应落盘：排查「模型到底说了什么」的唯一凭据
            let _ = std::fs::write(dir.join("vision_raw.txt"), &out.raw);
            warnings.extend(out.warnings);
            carrier.text_units = out.units.len();
            if out.units.is_empty() {
                warnings.push(format!("视觉档（{}）未取到文本", out.model));
            }
            let extraction = if carrier.text_units == 0 {
                "none".to_string()
            } else {
                "vision".to_string()
            };
            Ok(Extracted {
                carrier,
                units: out.units,
                extraction,
                extraction_model: Some(out.model),
                preview_kind: PreviewKind::ImageCanvas,
                tool: "vision".to_string(),
                warnings,
            })
        }
        // 高级模式 · 图片：PhotoCraft 读文字层，扁平位图再走视觉兜底
        (CarrierKind::Image, JobMode::Advanced) => {
            let pc = photo_craft(cfg)?;
            let carrier = pc.inspect(source).await.map_err(FlowError::from)?;
            warnings.extend(carrier.warnings.clone());
            let mut units = if cfg.use_structure_layer {
                pc.extract_units(source).await.map_err(FlowError::from)?
            } else {
                warnings.push("已关闭结构层抽取（media.use_structure_layer=false）".to_string());
                Vec::new()
            };
            let mut extraction = if units.is_empty() {
                "none"
            } else {
                "structure"
            }
            .to_string();
            let mut extraction_model: Option<String> = None;
            // 铁律：A 档取到文本就**绝不**用视觉文本覆盖——结构层是真值
            if units.is_empty() && cfg.use_vision {
                let ext = source.extension().and_then(|s| s.to_str()).unwrap_or("");
                let vision_image = match media_core::vision_mime(ext) {
                    Some(_) => source.to_path_buf(),
                    None => {
                        let png = dir.join("vision.png");
                        match pc.render_preview(source, &png).await {
                            Ok(()) => png,
                            Err(e) => {
                                warnings.push(format!("视觉档前置导出 png 失败：{e}"));
                                source.to_path_buf()
                            }
                        }
                    }
                };
                match vision::extract_vision_units(
                    &ctx.llm,
                    cfg,
                    &ctx.prompts,
                    lang,
                    &vision_image,
                    &carrier,
                )
                .await
                {
                    Ok(out) => {
                        let _ = std::fs::write(dir.join("vision_raw.txt"), &out.raw);
                        warnings.extend(out.warnings);
                        extraction_model = Some(out.model.clone());
                        if out.units.is_empty() {
                            warnings.push(format!("视觉档（{}）未取到文本", out.model));
                        } else {
                            extraction = "vision".to_string();
                            units = out.units;
                        }
                    }
                    // 视觉档失败不判死整个 job：结构层结果仍然可用
                    Err(e) => warnings.push(format!("视觉档失败：{e}")),
                }
            } else if units.is_empty() {
                warnings.push(
                    "未取到文本：该载体无文字层。开启视觉档（media.use_vision）或接 OCR 后可校对"
                        .to_string(),
                );
            }
            Ok(Extracted {
                carrier,
                units,
                extraction,
                extraction_model,
                preview_kind: PreviewKind::ImageCanvas,
                tool: "photocraft".to_string(),
                warnings,
            })
        }
        // 高级模式 · PDF / Word：适配器读结构层（带几何 / run 引用）
        (CarrierKind::Pdf, JobMode::Advanced) => {
            let tc = pdf_craft(cfg)?;
            let (carrier, units) = tc
                .inspect_with_units(source)
                .await
                .map_err(FlowError::from)?;
            warnings.extend(carrier.warnings.clone());
            Ok(Extracted {
                carrier,
                units,
                extraction: "structure".to_string(),
                extraction_model: None,
                preview_kind: PreviewKind::ImageCanvas,
                tool: "pdfcraft".to_string(),
                warnings,
            })
        }
        (CarrierKind::Docx, JobMode::Advanced) => {
            let tc = word_craft(cfg)?;
            let (carrier, units) = tc
                .inspect_with_units(source)
                .await
                .map_err(FlowError::from)?;
            warnings.extend(carrier.warnings.clone());
            Ok(Extracted {
                carrier,
                units,
                extraction: "structure".to_string(),
                extraction_model: None,
                preview_kind: PreviewKind::DocText,
                tool: "wordcraft".to_string(),
                warnings,
            })
        }
        (CarrierKind::Other, _) => Err(FlowError::BadRequest("该格式不支持审校".to_string())),
    }
}

/// 全链路：inspect → extract（A 档）→ 视觉兜底（B 档）→ stitch → detect → remap
///
/// 参数不拆结构体：函数体内 `id` / `lang` / `level` / `force_llm` / `category`
/// 直接成组使用（阶段推进、降级重试、回映射都要读），包一层反而到处 `.field`
#[allow(clippy::too_many_arguments)]
pub async fn run_job(
    ctx: FlowCtx,
    detector: Arc<dyn CarrierDetector>,
    id: String,
    lang: String,
    level: Option<u8>,
    force_llm: bool,
    category: Option<String>,
    apply_mode: ApplyMode,
    mode: JobMode,
) {
    let fail = |msg: String| {
        tracing::warn!(job = %id, error = %msg, "多模态 job 失败");
        ctx.store.write().unwrap().fail(&id, &msg);
    };
    let cfg = ctx.media.clone();
    let (dir, name, kind) = {
        let guard = ctx.store.read().unwrap();
        match guard.get(&id) {
            Some(j) => (guard.job_dir(&id), j.filename.clone(), j.kind),
            None => return,
        }
    };
    let source = dir.join(&name);

    // 读这份载体的是谁，由（载体 × 模式）决定，见 [`extract_stage`]
    set_stage(&ctx, &id, MediaStage::Inspecting);
    let Extracted {
        carrier,
        units,
        extraction,
        extraction_model,
        preview_kind,
        tool,
        mut warnings,
    } = match extract_stage(&ctx, &cfg, kind, mode, &dir, &source, &lang).await {
        Ok(e) => e,
        Err(e) => return fail(e.to_string()),
    };
    set_stage(&ctx, &id, MediaStage::Extracting);

    let (stitched, cleaned) = stitch_units(&units);
    let mut annotations = Vec::new();
    let mut layers = Vec::new();
    if !stitched.text.trim().is_empty() {
        set_stage(&ctx, &id, MediaStage::Detecting);
        // LLM 层跑挂不能把整个 job 打死：退回「规则 + 决策模型」再试一次，并把原因写进
        // warnings——与建 job 时「LLM 未配置」同一立场：跑不动必须可见，不能变成「没错误」。
        let first = detector
            .detect(&lang, &stitched.text, level, force_llm, category.as_deref())
            .await;
        let outcome = match first {
            Ok(o) => o,
            Err(e) if level == Some(3) || force_llm => {
                warnings.push(format!(
                    "LLM 层失败（{e}）：本次降级为规则 + 决策模型（level 1）；修好 LLM 配置重跑可得全层结果"
                ));
                match detector
                    .detect(&lang, &stitched.text, Some(1), false, category.as_deref())
                    .await
                {
                    Ok(o) => o,
                    Err(e2) => {
                        warnings.push(format!("降级到 level 1 仍然失败：{e2}"));
                        return fail(e2.to_string());
                    }
                }
            }
            Err(e) => return fail(e.to_string()),
        };
        layers = outcome.layers;
        set_stage(&ctx, &id, MediaStage::Remapping);
        // 跨文本单元的判定 = 判定文本把两个独立文本对象当成了连续文本（视觉块切分时最常见：
        // 「强电间」被切成两块，`电`+`间` 并不相邻）。这类判定**不成立**，不是「定位不到」：
        // 留成「未定位」行等于拿假相邻充数，还会把位置口径统计带偏。丢弃必须计数并告警。
        let (kept, crossed) = media_core::partition_cross_unit(&stitched, outcome.errors);
        if !crossed.is_empty() {
            let mut rules: Vec<String> = crossed.iter().filter_map(|e| e.rule_id.clone()).collect();
            rules.sort();
            rules.dedup();
            warnings.push(format!(
                "丢弃 {} 条跨文本单元的判定（假相邻：判定文本跨越两个独立文本对象）：{}",
                crossed.len(),
                if rules.is_empty() {
                    "（未关联规则）".to_string()
                } else {
                    rules.join(" / ")
                }
            ));
        }
        annotations = media_core::remap(&stitched, &cleaned, &kept, apply_mode);
    }

    // sidecar 先落盘：回写失败也不丢结果
    if let Ok(s) = serde_json::to_string_pretty(&annotations)
        && let Err(e) = std::fs::write(dir.join("annotations.json"), s)
    {
        warnings.push(format!("annotations.json 写入失败：{e}"));
    }
    let editable_ext = source
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("psd")
        .to_string();
    ctx.store.write().unwrap().update(&id, |j| {
        j.stage = MediaStage::Ready;
        j.carrier = Some(carrier);
        j.units = cleaned;
        j.extraction = extraction;
        j.extraction_model = extraction_model;
        j.preview_kind = preview_kind;
        j.tool = Some(tool);
        j.stitched = Some(stitched);
        j.annotations = annotations;
        j.layers = layers;
        // 建 job 时写入的留痕（如 LLM 层降级）必须保留，再接本轮流水线告警
        let mut merged = j.warnings.clone();
        merged.extend(warnings);
        j.warnings = merged;
        j.artifacts.editable = Some(
            dir.join(format!("result.{editable_ext}"))
                .display()
                .to_string(),
        );
        // 只读文档视图没有位图预览可取（回写后才有 `--markup` 渲染图），
        // 提前写路径只会让前端去取一个不存在的文件
        j.artifacts.preview = if preview_kind == PreviewKind::DocText {
            None
        } else {
            Some(dir.join("result.png").display().to_string())
        };
    });
}

/// 按扩展名给 content-type（预览回退到副本原件时必须给对，否则浏览器不显示）
fn content_type_of(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "pdf" => "application/pdf",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        _ => "application/octet-stream",
    }
}

/// 预览文件（无 `result.png` 时回退副本原件）：`(路径, content-type)`
pub fn preview_file(ctx: &FlowCtx, id: &str) -> Result<(PathBuf, &'static str), FlowError> {
    let guard = ctx.store.read().unwrap();
    let job = guard
        .get(id)
        .ok_or_else(|| FlowError::NotFound(format!("job 不存在: {id}")))?;
    let dir = guard.job_dir(id);
    let png = dir.join("result.png");
    let (path, ctype) = if png.is_file() {
        (png, "image/png")
    } else {
        let source = dir.join(&job.filename);
        let ctype = content_type_of(&source);
        (source, ctype)
    };
    if !path.is_file() {
        return Err(FlowError::NotFound(format!(
            "预览不可用：{}",
            path.display()
        )));
    }
    Ok((path, ctype))
}

/// 回写产物后缀（图片沿用 PhotoCraft 的 psd；PDF / Word 回到各自原格式）
fn result_ext(kind: CarrierKind) -> &'static str {
    match kind {
        CarrierKind::Image => "psd",
        CarrierKind::Pdf => "pdf",
        CarrierKind::Docx => "docx",
        CarrierKind::Other => "bin",
    }
}

/// 回写（默认 Overlay；幂等：图片清 `mark-*` 图层，PDF/Word 清同署名批注）
pub async fn apply_job(
    ctx: &FlowCtx,
    id: &str,
    apply_mode: Option<&str>,
) -> Result<MediaJob, FlowError> {
    let cfg = ctx.media.clone();
    ensure_enabled(&cfg)?;
    let (source, target, preview_path, annotations, units, mode, kind) = {
        let guard = ctx.store.read().unwrap();
        let j = guard
            .get(id)
            .ok_or_else(|| FlowError::NotFound(format!("job 不存在: {id}")))?;
        if !matches!(j.stage, MediaStage::Ready | MediaStage::Applied) {
            return Err(FlowError::BadRequest(format!(
                "job 尚未就绪（当前 {:?}）",
                j.stage
            )));
        }
        // 简单模式没有回写原语——这不是「没做」，是这一档的能力边界，必须说明白
        if j.mode == JobMode::Simple {
            return Err(FlowError::Unavailable(
                "简单模式不回写载体：它只出清单与正误表。要写回载体，请用高级模式（mode=advanced）重跑这份载体"
                    .to_string(),
            ));
        }
        let dir = guard.job_dir(id);
        let mode = apply_mode.map(ApplyMode::parse).unwrap_or(j.apply_mode);
        (
            dir.join(&j.filename),
            dir.join(format!("result.{}", result_ext(j.kind))),
            dir.join("result.png"),
            j.annotations.clone(),
            j.units.clone(),
            mode,
            j.kind,
        )
    };
    set_stage(ctx, id, MediaStage::Applying);
    let req = ApplyRequest {
        source: &source,
        target: &target,
        preview: &preview_path,
        annotations: &annotations,
        units: &units,
        mode,
        author: "LangFixer",
        group: media_core::DEFAULT_GROUP,
    };
    // 按载体选路；各家原语在各自适配器里（flag 名不同，不能共用拼装器）
    let res = match kind {
        CarrierKind::Image => {
            let pc = photo_craft(&cfg)?;
            let r = pc.apply(&req).await;
            if r.is_ok()
                && let Err(e) = pc.render_preview(&target, &preview_path).await
            {
                tracing::warn!(error = %e, "预览导出失败（可编辑产物仍在）");
            }
            r
        }
        CarrierKind::Pdf => pdf_craft(&cfg)?.apply(&req).await,
        CarrierKind::Docx => word_craft(&cfg)?.apply(&req).await,
        CarrierKind::Other => Err(MediaError::Unsupported("该载体不支持回写".to_string())),
    };
    let res = match res {
        Ok(r) => r,
        Err(e) => {
            ctx.store.write().unwrap().fail(id, &e.to_string());
            return Err(FlowError::from(e));
        }
    };
    let job = ctx
        .store
        .write()
        .unwrap()
        .update(id, |j| {
            j.stage = MediaStage::Applied;
            j.apply_mode = mode;
            j.artifacts.editable = Some(target.display().to_string());
            j.artifacts.preview = Some(preview_path.display().to_string());
            let mut w = j.warnings.clone();
            w.extend(res.warnings.clone());
            for (aid, why) in &res.skipped {
                w.push(format!("#{aid} 未回写：{why}"));
            }
            j.warnings = w;
        })
        .ok_or_else(|| FlowError::NotFound(format!("job 不存在: {id}")))?;
    if let Ok(s) = serde_json::to_string_pretty(&job.annotations) {
        let dir = ctx.store.read().unwrap().job_dir(id);
        let _ = std::fs::write(dir.join("annotations.json"), s);
    }
    Ok(job)
}

// —— 正误表导出（`MULTIMODAL.md` §18）———————————————————————————————

/// 载体标号：正误表的「页码及位置」必须能对上专家的「图片1 / 图片2」
pub fn carrier_label(kind: CarrierKind, idx: usize) -> String {
    let noun = match kind {
        CarrierKind::Image => "图片",
        CarrierKind::Pdf => "PDF",
        CarrierKind::Docx => "文档",
        CarrierKind::Other => "载体",
    };
    format!("{noun}{}", idx + 1)
}

/// 导出请求（语义同 `POST /gec/media/report` 的 body）
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ReportRequest {
    /// 参与导出的 job（缺省 = 全部就绪的 job，按提交顺序 = 载体顺序）
    #[serde(default)]
    pub job_ids: Option<Vec<String>>,
    /// 勾选白名单（按标注 id）；给了它就忽略 `checked` 位
    #[serde(default)]
    pub accepted_ids: Option<Vec<u32>>,
    /// 显式剔除
    #[serde(default)]
    pub rejected_ids: Vec<u32>,
    /// 人工改写（原文 / 建议 / 类别 / 计错数 / 备注）
    #[serde(default)]
    pub overrides: Vec<RowOverride>,
    /// 是否顺带用 wordcraft-cli 转 docx（默认尝试）
    #[serde(default)]
    pub docx: Option<bool>,
    #[serde(default)]
    pub title: Option<String>,
}

/// 载体说明行（「图片1：xxx.jpg（3072×4096，无文字层，抽取 vision）」）
fn carrier_caption(label: &str, job: &MediaJob) -> String {
    let size = match (
        job.carrier.as_ref().and_then(|c| c.width),
        job.carrier.as_ref().and_then(|c| c.height),
    ) {
        (Some(w), Some(h)) => format!("{w}×{h}"),
        _ => "尺寸未知".to_string(),
    };
    let layer = if job.extraction == "structure" {
        "含文字层"
    } else {
        "无文字层"
    };
    format!(
        "{}：{}（{}，{}，抽取 {}）",
        label, job.filename, size, layer, job.extraction
    )
}

fn carrier_size(job: &MediaJob) -> Option<(f64, f64)> {
    match (
        job.carrier.as_ref().and_then(|c| c.width),
        job.carrier.as_ref().and_then(|c| c.height),
    ) {
        (Some(w), Some(h)) => Some((f64::from(w), f64::from(h))),
        _ => None,
    }
}

/// 把多个载体的推演结果汇成一张正误表（与专家同构）
pub async fn build_report(
    ctx: &FlowCtx,
    req: ReportRequest,
) -> Result<serde_json::Value, FlowError> {
    let cfg = ctx.media.clone();
    ensure_enabled(&cfg)?;
    let mut warnings: Vec<String> = Vec::new();
    let (rid, dir, mut files, meta, rows, total, dropped) = {
        let guard = ctx.store.read().unwrap();
        let mut jobs: Vec<&MediaJob> = guard
            .list()
            .into_iter()
            .filter(|j| matches!(j.stage, MediaStage::Ready | MediaStage::Applied))
            .collect();
        if let Some(ids) = req.job_ids.as_ref() {
            jobs.retain(|j| ids.iter().any(|i| i == &j.id));
        }
        if jobs.is_empty() {
            return Err(FlowError::BadRequest(
                "没有可导出的 job（先建 job，或用 job_ids 指定）".to_string(),
            ));
        }
        // 表格顺序 = 提交顺序 = 载体顺序（专家表就是按载体排的）
        jobs.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        let labels: Vec<String> = jobs
            .iter()
            .enumerate()
            .map(|(i, j)| carrier_label(j.kind, i))
            .collect();
        let sources: Vec<media_core::CarrierSource> = jobs
            .iter()
            .enumerate()
            .map(|(i, j)| media_core::CarrierSource {
                job_id: &j.id,
                label: &labels[i],
                size: carrier_size(j),
                extraction: &j.extraction,
                annotations: &j.annotations,
                units: &j.units,
            })
            .collect();
        let candidates = media_core::candidates(&sources);
        drop(sources);
        let rows = media_core::apply_selection(
            &candidates,
            req.accepted_ids.as_deref(),
            &req.rejected_ids,
            &req.overrides,
        );
        let dropped: Vec<u32> = candidates
            .iter()
            .filter(|c| {
                !rows
                    .iter()
                    .any(|r| r.annotation_id == c.annotation_id && r.job_id == c.job_id)
            })
            .map(|c| c.annotation_id)
            .collect();
        // 各 job 的告警必须跟着进表——空抽取 / 未定位不能悄悄变成「没错误」
        for (i, j) in jobs.iter().enumerate() {
            for w in &j.warnings {
                warnings.push(format!("{}（{}）：{w}", labels[i], j.filename));
            }
        }
        let meta = ReportMeta {
            title: req
                .title
                .clone()
                .unwrap_or_else(|| "文字差错核对表".to_string()),
            lang: jobs
                .first()
                .and_then(|j| j.annotations.first())
                .map(|a| a.error.lang.clone())
                .unwrap_or_else(|| "zh".to_string()),
            generated_at: chrono::Utc::now().to_rfc3339(),
            carriers: jobs
                .iter()
                .enumerate()
                .map(|(i, j)| carrier_caption(&labels[i], j))
                .collect(),
            extractions: jobs.iter().map(|j| j.extraction.clone()).collect(),
            models: {
                let mut m: Vec<String> = jobs
                    .iter()
                    .filter_map(|j| j.extraction_model.clone())
                    .collect();
                m.sort();
                m.dedup();
                m
            },
            note: format!(
                "本表由 LangFixer 推演生成（候选 {} 条，导出 {} 条）。「计错数」与「正确或处理建议」以人工勾选为准；「未定位」条目只在列表中，不在载体上标记。",
                candidates.len(),
                rows.len()
            ),
        };
        let rid = uuid::Uuid::new_v4().simple().to_string();
        let dir = guard.root.join("reports").join(&rid);
        let files = report::write_artifacts(&dir, &rows, &meta).map_err(FlowError::from)?;
        (rid, dir, files, meta, rows, candidates.len(), dropped)
    };
    // docx 走 WordCraft（HTML → docx 实测保表格保中文）；不可用时降级为告警而非失败
    match req.docx {
        Some(false) => warnings.push("按要求未生成 docx（docx: false）".to_string()),
        _ => match report::WordCraft::new(Some(&cfg.wordcraft_bin), cfg.timeout_secs) {
            Ok(wc) => {
                let target = dir.join("report.docx");
                match wc.html_to_docx(&files.html, &target).await {
                    Ok(()) => files.docx = Some(target),
                    Err(e) => warnings.push(format!("docx 未生成（wordcraft-cli）：{e}")),
                }
            }
            Err(e) => warnings.push(format!("docx 未生成：{e}")),
        },
    }
    Ok(serde_json::json!({
        "report_id": rid,
        "dir": dir.display().to_string(),
        "candidates": total,
        "exported": rows.len(),
        "dropped_ids": dropped,
        "meta": meta,
        "rows": rows,
        "paths": {
            "json": files.json.display().to_string(),
            "markdown": files.markdown.display().to_string(),
            "html": files.html.display().to_string(),
            "docx": files.docx.map(|p| p.display().to_string()),
        },
        "warnings": warnings,
    }))
}

/// 取导出产物：`(路径, content-type)`
pub fn report_file_path(
    ctx: &FlowCtx,
    rid: &str,
    format: &str,
) -> Result<(PathBuf, &'static str), FlowError> {
    ensure_enabled(&ctx.media)?;
    if rid.is_empty()
        || !rid
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(FlowError::BadRequest("report id 非法".to_string()));
    }
    let dir = ctx.store.read().unwrap().root.join("reports").join(rid);
    let (name, ctype) = match format {
        "json" => ("report.json", "application/json"),
        "md" | "markdown" => ("report.md", "text/markdown; charset=utf-8"),
        "html" => ("report.html", "text/html; charset=utf-8"),
        "docx" => (
            "report.docx",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        ),
        other => return Err(FlowError::BadRequest(format!("未知 format: {other}"))),
    };
    let path = dir.join(name);
    if !path.is_file() {
        return Err(FlowError::NotFound(format!(
            "报告产物不可用：{}",
            path.display()
        )));
    }
    Ok((path, ctype))
}
