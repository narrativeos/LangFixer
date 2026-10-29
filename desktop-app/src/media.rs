//! 桌面端载体（图片）审校接线：把 `media-flow` 的编排暴露成 Tauri 命令
//!
//! 与 api-server 的关系：**链路只有一份**（`media_flow`：副本隔离、A/B 档抽取、
//! 拼接与回映射、回写、正误表）。两端各自注入自己的检测级联——
//! api-server 用 `run_detection`（缓存 / 历史复用），桌面端用本地引擎
//! （[`DesktopDetector`]：本地决策表 + 规则开关 + 自定义词白名单 + 离线模式）。
//!
//! 传输差异：HTTP 的字节流在这里走 base64（`invoke` 只能传 JSON）——
//! 上传用 `data_b64`，预览与报告产物用 `{ content_type, data_b64 }` 回传，
//! 由前端还原成 Blob / 走「另存为」对话框。

use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use tauri::State;

use media_flow::{
    CarrierDetector, DetectResult, FlowCtx, FlowError, JobOptions, MediaJob, ReportRequest, flow,
};

use crate::commands::Core;

/// 桌面端检测级联的载体接缝
///
/// 与 `detect` 命令同一判定口径（规则开关 / 自定义词白名单 / category 过滤），
/// 但**不做历史复用、不写检测历史**：载体 stitched text 是一次性输入，
/// 复用历史会把「同一 stitched 文本」的旧判定套到新载体上（假阳性来源）。
pub struct DesktopDetector {
    core: Arc<Core>,
}

impl DesktopDetector {
    pub fn new(core: Arc<Core>) -> Arc<Self> {
        Arc::new(Self { core })
    }
}

#[async_trait]
impl CarrierDetector for DesktopDetector {
    async fn detect(
        &self,
        lang: &str,
        text: &str,
        level: Option<u8>,
        force_llm: bool,
        category: Option<&str>,
    ) -> Result<DetectResult, FlowError> {
        let level = level.unwrap_or(1);
        if !(1..=3).contains(&level) {
            return Err(FlowError::BadRequest(format!(
                "无效响应级别: {level}（应为 1-3）"
            )));
        }
        // 读锁内只克隆路由 Arc（guard 不得跨 await）
        let router = self.core.router.read().unwrap().clone();
        let engine = router
            .get(lang)
            .cloned()
            .ok_or_else(|| FlowError::BadRequest(format!("语言 {lang} 无检测器")))?;
        let llm = self.core.llm_detect_engine(lang);
        let outcome = engine
            .detect_level(text, level, force_llm, llm.as_deref())
            .await
            .map_err(|e| match e {
                // LLM 未配置 / 不可用：编排层据此降级并留痕，不能变成「没错误」
                gec_core::DetectorError::LlmNotConfigured(m) => FlowError::Unavailable(m),
                other => FlowError::Unavailable(other.to_string()),
            })?;
        Ok(DetectResult {
            errors: filter_desktop_rules(&self.core, lang, outcome.errors, category),
            layers: outcome
                .layers
                .iter()
                .map(|l| serde_json::to_value(l).unwrap_or_default())
                .collect(),
        })
    }

    fn llm_ready(&self, lang: &str) -> bool {
        self.core.llm_detect_engine(lang).is_some()
    }
}

/// 与 `detect` 命令同口径的后处理：已禁用规则 / 自定义词白名单 / category 过滤
/// （开关状态存于 SQLite 词表存储；批量查询避免 N+1）
fn filter_desktop_rules(
    core: &Core,
    lang: &str,
    mut errors: Vec<shared_model::Error>,
    category: Option<&str>,
) -> Vec<shared_model::Error> {
    let rule_ids: Vec<&str> = errors.iter().filter_map(|e| e.rule_id.as_deref()).collect();
    let disabled: std::collections::HashSet<String> = core
        .lexicon_store
        .get_rules(lang, &rule_ids)
        .ok()
        .map(|defs| {
            defs.into_iter()
                .filter_map(|d| d.filter(|d| !d.enabled).map(|d| d.id))
                .collect()
        })
        .unwrap_or_default();
    errors.retain(|e| !e.rule_id.as_deref().is_some_and(|id| disabled.contains(id)));
    errors.retain(|e| !core.custom_words.is_whitelisted(lang, &e.original));
    if let Some(cat) = category {
        errors.retain(|e| e.category.as_deref() == Some(cat));
    }
    errors
}

/// 每次命令构造编排上下文（设置即时生效：读当前设置，不缓存）
fn flow_ctx(core: &Arc<Core>) -> FlowCtx {
    let settings = core.settings.read().unwrap();
    let llm = core.llm.read().unwrap();
    FlowCtx {
        store: core.media_jobs.clone(),
        media: settings.media.clone(),
        llm: llm.config.clone(),
        prompts: llm.templates.clone(),
    }
}

/// 前端传来的 base64（可能带 data URL 前缀）→ 字节
fn decode_b64(raw: &str) -> Result<Vec<u8>, String> {
    let payload = raw.split_once(',').map(|(_, b)| b).unwrap_or(raw);
    STANDARD
        .decode(payload)
        .map_err(|e| format!("base64 解码失败：{e}"))
}

