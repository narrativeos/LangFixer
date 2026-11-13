//! 测评中心（进化中心 P0）：Python 测评服务（`datasets/eval_server.py`）的代理
//!
//! - `POST /gec/eval/run` — 发起评测任务（转发 `/eval/run`；任务在 Python 侧
//!   后台执行：设置快照 → 换臂/消融 → 逐句走生产检测路径 → 层级归因计分 →
//!   报告落盘 `datasets/eval_reports/` → 恢复设置）
//! - `GET /gec/eval/job/:id` — 任务进度（轮询；`status` = pending/running/done/error）
//! - `GET /gec/eval/reports` — 历史报告列表（含各臂 Fβ / 召回 / LLM 触达率摘要）
//! - `GET /gec/eval/report/:id` — 报告详情（overall + 层级归因 + 漏斗）
//! - `GET /gec/eval/layers` — 各层贡献度报告（**不转发**：api-server 直读报告
//!   档案，生产无测评实例也可用；评审密钥白名单内，见 `middleware/auth.rs`）
//!
//! 测评服务是开发工具（默认 `127.0.0.1:4807`，`GEC_EVAL_ENDPOINT` 可覆盖）；
//! 不可达时全部端点返回 503（不阻断检测主链路）。闭环定位见 `EVOLUTION.md`：
//! 测评中心是「检测 → 反思 → 训练 → 测试」的测试环节 + 层级贡献测量基础设施。

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::state::AppState;

/// 测评服务地址（`GEC_EVAL_ENDPOINT` 覆盖；缺省 127.0.0.1:4807）
fn eval_endpoint() -> String {
    std::env::var("GEC_EVAL_ENDPOINT")
        .map(|s| s.trim_end_matches('/').to_string())
        .unwrap_or_else(|_| "http://127.0.0.1:4807".to_string())
}

/// 转发到测评服务并透传响应（状态码 + 原始 JSON 体）
async fn forward(method: &str, path: &str, body: Option<&str>) -> Response {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
    {
        Ok(c) => c,
        Err(e) => return err_response(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let mut req = client.request(
        method.parse().unwrap(),
        format!("{}{}", eval_endpoint(), path),
    );
    if let Some(b) = body {
        req = req
            .header("Content-Type", "application/json")
            .body(b.to_string());
    }
    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            return err_response(
                StatusCode::SERVICE_UNAVAILABLE,
                format!(
                    "测评服务未运行（{path} 不可达：{e}；启动：cd datasets && python eval_server.py）"
                ),
            );
        }
    };
    let status = resp.status();
    let text = match resp.text().await {
        Ok(t) => t,
        Err(e) => return err_response(StatusCode::BAD_GATEWAY, e.to_string()),
    };
    (
        StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
        text,
    )
        .into_response()
}

fn err_response(status: StatusCode, msg: String) -> Response {
    (status, Json(serde_json::json!({ "error": msg }))).into_response()
}

/// POST /gec/eval/run — 发起评测（请求体透传：benchmark / split / arm / mode /
/// ablation / subset / seed / workers / fresh）
pub async fn run(State(_state): State<Arc<AppState>>, body: String) -> Response {
    forward("POST", "/eval/run", Some(&body)).await
}

/// GET /gec/eval/job/:id — 任务进度
pub async fn job(State(_state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    forward("GET", &format!("/eval/job/{id}"), None).await
}

/// GET /gec/eval/reports — 历史报告列表
pub async fn reports(State(_state): State<Arc<AppState>>) -> Response {
    forward("GET", "/eval/reports", None).await
}

/// GET /gec/eval/report/:id — 报告详情
pub async fn report(State(_state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    forward("GET", &format!("/eval/report/{id}"), None).await
}

/// 报告档案目录：`GEC_EVAL_REPORTS_DIR` 覆盖 > `datasets/eval_reports`（仓库 cwd）>
/// `src/datasets/eval_reports`（服务器部署 cwd 为安装根，git 跟踪的数据集在 src 下）
pub fn reports_dir() -> Option<std::path::PathBuf> {
    if let Some(d) = std::env::var_os("GEC_EVAL_REPORTS_DIR") {
        let p = std::path::PathBuf::from(d);
        return p.is_dir().then_some(p);
    }
    ["datasets/eval_reports", "src/datasets/eval_reports"]
        .iter()
        .map(std::path::PathBuf::from)
        .find(|p| p.is_dir())
}

/// 响应内报告条数上限（档案会随评测次数膨胀，展示取最近若干份即可）
const LAYERS_REPORTS_CAP: usize = 60;

/// 报告排序键（ISO ts 字典序即时间序；缺失按最早处理）
fn report_ts(v: &serde_json::Value) -> &str {
    v.get("ts").and_then(|t| t.as_str()).unwrap_or("")
}

/// GET /gec/eval/layers — 各层贡献度报告（api-server 直读报告档案，不依赖
/// Python 测评服务：生产评测实例未起时也能展示「质量来自哪一层」；
/// 评审密钥经 `review_denied` 白名单可读，属质量透明面，与 detect 同级）
pub async fn layers() -> Response {
    let Some(dir) = reports_dir() else {
        return err_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "未找到评测报告目录（可设 GEC_EVAL_REPORTS_DIR；档案由评测任务落盘到 datasets/eval_reports）"
                .to_string(),
        );
    };
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) => return err_response(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let mut reports: Vec<serde_json::Value> = Vec::new();
    let mut skipped = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        // 有效报告 = 评测任务落盘的报告（带 id + layers 层级归因）；
        // 档案内辅助文件（如 baseline_rules.json）与损坏/半写文件按条跳过
        let Ok(text) = std::fs::read_to_string(&path) else {
            skipped += 1;
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            skipped += 1;
            continue;
        };
        if v.get("id").and_then(|i| i.as_str()).is_none()
            || v.get("layers").and_then(|l| l.as_object()).is_none()
        {
            skipped += 1;
            continue;
        }
        reports.push(v);
    }
    reports.sort_by(|a, b| report_ts(b).cmp(report_ts(a)));
    reports.truncate(LAYERS_REPORTS_CAP);
    Json(serde_json::json!({
        "dir": dir.display().to_string(),
        "reports": reports,
        "skipped": skipped,
    }))
    .into_response()
}
