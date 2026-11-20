//! 请求级检测进度登记表：分阶段进度条的数据面
//!
//! 客户端在发起检测前自行生成 `progress_id`，经 `X-Progress-Id` 请求头随
//! POST /gec/detect（/gec/:lang/detect、/gec/:lang/analyze）传入；检测在引擎内
//! 推进时由 [`gec_core::StageSink`] 把阶段事件写入本表（键为 progress_id），
//! 前端轮询 GET /gec/detect/progress?id= 取事件序列渲染进度与每阶段耗时。
//! 缓存 / 历史命中路径没有阶段事件（瞬时返回），直接标记 finished。
//! 条目最后更新超过 [`STALE_AFTER`] 视为客户端已放弃，由下一次 begin 回收。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Query, State};
use gec_core::{StageEvent, StageSink};
use serde::Deserialize;

use crate::state::AppState;

/// 条目闲置回收时限：正常轮询会不断刷新 updated，只有客户端中断 / 页面关闭才会闲置
const STALE_AFTER: Duration = Duration::from_secs(600);

/// 单请求进度条目（events 为已序列化事件，路由直接透出）
pub struct Entry {
    events: Vec<serde_json::Value>,
    finished: bool,
    updated: Instant,
}

/// 开检：登记空条目（并回收长期闲置条目）
pub fn begin(map: &Mutex<HashMap<String, Entry>>, id: &str) {
    let mut g = map.lock().unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    g.retain(|_, e| now.duration_since(e.updated) < STALE_AFTER);
    g.insert(
        id.to_string(),
        Entry {
            events: Vec::new(),
            finished: false,
            updated: now,
        },
    );
}

/// 引擎阶段事件入表（sink 侧调用；条目不存在 = 已被回收 / 非法 id，静默丢弃）
pub fn emit(map: &Mutex<HashMap<String, Entry>>, id: &str, ev: StageEvent) {
    let finished = matches!(ev, StageEvent::End { .. });
    let Ok(mut g) = map.lock() else { return };
    if let Some(e) = g.get_mut(id) {
        e.updated = Instant::now();
        e.finished |= finished;
        e.events.push(serde_json::to_value(&ev).unwrap_or_default());
    }
}

/// 句表事件：与引擎侧 Units 同一序列化路径（同一全局切句器，前端矩阵行同源）
fn units_event(text: &str) -> serde_json::Value {
    let unit_spans = gec_core::docchunk::sentence_spans_with_origin(text);
    serde_json::to_value(StageEvent::Units {
        spans: unit_spans.iter().map(|(s, _)| *s).collect(),
        origins: unit_spans.iter().map(|(_, o)| o.as_str()).collect(),
    })
    .unwrap_or_default()
}

/// 无事件路径（缓存 / 历史命中）：合成句表事件后标记完成，轮询端即时收敛，
/// 块矩阵在缓存命中下同样有行（引擎已推送过事件则 finished，此处 no-op）
pub fn finish(map: &Mutex<HashMap<String, Entry>>, id: &str, text: &str) {
    let Ok(mut g) = map.lock() else { return };
    if let Some(e) = g.get_mut(id) {
        if !e.finished {
            e.events.push(units_event(text));
        }
        e.updated = Instant::now();
        e.finished = true;
    }
}

/// 无事件引擎兜底：纯规则等默认配置走 trait 默认 `detect_level_traced`（零事件），
/// fresh 路径结束后若条目仍未落定，用本次 timings 合成 start/stage/end 序列，
/// 使 Web 轮询侧同样收敛出阶段与耗时（缓存 / 历史路径已 `finish`，在此为 no-op；
/// 支持事件的引擎已 `finished`，同样 no-op）
pub fn fallback(
    map: &Mutex<HashMap<String, Entry>>,
    id: &str,
    text: &str,
    timings: &gec_core::DetectTimings,
) {
    let Ok(mut g) = map.lock() else { return };
    let Some(e) = g.get_mut(id) else { return };
    if e.finished {
        return;
    }
    // 与引擎侧计划声明同序（rule → decision → feature → knowledge → llm）；
    // 统一走 StageEvent 序列化，避免手写 JSON 与枚举 wire format 漂移
    let mut plan: Vec<&'static str> = Vec::new();
    let mut events: Vec<serde_json::Value> = Vec::new();
    let mut stage = |key: &'static str, ms: Option<u64>| {
        if ms.is_some() {
            plan.push(key);
            events.push(
                serde_json::to_value(StageEvent::Stage {
                    stage: key,
                    status: "ran",
                    ms,
                })
                .unwrap_or_default(),
            );
        }
    };
    stage("rule", Some(timings.rule_ms));
    stage("decision", timings.decision_ms);
    stage("feature", timings.l3_ms);
    stage("knowledge", timings.recall_ms);
    stage("llm", timings.llm_ms);
    let start = serde_json::to_value(StageEvent::Start { stages: plan }).unwrap_or_default();
    let end = serde_json::to_value(StageEvent::End {
        total_ms: timings.total_ms,
    })
    .unwrap_or_default();
    e.events.insert(0, start);
    e.events.extend(events);
    // 句表先于 end，与引擎侧 Units→End 顺序一致
    e.events.push(units_event(text));
    e.events.push(end);
    e.finished = true;
    e.updated = Instant::now();
}

/// 构造注入引擎的事件接收器（Arc 捕获 state，锁内仅 append，不做 IO）
pub fn sink(state: Arc<AppState>, id: &str) -> StageSink {
    let map = state.detect_progress.clone();
    let id = id.to_string();
    Arc::new(move |ev| emit(&map, &id, ev))
}

