//! 知识源资源包：一个知识源单元（如内置示例源）的
//! 打包导出 / 导入 / 删除（以知识源为单位的整体生命周期维护）。
//!
//! 一个知识源的资源包（[`ResourcePack`]）= 该源派生的全部资源：
//!
//! | 组件 | 内容 | 存放 |
//! |---|---|---|
//! | `source` | 知识源注册表条目 | `knowledge-base/sources.json` |
//! | `knowledge_points` | 该源背书的全部知识点（知识数据） | `knowledge-base/points/{lang}.json` |
//! | `staging` | 未发布的导入会话草稿（可选） | `knowledge-base/staging/` |
//! | `decision_tables` | 含该源标记规则行的各语言决策表快照（规则；行级 `_meta` 溯源） | `decision-tables/{lang}_gec_attribution.json` |
//! | `aux_points` / `aux_index` | 源原始提取数据（决策数据；条目级 `source_id` 归属） | `decision-tables/{lang}_points.json` / `{lang}_point_index.json` |
//!
//! 三种操作（本模块为纯函数层，无 IO——目录读写由调用方完成）：
//! - [`export_pack`]：打包导出（决策表/aux 组件由调用方注入）；
//! - [`import_pack`]：校验 → 注册表 upsert → 知识点按 `point_id` 幂等归并
//!   （**既有点只追加 `sources[]` 背书、不覆盖内容**，与导入管线
//!   「点 ID 不随书变」原则一致；新概念才造新点）；
//! - [`clean_pack`]：删除资源包——注册表移除 → 各点剥离该源引用 →
//!   失去全部背书的点默认删除（`keep_orphans=true` 保留）。
//!
//! 规则溯源（决策表行级元数据，zen-engine 2.1.2 方言要求规则行全部为
//! 字符串键值，见 `decision-tables/generate_tables.py`）：
//! - `_id` = 规则稳定 ID；`_meta` = 派生该规则的知识源 source_id
//!   （无 `_meta` = 产品内置通用规则，删除资源包不触碰）。
//! 按源摘除/合并规则行见 [`remove_tagged_rows`] / [`merge_tagged_rows`]。
//!
//! 校验和为导出/导入同工具内自洽的内容指纹（SipHash，防篡改/截断提示），
//! 不是跨语言密码签名。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use shared_model::{KnowledgePoint, KnowledgeSource, KnowledgeSourceRef};

use crate::ingest::IngestSession;
use crate::knowledge::KnowledgeStore;

/// 归档格式标识（导入时校验，防误传其他 JSON）
pub const PACK_FORMAT: &str = "langfixer-resource-pack";
/// 归档格式版本（结构变更时递增；导入端只接受当前版本）
pub const PACK_FORMAT_VERSION: u32 = 1;

/// JDM 规则行元数据键：规则稳定 ID
pub const RULE_META_ID: &str = "_id";
/// JDM 规则行元数据键：派生该规则的知识源 source_id
pub const RULE_META_SOURCE: &str = "_meta";

/// 决策表组件：某语言归因决策表整表快照（JDM）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackDecisionTable {
    pub lang: String,
    /// JDM 决策表（nodes/edges；规则行带 `_id`/`_meta` 溯源）
    pub jdm: Value,
}

/// 资源包组件集合
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PackComponents {
    /// 该源背书的全部知识点（按 lang + point_id 排序）
    #[serde(default)]
    pub knowledge_points: Vec<KnowledgePoint>,
    /// 未发布的导入会话草稿（导出时存在则随包携带）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staging: Option<IngestSession>,
    /// 含该源 `_meta` 标记规则行的各语言归因决策表快照（每语言一项；
    /// 导入时按源逐表合并规则行）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decision_tables: Vec<PackDecisionTable>,
    /// `decision-tables/{lang}_points.json` 内容（源原始提取数据）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aux_points: Option<Value>,
    /// `decision-tables/{lang}_point_index.json` 内容（改错对 → 知识点索引）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aux_index: Option<Value>,
}

/// 资源包：某知识源单元的完整自描述归档（单文件 JSON）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourcePack {
    /// 格式标识（应为 [`PACK_FORMAT`]）
    pub format: String,
    /// 格式版本
    pub format_version: u32,
    /// 导出时间（RFC 3339）
    pub exported_at: String,
    /// 内容校验和（canonical(source + components) 的 SipHash，16 位十六进制）
    pub checksum: String,
    /// 知识源注册表条目
    pub source: KnowledgeSource,
    /// 资源组件
    pub components: PackComponents,
}

/// 盘点：某语言下该源的知识数据统计
#[derive(Debug, Clone, Serialize)]
pub struct PackLangStats {
    pub lang: String,
    /// 该语言知识点总数
    pub total_points: usize,
    /// 该源背书的点数
    pub backed_points: usize,
    /// 仅该源背书（删除资源包时将整体删除）的点数
    pub exclusive_points: usize,
    /// 还被其他源背书（删除资源包时只剥离引用）的点数
    pub shared_points: usize,
}

