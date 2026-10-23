use std::time::Duration;

/// HanLP（NarrativeOS narrative-operator-nlp）服务默认地址
pub const DEFAULT_HANLP_ENDPOINT: &str = "http://127.0.0.1:8000";

/// 检测器配置
#[derive(Debug, Clone)]
pub struct DetectorConfig {
    /// NLP 服务地址，如 `http://127.0.0.1:8000`
    pub endpoint: String,
    /// 请求超时
    pub timeout: Duration,
    /// 为 true 时完全跳过远端服务，仅使用本地规则
    pub offline_only: bool,
    /// 外置词表目录（`{dir}/en_gec_lexicon.json`；缺失时用内置默认词表）
    pub lexicons_dir: std::path::PathBuf,
}

impl DetectorConfig {
    /// 从环境变量读取：`HANLP_ENDPOINT`（服务地址）、`GEC_OFFLINE=1`（强制离线）、
    /// `GEC_LEXICONS_DIR`（词表目录，默认 `lexicons`）
    pub fn from_env() -> Self {
        let endpoint = std::env::var("HANLP_ENDPOINT")
            .ok()
            .unwrap_or_else(|| DEFAULT_HANLP_ENDPOINT.to_string());
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
