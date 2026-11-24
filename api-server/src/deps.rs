//! 依赖服务监督器（11-19）：启动自检 + 主动拉起本机层服务 + 状态面
//!
//! 用户口径：**启动时自动检查依赖资源，在资源允许的情况下主动拉起这几个服务；
//! 拉不起来的不能静默放弃，要给出原因并询问是否有其他资源可以部署。**
//!
//! - 本机可管服务（`decision-model` / `feature-model` / 字级判别侧车）：loopback 地址 +
//!   仓库内 venv 存在 + 资源门槛通过时自动 `spawn`，轮询 `/health` 确认；
//! - 远程服务（HanLP / LLM 网关 / 嵌入 / 统计 GEC 深度纠错臂）：只探测不拉起，
//!   失败给出「换其他机器/地址」提示；
//! - 所有状态沉淀在 `AppState::deps`（`GET /gec/deps` 读取），运营台「依赖服务」卡片
//!   展示原因与补救动作（拉起 / 改地址后重测）；
//! - [`keepalive_loop`]：启动监督一次性，周期保活让已拉起服务的事后死亡 /
//!   已恢复服务在分钟级内被状态面感知，不再停留在过期的 ok。

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::process::Command;

use crate::settings::AppSettings;
use crate::state::AppState;

/// 监督对象（id, 展示名）；顺序即 UI 展示顺序
pub const DEPS: &[(&str, &str)] = &[
    ("decision", "决策模型服务"),
    ("l3", "快速定案服务（特征模型）"),
    ("char_gate", "字级判别侧车（可选）"),
    ("stat", "深度纠错服务（统计 GEC 臂）"),
    ("hanlp", "文本处理服务（HanLP）"),
    ("llm", "AI 复查网关（LLM）"),
    ("embedding", "向量检索服务"),
];

/// 单项状态：pending（未探测）| ok | starting | failed | skipped（可选服务资源不足跳过）| off（设置关闭/未配置）
#[derive(Debug, Clone, Serialize)]
pub struct DepStatus {
    pub id: &'static str,
    pub label: &'static str,
    pub endpoint: String,
    pub state: &'static str,
    /// 本机可拉起（loopback 地址 + 仓库内服务目录与 venv 存在）
    pub managed: bool,
    /// 由监督器拉起（区别于外部早已在跑）
    pub auto: bool,
    pub reason: Option<String>,
    /// 拉不起来时的补救指引（是否有其他机器/资源可部署、去哪儿改地址）
    pub hint: Option<String>,
    pub latency_ms: u64,
}

/// 状态快照（`GET /gec/deps` 响应体）
#[derive(Debug, Clone, Serialize)]
pub struct DepRegistry {
    pub updated_at: String,
    pub items: Vec<DepStatus>,
}

impl Default for DepRegistry {
    fn default() -> Self {
        Self {
            updated_at: String::new(),
            items: DEPS
                .iter()
                .map(|(id, label)| DepStatus {
                    id,
                    label,
                    endpoint: String::new(),
                    state: "pending",
                    managed: false,
                    auto: false,
                    reason: None,
                    hint: None,
                    latency_ms: 0,
                })
                .collect(),
        }
    }
}

/// 更新单项状态并刷新快照时间戳
pub fn update(state: &AppState, id: &str, f: impl FnOnce(&mut DepStatus)) {
    let mut reg = state.deps.write().unwrap();
    reg.updated_at = chrono::Utc::now().to_rfc3339();
    if let Some(it) = reg.items.iter_mut().find(|i| i.id == id) {
        f(it);
    }
}

/// 读取快照
pub fn snapshot(state: &AppState) -> DepRegistry {
    state.deps.read().unwrap().clone()
}

fn find_endpoint(s: &AppSettings, id: &str) -> String {
    match id {
        "decision" => s.decision.endpoint.clone(),
        "l3" => s.l3.endpoint.clone(),
        "char_gate" => char_gate_endpoint(s),
        "stat" => stat_endpoint(s),
        "hanlp" => s.hanlp_endpoint.clone(),
        "llm" => s.llm.base_url.clone(),
        "embedding" => s.embedding.endpoint.clone(),
        _ => String::new(),
    }
}

