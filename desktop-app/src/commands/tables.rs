use std::sync::Arc;

use shared_model::DecisionTableMeta;

use super::core::Core;
/// 列出所有决策表
#[tauri::command]
pub fn list_tables(core: tauri::State<'_, Arc<Core>>) -> Vec<DecisionTableMeta> {
    core.tables.list()
}

/// 获取指定语言决策表原始 JSON
#[tauri::command]
pub fn get_table(core: tauri::State<'_, Arc<Core>>, lang: String) -> Result<String, String> {
    core.tables
        .get_raw(&lang)
        .ok_or_else(|| format!("语言 {lang} 无决策表"))
}

/// 热更新决策表
#[tauri::command]
pub fn update_table(
    core: tauri::State<'_, Arc<Core>>,
    lang: String,
    table_json: String,
) -> Result<DecisionTableMeta, String> {
    core.tables
        .update(&lang, &table_json)
        .map_err(|e| e.to_string())
}
