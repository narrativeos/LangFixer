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
    // triage 词典信号：把 gec-zh 本地规则命中密度注入块风险分。本地规则
    // 检出贡献低（MUCGEC 21/7258）但**定位可疑块**有效——economical 模式
    // 据此不把词典可见的疑似错误块跳过（eval 实测纯统计信号对语义级错误
    // 近乎全盲，见 datasets/eval_reports/doc_eval_latest.json）
    {
        let dir = std::path::PathBuf::from(state.settings.read().unwrap().lexicons_dir.clone());
        let lex = gec_zh::rules::Lexicon::load(&dir);
        if gec_core::triage::set_extra_signal(Arc::new(move |t: &str| {
            let hits = gec_zh::rules::detect_local(t, &lex).len();
            if hits == 0 {
                gec_core::ChunkRisk::default()
            } else {
                gec_core::ChunkRisk {
                    score: (0.4 + 0.1 * hits as f64).min(0.7),
                    signals: vec![format!("lexicon_hit:{hits}")],
                }
            }
        }))
        .is_err()
        {
            tracing::debug!("triage 附加信号已注册（跳过）");
        }
    }
    // 重启续跑：把上次进程遗留（created/running）的文档作业重新排队
    routes::doc_jobs::resume_interrupted_jobs(&state);
    // 依赖监督（启动自检 + 资源允许时主动拉起本机层服务；拉不起来不静默，见 GET /gec/deps）
    {
        let st = state.clone();
        tokio::spawn(async move { api_server::deps::supervise_init(st).await });
    }
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
        .merge(routes::mcp::router(state.clone()))
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
