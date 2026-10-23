//! LLM GEC 引擎：OpenAI 兼容 chat completions 客户端（三层检测级联的第三层）
//!
//! - **规则引擎**（各语言检测器）/ **决策模型**（vegaml）：快速但召回有限；
//! - **LLM**（本模块）：由 API 层按三级响应级别（level 2/3）按需触发，
//!   对全文做一次兜底检测，捕捉前两层漏掉的错误（「漏报」场景）。
//!
//! 协议：`POST {base_url}/chat/completions`（非流式，temperature 0），
//! system / user 提示词来自检测模板（`prompts/{lang}_detect_prompt.json`，
//! user 模板含 `{{text}}` 占位符）；响应 content 应为 JSON 数组：
//! `[{"error_type", "confidence", "original", "correction"?, "message"}]`。
//! zh_detect v1.2 起 prompt 使用 CEC（ACL 2026）教学错误分类（错别字/词语搭配
//! 错误/…）；`map_llm_error_type` 在解析边界把教学分类映射为统一内部类型
//! （spelling/collocation/grammar/punctuation/logic/anomaly），教学分类不进入
//! 决策表与知识库。
//!
//! span 映射：提示词要求 `original` 为原文中的**精确片段**，服务端在原文中
//! 定位片段解析字节偏移（规避 LLM 直接数 CJK 字节偏移的不可靠）；
//! 定位失败时 span 置 `[0, 0]` 并在 message 标注（漏报场景宁留不丢）。
//! LLM 常对同一处报多条（引用同一/重叠片段）：按出现顺序为各错误项
//! 认领互不重叠的 span（`claimed` 去重），全部出现位置被占用时按
//! `[0, 0]` 标注处理，避免前端出现重叠下划线。
//!
//! 产出的错误标记 `source = "llm"`（见 [`shared_model::sources`]）。

use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::detector::DetectorError;
use crate::engine::{GecEngine, GecEngineKind};
use shared_model::{Error, error_types, sources};

/// LLM 兜底检测引擎配置
///
/// 由调用方（api-server / 桌面端）从运行时 LLM 参数（`LlmConfig`）+
/// 检测模板（`{lang}_detect` 提示词）组装；可 JSON 序列化供持久化。
/// `output_format` 决定响应解析方式（见 [`crate::models`]）：`json`（generic，
/// 现有 JSON 数组契约）/ `plain`（cec3/cec4/自训 COT 模型的「think 块 + 纠正句」）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmDetectConfig {
    /// OpenAI 兼容 API 地址，如 `https://api.openai.com/v1`
    pub base_url: String,
    /// API Key
    pub api_key: String,
    /// 模型名，如 `gpt-4o-mini`
    pub model: String,
    /// 连接与读超时（秒）
    pub timeout_secs: u64,
    /// system 提示词（检测模板）
    pub system: String,
    /// user 提示词模板（含 `{{text}}` 占位符）
    pub user_template: String,
    /// 输出格式（`json` / `plain`；缺省 `json`，向后兼容旧持久化配置）
    #[serde(default = "default_output_format")]
    pub output_format: String,
    /// 生成 max_tokens（0 = 服务端默认；CEC4 需 ≥ 1024 容纳 think 块）
    #[serde(default)]
    pub max_tokens: u32,
}

fn default_output_format() -> String {
    crate::models::OUTPUT_JSON.to_string()
}

impl LlmDetectConfig {
    /// 从各部件组装（api-server / 桌面端从各自的 LLM 配置 + 模板取值的统一入口）。
    /// 默认 `output_format = json`、`max_tokens = 0`；plain 模型用 [`Self::with_output_format`] 覆盖。
    pub fn from_parts(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
        timeout_secs: u64,
        system: impl Into<String>,
        user_template: impl Into<String>,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            api_key: api_key.into(),
            model: model.into(),
            timeout_secs: timeout_secs.max(1),
            system: system.into(),
            user_template: user_template.into(),
            output_format: default_output_format(),
            max_tokens: 0,
        }
    }

    /// 覆盖输出格式（`json` / `plain`）
    pub fn with_output_format(mut self, format: impl Into<String>) -> Self {
        self.output_format = format.into();
        self
    }

    /// 覆盖生成 max_tokens
    pub fn with_max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = n;
        self
    }
}

/// LLM 兜底检测引擎
pub struct LlmGecEngine {
    lang: String,
    client: reqwest::Client,
    config: LlmDetectConfig,
}

impl LlmGecEngine {
    pub fn new(lang: impl Into<String>, config: LlmDetectConfig) -> Self {
        let timeout = Duration::from_secs(config.timeout_secs.max(1));
        let client = reqwest::Client::builder()
            .connect_timeout(timeout)
            .read_timeout(timeout)
            .build()
            .unwrap_or_default();
        Self {
            lang: lang.into(),
            client,
            config,
        }
    }
}