/// 字级判别侧车地址（settings 未建模；env 可覆盖，默认 4811）
fn char_gate_endpoint(_s: &AppSettings) -> String {
    std::env::var("GEC_CHAR_GATE_ENDPOINT").unwrap_or_else(|_| "http://127.0.0.1:4811".to_string())
}

/// 统计 GEC 臂地址（单一来源：gec-core StatisticalConfig，默认 4803）
fn stat_endpoint(_s: &AppSettings) -> String {
    gec_core::statistical::StatisticalConfig::from_env().endpoint
}

/// 统计臂「不参与检测」的判定（off 短路，不探测不报红）：
/// 未配地址；或配了地址但 GEC_ENGINE_{ZH,EN,JA} 无一设为 statistical（与
/// gec-core GecEngineKind::from_env 的组合语义对齐）
fn stat_off_reason_for(endpoint_env: &str, engine_envs: [Option<String>; 3]) -> Option<String> {
    if endpoint_env.trim().is_empty() {
        return Some("未配置（GEC_STAT_ENDPOINT 未设），检测默认纯规则".to_string());
    }
    let mounted = engine_envs
        .iter()
        .flatten()
        .any(|v| v.eq_ignore_ascii_case("statistical"));
    if !mounted {
        return Some(
            "已配置地址，但 GEC_ENGINE_{ZH,EN,JA} 均未设为 statistical，该臂不参与检测".to_string(),
        );
    }
    None
}

fn stat_off_reason() -> Option<String> {
    stat_off_reason_for(
        &std::env::var("GEC_STAT_ENDPOINT").unwrap_or_default(),
        ["ZH", "EN", "JA"].map(|l| std::env::var(format!("GEC_ENGINE_{l}")).ok()),
    )
}

/// 从 endpoint 提取 loopback 主机与端口；非 loopback 返回 None（远程服务不拉起）
fn loopback_port(endpoint: &str) -> Option<u16> {
    let rest = endpoint
        .split_once("://")
        .map(|(_, r)| r)
        .unwrap_or(endpoint);
    let hostport = rest.split('/').next().unwrap_or("");
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().ok()),
        None => (hostport, None),
    };
    let host = host.trim_matches(|c| c == '[' || c == ']');
    matches!(host, "127.0.0.1" | "localhost" | "::1").then_some(port?)
}

/// 可用内存（MB）。Linux 读 MemAvailable；macOS 由 vm_stat 推 free+inactive+speculative；
/// 取不到返回 None（轻服务乐观放行、重服务跳过）
async fn avail_mem_mb() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let txt = tokio::fs::read_to_string("/proc/meminfo").await.ok()?;
        for line in txt.lines() {
            if let Some(rest) = line.strip_prefix("MemAvailable:") {
                return rest
                    .trim()
                    .trim_end_matches(" kB")
                    .trim()
                    .parse::<u64>()
                    .ok()
                    .map(|kb| kb / 1024);
            }
        }
        None
    }
    #[cfg(target_os = "macos")]
    {
        let out = Command::new("vm_stat").output().await.ok()?;
        if !out.status.success() {
            return None;
        }
        let txt = String::from_utf8_lossy(&out.stdout).to_string();
        let page = regex_page_size(&txt)?;
        let pages = |name: &str| -> Option<u64> {
            txt.lines()
                .find(|l| l.starts_with(name))
                .and_then(|l| l.rsplit(':').next())
                .and_then(|v| v.trim().trim_end_matches('.').parse::<u64>().ok())
        };
        let free = pages("Pages free:")?
            + pages("Pages inactive:")?
            + pages("Pages speculative:").unwrap_or(0);
        Some(free * page / 1024 / 1024)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

#[cfg(target_os = "macos")]
fn regex_page_size(vm_stat: &str) -> Option<u64> {
    // 头部：Mach Virtual Memory Statistics: (page size of 16384 bytes)
    let i = vm_stat.find("page size of ")? + "page size of ".len();
    vm_stat[i..].split_whitespace().next()?.parse::<u64>().ok()
}

/// 本机可拉起判定：目录 + server 脚本 + venv python 存在
struct LocalSpec {
    dir: &'static str,
    script: &'static str,
    venv: &'static str,
    py_names: &'static [&'static str],
    port_env: &'static str,
    mem_floor_mb: u64,
    log_name: &'static str,
}