/// 产物字节 → `{ content_type, data_b64, size_bytes }`（预览 / 报告走 invoke 的唯一可行口径）
fn bytes_payload(path: &std::path::Path, ctype: &str) -> Result<serde_json::Value, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("读取失败：{e}"))?;
    Ok(serde_json::json!({
        "content_type": ctype,
        "data_b64": STANDARD.encode(&bytes),
        "size_bytes": bytes.len(),
    }))
}

/// `POST /gec/media/probes` 的桌面等价：三家工具 + 视觉档就绪探测（只读，不写文件）
#[tauri::command]
pub async fn media_probes(core: State<'_, Arc<Core>>) -> Result<serde_json::Value, String> {
    Ok(flow::probes(&flow_ctx(core.inner())).await)
}

/// `POST /gec/media/inspect` 的桌面等价：上传前能力探测（不落 job）
#[tauri::command]
pub async fn media_inspect(
    core: State<'_, Arc<Core>>,
    filename: String,
    data_b64: String,
) -> Result<serde_json::Value, String> {
    let bytes = decode_b64(&data_b64)?;
    flow::inspect_carrier(&flow_ctx(core.inner()), &filename, &bytes)
        .await
        .map_err(|e| e.to_string())
}

/// `POST /gec/media/jobs` 的桌面等价：建 job 并后台跑全链路（副本隔离，**永不改原件**）
///
/// 参数保持平铺：Tauri `invoke` 按参数名传 JSON，包一层结构体前端就要嵌套对象
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn media_create_job(
    core: State<'_, Arc<Core>>,
    filename: String,
    data_b64: String,
    apply_mode: Option<String>,
    lang: Option<String>,
    level: Option<u8>,
    force_llm: Option<bool>,
    category: Option<String>,
    mode: Option<String>,
) -> Result<MediaJob, String> {
    // 后台任务需要 'static：克隆 Core 的 Arc 出来用（State 生命周期不够）
    let core = core.inner().clone();
    let ctx = flow_ctx(&core);
    let bytes = decode_b64(&data_b64)?;
    flow::create_job(
        &ctx,
        DesktopDetector::new(core.clone()),
        &filename,
        &bytes,
        JobOptions {
            apply_mode,
            lang,
            level,
            force_llm,
            category,
            mode,
        },
    )
    .map_err(|e| e.to_string())
}

/// `GET /gec/media/jobs` 的桌面等价：job 列表（创建时间倒序）
#[tauri::command]
pub fn media_jobs(core: State<'_, Arc<Core>>) -> Result<Vec<MediaJob>, String> {
    Ok(core
        .media_jobs
        .read()
        .unwrap()
        .list()
        .into_iter()
        .cloned()
        .collect())
}

/// `GET /gec/media/jobs/:id` 的桌面等价：状态轮询（stage 到终态即停）
#[tauri::command]
pub fn media_job(core: State<'_, Arc<Core>>, id: String) -> Result<MediaJob, String> {
    core.media_jobs
        .read()
        .unwrap()
        .get(&id)
        .cloned()
        .ok_or_else(|| format!("job 不存在: {id}"))
}

/// `GET /gec/media/jobs/:id/preview` 的桌面等价：预览字节（base64，前端还原成 Blob）
#[tauri::command]
pub fn media_preview(core: State<'_, Arc<Core>>, id: String) -> Result<serde_json::Value, String> {
    let (path, ctype) =
        flow::preview_file(&flow_ctx(core.inner()), &id).map_err(|e| e.to_string())?;
    bytes_payload(&path, ctype)
}

/// `POST /gec/media/jobs/:id/apply` 的桌面等价：回写（默认 Overlay，幂等：重跑先清历史 `mark-*` 层）
#[tauri::command]
pub async fn media_apply(
    core: State<'_, Arc<Core>>,
    id: String,
    apply_mode: Option<String>,
) -> Result<MediaJob, String> {
    flow::apply_job(&flow_ctx(core.inner()), &id, apply_mode.as_deref())
        .await
        .map_err(|e| e.to_string())
}

/// `POST /gec/media/report` 的桌面等价：多个载体的结果汇成正误表（json / md / html / docx）
#[tauri::command]
pub async fn media_report(
    core: State<'_, Arc<Core>>,
    body: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let req: ReportRequest =
        serde_json::from_value(body).map_err(|e| format!("导出请求不合法：{e}"))?;
    flow::build_report(&flow_ctx(core.inner()), req)
        .await
        .map_err(|e| e.to_string())
}

/// `GET /gec/media/reports/:id?format=` 的桌面等价：取导出产物
///
/// 给了 `path`（前端弹原生「另存为」后传回）→ 直接写盘，只回 `{ filename, size_bytes }`
/// （与资源包导出同一口径）；未给则回 base64 让前端自建下载
#[tauri::command]
pub fn media_report_file(
    core: State<'_, Arc<Core>>,
    report_id: String,
    format: String,
    path: Option<String>,
) -> Result<serde_json::Value, String> {
    let (src, ctype) = flow::report_file_path(&flow_ctx(core.inner()), &report_id, &format)
        .map_err(|e| e.to_string())?;
    let Some(target) = path else {
        return bytes_payload(&src, ctype);
    };
    let bytes = std::fs::read(&src).map_err(|e| format!("读取导出产物失败：{e}"))?;
    if let Some(parent) = std::path::Path::new(&target).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&target, &bytes).map_err(|e| format!("写出失败：{e}"))?;
    Ok(serde_json::json!({
        "filename": target,
        "size_bytes": bytes.len(),
    }))
}