/// LLM 响应中的单个错误项
#[derive(Debug, Deserialize)]
struct LlmErrorItem {
    #[serde(default)]
    error_type: String,
    #[serde(default = "default_confidence")]
    confidence: f64,
    #[serde(default)]
    original: String,
    #[serde(default)]
    correction: Option<String>,
    #[serde(default)]
    message: String,
}

fn default_confidence() -> f64 {
    0.5
}

#[derive(Debug, Deserialize)]
struct CompletionResponse {
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: Message,
}

#[derive(Debug, Deserialize)]
struct Message {
    content: Option<String>,
    /// thinking 模式服务端（llama.cpp Qwen3 / vllm 等）会把 think 块拆到该字段
    /// （`content` 只留最终答案）；存在时拼在 content 前还原完整输出
    #[serde(default)]
    reasoning_content: Option<String>,
}

/// 从 LLM 响应文本中容错提取 JSON 数组（剥离 ```json 围栏 / 前后杂散文本）
fn extract_json_array(content: &str) -> Option<String> {
    let t = content.trim();
    let t = t
        .strip_prefix("```json")
        .or_else(|| t.strip_prefix("```"))
        .unwrap_or(t)
        .trim();
    let t = t.strip_suffix("```").map(str::trim).unwrap_or(t);
    let start = t.find('[')?;
    let end = t.rfind(']')?;
    (end > start).then(|| t[start..=end].to_string())
}

/// 将 LLM 返回的 `original` 片段解析为原文字节偏移；定位失败时 `[0, 0]`。
///
/// 多个错误项引用同一/重叠片段时，按出现顺序依次认领各出现位置：
/// 候选 span 与 `claimed` 中任一已认领 span 相交时改取下一个出现位置；
/// 所有出现位置均被占用（或片段不在原文中）时返回 `[0, 0]`，
/// 由调用方按「宁留不丢」策略保留错误并在 message 标注。
fn resolve_span(text: &str, original: &str, claimed: &[[usize; 2]]) -> [usize; 2] {
    if original.is_empty() {
        return [0, 0];
    }
    for (start, _) in text.match_indices(original) {
        let span = [start, start + original.len()];
        if !claimed.iter().any(|c| c[0] < span[1] && span[0] < c[1]) {
            return span;
        }
    }
    [0, 0]
}

/// LLM 输出的 `error_type` → 统一内部错误类型（`shared_model::error_types`）。
///
/// zh_detect v1.2 起 prompt 使用 CEC（ACL 2026）教学分类；本映射只在解析边界
/// 做一次转换，教学分类不进入决策表 / 知识库：
/// - 错别字 → spelling；词语搭配错误 → collocation；
/// - 词性/语序/成分残缺/成分赘余/关联词/指代不明 → grammar；
/// - 语义逻辑不通 → logic（保持 v1.1 枚举的透传语义）；标点 → punctuation；
/// - 空 → grammar（兜底）；anomaly 与旧版英文 token（spelling/grammar/…）
///   及其他未知值原样透传（兼容其他模型的输出）。
fn map_llm_error_type(raw: &str) -> String {
    match raw.trim() {
        "" => error_types::GRAMMAR.to_string(),
        "错别字" => error_types::SPELLING.to_string(),
        "词语搭配错误" => error_types::COLLOCATION.to_string(),
        "词性错误" | "语序错误" | "成分残缺" | "成分赘余" | "关联词使用错误" | "指代不明" => {
            error_types::GRAMMAR.to_string()
        }
        "语义逻辑不通" => "logic".to_string(),
        "标点" => error_types::PUNCTUATION.to_string(),
        other => other.to_string(),
    }
}

// ── plain 输出适配（cec3 / cec4 / 自训 COT 模型）────────────────────────────
//
// 这类模型不输出 JSON，而是「think 块（错误类型 + 修改原因）+ 纠正句」纯文本。
// 适配器：① 剥离 think 块取出纠正句；② 纠正句与原文做字级 diff 生成 span；
// ③ 从 think 块解析错误类型 / 原因，映射到 Error{source: llm}。

/// 答案标记（think 块与纠正句的分界；取最后一次出现）
const ANSWER_MARKERS: [&str; 7] = [
    "修正",
    "纠正",
    "改正",
    "修改",
    "corrected",
    "correction",
    "correct",
];

fn is_marker_line(line: &str) -> bool {
    let l = line.trim_start();
    ANSWER_MARKERS
        .iter()
        .any(|m| l.starts_with(m) && l[m.len()..].starts_with(['：', ':']))
}

