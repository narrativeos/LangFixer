//! 异常模式蒸馏（自学习飞轮二期：LLM 教师 → 模式归纳 → 金集评估门槛 → 词表沉淀）
//!
//! 一期（冷启动）用种子模式覆盖已知污染形态；二期闭环：
//! 1. 用户在检测页「确认污染」（LLM 兜底检出的 anomaly 被采纳）；
//! 2. **蒸馏**：LLM 从确认实例 + 现有模式 + 金集（正/负例）归纳一条新
//!    `AnomalyPattern`（正则 + 强/弱分级 + 说明）；
//! 3. **评估门槛**（本模块 [`evaluate_candidate`]，确定性、可回归）：
//!    模式可编译 + 必须命中确认实例 + **金集负例零误报**（硬门槛，
//!    重点防「作家角色」合法讨论文风的对话陷阱）；金集正例召回率仅报告；
//! 4. **沉淀**：过门槛 → 写回词表文件（[`append_pattern_to_lexicon`]，
//!    检测器重建后即时生效）；不过门槛 / LLM 不可用 → 降级知识分支
//!    （②.5 向量实例召回，由 UI 让用户选择保留/放弃）。
//!
//! 金集（[`GoldSet`]）默认路径 `datasets/anomaly_gold.json`
//! （环境变量 `GEC_ANOMALY_GOLD` 可覆盖）；它同时是一期种子模式的
//! 回归网（见测试 `seed_patterns_pass_gold_gate`）。

use std::path::{Path, PathBuf};

use gec_core::LlmGecEngine;
use serde::{Deserialize, Serialize};

use crate::rules::{AnomalyPattern, Lexicon};

/// 金集条目：一句独立判定的文本 + 说明
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoldItem {
    /// 句子文本（按「独立首句」语义评估：句首锚定生效，前窗为空）
    pub text: String,
    /// 条目说明（进入蒸馏提示词与门槛报告，帮助 LLM / 人类理解边界）
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

/// 异常金集（模式蒸馏的评估基准 + 蒸馏提示词的边界教材）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoldSet {
    pub lang: String,
    /// 金集版本（如 "1.0"；扩充后递增）
    #[serde(default)]
    pub version: String,
    /// 正例：已知的 AI 污染句（助手回复 / 元讨论混入正文）
    pub positives: Vec<GoldItem>,
    /// 负例：合法文本（正常叙述 + 「作家角色」讨论文风的对话陷阱）
    pub negatives: Vec<GoldItem>,
}

impl GoldSet {
    /// 默认金集路径：`GEC_ANOMALY_GOLD` 环境变量 > `datasets/anomaly_gold.json`
    pub fn default_path() -> PathBuf {
        std::env::var("GEC_ANOMALY_GOLD")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("datasets/anomaly_gold.json"))
    }

    /// 从 JSON 文件加载金集
    pub fn load(path: &Path) -> std::io::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        serde_json::from_str(&content)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
}

/// 候选模式评估门槛报告
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateReport {
    /// 正则是否编译通过
    pub compiles: bool,
    /// 是否命中确认实例（必须）
    pub matches_instance: bool,
    /// 金集负例误报（硬门槛：必须为空）
    pub fp_negatives: Vec<String>,
    /// 金集正例命中数（报告用；新子族模式可能只覆盖实例本身）
    pub tp_positives: usize,
    /// 金集正例总数
    pub total_positives: usize,
    /// 是否通过门槛（`compiles && matches_instance && fp_negatives.is_empty()`）
    pub accepted: bool,
    /// 门槛结论说明（通过 / 拒绝原因）
    pub reason: String,
}

