use http::{HeaderName, Method};
use tower_http::cors::{AllowOrigin, Any, CorsLayer};

/// CORS 层：允许 Web 前端跨域调用
///
/// 默认允许任意来源（本地开发便利）；生产部署建议设置
/// `CORS_ORIGINS`（逗号分隔的来源列表）收紧跨域范围。
pub fn cors_layer() -> CorsLayer {
    let layer = CorsLayer::new()
        .allow_methods([Method::GET, Method::POST, Method::PUT, Method::OPTIONS])
        .allow_headers([HeaderName::from_static("content-type")]);

    let origins: Vec<http::HeaderValue> = std::env::var("CORS_ORIGINS")
        .map(|v| v.split(',').filter_map(|o| o.trim().parse().ok()).collect())
        .unwrap_or_default();
    if origins.is_empty() {
        layer.allow_origin(Any)
    } else {
        layer.allow_origin(AllowOrigin::list(origins))
    }
}
