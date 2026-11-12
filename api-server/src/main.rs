use std::sync::Arc;

use api_server::{middleware, routes, state::AppState};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(4801);
    // 决策表目录跟随运行时设置（设置文件 > 环境变量 GEC_TABLES_DIR > 内置默认）
    let state = Arc::new(AppState::from_env(None, None));
    // 网关形态（评测通道）：.env 提供开关与超管令牌，单机形态鉴权整体关闭（见 docs/GATEWAY_AUTH.md）
    let dotenv = api_server::auth::load_dotenv(std::path::Path::new(".env"));
    let gateway = Arc::new(api_server::auth::GatewayConfig::from_env(&dotenv));
    let host = std::env::var("LANGFIXER_BIND")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| dotenv.get("LANGFIXER_BIND").cloned())
        .unwrap_or_else(|| "0.0.0.0".to_string());
    if gateway.enabled && host == "0.0.0.0" {
        tracing::warn!(
            "网关形态仍监听 0.0.0.0：公网入口应只由反向代理持有，建议 LANGFIXER_BIND=127.0.0.1"
        );
    }
    let app = routes::router(state.clone())
        .merge(routes::auth::router(gateway.clone()))
        .merge(routes::admin::router(gateway.clone()))
        .layer(axum::middleware::from_fn_with_state(
            gateway.clone(),
            middleware::auth::gate,
        ))
        .layer(middleware::cors::cors_layer());

    let listener = tokio::net::TcpListener::bind((host.as_str(), port))
        .await
        .expect("failed to bind");
    tracing::info!(
        port,
        gateway = gateway.enabled,
        tables = %state.settings.read().unwrap().tables_dir,
        "LangFixer API 服务已启动"
    );
    axum::serve(listener, app).await.expect("server error");
}