/// 评估候选模式（确定性门槛；与 [`crate::rules::detect_anomalies`] 同语义：
/// 金集条目按「独立首句」判定——句首锚定生效、前窗为空故引号守卫不触发）
pub fn evaluate_candidate(p: &AnomalyPattern, gold: &GoldSet, instance: &str) -> GateReport {
    let re = match regex::Regex::new(&p.pattern) {
        Ok(re) => re,
        Err(e) => {
            return GateReport {
                compiles: false,
                matches_instance: false,
                fp_negatives: Vec::new(),
                tp_positives: 0,
                total_positives: gold.positives.len(),
                accepted: false,
                reason: format!("正则编译失败：{e}"),
            };
        }
    };
    let matches_instance = re.is_match(instance);
    let fp_negatives: Vec<String> = gold
        .negatives
        .iter()
        .filter(|n| re.is_match(&n.text))
        .map(|n| n.text.clone())
        .collect();
    let tp_positives = gold
        .positives
        .iter()
        .filter(|n| re.is_match(&n.text))
        .count();
    let accepted = matches_instance && fp_negatives.is_empty();
    let reason = if !matches_instance {
        "模式未命中确认实例".to_string()
    } else if !fp_negatives.is_empty() {
        format!(
            "模式误报 {} 条金集负例（合法文本被误判为污染）",
            fp_negatives.len()
        )
    } else {
        format!(
            "通过门槛：命中确认实例，金集负例零误报（正例覆盖 {tp_positives}/{}）",
            gold.positives.len()
        )
    };
    GateReport {
        compiles: true,
        matches_instance,
        fp_negatives,
        tp_positives,
        total_positives: gold.positives.len(),
        accepted,
        reason,
    }
}

/// 从 LLM 蒸馏响应中解析候选模式（容错：剥离 ```json 围栏 / 前后杂散文本，
/// 取首个 JSON 对象；字段与 [`AnomalyPattern`] 一致）
pub fn parse_distill_response(content: &str) -> Option<AnomalyPattern> {
    let t = content.trim();
    let t = t
        .strip_prefix("```json")
        .or_else(|| t.strip_prefix("```"))
        .unwrap_or(t)
        .trim();
    let t = t.strip_suffix("```").map(str::trim).unwrap_or(t);
    let start = t.find('{')?;
    let end = t.rfind('}')?;
    if end <= start {
        return None;
    }
    let p: AnomalyPattern = serde_json::from_str(&t[start..=end]).ok()?;
    if p.pattern.is_empty() {
        return None;
    }
    Some(p)
}

/// 渲染蒸馏 user 提示词（`{{var}}` 占位符，与 `PromptTemplate::render` 同语义）
///
/// 变量：`{{instance}}`（确认实例）/ `{{existing_patterns}}`（现有模式 JSON）/
/// `{{gold_positives}}` / `{{gold_negatives}}`（金集编号列表，含 note）
pub fn build_distill_user_prompt(
    user_template: &str,
    instance: &str,
    existing: &[AnomalyPattern],
    gold: &GoldSet,
) -> String {
    let existing_json = serde_json::to_string_pretty(existing).unwrap_or_else(|_| "[]".into());
    let mut pos = String::new();
    for (i, it) in gold.positives.iter().enumerate() {
        pos.push_str(&format!("{}. {}\n", i + 1, it.text));
        if !it.note.is_empty() {
            pos.push_str(&format!("   （{}）\n", it.note));
        }
    }
    let mut neg = String::new();
    for (i, it) in gold.negatives.iter().enumerate() {
        neg.push_str(&format!("{}. {}\n", i + 1, it.text));
        if !it.note.is_empty() {
            neg.push_str(&format!("   （{}）\n", it.note));
        }
    }
    let mut out = user_template.to_string();
    for (k, v) in [
        ("instance", instance),
        ("existing_patterns", existing_json.as_str()),
        ("gold_positives", pos.as_str()),
        ("gold_negatives", neg.as_str()),
    ] {
        out = out.replace(&format!("{{{{{}}}}}", k), v);
    }
    out
}

