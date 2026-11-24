use std::sync::Arc;

use shared_model::SessionRecord;

use super::core::Core;
/// 会话记录（不存在时按 lang 建档返回；与 api-server GET /gec/sessions/:id 同构，
/// 差别：桌面端本地单用户，get 允许隐式建档）
#[tauri::command]
pub async fn get_session(
    core: tauri::State<'_, Arc<Core>>,
    session_id: String,
    lang: String,
) -> Result<SessionRecord, String> {
    Ok(core.sessions.get_or_create(&session_id, &lang))
}

/// 更新学习者画像（母语 / 自评 CEFR；部分更新，None/空串保持原值；
/// 与 api-server PUT /gec/sessions/:id/profile 同构）
#[tauri::command]
pub async fn update_session_profile(
    core: tauri::State<'_, Arc<Core>>,
    session_id: String,
    lang: String,
    native_language: Option<String>,
    cefr: Option<String>,
) -> Result<SessionRecord, String> {
    Ok(core
        .sessions
        .set_profile(&session_id, &lang, native_language, cefr))
}

/// 错题本列表（桌面端；Phase 2 学习扩展：用户采纳确认的错误，按聚合 + 最近出现倒序）
#[tauri::command]
pub fn session_mistakes(
    core: tauri::State<'_, Arc<Core>>,
    session_id: String,
) -> Result<Vec<shared_model::Mistake>, String> {
    Ok(core.sessions.list_mistakes(&session_id))
}

/// 到期复习队列（桌面端；Phase 2 间隔复习：未复习过或已到下次复习时间的错题）
#[tauri::command]
pub fn session_review_queue(
    core: tauri::State<'_, Arc<Core>>,
    session_id: String,
) -> Result<Vec<shared_model::Mistake>, String> {
    Ok(core.sessions.list_review_queue(&session_id))
}

/// 记录一次复习结果（桌面端；FSRS 四键间隔复习调度：rating = again/hard/good/easy，
/// 兼容旧契约 passed → good/again）
#[tauri::command]
pub fn review_mistake(
    core: tauri::State<'_, Arc<Core>>,
    session_id: String,
    mistake_id: i64,
    passed: Option<bool>,
    rating: Option<String>,
) -> Result<bool, String> {
    let rating = resolve_rating(rating.as_deref(), passed)?;
    Ok(core.sessions.record_review(&session_id, mistake_id, rating))
}

/// 撤销最近一次复习（桌面端；还原复习前记忆状态，每卡仅一次）
#[tauri::command]
pub fn undo_review_mistake(
    core: tauri::State<'_, Arc<Core>>,
    session_id: String,
    mistake_id: i64,
) -> Result<bool, String> {
    Ok(core.sessions.undo_review(&session_id, mistake_id))
}

/// 复习评分解析：rating 优先；无 rating 时用 passed 映射（good/again）
fn resolve_rating(
    rating: Option<&str>,
    passed: Option<bool>,
) -> Result<shared_model::session_store::ReviewRating, String> {
    use shared_model::session_store::ReviewRating;
    if let Some(r) = rating {
        return ReviewRating::parse(r).ok_or_else(|| format!("无效 rating: {r}"));
    }
    match passed {
        Some(p) => Ok(ReviewRating::from_passed(p)),
        None => Err("需要 rating 或 passed".into()),
    }
}

