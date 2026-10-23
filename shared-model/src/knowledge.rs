//! 知识库数据模型：知识源（KnowledgeSource）+ 知识点（KnowledgePoint）两个独立集合。
//!
//! 设计原则（规则与知识分离）：
//! - **决策表**只承载"可提取的规则"（错误类型 × 新手/置信度 → 归因策略），可随产品发布；
//! - **知识点内容**（讲解文本 / 例句 / 改错对）与**知识源**（教材等第三方资料）
//!   涉及知识产权，作为独立集合管理，不随产品内置发布；
//! - 知识点与知识源通过 [`KnowledgeSourceRef`] 关联（1:N）：
//!   同一个知识点 / 同一条规则可被多个知识源背书。

use serde::{Deserialize, Serialize};

/// 知识源引用：知识点 → 知识源的关联条目。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KnowledgeSourceRef {
    /// 知识源 ID（对应 [`KnowledgeSource::source_id`]）
    pub source_id: String,
    /// 源内位置（如 "第一讲 冠词（Article） · 一.1 定冠词的使用"）
    #[serde(default)]
    pub location: String,
    /// 源标题（由知识库在解析时回填；原始文件中为空，展示时回退 source_id）
    #[serde(default)]
    pub title: String,
}

/// 知识源（注册表条目）：仅元数据，不登记正文内容。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeSource {
    /// 稳定 ID（如 `src-sample-en`）
    pub source_id: String,
    /// 源标题（如书名）
    pub title: String,
    /// 作者
    #[serde(default)]
    pub author: String,
    /// 版次
    #[serde(default)]
    pub edition: String,
    /// 语言代码（en/zh/ja）
    pub lang: String,
    /// 许可范围：`internal_test`（仅内部测试，不对外发布）/ `licensed`（已获授权）/ `public`（公有领域）
    #[serde(default = "default_license_scope")]
    pub license_scope: String,
    /// 关联知识点数（由知识库在列表时统计；原始文件中为 0）
    #[serde(default)]
    pub point_count: usize,
}

fn default_license_scope() -> String {
    "internal_test".to_string()
}

/// 知识点：语言内稳定 ID，关联 1..N 个知识源。
///
/// 点 ID 与知识源解耦：新增一个知识源时，只需给相关点追加
/// [`KnowledgeSourceRef`]，点 ID 与决策表规则均不变。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgePoint {
    /// 稳定点 ID（语言作用域，如 `article.1`）
    pub point_id: String,
    /// 语言代码
    pub lang: String,
    /// 错误类型（对应决策表 `errorType`）
    pub error_type: String,
    /// 点标题
    pub title: String,
    /// 小节（如 "一.1"）
    #[serde(default)]
    pub section: String,
    /// CEFR 难度等级（A1-C2）
    #[serde(default)]
    pub cefr: String,
    /// 补救内容（知识内容；归因富化时覆盖决策表通用建议）
    #[serde(default)]
    pub remedial: String,
    /// 例句
    #[serde(default)]
    pub examples: Vec<String>,
    /// "错误 → 改正"对
    #[serde(default)]
    pub corrections: Vec<CorrectionPair>,
    /// 关联的知识源（1:N）
    #[serde(default)]
    pub sources: Vec<KnowledgeSourceRef>,
}

/// "错误 → 改正"对
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorrectionPair {
    pub wrong: String,
    pub right: String,
}

/// 学习知识条目（向量召回用）
///
/// 存储从 LLM 兜底采纳 / 用户手动录入的错误样本，
/// 附带文本嵌入向量用于相似度召回。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearnedKnowledge {
    /// 自增 ID
    pub id: i64,
    /// 语言代码
    pub lang: String,
    /// 原始文本（含错误的句子/片段）
    pub text: String,
    /// 错误类型
    pub error_type: String,
    /// 修正文本
    #[serde(default)]
    pub correction: String,
    /// 向量嵌入（f32 数组的 base64 编码，存储于 SQLite BLOB）
    #[serde(default, skip)]
    pub embedding: Vec<f32>,
    /// 来源：`llm_adopt` / `manual` / `llm_distill`
    #[serde(default)]
    pub source: String,
    /// 创建时间（ISO 8601）
    #[serde(default)]
    pub created_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_ref_defaults() {
        let json = r#"{ "source_id": "src-a" }"#;
        let r: KnowledgeSourceRef = serde_json::from_str(json).unwrap();
        assert_eq!(r.source_id, "src-a");
        assert!(r.location.is_empty());
        assert!(r.title.is_empty());
    }

    #[test]
    fn source_license_scope_defaults_to_internal_test() {
        let json = r#"{ "source_id": "s1", "title": "某书", "lang": "en" }"#;
        let s: KnowledgeSource = serde_json::from_str(json).unwrap();
        assert_eq!(s.license_scope, "internal_test");
        assert_eq!(s.point_count, 0);
    }

    #[test]
    fn point_round_trip_with_sources() {
        let p = KnowledgePoint {
            point_id: "article.1".into(),
            lang: "en".into(),
            error_type: "article".into(),
            title: "定冠词的使用".into(),
            section: "一.1".into(),
            cefr: "A2".into(),
            remedial: "规则讲解：…".into(),
            examples: vec!["The sun rose.".into()],
            corrections: vec![CorrectionPair {
                wrong: "a ancient".into(),
                right: "an ancient".into(),
            }],
            sources: vec![
                KnowledgeSourceRef {
                    source_id: "src-a".into(),
                    location: "第一讲 · 一.1".into(),
                    title: String::new(),
                },
                KnowledgeSourceRef {
                    source_id: "src-b".into(),
                    location: "第三章 · 2.1".into(),
                    title: String::new(),
                },
            ],
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: KnowledgePoint = serde_json::from_str(&json).unwrap();
        assert_eq!(back.sources.len(), 2, "同一知识点可关联多个知识源");
        assert_eq!(back.corrections[0].wrong, "a ancient");
    }
}
