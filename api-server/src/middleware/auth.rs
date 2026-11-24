//! 鉴权门：网关形态要求评审令牌（Bearer / 会话 cookie）或超管凭证（Bearer /
//! 超管会话 cookie）；单机形态业务端点保持全开放零变化，但识别并标记有效凭证
//! （超管 Bearer / 安装后创建的会话），供设置面在「owner 激活」后要求登录态。
//!
//! 放行的每个请求都会注入 `RequestScope`（原始身份）与 `Tenant`（业务守卫视图），
//! handler 侧统一用 `Extension<Tenant>` 取用（自定义提取器在 edition 2024 下与
//! axum-core 0.4 生命周期不兼容，见 `auth::Tenant::from_scope`）。
//! 超管会话在 `gw_sessions.token_id` 用伪 id `~admin` 表示，解析后归一为 admin 域。

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::header;
use axum::middleware::Next;
use axum::response::{IntoResponse as _, Response};

use crate::auth::{ADMIN_TOKEN_ID, AuthContext, GatewayConfig, RequestScope, Tenant};

pub async fn gate(State(cfg): State<Arc<GatewayConfig>>, mut req: Request, next: Next) -> Response {
    // 两种形态都把配置放进扩展，供需要直接取 store 的路由统一取用
    req.extensions_mut().insert(cfg.clone());

    let bearer = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_string);
    let sid = session_cookie(req.headers());

    // 单机形态：默认带登录门（login_gate）——业务端点要求有效凭证（超管 Bearer
    // 或密码/密钥换来的会话 cookie），未登录一律 401 + login_required；
    // LANGFIXER_LOGIN=0 或库嵌入装配（standalone 直通配置）时保持原有零变化直通。
    if !cfg.enabled {
        let mut authed = false;
        if let Some(token) = &bearer {
            authed |= cfg.admin.matches(token);
        }
        if let Some(sid) = &sid {
            authed |= matches!(cfg.store.resolve_session(sid), Ok(Some(_)));
        }
        let path = req.uri().path();
        let method = req.method().clone();
        if !authed && cfg.login_gate && !is_public_path(&method, path) {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                [(header::CONTENT_TYPE, "application/json")],
                r#"{"error":"unauthorized: 需要登录","login_required":true}"#,
            )
                .into_response();
        }
        admit(
            &mut req,
            &cfg,
            RequestScope {
                token_id: None,
                is_admin: true,
            },
            authed,
        );
        return next.run(req).await;
    }

    let path = req.uri().path().to_string();
    let method = req.method().clone();
    let started = std::time::Instant::now();
    let (log_token, resp) = decide(&cfg, req, next, bearer, sid, &method, &path).await;
    // 调用级日志：放行与拒绝都记一行（隐私口径见 TokenStore::log_call）
    cfg.store.log_call(
        log_token.as_deref(),
        &path,
        resp.status().as_u16(),
        started.elapsed().as_millis() as u64,
    );
    resp
}

