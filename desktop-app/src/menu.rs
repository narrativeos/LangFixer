//! 原生菜单栏 + 右键上下文菜单（macOS HIG §1：菜单栏是功能发现的第一入口）
//!
//! - 菜单栏：App / File / Edit / View / Window / Help 标准结构；所有命令项带快捷键；
//!   点击后统一以 `menu-action` 事件转发给前端（前端按 `menu:*` id 路由到视图 / 动作）。
//! - 右键菜单：`show_context_menu` 命令在鼠标位置弹出原生菜单，异步返回用户选择的项 id
//!   （Esc / 点击外部关闭 = 超时返回 null）。菜单事件经 `handle` 路由回该命令的通道。

use std::sync::Mutex;

use tauri::menu::{ContextMenu, MenuBuilder, MenuItemBuilder, SubmenuBuilder};
use tauri::{App, AppHandle, Emitter, Runtime, Window};
use tokio::sync::oneshot;

/// 当前等待中的右键菜单回执通道（同一时刻只允许一个原生弹出菜单）
static CTX_TX: Mutex<Option<oneshot::Sender<String>>> = Mutex::new(None);

/// 构建并设置应用菜单栏。id 约定：
/// - `menu:view:<view>` 顶层视图切换（⌘1–⌘7，前端按能力位门控）
/// - `menu:settings` ⌘, / `menu:import` ⌘O / `menu:export` ⇧⌘E / `menu:undo-apply` ⌥⌘Z
/// - `menu:toggle-theme` ⌥⌘T / `menu:onboarding` 使用引导 / `menu:check-update` 检查更新
pub fn build<R: Runtime>(app: &App<R>) -> tauri::Result<()> {
    let item =
        |id: &str, title: &str, accel: Option<&str>| -> tauri::Result<tauri::menu::MenuItem<R>> {
            let mut b = MenuItemBuilder::with_id(id, title);
            if let Some(a) = accel {
                b = b.accelerator(a);
            }
            b.build(app)
        };

    let settings = item("menu:settings", "设置…", Some("CmdOrCtrl+,"))?;

    let import = item("menu:import", "导入载体文件…", Some("CmdOrCtrl+O"))?;
    let export = item("menu:export", "导出正误表…", Some("Cmd+Shift+E"))?;

    let undo_apply = item("menu:undo-apply", "撤销应用全部修正", Some("Cmd+Alt+Z"))?;

    let v_workspace = item("menu:view:workspace", "进入检测", Some("Cmd+1"))?;
    let v_learning = item("menu:view:learning", "练习与复习", Some("Cmd+2"))?;
    let v_media = item("menu:view:media", "载体审校", Some("Cmd+3"))?;
    let v_rules = item("menu:view:rules", "规则管理", Some("Cmd+4"))?;
    let v_knowledge = item("menu:view:knowledge", "知识维护", Some("Cmd+5"))?;
    let v_evolution = item("menu:view:evolution", "进化中心", Some("Cmd+6"))?;
    let v_settings = item("menu:view:settings", "设置", Some("Cmd+7"))?;
    let theme = item(
        "menu:toggle-theme",
        "切换浅色 / 深色主题",
        Some("Cmd+Alt+T"),
    )?;

    let onboarding = item("menu:onboarding", "使用引导", None)?;
    let check_update = item("menu:check-update", "检查更新…", None)?;

    // macOS 上第一个子菜单自动作为应用菜单（标题被系统替换）
    let app_menu = SubmenuBuilder::new(app, "LangFixer")
        .about(None)
        .separator()
        .item(&settings)
        .separator()
        .services()
        .separator()
        .hide()
        .hide_others()
        .show_all()
        .separator()
        .quit()
        .build()?;

    let file_menu = SubmenuBuilder::new(app, "文件")
        .item(&import)
        .item(&export)
        .separator()
        .close_window()
        .build()?;

    let edit_menu = SubmenuBuilder::new(app, "编辑")
        .undo()
        .redo()
        .separator()
        .item(&undo_apply)
        .separator()
        .cut()
        .copy()
        .paste()
        .select_all()
        .build()?;

    let view_menu = SubmenuBuilder::new(app, "显示")
        .item(&v_workspace)
        .item(&v_learning)
        .item(&v_media)
        .item(&v_rules)
        .item(&v_knowledge)
        .item(&v_evolution)
        .item(&v_settings)
        .separator()
        .item(&theme)
        .separator()
        .fullscreen()
        .build()?;

    let window_menu = SubmenuBuilder::new(app, "窗口")
        .minimize()
        .maximize()
        .build()?;
    #[cfg(target_os = "macos")]
    window_menu.set_as_windows_menu_for_nsapp()?;

    let help_menu = SubmenuBuilder::new(app, "帮助")
        .item(&onboarding)
        .item(&check_update)
        .build()?;
    #[cfg(target_os = "macos")]
    help_menu.set_as_help_menu_for_nsapp()?;

    let menu = MenuBuilder::new(app)
        .items(&[
            &app_menu,
            &file_menu,
            &edit_menu,
            &view_menu,
            &window_menu,
            &help_menu,
        ])
        .build()?;
    app.set_menu(menu).map(|_| ())
}