fn local_spec(id: &str) -> Option<LocalSpec> {
    // 门槛值：decision/l3 为 KB 级 sklearn 模型服务（常驻 ~150–300MB）；
    // char_gate 侧车为 390MB torch CTC 模型（~1.5GB）。取不到可用内存时
    // 轻服务乐观放行（占用小），重服务必须确认达标。
    match id {
        "decision" => Some(LocalSpec {
            dir: "decision-model",
            script: "server.py",
            venv: "decision-model/.venv",
            py_names: &["python3", "python"],
            port_env: "DEC_PORT",
            mem_floor_mb: 300,
            log_name: "decision.log",
        }),
        "l3" => Some(LocalSpec {
            dir: "feature-model",
            script: "server.py",
            venv: "feature-model/.venv",
            py_names: &["python3", "python"],
            port_env: "L3_PORT",
            mem_floor_mb: 300,
            log_name: "l3.log",
        }),
        "char_gate" => Some(LocalSpec {
            dir: "feature-model",
            script: "char_gate.py",
            venv: "feature-model/.venv_char",
            py_names: &["python3", "python"],
            port_env: "",
            mem_floor_mb: 2048,
            log_name: "char_gate.log",
        }),
        _ => None,
    }
}

impl LocalSpec {
    /// 解析可用的 venv 解释器路径（不存在返回 None）
    fn interpreter(&self) -> Option<PathBuf> {
        // 用「绝对但不解析符号链」的路径：
        // - 相对路径会在 posix_spawn 切换 current_dir 后才被解析，拼错位置（ENOENT）；
        // - canonicalize 会解析 python→homebrew 的符号链，破坏 venv 探测（pyvenv.cfg 在可执行文件旁），
        //   导致子进程丢 site-packages（ModuleNotFoundError）。
        let root = match std::path::absolute(Path::new(self.venv)) {
            Ok(p) => p,
            Err(_) => return None,
        };
        if !root.join("bin").is_dir() {
            return None;
        }
        self.py_names
            .iter()
            .map(|n| root.join("bin").join(n))
            .find(|p| p.is_file())
    }
}

async fn probe_http(url: &str) -> bool {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap_or_default();
    match client.get(url).send().await {
        Ok(r) => r.status().is_success(),
        Err(_) => false,
    }
}