/// 调用 LLM 蒸馏器归纳候选模式（渲染提示词 → chat completion → 解析）
///
/// 返回解析出的候选（**未经门槛评估**；调用方用 [`evaluate_candidate`] 判定）。
/// LLM 调用失败 / 响应无法解析时返回 `Err`（调用方降级知识分支）。
pub async fn distill_pattern(
    llm: &LlmGecEngine,
    system: &str,
    user_template: &str,
    instance: &str,
    existing: &[AnomalyPattern],
    gold: &GoldSet,
) -> Result<AnomalyPattern, String> {
    let user = build_distill_user_prompt(user_template, instance, existing, gold);
    // 只取最终答案：蒸馏响应是 JSON 对象，思考块拼在前面会污染解析
    let content = llm
        .chat_completion_answer(system, &user)
        .await
        .map_err(|e| format!("蒸馏 LLM 调用失败：{e}"))?;
    parse_distill_response(&content)
        .ok_or_else(|| "蒸馏响应无法解析为模式 JSON（{content}）".to_string())
}

/// 将模式写回词表文件（追加到指定 `anomaly` 规则的 `patterns` 数组）
///
/// - 规则不存在时创建最小 anomaly 规则条目；
/// - 同 pattern 字符串已存在 → 不重复写入（返回 `Ok(false)`）；
/// - 原子写回（临时文件 + rename，与 `sync_rules_to_file` 一致）。
///
/// 返回 `Ok(true)` = 已写入新模式。
pub fn append_pattern_to_lexicon(
    dir: &Path,
    lang: &str,
    rule_id: &str,
    p: &AnomalyPattern,
) -> std::io::Result<bool> {
    let file = dir.join(format!("{lang}_gec_lexicon.json"));
    let content = std::fs::read_to_string(&file).map_err(|e| {
        std::io::Error::new(
            e.kind(),
            format!("词表文件读取失败（{}）: {e}", file.display()),
        )
    })?;
    let mut v: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let rules = v
        .get_mut("rules")
        .and_then(|r| r.as_array_mut())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("词表文件缺少 rules 数组: {}", file.display()),
            )
        })?;
    let p_json = serde_json::to_value(p)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    // 定位目标规则；不存在则创建最小 anomaly 规则
    let rule = match rules
        .iter_mut()
        .find(|r| r.get("id").and_then(|v| v.as_str()) == Some(rule_id))
    {
        Some(r) => r,
        None => {
            rules.push(serde_json::json!({
                "id": rule_id,
                "category": "anomaly",
                "priority": 20,
                "enabled": true,
                "description": "AI 生成文本污染（自动蒸馏模式）",
                "patterns": []
            }));
            rules.last_mut().unwrap()
        }
    };
    let patterns = rule
        .get_mut("patterns")
        .and_then(|p| p.as_array_mut())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("规则 {rule_id} 缺少 patterns 数组"),
            )
        })?;
    if patterns
        .iter()
        .any(|x| x.get("pattern").and_then(|v| v.as_str()) == Some(p.pattern.as_str()))
    {
        return Ok(false); // 已存在，不重复写入
    }
    patterns.push(p_json);
    let tmp = dir.join(format!(".{lang}_gec_lexicon.json.tmp"));
    let out = serde_json::to_string_pretty(&v)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&tmp, out)?;
    std::fs::rename(&tmp, &file)?;
    Ok(true)
}

