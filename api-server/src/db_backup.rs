//! 生产库每日备份自动化（macOS launchd）。
//!
//! 程序内可配置项（`AppSettings.db_backup_enabled`）：打开即注册 LaunchAgent，
//! 关闭即撤销；默认关，开发环境零副作用。备份动作内联进 plist（`sqlite3 .backup`
//! 一致性快照 + `PRAGMA integrity_check` + 14 天保留 + 追加日志），把 DB 绝对路径
//! 在注册时烘焙进去——不依赖仓库工作区路径，打包发布同样成立。
//!
//! 诚实性：`enabled` 是愿望，`installed` 是事实；GET 侧每次都实测 launchd/plist，
//! 两态不一致时面板如实显示「开关已开但任务缺失」。

use std::path::{Path, PathBuf};

pub const LABEL: &str = "com.langfixer.dbbackup";
const RETENTION_DAYS: u32 = 14;

/// launchd plist 的注册路径（`~/Library/LaunchAgents/<LABEL>.plist`）。
pub fn plist_path() -> Option<PathBuf> {
    dirs_home().map(|h| {
        h.join("Library/LaunchAgents")
            .join(format!("{LABEL}.plist"))
    })
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// 相对路径 → 绝对（存在即 canonicalize，不存在按 cwd 拼接），供 plist 烘焙。
fn resolve_abs(p: &Path) -> Result<PathBuf, String> {
    if p.is_absolute() {
        return Ok(p.to_path_buf());
    }
    if let Ok(abs) = p.canonicalize() {
        return Ok(abs);
    }
    let cwd = std::env::current_dir().map_err(|e| format!("无法确定工作目录: {e}"))?;
    Ok(cwd.join(p))
}

/// 备份脚本（sh -c 内联）。参数是烘焙时的 DB 绝对路径与备份目录。
/// DB 路径用 shell 单引号安全包裹；`[ -f ]` 守卫让开发机（无该 DB）静默跳过。
pub fn backup_script(db: &Path, dest: &Path) -> String {
    format!(
        "set -u; db='{}'; dest='{}'; [ -f \"$db\" ] || exit 0; mkdir -p \"$dest\"; \
         f=\"$dest/langfixer-$(date +%Y%m%dT%H%M%S).db\"; \
         sqlite3 \"$db\" \".backup \\\"$f\\\"\" || {{ echo \"$(date) 备份失败\" >> \"$dest/backup.log\"; exit 1; }}; \
         [ \"$(sqlite3 \"$f\" 'PRAGMA integrity_check;')\" = ok ] || {{ echo \"$(date) 完整性失败 $f\" >> \"$dest/backup.log\"; rm -f \"$f\"; exit 1; }}; \
         chmod 600 \"$f\"; echo \"$(date) ok $f\" >> \"$dest/backup.log\"; \
         find \"$dest\" -name 'langfixer-*.db' -mtime +{} -delete 2>/dev/null; true",
        shell_quote(&db.display().to_string()),
        shell_quote(&dest.display().to_string()),
        RETENTION_DAYS
    )
}

/// 单引号安全包裹（' → '\''）。
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// 每日 04:00 + 注册即跑一次。路径已烘焙，故 plist 内容即完整任务。
pub fn plist_xml(db: &Path, dest: &Path) -> String {
    let script = xml_escape(&backup_script(db, dest));
    let stdout = xml_escape(&dest.join("launchd.log").display().to_string());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{LABEL}</string>
	<key>ProgramArguments</key>
	<array>
		<string>/bin/sh</string>
		<string>-c</string>
		<string>{script}</string>
	</array>
	<key>StartCalendarInterval</key>
	<dict>
		<key>Hour</key>
		<integer>4</integer>
		<key>Minute</key>
		<integer>0</integer>
	</dict>
	<key>RunAtLoad</key>
	<true/>
	<key>StandardOutPath</key>
	<string>{stdout}</string>
	<key>StandardErrorPath</key>
	<string>{stdout}</string>
</dict>
</plist>
"#
    )
}

/// launchd 事实态：plist 在且已加载（`launchctl print` 为 0）。
pub fn installed() -> bool {
    let Some(pp) = plist_path() else { return false };
    if !pp.exists() {
        return false;
    }
    launchctl_print().is_some()
}

