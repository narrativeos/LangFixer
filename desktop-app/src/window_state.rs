//! 窗口状态持久化
//!
//! 记住上次退出时的窗口位置 / 大小 / 最大化状态，下次启动时恢复：
//! - 存储位置：`app_config_dir()/window-state.json`
//!   （macOS：`~/Library/Application Support/com.langfixer.desktop/`）
//! - 保存的位置已不在当前显示器可见范围内（如拔掉了外接显示器）时回退为居中
//! - 缩放 / 移动时防抖保存（≥1 秒一次），关闭 / 销毁时强制保存
//! - 仅依赖 std + serde + tauri 核心，不引入新 crate

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tauri::{Manager, PhysicalPosition, PhysicalSize, State, Window};

/// 窗口最小尺寸（与 tauri.conf.json 的 minWidth / minHeight 保持一致）
const MIN_WIDTH: f64 = 960.0;
const MIN_HEIGHT: f64 = 640.0;

/// 与当前显示器至少要有多少物理像素的重叠才认为"在屏"
const MIN_OVERLAP: f64 = 100.0;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct WindowState {
    /// 窗口外框位置 / 大小（物理像素）
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    maximized: bool,
}

/// 缩放 / 移动事件的防抖状态：距上次落盘不足 1 秒则跳过
#[derive(Default)]
pub struct Saver {
    last: Mutex<Option<Instant>>,
}

impl Saver {
    fn allow(&self) -> bool {
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        match *last {
            Some(t) if t.elapsed().as_secs() < 1 => false,
            _ => {
                *last = Some(Instant::now());
                true
            }
        }
    }
}

fn state_path(app: &tauri::AppHandle) -> Option<PathBuf> {
    app.path()
        .app_config_dir()
        .ok()
        .map(|dir| dir.join("window-state.json"))
}

/// 恢复上次的窗口状态（在 setup 中、窗口显示前调用）
pub fn restore(window: &Window) {
    let app = window.app_handle();
    let Some(path) = state_path(&app) else {
        window.center().ok();
        return;
    };
    let Ok(raw) = std::fs::read_to_string(path) else {
        window.center().ok();
        return;
    };
    let Ok(state) = serde_json::from_str::<WindowState>(&raw) else {
        window.center().ok();
        return;
    };
    let w = state.width.max(MIN_WIDTH);
    let h = state.height.max(MIN_HEIGHT);

    // 上次是最大化：先恢复窗口原始尺寸（取消最大化时用），再最大化
    if state.maximized {
        window.set_size(PhysicalSize::new(w as u32, h as u32)).ok();
        window.maximize().ok();
        return;
    }

    match window.current_monitor() {
        Ok(Some(monitor)) => {
            let m = monitor.position();
            let s = monitor.size();
            // 保存矩形与当前显示器矩形的交集（宽 / 高）
            let mx = m.x as f64;
            let my = m.y as f64;
            let mw = s.width as f64;
            let mh = s.height as f64;
            let overlap_w = (state.x + w).min(mx + mw) - state.x.max(mx);
            let overlap_h = (state.y + h).min(my + mh) - state.y.max(my);
            if overlap_w >= MIN_OVERLAP && overlap_h >= MIN_OVERLAP {
                window
                    .set_position(PhysicalPosition::new(state.x as i32, state.y as i32))
                    .ok();
                window.set_size(PhysicalSize::new(w as u32, h as u32)).ok();
                return;
            }
            // 保存位置已离屏（如外接显示器断开）→ 回退居中
            window.center().ok();
        }
        // 取不到显示器信息：直接按保存值恢复
        _ => {
            window
                .set_position(PhysicalPosition::new(state.x as i32, state.y as i32))
                .ok();
            window.set_size(PhysicalSize::new(w as u32, h as u32)).ok();
        }
    }
}

/// 立即保存当前窗口状态（关闭 / 销毁时调用）
pub fn save(window: &Window) {
    let app = window.app_handle();
    let Some(path) = state_path(&app) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let (Ok(pos), Ok(size)) = (window.outer_position(), window.outer_size()) else {
        return;
    };
    let state = WindowState {
        x: pos.x as f64,
        y: pos.y as f64,
        width: size.width as f64,
        height: size.height as f64,
        maximized: window.is_maximized().unwrap_or(false),
    };
    if let Ok(raw) = serde_json::to_string(&state) {
        let _ = std::fs::write(path, raw);
    }
}

/// 防抖保存（缩放 / 移动事件，最多 1 秒一次）
pub fn save_debounced(window: &Window) {
    let saver: State<'_, Saver> = window.state();
    if saver.allow() {
        save(window);
    }
}