/// 盘点：该源在决策表中标记的规则行统计
#[derive(Debug, Clone, Serialize)]
pub struct PackRuleStats {
    pub lang: String,
    /// 该语言决策表规则行总数
    pub total_rules: usize,
    /// `_meta` 标记为该源的规则行数（删除资源包时将被摘除）
    pub tagged_rules: usize,
}

/// 盘点：导入会话摘要（存在时）
#[derive(Debug, Clone, Serialize)]
pub struct PackStagingBrief {
    pub status: String,
    pub chunks_total: usize,
    pub chunks_done: usize,
    pub staged_points: usize,
}

/// 盘点结果（删除前的确认预览 + 导出前的组成清单）
#[derive(Debug, Clone, Serialize)]
pub struct PackSummary {
    pub source: KnowledgeSource,
    pub langs: Vec<PackLangStats>,
    pub total_points: usize,
    pub rules: Vec<PackRuleStats>,
    pub aux_points_present: bool,
    pub aux_index_present: bool,
    pub staging: Option<PackStagingBrief>,
}

/// 导入报告（知识数据部分；决策表/aux/staging 由调用方报告）
#[derive(Debug, Clone, Serialize, Default)]
pub struct PackImportReport {
    pub source_id: String,
    /// 注册表新增了源（false = 已存在，条目被包版本覆盖）
    pub source_added: bool,
    /// 既有点追加背书的数量
    pub points_endorsed: usize,
    /// 新建知识点的数量
    pub points_created: usize,
    /// 该源已背书、跳过的数量（幂等重复导入）
    pub points_skipped: usize,
}

/// 删除报告（知识数据部分；决策表/aux/staging 由调用方报告）
#[derive(Debug, Clone, Serialize, Default)]
pub struct PackCleanReport {
    pub source_id: String,
    /// 注册表条目已移除
    pub source_removed: bool,
    /// 从既有点剥离的背书引用数
    pub refs_detached: usize,
    /// 因失去全部背书而删除的点数
    pub points_removed: usize,
    /// `keep_orphans=true` 时保留的孤儿点数
    pub orphans_kept: usize,
}

/// 内容校验和：canonical JSON（source + components）的 SipHash
pub fn pack_checksum(source: &KnowledgeSource, components: &PackComponents) -> String {
    let canonical = serde_json::to_string(&(source, components)).unwrap_or_default();
    let mut hasher = DefaultHasher::new();
    canonical.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn backed_by(point: &KnowledgePoint, source_id: &str) -> bool {
    point.sources.iter().any(|r| r.source_id == source_id)
}

// ---------------------------------------------------------------------------
// JDM 决策表行级纯函数（规则溯源：按 _meta 统计/摘除/合并）
// ---------------------------------------------------------------------------

/// 定位 JDM 决策表节点的规则数组（只读）
fn rules_array(jdm: &Value) -> Option<&Vec<Value>> {
    jdm.get("nodes")?.as_array()?.iter().find_map(|n| {
        (n.get("type").and_then(|t| t.as_str()) == Some("decisionTableNode"))
            .then(|| n.get("content")?.get("rules")?.as_array())
            .flatten()
    })
}

/// 定位 JDM 决策表节点的可变规则数组
fn rules_array_mut(jdm: &mut Value) -> Option<&mut Vec<Value>> {
    jdm.get_mut("nodes")?
        .as_array_mut()?
        .iter_mut()
        .find_map(|n| {
            (n.get("type").and_then(|t| t.as_str()) == Some("decisionTableNode"))
                .then(|| n.get_mut("content")?.get_mut("rules")?.as_array_mut())
                .flatten()
        })
}

/// 规则行是否标记为某知识源派生（`_meta == source_id`）
pub fn row_tagged(row: &Value, source_id: &str) -> bool {
    row.get(RULE_META_SOURCE).and_then(|v| v.as_str()) == Some(source_id)
}

/// 规则行的稳定 ID（`_id`；缺省 None）
pub fn row_id(row: &Value) -> Option<&str> {
    row.get(RULE_META_ID).and_then(|v| v.as_str())
}

/// 某 JDM 决策表规则行总数（解析失败返回 0）
pub fn table_rule_count(jdm: &Value) -> usize {
    rules_array(jdm).map(|r| r.len()).unwrap_or(0)
}

/// 决策表中 `_meta` 标记为某源的规则行数
pub fn tagged_rule_count(jdm: &Value, source_id: &str) -> usize {
    rules_array(jdm)
        .map(|rows| rows.iter().filter(|r| row_tagged(r, source_id)).count())
        .unwrap_or(0)
}

/// 决策表全部规则行（克隆；解析失败返回空）。导入合并时取包内规则行用。
pub fn table_rows(jdm: &Value) -> Vec<Value> {
    rules_array(jdm).cloned().unwrap_or_default()
}

/// 摘除决策表中 `_meta` 标记为某源的规则行（删除资源包的规则部分；
/// 无 `_meta` 的产品内置通用规则行保守保留）。返回摘除行数。
pub fn remove_tagged_rows(jdm: &mut Value, source_id: &str) -> usize {
    let Some(rows) = rules_array_mut(jdm) else {
        return 0;
    };
    let before = rows.len();
    rows.retain(|r| !row_tagged(r, source_id));
    before - rows.len()
}

/// 规则行是否带知识源溯源标记（`_meta` 非空）
fn row_has_meta(row: &Value) -> bool {
    row.get(RULE_META_SOURCE)
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty())
}

