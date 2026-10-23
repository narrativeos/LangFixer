//! 检测参数快照 + 规则侧版本指纹（api-server / 桌面端共用）
//!
//! 「重复检测复用历史」的前提：历史记录携带检测时的参数快照（[`DetectParams`]），
//! 新请求与某条同文本历史的参数逐项一致、且该历史不含 LLM 时才复用其结果。
//! 规则侧（决策表 / 词表文件 / 禁用规则 / 自定义词白名单）的任何变化都会改变
//! [`rules_fingerprint`]；决策模型（vegaml）重训练后 [`DetectParams::model_version`]
//! 变化——两者任一变化都使旧历史自动失效，复用因此天然收敛到「规则侧与模型侧
//! 都未变的短时间反复检测」场景。

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, sources};
use crate::table::DecisionTableMeta;

/// 检测参数快照（存入历史 `params_json`；重复检测复用历史的比对依据）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectParams {
    /// 三级响应级别（1/2/3）
    pub level: u8,
    /// 「漏报」强制兜底（true 时该历史必然含 LLM，不可复用）
    pub force_llm: bool,
    /// 分类过滤（None = 不过滤）
    #[serde(default)]
    pub category: Option<String>,
    /// 决策模型（vegaml）层当时是否启用
    pub decision_enabled: bool,
    /// L3 特征模型层当时是否启用
    pub l3_enabled: bool,
    /// LLM 当时是否已配置（API Key）
    pub llm_configured: bool,
    /// LLM 模型名（未配置时为 None）
    #[serde(default)]
    pub llm_model: Option<String>,
    /// 决策模型（vegaml）版本号（来自决策服务 `GET /health` 的 `versions[lang]`；
    /// 每次重训练自增。`None` = 决策层未启用，或服务不可用 / 该语言无模型（版本未知）
    /// ——版本未知时与携带版本的历史不匹配，保守不复用）
    #[serde(default)]
    pub model_version: Option<String>,
    /// 规则侧版本指纹（[`rules_fingerprint`]；旧记录缺省为空 = 不可复用）
    #[serde(default)]
    pub rules_fingerprint: String,
}

/// 稳定的 64 位 FNV-1a 哈希。
///
/// 不用 `std::hash::DefaultHasher`：它按进程随机化种子，跨进程不稳定，
/// 无法用于持久化历史匹配。
pub fn fnv1a_64(data: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in data.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// 文本的稳定哈希（16 位 hex）——历史「同文本」快速查找键
pub fn text_hash(text: &str) -> String {
    format!("{:016x}", fnv1a_64(text))
}

/// 词表目录（`{lang}_gec_lexicon.json`）的文件哈希。
///
/// 检测器只在**构造时**从词表文件加载规则，故「生效词表」由最近一次
/// 检测器构建时的文件内容决定：调用方（api-server / 桌面端）在启动与
/// 检测器热重建时计算并持有该值，检测时直接读取（不每次读盘）。
/// 目录缺失 / 无词表文件时返回 `"builtin"`（检测器回退内置默认词表）。
pub fn lexicon_files_hash(dir: &Path) -> String {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return "builtin".to_string(),
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .ends_with("_gec_lexicon.json")
        })
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    if names.is_empty() {
        return "builtin".to_string();
    }
    names.sort();
    let mut digest = String::new();
    for name in &names {
        match std::fs::read(dir.join(name)) {
            Ok(bytes) => {
                digest.push_str(name);
                digest.push(':');
                digest.push_str(&format!(
                    "{:016x}",
                    fnv1a_64(&String::from_utf8_lossy(&bytes))
                ));
                digest.push(';');
            }
            Err(_) => {
                // 文件不可读：以文件名 + 空内容参与哈希（仍随文件出现/消失变化）
                digest.push_str(name);
                digest.push_str(":0000000000000000;");
            }
        }
    }
    format!("{:016x}", fnv1a_64(&digest))
}

/// 规则侧版本指纹：决策表元数据 + 词表文件 + 禁用规则集 + 自定义词白名单。
///
/// 任一输入变化 → 指纹变化 → 依赖旧规则状态的历史结果不再可复用。
/// 集合类输入（`disabled_rules` / `custom_words`）须由调用方排序后传入。
pub fn rules_fingerprint(
    table_meta: Option<&DecisionTableMeta>,
    lexicon_hash: &str,
    disabled_rules: &[String],
    custom_words: &[String],
) -> String {
    let mut s = String::new();
    if let Some(m) = table_meta {
        s.push_str("table:");
        s.push_str(&m.updated_at);
        s.push(':');
        s.push_str(&m.rule_count.to_string());
        s.push(';');
    }
    s.push_str("lex:");
    s.push_str(lexicon_hash);
    s.push(';');
    s.push_str("disabled:");
    for id in disabled_rules {
        s.push_str(id);
        s.push(',');
    }
    s.push_str("words:");
    for w in custom_words {
        s.push_str(w);
        s.push(',');
    }
    format!("{:016x}", fnv1a_64(&s))
}