/// 统一错误画像（桌面端；错题本与习惯性错误分析同一投影：
/// 错题本非空 → confirmed 基准；否则回退 detect_history 的 detected 基准）
#[tauri::command]
pub fn session_analysis(
    core: tauri::State<'_, Arc<Core>>,
    session_id: String,
) -> Result<shared_model::profile::ErrorProfile, String> {
    use shared_model::profile::{
        BASIS_CONFIRMED, BASIS_DETECTED, ErrorObservation, build_error_profile,
        mastery_summary_from_mistakes, observations_from_mistakes,
    };
    let mistakes = core.sessions.list_mistakes(&session_id);
    if !mistakes.is_empty() {
        let observations = observations_from_mistakes(&mistakes);
        let summary = mastery_summary_from_mistakes(&mistakes);
        return Ok(build_error_profile(
            &session_id,
            BASIS_CONFIRMED,
            &observations,
            Some(summary),
        ));
    }
    let records = core
        .detect_history
        .list_page(Some(&session_id), None, None, 200, 0)
        .map_err(|e| e.to_string())?;
    let mut observations: Vec<ErrorObservation> = Vec::new();
    for record in &records {
        let errors: Vec<shared_model::Error> =
            serde_json::from_str(&record.errors_json).unwrap_or_default();
        for err in errors {
            observations.push(ErrorObservation {
                error_type: err.error_type,
                original: err.original,
                correction: err.correction,
                weight: 1,
            });
        }
    }
    Ok(build_error_profile(
        &session_id,
        BASIS_DETECTED,
        &observations,
        None,
    ))
}

/// 全局错题本列表（桌面端；跨会话聚合，`sessions_seen` = 出现会话数）
#[tauri::command]
pub fn global_mistakes(
    core: tauri::State<'_, Arc<Core>>,
) -> Result<Vec<shared_model::Mistake>, String> {
    Ok(core.sessions.list_global_mistakes())
}

/// 全局到期复习队列（桌面端；跨会话，已掌握不进入）
#[tauri::command]
pub fn global_review_queue(
    core: tauri::State<'_, Arc<Core>>,
) -> Result<Vec<shared_model::Mistake>, String> {
    Ok(core.sessions.list_global_review_queue())
}

/// 记录一次全局错题复习结果（桌面端；FSRS 四键调度挂在错题键上，跨会话共享）
#[tauri::command]
pub fn review_global_mistake(
    core: tauri::State<'_, Arc<Core>>,
    gid: i64,
    passed: Option<bool>,
    rating: Option<String>,
) -> Result<bool, String> {
    let rating = resolve_rating(rating.as_deref(), passed)?;
    Ok(core.sessions.record_global_review(gid, rating))
}

/// 撤销全局错题最近一次复习（桌面端）
#[tauri::command]
pub fn undo_global_review(core: tauri::State<'_, Arc<Core>>, gid: i64) -> Result<bool, String> {
    Ok(core.sessions.undo_global_review(gid))
}

/// 学习统计（D1；桌面端）：近 N 天逐日采纳/复习计数 + 窗口内评分分布 + 全时累计
#[tauri::command]
pub fn global_stats(
    core: tauri::State<'_, Arc<Core>>,
    days: Option<i64>,
) -> Result<shared_model::session_store::LearningStats, String> {
    Ok(core.sessions.learning_stats(days.unwrap_or(30)))
}

/// 全局统一错误画像（桌面端；确认错误基准，跨会话，含掌握度摘要）
#[tauri::command]
pub fn global_analysis(
    core: tauri::State<'_, Arc<Core>>,
) -> Result<shared_model::profile::ErrorProfile, String> {
    use shared_model::profile::{
        BASIS_CONFIRMED, build_error_profile, mastery_summary_from_mistakes,
        observations_from_mistakes,
    };
    let mistakes = core.sessions.list_global_mistakes();
    let observations = observations_from_mistakes(&mistakes);
    let summary = mastery_summary_from_mistakes(&mistakes);
    Ok(build_error_profile(
        "global",
        BASIS_CONFIRMED,
        &observations,
        Some(summary),
    ))
}

/// 删除会话（REST `DELETE /gec/sessions/:id` 同构）：级联删除会话的犯错流水 /
/// 错题本 / 会话级掌握度 / 撤销快照与检测历史；**全局复习进度（global_mastery）
/// 不删**——错题移除、复习进度保留，同一错误日后重现时记忆复活。
#[tauri::command]
pub fn delete_session(
    core: tauri::State<'_, Arc<Core>>,
    session_id: String,
) -> Result<bool, String> {
    if !core.sessions.remove(&session_id) {
        return Err(format!("session#{session_id} 不存在"));
    }
    let _ = core.detect_history.remove_by_session(&session_id);
    Ok(true)
}
