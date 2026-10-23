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
    let app = routes::router(state.clone()).layer(middleware::cors::cors_layer());

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port))
        .await
        .expect("failed to bind");
    tracing::info!(
        port,
        tables = %state.settings.read().unwrap().tables_dir,
        "LangFixer API 服务已启动"
    );
    axum::serve(listener, app).await.expect("server error");
}
