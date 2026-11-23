use std::time::Duration;

/// HanLP（NarrativeOS narrative-operator-nlp）服务默认地址
pub const DEFAULT_HANLP_ENDPOINT: &str = "http://127.0.0.1:8000";

/// `/analyze` 分块字数上限默认值。sidecar 实测耗时在 50–500 字区间近似
/// 平方（T ≈ 0.035s + 1.7e-5·n²，每请求固定开销仅 ~33ms），吞吐在 ~50–150 字
/// 达峰（50 字 ≈ 533 字/s，150 字 ≈ 365，500 字 ≈ 114）；150 取近最优吞吐且
/// 请求数适中（6200 字 ≈ 42 块 ≈ 16s vs 500 字块 ≈ 52s），单块 ~0.4s 对 10s
/// 超时留 25 倍余量，块间事件循环空出、健康探针不再饿死。
pub const DEFAULT_NLP_CHUNK_CHARS: usize = 150;

/// 检测器配置
#[derive(Debug, Clone)]
pub struct DetectorConfig {
    /// NLP 服务地址，如 `http://127.0.0.1:8000`
    pub endpoint: String,
    /// 请求超时
    pub timeout: Duration,
    /// 为 true 时完全跳过远端服务，仅使用本地规则
    pub offline_only: bool,
    /// 外置词表目录（`{dir}/zh_gec_lexicon.json`；缺失时用内置默认词表）
    pub lexicons_dir: std::path::PathBuf,
    /// `/analyze` 长文本按句分块的字数上限（0 = 不分块，整段单发）
    pub chunk_chars: usize,
}

impl DetectorConfig {
    /// 从环境变量读取：`HANLP_ENDPOINT`（服务地址）、`GEC_OFFLINE=1`（强制离线）、
    /// `GEC_LEXICONS_DIR`（词表目录，默认 `lexicons`）、
    /// `GEC_NLP_CHUNK_CHARS`（`/analyze` 分块上限，默认 150，0 = 不分块）
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
        let chunk_chars = std::env::var("GEC_NLP_CHUNK_CHARS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_NLP_CHUNK_CHARS);
        Self {
            endpoint,
            timeout: Duration::from_secs(10),
            offline_only,
            lexicons_dir,
            chunk_chars,
        }
    }

    /// 强制离线模式（测试 / 桌面端使用）
    pub fn offline() -> Self {
        Self {
            endpoint: String::new(),
            timeout: Duration::from_secs(10),
            offline_only: true,
            lexicons_dir: std::path::PathBuf::from("lexicons"),
            chunk_chars: DEFAULT_NLP_CHUNK_CHARS,
        }
    }
}