/// 把资源包规则行合并进决策表（导入的规则部分）：只合并 `_meta` 标记为
/// 该源的行——`_id` 已存在则原地替换（幂等重导），否则插入到首条通用行
/// （无 `_meta` 行）之前：保持「源派生行在前、通用兜底行最后」的行序不变量
/// （hit-policy first 下 default 行必须最后匹配）。返回 (替换数, 新增数)。
pub fn merge_tagged_rows(jdm: &mut Value, incoming: &[Value], source_id: &str) -> (usize, usize) {
    let (mut replaced, mut added) = (0usize, 0usize);
    let Some(rows) = rules_array_mut(jdm) else {
        return (0, 0);
    };
    for row in incoming.iter().filter(|r| row_tagged(r, source_id)) {
        let replaced_here = match row_id(row) {
            Some(id) => match rows.iter_mut().find(|r| row_id(r) == Some(id)) {
                Some(existing) => {
                    *existing = row.clone();
                    true
                }
                None => false,
            },
            None => false,
        };
        if replaced_here {
            replaced += 1;
        } else {
            let pos = rows
                .iter()
                .position(|r| !row_has_meta(r))
                .unwrap_or(rows.len());
            rows.insert(pos, row.clone());
            added += 1;
        }
    }
    (replaced, added)
}

// ---------------------------------------------------------------------------
// 盘点 / 打包导出 / 校验 / 导入 / 删除（知识数据部分；纯内存，无 IO）
// ---------------------------------------------------------------------------

/// 盘点某知识源资源包（决策表统计与 aux 存在性由调用方注入）
pub fn summarize(
    store: &KnowledgeStore,
    staging: Option<&IngestSession>,
    tables: &[(String, Value)],
    aux_present: (bool, bool),
    source_id: &str,
) -> Result<PackSummary, String> {
    let mut source = store
        .sources()
        .into_iter()
        .find(|s| s.source_id == source_id)
        .ok_or_else(|| format!("知识源不存在: {source_id}"))?;
    let mut langs = Vec::new();
    let mut total = 0usize;
    for lang in store.langs() {
        let all = store.points(&lang, None);
        let backed: Vec<&KnowledgePoint> = all.iter().filter(|p| backed_by(p, source_id)).collect();
        if backed.is_empty() {
            continue;
        }
        let exclusive = backed.iter().filter(|p| p.sources.len() == 1).count();
        langs.push(PackLangStats {
            lang,
            total_points: all.len(),
            backed_points: backed.len(),
            exclusive_points: exclusive,
            shared_points: backed.len() - exclusive,
        });
        total += backed.len();
    }
    let rules: Vec<PackRuleStats> = tables
        .iter()
        .filter_map(|(lang, jdm)| {
            let tagged = tagged_rule_count(jdm, source_id);
            (tagged > 0).then(|| PackRuleStats {
                lang: lang.clone(),
                total_rules: table_rule_count(jdm),
                tagged_rules: tagged,
            })
        })
        .collect();
    source.point_count = total;
    let brief = staging.map(|s| PackStagingBrief {
        status: s.status.clone(),
        chunks_total: s.chunks_total,
        chunks_done: s.chunks_done,
        staged_points: s.drafts.len(),
    });
    Ok(PackSummary {
        source,
        langs,
        total_points: total,
        rules,
        aux_points_present: aux_present.0,
        aux_index_present: aux_present.1,
        staging: brief,
    })
}

/// 打包导出：知识数据组件（source + 该源背书的全部知识点 + 可选 staging）。
/// 决策表/aux 组件由调用方（api-server / 桌面端）读文件后注入并重算校验和。
pub fn export_pack(
    store: &KnowledgeStore,
    staging: Option<&IngestSession>,
    source_id: &str,
) -> Result<ResourcePack, String> {
    let summary = summarize(store, staging, &[], (false, false), source_id)?;
    let mut points = Vec::new();
    for lang in store.langs() {
        points.extend(store.points(&lang, Some(source_id)));
    }
    points.sort_by(|a, b| (&a.lang, &a.point_id).cmp(&(&b.lang, &b.point_id)));
    let components = PackComponents {
        knowledge_points: points,
        staging: staging.cloned(),
        decision_tables: Vec::new(),
        aux_points: None,
        aux_index: None,
    };
    let checksum = pack_checksum(&summary.source, &components);
    Ok(ResourcePack {
        format: PACK_FORMAT.to_string(),
        format_version: PACK_FORMAT_VERSION,
        exported_at: chrono::Utc::now().to_rfc3339(),
        checksum,
        source: summary.source,
        components,
    })
}

