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
    /// 瞬时故障（网关排队 / 过载 / 传输层超时）的退避表（秒）：**长度即重试次数**
    ///
    /// 缺省 `[5, 20]`：真实网关是共享 / 单槽的，503 常常只是「没排到」，一次就降级
    /// 成规则层等于把可用结果丢掉；但前端 level ≥ 2 的请求超时是 180s
    /// （`web-ui/src/api/client.ts`），默认必须落在它之内。
    /// 环境变量 `LLM_RETRY_BACKOFF_SECS`（逗号分隔）可覆盖；空表 = 不重试。
    #[serde(default = "default_llm_retry_backoff")]
    pub retry_backoff_secs: Vec<u64>,
    /// 重试的总等待预算（秒）：排队可能比退避表长得多，预算封顶才不会把请求挂死
    #[serde(default = "default_llm_retry_budget")]
    pub retry_budget_secs: u64,
}

fn default_llm_retry_backoff() -> Vec<u64> {
    parse_backoff(std::env::var("LLM_RETRY_BACKOFF_SECS").ok().as_deref())
}

fn default_llm_retry_budget() -> u64 {
    std::env::var("LLM_RETRY_BUDGET_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(120)
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
            retry_backoff_secs: default_llm_retry_backoff(),
            retry_budget_secs: default_llm_retry_budget(),
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

    /// 覆盖瞬时故障重试策略（退避表 + 总预算）：后台链路（媒体 job）可比在线检测更激进
    pub fn with_retry(mut self, backoff_secs: Vec<u64>, budget_secs: u64) -> Self {
        self.retry_backoff_secs = backoff_secs;
        self.retry_budget_secs = budget_secs;
        self
    }
}

/// 纯解析（不碰环境变量，免得并行测试互相踩）
fn parse_backoff(raw: Option<&str>) -> Vec<u64> {
    const DEFAULT: [u64; 2] = [5, 20];
    match raw {
        Some(v) => {
            let parsed: Vec<u64> = v
                .split(',')
                .filter_map(|s| s.trim().parse::<u64>().ok())
                .collect();
            if parsed.is_empty() {
                DEFAULT.to_vec()
            } else {
                parsed
            }
        }
        None => DEFAULT.to_vec(),
    }
}

/// 排队 / 过载状态码：429（限流）与全部 5xx（网关过载给 502/503/504）
fn is_queue_status(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

/// `Retry-After` 的 delta-seconds 形式；HTTP-date 不猜（两端时钟未必对齐）
fn retry_after_secs(resp: &reqwest::Response) -> Option<u64> {
    resp.headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(|s| s.min(3600))
}

/// 重试决策（纯函数，便于测）：`Some(等待秒数)` 继续重试，`None` 就此收手
///
/// 网关给了 `retry_after` 就按它等（它知道自己排到哪，猜的退避只会短），
/// 但总等待不能越过预算——越过就是「一直等下去」，用户只会看到卡死。
fn next_wait(
    backoff: &[u64],
    attempt: usize,
    retry_after: Option<u64>,
    waited: u64,
    budget: u64,
) -> Option<u64> {
    let step = *backoff.get(attempt)?;
    let wait = retry_after.unwrap_or(0).max(step);
    if waited + wait > budget {
        None
    } else {
        Some(wait)
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

/// reqwest 传输层错误的完整原因链：`Display` 只到「error sending request for url (…)」，
/// 超时 / 连接重置藏在 `source` 链里——不带出来就只能靠猜（实测：真实载体 LLM 层
/// 反复失败，日志看不出是超时还是网络问题）
fn transport_chain(e: &reqwest::Error) -> String {
    let mut msg = e.to_string();
    let mut src = std::error::Error::source(e);
    while let Some(s) = src {
        msg.push('：');
        msg.push_str(&s.to_string());
        src = s.source();
    }
    if e.is_timeout() {
        msg.push_str("（请求超时）");
    }
    msg
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
    /// （`content` 只留最终答案）；`chat_completion()` 把它拼在答案前还原完整输出，
    /// JSON 契约走 `chat_completion_answer()`（不拼，见其文档）
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
    // 扫描所有**配平**的 `[...]`（字符串内的括号与转义不计入深度），取第一个非空数组；
    // 全是空数组时才返回空数组（`[]` 是「无错误」的合法答案）。
    // 不能取「首个 `[` 到末个 `]`」：思考模型会把指令原样回显（指令里就含 `[]`），
    // 那样会把说明文字吞进 JSON 导致解析失败 → 该层静默产出 0 条（实测 qwen 思考模型）。
    let mut depth: usize = 0;
    let mut start = 0usize;
    let mut in_str = false;
    let mut escaped = false;
    let mut first_empty: Option<String> = None;
    for (i, ch) in t.char_indices() {
        if in_str {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_str = false;
            }
            continue;
        }
        match ch {
            '"' => in_str = true,
            '[' => {
                if depth == 0 {
                    start = i;
                }
                depth += 1;
            }
            ']' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    let slice = &t[start..=i];
                    if slice
                        .chars()
                        .any(|c| !c.is_whitespace() && c != '[' && c != ']')
                    {
                        return Some(slice.to_string());
                    }
                    if first_empty.is_none() {
                        first_empty = Some(slice.to_string());
                    }
                }
            }
            _ => {}
        }
    }
    first_empty
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
        // JSON 契约只取最终答案：思考模型（qwen 思考档等）把思考块放在
        // `reasoning_content`，拼在前面会把指令回显里的 `[]` 吞进数组 → 解析失败 → 本层静默 0 条
        let content = self
            .chat_completion_answer(&self.config.system, &user)
            .await?;

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

    /// 通用 chat completion：给定 system / user 提示词，返回（最终答案 `content`，
    /// 思考块 `reasoning_content`）。
    ///
    /// thinking 模式服务端（llama.cpp Qwen3 / vllm 等）把 think 块拆进
    /// `reasoning_content`、`content` 只留最终答案，这里原样分开返回，
    /// 由调用方决定要不要把思考块拼进来。
    pub async fn chat_completion_parts(
        &self,
        system: &str,
        user: &str,
    ) -> Result<(String, String), DetectorError> {
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
        // 排队感知：网关过载 / 限流（429、5xx）与传输层超时都按「没排到」处理——
        // 按退避表与网关给的 `Retry-After` 重试。4xx（鉴权、路径、参数）不重试：
        // 那是要人修配置，重试只会把一次明确的失败拖成几分钟的卡死。
        let backoff = &self.config.retry_backoff_secs;
        let budget = self.config.retry_budget_secs;
        let mut attempt = 0usize;
        let mut waited = 0u64;
        let timeout_hint = format!(
            "（超时上限 {}s：调设置 `llm.timeout_secs` 或环境变量 `LLM_TIMEOUT_SECS`；\
             思考模型整载体检测实测要 60s 以上，默认 30s 必然失败）",
            self.config.timeout_secs.max(1)
        );
        let resp = loop {
            match self
                .client
                .post(&url)
                .bearer_auth(&self.config.api_key)
                .json(&body)
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => break r,
                Ok(r) => {
                    let status = r.status();
                    if !is_queue_status(status) {
                        return Err(DetectorError::ServiceUnavailable(format!(
                            "LLM 服务返回 HTTP {status}"
                        )));
                    }
                    let ra = retry_after_secs(&r);
                    match next_wait(backoff, attempt, ra, waited, budget) {
                        Some(wait) => {
                            tracing::warn!(
                                status = status.as_u16(),
                                wait_secs = wait,
                                attempt = attempt + 1,
                                "LLM 服务排队：退避后重试"
                            );
                            tokio::time::sleep(Duration::from_secs(wait)).await;
                            waited += wait;
                            attempt += 1;
                        }
                        None => {
                            return Err(DetectorError::ServiceUnavailable(format!(
                                "LLM 服务繁忙（HTTP {status}）：已重试 {attempt} 次、共等待 {waited}s \
                                 仍未放行（退避表 {backoff:?}、预算 {budget}s：\
                                 LLM_RETRY_BACKOFF_SECS / LLM_RETRY_BUDGET_SECS 可调）"
                            )));
                        }
                    }
                }
                Err(e) => {
                    // 传输层：超时 / 连不上算瞬时（排队时网关也会直接掐连接）；
                    // 其余（DNS、非法 URL…）不重试。
                    let chain = transport_chain(&e);
                    if !(e.is_timeout() || e.is_connect()) {
                        return Err(DetectorError::ServiceUnavailable(format!(
                            "{chain}{timeout_hint}"
                        )));
                    }
                    match next_wait(backoff, attempt, None, waited, budget) {
                        Some(wait) => {
                            tracing::warn!(
                                wait_secs = wait,
                                attempt = attempt + 1,
                                error = %chain,
                                "LLM 传输层瞬时故障：退避后重试"
                            );
                            tokio::time::sleep(Duration::from_secs(wait)).await;
                            waited += wait;
                            attempt += 1;
                        }
                        None => {
                            return Err(DetectorError::ServiceUnavailable(format!(
                                "{chain}{timeout_hint}（已重试 {attempt} 次、共等待 {waited}s）"
                            )));
                        }
                    }
                }
            }
        };
        let cr: CompletionResponse = resp
            .json()
            .await
            .map_err(|e| DetectorError::ServiceUnavailable(e.to_string()))?;
        let msg = match cr.choices.first() {
            Some(c) => &c.message,
            None => return Ok((String::new(), String::new())),
        };
        let answer = msg
            .content
            .clone()
            .filter(|c| !c.is_empty())
            .unwrap_or_default();
        let reasoning = msg.reasoning_content.clone().unwrap_or_default();
        Ok((answer, reasoning))
    }

    /// 通用 chat completion：把思考块拼在答案前，还原 COT 模型的完整输出
    /// （`plain` 分支的 think/答案分离依赖全文）。
    ///
    /// **要 JSON 契约的调用方请用 [`Self::chat_completion_answer`]**：思考块里
    /// 常回显指令原文（含 `[]`），拼在前面会让数组提取取错片段。
    pub async fn chat_completion(&self, system: &str, user: &str) -> Result<String, DetectorError> {
        let (answer, reasoning) = self.chat_completion_parts(system, user).await?;
        if reasoning.trim().is_empty() {
            return Ok(answer);
        }
        Ok(format!("{}\n{}", reasoning.trim_end(), answer))
    }

    /// 只取最终答案（思考块为空时回退思考块）：JSON 契约专用入口。
    pub async fn chat_completion_answer(
        &self,
        system: &str,
        user: &str,
    ) -> Result<String, DetectorError> {
        let (answer, reasoning) = self.chat_completion_parts(system, user).await?;
        if answer.trim().is_empty() {
            return Ok(reasoning);
        }
        Ok(answer)
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
        // 测试不真等退避（默认 [5,20] 会把失败类测试拖成 25s）；
        // 重试行为由 `with_retry(vec![0, 0], …)` 的专项测试覆盖
        .with_retry(Vec::new(), 0)
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

    /// 思考模型回归：最终答案在 `content`、思考块在 `reasoning_content`，而思考块里
    /// 会回显指令原文（含 `[]`）。把思考块拼在前面取数组会取到空数组或把说明吞进
    /// JSON → 解析失败 → 本层曾静默产出 0 条（实测 qwen 思考档 + zh_detect v1.2）。
    #[tokio::test]
    async fn detect_uses_answer_not_reasoning_for_thinking_models() {
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(move |_body: axum::body::Bytes| async move {
                let resp = serde_json::json!({
                    "choices": [{ "message": {
                        "content": "[{\"error_type\":\"错别字\",\"confidence\":0.98,\"original\":\"报复\",\"correction\":\"抱负\",\"message\":\"原『报复』应为『抱负』\"}]",
                        "reasoning_content": "要求：整段无误时直接输出 []。文本：找出以下文本中的错误：他昨天报复很大。\n需要判断错误类型……"
                    }}]
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
        let errors = engine.detect("他昨天报复很大。").await.unwrap();
        assert_eq!(errors.len(), 1, "思考块不得污染 JSON 解析");
        assert_eq!(errors[0].correction.as_deref(), Some("抱负"));
        assert_eq!(errors[0].span, [9, 15]);
        assert_eq!(errors[0].error_type, "spelling", "教学分类应在解析边界映射");
    }

    /// 服务端只填 `reasoning_content`（无 `content`）时仍要能出结果
    #[tokio::test]
    async fn detect_falls_back_to_reasoning_when_answer_empty() {
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(move |_body: axum::body::Bytes| async move {
                let resp = serde_json::json!({
                    "choices": [{ "message": {
                        "content": null,
                        "reasoning_content": "[{\"error_type\":\"spelling\",\"confidence\":0.9,\"original\":\"报复\",\"correction\":\"抱负\",\"message\":\"同音错别字\"}]"
                    }}]
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
        let errors = engine.detect("他昨天报复很大。").await.unwrap();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].original, "报复");
    }

    /// `chat_completion` 仍把思考块拼在答案前（`plain` 分支的 think/答案分离依赖完整输出）
    #[tokio::test]
    async fn chat_completion_still_prefixes_reasoning() {
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(move |_body: axum::body::Bytes| async move {
                let resp = serde_json::json!({
                    "choices": [{ "message": {
                        "content": "他抱负很大。",
                        "reasoning_content": "错误类型：错别字"
                    }}]
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
        assert_eq!(
            engine.chat_completion("s", "u").await.unwrap(),
            "错误类型：错别字\n他抱负很大。"
        );
    }

    /// JSON 契约入口只取最终答案（思考块不得进 JSON）
    #[tokio::test]
    async fn chat_completion_answer_returns_answer_only() {
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(move |_body: axum::body::Bytes| async move {
                let resp = serde_json::json!({
                    "choices": [{ "message": {
                        "content": "他抱负很大。",
                        "reasoning_content": "错误类型：错别字"
                    }}]
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
        assert_eq!(
            engine.chat_completion_answer("s", "u").await.unwrap(),
            "他抱负很大。"
        );
    }

    #[test]
    fn extract_json_array_prefers_balanced_non_empty_array() {
        // 思考文本回显指令里的 [] 不得顶掉真正的数组
        assert_eq!(
            extract_json_array(
                "整段无误时输出 []。文本：他昨天报复很大。\n[{\"original\":\"报复\"}]\n以上"
            )
            .as_deref(),
            Some("[{\"original\":\"报复\"}]")
        );
        // 字符串内的方括号不计入深度
        assert_eq!(
            extract_json_array("[{\"message\":\"见 [原文] 标注\"}] 收尾").as_deref(),
            Some("[{\"message\":\"见 [原文] 标注\"}]")
        );
        // 合法空数组仍是空数组（「无错误」的答案）
        assert_eq!(extract_json_array("结论：[]").as_deref(), Some("[]"));
        // 截断的数组不猜
        assert_eq!(
            extract_json_array("[{\"original\":\"报复\"").as_deref(),
            None
        );
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

    /// 超时必须把上限写进失败原因：否则「LLM 层失败」日志看不出该调哪个旋钮
    /// （真实载体整载体检测实测 60s+，默认 30s 必然超时）
    #[tokio::test]
    async fn detect_timeout_names_the_ceiling() {
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(move |_body: axum::body::Bytes| async move {
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                let resp = serde_json::json!({"choices": [{"message": {"content": "[]"}}]});
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
            LlmDetectConfig::from_parts(
                format!("http://{addr}/v1"),
                "sk-test",
                "mock-model",
                1,
                "你是校对专家",
                "找出以下文本中的错误：{{text}}",
            ),
        );
        let err = engine.detect("他昨天报复很大。").await.unwrap_err();
        let msg = match err {
            DetectorError::ServiceUnavailable(m) => m,
            other => panic!("应为服务不可用：{other:?}"),
        };
        assert!(msg.contains("超时上限 1s"), "{msg}");
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

    // ---- 排队感知（LLM 服务基本稳定，只是可能排队）----

    /// mock 网关：前 `fails` 次回 `status`（带 `Retry-After: 0`），之后正常回话。
    /// 返回 `(base_url, 请求次数)`——次数是关键断言：重试必须真的发生 / 真的没发生
    async fn flaky_llm_server(
        status: u16,
        fails: usize,
        content: &'static str,
    ) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(move |_body: axum::body::Bytes| {
                let counter = counter.clone();
                async move {
                    let n = counter.fetch_add(1, Ordering::SeqCst);
                    if n < fails {
                        let mut headers = axum::http::HeaderMap::new();
                        // 0：测试不真等（真实网关给的是几十秒到两分钟的排队窗口）
                        headers.insert(
                            axum::http::header::RETRY_AFTER,
                            axum::http::HeaderValue::from_static("0"),
                        );
                        return (
                            axum::http::StatusCode::from_u16(status).unwrap(),
                            headers,
                            "",
                        )
                            .into_response();
                    }
                    let resp = serde_json::json!({
                        "choices": [{ "message": { "content": content } }]
                    });
                    (axum::http::StatusCode::OK, axum::Json(resp)).into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        (format!("http://{addr}/v1"), hits)
    }

    #[tokio::test]
    async fn gateway_queueing_retries_instead_of_degrading() {
        // 关键立场：排队不是坏了。整轮降级成规则层 = 把可用结果丢掉
        use std::sync::atomic::Ordering;
        let (base, hits) = flaky_llm_server(503, 2, "[]").await;
        let engine = LlmGecEngine::new("zh", config(&base).with_retry(vec![0, 0], 60));
        let errors = engine.detect("测试文本").await.unwrap();
        assert!(errors.is_empty());
        assert_eq!(
            hits.load(Ordering::SeqCst),
            3,
            "排队两次后必须重试到放行（首次 + 2 次重试）"
        );
    }

    #[tokio::test]
    async fn queue_exhaustion_reports_attempts_and_wait() {
        use std::sync::atomic::Ordering;
        let (base, hits) = flaky_llm_server(503, usize::MAX, "[]").await;
        let engine = LlmGecEngine::new("zh", config(&base).with_retry(vec![0, 1], 60));
        let err = engine.detect("测试文本").await.unwrap_err().to_string();
        assert!(err.contains("HTTP 503"), "要说清是网关繁忙：{err}");
        assert!(err.contains("已重试 2 次"), "要给出重试了几次：{err}");
        assert!(err.contains("共等待 1s"), "要给出等了多久：{err}");
        assert_eq!(hits.load(Ordering::SeqCst), 3, "退避表用完就收手");
    }

    #[tokio::test]
    async fn non_queue_status_does_not_retry() {
        use std::sync::atomic::Ordering;
        let (base, hits) = flaky_llm_server(401, usize::MAX, "[]").await;
        let engine = LlmGecEngine::new("zh", config(&base).with_retry(vec![0, 0], 60));
        let err = engine.detect("测试文本").await.unwrap_err().to_string();
        assert!(err.contains("HTTP 401"), "{err}");
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "鉴权失败是要人修配置，重试只会把明确失败拖成卡死"
        );
    }

    #[test]
    fn next_wait_follows_gateway_hint_within_budget() {
        assert_eq!(next_wait(&[5, 20], 0, None, 0, 120), Some(5));
        assert_eq!(
            next_wait(&[5, 20], 0, Some(90), 0, 120),
            Some(90),
            "网关说等多久就等多久（猜的退避只会短）"
        );
        assert_eq!(next_wait(&[5, 20], 1, None, 90, 120), Some(20));
        assert_eq!(
            next_wait(&[5, 20], 1, None, 110, 120),
            None,
            "越过预算就收手"
        );
        assert_eq!(
            next_wait(&[5, 20], 2, None, 0, 120),
            None,
            "退避表用完 = 次数用完"
        );
        assert_eq!(next_wait(&[], 0, None, 0, 120), None, "空表 = 不重试");
    }

    #[test]
    fn backoff_table_parses_conservatively() {
        assert_eq!(
            parse_backoff(None),
            vec![5, 20],
            "默认必须落在前端 level ≥ 2 的 180s 超时之内"
        );
        assert_eq!(parse_backoff(Some("0,0")), vec![0, 0], "测试要免等待");
        assert_eq!(parse_backoff(Some(" 30 , 90 ")), vec![30, 90], "空白要容忍");
        assert_eq!(
            parse_backoff(Some("abc,x")).len(),
            2,
            "全是垃圾值必须回默认，不能退化成不重试"
        );
    }

    #[test]
    fn queue_status_covers_overload_and_rate_limit() {
        for s in [429u16, 500, 502, 503, 504] {
            assert!(
                is_queue_status(reqwest::StatusCode::from_u16(s).unwrap()),
                "{s} 应视为排队 / 过载"
            );
        }
        for s in [400u16, 401, 404, 422] {
            assert!(
                !is_queue_status(reqwest::StatusCode::from_u16(s).unwrap()),
                "{s} 是要人修配置，不是排队"
            );
        }
    }
}
