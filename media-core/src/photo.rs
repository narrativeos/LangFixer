//! PhotoCraft 适配器（图片载体，P0；契约基线 = `scripts/media_smoke.sh`）
//!
//! 已实测的 CLI 形态（0.16.0）：
//!
//! ```sh
//! photocraft-cli info <FILE> --compact     # 单行 JSON：width/height/layers[]{id,kind,name,bounds,text}
//! photocraft-cli run <FILE> --cmd <id> --params '<json>' … --out <OUT>
//! photocraft-cli convert <IN> <OUT.png>    # 扁平预览（实测会 flatten，故必须双产物）
//! ```
//!
//! 已实测的坑（实现必须照做）：
//! - 结构编辑后 **layer id 会重排** → 每阶段前重新 `info`，绝不跨命令缓存 id；
//! - `layer.new.group` 建的是空组，后续图层不会自动进组 → 必须 `layer.moveTo {position:"into"}`；
//! - `type.edit` 的 `replace.start/end` 是 **字符** 下标（实测 CJK 正确），
//!   我们的 span 是字节 → 必须换算；
//! - 默认字体 Inter，CJK 标注必须显式指定 CJK 字体；
//! - 导出 PNG 会 flatten → 产物必须同时给可编辑 `.psd` 与预览 `.png`。

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::process::Command;

use crate::adapter::MediaError;
use crate::adapter::{ApplyRequest, ApplyResult};
use crate::model::{CarrierInfo, CarrierKind, ExtractSource, LayerKind, Region, TextUnit};

/// 标注层命名前缀（幂等清理按此前缀删；也是 UI 图层的约定）
pub const MARK_PREFIX: &str = "mark-";
/// 标注组名（所有 LangFixer 标注收进一个组，一键显隐）
pub const DEFAULT_GROUP: &str = "LangFixer Marks";

/// 常见安装位置（GUI 启动的进程不继承交互 shell 的 `PATH`，见 MULTIMODAL.md §13）
pub fn default_candidates() -> Vec<PathBuf> {
    default_candidates_for("photocraft-cli")
}

/// 同上，但按工具名（wordcraft-cli / pdfcraft-cli 共用同一套解析规则）
pub fn default_candidates_for(name: &str) -> Vec<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_default();
    let mut v = vec![PathBuf::from(format!("{home}/.local/bin/{name}"))];
    for dir in ["/usr/local/bin", "/opt/homebrew/bin"] {
        v.push(PathBuf::from(format!("{dir}/{name}")));
    }
    v
}

fn expand_tilde(s: &str) -> PathBuf {
    match s.strip_prefix("~/") {
        Some(rest) => PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(rest),
        None => PathBuf::from(s),
    }
}

/// 在 `$PATH` 里找可执行文件（不依赖 `which` 子进程）
fn lookup_in_path(name: &str) -> Option<PathBuf> {
    if name.contains('/') {
        let p = expand_tilde(name);
        return if p.is_file() { Some(p) } else { None };
    }
    std::env::var("PATH")
        .ok()?
        .split(':')
        .map(|dir| PathBuf::from(dir).join(name))
        .find(|p| p.is_file())
}

/// 三级解析：配置里的绝对路径 → `$PATH` → 常见安装位置
pub fn resolve_bin(explicit: Option<&str>) -> Option<PathBuf> {
    resolve_tool_bin(explicit, "photocraft-cli")
}

/// 同 [`resolve_bin`]，但工具名可变（wordcraft-cli / pdfcraft-cli）
pub fn resolve_tool_bin(explicit: Option<&str>, name: &str) -> Option<PathBuf> {
    if let Some(e) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(p) = lookup_in_path(e) {
            return Some(p);
        }
        tracing::warn!(explicit = %e, "配置的可执行路径不可用，回退 $PATH / 常见位置");
    }
    if let Some(p) = lookup_in_path(name) {
        return Some(p);
    }
    default_candidates_for(name)
        .into_iter()
        .find(|p| p.is_file())
}

