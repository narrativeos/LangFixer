//! 协作工具契约：探测 / 取文本 / 回写 / 预览（三家 flag 名不同，必须各自实现）

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::model::{Annotation, ApplyMode, CarrierInfo, TextUnit};

/// 适配器错误（api-server 据此映射 HTTP 状态码；降级动作见 §12）
#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    /// 工具未安装 / 版本过旧 → 503（UI 提示去设置里配置）
    #[error("协作工具未就绪：{0}")]
    NotReady(String),
    /// 载体格式不支持 → 400
    #[error("载体格式不支持：{0}")]
    Unsupported(String),
    /// 工具调用失败（非零退出 / 输出不可解析）→ 502
    #[error("{tool} 调用失败：{detail}")]
    ToolFailed { tool: String, detail: String },
    /// 超时 → 504
    #[error("协作工具超时（{secs}s）")]
    Timeout { secs: u64 },
    /// 载体不可写（只读/加密）→ 422
    #[error("载体不可写：{0}")]
    NotWritable(String),
    #[error("内部错误：{0}")]
    Internal(String),
}

/// 工具能力位（探测阶段填；缺能力 → 该载体标记 degraded，回写降级 ListOnly）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCapability {
    /// 读结构层文本（A 档）
    InspectText,
    /// 无损改字（InPlace）
    EditInPlace,
    /// 叠加标注（Overlay）
    OverlayMarks,
    /// 批注（Comment）
    Comments,
    /// 导出扁平预览
    Preview,
    /// 内置 OCR（语言受限，必须另外上报语言列表）
    Ocr,
}

/// 工具探测结果（设置卡片与 job 报告都展示这个）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolProbe {
    /// photocraft / pdfcraft / wordcraft
    pub app: String,
    /// 解析后的真实可执行路径（GUI 进程不继承 shell PATH，故必须上报）
    pub bin: String,
    /// `--version` 首行
    pub version: String,
    pub ready: bool,
    pub capabilities: Vec<ToolCapability>,
    /// 不可用的原因（缺哪个原语 / 未安装 / 版本过低）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<String>,
    /// 额外事实（如 OCR 可用语言、可用 CJK 字体）——UI 必须如实显示
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// 回写请求（适配器翻译成各家原语）
pub struct ApplyRequest<'a> {
    /// job 目录内的只读原件
    pub source: &'a Path,
    /// 可编辑产物输出路径（**绝不等于 source**，见 §9）
    pub target: &'a Path,
    /// 预览输出路径（png）
    pub preview: &'a Path,
    pub annotations: &'a [Annotation],
    pub units: &'a [TextUnit],
    pub mode: ApplyMode,
    /// 标注署名（Word 修订/批注作者；PDF 批注 author）
    pub author: &'a str,
    /// 标注组名（幂等：重跑先删同名组下的旧标注）
    pub group: &'a str,
}

/// 回写结果
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ApplyResult {
    /// 实际写入载体的 annotation id
    pub applied: Vec<u32>,
    /// 跳过的（id, 原因）——不可定位 / 载体不支持 / 原语缺失
    pub skipped: Vec<(u32, String)>,
    /// 产物路径
    pub artifacts: Vec<PathBuf>,
    pub warnings: Vec<String>,
}

/// 一个载体协作工具的最小契约。三家 CLI 形态差异大
/// （`--cmd/--params` vs `run <tool> k=v` vs `--cmd 'id={json}'`），
/// 所以 transport 必须是各 adapter 内部实现，不能共用命令行拼装器。
#[async_trait]
pub trait Collaborator: Send + Sync {
    /// 载体大类标识（image / pdf / docx）
    fn kind(&self) -> &'static str;

    /// 只读探测（`--version` 等，不启动 GUI、不写文件）
    async fn probe(&self) -> Result<ToolProbe, MediaError>;

    /// 能力探测：载体尺寸/页数/文字层数量/可编辑性
    async fn inspect(&self, file: &Path) -> Result<CarrierInfo, MediaError>;

    /// A 档取文本（结构层）。B/C 档由 api-server 侧的 extractor 负责，
    /// 因为它们要调 LLM/OCR，不属于载体适配器
    async fn extract_units(&self, file: &Path) -> Result<Vec<TextUnit>, MediaError>;

    /// 回写（Overlay / Comment / InPlace 由 mode 决定）
    async fn apply(&self, req: &ApplyRequest<'_>) -> Result<ApplyResult, MediaError>;

    /// 导出扁平预览
    async fn render_preview(&self, file: &Path, out: &Path) -> Result<(), MediaError>;
}