/// 菜单事件路由：`ctx:*` 交给等待中的右键菜单，其余以 `menu-action` 事件转发前端
pub fn handle(app: &AppHandle, id: &str) {
    if let Some(rest) = id.strip_prefix("ctx:") {
        if let Some(tx) = CTX_TX.lock().ok().and_then(|mut g| g.take()) {
            let _ = tx.send(rest.to_string());
        }
        return;
    }
    let _ = app.emit("menu-action", id.to_string());
}

/// 右键上下文菜单：在鼠标位置弹出原生菜单，返回用户选择的项（null = 未选择）。
/// kind：history（检测历史行）/ error（错误条目）/ media-job（载体任务）
#[tauri::command]
pub async fn show_context_menu(
    app: AppHandle,
    window: Window,
    kind: String,
) -> Result<Option<String>, String> {
    let items: &[(&str, &str)] = match kind.as_str() {
        "history" => &[
            ("ctx:history:load", "载入工作区"),
            ("ctx:history:delete", "删除记录"),
        ],
        "error" => &[
            ("ctx:error:locate", "定位到原文"),
            ("ctx:error:copy-original", "复制原文"),
            ("ctx:error:copy-correction", "复制修正文本"),
        ],
        "media-job" => &[
            ("ctx:job:open", "打开载体"),
            ("ctx:job:export", "导出正误表"),
        ],
        other => return Err(format!("未知右键菜单类型: {other}")),
    };
    let mut builder = MenuBuilder::new(&app);
    for (id, title) in items {
        builder = builder.item(
            &MenuItemBuilder::with_id(*id, *title)
                .build(&app)
                .map_err(|e| e.to_string())?,
        );
    }
    let menu = builder.build().map_err(|e| e.to_string())?;

    let (tx, rx) = oneshot::channel();
    match CTX_TX.lock() {
        Ok(mut guard) => *guard = Some(tx),
        Err(e) => return Err(format!("右键菜单状态锁失败: {e}")),
    }
    // 弹出必须在主线程；Esc / 点击外部关闭不会产生菜单事件 → 由超时兜底返回 None
    let popup_menu = menu.clone();
    let popup_win = window.clone();
    window
        .run_on_main_thread(move || {
            if let Err(e) = popup_menu.popup(popup_win) {
                tracing::warn!("右键菜单弹出失败: {e}");
            }
        })
        .map_err(|e| e.to_string())?;

    let selected = match tokio::time::timeout(std::time::Duration::from_secs(30), rx).await {
        Ok(Ok(id)) => Some(id),
        _ => None, // 超时（Esc / 点击外部关闭）或通道被替换：视为未选择
    };
    // 清理未消费的通道（超时 / 重复弹出场景）
    if let Ok(mut guard) = CTX_TX.lock() {
        guard.take();
    }
    Ok(selected)
}
