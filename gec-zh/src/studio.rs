//! NarrativeStudio sidecar 客户端——**可选增强**：文档级指标（`POST /metrics`）
//! 与前后对比报告（`POST /diff-report`）。
//!
//! 照 HanLP 的优雅降级模式：离线 / 不可达时全部方法返回 `None` 并 `tracing::warn!`
//! 一次；宿主据此把报告文档级区渲染为「未连接」，检测正确性与 A/B 级命中不受影响。
//! 跨仓唯一共享物是 vendored 契约 `contract/studio-metrics.v1.json`（源
//! narrative-docs，用 scripts/sync-studio-contract.sh 刷新）；HOST_METRICS 登记表 + `tests/studio_contract_mirror.rs` 镜像测试保证归属不漂移。
//!
//! 契约姿态：sidecar 只回**数字与口径**，不回判断文案；owner=langfixer 的 id
//! 绝不上 wire，findings 由宿主自身生成。

use serde_json::{json, Value};

use crate::config::StudioConfig;

/// 宿主侧指标登记表：契约中**所有** `owner: langfixer` 的 id 与其 grade，逐项与
/// vendored 契约对齐。契约加了 id 而这里没登记、或这里登记了而契约里没有，镜像
/// 测试即红。这些 id 不经 sidecar——由宿主（gec-zh 规则层 / LLM 判读 / gold set
/// 负样本）实现与负责。
pub const HOST_METRICS: &[(&str, &str)] = &[
    // B/A 级 · 句内命中（gec-zh 规则层，`style/ai-tone` 类目）
    ("sentence.antithesis_contrast", "A"),
    ("sentence.leadin_colon", "A"),
    ("sentence.cliche_opener", "A"),
    ("sentence.dunhao_run", "B"),
    ("sentence.long_attributive", "B"),
    ("sentence.iso_adjacent", "B"),
    // 语义层四项（closed-criteria LLM 判读）
    ("semantic.synonym_rename", "B"),
    ("semantic.connective_chain", "B"),
    ("semantic.conclusion_colon", "B"),
    ("semantic.iso_triple", "F"),
    // X 级 · 永不改（gold set 负样本对齐）
    ("x.ordinal_heading", "X"),
    ("x.restatement_bridge", "X"),
    ("x.leadin_list", "X"),
    ("x.abstraction_shift", "X"),
    ("x.blacklist_jargon", "X"),
    ("x.connective_markers", "X"),
];

/// studio sidecar 客户端。构造廉价，宿主常驻一份即可。
pub struct StudioClient {
    endpoint: String,
    client: reqwest::Client,
    offline_only: bool,
}

impl StudioClient {
    pub fn new(config: StudioConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .build()
            .unwrap_or_default();
        Self {
            endpoint: config.endpoint.trim_end_matches('/').to_string(),
            client,
            offline_only: config.offline_only,
        }
    }

    /// 本形态是否会与 sidecar 对话（离线 = 否；报告据此直接渲染「未连接」）
    pub fn enabled(&self) -> bool {
        !self.offline_only
    }

    async fn get_json(&self, path: &str) -> Option<Value> {
        let url = format!("{}{path}", self.endpoint);
        match self.client.get(&url).send().await {
            Ok(r) if r.status().is_success() => r.json::<Value>().await.ok(),
            Ok(r) => {
                tracing::warn!(%url, status = %r.status(), "studio sidecar 返回异常，文档级增强降级");
                None
            }
            Err(e) => {
                tracing::warn!(%url, error = %e, "studio sidecar 不可达，文档级增强降级");
                None
            }
        }
    }

    async fn post_json(&self, path: &str, body: Value) -> Option<Value> {
        let url = format!("{}{path}", self.endpoint);
        match self.client.post(&url).json(&body).send().await {
            Ok(r) if r.status().is_success() => r.json::<Value>().await.ok(),
            Ok(r) => {
                // 4xx 多为口径问题（如 digest 不符），值得在日志里点名
                tracing::warn!(%url, status = %r.status(), "studio sidecar 返回异常，文档级增强降级");
                None
            }
            Err(e) => {
                tracing::warn!(%url, error = %e, "studio sidecar 不可达，文档级增强降级");
                None
            }
        }
    }

    /// 探活：`Some` = 在线，响应体 `{ contract_version, version, capabilities }`
    pub async fn healthz(&self) -> Option<Value> {
        if self.offline_only {
            return None;
        }
        self.get_json("/healthz").await
    }

    /// 文档级指标：出 `{ contract_version, sidecar_version, text_digest, coverage, metrics }`；
    /// `format` 缺省 markdown，`digest` 若给出必须等于服务端对 text 算的 sha256
    pub async fn metrics(
        &self,
        text: &str,
        format: Option<&str>,
        digest: Option<&str>,
    ) -> Option<Value> {
        if self.offline_only {
            return None;
        }
        let mut body = json!({ "text": text });
        if let Some(f) = format {
            body["format"] = json!(f);
        }
        if let Some(d) = digest {
            body["digest"] = json!(d);
        }
        self.post_json("/metrics", body).await
    }

    /// 前后对比报告：出 `{ markdown, html? }`
    pub async fn diff_report(
        &self,
        before: &str,
        after: &str,
        format: Option<&str>,
    ) -> Option<Value> {
        if self.offline_only {
            return None;
        }
        let mut body = json!({ "before": before, "after": after });
        if let Some(f) = format {
            body["format"] = json!(f);
        }
        self.post_json("/diff-report", body).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_registry_has_no_duplicate_ids() {
        let ids: std::collections::HashSet<&str> =
            HOST_METRICS.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids.len(), HOST_METRICS.len(), "登记表存在重复 id");
    }

    #[tokio::test]
    async fn offline_client_never_touches_network() {
        let client = StudioClient::new(StudioConfig {
            endpoint: "http://127.0.0.1:1".to_string(), // 即便可达也不该被访问
            timeout: std::time::Duration::from_millis(50),
            offline_only: true,
        });
        assert!(!client.enabled());
        assert!(client.healthz().await.is_none());
        assert!(client.metrics("测试文本。", None, None).await.is_none());
        assert!(client.diff_report("前", "后", None).await.is_none());
    }
}