/// 历史复用判定（纯函数）——全部满足才可复用：
/// 1. `params_json` 非空且可解析（旧记录一律不可复用，保守）；
/// 2. 存储文本与当前文本一致（`text_hash` 命中后的精确校验，防哈希碰撞）；
/// 3. 参数逐项一致（level / force_llm / category / decision_enabled / l3_enabled /
///    llm_configured / **决策模型版本号** / rules_fingerprint）；
/// 4. **不含 LLM**：该记录本身是 level 1 且非强制运行，且结果中无
///    `source == "llm"` 的错误（双保险；LLM 结果非确定性，一律实时重跑）。
pub fn history_reusable(
    params_json: &str,
    stored_text: &str,
    errors_json: &str,
    current: &DetectParams,
    current_text: &str,
) -> bool {
    if params_json.is_empty() || stored_text != current_text {
        return false;
    }
    let stored: DetectParams = match serde_json::from_str(params_json) {
        Ok(p) => p,
        Err(_) => return false,
    };
    if stored.level != current.level
        || stored.force_llm != current.force_llm
        || stored.category != current.category
        || stored.decision_enabled != current.decision_enabled
        || stored.l3_enabled != current.l3_enabled
        || stored.llm_configured != current.llm_configured
        || stored.model_version != current.model_version
        || stored.rules_fingerprint != current.rules_fingerprint
    {
        return false;
    }
    if stored.level != 1 || stored.force_llm {
        return false;
    }
    match serde_json::from_str::<Vec<Error>>(errors_json) {
        Ok(errs) => !errs
            .iter()
            .any(|e| e.source.as_deref() == Some(sources::LLM)),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_params() -> DetectParams {
        DetectParams {
            level: 1,
            force_llm: false,
            category: None,
            decision_enabled: true,
            l3_enabled: true,
            llm_configured: false,
            llm_model: None,
            model_version: None,
            rules_fingerprint: "f1".into(),
        }
    }

    fn meta(updated_at: &str, rule_count: usize) -> DecisionTableMeta {
        DecisionTableMeta {
            lang: "zh".into(),
            version: "1.0.0".into(),
            updated_at: updated_at.into(),
            rule_count,
        }
    }

    #[test]
    fn fnv1a_known_vectors() {
        // FNV-1a 64 标准测试向量
        assert_eq!(format!("{:016x}", fnv1a_64("")), "cbf29ce484222325");
        assert_eq!(format!("{:016x}", fnv1a_64("a")), "af63dc4c8601ec8c");
        assert_eq!(format!("{:016x}", fnv1a_64("foobar")), "85944171f73967e8");
    }

    #[test]
    fn text_hash_stable_and_distinct() {
        assert_eq!(text_hash("他昨天报复很大。"), text_hash("他昨天报复很大。"));
        assert_ne!(text_hash("abc"), text_hash("abd"));
        assert_eq!(text_hash("abc").len(), 16);
    }

    #[test]
    fn lexicon_files_hash_builtin_when_missing() {
        assert_eq!(
            lexicon_files_hash(Path::new("/nonexistent-dir-xyz")),
            "builtin"
        );
    }

    #[test]
    fn rules_fingerprint_stable_for_same_inputs() {
        let a = rules_fingerprint(
            Some(&meta("t1", 10)),
            "lex1",
            &["r1".into()],
            &["w1".into()],
        );
        let b = rules_fingerprint(
            Some(&meta("t1", 10)),
            "lex1",
            &["r1".into()],
            &["w1".into()],
        );
        assert_eq!(a, b);
        assert_eq!(a.len(), 16);
    }

    #[test]
    fn rules_fingerprint_changes_with_any_input() {
        let base = rules_fingerprint(
            Some(&meta("t1", 10)),
            "lex1",
            &["r1".into()],
            &["w1".into()],
        );
        assert_ne!(
            base,
            rules_fingerprint(
                Some(&meta("t2", 10)),
                "lex1",
                &["r1".into()],
                &["w1".into()]
            )
        );
        assert_ne!(
            base,
            rules_fingerprint(
                Some(&meta("t1", 11)),
                "lex1",
                &["r1".into()],
                &["w1".into()]
            )
        );
        assert_ne!(
            base,
            rules_fingerprint(
                Some(&meta("t1", 10)),
                "lex2",
                &["r1".into()],
                &["w1".into()]
            )
        );
        assert_ne!(
            base,
            rules_fingerprint(
                Some(&meta("t1", 10)),
                "lex1",
                &["r2".into()],
                &["w1".into()]
            )
        );
        assert_ne!(
            base,
            rules_fingerprint(
                Some(&meta("t1", 10)),
                "lex1",
                &["r1".into()],
                &["w2".into()]
            )
        );
        assert_ne!(
            base,
            rules_fingerprint(None, "lex1", &["r1".into()], &["w1".into()])
        );
    }

    #[test]
    fn history_reusable_accepts_matching_non_llm_record() {
        let p = test_params();
        let json = serde_json::to_string(&p).unwrap();
        assert!(history_reusable(&json, "文本A", "[]", &p, "文本A"));
        let e = Error::new(
            "zh",
            "grammar",
            0.9,
            [0, 3],
            "报复",
            Some("抱负".into()),
            "错别字",
        );
        let errs = serde_json::to_string(&vec![e]).unwrap();
        assert!(history_reusable(&json, "文本A", &errs, &p, "文本A"));
    }

    #[test]
    fn history_reusable_rejects_llm_sourced_errors() {
        let p = test_params();
        let json = serde_json::to_string(&p).unwrap();
        let mut e = Error::new("zh", "grammar", 0.9, [0, 3], "报复", None, "错别字");
        e.source = Some(sources::LLM.to_string());
        let errs = serde_json::to_string(&vec![e]).unwrap();
        assert!(!history_reusable(&json, "文本A", &errs, &p, "文本A"));
    }

    #[test]
    fn history_reusable_rejects_param_mismatch_and_old_records() {
        let p = test_params();
        let json = serde_json::to_string(&p).unwrap();
        // 文本不一致
        assert!(!history_reusable(&json, "文本A", "[]", &p, "文本B"));
        // 旧记录（params_json 为空）
        assert!(!history_reusable("", "文本A", "[]", &p, "文本A"));
        // 不可解析
        assert!(!history_reusable("{bad", "文本A", "[]", &p, "文本A"));
        // level 不一致
        let mut other = test_params();
        other.level = 2;
        assert!(!history_reusable(&json, "文本A", "[]", &other, "文本A"));
        // force_llm 不一致
        let mut other = test_params();
        other.force_llm = true;
        assert!(!history_reusable(&json, "文本A", "[]", &other, "文本A"));
        // category 不一致
        let mut other = test_params();
        other.category = Some("grammar".into());
        assert!(!history_reusable(&json, "文本A", "[]", &other, "文本A"));
        // 指纹不一致
        let mut other = test_params();
        other.rules_fingerprint = "f2".into();
        assert!(!history_reusable(&json, "文本A", "[]", &other, "文本A"));
        // 存储侧是 level 2（含 LLM 的历史）
        let mut stored = test_params();
        stored.level = 2;
        stored.llm_configured = true;
        assert!(!history_reusable(
            &serde_json::to_string(&stored).unwrap(),
            "文本A",
            "[]",
            &p,
            "文本A"
        ));
    }

    #[test]
    fn history_reusable_model_version_gate() {
        // 双方版本一致（含 None == None：决策层未启用 / 服务不可用）→ 可复用
        let p = test_params();
        let json = serde_json::to_string(&p).unwrap();
        assert!(history_reusable(&json, "文本A", "[]", &p, "文本A"));

        let mut v1 = test_params();
        v1.model_version = Some("v1".into());
        let json_v1 = serde_json::to_string(&v1).unwrap();
        assert!(history_reusable(&json_v1, "文本A", "[]", &v1, "文本A"));

        // 模型重训练（v1 → v2）→ 不再可复用
        let mut v2 = test_params();
        v2.model_version = Some("v2".into());
        assert!(!history_reusable(&json_v1, "文本A", "[]", &v2, "文本A"));
        assert!(!history_reusable(&json_v2(), "文本A", "[]", &v1, "文本A"));

        // 版本未知（None）与携带版本的历史不匹配（保守：无法证明模型未变）
        assert!(!history_reusable(&json_v1, "文本A", "[]", &p, "文本A"));
        assert!(!history_reusable(&json, "文本A", "[]", &v1, "文本A"));
    }

    fn json_v2() -> String {
        let mut v2 = test_params();
        v2.model_version = Some("v2".into());
        serde_json::to_string(&v2).unwrap()
    }

    #[test]
    fn detect_params_old_json_defaults_model_version_to_none() {
        // 旧记录（无 model_version 字段）→ serde 缺省 None，可解析
        let p: DetectParams =
            serde_json::from_str(r#"{"level":1,"force_llm":false,"category":null,"decision_enabled":false,"l3_enabled":false,"llm_configured":false,"llm_model":null,"rules_fingerprint":"f1"}"#)
                .unwrap();
        assert_eq!(p.model_version, None);
    }
}
