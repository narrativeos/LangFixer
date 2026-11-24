/// 前端主题切换 / 启动对齐时同步原生窗口背景色。
/// 与 style.css 的 `--bg` 一致（浅色 #f2f3f8 / 暗色 #14161e）：
/// 消除暗色模式下冷启动先闪一帧浅色原生背景的窗口级闪烁
#[tauri::command]
pub fn set_window_theme(window: tauri::Window, dark: bool) -> Result<(), String> {
    let color = if dark {
        tauri::utils::config::Color(0x14, 0x16, 0x1e, 255)
    } else {
        tauri::utils::config::Color(0xf2, 0xf3, 0xf8, 255)
    };
    window
        .set_background_color(Some(color))
        .map_err(|e| e.to_string())
}