/// 校验资源包结构（格式标识/版本/必填字段/校验和）。失败整体拒收。
pub fn validate_pack(pack: &ResourcePack) -> Result<(), String> {
    if pack.format != PACK_FORMAT {
        return Err(format!(
            "资源包格式标识不符（期望 {PACK_FORMAT}，实际 {}）",
            pack.format
        ));
    }
    if pack.format_version != PACK_FORMAT_VERSION {
        return Err(format!(
            "资源包格式版本不支持（期望 {PACK_FORMAT_VERSION}，实际 {}）",
            pack.format_version
        ));
    }
    if pack.source.source_id.trim().is_empty() {
        return Err("资源包缺少 source.source_id".into());
    }
    for p in &pack.components.knowledge_points {
        if p.point_id.trim().is_empty() {
            return Err("资源包知识点 point_id 不能为空".into());
        }
        if p.lang.trim().is_empty() {
            return Err(format!("资源包知识点 lang 不能为空: {}", p.point_id));
        }
    }
    if pack.checksum != pack_checksum(&pack.source, &pack.components) {
        return Err("资源包校验和不符（内容被篡改或截断），拒收".into());
    }
    Ok(())
}

/// 导入（知识数据部分）：注册表 upsert → 知识点按 `point_id` 幂等归并。
///
/// 归并规则（与 [`crate::ingest::plan_merge`] 发布语义一致）：
/// - 既有点：只追加该源 `sources[]` 背书（已背书则跳过），**不覆盖内容**；
/// - 新点：直接收录（强制 lang 与分组一致，并保证含该源引用）。
///
/// 前置 [`validate_pack`] 通过后调用；校验失败调用方不得进入本函数。
// ---------------------------------------------------------------------------
// aux 决策数据文件（条目级归属标记：entry.source_id）
// ---------------------------------------------------------------------------
//
// aux 文件结构：`{ "<error_type>": [ entry, ... ] }`（源原始提取数据，
// 运行时不加载）。条目归属 = `entry.source_id`（派生该条目的知识源）。
// 无任何标记的旧格式文件 = 整体视为资源包源所有（导出时注入标记、导入写回
// 带标记，使存量文件逐步迁移到条目级归属）——同语言多源共存时，删除/导出
// 某源的资源包不再误伤其他源的条目。

/// aux 文件中是否存在条目级归属标记（任一 entry 带 source_id）
fn aux_has_source_marks(aux: &Value) -> bool {
    aux.as_object().is_some_and(|m| {
        m.values()
            .filter_map(|v| v.as_array())
            .flatten()
            .any(|e| e.get("source_id").and_then(|v| v.as_str()).is_some())
    })
}

/// aux 文件是否为空（非对象、或所有分组都没有条目）
pub fn aux_is_empty(aux: &Value) -> bool {
    aux.as_object().is_some_and(|m| {
        m.values()
            .all(|v| v.as_array().is_some_and(|a| a.is_empty()))
    })
}

fn aux_from_groups(groups: Vec<(String, Vec<Value>)>) -> Value {
    let mut map = serde_json::Map::new();
    for (key, arr) in groups {
        if !arr.is_empty() {
            map.insert(key, Value::Array(arr));
        }
    }
    Value::Object(map)
}

/// 按源拆分 aux 文件 → (该源条目, 其余条目)。
/// - 文件已有归属标记：按 `entry.source_id` 精确拆分；无标记条目保守归「其余」；
/// - 旧格式（全无标记）：整文件判给该源，并向 owned 条目注入 `source_id` 标记。
pub fn aux_split_source(aux: &Value, source_id: &str) -> (Value, Value) {
    let Some(map) = aux.as_object() else {
        return (Value::Object(Default::default()), aux.clone());
    };
    let legacy = !aux_has_source_marks(aux);
    let mut owned_groups = Vec::new();
    let mut rest_groups = Vec::new();
    for (key, arr) in map {
        let entries = arr.as_array().cloned().unwrap_or_default();
        let (mut owned, mut rest) = (Vec::new(), Vec::new());
        for mut entry in entries {
            let owner = entry
                .get("source_id")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            if legacy || owner.as_deref() == Some(source_id) {
                if let Some(obj) = entry.as_object_mut() {
                    obj.entry("source_id".to_string())
                        .or_insert_with(|| Value::String(source_id.to_string()));
                }
                owned.push(entry);
            } else {
                rest.push(entry);
            }
        }
        if !owned.is_empty() {
            owned_groups.push((key.clone(), owned));
        }
        if !rest.is_empty() {
            rest_groups.push((key.clone(), rest));
        }
    }
    (aux_from_groups(owned_groups), aux_from_groups(rest_groups))
}

/// 导入合并 aux 文件：当前文件中该源条目整体移除（资源包为该源权威版本），
/// 再加入包内条目（其他源条目原样保留）。
pub fn aux_merge_source(current: &Value, incoming: &Value, source_id: &str) -> Value {
    let (_, mut merged) = aux_split_source(current, source_id);
    let Some(in_map) = incoming.as_object() else {
        return merged;
    };
    let Some(merged_map) = merged.as_object_mut() else {
        return merged;
    };
    for (key, arr) in in_map {
        let entries = arr.as_array().cloned().unwrap_or_default();
        if entries.is_empty() {
            continue;
        }
        merged_map
            .entry(key.clone())
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .expect("aux group is array")
            .extend(entries);
    }
    merged
}