/// 探测单项（只读，不拉起）；返回 (ok, message, latency_ms)
async fn probe(_state: &AppState, id: &str, s: &AppSettings) -> (bool, String, u64) {
    use gec_core::settings_test as st;
    let t0 = Instant::now();
    let res = match id {
        "decision" => st::test_decision(&s.decision).await,
        "l3" => st::test_l3(&s.l3).await,
        "hanlp" => st::test_hanlp(&s.hanlp_endpoint).await,
        "llm" => {
            st::test_llm(
                &s.llm.base_url,
                &s.llm.api_key,
                &s.llm.model,
                s.llm.timeout_secs,
            )
            .await
        }
        "embedding" => st::test_embedding(&s.embedding).await,
        "char_gate" => {
            let url = format!("{}/health", char_gate_endpoint(s).trim_end_matches('/'));
            if probe_http(&url).await {
                gec_core::settings_test::TestResult {
                    target: id.to_string(),
                    ok: true,
                    message: "侧车在线".to_string(),
                    latency_ms: 0,
                    skipped: false,
                }
            } else {
                gec_core::settings_test::TestResult {
                    target: id.to_string(),
                    ok: false,
                    message: format!("{url} 无响应"),
                    latency_ms: 0,
                    skipped: false,
                }
            }
        }
        "stat" => {
            // 先探 /health（csc_server 等自带）；未实现 /health 的通用统计服务
            // 退到 POST /gec 探测句验连通（与 statistical.rs 消费面同构）
            let base = stat_endpoint(s).trim_end_matches('/').to_string();
            if probe_http(&format!("{base}/health")).await {
                gec_core::settings_test::TestResult {
                    target: id.to_string(),
                    ok: true,
                    message: "深度纠错服务在线（/health）".to_string(),
                    latency_ms: 0,
                    skipped: false,
                }
            } else {
                let client = reqwest::Client::builder()
                    .timeout(Duration::from_secs(5))
                    .build()
                    .unwrap_or_default();
                match client
                    .post(format!("{base}/gec"))
                    .json(&serde_json::json!({ "text": "测试。", "lang": "zh" }))
                    .send()
                    .await
                {
                    Ok(r) if r.status().is_success() => gec_core::settings_test::TestResult {
                        target: id.to_string(),
                        ok: true,
                        message: "深度纠错服务在线（POST /gec 探测句）".to_string(),
                        latency_ms: 0,
                        skipped: false,
                    },
                    Ok(r) => gec_core::settings_test::TestResult {
                        target: id.to_string(),
                        ok: false,
                        message: format!("{base}/gec 探测句返回 {}", r.status()),
                        latency_ms: 0,
                        skipped: false,
                    },
                    Err(e) => gec_core::settings_test::TestResult {
                        target: id.to_string(),
                        ok: false,
                        message: format!("{base} 无响应：{e}"),
                        latency_ms: 0,
                        skipped: false,
                    },
                }
            }
        }
        _ => return (false, "未知依赖".to_string(), 0),
    };
    (res.ok, res.message, t0.elapsed().as_millis() as u64)
}

/// ok 态下的 reason：决策模型把探测结果（含门控判据 / 旧版进程提示）原样上浮，
/// 让门控口径随状态条与依赖卡可见（审计 F7：判据不出盲盒）；其余项 ok 态照旧静默。
fn ok_reason(id: &str, msg: &str) -> Option<String> {
    (id == "decision" && !msg.is_empty()).then(|| msg.to_string())
}

/// 拉起子进程（stdout/stderr 汇入 log/supervisor/<name>；进程不随 api-server 退出被杀）
async fn spawn_logged(
    spec: &LocalSpec,
    py: &Path,
    args: &[&str],
    envs: &[(&str, String)],
    log: &Path,
) -> std::io::Result<u32> {
    if let Some(parent) = log.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let out = OpenOptions::new().create(true).append(true).open(log)?;
    let err = OpenOptions::new().create(true).append(true).open(log)?;
    let mut cmd = Command::new(py);
    cmd.current_dir(spec.dir)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err));
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let child = cmd.spawn()?;
    let pid = child.id().unwrap_or(0);
    drop(child); // kill_on_drop=false：日志子进程独立存活（下次启动探测即见健康）
    Ok(pid)
}

