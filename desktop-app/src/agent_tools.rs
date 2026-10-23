//! Agent 工具面（桌面端镜像；与 api-server `agent_tools` 同源，捕获 `Core` 句柄）
//!
//! 层级策略与 API 端一致：
//! - **Autonomous**（只读 + 可逆写）：agent 直接执行
//! - **Confirm**（破坏性）：gate 拦截转提案，用户点击执行

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use gec_core::{ActionTier, AgentToolExecutor, ManagedTool};
use lexicon_store::{append_rule_exclude, remove_rule_exclude};

use crate::commands::Core;

/// 工具上下文：单语言作用域 + Core 句柄
#[derive(Clone)]
pub struct AgentToolCtx {
    pub lang: String,
    pub core: Arc<Core>,
}

impl AgentToolCtx {
    pub fn new(core: &Arc<Core>, lang: &str) -> Self {
        Self {
            lang: lang.to_string(),
            core: core.clone(),
        }
    }

    /// 构建该语言的完整工具集（只读 + Autonomous + Confirm）
    pub fn build_tools(&self) -> Vec<ManagedTool> {
        let ctx = self.clone();
        vec![
            // ---- 只读（Autonomous）----
            ManagedTool::new(
                "list_rules",
                "列出规则库：规则 id / 分类 / 启用状态 / 描述 / 示例数。用于了解有哪些规则及其状态。",
                json!({"type": "object", "properties": {
                    "category": {"type": "string", "description": "按分类过滤（如 spelling / grammar），可选"},
                    "q": {"type": "string", "description": "按 id 或描述关键词搜索，可选"}
                }}),
                ActionTier::Autonomous,
                Arc::new(ListRules { ctx: ctx.clone() }),
            ),
            ManagedTool::new(
                "get_rule",
                "查看单条规则详情：定义 / 示例对 / 排除词名单。",
                json!({"type": "object", "properties": {
                    "rule_id": {"type": "string"}
                }, "required": ["rule_id"]}),
                ActionTier::Autonomous,
                Arc::new(GetRule { ctx: ctx.clone() }),
            ),
            ManagedTool::new(
                "search_knowledge",
                "搜索已学习的知识条目（误报模式 / 确认过的错误模式）。",
                json!({"type": "object", "properties": {
                    "query": {"type": "string", "description": "搜索关键词（文本子串匹配），可选"},
                    "limit": {"type": "integer", "description": "返回条数上限，默认 10"}
                }}),
                ActionTier::Autonomous,
                Arc::new(SearchKnowledge { ctx: ctx.clone() }),
            ),
            ManagedTool::new(
                "feedback_stats",
                "查看各规则的历史反馈统计（采纳 / 拒绝次数、最近时间）。判断规则误报率的核心依据。",
                json!({"type": "object"}),
                ActionTier::Autonomous,
                Arc::new(FeedbackStats { ctx: ctx.clone() }),
            ),
            // ---- 可逆写（Autonomous）----
            ManagedTool::new(
                "add_exclude_word",
                "把一个词加入指定规则的排除名单：该词不再触发此规则（规则保持启用）。适合单点误报（如 POS 误标）。",
                json!({"type": "object", "properties": {
                    "rule_id": {"type": "string"},
                    "word": {"type": "string", "description": "要排除的词"}
                }, "required": ["rule_id", "word"]}),
                ActionTier::Autonomous,
                Arc::new(AddExcludeWord { ctx: ctx.clone() }),
            ),
            ManagedTool::new(
                "remove_exclude_word",
                "把词从规则的排除名单中移除（撤销之前误加的排除）。",
                json!({"type": "object", "properties": {
                    "rule_id": {"type": "string"},
                    "word": {"type": "string"}
                }, "required": ["rule_id", "word"]}),
                ActionTier::Autonomous,
                Arc::new(RemoveExcludeWord { ctx: ctx.clone() }),
            ),
            ManagedTool::new(
                "add_l3_negative",
                "把一条文本汇入 L3 模型飞轮的负样本（label=0，确认非错误）。积累后重训练可系统性修正模型层误报。",
                json!({"type": "object", "properties": {
                    "text": {"type": "string", "description": "原文片段"},
                    "error_type": {"type": "string", "description": "可选：当时报告的错误类型"}
                }, "required": ["text"]}),
                ActionTier::Autonomous,
                Arc::new(AddL3Negative { ctx: ctx.clone() }),
            ),
            ManagedTool::new(
                "add_knowledge",
                "新增一条学习知识条目（后续检测可召回，用于抑制同类误报或补充修正模式）。",
                json!({"type": "object", "properties": {
                    "text": {"type": "string", "description": "知识文本（模式描述或原文片段）"},
                    "error_type": {"type": "string"},
                    "correction": {"type": "string", "description": "建议修正，可选"}
                }, "required": ["text", "error_type"]}),
                ActionTier::Autonomous,
                Arc::new(AddKnowledge { ctx: ctx.clone() }),
            ),
            // ---- 知识源导入（Autonomous：草稿区操作，发布前不影响生效知识库）----
            ManagedTool::new(
                "list_knowledge_points",
                "列出知识库既有点（point_id / error_type / section / title / cefr）。抽取书本内容前先查询：既有概念用 match_point_id 背书既有点（1:N），不要造重复新点。",
                json!({"type": "object", "properties": {
                    "error_type": {"type": "string", "description": "按错误类型过滤（如 article），可选"},
                    "q": {"type": "string", "description": "按标题/point_id 关键词搜索，可选"},
                    "limit": {"type": "integer", "description": "返回条数上限，默认 50，最大 200"}
                }}),
                ActionTier::Autonomous,
                Arc::new(ListKnowledgePoints { ctx: ctx.clone() }),
            ),
            ManagedTool::new(
                "register_ingest_source",
                "注册一个知识源导入会话（书/教材元数据）。导入流程第一步；同名会话需先发布或放弃。",
                json!({"type": "object", "properties": {
                    "source_id": {"type": "string", "description": "稳定源 ID（如 src-xxx-v1）"},
                    "title": {"type": "string", "description": "书名"},
                    "author": {"type": "string", "description": "作者，可选"},
                    "edition": {"type": "string", "description": "版次，可选"},
                    "license_scope": {"type": "string", "description": "internal_test / licensed / public，默认 internal_test"}
                }, "required": ["source_id", "title"]}),
                ActionTier::Autonomous,
                Arc::new(RegisterIngestSource { ctx: ctx.clone() }),
            ),
            ManagedTool::new(
                "stage_points",
                "把一批从书本抽取的知识点草稿写入导入草稿区（逐条校验，非法条目被拒并回报原因）。草稿不影响生效知识库；发布由 publish_knowledge（需用户确认）完成。",
                json!({"type": "object", "properties": {
                    "source_id": {"type": "string"},
                    "points": {"type": "array", "description": "草稿点数组", "items": {
                        "type": "object",
                        "properties": {
                            "error_type": {"type": "string", "description": "错误类型（对齐既有分类法，如 article/tense/preposition）"},
                            "title": {"type": "string", "description": "规则点标题（书中规则表述）"},
                            "section": {"type": "string", "description": "小节号，如 一.1"},
                            "remedial": {"type": "string", "description": "规则讲解/补救内容"},
                            "cefr": {"type": "string", "description": "A1/A2/B1/B2/C1/C2"},
                            "examples": {"type": "array", "items": {"type": "string"}},
                            "corrections": {"type": "array", "items": {
                                "type": "object",
                                "properties": {"wrong": {"type": "string"}, "right": {"type": "string"}},
                                "required": ["wrong", "right"]
                            }},
                            "location": {"type": "string", "description": "源内位置（讲次 · 小节）"},
                            "match_point_id": {"type": "string", "description": "匹配到的既有点 ID（只追加背书，不造新点），可选"}
                        },
                        "required": ["error_type", "title"]
                    }}
                }, "required": ["source_id", "points"]}),
                ActionTier::Autonomous,
                Arc::new(StagePoints { ctx: ctx.clone() }),
            ),
            ManagedTool::new(
                "stage_rule_drafts",
                "把一批归因规则草稿写入导入会话（按 error_type 派生决策表候选行；逐条校验，非法条目拒收并回报）。JDM 行语法与 ID/溯源标记由系统生成，草稿只描述语义。发布时随知识点一并应用。",
                json!({"type": "object", "properties": {
                    "source_id": {"type": "string"},
                    "rules": {"type": "array", "description": "规则草稿数组", "items": {
                        "type": "object",
                        "properties": {
                            "error_type": {"type": "string", "description": "错误类型（对齐既有分类法）"},
                            "novice": {"type": "boolean", "description": "true=新手层行 / false=老手层行 / 省略=不限层兜底行"},
                            "min_confidence": {"type": "number", "description": "老手层置信度下限（0-1，novice=false 时可选，默认 0.8）"},
                            "attribution": {"type": "string", "description": "rule_missing / habit_solidified / knowledge_gap / rule_forgotten / collocation_weak"},
                            "weiner_locus": {"type": "string", "description": "internal / external，可省略"},
                            "weiner_stability": {"type": "string", "description": "stable / unstable，可省略"},
                            "weiner_controllability": {"type": "string", "description": "controllable / uncontrollable，可省略"},
                            "cefr": {"type": "string", "description": "A1/A2/B1/B2/C1/C2"},
                            "remedial": {"type": "string", "description": "补救讲解文案（决策表 remedial 输出，简洁可执行）"}
                        },
                        "required": ["error_type", "attribution", "remedial"]
                    }}
                }, "required": ["source_id", "rules"]}),
                ActionTier::Autonomous,
                Arc::new(StageRuleDrafts { ctx: ctx.clone() }),
            ),
            ManagedTool::new(
                "ingest_status",
                "查看导入会话状态（块进度 / 草稿数 / 错误记录）。",
                json!({"type": "object", "properties": {
                    "source_id": {"type": "string", "description": "省略时列出全部会话摘要"}
                }}),
                ActionTier::Autonomous,
                Arc::new(IngestStatus { ctx: ctx.clone() }),
            ),
            ManagedTool::new(
                "discard_ingest",
                "放弃一个导入会话（丢弃全部草稿；生效知识库不受影响）。",
                json!({"type": "object", "properties": {
                    "source_id": {"type": "string"}
                }, "required": ["source_id"]}),
                ActionTier::Autonomous,
                Arc::new(DiscardIngest { ctx: ctx.clone() }),
            ),
            // ---- 破坏性（Confirm：gate 拦截转提案）----
            ManagedTool::new(
                "disable_rule",
                "停用一条规则（破坏性：该规则不再检出任何错误；可在规则面板重新启用）。仅当规则误报率显著偏高时提案。",
                json!({"type": "object", "properties": {
                    "rule_id": {"type": "string"}
                }, "required": ["rule_id"]}),
                ActionTier::Confirm,
                Arc::new(DisableRule { ctx: ctx.clone() }),
            ),
            ManagedTool::new(
                "enable_rule",
                "重新启用一条已停用的规则。",
                json!({"type": "object", "properties": {
                    "rule_id": {"type": "string"}
                }, "required": ["rule_id"]}),
                ActionTier::Confirm,
                Arc::new(EnableRule { ctx: ctx.clone() }),
            ),
            ManagedTool::new(
                "remove_example",
                "从规则中删除一个示例对（wrong → right）。",
                json!({"type": "object", "properties": {
                    "rule_id": {"type": "string"},
                    "wrong": {"type": "string"},
                    "right": {"type": "string", "description": "示例对的修正侧，可选（省略时删除该 wrong 的全部示例对）"}
                }, "required": ["rule_id", "wrong"]}),
                ActionTier::Confirm,
                Arc::new(RemoveExample { ctx: ctx.clone() }),
            ),
            ManagedTool::new(
                "publish_knowledge",
                "发布导入会话：草稿归并进生效知识库（既有点追加 1:N 源背书 / 新点造点），归因富化立即生效。高风险动作：需用户确认。",
                json!({"type": "object", "properties": {
                    "source_id": {"type": "string"}
                }, "required": ["source_id"]}),
                ActionTier::Confirm,
                Arc::new(PublishKnowledge { ctx: ctx.clone() }),
            ),
            ManagedTool::new(
                "remove_knowledge",
                "删除一条学习知识条目（后续不再召回）。",
                json!({"type": "object", "properties": {
                    "id": {"type": "integer", "description": "知识条目 id（见 search_knowledge 结果）"}
                }, "required": ["id"]}),
                ActionTier::Confirm,
                Arc::new(RemoveKnowledge { ctx }),
            ),
        ]
    }
}

