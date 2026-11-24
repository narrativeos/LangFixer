use std::path::PathBuf;
use std::sync::Arc;

use super::core::Core;
pub(crate) fn tables_dir_of(core: &Core) -> PathBuf {
    PathBuf::from(core.settings.read().unwrap().tables_dir.clone())
}

/// 某语言决策表文件路径（发布派生规则等写回用）
pub(crate) fn tables_file_path(core: &Core, lang: &str) -> PathBuf {
    tables_dir_of(core).join(format!("{lang}_gec_attribution.json"))
}

pub(crate) fn pack_atomic_write(path: &std::path::Path, content: &str) -> Result<(), String> {
    // 临时名带随机后缀：并发写同一目标不互相踩临时文件
    let tmp = path.with_extension(format!(
        "{}.tmp.{}",
        path.extension().and_then(|e| e.to_str()).unwrap_or("tmp"),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::write(&tmp, content).map_err(|e| format!("写入失败 {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("替换失败 {}: {e}", path.display()))?;
    Ok(())
}

pub(crate) fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// RFC3339 → SQLite `datetime('now')` 格式（反馈表 created_at 用 datetime('now')，
/// 字符串比较要求同格式；轮次增量窗口边界在此归一化）
pub(crate) fn sqlite_dt(iso: &str) -> String {
    match iso.split_once('T') {
        Some((d, t)) => format!("{} {}", d, &t[..t.len().min(8)]),
        None => iso.to_string(),
    }
}

/// 进程级 LLM 网关熔断器（桌面端 LLM 引擎按调用现构建，检测与蒸馏两处共用同一只；
/// 语义见 `gec_core::circuit`：连续失败开断，冷却期内毫秒级失败，到期自动探测恢复）
pub(crate) fn llm_breaker() -> Arc<gec_core::circuit::CircuitBreaker> {
    static BREAKER: std::sync::OnceLock<Arc<gec_core::circuit::CircuitBreaker>> =
        std::sync::OnceLock::new();
    BREAKER
        .get_or_init(|| Arc::new(gec_core::circuit::CircuitBreaker::default()))
        .clone()
}
