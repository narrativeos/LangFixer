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
        .is_err() {
            tracing::debug!("triage 附加信号已注册（跳过）");
        }
    }
    // 重启续跑：把上次进程遗留（created/running）的文档作业重新排队
    routes::doc_jobs::resume_interrupted_jobs(&state);
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