// ---------------------------------------------------------------------------
// 压缩包容器（zip / deflate）：导出转 .zip 单文件下载，导入自动识别 zip
// ---------------------------------------------------------------------------

/// zip 内资源包条目名
pub const PACK_ZIP_ENTRY: &str = "pack.json";

/// 单条目解压尺寸上限（解压炸弹防护：64MB 请求体上限的 zip 可解压出任意大 JSON）
pub const MAX_PACK_ENTRY_BYTES: u64 = 64 * 1024 * 1024;

/// 字节流是否为 zip（PK\x03\x04 本地文件头 magic）
pub fn is_zip_bytes(bytes: &[u8]) -> bool {
    bytes.starts_with(b"PK\x03\x04")
}

/// 资源包 → zip 压缩包字节（单条目 `pack.json`，deflate 压缩；
/// aux 决策数据等大 JSON 压缩率高，便于直接下载保存）
pub fn pack_to_zip(pack: &ResourcePack) -> Result<Vec<u8>, String> {
    use std::io::{Cursor, Write};
    use zip::write::SimpleFileOptions;

    let json = serde_json::to_string(pack).map_err(|e| format!("资源包序列化失败: {e}"))?;
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    writer
        .start_file(PACK_ZIP_ENTRY, options)
        .map_err(|e| format!("zip 写入失败: {e}"))?;
    writer
        .write_all(json.as_bytes())
        .map_err(|e| format!("zip 写入失败: {e}"))?;
    let cursor = writer.finish().map_err(|e| format!("zip 收尾失败: {e}"))?;
    Ok(cursor.into_inner())
}

/// zip 压缩包字节 → 资源包（读 `pack.json` 条目；无则取首个 .json 条目；
/// 解析后由调用方 validate_pack 校验；条目解压尺寸受 [`MAX_PACK_ENTRY_BYTES`]
/// 上限约束——解压炸弹防护）
pub fn pack_from_zip(bytes: &[u8]) -> Result<ResourcePack, String> {
    use std::io::{Cursor, Read};

    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| format!("zip 解析失败: {e}"))?;
    // 优先 pack.json；否则取首个 .json 文件条目
    let entry_name = if archive.by_name(PACK_ZIP_ENTRY).is_ok() {
        PACK_ZIP_ENTRY.to_string()
    } else {
        (0..archive.len())
            .find_map(|i| {
                archive.by_index(i).ok().and_then(|f| {
                    if f.is_file() && f.name().ends_with(".json") {
                        Some(f.name().to_string())
                    } else {
                        None
                    }
                })
            })
            .ok_or_else(|| format!("zip 内未找到 {PACK_ZIP_ENTRY}（或任何 .json）条目"))?
    };
    let mut entry = archive
        .by_name(&entry_name)
        .map_err(|e| format!("zip 条目 {entry_name} 读取失败: {e}"))?;
    if entry.size() > MAX_PACK_ENTRY_BYTES {
        return Err(format!(
            "zip 条目过大（声明 {} 字节，上限 {MAX_PACK_ENTRY_BYTES} 字节）",
            entry.size()
        ));
    }
    // 实际读取同样设限（防条目头虚报小尺寸、解压后膨胀的压缩炸弹）
    let mut buf = Vec::new();
    entry
        .by_ref()
        .take(MAX_PACK_ENTRY_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(|e| format!("zip 读取失败: {e}"))?;
    if buf.len() as u64 > MAX_PACK_ENTRY_BYTES {
        return Err(format!(
            "zip 条目解压超限（上限 {MAX_PACK_ENTRY_BYTES} 字节，疑似压缩炸弹），拒收"
        ));
    }
    serde_json::from_slice(&buf)
        .map_err(|e| format!("资源包 JSON 解析失败（条目 {entry_name}）: {e}"))
}

/// 导入入口字节自适应：zip 压缩包或裸 JSON → 资源包（不校验；调用方 validate_pack）
pub fn pack_from_bytes(bytes: &[u8]) -> Result<ResourcePack, String> {
    if is_zip_bytes(bytes) {
        pack_from_zip(bytes)
    } else {
        serde_json::from_slice(bytes).map_err(|e| format!("资源包 JSON 解析失败: {e}"))
    }
}

