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
    // 每日备份对账：设置开着就幂等重注册 launchd 任务（plist 被人删 / DB 路径变更后自愈）；
    // 默认关，开发环境零副作用；失败只告警不阻断启动（GET /gec/settings 会如实显示未安装态）
    if state.settings.read().unwrap().db_backup_enabled {
        let db_path = state.db_path.clone();
        tokio::task::spawn_blocking(move || {
            if let Err(e) = api_server::db_backup::install(&db_path) {
                tracing::warn!("每日备份任务对账失败: {e}");
            }
        });
    }
    // 依赖监督（启动自检 + 资源允许时主动拉起本机层服务；拉不起来不静默，见 GET /gec/deps）
    {
        let st = state.clone();
        tokio::spawn(async move { api_server::deps::supervise_init(st).await });
    }
    // 依赖保活：启动监督只跑一次，周期重探把拉起服务的事后死亡 / failed 的事后恢复
    // 压到分钟级内被状态面感知（只探测不拉起，见 deps::keepalive_loop）
    {
        let st = state.clone();
        tokio::spawn(async move { api_server::deps::keepalive_loop(st).await });
    }
    // 网关形态（评测通道）：.env 提供开关与超管令牌，单机形态鉴权整体关闭（见 docs/GATEWAY_AUTH.md）
    let dotenv = api_server::auth::load_dotenv(std::path::Path::new(".env"));
    let gateway = Arc::new(api_server::auth::GatewayConfig::from_env(&dotenv));
    let host = std::env::var("LANGFIXER_BIND")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| dotenv.get("LANGFIXER_BIND").cloned())
        // 默认只绑回环：登录门/口令在 LAN 上裸跑不如不绑；确实要对外用
        // LANGFIXER_BIND=0.0.0.0 显式声明，下方对危险组合照旧 WARN。
        .unwrap_or_else(|| "127.0.0.1".to_string());
    if gateway.enabled && host == "0.0.0.0" {
        tracing::warn!(
            "网关形态仍监听 0.0.0.0：公网入口应只由反向代理持有，建议 LANGFIXER_BIND=127.0.0.1"
        );
    } else if !gateway.enabled && gateway.login_gate && host == "0.0.0.0" {
        tracing::warn!(
            "登录门形态监听 0.0.0.0：登录入口直接暴露给局域网/公网，建议 LANGFIXER_BIND=127.0.0.1 或置于反向代理之后"
        );
        if gateway
            .password
            .matches(api_server::auth::DEFAULT_ADMIN_PASSWORD)
        {
            tracing::warn!(
                "默认管理员密码仍生效且端口对外暴露：请立即改密（运营台改密表单或 POST /auth/password）"
            );
        }
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
    // ConnectInfo 供登录限流取对端 IP（反代部署下另有 XFF 头优先，见 routes/auth.rs::client_ip）
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
    .expect("server error");
}
