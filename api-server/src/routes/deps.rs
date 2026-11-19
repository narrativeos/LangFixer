//! GET /gec/deps — 依赖服务状态快照（启动自检 + 主动拉起结果）
//! POST /gec/deps/start — 拉起单个本机可管依赖（decision / l3 / char_gate）
//! POST /gec/deps/recheck — 重新探测（可指定单项；不改配置、不拉起）

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::deps;
use crate::state::AppState;

fn valid_id(id: &str) -> bool {
    deps::DEPS.iter().any(|(i, _)| *i == id)
}

fn bad(msg: impl Into<String>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": msg.into() })),
    )
        .into_response()
}

pub async fn list(State(state): State<Arc<AppState>>) -> Json<deps::DepRegistry> {
    Json(deps::snapshot(&state))
}

#[derive(Deserialize)]
pub struct StartRequest {
    pub id: String,
}

pub async fn start(State(state): State<Arc<AppState>>, Json(req): Json<StartRequest>) -> Response {
    if !valid_id(&req.id) {
        return bad(format!(
            "未知依赖: {}（可用: {:?}）",
            req.id,
            deps::DEPS.iter().map(|(i, _)| *i).collect::<Vec<_>>()
        ));
    }
    if !matches!(req.id.as_str(), "decision" | "l3" | "char_gate") {
        return bad("该依赖是外部服务，本机不可拉起；请检查其是否可达，或在设置里更换地址");
    }
    // 拉起可能耗时分钟级（侧车加载 torch 模型）；前端用长超时等待
    let item = deps::bring_up_locked(&state, &req.id).await;
    Json(item).into_response()
}

#[derive(Deserialize, Default)]
pub struct RecheckRequest {
    pub id: Option<String>,
}

pub async fn recheck(
    State(state): State<Arc<AppState>>,
    Json(req): Json<RecheckRequest>,
) -> Response {
    let ids: Vec<&str> = match req.id {
        Some(ref id) => {
            if !valid_id(id) {
                return bad(format!("未知依赖: {id}"));
            }
            vec![id.as_str()]
        }
        None => deps::DEPS.iter().map(|(i, _)| *i).collect(),
    };
    for id in ids {
        deps::recheck(&state, id).await;
    }
    Json(deps::snapshot(&state)).into_response()
}