/// 从「think 块 + 纠正句」纯文本分离出纠正句，返回 `(think, corrected)`。
///
/// - 存在答案标记行（`<marker>[:：]`）时：标记之后（含后续行）为纠正句，之前为 think；
/// - 无标记时：最后一非空行为纠正句，之前为 think；
/// - 单行 / 空：整体（或空）即纠正句，think 为空。
fn split_think_answer(content: &str) -> (String, String) {
    let mut lines: Vec<String> = content
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    if lines.is_empty() {
        return (String::new(), String::new());
    }
    if let Some(idx) = lines.iter().rposition(|l| is_marker_line(l)) {
        let line = &lines[idx];
        let colon = line.find(['：', ':']).unwrap();
        let after_inline = line[colon..]
            .trim_start_matches(['：', ':'])
            .trim()
            .to_string();
        let mut answer = after_inline;
        if idx + 1 < lines.len() {
            let tail = lines[idx + 1..].join("\n");
            if !tail.is_empty() {
                answer = if answer.is_empty() {
                    tail
                } else {
                    format!("{answer}\n{tail}")
                };
            }
        }
        if !answer.is_empty() {
            return (lines[..idx].join("\n"), answer);
        }
    }
    let answer = lines.pop().unwrap();
    (lines.join("\n"), answer)
}

/// 解析 plain 答案中的 tab 回显格式。
///
/// CEC3/CEC4 对较长输入输出「纠正句\t原句回显」（实测复现于
/// zh_detect_cec 模板）。[`split_think_answer`] 只按行/标记切分，
/// 回显若留在答案里会被 [`char_diff_edits`] diff 成假插入错误（整句
/// 重复上报）。此处以原文为锚消解：含 `\t` 时按段剔除与原文相同的
/// 回显段，取剩余首段（实测纠正段在前）；所有段都等于原文（模型
/// 认为无误）时返回原文，由调用方按「无编辑」处理。
fn resolve_tab_echo(answer: &str, original: &str) -> String {
    if !answer.contains('\t') {
        return answer.to_string();
    }
    let segs: Vec<&str> = answer
        .split('\t')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if segs.len() <= 1 {
        return segs.first().copied().unwrap_or(answer).to_string();
    }
    let orig = original.trim();
    for seg in &segs {
        if *seg != orig && *seg != original {
            return (*seg).to_string();
        }
    }
    // 全部段都等于原文：无误判定
    orig.to_string()
}

/// 从 think 块解析「错误类型 / 修改原因」字段（解析不到时返回空串）
fn parse_think_fields(think: &str) -> (String, String) {
    let error_type = find_field(
        think,
        &["错误类型", "错误类别", "错误", "类型", "error_type", "type"],
    );
    let message = find_field(
        think,
        &["修改原因", "修改说明", "原因", "说明", "reason", "message"],
    );
    (error_type, message)
}

fn find_field(text: &str, keys: &[&str]) -> String {
    for line in text.lines() {
        let l = line.trim();
        for k in keys {
            if let Some(pos) = l.find(k) {
                let val = l[pos + k.len()..]
                    .trim_start_matches(['：', ':', ' '])
                    .trim()
                    .to_string();
                if !val.is_empty() {
                    return val;
                }
            }
        }
    }
    String::new()
}

/// plain 适配产出的单个编辑（原文区间 + 纠正文本）
#[derive(Debug, Clone, PartialEq)]
struct PlainEdit {
    /// 原文字节偏移 [start, end)
    orig_start: usize,
    orig_end: usize,
    /// 原文片段
    orig: String,
    /// 纠正文本（删除时为空串）
    corr: String,
}