async fn wait_healthy(url: &str, budget_secs: u64) -> bool {
    let t0 = Instant::now();
    loop {
        if probe_http(url).await {
            return true;
        }
        if t0.elapsed() >= Duration::from_secs(budget_secs) {
            return false;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// 公共入口：持锁拉起单个依赖（路由手动拉起 / 启动监督共用，串行防重复 spawn）
pub async fn bring_up_locked(state: &Arc<AppState>, id: &str) -> DepStatus {
    let _g = state.deps_lock.lock().await;
    bring_up_local(state, id).await
}

/// 拉起一个本机可管服务（决策/定案/侧车）。返回更新后的状态。
/// 对 char_gate 有单向递归（l3 → char_gate），故手工装箱打破 async 递归限制。
fn bring_up_local<'a>(
    state: &'a Arc<AppState>,
    id: &'a str,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = DepStatus> + Send + 'a>> {
    Box::pin(async move {
        let Some(spec) = local_spec(id) else {
            let item = snapshot(state).items.into_iter().find(|i| i.id == id);
            return item.unwrap();
        };
        let s = state.settings.read().unwrap().clone();
        let endpoint = find_endpoint(&s, id);
        let Some(port) = loopback_port(&endpoint) else {
            // 地址非本机：不是我们能拉起的对象，交由远程失败路径
            let (ok, msg, lat) = probe(state, id, &s).await;
            let (state_v, reason, hint) = if ok {
                ("ok", ok_reason(id, &msg), None)
            } else {
                (
                "failed",
                Some(format!("探测失败：{msg}")),
                Some("该地址不是本机服务。若有其他机器已部署/可部署该服务，请在设置里把它的地址填上并保存；若从未部署，请确认是否有可用的部署资源。".to_string()),
            )
            };
            update(state, id, |it| {
                it.endpoint = endpoint.clone();
                it.state = state_v;
                it.managed = false;
                it.reason = reason;
                it.hint = hint;
                it.latency_ms = lat;
            });
            return snapshot(state)
                .items
                .into_iter()
                .find(|i| i.id == id)
                .unwrap();
        };

        // 已健康则直接 ok（幂等：重复启动前先探测）
        let (ok, msg, lat) = probe(state, id, &s).await;
        if ok {
            update(state, id, |it| {
                it.endpoint = endpoint.clone();
                it.state = "ok";
                it.managed = true;
                it.reason = ok_reason(id, &msg);
                it.hint = None;
                it.latency_ms = lat;
            });
            return snapshot(state)
                .items
                .into_iter()
                .find(|i| i.id == id)
                .unwrap();
        }

        let py = match spec.interpreter() {
            Some(p) => p,
            None => {
                update(state, id, |it| {
                    it.endpoint = endpoint.clone();
                    it.state = "failed";
                    it.managed = false;
                    it.reason = Some(format!(
                        "本机无法拉起：{}/ 缺 venv 或 {}",
                        spec.venv, spec.script
                    ));
                    it.hint = Some("本仓库缺少该服务的运行环境。是否有其他机器可以部署该服务？部署后把地址填入设置（decision.endpoint / l3.endpoint）并保存重试。".to_string());
                    it.latency_ms = lat;
                });
                return snapshot(state)
                    .items
                    .into_iter()
                    .find(|i| i.id == id)
                    .unwrap();
            }
        };

        let avail = avail_mem_mb().await;
        if let Some(a) = avail {
            if a < spec.mem_floor_mb {
                update(state, id, |it| {
                    it.endpoint = endpoint.clone();
                    it.state = if id == "char_gate" {
                        "skipped"
                    } else {
                        "failed"
                    };
                    it.managed = true;
                    it.reason = Some(format!(
                        "资源检查未通过：可用内存约 {a} MB < 门槛 {} MB，暂不拉起",
                        spec.mem_floor_mb
                    ));
                    it.hint = Some("内存紧张：可释放其他进程后点「拉起服务」重试；若有内存更充裕的机器，可把该服务部署到那台机器并在设置里改地址。".to_string());
                    it.latency_ms = lat;
                });
                return snapshot(state)
                    .items
                    .into_iter()
                    .find(|i| i.id == id)
                    .unwrap();
            }
        } else if id == "char_gate" {
            // 重服务在内存未知时保守跳过
            update(state, id, |it| {
                it.endpoint = endpoint.clone();
                it.state = "skipped";
                it.managed = true;
                it.reason =
                    Some("资源检查不可用（无法读取可用内存），可选侧车保守跳过".to_string());
                it.hint = Some(
                    "L3 将以启发式回退运行；如需侧车，可在资源确认后点「拉起服务」。".to_string(),
                );
                it.latency_ms = lat;
            });
            return snapshot(state)
                .items
                .into_iter()
                .find(|i| i.id == id)
                .unwrap();
        }

        update(state, id, |it| {
            it.endpoint = endpoint.clone();
            it.state = "starting";
            it.managed = true;
            it.reason = None;
            it.hint = None;
            it.latency_ms = 0;
        });

        let mut envs: Vec<(&str, String)> = Vec::new();
        if !spec.port_env.is_empty() {
            envs.push((spec.port_env, port.to_string()));
        }
        if id == "l3" {
            // L3 先确保侧车（可选）：已健康则挂上；尝试拉起失败则启发式回退继续
            let gate = char_gate_endpoint(&s);
            let gate_url = format!("{}/health", gate.trim_end_matches('/'));
            let gate_ready = if probe_http(&gate_url).await {
                true
            } else {
                bring_up_local(state, "char_gate").await;
                probe_http(&gate_url).await
            };
            if gate_ready {
                envs.push((
                    "L3_CHAR_GATE_URL",
                    format!("{}/gate", gate.trim_end_matches('/')),
                ));
            }
        }
        let argv_owned: Vec<String> = if id == "char_gate" {
            vec![
                spec.script.to_string(),
                "--port".to_string(),
                port.to_string(),
            ]
        } else {
            vec![spec.script.to_string()]
        };
        let argv: Vec<&str> = argv_owned.iter().map(|s| s.as_str()).collect();

        let log = PathBuf::from("log/supervisor").join(spec.log_name);
        match spawn_logged(&spec, &py, &argv, &envs, &log).await {
            Ok(pid) => {
                tracing::info!(id, pid, log = %log.display(), "依赖监督：已拉起服务");
                let health_url = match id {
                    "char_gate" => format!("http://127.0.0.1:{port}/health"),
                    _ => format!("{}/health", endpoint.trim_end_matches('/')),
                };
                let budget = match id {
                    "char_gate" => 150, // torch 390MB 模型加载慢
                    _ => 90,
                };
                let ready = wait_healthy(&health_url, budget).await;
                let (ok, msg, _lat) = probe(state, id, &s).await;
                let _ = msg;
                update(state, id, |it| {
                    it.state = if ready || ok { "ok" } else { "failed" };
                    it.auto = ready || ok;
                    if !(ready || ok) {
                        it.reason = Some(format!(
                            "已拉起（pid {pid}）但 {budget}s 内未就绪，详见 {}",
                            log.display()
                        ));
                        it.hint = Some("可再点「拉起服务」重试；若有其他机器可部署该服务，请在设置中改地址（decision.endpoint / l3.endpoint）后保存重试。".to_string());
                    }
                });
                tracing::info!(id, ok, "依赖监督：服务状态确认");
            }
            Err(e) => {
                update(state, id, |it| {
                    it.state = "failed";
                    it.reason = Some(format!("拉起失败：{e}"));
                    it.hint = Some("是否有其他机器可部署该服务？部署后在设置中改地址（decision.endpoint / l3.endpoint）保存重试。".to_string());
                });
            }
        }
        snapshot(state)
            .items
            .into_iter()
            .find(|i| i.id == id)
            .unwrap()
    })
}

/// 只探测（不拉起），刷新快照
pub async fn recheck(state: &Arc<AppState>, id: &str) {
    let s = state.settings.read().unwrap().clone();
    let endpoint = find_endpoint(&s, id);
    // 统计臂未挂载时 off 短路（纯 env 判定，不探测）
    if id == "stat"
        && let Some(reason) = stat_off_reason()
    {
        update(state, id, |it| {
            it.endpoint = endpoint;
            it.state = "off";
            it.reason = Some(reason);
            it.hint = None;
        });
        return;
    }
    // off 短路
    match id {
        "decision" if !s.decision.enabled => {
            update(state, id, |it| {
                it.endpoint = endpoint;
                it.state = "off";
                it.reason = Some("该层未启用（decision.enabled=false）".to_string());
                it.hint = None;
            });
            return;
        }
        "l3" if !s.l3.enabled => {
            update(state, id, |it| {
                it.endpoint = endpoint;
                it.state = "off";
                it.reason = Some("该层未启用（l3.enabled=false）".to_string());
                it.hint = None;
            });
            return;
        }
        "hanlp" if s.offline_mode => {
            update(state, id, |it| {
                it.endpoint = endpoint;
                it.state = "off";
                it.reason = Some("离线模式（offline_mode=true），本项按需跳过".to_string());
                it.hint = None;
            });
            return;
        }
        "embedding" if s.embedding.endpoint.is_empty() => {
            update(state, id, |it| {
                it.state = "off";
                it.reason = Some("未配置（embedding.endpoint 为空）".to_string());
                it.hint = None;
            });
            return;
        }
        "llm" if s.llm.base_url.is_empty() => {
            update(state, id, |it| {
                it.state = "off";
                it.reason = Some("未配置（llm.base_url 为空）".to_string());
                it.hint = None;
            });
            return;
        }
        _ => {}
    }
    let (ok, msg, lat) = probe(state, id, &s).await;
    let known = find_endpoint(&s, id);
    update(state, id, |it| {
        it.endpoint = known;
        it.latency_ms = lat;
        if ok {
            it.state = "ok";
            it.reason = ok_reason(id, &msg);
            it.hint = None;
        } else {
            it.state = "failed";
            it.reason = Some(format!("探测失败：{msg}"));
            if local_spec(id).is_some() {
                it.hint = Some("点「拉起服务」由本机尝试启动；若无本机环境，可在其他机器部署该服务并把地址填入设置。".to_string());
            } else {
                it.hint = Some("这是外部服务：确认其是否运行/可达；若有其他机器或替代地址，请在对应设置卡片里修改地址并保存。".to_string());
            }
        }
    });
}

/// 启动监督：依次确认全部依赖（后台运行，不阻塞端口监听）
pub async fn supervise_init(state: Arc<AppState>) {
    for (id, _) in DEPS {
        match *id {
            "decision" | "l3" | "char_gate" => {
                let _ = bring_up_locked(&state, id).await;
            }
            _ => recheck(&state, id).await,
        }
    }
    let reg = snapshot(&state);
    let bad: Vec<&str> = reg
        .items
        .iter()
        .filter(|i| i.state == "failed")
        .map(|i| i.label)
        .collect();
    if bad.is_empty() {
        tracing::info!("依赖监督：全部依赖就绪");
    } else {
        // 不静默放弃：启动期失败必须可见（日志 + 状态面 + 前端提示）
        tracing::warn!(
            deps = bad.join("、"),
            "依赖监督：部分依赖不可用，请见 GET /gec/deps 与运营台「依赖服务」卡片"
        );
    }
}

/// 保活重探间隔（秒）：启动监督只跑一次，本循环把「拉起的服务事后死亡」
/// 与「failed 的服务事后恢复」压到分钟级内被状态面感知
const KEEPALIVE_SECS: u64 = 60;

/// 周期保活：低频全量重探（只探测不拉起，避免重启风暴；拉起仍走启动监督 /
/// 手动按钮）。例外：skipped（资源门槛下的可选侧车）不自动升级；off 短路零成本；
/// LLM 探测含真实模型往返，已 ok 时不再重复，避免每轮的 token 成本
pub async fn keepalive_loop(state: Arc<AppState>) {
    loop {
        tokio::time::sleep(Duration::from_secs(KEEPALIVE_SECS)).await;
        let reg = snapshot(&state);
        for (id, _) in DEPS {
            let cur = reg
                .items
                .iter()
                .find(|i| i.id == *id)
                .map(|i| i.state)
                .unwrap_or("pending");
            if cur == "skipped" || (*id == "llm" && cur == "ok") {
                continue;
            }
            let before = cur;
            recheck(&state, id).await;
            let after = snapshot(&state)
                .items
                .iter()
                .find(|i| i.id == *id)
                .map(|i| i.state)
                .unwrap_or("pending");
            // 状态翻转才记日志，避免周期噪音
            if before != after {
                tracing::warn!(
                    id,
                    from = before,
                    to = after,
                    "依赖保活：状态翻转（周期重探）"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::stat_off_reason_for;

    #[test]
    fn stat_off_when_endpoint_unset() {
        let r = stat_off_reason_for("", [None, None, None]);
        assert!(r.unwrap().contains("GEC_STAT_ENDPOINT"));
    }

    #[test]
    fn stat_off_when_configured_but_not_mounted() {
        let r = stat_off_reason_for("http://127.0.0.1:4807", [Some("rule".into()), None, None]);
        assert!(r.unwrap().contains("GEC_ENGINE"));
    }

    #[test]
    fn stat_on_when_any_lang_mounted() {
        assert_eq!(
            stat_off_reason_for(
                "http://127.0.0.1:4807",
                [None, Some("Statistical".into()), None]
            ),
            None
        );
    }
}