#[derive(Debug, Clone)]
pub struct PhotoCraftConfig {
    /// 设置里显式配置的可执行路径（空 = 自动解析）
    pub bin: Option<String>,
    /// 单次 CLI 调用超时
    pub timeout_secs: u64,
    /// 标注文字字体（实测默认 Inter 下 CJK 会回退成方框）
    pub cjk_font: String,
    /// 标注组名
    pub group: String,
    /// 标注颜色 `#RRGGBB`
    pub mark_color: String,
}

impl Default for PhotoCraftConfig {
    fn default() -> Self {
        Self {
            bin: None,
            timeout_secs: 120,
            cjk_font: "Heiti SC".to_string(),
            group: DEFAULT_GROUP.to_string(),
            mark_color: "#e03131".to_string(),
        }
    }
}

/// 一次 CLI 调用的输出（stderr 里的 warning 不等于失败，如 PNG flatten 警告）
pub struct CallOut {
    pub stdout: String,
    pub stderr: String,
}

impl CallOut {
    /// stdout 解析为 JSON（非 JSON → `Null`，由调用方决定是否需要）
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.stdout).unwrap_or(Value::Null)
    }
    pub fn first_line(&self) -> String {
        self.stdout
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .to_string()
    }
}

/// 文字层（`info` 的 `layers[]` 递归展平后的 Type 层）
#[derive(Debug, Clone)]
pub struct TypeLayer {
    pub id: u32,
    pub name: String,
    pub bounds: Option<[f64; 4]>,
    pub text: String,
    pub font: Option<String>,
}

/// 子层数组：顶层文档用 `layers`，组节点用 `children`（实测 `info` 输出）
fn child_layers(v: &Value) -> Option<&Vec<Value>> {
    v.get("layers")
        .or(v.get("children"))
        .and_then(Value::as_array)
}