/// 字级 diff（LCS 匹配块之间的间隙即编辑），产出编辑列表。
/// 原文 / 纠正完全相同时返回空。
fn char_diff_edits(original: &str, corrected: &str) -> Vec<PlainEdit> {
    let a: Vec<char> = original.chars().collect();
    let b: Vec<char> = corrected.chars().collect();
    let n = a.len();
    let m = b.len();
    let mut byte_a = vec![0usize; n + 1];
    for (k, c) in a.iter().enumerate() {
        byte_a[k + 1] = byte_a[k] + c.len_utf8();
    }
    let mut byte_b = vec![0usize; m + 1];
    for (k, c) in b.iter().enumerate() {
        byte_b[k + 1] = byte_b[k] + c.len_utf8();
    }
    // 后缀 LCS 动态规划
    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if a[i] == b[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    // 回溯收集匹配块（连续匹配合并为一块）
    let mut blocks: Vec<(usize, usize, usize)> = Vec::new();
    let mut i = 0;
    let mut j = 0;
    let mut start: Option<(usize, usize)> = None;
    let mut size = 0;
    while i < n && j < m {
        if a[i] == b[j] && dp[i][j] == dp[i + 1][j + 1] + 1 {
            if start.is_none() {
                start = Some((i, j));
            }
            size += 1;
            i += 1;
            j += 1;
        } else {
            if let Some((si, sj)) = start {
                blocks.push((si, sj, size));
                start = None;
                size = 0;
            }
            if dp[i + 1][j] >= dp[i][j + 1] {
                i += 1;
            } else {
                j += 1;
            }
        }
    }
    if let Some((si, sj)) = start {
        blocks.push((si, sj, size));
    }
    // 匹配块之间的间隙 → 编辑
    let mut edits = Vec::new();
    let mut prev_i = 0;
    let mut prev_j = 0;
    for (bi, bj, size) in &blocks {
        push_gap_edit(
            &mut edits, original, corrected, &byte_a, &byte_b, prev_i, prev_j, *bi, *bj,
        );
        prev_i = bi + size;
        prev_j = bj + size;
    }
    push_gap_edit(
        &mut edits, original, corrected, &byte_a, &byte_b, prev_i, prev_j, n, m,
    );
    edits
}

fn push_gap_edit(
    edits: &mut Vec<PlainEdit>,
    original: &str,
    corrected: &str,
    byte_a: &[usize],
    byte_b: &[usize],
    pi: usize,
    pj: usize,
    ci: usize,
    cj: usize,
) {
    if ci > pi || cj > pj {
        let o = &original[byte_a[pi]..byte_a[ci]];
        let c = &corrected[byte_b[pj]..byte_b[cj]];
        if o != c {
            edits.push(PlainEdit {
                orig_start: byte_a[pi],
                orig_end: byte_a[ci],
                orig: o.to_string(),
                corr: c.to_string(),
            });
        }
    }
}

#[async_trait]
impl GecEngine for LlmGecEngine {
    fn kind(&self) -> GecEngineKind {
        GecEngineKind::Llm
    }
    fn name(&self) -> &str {
        &self.config.model
    }

    async fn detect(&self, text: &str) -> Result<Vec<Error>, DetectorError> {
        if text.trim().is_empty() {
            return Ok(Vec::new());
        }
        // plain 模型（cec3/cec4/自训 COT）：think 块 + 纠正句，走专用适配
        if self.config.output_format == crate::models::OUTPUT_PLAIN {
            return self.detect_plain(text).await;
        }
        let user = self.config.user_template.replace("{{text}}", text);
        let content = self.chat_completion(&self.config.system, &user).await?;

        // 容错解析 JSON 数组；解析失败时告警并返回空（请求已成功，不算服务故障）
        let raw = match extract_json_array(&content) {
            Some(r) => r,
            None => {
                tracing::warn!(
                    model = %self.config.model,
                    content = %content.chars().take(200).collect::<String>(),
                    "LLM 兜底检测响应无法解析为 JSON 数组，返回空结果"
                );
                return Ok(Vec::new());
            }
        };
        let items: Vec<LlmErrorItem> = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    model = %self.config.model,
                    error = %e,
                    "LLM 兜底检测响应 JSON 解析失败，返回空结果"
                );
                return Ok(Vec::new());
            }
        };

        // 逐条认领 span：先处理的错误项优先，后续项引用同一/重叠片段时
        // 改取下一个出现位置，全部被占用则按 [0, 0] 标注（宁留不丢）
        let mut claimed: Vec<[usize; 2]> = Vec::new();
        let mut errors = Vec::with_capacity(items.len());
        for it in items {
            if it.original.is_empty() && it.correction.is_none() {
                continue;
            }
            let span = resolve_span(text, &it.original, &claimed);
            if span != [0, 0] {
                claimed.push(span);
            }
            let error_type = map_llm_error_type(&it.error_type);
            let message = if it.message.is_empty() {
                "LLM 兜底检出错误".to_string()
            } else if span == [0, 0] && !it.original.is_empty() {
                format!("{}（未能定位到原文位置）", it.message)
            } else {
                it.message
            };
            let mut e = Error::new(
                &self.lang,
                error_type,
                it.confidence.clamp(0.0, 1.0),
                span,
                it.original,
                it.correction,
                message,
            );
            e.source = Some(sources::LLM.to_string());
            errors.push(e);
        }
        Ok(errors)
    }
}

impl LlmGecEngine {
    /// plain 模型（cec3/cec4/自训 COT）检测：think 块 + 纠正句 → 字级 diff 生成 span。
    ///
    /// 纠正句与原文逐字 diff，匹配块之间的间隙即编辑（替换 / 插入 / 删除）；
    /// think 块解析出的错误类型 / 原因映射到每个编辑（CEC 10 类 → 内部 5 类
    /// 复用 [`map_llm_error_type`]）。纠正句与原文相同（模型认为无误）时返回空。
    async fn detect_plain(&self, text: &str) -> Result<Vec<Error>, DetectorError> {
        let user = self.config.user_template.replace("{{text}}", text);
        let content = self.chat_completion(&self.config.system, &user).await?;
        let (think, corrected) = split_think_answer(&content);
        let corrected = resolve_tab_echo(corrected.trim(), text);
        if corrected.is_empty() || corrected == text || corrected == text.trim() {
            return Ok(Vec::new());
        }
        let edits = char_diff_edits(text, &corrected);
        if edits.is_empty() {
            return Ok(Vec::new());
        }
        let (raw_type, parsed_msg) = parse_think_fields(&think);
        let error_type = map_llm_error_type(&raw_type);
        let mut errors = Vec::with_capacity(edits.len());
        for e in edits {
            let message = if parsed_msg.is_empty() {
                format!("原『{}』应为『{}』", e.orig, e.corr)
            } else {
                parsed_msg.clone()
            };
            let mut err = Error::new(
                &self.lang,
                error_type.clone(),
                0.8,
                [e.orig_start, e.orig_end],
                e.orig,
                Some(e.corr),
                message,
            );
            err.source = Some(sources::LLM.to_string());
            errors.push(err);
        }
        Ok(errors)
    }

