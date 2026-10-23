//! 测评中心（进化中心 P0）：Python 测评服务（`datasets/eval_server.py`）的代理
//!
//! - `POST /gec/eval/run` — 发起评测任务（转发 `/eval/run`；任务在 Python 侧
//!   后台执行：设置快照 → 换臂/消融 → 逐句走生产检测路径 → 层级归因计分 →
//!   报告落盘 `datasets/eval_reports/` → 恢复设置）
//! - `GET /gec/eval/job/:id` — 任务进度（轮询；`status` = pending/running/done/error）
//! - `GET /gec/eval/reports` — 历史报告列表（含各臂 Fβ / 召回 / LLM 触达率摘要）
//! - `GET /gec/eval/report/:id` — 报告详情（overall + 层级归因 + 漏斗）
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
