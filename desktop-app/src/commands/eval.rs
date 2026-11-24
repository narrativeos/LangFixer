pub(crate) fn eval_endpoint() -> String {
    std::env::var("GEC_EVAL_ENDPOINT")
        .map(|s| s.trim_end_matches('/').to_string())
        .unwrap_or_else(|_| "http://127.0.0.1:4807".to_string())
}

async fn eval_forward(
    method: &str,
    path: &str,
    body: Option<&serde_json::Value>,
    timeout: std::time::Duration,
) -> Result<serde_json::Value, String> {
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| format!("HTTP 客户端构建失败: {e}"))?;
    let mut req = client.request(
        method
            .parse::<reqwest::Method>()
            .map_err(|e| e.to_string())?,
        format!("{}{}", eval_endpoint(), path),
    );
    if let Some(b) = body {
        req = req.json(b);
    }
    let resp = req.send().await.map_err(|e| {
        format!("测评服务未运行（{path} 不可达：{e}；启动：cd datasets && python eval_server.py）")
    })?;
    let (code, body) = (resp.status(), resp.json::<serde_json::Value>().await);
    let body = body.map_err(|e| format!("测评服务响应解析失败: {e}"))?;
    if !code.is_success() {
        let msg = body
            .get("detail")
            .and_then(|v| v.as_str())
            .or_else(|| body.get("error").and_then(|v| v.as_str()))
            .unwrap_or("评测请求失败");
        return Err(format!("{msg}（HTTP {code}）"));
    }
    Ok(body)
}

/// 测评中心：发起评测（转发测评服务 POST /eval/run；body 透传）
#[tauri::command]
pub async fn run_eval(body: serde_json::Value) -> Result<serde_json::Value, String> {
    eval_forward(
        "POST",
        "/eval/run",
        Some(&body),
        std::time::Duration::from_secs(30),
    )
    .await
}

/// 测评中心：任务进度（转发 GET /eval/job/:id）
#[tauri::command]
pub async fn eval_job(id: String) -> Result<serde_json::Value, String> {
    eval_forward(
        "GET",
        &format!("/eval/job/{id}"),
        None,
        std::time::Duration::from_secs(10),
    )
    .await
}

/// 测评中心：历史报告列表（转发 GET /eval/reports）
#[tauri::command]
pub async fn list_eval_reports() -> Result<serde_json::Value, String> {
    eval_forward(
        "GET",
        "/eval/reports",
        None,
        std::time::Duration::from_secs(10),
    )
    .await
}

/// 测评中心：报告详情（转发 GET /eval/report/:id）
#[tauri::command]
pub async fn get_eval_report(id: String) -> Result<serde_json::Value, String> {
    eval_forward(
        "GET",
        &format!("/eval/report/{id}"),
        None,
        std::time::Duration::from_secs(10),
    )
    .await
}
