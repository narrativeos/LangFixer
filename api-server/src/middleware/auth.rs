//! 网关鉴权门：单机形态直通；网关形态要求评审令牌（Bearer / 会话 cookie）
//! 或超管令牌（仅 Bearer，且仅 `/admin/*` 与全局放行二选一——admin 不占评审配额归户）。
//!
//! 放行的每个请求都会注入 `RequestScope`（原始身份）与 `Tenant`（业务守卫视图），
//! handler 侧统一用 `Extension<Tenant>` 取用（自定义提取器在 edition 2024 下与
//! axum-core 0.4 生命周期不兼容，见 `auth::Tenant::from_scope`）。

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::header;
use axum::middleware::Next;
use axum::response::{IntoResponse as _, Response};

use crate::auth::{GatewayConfig, RequestScope, Tenant};

pub async fn gate(State(cfg): State<Arc<GatewayConfig>>, mut req: Request, next: Next) -> Response {
    // 两种形态都把配置放进扩展，供需要直接取 store 的路由统一取用
    req.extensions_mut().insert(cfg.clone());
    // 单机形态：行为与接入鉴权前完全一致（Tenant 恒直通）
    if !cfg.enabled {
        admit(
            &mut req,
            &cfg,
            RequestScope {
                token_id: None,
                is_admin: true,
            },
        );
        return next.run(req).await;
    }

    let path = req.uri().path().to_string();
    // 免注册登录入口本身放行
    if path == "/auth/session" {
        return next.run(req).await;
    }

    let bearer = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_string);

    if let Some(token) = bearer {
        if cfg.admin.matches(&token) {
            admit(
                &mut req,
                &cfg,
                RequestScope {
                    token_id: None,
                    is_admin: true,
                },
            );
            return next.run(req).await;
        }
        if path.starts_with("/admin") {
            // 评审令牌不得进入管理面
            return axum::http::StatusCode::FORBIDDEN.into_response();
        }
        if let Some(scope) = token_or_none(&cfg, &token) {
            admit(&mut req, &cfg, scope);
            return next.run(req).await;
        }
    } else if !path.starts_with("/admin") {
        // cookie 会话仅走业务面；admin 永不发 cookie
        if let Some(sid) = session_cookie(req.headers()) {
            if let Ok(Some(token_id)) = cfg.store.resolve_session(&sid) {
                admit(
                    &mut req,
                    &cfg,
                    RequestScope {
                        token_id: Some(token_id),
                        is_admin: false,
                    },
                );
                return next.run(req).await;
            }
        }
    }

    axum::http::StatusCode::UNAUTHORIZED.into_response()
}

/// 放行：注入身份双扩展（admin / 单机形态的 Tenant.token_id 恒 None = 不隔离不计量）。
fn admit(req: &mut Request, cfg: &Arc<GatewayConfig>, scope: RequestScope) {
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