#[derive(Deserialize)]
pub struct ProgressQuery {
    pub id: String,
}

/// GET /gec/detect/progress?id= —— 事件快照。found=false 表示条目不存在
/// （POST 尚未到达路由 / 已回收），前端按未完成继续轮询即可
pub async fn progress(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ProgressQuery>,
) -> Json<serde_json::Value> {
    let g = state
        .detect_progress
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    match g.get(&q.id) {
        Some(e) => axum::Json(serde_json::json!({
            "found": true,
            "finished": e.finished,
            "events": e.events,
        })),
        None => axum::Json(serde_json::json!({
            "found": false,
            "finished": false,
            "events": [],
        })),
    }
}

/// 从请求头提取 progress_id（缺省 / 空值 = 不启用进度）
pub fn id_from_headers(headers: &axum::http::HeaderMap) -> Option<&str> {
    headers
        .get("x-progress-id")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emit_end_marks_finished_and_keeps_events() {
        let map: Mutex<HashMap<String, Entry>> = Mutex::new(HashMap::new());
        begin(&map, "p1");
        emit(
            &map,
            "p1",
            StageEvent::Start {
                stages: vec!["rule", "llm"],
            },
        );
        emit(
            &map,
            "p1",
            StageEvent::Stage {
                stage: "rule",
                status: "ran",
                ms: Some(12),
            },
        );
        {
            let g = map.lock().unwrap();
            let e = g.get("p1").unwrap();
            assert!(!e.finished);
            assert_eq!(e.events.len(), 2);
        }
        emit(&map, "p1", StageEvent::End { total_ms: 42 });
        let g = map.lock().unwrap();
        let e = g.get("p1").unwrap();
        assert!(e.finished);
        assert_eq!(e.events.len(), 3);
    }

    #[test]
    fn emit_to_unknown_id_is_dropped() {
        let map: Mutex<HashMap<String, Entry>> = Mutex::new(HashMap::new());
        emit(&map, "ghost", StageEvent::End { total_ms: 1 });
        assert!(map.lock().unwrap().is_empty());
    }

    #[test]
    fn fallback_synthesizes_events_when_engine_silent() {
        let map: Mutex<HashMap<String, Entry>> = Mutex::new(HashMap::new());
        begin(&map, "p1");
        let timings = gec_core::DetectTimings {
            rule_ms: 7,
            decision_ms: None,
            l3_ms: None,
            recall_ms: None,
            llm_ms: Some(30),
            total_ms: 40,
            ..Default::default()
        };
        fallback(&map, "p1", "甲。乙", &timings);
        let g = map.lock().unwrap();
        let e = g.get("p1").unwrap();
        assert!(e.finished);
        assert_eq!(e.events.len(), 5);
        assert_eq!(e.events[0]["stages"].as_array().unwrap().len(), 2);
        assert_eq!(e.events[1]["stage"], "rule");
        assert_eq!(e.events[2]["stage"], "llm");
        assert_eq!(e.events[3]["kind"], "units");
        assert_eq!(e.events[4]["total_ms"], 40);
        // 句表与全局切句器逐字同源（前端矩阵行不得自行切句）
        let expected = gec_core::docchunk::sentence_spans_with_origin("甲。乙");
        let spans: Vec<Vec<usize>> = expected.iter().map(|(s, _)| vec![s[0], s[1]]).collect();
        assert_eq!(e.events[3]["spans"], serde_json::json!(spans));
    }

    #[test]
    fn finish_synthesizes_units_and_is_idempotent() {
        let map: Mutex<HashMap<String, Entry>> = Mutex::new(HashMap::new());
        let text = "武汉大学图书馆闻名于世。东湖之滨建有新书馆。";
        begin(&map, "p1");
        finish(&map, "p1", text);
        // 幂等：重复 finish 不重复合成
        finish(&map, "p1", text);
        let g = map.lock().unwrap();
        let e = g.get("p1").unwrap();
        assert!(e.finished);
        assert_eq!(e.events.len(), 1);
        assert_eq!(e.events[0]["kind"], "units");
        let expected = gec_core::docchunk::sentence_spans_with_origin(text);
        let spans: Vec<Vec<usize>> = expected.iter().map(|(s, _)| vec![s[0], s[1]]).collect();
        assert_eq!(e.events[0]["spans"], serde_json::json!(spans));
    }

    #[test]
    fn fallback_noop_when_finished() {
        let map: Mutex<HashMap<String, Entry>> = Mutex::new(HashMap::new());
        begin(&map, "p1");
        finish(&map, "p1", "甲。");
        fallback(&map, "p1", "甲。", &gec_core::DetectTimings::default());
        let g = map.lock().unwrap();
        // finish 已合成一条 units，fallback 不得重复合成
        let e = g.get("p1").unwrap();
        assert_eq!(e.events.len(), 1);
        assert_eq!(e.events[0]["kind"], "units");
    }

    #[test]
    fn begin_purges_stale_entries() {
        let map: Mutex<HashMap<String, Entry>> = Mutex::new(HashMap::new());
        begin(&map, "old");
        // 人为老化：把 updated 推到回收时限之外
        {
            let mut g = map.lock().unwrap();
            g.get_mut("old").unwrap().updated =
                Instant::now() - STALE_AFTER - Duration::from_secs(1);
        }
        begin(&map, "new");
        let g = map.lock().unwrap();
        assert!(g.get("old").is_none());
        assert!(g.contains_key("new"));
    }
}
