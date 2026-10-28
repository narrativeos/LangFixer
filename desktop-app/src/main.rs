//! LangFixer Tauri 桌面端入口
//!
//! 桌面端支持完全离线运行：本地检测器（离线兜底）+ 本地决策表 + 内存用户档案。
//! 窗口布局：首次启动居中（tauri.conf.json `center`），之后自动记住并恢复
//! 上次的位置 / 大小 / 最大化状态（`window_state` 模块）。

mod agent_tools;
mod commands;
mod knowledge_recall;
mod media;
mod settings;
mod window_state;

use std::sync::Arc;

use commands::Core;
use tauri::{Manager, WindowEvent};

fn main() {
    tauri::Builder::default()
        // 原生对话框（导出资源包「另存为」选择保存位置；capabilities/default.json 授权）
        .plugin(tauri_plugin_dialog::init())
        .manage(Arc::new(Core::from_env()))
        .manage(window_state::Saver::default())
        .invoke_handler(tauri::generate_handler![
            commands::detect,
            commands::analyze,
            commands::list_tables,
            commands::get_table,
            commands::update_table,
            commands::knowledge_sources,
            commands::knowledge_points,
            commands::start_ingest,
            commands::start_ingest_file,
            commands::parse_preview,
            commands::ingest_sessions,
            commands::ingest_session,
            commands::ingest_drafts,
            commands::ingest_plan,
            commands::derive_rules,
            commands::ingest_rules_plan,
            commands::publish_ingest,
            commands::discard_ingest,
            commands::dataset_summary,
            commands::dataset_export,
            commands::dataset_import,
            commands::dataset_import_bytes,
            commands::dataset_clean,
            commands::record_error,
            commands::explain,
            commands::languages,
            commands::list_rules,
            commands::toggle_rule,
            commands::put_toggles,
            commands::list_rule_examples,
            commands::add_custom_word,
            commands::list_custom_words,
            commands::remove_custom_word,
            commands::list_history,
            commands::get_history,
            commands::remove_history,
            commands::clear_history,
            commands::get_settings,
            commands::update_settings,
            commands::test_setting,
            commands::list_learned_knowledge,
            commands::add_learned_knowledge,
            commands::remove_learned_knowledge,
            commands::llm_feedback,
            commands::test_knowledge_recall,
            commands::l3_collect,
            commands::l3_status,
            commands::l3_retrain,
            commands::error_feedback,
            commands::feedback_agent,
            commands::confirm_agent_proposal,
            commands::feedback_action,
            commands::reflect_llm,
            commands::feedback_history,
            commands::run_eval,
            commands::eval_job,
            commands::list_eval_reports,
            commands::get_eval_report,
            commands::run_reflect,
            commands::list_reflect_reports,
            commands::get_reflect_report,
            commands::list_reflect_suggestions,
            commands::update_reflect_suggestion,
            commands::train_build_pool,
            commands::train_build_llm_data,
            commands::train_run_l3_retrain,
            commands::train_apply_lexicon,
            commands::train_run_suggestion,
            commands::train_list_jobs,
            commands::train_get_job,
            commands::eval_contributions,
            commands::round_evaluate,
            commands::round_list,
            commands::round_get,
            commands::round_confirm,
            commands::round_reject,
            commands::round_execute,
            commands::round_report,
            // 载体（图片 / PDF / Word）审校：链路由 `media-flow` 提供，桌面端注入本地检测级联
            media::media_probes,
            media::media_inspect,
            media::media_create_job,
            media::media_jobs,
            media::media_job,
            media::media_preview,
            media::media_apply,
            media::media_report,
            media::media_report_file
        ])
        .setup(|app| {
            // 窗口显示前恢复上次的窗口状态（位置 / 大小 / 最大化）
            if let Some(win) = app.get_webview_window("main") {
                let window: tauri::Window = win.as_ref().window();
                window_state::restore(&window);
            }
            Ok(())
        })
        .on_window_event(|window, event| match event {
            // 缩放 / 移动：防抖保存（最多 1 秒一次）
            WindowEvent::Resized(_) | WindowEvent::Moved(_) => window_state::save_debounced(window),
            // 关闭 / 销毁：强制保存最终状态
            WindowEvent::CloseRequested { .. } | WindowEvent::Destroyed => {
                window_state::save(window)
            }
            _ => {}
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