/// 词表中第一个 `anomaly` 分类规则的 id（蒸馏模式的目标规则）
pub fn anomaly_rule_id(lexicon: &Lexicon) -> Option<&str> {
    lexicon
        .rules
        .iter()
        .find(|r| r.category == "anomaly")
        .map(|r| r.id.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gold() -> GoldSet {
        GoldSet {
            lang: "zh".into(),
            version: "1.0".into(),
            positives: vec![
                GoldItem {
                    text: "需要我再微调部分语句，让整体文风更贴合悬疑灵异的氛围吗?".into(),
                    note: "服务性问句".into(),
                },
                GoldItem {
                    text: "好的，我已经帮你把第三章的结尾改得更紧凑了。".into(),
                    note: "确认话术".into(),
                },
                GoldItem {
                    text: "这段内容已经按照你的要求生成完毕，如有其他需要随时提出。".into(),
                    note: "新子族：交付话术".into(),
                },
            ],
            negatives: vec![
                GoldItem {
                    text: "他说：\"要不要再润色一下这段？\"她点点头，没再多说。".into(),
                    note: "作家角色讨论文风（对话内）".into(),
                },
                GoldItem {
                    text: "\"好的，我已经帮你改好了。\"她说完，转身离开了房间。".into(),
                    note: "对话陷阱：句首引号".into(),
                },
                GoldItem {
                    text: "夜色从窗缝里渗进来，落在摊开的稿纸上。".into(),
                    note: "正常叙述".into(),
                },
            ],
        }
    }

    #[test]
    fn gate_accepts_specific_pattern() {
        let g = gold();
        let p = AnomalyPattern {
            pattern: "^需要我再微调".into(),
            strong: true,
            note: "服务性问句".into(),
        };
        let r = evaluate_candidate(&p, &g, "需要我再微调部分语句吗?");
        assert!(
            r.compiles && r.matches_instance && r.accepted,
            "{}",
            r.reason
        );
        assert!(r.fp_negatives.is_empty());
    }

    #[test]
    fn gate_rejects_pattern_hitting_negative() {
        let g = gold();
        // 无锚定模式：会命中「对话内」的合法讨论文风
        let p = AnomalyPattern {
            pattern: "要不要再润色".into(),
            strong: false,
            note: "".into(),
        };
        let r = evaluate_candidate(&p, &g, "要不要再润色一下？");
        assert!(!r.accepted);
        assert_eq!(r.fp_negatives.len(), 1);
        assert!(r.reason.contains("误报"));
    }

    #[test]
    fn gate_rejects_uncompilable_and_unmatched() {
        let g = gold();
        let p = AnomalyPattern {
            pattern: "([".into(),
            strong: true,
            note: "".into(),
        };
        let r = evaluate_candidate(&p, &g, "x");
        assert!(!r.compiles && !r.accepted);
        assert!(r.reason.contains("编译"));

        let p2 = AnomalyPattern {
            pattern: "^不存在的开头".into(),
            strong: true,
            note: "".into(),
        };
        let r2 = evaluate_candidate(&p2, &g, "需要我再改吗?");
        assert!(!r2.matches_instance && !r2.accepted);
    }

    #[test]
    fn parse_response_fenced_and_garbage() {
        let p = parse_distill_response(
            "分析完毕。\n```json\n{\"pattern\":\"^好的，我\",\"strong\":true,\"note\":\"确认话术\"}\n```",
        )
        .unwrap();
        assert_eq!(p.pattern, "^好的，我");
        assert!(p.strong);
        assert_eq!(p.note, "确认话术");

        // strong 缺省 true
        let p2 = parse_distill_response("{\"pattern\":\"^以下是修改\"}").unwrap();
        assert!(p2.strong);

        assert!(parse_distill_response("抱歉，我无法完成该任务。").is_none());
        assert!(parse_distill_response("{\"pattern\":\"\"}").is_none());
        assert!(parse_distill_response("not json at all").is_none());
    }

    #[test]
    fn build_prompt_renders_all_vars() {
        let g = gold();
        let existing = vec![AnomalyPattern {
            pattern: "^需要我".into(),
            strong: true,
            note: "旧模式".into(),
        }];
        let out = build_distill_user_prompt(
            "实例：{{instance}}\n现有：{{existing_patterns}}\n正例：\n{{gold_positives}}\n负例：\n{{gold_negatives}}",
            "需要我再改吗?",
            &existing,
            &g,
        );
        assert!(out.contains("实例：需要我再改吗?"));
        assert!(out.contains("\"^需要我\""));
        assert!(out.contains("1. 需要我再微调部分语句"));
        assert!(out.contains("作家角色讨论文风"));
        assert!(!out.contains("{{"), "占位符应全部渲染");
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("anomaly_distill_{tag}_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn append_pattern_roundtrip_and_dedup() {
        let dir = temp_dir("append");
        std::fs::write(
            dir.join("zh_gec_lexicon.json"),
            r#"{
                "rules": [
                    {"id": "ZH_ANOMALY_ASSISTANT_REPLY", "category": "anomaly",
                     "enabled": true, "patterns": [{"pattern": "^需要我", "strong": true, "note": "旧"}]}
                ]
            }"#,
        )
        .unwrap();
        let p = AnomalyPattern {
            pattern: "^这段内容已经".into(),
            strong: true,
            note: "交付话术".into(),
        };
        assert!(append_pattern_to_lexicon(&dir, "zh", "ZH_ANOMALY_ASSISTANT_REPLY", &p).unwrap());
        // 重复写入 → false（去重）
        assert!(!append_pattern_to_lexicon(&dir, "zh", "ZH_ANOMALY_ASSISTANT_REPLY", &p).unwrap());
        let v: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("zh_gec_lexicon.json")).unwrap(),
        )
        .unwrap();
        let rules = v["rules"].as_array().unwrap();
        assert_eq!(rules.len(), 1);
        let patterns = rules[0]["patterns"].as_array().unwrap();
        assert_eq!(patterns.len(), 2, "旧模式 + 新模式");
        assert_eq!(patterns[1]["pattern"], "^这段内容已经");
        assert_eq!(patterns[0]["pattern"], "^需要我", "原有模式不被破坏");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn append_pattern_creates_rule_when_missing() {
        let dir = temp_dir("create");
        std::fs::write(dir.join("zh_gec_lexicon.json"), r#"{"rules": []}"#).unwrap();
        let p = AnomalyPattern {
            pattern: "^希望这次".into(),
            strong: false,
            note: "新子族".into(),
        };
        assert!(append_pattern_to_lexicon(&dir, "zh", "ZH_ANOMALY_NEW", &p).unwrap());
        let v: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("zh_gec_lexicon.json")).unwrap(),
        )
        .unwrap();
        let rules = v["rules"].as_array().unwrap();
        assert_eq!(rules[0]["id"], "ZH_ANOMALY_NEW");
        assert_eq!(rules[0]["category"], "anomaly");
        assert_eq!(rules[0]["patterns"][0]["pattern"], "^希望这次");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 金集回归网：仓库 `datasets/anomaly_gold.json` 必须可加载，
    /// 且一期全部种子模式对金集**负例零误报**（正例覆盖 ≥ 5 条）
    #[test]
    fn seed_patterns_pass_gold_gate() {
        let gold = GoldSet::load(Path::new("../datasets/anomaly_gold.json"))
            .expect("仓库金集文件应可加载");
        assert!(gold.positives.len() >= 5, "金集正例不足");
        assert!(gold.negatives.len() >= 5, "金集负例不足");
        let lexicon = Lexicon::default();
        let mut covered = 0usize;
        for rule in &lexicon.rules {
            if rule.category != "anomaly" {
                continue;
            }
            for p in &rule.patterns {
                let re = regex::Regex::new(&p.pattern).expect("种子模式应可编译");
                let fp: Vec<&GoldItem> = gold
                    .negatives
                    .iter()
                    .filter(|n| re.is_match(&n.text))
                    .collect();
                assert!(
                    fp.is_empty(),
                    "种子模式 {} 误报金集负例: {:?}",
                    p.pattern,
                    fp
                );
                if gold.positives.iter().any(|n| re.is_match(&n.text)) {
                    covered += 1;
                }
            }
        }
        assert!(
            covered >= 5,
            "种子模式应覆盖金集 ≥5 条正例（实际 {covered}）"
        );
    }
}