/// 递归收集 Type 文字层（含组内子层）
pub fn collect_type_layers(v: &Value, out: &mut Vec<TypeLayer>) {
    let Some(layers) = child_layers(v) else {
        return;
    };
    for l in layers {
        if l.get("kind").and_then(Value::as_str) == Some("Type") {
            out.push(TypeLayer {
                id: l.get("id").and_then(Value::as_u64).unwrap_or(0) as u32,
                name: l
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                bounds: l
                    .get("bounds")
                    .and_then(Value::as_array)
                    .filter(|a| a.len() == 4)
                    .map(|a| [0, 1, 2, 3].map(|i| a[i].as_f64().unwrap_or(0.0))),
                text: l
                    .get("text")
                    .and_then(|t| t.get("text"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                font: l
                    .get("text")
                    .and_then(|t| t.get("font"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
            });
        }
        collect_type_layers(l, out);
    }
}

/// 递归收集满足条件 `pred` 的图层 id
pub fn collect_ids_where(v: &Value, pred: &dyn Fn(&Value) -> bool, out: &mut Vec<u32>) {
    let Some(layers) = child_layers(v) else {
        return;
    };
    for l in layers {
        if pred(l) {
            out.push(l.get("id").and_then(Value::as_u64).unwrap_or(0) as u32);
        }
        collect_ids_where(l, pred, out);
    }
}

/// 字节偏移 → 字符偏移（`type.edit` 的 `replace` 用字符下标，实测 CJK 正确）
pub fn byte_to_char(text: &str, byte: usize) -> usize {
    text[..byte.min(text.len())].chars().count()
}

pub struct PhotoCraft {
    bin: PathBuf,
    cfg: PhotoCraftConfig,
}

/// 中间产物路径（`result.psd` → `result.clean.psd` / `result.marks.psd`）
fn stage_path(target: &Path, tag: &str) -> PathBuf {
    let stem = target
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "result".into());
    target.with_file_name(format!("{stem}.{tag}.psd"))
}

/// 标注落点：从 region 取像素矩形（`LayerRef.bounds` 或 `ImageBox`）
fn mark_rect(a: &crate::model::Annotation) -> Option<[f64; 4]> {
    use crate::model::Region as R;
    match a.region {
        R::LayerRef {
            bounds: Some(b), ..
        } => Some(b),
        R::ImageBox { x, y, w, h } => Some([x, y, w, h]),
        _ => None,
    }
}

impl PhotoCraft {
    pub fn new(cfg: PhotoCraftConfig) -> Result<Self, MediaError> {
        let bin = resolve_bin(cfg.bin.as_deref()).ok_or_else(|| {
            MediaError::NotReady(
                "photocraft-cli 未找到（装到 ~/.local/bin/photocraft-cli，或在设置里给绝对路径）"
                    .to_string(),
            )
        })?;
        Ok(Self { bin, cfg })
    }

    pub fn bin_path(&self) -> &Path {
        &self.bin
    }

    pub fn config(&self) -> &PhotoCraftConfig {
        &self.cfg
    }

    async fn call(&self, args: &[String]) -> Result<CallOut, MediaError> {
        let mut cmd = Command::new(&self.bin);
        cmd.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let fut = cmd.output();
        let out = tokio::time::timeout(Duration::from_secs(self.cfg.timeout_secs), fut)
            .await
            .map_err(|_| MediaError::Timeout {
                secs: self.cfg.timeout_secs,
            })?
            .map_err(|e| MediaError::Internal(format!("启动 {} 失败：{e}", self.bin.display())))?;
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        if !out.status.success() {
            let detail = stderr
                .lines()
                .chain(stdout.lines())
                .find(|l| !l.trim().is_empty())
                .unwrap_or("无输出")
                .to_string();
            return Err(MediaError::ToolFailed {
                tool: "photocraft-cli".into(),
                detail,
            });
        }
        Ok(CallOut { stdout, stderr })
    }

    /// `info <FILE> --compact` → 文档 JSON
    pub async fn info(&self, file: &Path) -> Result<Value, MediaError> {
        let o = self
            .call(&[
                "info".into(),
                file.display().to_string(),
                "--compact".into(),
            ])
            .await?;
        let v = o.json();
        if v.is_null() {
            return Err(MediaError::ToolFailed {
                tool: "photocraft-cli".into(),
                detail: "info 输出不是 JSON".into(),
            });
        }
        Ok(v)
    }

    /// `run <FILE> --cmd … --out <OUT>`
    async fn run_cmds(
        &self,
        file: &Path,
        cmds: &[(String, Value)],
        out: &Path,
    ) -> Result<CallOut, MediaError> {
        let mut args = vec!["run".into(), file.display().to_string()];
        for (id, params) in cmds {
            args.push("--cmd".into());
            args.push(id.clone());
            args.push("--params".into());
            args.push(params.to_string());
        }
        args.push("--out".into());
        args.push(out.display().to_string());
        self.call(&args).await
    }

    /// 可用字体族（CJK 标注前置校验）
    pub async fn font_families(&self) -> Result<Vec<String>, MediaError> {
        let o = self
            .call(&["run".into(), "--cmd".into(), "type.fonts".into()])
            .await?;
        Ok(o.json()
            .get("families")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default())
    }

    /// `--version` 首行
    pub async fn version(&self) -> Result<String, MediaError> {
        Ok(self.call(&["--version".into()]).await?.first_line())
    }

    /// 关键原语是否齐备（缺任一 → degraded，回写降级 ListOnly）
    pub async fn command_snapshot(&self) -> Result<Vec<String>, MediaError> {
        let o = self.call(&["commands".into(), "--json".into()]).await?;
        Ok(o.json()
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|c| c.get("id").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default())
    }
}

#[async_trait::async_trait]
impl crate::adapter::Collaborator for PhotoCraft {
    fn kind(&self) -> &'static str {
        "image"
    }

    async fn apply(&self, req: &ApplyRequest<'_>) -> Result<ApplyResult, MediaError> {
        self.apply_marks(req).await
    }

    async fn probe(&self) -> Result<crate::adapter::ToolProbe, MediaError> {
        use crate::adapter::{ToolCapability as Cap, ToolProbe};
        let version = self.version().await.unwrap_or_default();
        let cmds = self.command_snapshot().await.unwrap_or_default();
        let need = [
            "type.edit",
            "type.create",
            "shape.create",
            "layer.new.group",
            "layer.moveTo",
            "layer.delete",
            "layer.select",
        ];
        // commands 读不到（旧版/权限）时不武断判定 degraded，交给调用时的真实错误
        let missing: Vec<&str> = if cmds.is_empty() {
            vec![]
        } else {
            need.iter()
                .copied()
                .filter(|n| !cmds.iter().any(|c| c == *n))
                .collect()
        };
        let mut notes = Vec::new();
        match self.font_families().await {
            Ok(f) if !f.iter().any(|x| x == &self.cfg.cjk_font) => notes.push(format!(
                "字体「{}」未找到，中文标注可能显示为方框（设置里换 cjk_font）",
                self.cfg.cjk_font
            )),
            Ok(_) => notes.push(format!("CJK 标注字体可用：{}", self.cfg.cjk_font)),
            Err(_) => notes.push("字体列表不可读（无法预校验 CJK 字体）".into()),
        }
        Ok(ToolProbe {
            app: "photocraft".into(),
            bin: self.bin.display().to_string(),
            version,
            ready: missing.is_empty(),
            capabilities: vec![
                Cap::InspectText,
                Cap::EditInPlace,
                Cap::OverlayMarks,
                Cap::Preview,
            ],
            degraded_reason: (!missing.is_empty())
                .then(|| format!("缺少原语：{}", missing.join(", "))),
            notes,
        })
    }

    async fn inspect(&self, file: &Path) -> Result<CarrierInfo, MediaError> {
        let info = self.info(file).await?;
        let mut layers = Vec::new();
        collect_type_layers(&info, &mut layers);
        let n_text = layers.iter().filter(|l| !l.text.trim().is_empty()).count();
        let mut warnings: Vec<String> = info
            .get("warnings")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        if n_text == 0 {
            warnings.push("无文字层：只能走视觉/OCR 档，回写只能 Overlay".to_string());
        }
        Ok(CarrierInfo {
            kind: CarrierKind::Image,
            width: info.get("width").and_then(Value::as_u64).map(|v| v as u32),
            height: info.get("height").and_then(Value::as_u64).map(|v| v as u32),
            pages: Some(1),
            text_units: n_text,
            editable: n_text > 0,
            warnings,
        })
    }

    async fn extract_units(&self, file: &Path) -> Result<Vec<TextUnit>, MediaError> {
        let info = self.info(file).await?;
        let mut layers = Vec::new();
        collect_type_layers(&info, &mut layers);
        Ok(layers
            .into_iter()
            .filter(|l| !l.text.trim().is_empty())
            .enumerate()
            .map(|(i, l)| TextUnit {
                id: (i + 1) as u32,
                text: l.text,
                source: ExtractSource::Structure,
                region: Region::LayerRef {
                    layer_id: l.id,
                    kind: LayerKind::Type,
                    bounds: l.bounds,
                },
                confidence: 1.0,
                editable: true,
                page: Some(1),
            })
            .collect())
    }

    async fn render_preview(&self, file: &Path, out: &Path) -> Result<(), MediaError> {
        let o = self
            .call(&[
                "convert".into(),
                file.display().to_string(),
                out.display().to_string(),
            ])
            .await?;
        if !o.stderr.trim().is_empty() {
            // 实测 flatten 警告走 stderr，不影响产物（故必须双产物：.psd + .png）
            tracing::debug!(stderr = %o.stderr.trim(), "photocraft convert 提示");
        }
        Ok(())
    }
}

/// 回写计划：`Annotation[]` → PhotoCraft 命令序列 + 记账（纯计算，便于单测）
#[derive(Debug, Default)]
pub struct Plan {
    pub cmds: Vec<(String, Value)>,
    pub applied: Vec<u32>,
    pub skipped: Vec<(u32, String)>,
    pub warnings: Vec<String>,
}

impl PhotoCraft {
    /// 把标注翻译成命令。InPlace 走 `type.edit`（字节→字符下标换算），
    /// Overlay 走 `shape.create` + `type.create`；不可定位的一律 skip（不画假框）
    fn build_plan(&self, annotations: &[crate::model::Annotation], units: &[TextUnit]) -> Plan {
        use crate::model::ApplyMode;
        let mut p = Plan::default();
        for a in annotations {
            let mode = match a.apply {
                ApplyMode::ListOnly => {
                    p.skipped.push((a.id, "只出清单，不写载体".into()));
                    continue;
                }
                ApplyMode::Comment => {
                    p.warnings
                        .push(format!("#{} 图片载体无批注原语，按 Overlay 处理", a.id));
                    ApplyMode::Overlay
                }
                m => m,
            };
            if mode == ApplyMode::InPlace {
                let layer_id = match a.region {
                    Region::LayerRef { layer_id, .. } => layer_id,
                    _ => {
                        p.skipped.push((a.id, "InPlace 需要文字层对象".into()));
                        continue;
                    }
                };
                let Some(u) = units.iter().find(|u| u.id == a.unit_id) else {
                    p.skipped.push((a.id, "找不到对应文本单元".into()));
                    continue;
                };
                let corr = a.error.correction.clone().unwrap_or_default();
                if corr.is_empty() {
                    p.skipped.push((a.id, "无修正文本，无法改字".into()));
                    continue;
                }
                p.cmds.push((
                    "type.edit".to_string(),
                    json!({"layer": layer_id,
                           "replace": {
                               "start": byte_to_char(&u.text, a.local_span[0]),
                               "end": byte_to_char(&u.text, a.local_span[1]),
                               "text": corr}}),
                ));
                p.applied.push(a.id);
            }
            // 标注层（InPlace 也叠编号层，便于对照）
            match mark_rect(a) {
                Some([x, y, w, h]) => {
                    p.cmds.push((
                        "shape.create".to_string(),
                        json!({"kind": "rect", "rect": [x, y, w, h], "fill": null,
                               "stroke": {"width": 3, "color": a.style.color},
                               "name": format!("{MARK_PREFIX}{}-box", a.id)}),
                    ));
                    let note = format!(
                        "#{} {}",
                        a.id,
                        a.error.correction.as_deref().unwrap_or(&a.error.message)
                    );
                    p.cmds.push((
                        "type.create".to_string(),
                        json!({"x": x, "y": y + h + 26.0, "text": note, "size": 20,
                               "color": a.style.color, "font": self.cfg.cjk_font,
                               "name": format!("{MARK_PREFIX}{}-text", a.id)}),
                    ));
                }
                None if mode != ApplyMode::InPlace => {
                    p.skipped.push((a.id, "无几何信息，不画假框".into()));
                }
                None => {}
            }
        }
        p
    }
}

impl PhotoCraft {
    /// 回写（三段式，每段前重新 `info`——结构编辑后 layer id 会重排）
    pub async fn apply_marks(&self, req: &ApplyRequest<'_>) -> Result<ApplyResult, MediaError> {
        let mut res = ApplyResult::default();

        // 1) 幂等清理：删历史 mark-* 层 + 历史标注组
        let src_info = self.info(req.source).await?;
        let mut stale: Vec<u32> = Vec::new();
        collect_ids_where(
            &src_info,
            &|l| {
                l.get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|n| n.starts_with(MARK_PREFIX))
            },
            &mut stale,
        );
        collect_ids_where(
            &src_info,
            &|l| l.get("name").and_then(Value::as_str) == Some(self.cfg.group.as_str()),
            &mut stale,
        );
        let mut stage = req.source.to_path_buf();
        if !stale.is_empty() {
            let cleaned = stage_path(req.target, "clean");
            let mut cmds: Vec<(String, Value)> = stale
                .iter()
                .map(|id| {
                    (
                        "layer.select".to_string(),
                        json!({"layer": id, "mode": "add"}),
                    )
                })
                .collect();
            cmds.push(("layer.delete".to_string(), json!({})));
            self.run_cmds(&stage, &cmds, &cleaned).await?;
            stage = cleaned;
            res.warnings
                .push(format!("清理了 {} 个历史标注图层", stale.len()));
        }

        // 2) 主写入
        if stage != req.source {
            self.info(&stage).await?; // 确认可读（id 已重排，后续只用 name 匹配）
        }
        let plan = self.build_plan(req.annotations, req.units);
        res.applied = plan.applied;
        res.skipped = plan.skipped;
        res.warnings.extend(plan.warnings);
        if plan.cmds.is_empty() {
            std::fs::copy(&stage, req.target)
                .map_err(|e| MediaError::Internal(format!("复制产物失败：{e}")))?;
        } else {
            let mut cmds = plan.cmds;
            cmds.insert(
                0,
                (
                    "layer.new.group".to_string(),
                    json!({"name": self.cfg.group}),
                ),
            );
            let marked = stage_path(req.target, "marks");
            self.run_cmds(&stage, &cmds, &marked).await?;
            stage = marked;

            // 3) 入组（实测 new.group 不自动收纳后续图层）
            let minfo = self.info(&stage).await?;
            let mut group: Vec<u32> = Vec::new();
            let mut marks: Vec<u32> = Vec::new();
            collect_ids_where(
                &minfo,
                &|l| l.get("name").and_then(Value::as_str) == Some(self.cfg.group.as_str()),
                &mut group,
            );
            collect_ids_where(
                &minfo,
                &|l| {
                    l.get("name")
                        .and_then(Value::as_str)
                        .is_some_and(|n| n.starts_with(MARK_PREFIX))
                },
                &mut marks,
            );
            match (group.first(), marks.is_empty()) {
                (Some(g), false) => {
                    self.run_cmds(
                        &stage,
                        &[(
                            "layer.moveTo".to_string(),
                            json!({"layers": marks, "target": g, "position": "into"}),
                        )],
                        req.target,
                    )
                    .await?;
                }
                _ => {
                    std::fs::rename(&stage, req.target)
                        .map_err(|e| MediaError::Internal(format!("产物落位失败：{e}")))?;
                }
            }
        }
        res.artifacts.push(req.target.to_path_buf());
        Ok(res)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_to_char_handles_cjk() {
        let s = "或血压心率增高";
        assert_eq!(byte_to_char(s, 0), 0);
        assert_eq!(byte_to_char(s, 3), 1); // 「或」占 3 字节
        assert_eq!(byte_to_char(s, 9), 3);
        assert_eq!(byte_to_char(s, s.len()), s.chars().count());
        assert_eq!(byte_to_char(s, 999), s.chars().count()); // 越界钳制
    }

    #[test]
    fn collect_type_layers_walks_groups() {
        let v: Value = json!({
            "width": 900, "height": 300,
            "layers": [
                {"id": 3, "kind": "Type", "name": "headline", "bounds": [63, 85, 264, 37],
                 "text": {"font": "Inter", "sizePt": 48.0, "text": "He'llo wrold"}},
                {"id": 5, "kind": "Group", "name": "LangFixer Marks", "children": [
                    {"id": 6, "kind": "Type", "name": "mark-1-text", "bounds": [1,2,3,4],
                     "text": {"font": "Heiti SC", "sizePt": 20.0, "text": "#1 建议"}}
                ]},
                {"id": 2, "kind": "Pixel", "name": "Background"}
            ]
        });
        let mut out = Vec::new();
        collect_type_layers(&v, &mut out);
        assert_eq!(out.len(), 2, "组内 Type 层也要收到，Pixel 层不算");
        assert_eq!(out[0].bounds, Some([63.0, 85.0, 264.0, 37.0]));
        assert_eq!(out[1].name, "mark-1-text");
    }

    #[test]
    fn stale_marks_are_selected_by_name_prefix() {
        let v: Value = json!({"layers": [
            {"id": 7, "kind": "Shape", "name": "mark-1-box"},
            {"id": 8, "kind": "Type", "name": "mark-1-text"},
            {"id": 9, "kind": "Group", "name": "LangFixer Marks"},
            {"id": 3, "kind": "Type", "name": "headline"}
        ]});
        let mut ids = Vec::new();
        collect_ids_where(
            &v,
            &|l| {
                l.get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|n| n.starts_with(MARK_PREFIX))
            },
            &mut ids,
        );
        assert_eq!(ids, vec![7, 8]);
    }

    #[test]
    fn resolve_bin_falls_back_when_explicit_missing() {
        let p = resolve_bin(Some("/nonexistent/photocraft-cli"));
        // 本机装了就能在 PATH/常见位置找到；没装则 None。两种都不 panic
        if let Some(p) = p {
            assert!(p.is_file());
        }
    }
}
