use std::time::Duration;

/// Sudachi 分词服务默认地址
pub const DEFAULT_SUDACHI_ENDPOINT: &str = "http://localhost:4802";

/// 检测器配置
#[derive(Debug, Clone)]
pub struct DetectorConfig {
    pub endpoint: String,
    pub timeout: Duration,
    pub offline_only: bool,
    /// 外置词表目录（`{dir}/ja_gec_lexicon.json`；缺失时用内置默认词表）
    pub lexicons_dir: std::path::PathBuf,
}

impl DetectorConfig {
    /// 从环境变量读取：`SUDACHI_ENDPOINT`、`GEC_OFFLINE=1`、`GEC_LEXICONS_DIR`（词表目录）
    pub fn from_env() -> Self {
        let endpoint = std::env::var("SUDACHI_ENDPOINT")
            .ok()
            .unwrap_or_else(|| DEFAULT_SUDACHI_ENDPOINT.to_string());
        let offline_only = std::env::var("GEC_OFFLINE")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let lexicons_dir = std::path::PathBuf::from(
            std::env::var("GEC_LEXICONS_DIR").unwrap_or_else(|_| "lexicons".to_string()),
        );
        Self {
            endpoint,
            timeout: Duration::from_secs(10),
            offline_only,
            lexicons_dir,
        }
    }

    /// 强制离线模式（测试 / 桌面端使用）
    pub fn offline() -> Self {
        Self {
            endpoint: String::new(),
            timeout: Duration::from_secs(10),
            offline_only: true,
            lexicons_dir: std::path::PathBuf::from("lexicons"),
        }
    }
}