// ---------------------------------------------------------------------------
// 执行器（与 api-server 版同构，捕获 Core 句柄）
// ---------------------------------------------------------------------------

struct ListRules {
    ctx: AgentToolCtx,
}
#[async_trait]
impl AgentToolExecutor for ListRules {
    async fn execute(&self, params: Value) -> Result<String, String> {
        let category = params
            .get("category")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let q = params.get("q").and_then(|v| v.as_str()).unwrap_or("");
        let list = self
            .ctx
            .core
            .lexicon_store
            .list_rules(
                &self.ctx.lang,
                if category.is_empty() {
                    None
                } else {
                    Some(category)
                },
                if q.is_empty() { None } else { Some(q) },
                1,
                100,
            )
            .map_err(|e| e.to_string())?;
        let rules: Vec<Value> = list
            .rules
            .iter()
            .map(|r| {
                json!({
                    "id": r.def.id,
                    "category": r.def.category,
                    "enabled": r.def.enabled,
                    "description": r.def.description,
                    "example_count": r.example_count,
                })
            })
            .collect();
        Ok(json!({ "total": list.total, "rules": rules }).to_string())
    }
}

struct GetRule {
    ctx: AgentToolCtx,
}
#[async_trait]
impl AgentToolExecutor for GetRule {
    async fn execute(&self, params: Value) -> Result<String, String> {
        let rule_id = params.get("rule_id").and_then(|v| v.as_str()).unwrap_or("");
        if rule_id.is_empty() {
            return Err("rule_id 不能为空".into());
        }
        let def = self
            .ctx
            .core
            .lexicon_store
            .get_rule(&self.ctx.lang, rule_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("规则 {rule_id} 不存在（用 list_rules 查看可用规则）"))?;
        let examples = self
            .ctx
            .core
            .lexicon_store
            .list_rule_examples(&self.ctx.lang, rule_id, 1, 10)
            .map_err(|e| e.to_string())?;
        let lexicons_dir = {
            let s = self.ctx.core.settings.read().unwrap();
            std::path::PathBuf::from(s.lexicons_dir.clone())
        };
        let excludes = file_rule_excludes(&lexicons_dir, &self.ctx.lang, rule_id);
        Ok(json!({
            "id": def.id,
            "category": def.category,
            "enabled": def.enabled,
            "description": def.description,
            "source": def.source,
            "example_count": examples.total,
            "examples": examples.examples.iter().map(|e| json!({"wrong": e.wrong, "right": e.right, "source": e.source})).collect::<Vec<_>>(),
            "excludes": excludes,
        })
        .to_string())
    }
}