pub fn import_pack(
    store: &KnowledgeStore,
    pack: &ResourcePack,
) -> Result<PackImportReport, String> {
    let source_id = pack.source.source_id.clone();
    let mut report = PackImportReport {
        source_id: source_id.clone(),
        ..Default::default()
    };

    // ① 注册表 upsert（包条目覆盖既有；point_count 为列表时统计值，清零）
    let mut sources = store.sources();
    report.source_added = !sources.iter().any(|s| s.source_id == source_id);
    sources.retain(|s| s.source_id != source_id);
    let mut entry = pack.source.clone();
    entry.point_count = 0;
    sources.push(entry);
    store.update_sources(&serde_json::to_string(&sources).map_err(|e| e.to_string())?)?;

    // ② 知识点按语言分组归并
    let mut by_lang: std::collections::BTreeMap<String, Vec<KnowledgePoint>> = Default::default();
    for p in &pack.components.knowledge_points {
        by_lang.entry(p.lang.clone()).or_default().push(p.clone());
    }
    for (lang, incoming) in by_lang {
        let mut current = store.points(&lang, None);
        for mut p in incoming {
            if !backed_by(&p, &source_id) {
                p.sources.push(KnowledgeSourceRef {
                    source_id: source_id.clone(),
                    location: String::new(),
                    title: String::new(),
                });
            }
            match current.iter_mut().find(|e| e.point_id == p.point_id) {
                Some(existing) => {
                    if backed_by(existing, &source_id) {
                        report.points_skipped += 1;
                    } else {
                        let src_ref = p
                            .sources
                            .iter()
                            .find(|r| r.source_id == source_id)
                            .cloned()
                            .unwrap();
                        existing.sources.push(src_ref);
                        report.points_endorsed += 1;
                    }
                }
                None => {
                    p.lang = lang.clone();
                    current.push(p);
                    report.points_created += 1;
                }
            }
        }
        store.update_points(
            &lang,
            &serde_json::to_string(&current).map_err(|e| e.to_string())?,
        )?;
    }
    Ok(report)
}