/// 网关形态判定：返回（日志行身份，响应）。身份为 None 涵盖超管、匿名放行与
/// 无法核验的凭证；**核验成功的评审令牌即使被拒也记本人**——他人的越权尝试
/// 不能洗成匿名。
async fn decide(
    cfg: &Arc<GatewayConfig>,
    mut req: Request,
    next: Next,
    bearer: Option<String>,
    sid: Option<String>,
    method: &axum::http::Method,
    path: &str,
) -> (Option<String>, Response) {
    if let Some(token) = &bearer {
        if cfg.admin.matches(token) {
            admit(
                &mut req,
                cfg,
                RequestScope {
                    token_id: None,
                    is_admin: true,
                },
                true,
            );
            return (None, next.run(req).await);
        }
        if path.starts_with("/admin") {
            // 评审令牌不得进入管理面
            let tid = token_or_none(cfg, token).and_then(|s| s.token_id);
            return (tid, axum::http::StatusCode::FORBIDDEN.into_response());
        }
        if let Some(scope) = token_or_none(cfg, token) {
            let tid = scope.token_id.clone();
            if let Some(deny) = review_denied(method, path) {
                return (tid, forbidden(deny));
            }
            admit(&mut req, cfg, scope, true);
            return (tid, next.run(req).await);
        }
    } else if let Some(s) = &sid
        && let Ok(Some(tid)) = cfg.store.resolve_session(s)
    {
        if tid == ADMIN_TOKEN_ID {
            // 超管会话 cookie：可进管理面与业务面（admin 不占配额不隔离）
            admit(
                &mut req,
                cfg,
                RequestScope {
                    token_id: None,
                    is_admin: true,
                },
                true,
            );
            return (None, next.run(req).await);
        }
        if path.starts_with("/admin") {
            return (Some(tid), axum::http::StatusCode::FORBIDDEN.into_response());
        }
        if let Some(deny) = review_denied(method, path) {
            return (Some(tid.clone()), forbidden(deny));
        }
        admit(
            &mut req,
            cfg,
            RequestScope {
                token_id: Some(tid.clone()),
                is_admin: false,
            },
            true,
        );
        return (Some(tid), next.run(req).await);
    }

    // 无有效凭证：免注册登录入口与门禁页探测匿名放行（whoami 回 authenticated=false），
    // 首屏初始化与邀请码激活也是匿名入口（内部分别有密钥/邀请码把关），其余端点一律 401。
    if path == "/auth/session"
        || path == "/auth/whoami"
        || path == "/auth/admin/init"
        || path == "/auth/invite/activate"
    {
        admit(
            &mut req,
            cfg,
            RequestScope {
                token_id: None,
                is_admin: false,
            },
            false,
        );
        return (None, next.run(req).await);
    }

    (None, axum::http::StatusCode::UNAUTHORIZED.into_response())
}

/// 评审身份的可达面（网关形态，**fail-closed 白名单**）。
///
/// 为什么是白名单而不是逐端点加守卫：参赛密钥是**共享**身份，全体评委共用一枚，
/// 一次误触就是全体评委的现场。11-12 走查实测评审密钥可直接打通 `POST
/// /gec/l3/retrain`（重训模型）、`POST /gec/agent/run`（带自主档写工具的 agent
/// 跑批）、`POST /gec/eval/run`（长跑占满 LLM 网关）、`POST /gec/llm-feedback`
/// （`adopt` 分支 `adopt_as_rule` 直接改写共享规则库）、`POST /gec/ops/unlock`
/// （未设 `GEC_TABLES_UPDATE_TOKEN` 时直接回 `unlocked:true`，等于把「平台运营」
/// 面连同规则管理 / 进化中心一起交给评委）。逐端点补漏已经证明会漏，所以这里
/// 一次性把评测通道面钉死，其余一律 403 + 说明。
///
/// 放行的是「检测 + 本人历史 + 只读说明」，**不放开改写共享状态**——与
/// `routes/mcp.rs` 把写档工具改成提案信封同一个立场（写不隔离共享状态）：
/// - `/auth/*`：门禁页与会话（whoami / session / logout）；
/// - `POST /gec/detect`、`POST /gec/:lang/detect`、`POST /gec/analyze`、
///   `POST /gec/recheck`、`POST /gec/explain`(`/stream`)：检测、句级定向复查与
///   讲解本体，按字符数计配额（D2）；
/// - `/gec/history*`、`/gec/sessions*`：本人历史与练习会话，跨账号可见性由
///   `Tenant` 归属守卫负责（这里不再判归属）；
/// - `GET /gec/settings`、`GET /gec/languages`：只读的配置与语言列表；
/// - `GET /gec/eval/layers`：各层贡献度报告（只读质量透明面：直读报告档案，
///   评委可核对「质量来自哪一层」；评测**发起**仍属运营面）；
/// - `/mcp*`：工具面自带分档——只读工具直跑，写档工具回提案信封。
///
/// 超管与单机形态不经过这里（admin 恒直通，单机零变化）。
fn review_denied(method: &axum::http::Method, path: &str) -> Option<&'static str> {
    const MSG: &str = "评测通道仅开放检测与本人历史；改写共享规则库 / 模型训练 / 长跑评测属产品运营面，需运营凭证";
    if path.starts_with("/auth/") || path == "/mcp" || path.starts_with("/mcp/") {
        return None;
    }
    if path.starts_with("/gec/history") || path.starts_with("/gec/sessions") {
        return None;
    }
    let m = method.as_str();
    // POST /gec/<lang>/detect（语言在路径里，见 routes/mod.rs 的 /gec/:lang/detect）
    if m == "POST"
        && path
            .strip_prefix("/gec/")
            .is_some_and(|rest| rest.ends_with("/detect") && rest.matches('/').count() == 1)
    {
        return None;
    }
    if m == "GET" && matches!(path, "/gec/settings" | "/gec/languages") {
        return None;
    }
    // 各层贡献度报告（只读质量透明面：直读报告档案，不落库、不改状态）
    if m == "GET" && path == "/gec/eval/layers" {
        return None;
    }
    if m == "POST"
        && matches!(
            path,
            "/gec/detect"
                | "/gec/analyze"
                | "/gec/recheck"
                | "/gec/explain"
                | "/gec/explain/stream"
        )
    {
        return None;
    }
    Some(MSG)
}

