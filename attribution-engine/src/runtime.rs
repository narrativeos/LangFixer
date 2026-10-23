//! Zen 专用工作线程
//!
//! `zen_engine::Decision::evaluate` 返回的 future 不是 `Send`
//! （`DecisionGraph` 内部持有 `Arc<OnceCell>`），无法直接在
//! tokio 多线程运行时（如 axum handler）中 await。
//!
//! 本模块启动一个专用工作线程（内部运行 current-thread tokio 运行时），
//! 通过 tokio mpsc 提交求值任务、tokio oneshot 取回结果，从而让
//! [`crate::AttributionEngine::attribute`] 的 future 保持 `Send`。

use std::sync::Arc;
use std::sync::OnceLock;

use tokio::sync::{mpsc, oneshot};
use zen_engine::Variable;

pub type EvalResult = Result<serde_json::Value, String>;

struct Job {
    decision: Arc<zen_engine::Decision>,
    /// 以 JSON 传递输入（`Variable` 含 RcCell 非 Send，无法跨线程）
    input: serde_json::Value,
    reply: oneshot::Sender<EvalResult>,
}

/// Zen 求值工作线程（进程级单例）
pub struct ZenWorker {
    tx: mpsc::Sender<Job>,
}

impl ZenWorker {
    /// 启动工作线程（进程内仅一次）
    pub fn shared() -> Arc<Self> {
        static WORKER: OnceLock<Arc<ZenWorker>> = OnceLock::new();
        WORKER
            .get_or_init(|| {
                let (tx, rx) = mpsc::channel::<Job>(64);
                std::thread::Builder::new()
                    .name("zen-worker".into())
                    .spawn(move || {
                        let rt = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .expect("zen worker runtime");
                        rt.block_on(async move {
                            let mut rx = rx;
                            while let Some(job) = rx.recv().await {
                                let input = Variable::from(job.input);
                                let result = match job.decision.evaluate(input).await {
                                    Ok(resp) => serde_json::to_value(&resp.result)
                                        .map_err(|e| format!("结果序列化失败: {e}")),
                                    Err(e) => Err(e.to_string()),
                                };
                                let _ = job.reply.send(result);
                            }
                        });
                    })
                    .expect("zen worker thread");
                Arc::new(Self { tx })
            })
            .clone()
    }

    /// 提交求值任务，返回 oneshot 接收端（在任意 tokio 运行时中 await 即可）。
    ///
    /// 使用 `send().await` 而非 `try_send`：worker 线程独占求值，
    /// 队列满时在此背压等待，不会静默丢弃任务。
    pub async fn submit(
        &self,
        decision: &Arc<zen_engine::Decision>,
        input: serde_json::Value,
    ) -> oneshot::Receiver<EvalResult> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let job = Job {
            decision: decision.clone(),
            input,
            reply: reply_tx,
        };
        if self.tx.send(job).await.is_err() {
            // worker 线程已不可用：返回立即失败的结果
            let (tx, rx) = oneshot::channel();
            let _ = tx.send(Err("Zen 工作线程不可用".to_string()));
            return rx;
        }
        reply_rx
    }
}