/// 读词表文件中该规则的排除词名单（文件侧数据，store 不承载）
fn file_rule_excludes(dir: &std::path::Path, lang: &str, rule_id: &str) -> Vec<String> {
    let file = dir.join(format!("{lang}_gec_lexicon.json"));
    let content = match std::fs::read_to_string(&file) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let v: Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    v.get("rules")
        .and_then(|r| r.as_array())
        .and_then(|rules| {
            rules
                .iter()
                .find(|r| r.get("id").and_then(|v| v.as_str()) == Some(rule_id))
        })
        .and_then(|r| r.get("excludes"))
        .and_then(|e| e.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|w| w.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

struct SearchKnowledge {
    ctx: AgentToolCtx,
}
#[async_trait]
impl AgentToolExecutor for SearchKnowledge {
    async fn execute(&self, params: Value) -> Result<String, String> {
        let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
        let all = self
            .ctx
            .core
            .learned_knowledge
            .list(&self.ctx.lang)
            .map_err(|e| e.to_string())?;
        let items: Vec<Value> = all
            .iter()
            .filter(|k| query.is_empty() || k.text.contains(query))
            .take(limit)
            .map(|k| {
                json!({
                    "id": k.id,
                    "text": k.text,
                    "error_type": k.error_type,
                    "correction": k.correction,
                    "source": k.source,
                })
            })
            .collect();
        Ok(json!({ "total": all.len(), "items": items }).to_string())
    }
}

struct FeedbackStats {
    ctx: AgentToolCtx,
}
#[async_trait]
impl AgentToolExecutor for FeedbackStats {
    async fn execute(&self, _params: Value) -> Result<String, String> {
        let stats = self
            .ctx
            .core
            .feedback_log
            .stats(&self.ctx.lang)
            .map_err(|e| e.to_string())?;
        let items: Vec<Value> = stats
            .iter()
            .map(|s| {
                json!({
                    "source": s.source,
                    "rule_id": s.rule_id,
                    "adopted": s.adopted,
                    "rejected": s.rejected,
                    "last_at": s.last_at,
                })
            })
            .collect();
        Ok(json!({ "stats": items }).to_string())
    }
}

struct AddExcludeWord {
    ctx: AgentToolCtx,
}
#[async_trait]
impl AgentToolExecutor for AddExcludeWord {
    async fn execute(&self, params: Value) -> Result<String, String> {
        let rule_id = params.get("rule_id").and_then(|v| v.as_str()).unwrap_or("");
        let word = params.get("word").and_then(|v| v.as_str()).unwrap_or("");
        if rule_id.is_empty() || word.is_empty() {
            return Err("rule_id 与 word 均不能为空".into());
        }
        let settings = self.ctx.core.settings.read().unwrap().clone();
        let lexicons_dir = std::path::PathBuf::from(settings.lexicons_dir.clone());
        let added = append_rule_exclude(&lexicons_dir, &self.ctx.lang, rule_id, word)
            .map_err(|e| e.to_string())?;
        if added {
            self.ctx.core.rebuild_router(&settings);
        }
        Ok(if added {
            format!("「{word}」已加入规则 {rule_id} 排除名单（该词不再触发此规则，检测器已重建）")
        } else {
            format!("「{word}」已在规则 {rule_id} 排除名单中")
        })
    }
}

struct RemoveExcludeWord {
    ctx: AgentToolCtx,
}
#[async_trait]
impl AgentToolExecutor for RemoveExcludeWord {
    async fn execute(&self, params: Value) -> Result<String, String> {
        let rule_id = params.get("rule_id").and_then(|v| v.as_str()).unwrap_or("");
        let word = params.get("word").and_then(|v| v.as_str()).unwrap_or("");
        if rule_id.is_empty() || word.is_empty() {
            return Err("rule_id 与 word 均不能为空".into());
        }
        let settings = self.ctx.core.settings.read().unwrap().clone();
        let lexicons_dir = std::path::PathBuf::from(settings.lexicons_dir.clone());
        let removed = remove_rule_exclude(&lexicons_dir, &self.ctx.lang, rule_id, word)
            .map_err(|e| e.to_string())?;
        if removed {
            self.ctx.core.rebuild_router(&settings);
        }
        Ok(if removed {
            format!("「{word}」已从规则 {rule_id} 排除名单移除")
        } else {
            format!("「{word}」不在规则 {rule_id} 排除名单中")
        })
    }
}

struct AddL3Negative {
    ctx: AgentToolCtx,
}
#[async_trait]
impl AgentToolExecutor for AddL3Negative {
    async fn execute(&self, params: Value) -> Result<String, String> {
        let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");
        if text.is_empty() {
            return Err("text 不能为空".into());
        }
        if !matches!(self.ctx.lang.as_str(), "en" | "zh") {
            return Err(format!("L3 飞轮暂不支持语言: {}", self.ctx.lang));
        }
        let error_type = params
            .get("error_type")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let sample = json!({
            "text": text,
            "label": 0,
            "error_type": error_type,
            "source": "agent_negative",
            "ts": chrono::Utc::now().to_rfc3339(),
        });
        match crate::commands::l3_collect_sample(&self.ctx.core.l3_data, &self.ctx.lang, &sample) {
            Some(total) => Ok(format!(
                "「{text}」已汇入 L3 飞轮负样本（当前共 {total} 条）"
            )),
            None => Ok(format!("「{text}」已在 L3 飞轮负样本中（去重）")),
        }
    }
}

struct AddKnowledge {
    ctx: AgentToolCtx,
}
#[async_trait]
impl AgentToolExecutor for AddKnowledge {
    async fn execute(&self, params: Value) -> Result<String, String> {
        let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");
        let error_type = params
            .get("error_type")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if text.is_empty() || error_type.is_empty() {
            return Err("text 与 error_type 均不能为空".into());
        }
        let correction = params
            .get("correction")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let id = self
            .ctx
            .core
            .learned_knowledge
            .insert(&self.ctx.lang, text, error_type, correction, &[], "agent")
            .map_err(|e| e.to_string())?;
        Ok(if id > 0 {
            format!("知识条目 #{id} 已添加（后续检测可召回）")
        } else {
            "该知识条目已存在（去重）".to_string()
        })
    }
}

struct DisableRule {
    ctx: AgentToolCtx,
}
#[async_trait]
impl AgentToolExecutor for DisableRule {
    async fn execute(&self, params: Value) -> Result<String, String> {
        let rule_id = params.get("rule_id").and_then(|v| v.as_str()).unwrap_or("");
        if rule_id.is_empty() {
            return Err("rule_id 不能为空".into());
        }
        // 只写 store（`enabled` 以 store 为权威）：检测路径经 get_rules 从
        // store 过滤禁用规则，停用即时生效，无需改词表文件 / 重建检测器
        self.ctx
            .core
            .lexicon_store
            .toggle_rule(&self.ctx.lang, rule_id, false)
            .map_err(|e| e.to_string())?;
        Ok(format!("规则 {rule_id} 已停用（可在规则面板重新启用）"))
    }
}

struct EnableRule {
    ctx: AgentToolCtx,
}
#[async_trait]
impl AgentToolExecutor for EnableRule {
    async fn execute(&self, params: Value) -> Result<String, String> {
        let rule_id = params.get("rule_id").and_then(|v| v.as_str()).unwrap_or("");
        if rule_id.is_empty() {
            return Err("rule_id 不能为空".into());
        }
        // 同 DisableRule：只写 store，检测路径从 store 读开关，即时生效
        self.ctx
            .core
            .lexicon_store
            .toggle_rule(&self.ctx.lang, rule_id, true)
            .map_err(|e| e.to_string())?;
        Ok(format!("规则 {rule_id} 已重新启用"))
    }
}

struct RemoveExample {
    ctx: AgentToolCtx,
}
#[async_trait]
impl AgentToolExecutor for RemoveExample {
    async fn execute(&self, params: Value) -> Result<String, String> {
        let rule_id = params.get("rule_id").and_then(|v| v.as_str()).unwrap_or("");
        let wrong = params.get("wrong").and_then(|v| v.as_str()).unwrap_or("");
        let right = params.get("right").and_then(|v| v.as_str()).unwrap_or("");
        if rule_id.is_empty() || wrong.is_empty() {
            return Err("rule_id 与 wrong 均不能为空".into());
        }
        // `right` 省略时按 wrong 兜底删除该 wrong 的全部示例对
        // （四元组精确匹配在修正侧未知时几乎必然失配，会误导模型）
        let deleted = if right.is_empty() {
            self.ctx
                .core
                .lexicon_store
                .delete_example_by_wrong(&self.ctx.lang, rule_id, wrong)
                .map_err(|e| e.to_string())?
        } else {
            if self
                .ctx
                .core
                .lexicon_store
                .delete_example(&self.ctx.lang, rule_id, wrong, right)
                .map_err(|e| e.to_string())?
            {
                1
            } else {
                0
            }
        };
        Ok(if deleted > 0 {
            format!("示例「{wrong}」已从规则 {rule_id} 删除（共 {deleted} 条）")
        } else {
            format!("示例「{wrong}」不在规则 {rule_id} 中（可能已被删除）")
        })
    }
}

struct RemoveKnowledge {
    ctx: AgentToolCtx,
}
#[async_trait]
impl AgentToolExecutor for RemoveKnowledge {
    async fn execute(&self, params: Value) -> Result<String, String> {
        let id = params.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
        if id <= 0 {
            return Err("id 必须为正整数".into());
        }
        let removed = self
            .ctx
            .core
            .learned_knowledge
            .remove(id)
            .map_err(|e| e.to_string())?;
        Ok(if removed {
            format!("知识条目 #{id} 已删除（后续不再召回）")
        } else {
            format!("知识条目 #{id} 不存在（可能已被删除）")
        })
    }
}

// ---------------------------------------------------------------------------
// 知识源导入工具（草稿区操作 + 发布；与 api-server agent_tools 同源）
// ---------------------------------------------------------------------------

/// 发布一个导入会话（确认后执行；与 api-server `publish_session` 同源语义）
/// 发布后反哺：新点改错对 → `lexicon_add` 建议队列（与 API 端同源逻辑；
/// 幂等去重 pending；失败不阻断发布）
pub(crate) fn stage_lexicon_add_suggestions(
    core: &Core,
    source_id: &str,
    plan: &attribution_engine::ingest::MergePlan,
) -> Result<usize, String> {
    use gec_core::reflection_center::suggestion_kinds::LEXICON_ADD;
    let lang = plan
        .new_points
        .first()
        .map(|p| p.lang.clone())
        .unwrap_or_default();
    if lang.is_empty() {
        return Ok(0);
    }
    let pending = core
        .reflection_center
        .list_suggestions(Some("pending"), Some(&lang), 500)
        .map_err(|e| e.to_string())?;
    let mut seen: std::collections::HashSet<(String, String)> = pending
        .iter()
        .filter(|s| s.kind == LEXICON_ADD)
        .map(|s| {
            (
                s.payload
                    .get("wrong")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                s.payload
                    .get("right")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            )
        })
        .collect();
    let mut seeds: Vec<lexicon_store::SuggestionSeed> = Vec::new();
    for p in &plan.new_points {
        for c in &p.corrections {
            let key = (c.wrong.trim().to_lowercase(), c.right.trim().to_lowercase());
            if key.0.is_empty() || key.1.is_empty() || seen.contains(&key) {
                continue;
            }
            seen.insert(key);
            seeds.push(lexicon_store::SuggestionSeed {
                kind: LEXICON_ADD.into(),
                target: p.point_id.clone(),
                summary: format!("改错对：{} → {}（知识源 {source_id}）", c.wrong, c.right),
                evidence: vec![format!("知识源 {source_id} 发布新点 {}", p.point_id)],
                payload: serde_json::json!({
                    "lang": p.lang,
                    "point_id": p.point_id,
                    "error_type": p.error_type,
                    "wrong": c.wrong,
                    "right": c.right,
                    "source_id": source_id,
                }),
                priority: 1,
            });
        }
    }
    if seeds.is_empty() {
        return Ok(0);
    }
    let origin = serde_json::json!({ "source": "knowledge_ingest", "source_id": source_id });
    core.reflection_center
        .save_report(
            &lang,
            &chrono::Utc::now().to_rfc3339(),
            &origin.to_string(),
            &seeds,
        )
        .map_err(|e| e.to_string())?;
    Ok(seeds.len())
}

pub(crate) fn publish_session(
    core: &Core,
    source_id: &str,
) -> Result<attribution_engine::ingest::ApplyReport, String> {
    let session = core
        .ingest
        .session(source_id)
        .ok_or_else(|| format!("导入会话不存在: {source_id}"))?;
    let drafts: Vec<attribution_engine::ingest::DraftPoint> =
        session.drafts.iter().map(|d| d.draft.clone()).collect();
    if drafts.is_empty() {
        return Err(format!("会话 {source_id} 没有草稿可发布"));
    }
    let plan = attribution_engine::ingest::plan_merge(&core.knowledge, &session.source, &drafts);

    // ⓪ 预检：规则草稿合并结果先算出并通过引擎校验，再开始任何落盘（与 api-server 同构）
    let rule_drafts: Vec<attribution_engine::ingest::DraftRule> = session
        .rule_drafts
        .iter()
        .map(|d| d.draft.clone())
        .collect();
    let mut table_updates: Vec<(String, String, attribution_engine::ingest::RulePlan)> = Vec::new();
    if !rule_drafts.is_empty() {
        let lang = session.source.lang.clone();
        let raw = core
            .tables
            .get_raw(&lang)
            .ok_or_else(|| format!("语言 {lang} 无决策表，无法应用派生规则"))?;
        let rule_plan =
            attribution_engine::ingest::plan_rules(&raw, &lang, source_id, &rule_drafts)
                .map_err(|e| format!("规则归并计划失败: {e}"))?;
        if rule_plan.new + rule_plan.replaced > 0 {
            let mut jdm: serde_json::Value =
                serde_json::from_str(&raw).map_err(|e| e.to_string())?;
            let rows: Vec<serde_json::Value> = rule_plan
                .rows
                .iter()
                .filter(|r| r.action != attribution_engine::ingest::RULE_CONFLICT)
                .map(|r| r.row.clone())
                .collect();
            attribution_engine::merge_tagged_rows(&mut jdm, &rows, source_id);
            let json = serde_json::to_string(&jdm).map_err(|e| e.to_string())?;
            attribution_engine::AttributionEngine::validate(&json)
                .map_err(|e| format!("决策表合并结果校验未通过（{lang}），中止发布: {e}"))?;
            table_updates.push((lang, json, rule_plan));
        }
    }

    // ① 知识点归并发布（知识库热更新）
    let mut report =
        attribution_engine::ingest::apply_plan(&core.knowledge, &session.source, &plan)?;
    // 合并成果写回知识库目录（跨重启存活；失败不阻断发布，内存已生效 + 告警）
    if let Err(e) = core.knowledge.save_to_dir(&core.knowledge_dir) {
        tracing::warn!(error = %e, "知识库写回失败（内存已更新，重启后将丢失）");
    }
    // 反哺闭环：新点改错对 → lexicon_add 建议队列（人工采纳才生效；失败不阻断发布）
    match stage_lexicon_add_suggestions(core, &report.source_id, &plan) {
        Ok(n) if n > 0 => tracing::info!(suggestions = n, "改错对已汇入建议队列（待人工采纳）"),
        Err(e) => tracing::warn!(error = %e, "改错对建议入队失败（不影响发布）"),
        _ => {}
    }

    // ② 决策表规则应用（预检已通过；校验 + 原子写回文件跨重启存活）
    for (lang_t, json, rule_plan) in &table_updates {
        core.tables
            .update(lang_t, json)
            .map_err(|e| e.to_string())?;
        let path = crate::commands::tables_file_path(core, lang_t);
        if let Err(e) = crate::commands::pack_atomic_write(&path, json) {
            tracing::warn!(lang = %lang_t, error = %e, "决策表写回文件失败（内存已生效，重启后回退）");
        }
        report.rules_new = rule_plan.new;
        report.rules_replaced = rule_plan.replaced;
        report.rules_conflicts = rule_plan.conflicts;
    }

    let _ = core.ingest.discard(source_id);
    let staged_file = core.ingest_dir.join(format!("{source_id}.json"));
    if staged_file.exists() {
        let _ = std::fs::remove_file(&staged_file);
    }
    tracing::info!(
        source_id = %report.source_id,
        new_points = report.new_points,
        endorsed = report.endorsed,
        "知识源已发布（知识库热更新，归因富化即时生效）"
    );
    Ok(report)
}

struct ListKnowledgePoints {
    ctx: AgentToolCtx,
}
#[async_trait]
impl AgentToolExecutor for ListKnowledgePoints {
    async fn execute(&self, params: Value) -> Result<String, String> {
        let error_type = params
            .get("error_type")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let q = params.get("q").and_then(|v| v.as_str()).unwrap_or("");
        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(50)
            .clamp(1, 200) as usize;
        let all = self.ctx.core.knowledge.points(&self.ctx.lang, None);
        let matched: Vec<Value> = all
            .iter()
            .filter(|p| {
                (error_type.is_empty() || p.error_type == error_type)
                    && (q.is_empty()
                        || p.title.to_lowercase().contains(&q.to_lowercase())
                        || p.point_id.to_lowercase().contains(&q.to_lowercase()))
            })
            .take(limit)
            .map(|p| {
                json!({
                    "point_id": p.point_id,
                    "error_type": p.error_type,
                    "section": p.section,
                    "title": p.title,
                    "cefr": p.cefr,
                    "sources": p.sources.iter().map(|s| s.source_id.clone()).collect::<Vec<_>>(),
                })
            })
            .collect();
        Ok(json!({
            "total_in_kb": all.len(),
            "returned": matched.len(),
            "points": matched,
        })
        .to_string())
    }
}

struct RegisterIngestSource {
    ctx: AgentToolCtx,
}
#[async_trait]
impl AgentToolExecutor for RegisterIngestSource {
    async fn execute(&self, params: Value) -> Result<String, String> {
        let source = shared_model::KnowledgeSource {
            source_id: params
                .get("source_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            title: params
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            author: params
                .get("author")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            edition: params
                .get("edition")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            lang: self.ctx.lang.clone(),
            license_scope: params
                .get("license_scope")
                .and_then(|v| v.as_str())
                .unwrap_or("internal_test")
                .to_string(),
            point_count: 0,
        };
        let summary = self.ctx.core.ingest.open_session(source, 0)?;
        Ok(format!(
            "导入会话已开启: {}（{}，lang={}）",
            summary.source_id, summary.title, summary.lang
        ))
    }
}

struct StagePoints {
    ctx: AgentToolCtx,
}
#[async_trait]
impl AgentToolExecutor for StagePoints {
    async fn execute(&self, params: Value) -> Result<String, String> {
        let source_id = params
            .get("source_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if source_id.is_empty() {
            return Err("source_id 不能为空".into());
        }
        let points: Vec<attribution_engine::ingest::DraftPoint> = serde_json::from_value(
            params
                .get("points")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        )
        .map_err(|e| format!("points 参数解析失败: {e}"))?;
        let report = self.ctx.core.ingest.stage(source_id, points)?;
        let _ = self.ctx.core.ingest.save_to_dir(&self.ctx.core.ingest_dir);
        Ok(serde_json::to_string(&report).unwrap_or_default())
    }
}

struct StageRuleDrafts {
    ctx: AgentToolCtx,
}
#[async_trait]
impl AgentToolExecutor for StageRuleDrafts {
    async fn execute(&self, params: Value) -> Result<String, String> {
        let source_id = params
            .get("source_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if source_id.is_empty() {
            return Err("source_id 不能为空".into());
        }
        let rules: Vec<attribution_engine::ingest::DraftRule> = serde_json::from_value(
            params
                .get("rules")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        )
        .map_err(|e| format!("rules 参数解析失败: {e}"))?;
        let report = self.ctx.core.ingest.stage_rules(source_id, rules)?;
        let _ = self.ctx.core.ingest.save_to_dir(&self.ctx.core.ingest_dir);
        Ok(serde_json::to_string(&report).unwrap_or_default())
    }
}

struct IngestStatus {
    ctx: AgentToolCtx,
}
#[async_trait]
impl AgentToolExecutor for IngestStatus {
    async fn execute(&self, params: Value) -> Result<String, String> {
        let source_id = params
            .get("source_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if source_id.is_empty() {
            return Ok(serde_json::to_string(&self.ctx.core.ingest.list()).unwrap_or_default());
        }
        let session = self
            .ctx
            .core
            .ingest
            .session(source_id)
            .ok_or_else(|| format!("导入会话不存在: {source_id}"))?;
        Ok(json!({
            "source_id": session.source.source_id,
            "title": session.source.title,
            "status": session.status,
            "chunks_done": session.chunks_done,
            "chunks_total": session.chunks_total,
            "chunks_failed": session.chunks_failed,
            "staged_points": session.drafts.len(),
            "errors": session.errors,
        })
        .to_string())
    }
}

struct DiscardIngest {
    ctx: AgentToolCtx,
}
#[async_trait]
impl AgentToolExecutor for DiscardIngest {
    async fn execute(&self, params: Value) -> Result<String, String> {
        let source_id = params
            .get("source_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let dropped = self.ctx.core.ingest.discard(source_id)?;
        let staged_file = self.ctx.core.ingest_dir.join(format!("{source_id}.json"));
        if staged_file.exists() {
            let _ = std::fs::remove_file(&staged_file);
        }
        Ok(format!(
            "导入会话 {source_id} 已放弃（丢弃草稿 {dropped} 条）"
        ))
    }
}

struct PublishKnowledge {
    ctx: AgentToolCtx,
}
#[async_trait]
impl AgentToolExecutor for PublishKnowledge {
    async fn execute(&self, params: Value) -> Result<String, String> {
        let source_id = params
            .get("source_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let report = publish_session(&self.ctx.core, source_id)?;
        Ok(format!(
            "知识源 {} 已发布：新点 {}，既有点追加背书 {}，已覆盖跳过 {}，{} 知识点共 {} 个（归因富化即时生效）",
            report.source_id,
            report.new_points,
            report.endorsed,
            report.already_covered,
            report.lang,
            report.total_points
        ))
    }
}
