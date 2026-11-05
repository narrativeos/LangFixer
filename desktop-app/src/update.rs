//! 自动更新：安装包在构建时烧入「发布源」（`LANGFIXER_RELEASE_SOURCE`，由 build.rs 注入），
//! 运行时自动从该源检查更新，发现新版后下载并安装。用户不需要也无法选择源：
//! 从哪个源下载安装包，就从哪个源更新。
//!
//! 端点与 scripts/release.sh / scripts/make_latest_json.py 的上传路径一一对应；
//! 所有源共用同一把 minisign 签名密钥（公钥在 tauri.conf.json，私钥在
//! ~/.config/langfixer/updater.key，勿入库）。
//!
//! 平台差异（插件的 install() 在 Windows 上拉起安装器后会直接 `std::process::exit`
//! 结束本进程，macOS / Linux 只是在运行进程内替换磁盘文件）：
//! - macOS / Linux：自动下载 + 安装，装完提示重启生效；「已装好但未重启」的
//!   版本号持久化到配置目录 `update-pending.txt`，冷启动不再重复下载安装与弹框；
//! - Windows：必须先征得用户同意再下载并 install()（应用随之退出、由安装器接管）；
//!   用户选「稍后再说」则不落盘，下次启动仍会询问；同一会话内同一版本只问一次。

use std::path::PathBuf;
use std::time::Duration;

use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
use tauri_plugin_updater::UpdaterExt;

/// 打包时烧入的发布源：github | gitea | atomgit（未指定时默认 github）
pub fn release_source() -> &'static str {
    env!("LANGFIXER_RELEASE_SOURCE")
}

/// 该源对应的 latest.json 更新清单地址
pub fn endpoints() -> Vec<reqwest::Url> {
    let url = match release_source() {
        "gitea" => {
            "https://gitea.jiulu.ltd/narrativeos/LangFixer/releases/latest/download/latest.json"
        }
        // TODO: atomgit 的 release 资产下载 URL 格式待验证，不支持时改用 raw 文件托管
        "atomgit" => {
            "https://atomgit.com/futuremeng/LangFixer/releases/latest/download/latest.json"
        }
        _ => "https://github.com/narrativeos/LangFixer/releases/latest/download/latest.json",
    };
    vec![url.parse().expect("内置更新 URL 应合法")]
}

/// 启动 30 秒后做首次检查，之后每 6 小时一次；`asked` 记录本会话已询问/已
/// 提示过的版本，避免同一会话反复打扰用户。
pub fn spawn(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut asked: Option<String> = None;
        tokio::time::sleep(Duration::from_secs(30)).await;
        loop {
            check_and_install(&app, &mut asked).await;
            tokio::time::sleep(Duration::from_secs(6 * 3600)).await;
        }
    });
}

async fn check_and_install(app: &AppHandle, asked: &mut Option<String>) {
    let updater = match app
        .updater_builder()
        .endpoints(endpoints())
        .and_then(|b| b.build())
    {
        Ok(u) => u,
        Err(e) => {
            tracing::warn!("updater 初始化失败: {e}");
            return;
        }
    };
    let update = match updater.check().await {
        Ok(Some(u)) => u,
        Ok(None) => {
            clear_pending(app); // 已是最新（或 pending 标记对应的版本已生效）
            return;
        }
        Err(e) => {
            tracing::warn!("检查更新失败（源: {}）: {e}", release_source());
            return;
        }
    };
    // 已装好、等待重启（macOS / Linux 安装成功后落盘）：不再重复下载安装与弹框
    if pending(app).as_deref() == Some(update.version.as_str()) {
        return;
    }
    // 本会话已询问过（用户选了「稍后再说」或已提示过）：本轮不再打扰
    if asked.as_deref() == Some(update.version.as_str()) {
        return;
    }

    #[cfg(windows)]
    {
        *asked = Some(update.version.clone());
        // 必须先征得同意：install() 拉起安装器后会直接结束本进程
        let agreed = ask(
            app,
            format!(
                "发现新版本 {}，是否立即更新？\n应用将自动退出并完成安装，随后重新启动。",
                update.version
            ),
            "立即更新",
        )
        .await;
        if !agreed {
            return; // 不落盘 pending：下次启动仍会询问
        }
        match update.download(|_, _| {}, || {}).await {
            Ok(bytes) => {
                if let Err(e) = update.install(&bytes) {
                    tracing::warn!("更新安装失败（Windows）: {e}");
                }
                // 成功时 install() 已结束进程，交由安装器接管
            }
            Err(e) => tracing::warn!("更新下载失败: {e}"),
        }
    }

    #[cfg(not(windows))]
    {
        tracing::info!(
            "发现新版本 {}（源: {}），开始自动下载安装",
            update.version,
            release_source()
        );
        match update.download(|_, _| {}, || {}).await {
            Ok(bytes) => match update.install(&bytes) {
                Ok(()) => {
                    // 先持久化「已装好待重启」再弹框：冷启动不再重复下载安装与提示
                    write_pending(app, &update.version);
                    *asked = Some(update.version.clone());
                    let restart = ask(
                        app,
                        format!(
                            "新版本 {} 已安装完成，是否立即重启生效？",
                            update.version
                        ),
                        "立即重启",
                    )
                    .await;
                    if restart {
                        app.restart();
                    }
                }
                Err(e) => tracing::warn!("自动更新安装失败: {e}"),
            },
            Err(e) => tracing::warn!("自动更新下载失败: {e}"),
        }
    }
}

/// 原生确认弹框（确定 / 取消），在异步上下文中等待用户选择；
/// 弹框异常时按「取消」处理，绝不做破坏性默认动作。
async fn ask(app: &AppHandle, message: String, ok_label: &str) -> bool {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .message(message)
        .title("LangFixer 更新")
        .buttons(MessageDialogButtons::OkCancelCustom(
            ok_label.into(),
            "稍后再说".into(),
        ))
        .show(move |ok| {
            let _ = tx.send(ok);
        });
    rx.await.unwrap_or(false)
}

/// 「已装好待重启」版本号的持久化文件（与 window-state.json 同目录）
fn pending_path(app: &AppHandle) -> Option<PathBuf> {
    app.path()
        .app_config_dir()
        .ok()
        .map(|dir| dir.join("update-pending.txt"))
}

fn pending(app: &AppHandle) -> Option<String> {
    let raw = std::fs::read_to_string(pending_path(app)?).ok()?;
    let version = raw.trim().to_string();
    (!version.is_empty()).then_some(version)
}

/// 仅 macOS / Linux 使用：Windows 的 install() 直接由安装器接管，无需此标记
#[cfg(not(windows))]
fn write_pending(app: &AppHandle, version: &str) {
    if let Some(path) = pending_path(app) {
        if let Err(e) = std::fs::write(&path, version) {
            tracing::warn!("写入更新待重启标记失败（{}）: {e}", path.display());
        }
    }
}

fn clear_pending(app: &AppHandle) {
    if let Some(path) = pending_path(app) {
        let _ = std::fs::remove_file(path);
    }
}
