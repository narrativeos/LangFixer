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

    // 单机形态：业务端点行为与接入鉴权前完全一致（Tenant 恒直通、不拒任何请求），
    // 只标记「这个请求带了有效凭证」。调用级日志是网关侧的运维资料，
    // 单机形态不落（零新增写盘，也避免把私人使用记成「调用」）。
    if !cfg.enabled {
        let mut authed = false;
        if let Some(token) = &bearer {
            authed |= cfg.admin.matches(token);
        }
        if let Some(sid) = &sid {
            authed |= matches!(cfg.store.resolve_session(sid), Ok(Some(_)));
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
    let started = std::time::Instant::now();
    let (log_token, resp) = decide(&cfg, req, next, bearer, sid, &path).await;
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
            admit(&mut req, cfg, scope, true);
            return (tid, next.run(req).await);
        }
    } else if let Some(s) = &sid {
        if let Ok(Some(tid)) = cfg.store.resolve_session(s) {
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
    }

    // 无有效凭证：免注册登录入口与门禁页探测匿名放行（whoami 回 authenticated=false），
    // 其余端点一律 401。
    if path == "/auth/session" || path == "/auth/whoami" {
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

/// 放行：注入身份双扩展与身份上下文（admin / 单机形态的 Tenant.token_id 恒 None = 不隔离不计量）。
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