/// 与 `ApiError::Forbidden` 同形状的 403 体（前端 `errMsg` 直接取 `error`）。
fn forbidden(msg: &str) -> Response {
    (
        axum::http::StatusCode::FORBIDDEN,
        axum::Json(serde_json::json!({ "error": msg })),
    )
        .into_response()
}

/// 库嵌入 / 测试装配用：不挂鉴权门也注入单机形态的直通 `Tenant`。
///
/// `routes::router` 的业务 handler 以 `Extension<Tenant>` 取归属守卫，而生产入口
/// （`main.rs`）恒挂 `gate`，两种形态都会注入该扩展。直接组装 router 的调用方
/// 缺这个扩展会得到 500（`Extension` 缺失）——语义等价于单机直通，所以在这里
/// 一行补齐，别在每个装配点各写一遍（见 `Tenant::standalone`）。
pub fn with_standalone_tenant(app: axum::Router) -> axum::Router {
    let cfg = std::sync::Arc::new(crate::auth::GatewayConfig::standalone());
    app.layer(axum::middleware::from_fn(
        move |mut req: Request, next: Next| {
            let cfg = cfg.clone();
            async move {
                req.extensions_mut().insert(cfg);
                req.extensions_mut()
                    .insert(crate::auth::Tenant::standalone());
                req.extensions_mut().insert(crate::auth::AuthContext {
                    gateway: false,
                    admin: true,
                    authed: false,
                });
                next.run(req).await
            }
        },
    ))
}

/// 放行：注入身份双扩展与身份上下文（admin / 单机形态的 Tenant.token_id 恒 None = 不隔离不计量）。
/// 单机登录门的公开面：/auth/* 探测与登录本身必须永远可达；
/// /gec/deps 体检只有 GET 快照开放（依赖监督/重启保活探测）——
/// 同前缀下的 POST start/recheck 会 posix_spawn 拉起本机进程，必须过登录。
fn is_public_path(method: &axum::http::Method, path: &str) -> bool {
    path.starts_with("/auth/") || (*method == axum::http::Method::GET && path == "/gec/deps")
}

fn admit(req: &mut Request, cfg: &Arc<GatewayConfig>, scope: RequestScope, authed: bool) {
    req.extensions_mut().insert(AuthContext {
        gateway: cfg.enabled,
        admin: scope.is_admin,
        authed,
    });
    req.extensions_mut().insert(Tenant::from_scope(&scope, cfg));
    req.extensions_mut().insert(scope);
}

fn token_or_none(cfg: &Arc<GatewayConfig>, token: &str) -> Option<RequestScope> {
    cfg.store
        .verify_token(token)
        .ok()
        .flatten()
        .map(|info| RequestScope {
            token_id: Some(info.id),
            is_admin: false,
        })
}

fn session_cookie(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .map(str::trim)
        .find_map(|c| c.strip_prefix("lf_session="))
        .map(str::to_string)
}
