use serde::{Deserialize, Serialize};

/// 决策表元数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionTableMeta {
    /// 语言代码
    pub lang: String,
    pub version: String,
    /// ISO 8601 时间戳
    pub updated_at: String,
    /// 决策表规则行数（真实统计；旧客户端缺省为 0）
    #[serde(default)]
    pub rule_count: usize,
}

/// 决策表完整视图（元数据 + 原始 JDM JSON）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionTableView {
    pub meta: DecisionTableMeta,
    pub raw: String,
}
