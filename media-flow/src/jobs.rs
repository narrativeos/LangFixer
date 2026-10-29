//! 多模态审校 job 状态（`MULTIMODAL.md` §8）
//!
//! 每个 job 一个独立目录（`{workdir}/{job_id}/`）：原件副本、中间产物、
//! `result.*`、`annotations.json` 全在里面。**永不改原件**。
//! 状态机：`queued → copying → inspecting → extracting → detecting → remapping → ready
//!          →(apply) applying → applied`，任一步失败 → `failed`（带 error）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use media_core::{
    Annotation, ApplyMode, Artifacts, CarrierInfo, CarrierKind, JobMode, PreviewKind, Stitched,
    TextUnit,
};

/// job 阶段
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaStage {
    Queued,
    Copying,
    Inspecting,
    Extracting,
    Detecting,
    Remapping,
    Ready,
    Applying,
    Applied,
    Failed,
}

impl MediaStage {
    /// 终态（前端可停止轮询）
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Ready | Self::Applied | Self::Failed)
    }
}

/// 一个多模态 job
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaJob {
    pub id: String,
    pub filename: String,
    pub kind: CarrierKind,
    /// 工作模式（简单 = 零外部协作工具；高级 = 三家适配器）——决定读法与回写能力
    #[serde(default)]
    pub mode: JobMode,
    /// 预览面（前端据此选渲染分支：位图画布 / 只读文档 / 无预览）
    #[serde(default)]
    pub preview_kind: PreviewKind,
    /// 实际用了谁来读这份载体（`photocraft` / `pdfcraft` / `wordcraft` /
    /// `document-parse` / `vision`）——留痕用，别看猜测
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    pub stage: MediaStage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carrier: Option<CarrierInfo>,
    /// 抽取到的文本单元
    pub units: Vec<TextUnit>,
    /// 文本来源档：`structure`（A 档，真值）/ `vision`（B 档）/ `ocr`（C 档）/ `none`
    /// （A 档有文本时不会被 B/C 档覆盖）
    #[serde(default)]
    pub extraction: String,
    /// B/C 档实际使用的模型 / 引擎名
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extraction_model: Option<String>,
    /// 拼接产物（检测输入 + 反查表；UI 可直接展示 stitched text 与单元高亮）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stitched: Option<Stitched>,
    /// 回映射后的标注（含 error 本体 → 归因/讲解/反馈全接得上）
    pub annotations: Vec<Annotation>,
    /// 检测层报告（与 /gec/detect 的 layers 同构）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<serde_json::Value>,
    pub apply_mode: ApplyMode,
    pub artifacts: Artifacts,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl MediaJob {
    /// 按 id 取文本单元（标注落点、上下文回看都按 id 找）
    pub fn unit(&self, id: u32) -> Option<&TextUnit> {
        self.units.iter().find(|u| u.id == id)
    }

    /// 按 id 取标注
    pub fn annotation(&self, id: u32) -> Option<&Annotation> {
        self.annotations.iter().find(|a| a.id == id)
    }
}

/// job 存储（内存表 + 磁盘目录；P0 不落 SQLite，重启后 job 记录丢失但产物仍在）
pub struct MediaJobStore {
    pub root: PathBuf,
    jobs: HashMap<String, MediaJob>,
}

impl MediaJobStore {
    pub fn new(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref().to_path_buf();
        let _ = std::fs::create_dir_all(&root);
        Self {
            root,
            jobs: HashMap::new(),
        }
    }

    /// job 工作目录（不存在则创建）
    pub fn job_dir(&self, id: &str) -> PathBuf {
        let dir = self.root.join(id);
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    pub fn insert(&mut self, job: MediaJob) {
        self.jobs.insert(job.id.clone(), job);
    }

    pub fn get(&self, id: &str) -> Option<&MediaJob> {
        self.jobs.get(id)
    }

    pub fn list(&self) -> Vec<&MediaJob> {
        let mut v: Vec<&MediaJob> = self.jobs.values().collect();
        v.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        v
    }

    /// 更新（闭包改状态；job 不存在返回 None）
    pub fn update<F: FnOnce(&mut MediaJob)>(&mut self, id: &str, f: F) -> Option<MediaJob> {
        let job = self.jobs.get_mut(id)?;
        f(job);
        job.updated_at = chrono::Utc::now().to_rfc3339();
        Some(job.clone())
    }

    /// 置为失败（记录阶段与原因）
    pub fn fail(&mut self, id: &str, msg: &str) {
        self.update(id, |j| {
            j.stage = MediaStage::Failed;
            j.error = Some(msg.to_string());
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use media_core::Artifacts;

    fn job(id: &str) -> MediaJob {
        MediaJob {
            id: id.to_string(),
            filename: "a.psd".into(),
            kind: CarrierKind::Image,
            mode: JobMode::Advanced,
            preview_kind: PreviewKind::ImageCanvas,
            tool: Some("photocraft".into()),
            stage: MediaStage::Queued,
            error: None,
            carrier: None,
            units: vec![],
            extraction: "none".into(),
            extraction_model: None,
            stitched: None,
            annotations: vec![],
            layers: vec![],
            apply_mode: ApplyMode::Overlay,
            artifacts: Artifacts::default(),
            warnings: vec![],
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn store_tracks_stage_and_failure() {
        let dir =
            std::env::temp_dir().join(format!("lf_media_jobs_{}", uuid::Uuid::new_v4().simple()));
        let mut s = MediaJobStore::new(&dir);
        s.insert(job("j1"));
        assert_eq!(s.get("j1").unwrap().stage, MediaStage::Queued);
        s.update("j1", |j| j.stage = MediaStage::Detecting);
        assert_eq!(s.get("j1").unwrap().stage, MediaStage::Detecting);
        s.fail("j1", "photocraft-cli 未找到");
        let j = s.get("j1").unwrap();
        assert_eq!(j.stage, MediaStage::Failed);
        assert!(j.stage.is_terminal());
        assert_eq!(j.error.as_deref(), Some("photocraft-cli 未找到"));
        // job 目录按 id 隔离
        assert!(s.job_dir("j1").ends_with("j1"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