/// 删除（知识数据部分）：从知识库移除某知识源——注册表条目 + 全部背书
/// 引用；失去全部背书的点默认删除（`keep_orphans=true` 保留为孤儿点）。
pub fn clean_pack(
    store: &KnowledgeStore,
    source_id: &str,
    keep_orphans: bool,
) -> Result<PackCleanReport, String> {
    let mut sources = store.sources();
    if !sources.iter().any(|s| s.source_id == source_id) {
        return Err(format!("知识源不存在: {source_id}"));
    }
    let mut report = PackCleanReport {
        source_id: source_id.to_string(),
        ..Default::default()
    };
    // ① 各语言知识点：剥离该源引用；失去全部背书的点按策略处置
    for lang in store.langs() {
        let current = store.points(&lang, None);
        if !current.iter().any(|p| backed_by(p, source_id)) {
            continue;
        }
        let mut next = Vec::with_capacity(current.len());
        for mut p in current {
            if !backed_by(&p, source_id) {
                next.push(p);
                continue;
            }
            p.sources.retain(|r| r.source_id != source_id);
            report.refs_detached += 1;
            if p.sources.is_empty() {
                if keep_orphans {
                    report.orphans_kept += 1;
                    next.push(p);
                } else {
                    report.points_removed += 1;
                }
            } else {
                next.push(p);
            }
        }
        store.update_points(
            &lang,
            &serde_json::to_string(&next).map_err(|e| e.to_string())?,
        )?;
    }
    // ② 注册表条目移除
    sources.retain(|s| s.source_id != source_id);
    store.update_sources(&serde_json::to_string(&sources).map_err(|e| e.to_string())?)?;
    report.source_removed = true;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::{DraftPoint, IngestSession, StagedDraft};
    use serde_json::json;
    use shared_model::CorrectionPair;

    fn src(id: &str) -> KnowledgeSource {
        KnowledgeSource {
            source_id: id.into(),
            title: format!("测试源 {id}"),
            author: "作者".into(),
            edition: "1".into(),
            lang: "en".into(),
            license_scope: "internal_test".into(),
            point_count: 0,
        }
    }

    fn point(pid: &str, src_ids: &[&str]) -> KnowledgePoint {
        KnowledgePoint {
            point_id: pid.into(),
            lang: "en".into(),
            error_type: pid.split('.').next().unwrap().into(),
            title: format!("点 {pid}"),
            section: "一.1".into(),
            cefr: "A2".into(),
            remedial: format!("讲解 {pid}"),
            examples: vec!["example".into()],
            corrections: vec![CorrectionPair {
                wrong: "wrong".into(),
                right: "right".into(),
            }],
            sources: src_ids
                .iter()
                .map(|s| KnowledgeSourceRef {
                    source_id: s.to_string(),
                    location: format!("第讲 {s}"),
                    title: String::new(),
                })
                .collect(),
        }
    }

    /// JDM 表骨架 + 指定规则行
    fn jdm(rows: Vec<Value>) -> Value {
        json!({
            "nodes": [
                {"id": "input", "name": "input", "type": "inputNode"},
                {"id": "table", "name": "t", "type": "decisionTableNode",
                 "content": {"inputs": [], "outputs": [], "rules": rows, "hitPolicy": "first"}},
                {"id": "output", "name": "output", "type": "outputNode"}
            ],
            "edges": []
        })
    }

    fn tagged_row(rid: &str, source: &str) -> Value {
        json!({"_id": rid, "_meta": source, "matchedRule": format!("\"{rid}\"")})
    }

    /// 知识库：src-a 独占点 ×2 + src-a/src-b 共享点 ×1 + src-b 独占点 ×1
    fn store_fixture() -> KnowledgeStore {
        let store = KnowledgeStore::new();
        store
            .update_sources(&serde_json::to_string(&vec![src("src-a"), src("src-b")]).unwrap())
            .unwrap();
        let points = vec![
            point("article.1", &["src-a"]),
            point("article.2", &["src-a", "src-b"]),
            point("article.3", &["src-b"]),
        ];
        store
            .update_points("en", &serde_json::to_string(&points).unwrap())
            .unwrap();
        store
    }

    fn tables_fixture() -> Vec<(String, Value)> {
        vec![(
            "en".to_string(),
            jdm(vec![
                tagged_row("article_novice", "src-a"),
                tagged_row("article_any", "src-a"),
                json!({"_id": "default_knowledge_gap", "matchedRule": "\"default\""}),
            ]),
        )]
    }

    #[test]
    fn summarize_counts_points_rules_and_aux() {
        let store = store_fixture();
        let s = summarize(&store, None, &tables_fixture(), (true, true), "src-a").unwrap();
        assert_eq!(s.total_points, 2);
        assert_eq!(s.langs[0].exclusive_points, 1);
        assert_eq!(s.langs[0].shared_points, 1);
        assert_eq!(s.rules[0].tagged_rules, 2);
        assert_eq!(s.rules[0].total_rules, 3);
        assert!(s.aux_points_present && s.aux_index_present);
        assert!(summarize(&store, None, &[], (false, false), "src-x").is_err());
    }

    #[test]
    fn export_then_clean_then_import_restores_knowledge() {
        let store = store_fixture();
        let pack = export_pack(&store, None, "src-a").unwrap();
        assert_eq!(pack.components.knowledge_points.len(), 2);
        assert_eq!(pack.components.knowledge_points[0].point_id, "article.1");

        // 删除：src-a 独占点 article.1 删除，共享点 article.2 只剥离引用
        let report = clean_pack(&store, "src-a", false).unwrap();
        assert!(report.source_removed);
        assert_eq!(report.refs_detached, 2);
        assert_eq!(report.points_removed, 1);
        assert_eq!(store.points("en", None).len(), 2);

        // 导入资源包：article.1 重建、article.2 重新背书
        validate_pack(&pack).unwrap();
        let report = import_pack(&store, &pack).unwrap();
        assert!(report.source_added);
        assert_eq!(report.points_created, 1);
        assert_eq!(report.points_endorsed, 1);
        assert_eq!(store.points("en", None).len(), 3);
        assert!(backed_by(
            &store.get_point("en", "article.1").unwrap(),
            "src-a"
        ));
    }

    #[test]
    fn import_is_idempotent_and_preserves_existing_content() {
        let store = store_fixture();
        let pack = export_pack(&store, None, "src-a").unwrap();
        import_pack(&store, &pack).unwrap();
        // 篡改既有点内容后再导入：不得覆盖
        let mut points = store.points("en", None);
        for p in &mut points {
            if p.point_id == "article.1" {
                p.title = "人工修订过的标题".into();
            }
        }
        store
            .update_points("en", &serde_json::to_string(&points).unwrap())
            .unwrap();
        let second = import_pack(&store, &pack).unwrap();
        assert_eq!(second.points_skipped, 2);
        assert_eq!(second.points_created, 0);
        assert_eq!(
            store.get_point("en", "article.1").unwrap().title,
            "人工修订过的标题"
        );
    }

    #[test]
    fn clean_keeps_shared_points_and_orphans_by_policy() {
        let store = store_fixture();
        let report = clean_pack(&store, "src-a", true).unwrap();
        assert_eq!(report.points_removed, 0);
        assert_eq!(report.orphans_kept, 1);
        assert_eq!(store.points("en", None).len(), 3);
        assert!(
            clean_pack(&store, "src-a", true).is_err(),
            "重复删除应报错（源已不存在）"
        );
    }

    #[test]
    fn validate_rejects_tampered_or_foreign_pack() {
        let store = store_fixture();
        let mut pack = export_pack(&store, None, "src-a").unwrap();
        pack.components.knowledge_points[0].title = "篡改".into();
        assert!(validate_pack(&pack).unwrap_err().contains("校验和"));
        pack.format = "other".into();
        assert!(validate_pack(&pack).unwrap_err().contains("格式标识"));
    }

    #[test]
    fn jdm_remove_and_merge_tagged_rows() {
        // merge：同 _id 替换、新 _id 追加、他源标记行不合并（在未删除的表上验证）
        let mut merged = tables_fixture().remove(0).1;
        let incoming = vec![
            tagged_row("article_novice", "src-a"),
            tagged_row("tense_novice", "src-a"),
            tagged_row("foreign_rule", "src-z"),
        ];
        let (replaced, added) = merge_tagged_rows(&mut merged, &incoming, "src-a");
        assert_eq!((replaced, added), (1, 1));
        assert_eq!(table_rule_count(&merged), 4); // 原 3 行 + tense_novice
        // 新增行插在通用行（无 _meta）之前：hit-policy first 下兜底行仍最后匹配
        let rows = rules_array(&merged).unwrap();
        assert_eq!(row_id(&rows[2]), Some("tense_novice"));
        assert_eq!(row_id(&rows[3]), Some("default_knowledge_gap"));

        // remove：摘除该源全部标记行，通用 default 行保留
        let mut table = tables_fixture().remove(0).1;
        assert_eq!(tagged_rule_count(&table, "src-a"), 2);
        assert_eq!(remove_tagged_rows(&mut table, "src-a"), 2);
        assert_eq!(table_rule_count(&table), 1);
    }

    #[test]
    fn export_includes_staging_session() {
        let store = store_fixture();
        let draft = StagedDraft {
            seq: 1,
            draft: DraftPoint {
                error_type: "article".into(),
                title: "草稿点".into(),
                section: String::new(),
                remedial: String::new(),
                cefr: String::new(),
                examples: vec![],
                corrections: vec![],
                location: String::new(),
                match_point_id: String::new(),
            },
            staged_at: "2026-01-01T00:00:00Z".into(),
        };
        let session = IngestSession {
            source: src("src-a"),
            chunks_total: 3,
            chunks_done: 1,
            chunks_failed: 0,
            created_at: "2026-01-01T00:00:00Z".into(),
            drafts: vec![draft],
            errors: vec![],
            status: "running".into(),
            consecutive_failures: 0,
            rule_drafts: vec![],
            rules_status: String::new(),
            rules_total: 0,
            rules_done: 0,
            rules_errors: vec![],
        };
        let pack = export_pack(&store, Some(&session), "src-a").unwrap();
        assert_eq!(pack.components.staging.as_ref().unwrap().drafts.len(), 1);
        validate_pack(&pack).unwrap();
        let s = summarize(&store, Some(&session), &[], (false, false), "src-a").unwrap();
        assert_eq!(s.staging.unwrap().staged_points, 1);
    }

    #[test]
    fn zip_roundtrip_and_import() {
        let store = store_fixture();
        let mut pack = export_pack(&store, None, "src-a").unwrap();
        pack.components.decision_tables = vec![PackDecisionTable {
            lang: "en".into(),
            jdm: jdm(vec![tagged_row("r1", "src-a")]),
        }];
        pack.checksum = pack_checksum(&pack.source, &pack.components);

        let zip = pack_to_zip(&pack).unwrap();
        assert!(is_zip_bytes(&zip));
        let back = pack_from_zip(&zip).unwrap();
        validate_pack(&back).unwrap();
        assert_eq!(back.checksum, pack.checksum);
        assert_eq!(
            back.components.knowledge_points.len(),
            pack.components.knowledge_points.len()
        );

        // 字节自适应：zip 与裸 JSON 都能解析
        let raw = serde_json::to_vec(&pack).unwrap();
        assert!(!is_zip_bytes(&raw));
        assert!(pack_from_bytes(&raw).is_ok());
        assert!(pack_from_bytes(&zip).is_ok());

        // zip 导入到干净库（等价于 JSON 导入语义）
        let fresh = KnowledgeStore::new();
        let report = import_pack(&fresh, &back).unwrap();
        assert_eq!(fresh.points("en", None).len(), 2);
        assert_eq!(report.points_created, 2);
    }

    #[test]
    fn zip_rejects_garbage_and_missing_entry() {
        assert!(pack_from_zip(b"not a zip at all").is_err());
        // 合法 zip 但无 .json 条目
        use std::io::{Cursor, Write};
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opt = zip::write::SimpleFileOptions::default();
        w.start_file("readme.txt", opt).unwrap();
        w.write_all(b"hello").unwrap();
        let bytes = w.finish().unwrap().into_inner();
        assert!(pack_from_zip(&bytes).is_err());
    }

    #[test]
    fn aux_source_split_and_merge() {
        // 旧格式（无归属标记）：整文件判给导出源，并注入标记
        let legacy = json!({"article": [{"title": "a"}, {"title": "b"}]});
        let (owned, rest) = aux_split_source(&legacy, "src-a");
        assert!(aux_is_empty(&rest));
        assert_eq!(owned["article"].as_array().unwrap().len(), 2);
        assert_eq!(owned["article"][0]["source_id"], "src-a");

        // 已标记文件：按源精确拆分；无标记条目保守归「其余」
        let marked = json!({
            "article": [
                {"title": "a", "source_id": "src-a"},
                {"title": "c", "source_id": "src-b"},
                {"title": "generic"}
            ]
        });
        let (owned_a, rest_a) = aux_split_source(&marked, "src-a");
        assert_eq!(owned_a["article"].as_array().unwrap().len(), 1);
        assert_eq!(rest_a["article"].as_array().unwrap().len(), 2);

        // 导入 src-b 包：其他源条目保留，该源条目以包为权威
        let incoming_b = json!({"article": [{"title": "c2", "source_id": "src-b"}]});
        let merged = aux_merge_source(&marked, &incoming_b, "src-b");
        let titles: Vec<&str> = merged["article"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["title"].as_str().unwrap())
            .collect();
        assert_eq!(titles, ["a", "generic", "c2"]);

        // 重导 src-a：该源旧条目被包版本整体替换（幂等），条目总数不变
        let incoming_a = json!({"article": [{"title": "a", "source_id": "src-a"}]});
        let merged2 = aux_merge_source(&merged, &incoming_a, "src-a");
        assert_eq!(merged2["article"].as_array().unwrap().len(), 3);
        assert!(aux_is_empty(&json!({})));
        assert!(!aux_is_empty(&marked));
    }
}