fn launchctl_uid() -> Option<u32> {
    #[cfg(target_os = "macos")]
    {
        Some(libc_getuid())
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

#[cfg(target_os = "macos")]
fn libc_getuid() -> u32 {
    // 不引 libc crate：id -u 足够，且只在 mac 分支调用。
    std::process::Command::new("/usr/bin/id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
        .unwrap_or(0)
}

fn launchctl_print() -> Option<()> {
    let uid = launchctl_uid()?;
    let out = std::process::Command::new("/bin/launchctl")
        .args(["print", &format!("gui/{uid}/{LABEL}")])
        .output()
        .ok()?;
    if out.status.success() { Some(()) } else { None }
}

/// 注册任务：写 plist → bootout（幂等）→ bootstrap。仅 macOS。
/// DB 路径烘焙前解析为绝对路径（开发环境默认 `langfixer.db` 是相对 cwd 的）；
/// 备份目录固定为 DB 同级 `backups/`。
pub fn install(db: &Path) -> Result<(), String> {
    if !cfg!(target_os = "macos") {
        return Err("每日备份仅支持 macOS（launchd）".into());
    }
    let db = resolve_abs(db)?;
    let dest = backup_dest(&db);
    let db = db.as_path();
    let dest = dest.as_path();
    let pp = plist_path().ok_or("无法确定 HOME 目录")?;
    std::fs::create_dir_all(pp.parent().ok_or("LaunchAgents 路径异常")?)
        .map_err(|e| format!("创建 LaunchAgents 目录失败: {e}"))?;
    std::fs::create_dir_all(dest).map_err(|e| format!("创建备份目录失败: {e}"))?;
    std::fs::write(&pp, plist_xml(db, dest)).map_err(|e| format!("写 plist 失败: {e}"))?;
    let uid = launchctl_uid().ok_or("无法确定用户 id")?;
    // 先 bootout 再 bootstrap：重复打开/改 DB 路径时原子替换，已加载态不报重复。
    let _ = run(
        "/bin/launchctl",
        &["bootout", &format!("gui/{uid}/{LABEL}")],
    );
    let out = run(
        "/bin/launchctl",
        &[
            "bootstrap",
            &format!("gui/{uid}"),
            &pp.display().to_string(),
        ],
    )?;
    if out.status.success() {
        Ok(())
    } else {
        let _ = std::fs::remove_file(&pp); // 注册失败不留半吊子 plist
        Err(format!(
            "launchctl bootstrap 失败: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// 撤销任务：bootout（容忍未加载）→ 删 plist。幂等。
pub fn uninstall() -> Result<(), String> {
    if let Some(uid) = launchctl_uid() {
        let _ = run(
            "/bin/launchctl",
            &["bootout", &format!("gui/{uid}/{LABEL}")],
        );
    }
    if let Some(pp) = plist_path() {
        match std::fs::remove_file(&pp) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("删除 plist 失败: {e}")),
        }
    }
    Ok(())
}

fn run(prog: &str, args: &[&str]) -> Result<std::process::Output, String> {
    std::process::Command::new(prog)
        .args(args)
        .output()
        .map_err(|e| format!("无法执行 {prog}: {e}"))
}

/// 备份目标目录：DB 同级 `backups/`（与 RUNBOOK 纪律一致；目录权限收紧由脚本 chmod 600 承担）。
pub fn backup_dest(db: &Path) -> PathBuf {
    db.parent()
        .map(|p| p.join("backups"))
        .unwrap_or_else(|| PathBuf::from("backups"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_embeds_paths_and_escaping() {
        let xml = plist_xml(Path::new("/tmp/o'brien/langfixer.db"), Path::new("/tmp/db"));
        assert!(xml.contains(LABEL), "label 缺失");
        // 路径里的单引号被 shell 安全包裹，XML 侧再无裸 < > &。
        assert!(
            xml.contains(r#"'\'o'\''brien'"#) || xml.contains(r#"'\''"#),
            "单引号转义缺失: {xml}"
        );
        assert!(!xml.contains("&lt;"), "不应双重转义");
        assert!(xml.contains("integrity_check"));
        assert!(xml.contains("-mtime +14"));
    }

    #[test]
    fn backup_dest_is_sibling_of_db() {
        assert_eq!(
            backup_dest(Path::new("/repo/langfixer.db")),
            PathBuf::from("/repo/backups")
        );
    }

    #[test]
    fn uninstall_is_idempotent_when_absent() {
        // 开发环境默认从未安装：撤销必须安静成功（不触碰 launchd 已加载项）。
        if installed() {
            return; // 真装了就不动它
        }
        uninstall().expect("未安装态撤销应幂等成功");
    }

    #[test]
    fn installed_agrees_with_plist_presence() {
        // 不造假：plist 不在磁盘上时 installed 必须为 false。
        let present = plist_path().map(|p| p.exists()).unwrap_or(false);
        if !present {
            assert!(!installed());
        }
    }
}