    /// 通用 chat completion：给定 system / user 提示词，返回响应 content 文本。
    ///
    /// `detect` 的底层实现；也供上层（如 anomaly 模式蒸馏器）以任意提示词
    /// 复用同一 OpenAI 兼容客户端（temperature 0，非流式）。
    pub async fn chat_completion(&self, system: &str, user: &str) -> Result<String, DetectorError> {
        let url = format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        );
        let mut body = serde_json::json!({
            "model": self.config.model,
            "messages": [
                { "role": "system", "content": system },
                { "role": "user", "content": user }
            ],
            "stream": false,
            "temperature": 0.0,
        });
        // max_tokens > 0 时显式下发（CEC4 需 ≥ 1024 容纳 think 块）
        if self.config.max_tokens > 0 {
            body["max_tokens"] = serde_json::json!(self.config.max_tokens);
        }
        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.config.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| DetectorError::ServiceUnavailable(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(DetectorError::ServiceUnavailable(format!(
                "LLM 服务返回 HTTP {}",
                resp.status()
            )));
        }
        let cr: CompletionResponse = resp
            .json()
            .await
            .map_err(|e| DetectorError::ServiceUnavailable(e.to_string()))?;
        // thinking 模式服务端（llama.cpp Qwen3 / vllm 等）把 think 块拆进
        // `reasoning_content`、`content` 只留最终答案：拼回完整输出
        // （plain 分支的 think/答案分离依赖全文；JSON 分支的数组提取不受影响）
        let msg = match cr.choices.first() {
            Some(c) => &c.message,
            None => return Ok(String::new()),
        };
        let mut content = msg
            .content
            .clone()
            .filter(|c| !c.is_empty())
            .unwrap_or_default();
        if let Some(r) = &msg.reasoning_content {
            if !r.trim().is_empty() {
                content = format!("{}\n{}", r.trim_end(), content);
            }
        }
        Ok(content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;

    fn config(base_url: &str) -> LlmDetectConfig {
        LlmDetectConfig::from_parts(
            base_url,
            "sk-test",
            "mock-model",
            5,
            "你是校对专家",
            "找出以下文本中的错误：{{text}}",
        )
    }

    /// 本地 mock OpenAI 兼容服务（非流式）
    async fn mock_llm_server(content: &'static str) -> String {
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(move |body: axum::body::Bytes| async move {
                let req: serde_json::Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(req["model"], "mock-model");
                assert_eq!(req["stream"], false);
                let sys = req["messages"][0]["content"].as_str().unwrap_or("");
                let user = req["messages"][1]["content"].as_str().unwrap_or("");
                assert_eq!(sys, "你是校对专家");
                assert!(user.contains("找出以下文本中的错误："), "user 模板应已渲染");
                let resp = serde_json::json!({
                    "choices": [{ "message": { "content": content } }]
                });
                (axum::http::StatusCode::OK, axum::Json(resp)).into_response()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        format!("http://{addr}/v1")
    }

    #[tokio::test]
    async fn detect_parses_json_array() {
        let endpoint = mock_llm_server(
            r#"[{"error_type":"spelling","confidence":0.9,"original":"报复","correction":"抱负","message":"同音错别字"}]"#,
        )
        .await;
        let engine = LlmGecEngine::new("zh", config(&endpoint));
        assert_eq!(engine.kind(), GecEngineKind::Llm);
        assert_eq!(engine.name(), "mock-model");
        let errors = engine.detect("他昨天报复很大。").await.unwrap();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].lang, "zh");
        assert_eq!(errors[0].error_type, "spelling");
        assert_eq!(errors[0].confidence, 0.9);
        assert_eq!(errors[0].original, "报复");
        assert_eq!(errors[0].correction.as_deref(), Some("抱负"));
        // span 由 original 在原文中定位（CJK 每字 3 字节：「他昨天」= 9 字节）
        assert_eq!(errors[0].span, [9, 15]);
        assert_eq!(errors[0].source.as_deref(), Some("llm"));
    }

    #[tokio::test]
    async fn detect_strips_code_fence() {
        let endpoint = mock_llm_server(
            "以下是检测结果：\n```json\n[{\"error_type\":\"grammar\",\"confidence\":0.8,\"original\":\"不但\",\"correction\":\"不仅\",\"message\":\"搭配\"}]\n```\n以上。",
        )
        .await;
        let engine = LlmGecEngine::new("zh", config(&endpoint));
        let errors = engine.detect("他不但成绩提高。").await.unwrap();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].error_type, "grammar");
        assert_eq!(errors[0].span, [3, 9]);
    }

    #[tokio::test]
    async fn detect_unresolvable_span_kept_with_marker() {
        let endpoint = mock_llm_server(
            r#"[{"error_type":"grammar","confidence":0.7,"original":"不存在的片段","correction":null,"message":"疑似语病"}]"#,
        )
        .await;
        let engine = LlmGecEngine::new("zh", config(&endpoint));
        let errors = engine.detect("正确句子").await.unwrap();
        assert_eq!(errors.len(), 1, "定位失败的错误应保留（漏报场景宁留不丢）");
        assert_eq!(errors[0].span, [0, 0]);
        assert!(
            errors[0].message.contains("未能定位"),
            "message 应标注定位失败"
        );
    }

    /// 两条错误引用同一片段：按出现顺序各认领一个位置，span 不再重叠
    #[tokio::test]
    async fn detect_dedupes_repeated_citations() {
        let endpoint = mock_llm_server(
            r#"[{"error_type":"grammar","confidence":0.8,"original":"不但","correction":"不仅","message":"搭配1"},
                {"error_type":"grammar","confidence":0.7,"original":"不但","correction":"不仅","message":"搭配2"}]"#,
        )
        .await;
        let engine = LlmGecEngine::new("zh", config(&endpoint));
        // 「他」= 3 字节：第一个「不但」= [3, 9)，第二个 = [24, 30)
        let errors = engine
            .detect("他不但成绩提高，不但人也变好了。")
            .await
            .unwrap();
        assert_eq!(errors.len(), 2);
        assert_eq!(errors[0].span, [3, 9]);
        assert_eq!(errors[1].span, [24, 30]);
    }

    /// 后一条引用的片段与已认领 span 部分重叠且无其他出现位置：
    /// 保留错误但 span 置 [0, 0] 并标注（宁留不丢）
    #[tokio::test]
    async fn detect_overlapping_citation_falls_back_to_marker() {
        let endpoint = mock_llm_server(
            r#"[{"error_type":"grammar","confidence":0.8,"original":"不但成绩","correction":null,"message":"长片段"},
                {"error_type":"grammar","confidence":0.7,"original":"成绩提高","correction":null,"message":"重叠片段"}]"#,
        )
        .await;
        let engine = LlmGecEngine::new("zh", config(&endpoint));
        // 「他」= 3 字节：「不但成绩」= [3, 15)，「成绩提高」= [9, 21) 与之相交
        let errors = engine.detect("他不但成绩提高了。").await.unwrap();
        assert_eq!(errors.len(), 2);
        assert_eq!(errors[0].span, [3, 15]);
        assert_eq!(errors[1].span, [0, 0]);
        assert!(
            errors[1].message.contains("未能定位"),
            "message 应标注定位失败"
        );
    }

    #[tokio::test]
    async fn detect_empty_or_garbage_response_yields_empty() {
        let endpoint = mock_llm_server("[]").await;
        let engine = LlmGecEngine::new("zh", config(&endpoint));
        assert!(engine.detect("正确句子").await.unwrap().is_empty());

        let endpoint = mock_llm_server("抱歉，我无法完成该任务。").await;
        let engine = LlmGecEngine::new("zh", config(&endpoint));
        assert!(engine.detect("正确句子").await.unwrap().is_empty());
    }

    #[test]
    fn map_llm_error_type_converts_cec_taxonomy() {
        // CEC（ACL 2026）教学分类 → 统一内部类型
        assert_eq!(map_llm_error_type("错别字"), "spelling");
        assert_eq!(map_llm_error_type("词语搭配错误"), "collocation");
        for t in [
            "词性错误",
            "语序错误",
            "成分残缺",
            "成分赘余",
            "关联词使用错误",
            "指代不明",
        ] {
            assert_eq!(map_llm_error_type(t), "grammar");
        }
        assert_eq!(map_llm_error_type("语义逻辑不通"), "logic");
        assert_eq!(map_llm_error_type("标点"), "punctuation");
        // 空值兜底 grammar；anomaly / 旧版英文 token / 未知值原样透传
        assert_eq!(map_llm_error_type(""), "grammar");
        assert_eq!(map_llm_error_type("anomaly"), "anomaly");
        assert_eq!(map_llm_error_type("spelling"), "spelling");
        assert_eq!(map_llm_error_type("grammar"), "grammar");
        assert_eq!(map_llm_error_type("  错别字  "), "spelling");
        assert_eq!(map_llm_error_type("tense"), "tense");
    }

    #[tokio::test]
    async fn detect_maps_cec_taxonomy_in_items() {
        let endpoint = mock_llm_server(
            r#"[{"error_type":"错别字","confidence":0.9,"original":"报复","correction":"抱负","message":"原『报复』应为『抱负』"},
                {"error_type":"成分残缺","confidence":0.8,"original":"进行修订","correction":"进行了修订","message":"成分残缺"},
                {"error_type":"词语搭配错误","confidence":0.8,"original":"降低","correction":"缩小","message":"搭配不当"},
                {"error_type":"标点","confidence":0.7,"original":"。","correction":"，","message":"标点错误"},
                {"error_type":"语义逻辑不通","confidence":0.7,"original":"前后","correction":null,"message":"逻辑不通"}]"#,
        )
        .await;
        let engine = LlmGecEngine::new("zh", config(&endpoint));
        let errors = engine
            .detect("他报复了。进行修订。降低范围。前后。")
            .await
            .unwrap();
        assert_eq!(errors.len(), 5);
        assert_eq!(errors[0].error_type, "spelling");
        assert_eq!(errors[1].error_type, "grammar");
        assert_eq!(errors[2].error_type, "collocation");
        assert_eq!(errors[3].error_type, "punctuation");
        assert_eq!(errors[4].error_type, "logic");
    }

    #[tokio::test]
    async fn detect_unavailable_when_service_down() {
        let engine = LlmGecEngine::new("zh", config("http://127.0.0.1:1"));
        let err = engine.detect("x").await.unwrap_err();
        assert!(matches!(err, DetectorError::ServiceUnavailable(_)));
    }

    /// 通用 chat completion：任意 system / user 提示词，返回 content 原文
    /// （anomaly 模式蒸馏器等上层复用此入口）
    #[tokio::test]
    async fn chat_completion_returns_raw_content() {
        let content = "蒸馏结果：{\"pattern\":\"^好的\"}";
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(move |body: axum::body::Bytes| async move {
                let req: serde_json::Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(req["messages"][0]["role"], "system");
                assert_eq!(req["messages"][0]["content"], "蒸馏器 system");
                assert_eq!(req["messages"][1]["content"], "蒸馏器 user");
                let resp = serde_json::json!({
                    "choices": [{ "message": { "content": content } }]
                });
                (axum::http::StatusCode::OK, axum::Json(resp)).into_response()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        let engine = LlmGecEngine::new("zh", config(&format!("http://{addr}/v1")));
        let out = engine
            .chat_completion("蒸馏器 system", "蒸馏器 user")
            .await
            .unwrap();
        assert_eq!(out, content);
    }

    #[tokio::test]
    async fn chat_completion_empty_content_is_empty_string() {
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(move |_body: axum::body::Bytes| async move {
                let resp = serde_json::json!({
                    "choices": [{ "message": { "content": null } }]
                });
                (axum::http::StatusCode::OK, axum::Json(resp)).into_response()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        let engine = LlmGecEngine::new("zh", config(&format!("http://{addr}/v1")));
        assert_eq!(engine.chat_completion("s", "u").await.unwrap(), "");
    }

    // ── plain 输出适配 ────────────────────────────────────────────────

    #[test]
    fn split_think_answer_with_inline_marker() {
        // 标记行冒号后同行为纠正句
        let (think, ans) = split_think_answer(
            "错误类型：错别字\n修改原因：原『报复』应为『抱负』\n修正：他抱负很大。",
        );
        assert_eq!(think, "错误类型：错别字\n修改原因：原『报复』应为『抱负』");
        assert_eq!(ans, "他抱负很大。");
    }

    #[test]
    fn split_think_answer_marker_with_following_lines() {
        // 标记行冒号后为空，纠正句在后续行
        let (think, ans) = split_think_answer("分析：这里有搭配问题\n修正：\n他每天坚持锻炼。");
        assert_eq!(think, "分析：这里有搭配问题");
        assert_eq!(ans, "他每天坚持锻炼。");
    }

    #[test]
    fn split_think_answer_no_marker_uses_last_line() {
        // 无标记：最后一非空行为纠正句
        let (think, ans) = split_think_answer("先判断错误类型\n再给出原因\n他昨天本来要去。");
        assert_eq!(think, "先判断错误类型\n再给出原因");
        assert_eq!(ans, "他昨天本来要去。");
    }

    #[test]
    fn split_think_answer_single_line_is_answer() {
        let (think, ans) = split_think_answer("他今天很高兴。");
        assert_eq!(think, "");
        assert_eq!(ans, "他今天很高兴。");
    }

    #[test]
    fn resolve_tab_echo_strips_original_echo() {
        // CEC4 长输入实测格式：纠正句\t原句回显
        let orig = "公司有一只高素质管理团队。";
        let ans = "公司有一支高素质管理团队。\t公司有一只高素质管理团队。";
        assert_eq!(resolve_tab_echo(ans, orig), "公司有一支高素质管理团队。");
    }

    #[test]
    fn resolve_tab_echo_all_segments_equal_original() {
        // 模型认为无误：纠正段与回显段都等于原文 → 返回原文（无编辑）
        let orig = "他今天很高兴。";
        assert_eq!(
            resolve_tab_echo("他今天很高兴。\t他今天很高兴。", orig),
            orig
        );
    }

    #[test]
    fn resolve_tab_echo_no_tab_passthrough() {
        assert_eq!(
            resolve_tab_echo("他抱负很大。", "他报复很大。"),
            "他抱负很大。"
        );
    }

    #[test]
    fn resolve_tab_echo_picks_first_non_echo_segment() {
        // 多段：跳过与原文相同的回显段
        let orig = "原句内容。";
        let ans = "纠正内容。\t原句内容。\t原句内容。";
        assert_eq!(resolve_tab_echo(ans, orig), "纠正内容。");
    }

    #[test]
    fn char_diff_substitution_insertion_deletion() {
        // 替换：报复 → 抱负
        let e = char_diff_edits("他报复了。", "他抱负了。");
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].orig, "报复");
        assert_eq!(e[0].corr, "抱负");
        // span 应为「报复」的字节区间
        let text = "他报复了。";
        let start = text.find("报复").unwrap();
        assert_eq!(
            (e[0].orig_start, e[0].orig_end),
            (start, start + "报复".len())
        );

        // 插入：缺少「的」
        let e = char_diff_edits("猫在跑。", "猫在飞快地跑。");
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].orig, "");
        assert_eq!(e[0].corr, "飞快地");

        // 删除：多余「地」
        let e = char_diff_edits("他地跑。", "他跑。");
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].orig, "地");
        assert_eq!(e[0].corr, "");

        // 多处编辑（两个替换）
        let e = char_diff_edits("他报复了，进行修订。", "他抱负了，进行修正。");
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].orig, "报复");
        assert_eq!(e[0].corr, "抱负");
        assert_eq!(e[1].orig, "订");
        assert_eq!(e[1].corr, "正");
    }

    #[test]
    fn char_diff_identical_yields_no_edits() {
        assert!(char_diff_edits("他今天很高兴。", "他今天很高兴。").is_empty());
    }

    #[test]
    fn parse_think_fields_extracts_type_and_reason() {
        let (t, m) =
            parse_think_fields("错误类型：词语搭配错误\n修改原因：『降低』与『范围』搭配不当");
        assert_eq!(t, "词语搭配错误");
        assert_eq!(m, "『降低』与『范围』搭配不当");
        // 映射到内部类型
        assert_eq!(map_llm_error_type(&t), "collocation");
    }

    #[tokio::test]
    async fn detect_plain_end_to_end() {
        // CEC 风格：think 块（错误类型 + 原因）+ 纠正句
        let endpoint = mock_llm_server(
            "错误类型：错别字\n修改原因：原『报复』应为『抱负』\n修正：他抱负很大。",
        )
        .await;
        let engine = LlmGecEngine::new(
            "zh",
            config(&endpoint).with_output_format(crate::models::OUTPUT_PLAIN),
        );
        let errors = engine.detect("他报复很大。").await.unwrap();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].error_type, "spelling");
        assert_eq!(errors[0].original, "报复");
        assert_eq!(errors[0].correction.as_deref(), Some("抱负"));
        assert_eq!(errors[0].source.as_deref(), Some("llm"));
        assert!(errors[0].message.contains("抱负"));
        // span 落在「报复」
        let start = "他报复很大。".find("报复").unwrap();
        assert_eq!(errors[0].span, [start, start + "报复".len()]);
    }

    #[tokio::test]
    async fn detect_plain_no_change_yields_empty() {
        // 模型认为无误：纠正句 == 原文
        let endpoint = mock_llm_server("错误类型：无误\n修正：他今天很高兴。").await;
        let engine = LlmGecEngine::new(
            "zh",
            config(&endpoint).with_output_format(crate::models::OUTPUT_PLAIN),
        );
        assert!(engine.detect("他今天很高兴。").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn detect_plain_sends_max_tokens() {
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(|body: axum::body::Bytes| async move {
                let req: serde_json::Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(req["max_tokens"], 2048);
                let resp = serde_json::json!({
                    "choices": [{ "message": { "content": "修正：他抱负很大。" } }]
                });
                (axum::http::StatusCode::OK, axum::Json(resp)).into_response()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        let engine = LlmGecEngine::new(
            "zh",
            config(&format!("http://{addr}/v1"))
                .with_output_format(crate::models::OUTPUT_PLAIN)
                .with_max_tokens(2048),
        );
        let errors = engine.detect("他报复很大。").await.unwrap();
        assert_eq!(errors.len(), 1);
    }
}
