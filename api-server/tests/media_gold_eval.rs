//! 金集评测（`MULTIMODAL.md` §15 / §14 P1 验收）
//!
//! 两层，各自衡量不同东西，别混：
//!
//! 1. **上限评测**（离线、零凭据、零外部工具）：把 gold 的「错误或待核原文」当作
//!    **完美抽取**的文本单元喂给既有检测级联，再对回映射后的标注打分。
//!    这个数字衡量的是「检测 + 回映射」这一半的上限，**与视觉/OCR 质量无关**——
//!    它掉下来就说明内核或 remap 退化了。
//! 2. **真实评测**（`#[ignore]`）：`LANGFIXER_MEDIA_DIR` 指向原图目录 + 已配置视觉 LLM，
//!    跑完整 job 链路（A 档为空 → B 档视觉）再打分。这才是端到端召回/精确率。

use std::future::IntoFuture;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use api_server::{media_report, routes, state::AppState};
use media_core::{
    ApplyMode, CarrierReplay, CarrierSource, CarrierSpans, Case, CaseBase, ExtractSource, GoldItem,
    MatchKind, Region, ReportFile, ReportMeta, ReportRow, RuleVia, Stitched, TextUnit, attach_gold,
    attach_gold_by_carrier, attach_gold_spanned, candidates, gold_locate_spans,
    missed_rule_evidence, remap, reproduction, reproduction_by_carrier, reproduction_spanned,
    score_gold, score_gold_spanned, stitch_units, stitched_spans,
};

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn env_guard() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn gold_path() -> PathBuf {
    manifest_dir().join("../datasets/media_gold.json")
}

#[derive(serde::Deserialize)]
struct GoldFile {
    lang: String,
    items: Vec<GoldItem>,
    carriers: Vec<Carrier>,
}

#[derive(serde::Deserialize)]
struct Carrier {
    file: String,
    #[serde(default)]
    #[allow(dead_code)]
    title: String,
    #[serde(default)]
    items: Vec<u32>,
}

fn load_gold() -> GoldFile {
    let raw = std::fs::read_to_string(gold_path())
        .unwrap_or_else(|e| panic!("读金集失败 {}：{e}", gold_path().display()));
    serde_json::from_str(&raw).expect("金集 JSON 解析失败")
}

/// gold 条目 → 喂给检测内核的文本
///
/// 第 14 项的「原文」列写的是**说明**（`三处出现“…”`）而不是载体上的文字，
/// 直接喂进去等于喂一句不存在的话；这里取其引号内的实际文字，并按实际出现次数喂 3 份。
fn unit_texts(item: &GoldItem) -> Vec<String> {
    if item.original.starts_with("三处出现")
        && let Some((_, rest)) = item.original.split_once('“')
        && let Some((quoted, _)) = rest.split_once('”')
    {
        return vec![quoted.to_string(); 3];
    }
    vec![item.original.clone()]
}

/// 闭卷漏报 → 反思中心靶子（上限评测与真实重演**共用同一份写法**，避免两套口径）
///
/// `rules`：真实重演回填的「规则触发了但没 credited」（`report::missed_rule_evidence`），
/// 值为 `(rule_id, 归因来源)`，来源是 `positional`（坐标重合，强）或 `text`（原文子串重叠，弱）——
/// 有它，靶子才是 `rule_tune`（改规则）而不是「再补一条词典」；强弱必须一起带上。
/// 上限评测是闭卷跑的，没有任何触发记录，传空表。
fn write_reflection_seeds(
    path: &std::path::Path,
    source: &str,
    gold: &[GoldItem],
    credited: &std::collections::BTreeSet<u32>,
    text_hits: usize,
    positional_hits: usize,
    rules: &std::collections::BTreeMap<u32, (String, String)>,
) {
    let evidence = gec_core::GoldEvidenceIn {
        source: source.to_string(),
        gold_total: gold.len(),
        counted_total: gold.iter().filter(|g| g.counted).count(),
        text_hits,
        positional_hits,
        misses: gold
            .iter()
            .filter(|g| !credited.contains(&g.no))
            .map(|g| gec_core::GoldMissIn {
                no: g.no,
                original: g.original.clone(),
                suggestion: g.suggestion.clone(),
                location: g.location.clone(),
                verdict: g.verdict.clone(),
                note: g.note.clone(),
                counted: g.counted,
                rule_id: rules.get(&g.no).map(|(r, _)| r.clone()),
                rule_via: rules.get(&g.no).map(|(_, v)| v.clone()),
            })
            .collect(),
    };
    let seeds = gec_core::gold_suggestions(&evidence);
    for g in gold
        .iter()
        .filter(|g| g.counted && !credited.contains(&g.no))
    {
        assert!(
            seeds.iter().any(|s| {
                s.payload
                    .get("gold_nos")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().any(|x| x.as_u64() == Some(u64::from(g.no))))
                    .unwrap_or(false)
            }),
            "金集 #{}（计差错）没有靶子建议 → 反思中心接线坏了",
            g.no
        );
    }
    eprintln!(
        "\n=== 反思中心靶子（闭卷漏报 {} 条 → 建议 {} 条）===\n\
         写产物：{}",
        evidence.misses.len(),
        seeds.len(),
        path.display()
    );
    for s in &seeds {
        eprintln!(
            "  [{}] target={} priority={} · 证据 {} 条\n    {}",
            s.kind,
            s.target,
            s.priority,
            s.evidence.len(),
            s.summary
        );
    }
    std::fs::write(path, serde_json::to_string_pretty(&evidence).unwrap()).unwrap();
}

/// 闭环：重演产物 → 反思中心建议队列
///
/// 把 `reflection_seeds.json` 原样回灌 `/gec/reflect/run`（作为 `gold` 证据）。
/// 「重演跑完 → 建议队列里有对应靶子」必须在同一条命令里可验证，不能靠人工复制粘贴。
async fn post_seeds_to_reflection(base: &str, path: &std::path::Path, expect_nos: &[u32]) {
    let raw = std::fs::read_to_string(path).unwrap();
    let evidence: gec_core::GoldEvidenceIn = serde_json::from_str(&raw).unwrap();
    let resp = reqwest::Client::new()
        .post(format!("{base}/gec/reflect/run"))
        .json(&serde_json::json!({
            "lang": "zh",
            "include_eval": false,
            "gold": evidence,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "reflect/run 失败：{}",
        resp.text().await.unwrap()
    );
    let res: serde_json::Value = resp.json().await.unwrap();
    let queued: Vec<u64> = res["report"]["suggestions"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|s| {
            s["payload"]["gold_nos"]
                .as_array()
                .map(|a| a.iter().filter_map(|x| x.as_u64()).collect::<Vec<u64>>())
                .unwrap_or_default()
        })
        .collect();
    for no in expect_nos {
        assert!(
            queued.contains(&u64::from(*no)),
            "金集 #{no} 的漏报没有进建议队列：{queued:?}"
        );
    }
    assert_eq!(
        res["suggestion_ids"].as_array().unwrap().len(),
        res["report"]["suggestions"].as_array().unwrap().len(),
        "建议与队列 id 必须一一对应"
    );
    eprintln!(
        "  闭环：{} → /gec/reflect/run → 建议 {} 条，覆盖闭卷漏报 {:?}",
        path.file_name().unwrap().to_string_lossy(),
        res["report"]["suggestions"].as_array().unwrap().len(),
        expect_nos
    );
}

/// 起一个离线检测服务（只用本地规则，与生产同一条级联）
#[allow(clippy::await_holding_lock)]
async fn start_detect_server() -> String {
    let _guard = env_guard();
    unsafe {
        for k in [
            "LLM_API_KEY",
            "LLM_BASE_URL",
            "GEC_ENGINE_ZH",
            "GEC_ENGINE_EN",
            "GEC_STAT_ENDPOINT",
        ] {
            std::env::remove_var(k);
        }
        std::env::set_var("GEC_OFFLINE", "1");
        std::env::set_var("GEC_LEXICONS_DIR", manifest_dir().join("../lexicons"));
        std::env::set_var(
            "GEC_SETTINGS_FILE",
            std::env::temp_dir().join(format!("lf_gold_{}.json", uuid::Uuid::new_v4().simple())),
        );
    }
    let db = std::env::temp_dir().join(format!("lf_gold_{}.db", uuid::Uuid::new_v4().simple()));
    let state = Arc::new(AppState::from_env(
        Some(&manifest_dir().join("../decision-tables")),
        Some(&db),
    ));
    let app = routes::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, app).into_future());
    format!("http://{addr}")
}

/// 上限评测：完美抽取（gold 原文即单元）→ 检测 → remap → 打分
#[tokio::test]
async fn gold_upper_bound_with_perfect_extraction() {
    let gold = load_gold();
    let base = start_detect_server().await;

    // 「完美抽取」：每条 gold 的原文成为一个文本单元（无位置信息 → Unlocated）。
    // 另加一条**正对照**（本地 zh 词典必命中的错），用来证明评测管线本身能计分——
    // 否则「0 命中」到底是管线坏了还是规则覆盖不足，就分不清了。
    let mut units: Vec<TextUnit> = Vec::new();
    let mut next_id = 0u32;
    // 单元 → 载体序号：正误表要按载体排（专家的表就是图片1 在前、图片2 在后）
    let mut unit_carrier: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
    for (ci, carrier) in gold.carriers.iter().enumerate() {
        for no in &carrier.items {
            let Some(item) = gold.items.iter().find(|g| g.no == *no) else {
                panic!("金集载体 {ci} 引用了不存在的条目 #{no}");
            };
            for text in unit_texts(item) {
                next_id += 1;
                unit_carrier.insert(next_id, ci);
                units.push(TextUnit {
                    id: next_id,
                    text,
                    source: ExtractSource::Structure,
                    region: Region::Unlocated,
                    confidence: 1.0,
                    editable: false,
                    page: None,
                });
            }
        }
    }
    next_id += 1;
    let control_unit_id = next_id;
    // 正对照用 `ZH_PUNCTUATION`（出厂词表里 enabled = true，必命中）：
    // 它不在金集里，用来证明「评测管线本身能计分」。
    // `ZH_CONFUSABLE` / `ZH_MISSING_SUBJECT` 现已出厂开启（金集重演驱动的词表工厂改动），
    // 所以「报道/报到」这类同音误用能直接命中——金集闭卷召回不再为 0。
    units.push(TextUnit {
        id: control_unit_id,
        text: "住院患者请携带病历,到窗口办理。".to_string(),
        source: ExtractSource::Structure,
        region: Region::Unlocated,
        confidence: 1.0,
        editable: false,
        page: None,
    });
    let (stitched, cleaned) = stitch_units(&units);
    assert!(
        stitched.text.chars().count() > 200,
        "金集文本过短说明读取有问题：{} 字符",
        stitched.text.chars().count()
    );

    // 与生产同一条检测级联。默认火力全开（level 3 + force_llm）；
    // LLM 层未配置时按 media 路径同样的口径降级为规则 + 决策模型，并在输出里说明——
    // 「跑不动」必须显式可见，不能变成「没错误」，也不能像 /gec/detect 那样直接 503。
    let llm_ready = std::env::var("LLM_API_KEY").is_ok_and(|v| !v.is_empty())
        && std::env::var("LLM_BASE_URL").is_ok_and(|v| !v.is_empty());
    let want_level: u8 = std::env::var("LANGFIXER_REPLAY_LEVEL")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);
    let (level, force_llm) = if llm_ready {
        (want_level, true)
    } else {
        (1, false)
    };
    println!(
        "检测级别：{level}（force_llm={force_llm}）；LLM 层：{}",
        if llm_ready {
            "已配置"
        } else {
            "未配置 → 降级为规则 + 决策模型"
        }
    );
    let resp = reqwest::Client::new()
        .post(format!("{base}/gec/detect"))
        .json(&serde_json::json!({
            "lang": gold.lang,
            "text": stitched.text,
            "level": level,
            "force_llm": force_llm,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "detect 失败：{}",
        resp.text().await.unwrap()
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    let errors: Vec<shared_model::Error> =
        serde_json::from_value(body["errors"].clone()).expect("errors 反序列化");
    // 检测路径（哪一层实际参与）——评测报告必须带上，否则「0 检出」无从归因
    eprintln!(
        "检测路径：{}",
        serde_json::to_string(&body["layers"]).unwrap_or_default()
    );

    let anns = remap(&stitched, &cleaned, &errors, ApplyMode::ListOnly);

    // 字节级不变量必须在真实中文语料上成立（CJK 偏移的回归防线）
    for a in &anns {
        if a.locate_warning.is_some() {
            continue;
        }
        let (_, [u0, u1]) = stitched
            .unit_ranges
            .iter()
            .find(|(uid, _)| *uid == a.unit_id)
            .expect("unit_ranges 必须含该单元");
        let (l0, l1) = (a.local_span[0], a.local_span[1]);
        assert!(l1 <= u1 - u0, "局部 span 越出单元边界：{l0},{l1}");
        assert_eq!(
            &stitched.text[u0 + l0..u0 + l1],
            a.error.original,
            "回映射字节必须与 error.original 一致（gold 语料 CJK 偏移）"
        );
    }

    let score = score_gold(&gold.items, &anns, 0.5);

    // ── 三种口径分开报（混在一起，数字就没有意义）────────────────────────────
    // ① 开卷 · 案例库：把「已确认过的判定」当案例回放。命中 = **记忆**，不是能力；
    //    它证明的是「抽取 → 定位 → 回映射 → 导出」这条链路无损（回归门 + 学习闭环可用）。
    // ② 闭卷 · 文本口径：只靠检测内核，按文本相似计分（就是上面的 `score`）。
    // ③ 闭卷 · 位置口径：文本口径判不上、但字节区间重合（专家引整句、我们标词级片段）
    //    的记 `MatchKind::Positional`——「同一处判定」的口径。
    let mut casebase = CaseBase::default();
    let mut case_gold: Vec<u32> = Vec::new();
    for item in &gold.items {
        for text in unit_texts(item) {
            casebase.cases.push(Case {
                original: text,
                suggestion: item.suggestion.clone(),
                counted: item.counted,
                note: item.note.clone(),
                source: "gold".to_string(),
            });
            case_gold.push(item.no);
        }
    }
    let mut case_hit: Vec<u32> = casebase
        .match_text(&stitched.text)
        .iter()
        .map(|h| case_gold[h.case])
        .collect();
    case_hit.sort_unstable();
    case_hit.dedup();

    // gold 引用在载体文本中的字节区间；找不到就是抽取保真度问题（标 None，绝不编坐标）。
    // 定位器与真实重演共用同一个（`gold::locate_spans`）——两套定位器 = 两套口径
    let gold_cands: Vec<Vec<String>> = gold.items.iter().map(unit_texts).collect();
    let gold_spans: Vec<Option<(usize, usize)>> = gold_locate_spans(&stitched.text, &gold_cands);
    let gold_unlocated = gold_spans.iter().filter(|s| s.is_none()).count();
    // 我们的判定换算成 stitched 坐标（单元起点 + local_span，与下面的字节不变量同一算法）
    let unit_start: std::collections::HashMap<u32, usize> = stitched
        .unit_ranges
        .iter()
        .map(|(id, r)| (*id, r[0]))
        .collect();
    let ours_spanned: Vec<(u32, String, (usize, usize))> = anns
        .iter()
        .filter_map(|a| {
            let base = *unit_start.get(&a.unit_id)?;
            Some((
                a.id,
                a.error.original.clone(),
                (base + a.local_span[0], base + a.local_span[1]),
            ))
        })
        .collect();
    let span_score = score_gold_spanned(&gold.items, &gold_spans, &ours_spanned, 0.5);
    let counted_hits = |s: &media_core::GoldScore| {
        s.per_gold
            .iter()
            .filter(|(no, _, kind, _)| {
                *kind != MatchKind::None && gold.items.iter().any(|g| g.no == *no && g.counted)
            })
            .count()
    };
    eprintln!(
        "\n=== 复现口径（三个数各自回答不同的问题）===\n\
         ① 开卷 · 案例库（记忆，不是能力）：{}/{} 条 —— 只证明抽取→定位→回映射→导出无损\n\
         ② 闭卷 · 文本口径：{}/{} 条（计错项 {}/{}，加权召回 {:.1}%）\n\
         ③ 闭卷 · 位置口径：{}/{} 条（计错项 {}/{}，加权召回 {:.1}%）\n\
         gold 引用未能在载体文本中定位：{gold_unlocated} 条（抽取保真度问题，不参与位置口径）",
        case_hit.len(),
        gold.items.len(),
        score.matched,
        score.gold_total,
        counted_hits(&score),
        score.gold_counted,
        score.recall.max(0.0) * 100.0,
        span_score.matched,
        span_score.gold_total,
        counted_hits(&span_score),
        span_score.gold_counted,
        span_score.recall.max(0.0) * 100.0,
    );
    for (no, ann_id, kind, ratio) in &span_score.per_gold {
        if *kind == MatchKind::Positional {
            let text = gold
                .items
                .iter()
                .find(|g| g.no == *no)
                .map(|g| g.original.chars().take(24).collect::<String>())
                .unwrap_or_default();
            eprintln!(
                "  位置口径新增命中 #{no:<2}（词级片段落在专家引用内）重叠 {ratio:.2}  ← ann{ann_id:?}  {text}"
            );
        }
    }
    let verdict = |no: u32| {
        gold.items
            .iter()
            .find(|g| g.no == no)
            .map(|g| g.verdict.as_str())
            .unwrap_or("?")
    };
    eprintln!(
        "\n=== 金集上限评测（完美抽取 → 检测 → remap） ===\ngold {} 条（计差错 {}），我们 {} 条，命中 {} 条\n加权召回 {:.1}%，精确率 {:.1}%",
        score.gold_total,
        score.gold_counted,
        score.ours,
        score.matched,
        score.recall.max(0.0) * 100.0,
        score.precision.max(0.0) * 100.0
    );
    for (no, ann_id, kind, ratio) in &score.per_gold {
        eprintln!(
            "  #{no:<2} {:<12} {:?} 重叠 {:.2}{}",
            verdict(*no),
            kind,
            ratio,
            ann_id.map(|i| format!("  ← ann#{i}")).unwrap_or_default()
        );
    }
    if !score.unmatched_ours.is_empty() {
        eprintln!(
            "  未覆盖 gold 的产出（多报，或 gold 未列的真错，含正对照）：{:?}",
            score.unmatched_ours
        );
    }

    // 管线本身必须能计分：正对照句必须被检出并回映射
    assert!(
        anns.iter().any(|a| a.unit_id == control_unit_id),
        "正对照句（半角逗号）未被检出 → 评测管线或 zh 标点规则坏了，而不是覆盖问题"
    );
    // 正对照不在金集里：它必须落在 unmatched_ours，不能被「包含」巧合白送一条 gold
    let control_ids: Vec<u32> = anns
        .iter()
        .filter(|a| a.unit_id == control_unit_id)
        .map(|a| a.id)
        .collect();
    assert!(
        control_ids
            .iter()
            .all(|id| score.unmatched_ours.contains(id)),
        "单字符标点错误不得按包含匹配到 gold 整句：{:?}",
        score.per_gold
    );
    // 数据集完整性（条目数 / 计差错数变了，说明金集被改动，评测口径要重新对齐）
    assert_eq!(score.gold_total, 15, "金集条目数");
    assert_eq!(score.gold_counted, 7, "金集计差错数");

    // 实测结论（离线：本地规则 + 决策模型，无 LLM）：金集 15 条里
    // 文本口径命中 2 条（#9「下午15点」= `ZH_TIME_MIX`、#14 微信小程序冒号 = `ZH_COLON_AFTER_NAME`），
    // 位置口径再加 1 条（#13「报道/报到」= `ZH_CONFUSABLE` 的词级片段落在专家整句引用内）
    // → 计错项 3/7。剩下的集中在搭配 / 语义判断（#3 并列缺分隔、#8/#11/#12 主谓宾残缺、
    // #15 表述不清），那些是 LLM 层与反思中心的靶子，不是再加几条词典能解决的。
    // 因此多模态载体的召回瓶颈**不在抽取，而在中文判定覆盖 + 语义层**：
    // 真实召回要靠 LLM 层（level 2/3）+ 视觉抽取，见本文件末尾的真实评测。
    eprintln!(
        "  说明：本地规则层对金集召回 {:.1}%（覆盖不足，非管线故障）；\n  带 LLM 层请跑：LANGFIXER_MEDIA_DIR=<原图目录> cargo test -p api-server --test media_gold_eval -- --ignored --nocapture",
        score.recall.max(0.0) * 100.0
    );

    // —— 重演三段式 ①：把推演结果写成 candidates.json（人工勾选的入口）+ 正误表 ——
    // 目的：让「基于当前软件重演专家结果」变成一条可跑的命令，而不是口头承诺。
    let replay = std::env::var("LANGFIXER_REPLAY_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| manifest_dir().join("../target/media-replay"));
    let labels: Vec<String> = (0..gold.carriers.len())
        .map(|i| format!("图片{}", i + 1))
        .collect();
    let per_carrier_units: Vec<Vec<TextUnit>> = gold
        .carriers
        .iter()
        .enumerate()
        .map(|(ci, _)| {
            units
                .iter()
                .filter(|u| unit_carrier.get(&u.id) == Some(&ci))
                .cloned()
                .collect()
        })
        .collect();
    let per_carrier_anns: Vec<Vec<_>> = gold
        .carriers
        .iter()
        .enumerate()
        .map(|(ci, _)| {
            anns.iter()
                .filter(|a| unit_carrier.get(&a.unit_id) == Some(&ci))
                .cloned()
                .collect()
        })
        .collect();
    let sources: Vec<CarrierSource> = gold
        .carriers
        .iter()
        .enumerate()
        .map(|(ci, c)| CarrierSource {
            job_id: c.file.as_str(),
            label: &labels[ci],
            size: None,
            extraction: "gold",
            annotations: &per_carrier_anns[ci],
            units: &per_carrier_units[ci],
        })
        .collect();
    // 不属于任何载体的标注（上限评测里的正对照句）也必须出现在表里——
    // 悄悄丢掉就等于「报告比检测结果少」，而读者无从知道为什么
    let orphan_anns: Vec<_> = anns
        .iter()
        .filter(|a| !unit_carrier.contains_key(&a.unit_id))
        .cloned()
        .collect();
    let orphan_units: Vec<TextUnit> = units
        .iter()
        .filter(|u| !unit_carrier.contains_key(&u.id))
        .cloned()
        .collect();
    let orphan_label = "未归载体（正对照）".to_string();
    let sources: Vec<CarrierSource> = if orphan_anns.is_empty() {
        sources
    } else {
        let mut v = sources;
        v.push(CarrierSource {
            job_id: "control",
            label: &orphan_label,
            size: None,
            extraction: "gold",
            annotations: &orphan_anns,
            units: &orphan_units,
        });
        v
    };
    let mut rows = candidates(&sources);
    // 导出表的 gold 回填必须与评测同一把尺子，否则「表里 2 条 / 评测输出 3 条」
    // 会在同一份产物里打架。先文本口径，再位置口径（后者只补前者的漏，并留痕口径）
    let replay_score = attach_gold(&mut rows, &gold.items, 0.5);
    assert_eq!(
        replay_score.matched, score.matched,
        "报告口径与标注口径必须同一把尺子（命中数不一致说明两套实现）"
    );
    // 行上的坐标是单元内坐标（`local_span`），换算成 stitched 全局坐标要用单元起点——
    // 与上面 `ours_spanned` 同一算法，两套算法必须一致，否则位置口径就是自欺
    let row_spans: Vec<Option<(usize, usize)>> = rows
        .iter()
        .map(|r| {
            let ls = r.local_span?;
            let base = *unit_start.get(&r.unit_id)?;
            Some((base + ls[0], base + ls[1]))
        })
        .collect();
    let span_replay = attach_gold_spanned(&mut rows, &gold.items, &gold_spans, &row_spans, 0.5);
    assert_eq!(
        span_replay.matched, span_score.matched,
        "导出表的位置口径与评测的位置口径必须同一把尺子"
    );
    let pos_rows = rows
        .iter()
        .filter(|r| r.gold_match.as_deref() == Some("positional"))
        .count();
    assert!(
        pos_rows >= 1,
        "至少 #13「报道/报到」应通过位置口径回填到导出表"
    );
    // 四维复现率：命中只是第一层，位置 / 计错数 / 建议三个维度必须分开看。
    // 用**位置口径**对齐命中（与导出表同一套坐标）：位置口径命中的行也进四维，
    // 于是 #13「报道」计入「计错数一致」，但「原文对齐」仍不达标（词级片段 ≠ 专家整句）——
    // 计入同一处判定，不等于假装原文一致
    let repro = reproduction_spanned(&rows, &gold.items, &gold_spans, &row_spans, 0.5);
    assert_eq!(
        repro.matched, span_replay.matched,
        "四维口径与导出表位置口径必须同一把尺子"
    );
    let repro_text = reproduction(&rows, &gold.items, 0.5);
    assert_eq!(
        repro_text.matched, replay_score.matched,
        "文本口径四维与文本口径命中必须同数"
    );
    assert!(
        repro.counted_ok >= repro_text.counted_ok && repro.matched >= repro_text.matched,
        "位置口径只会补命中，不能把文本口径已认的判定弄丢"
    );
    eprintln!(
        "\n=== 复现质量（四维，逐维给数，不合成一个含糊的百分比）===\n\
         原文对齐 {}/{} · 位置一致 {}/{} · 计错数一致 {}/{} · 建议接近 {}/{} → 严格复现 {}/{}\n\
         四维按**位置口径**对齐命中（文本口径命中 {}/{}）：位置口径补进来的条目计入计错数维，\n\
         但原文维不达标（词级片段 ≠ 专家整句）——计入同一处判定不等于假装原文一致。\n\
         位置一致维为 0 是因为上限评测的单元没有载体坐标（Region::Unlocated），不是漏报；\n\
         真实位置维数字来自真实载体重演：cargo test -p api-server --test media_gold_eval -- --ignored --nocapture",
        repro.original_ok,
        repro.gold_total,
        repro.location_ok,
        repro.gold_total,
        repro.counted_ok,
        repro.gold_total,
        repro.suggestion_ok,
        repro.gold_total,
        repro.full_ok,
        repro.gold_total,
        repro_text.matched,
        repro_text.gold_total,
    );
    // —— 反思中心接线：闭卷漏报必须变成**可执行靶子**，不能只留一句「召回偏低」——
    // 漏报按靶子分类（局部可形式化 → 词表条件；结构性 → L3 样本；编辑判断 → LLM 层），
    // 产物 `reflection_seeds.json` 可直接 POST 到 /gec/reflect/run 的 `gold` 字段落进建议队列。
    let credited: std::collections::BTreeSet<u32> = span_score
        .per_gold
        .iter()
        .filter(|(_, ann, kind, _)| *kind != MatchKind::None && ann.is_some())
        .map(|(no, ..)| *no)
        .collect();
    let miss_nos: Vec<u32> = gold
        .items
        .iter()
        .filter(|g| g.counted && !credited.contains(&g.no))
        .map(|g| g.no)
        .collect();
    write_reflection_seeds(
        &replay.join("reflection_seeds.json"),
        "media_gold@2026-10-26 · 上限评测 · 离线 level 1",
        &gold.items,
        &credited,
        score.matched,
        span_score.matched,
        &std::collections::BTreeMap::new(),
    );
    post_seeds_to_reflection(&base, &replay.join("reflection_seeds.json"), &miss_nos).await;

    let meta = ReportMeta {
        title: "文字差错核对表（金集重演 · 上限评测）".to_string(),
        lang: gold.lang.clone(),
        generated_at: chrono::Utc::now().to_rfc3339(),
        carriers: gold
            .carriers
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{}：{}", labels[i], c.file))
            .chain(
                std::iter::once(format!("{orphan_label}：上限评测的正对照句"))
                    .filter(|_| !orphan_anns.is_empty()),
            )
            .collect(),
        extractions: {
            let mut e: Vec<String> =
                vec!["gold（完美抽取）".to_string(); gold.carriers.len()];
            if !orphan_anns.is_empty() {
                e.push("gold（完美抽取）".to_string());
            }
            e
        },
        models: vec![],
        note: format!(
            "上限评测：把专家表的原文当作完美抽取。候选 {} 条；命中专家条目：文本口径 {} 条 / 位置口径 {} 条 / \
             开卷案例库 {} 条（案例库 = 记忆，不是能力）。每行 `gold_match` 标出口径（text / positional），\
             两个数不得混算。\
             复现四维（按位置口径对齐命中，共 {} 条 gold）：原文对齐 {}、位置一致 {}、计错数一致 {}、建议接近 {}，严格复现 {} 条；\
             位置一致维为 0 是因为上限评测的单元无载体坐标（未定位），不是漏报。",
            rows.len(),
            replay_score.matched,
            span_replay.matched,
            case_hit.len(),
            repro.gold_total,
            repro.original_ok,
            repro.location_ok,
            repro.counted_ok,
            repro.suggestion_ok,
            repro.full_ok
        ),
    };
    let file = ReportFile {
        meta: meta.clone(),
        rows: rows.clone(),
    };
    media_report::write_candidates(&replay.join("candidates.json"), &file).unwrap();
    let files = media_report::write_artifacts(&replay, &rows, &meta).unwrap();
    assert!(files.json.is_file() && files.html.is_file() && files.markdown.is_file());
    // candidates.json 必须能读回来（人工就是在这个文件里勾 `checked`）
    let back = media_report::read_report_file(&replay.join("candidates.json")).unwrap();
    assert_eq!(back.rows.len(), rows.len(), "candidates.json 必须可回读");
    match media_report::WordCraft::new(None, 120) {
        Ok(wc) => {
            wc.html_to_docx(&files.html, &replay.join("report.docx"))
                .await
                .unwrap();
            assert!(
                replay.join("report.docx").is_file(),
                "wordcraft 可用时必须产出 docx"
            );
            eprintln!(
                "  重演产物：{}（candidates.json / report.md / report.docx）",
                replay.display()
            );
        }
        Err(e) => eprintln!("  wordcraft-cli 不可用，本次只出 json/md/html：{e}"),
    }
}

/// PhotoCraft CLI 解析（与 media-core 同样的三级顺序）
fn photocraft_bin() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("PHOTOCRAFT_CLI") {
        let pb = PathBuf::from(&p);
        if pb.is_file() {
            return Some(pb);
        }
    }
    for dir in std::env::var("PATH").unwrap_or_default().split(':') {
        let pb = PathBuf::from(dir).join("photocraft-cli");
        if pb.is_file() {
            return Some(pb);
        }
    }
    let home = std::env::var("HOME").unwrap_or_default();
    [
        format!("{home}/.local/bin/photocraft-cli"),
        "/usr/local/bin/photocraft-cli".to_string(),
    ]
    .iter()
    .map(PathBuf::from)
    .find(|p| p.is_file())
}

/// 起一个开启视觉档的 media 服务（LLM 用**真实**凭据，从环境继承）
#[allow(clippy::await_holding_lock)]
async fn start_media_server(workdir: &std::path::Path) -> String {
    let _guard = env_guard();
    unsafe {
        std::env::set_var("GEC_MEDIA_ENABLED", "1");
        std::env::set_var("GEC_MEDIA_VISION", "1");
        std::env::set_var("GEC_MEDIA_WORKDIR", workdir);
        std::env::set_var("GEC_LEXICONS_DIR", manifest_dir().join("../lexicons"));
        std::env::set_var("LLM_PROMPTS_DIR", manifest_dir().join("../prompts"));
        std::env::set_var(
            "GEC_SETTINGS_FILE",
            std::env::temp_dir().join(format!(
                "lf_gold_media_{}.json",
                uuid::Uuid::new_v4().simple()
            )),
        );
    }
    let db = std::env::temp_dir().join(format!(
        "lf_gold_media_{}.db",
        uuid::Uuid::new_v4().simple()
    ));
    let state = Arc::new(AppState::from_env(
        Some(&manifest_dir().join("../decision-tables")),
        Some(&db),
    ));
    let app = routes::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, app).into_future());
    format!("http://{addr}")
}

/// 真实评测：原图 → 视觉抽取 → 检测（level 3）→ remap → 对金集打分
///
/// 前置（缺一律 SKIP，不判失败）：
/// - `LANGFIXER_MEDIA_DIR` 指向存着 `origin_pic_*.jpg` 的目录（原图不入库）；
/// - `LLM_API_KEY` / `LLM_BASE_URL` 指向一个**视觉**模型（`GEC_MEDIA_VISION_MODEL` 指定）；
/// - `photocraft-cli`（载体探测与 png 导出）。
#[tokio::test]
#[ignore = "需要原图目录 + 真实视觉模型凭据（见 MULTIMODAL.md §15）"]
async fn gold_real_vision_eval() {
    let media_dir = std::env::var("LANGFIXER_MEDIA_DIR").unwrap_or_default();
    if media_dir.is_empty() {
        eprintln!("SKIP：未设置 LANGFIXER_MEDIA_DIR（原图目录）");
        return;
    }
    if std::env::var("LLM_API_KEY").unwrap_or_default().is_empty() {
        eprintln!("SKIP：未配置 LLM_API_KEY（视觉档需要真实多模态模型）");
        return;
    }
    if photocraft_bin().is_none() {
        eprintln!("SKIP：未安装 photocraft-cli");
        return;
    }
    let gold = load_gold();
    let wd = std::env::temp_dir().join(format!("lf_gold_run_{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&wd).unwrap();
    let base = start_media_server(&wd).await;
    let client = reqwest::Client::new();
    // level 3 = 规则 + 决策 + 无条件 LLM 全文兜底：评测要的是召回上限
    let level: u8 = std::env::var("GEC_MEDIA_EVAL_LEVEL")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);

    let mut all_anns: Vec<media_core::Annotation> = Vec::new();
    // 重演产物按载体收集（正误表要按载体顺序排，与专家表一致）
    let mut per_carrier_anns: Vec<Vec<media_core::Annotation>> = Vec::new();
    let mut per_carrier_units: Vec<Vec<media_core::TextUnit>> = Vec::new();
    let mut carrier_meta: Vec<(String, String, Option<(f64, f64)>)> = Vec::new();
    let mut models: Vec<String> = Vec::new();
    for carrier in &gold.carriers {
        let path = PathBuf::from(&media_dir).join(&carrier.file);
        if !path.is_file() {
            eprintln!("SKIP 载体：{} 不存在", path.display());
            continue;
        }
        let url = format!(
            "{base}/gec/media/jobs?filename={}&lang={}&level={}",
            carrier.file, gold.lang, level
        );
        let resp = client
            .post(url)
            .body(std::fs::read(&path).unwrap())
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            200,
            "建 job 失败：{}",
            resp.text().await.unwrap()
        );
        let created: serde_json::Value = resp.json().await.unwrap();
        let id = created["id"].as_str().unwrap().to_string();

        let job = loop {
            let j: serde_json::Value = client
                .get(format!("{base}/gec/media/jobs/{id}"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if matches!(
                j["stage"].as_str().unwrap_or(""),
                "ready" | "failed" | "applied"
            ) {
                break j;
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        };
        eprintln!(
            "\n载体 {}：stage={:?} 来源档={:?} 模型={:?} 单元 {} 标注 {}",
            carrier.file,
            job["stage"],
            job["extraction"],
            job["extraction_model"],
            job["units"].as_array().map(|a| a.len()).unwrap_or(0),
            job["annotations"].as_array().map(|a| a.len()).unwrap_or(0)
        );
        for w in job["warnings"].as_array().cloned().unwrap_or_default() {
            eprintln!("  告警：{}", w.as_str().unwrap_or(""));
        }
        let typed: Vec<media_core::Annotation> = job["annotations"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|a| serde_json::from_value(a.clone()).ok())
            .collect();
        // 按载体打分：金集里每条 gold 都挂在某个 carrier 上
        let units_typed: Vec<media_core::TextUnit> = job["units"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|u| serde_json::from_value(u.clone()).ok())
            .collect();
        let size = match (
            job["carrier"]["width"].as_u64(),
            job["carrier"]["height"].as_u64(),
        ) {
            (Some(w), Some(h)) => Some((w as f64, h as f64)),
            _ => None,
        };
        per_carrier_anns.push(typed.clone());
        per_carrier_units.push(units_typed);
        carrier_meta.push((
            carrier.file.clone(),
            job["extraction"].as_str().unwrap_or("none").to_string(),
            size,
        ));
        if let Some(m) = job["extraction_model"].as_str() {
            models.push(m.to_string());
        }
        let subset: Vec<GoldItem> = gold
            .items
            .iter()
            .filter(|g| carrier.items.contains(&g.no))
            .cloned()
            .collect();
        let s = score_gold(&subset, &typed, 0.5);
        eprintln!(
            "  本载体召回 {:.1}%（命中 {}/{} 条，计差错 {}），精确率 {:.1}%，产出 {} 条",
            s.recall.max(0.0) * 100.0,
            s.matched,
            s.gold_total,
            s.gold_counted,
            s.precision * 100.0,
            s.ours
        );
        for (no, ann_id, kind, ratio) in &s.per_gold {
            let text = gold
                .items
                .iter()
                .find(|g| g.no == *no)
                .map(|g| g.original.chars().take(24).collect::<String>())
                .unwrap_or_default();
            eprintln!(
                "    #{no:<2} {:?} 重叠 {:.2}{}  {text}",
                kind,
                ratio,
                ann_id.map(|i| format!("← ann#{i}")).unwrap_or_default()
            );
        }
        all_anns.extend(typed);
    }

    // 全量打分（跨载体合并；与按载体打分互为交叉校验）
    let overall = score_gold(&gold.items, &all_anns, 0.5);
    eprintln!(
        "\n=== 真实评测合计 ===\n召回 {:.1}%，精确率 {:.1}%，产出 {} 条，命中 {} 条",
        overall.recall.max(0.0) * 100.0,
        overall.precision * 100.0,
        overall.ours,
        overall.matched
    );
    eprintln!(
        "产物目录（含 vision_raw.txt / annotations.json）：{}",
        wd.display()
    );

    // —— 重演三段式 ①：真实载体 → candidates.json（人工勾选入口）+ 正误表 ——
    let replay = std::env::var("LANGFIXER_REPLAY_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| manifest_dir().join("../target/media-replay"));
    let labels: Vec<String> = (0..carrier_meta.len())
        .map(|i| format!("图片{}", i + 1))
        .collect();
    let sources: Vec<CarrierSource> = carrier_meta
        .iter()
        .enumerate()
        .map(|(ci, (file, extraction, size))| CarrierSource {
            job_id: file.as_str(),
            label: &labels[ci],
            size: *size,
            extraction: extraction.as_str(),
            annotations: &per_carrier_anns[ci],
            units: &per_carrier_units[ci],
        })
        .collect();
    let mut rows = candidates(&sources);
    // 真实重演：**每载体一套 stitched 坐标系**，必须按载体分别 attach。
    // 跨载体合并坐标 = 假命中（A 图的 offset 100 与 B 图的 offset 100 毫无关系）；
    // 标注 id 又是每载体各自编号的，合并打分还会撞号把 gold 挂到别的载体的行上。
    let stitched_per: Vec<Stitched> = per_carrier_units
        .iter()
        .map(|u| stitch_units(u).0)
        .collect();
    let cands: Vec<Vec<String>> = gold.items.iter().map(unit_texts).collect();
    let mut rows_by_carrier: Vec<Vec<ReportRow>> = Vec::with_capacity(carrier_meta.len());
    for (file, _, _) in &carrier_meta {
        rows_by_carrier.push(rows.iter().filter(|r| &r.job_id == file).cloned().collect());
    }
    let att = {
        // 逐载体打包：`iter_mut()` 给出的 mut 借用互不相交，每载体一套坐标系
        let mut groups: Vec<CarrierReplay<'_>> = Vec::with_capacity(rows_by_carrier.len());
        let mut label_it = labels.iter();
        let mut st_it = stitched_per.iter();
        for chunk in rows_by_carrier.iter_mut() {
            let label = label_it.next().expect("label 与载体一一对应");
            let st = st_it.next().expect("stitched 与载体一一对应");
            groups.push(CarrierReplay {
                label,
                text: &st.text,
                stitched: st,
                rows: chunk,
            });
        }
        attach_gold_by_carrier(&mut groups, &gold.items, &cands, 0.5)
    };
    let gold_spans_per: Vec<Vec<Option<(usize, usize)>>> = stitched_per
        .iter()
        .map(|st| gold_locate_spans(&st.text, &cands))
        .collect();
    let row_spans_per: Vec<Vec<Option<(usize, usize)>>> = (0..rows_by_carrier.len())
        .map(|ci| stitched_spans(&stitched_per[ci], &rows_by_carrier[ci]))
        .collect();
    // 「规则触发了但没 credited」→ 回填 rule_id + 归因来源：反思靶子才是 rule_tune（改规则）
    // 而不是补词典。跨载体合并时**坐标重合优先于原文重叠**：强归因不能被弱归因顶掉。
    let mut rule_ids: std::collections::BTreeMap<u32, (String, String)> =
        std::collections::BTreeMap::new();
    for ci in 0..rows_by_carrier.len() {
        for (no, (rid, via)) in missed_rule_evidence(
            &rows_by_carrier[ci],
            &row_spans_per[ci],
            &gold.items,
            &gold_spans_per[ci],
        ) {
            let via_s = match via {
                RuleVia::Positional => "positional",
                RuleVia::Text => "text",
            };
            if let Some((_, cur)) = rule_ids.get(&no)
                && cur == "positional"
                && via_s == "text"
            {
                continue;
            }
            rule_ids.insert(no, (rid, via_s.to_string()));
        }
    }
    let strong = rule_ids.values().filter(|(_, v)| v == "positional").count();
    eprintln!(
        "\n规则归因回填：{} 条漏报有触发规则（坐标重合 {strong} / 原文重叠 {}）",
        rule_ids.len(),
        rule_ids.len() - strong
    );
    let repro = {
        let mut views: Vec<CarrierSpans<'_>> = Vec::with_capacity(rows_by_carrier.len());
        for ci in 0..rows_by_carrier.len() {
            views.push(CarrierSpans {
                rows: &rows_by_carrier[ci],
                gold_spans: &gold_spans_per[ci],
                row_spans: &row_spans_per[ci],
            });
        }
        reproduction_by_carrier(&views, &gold.items, 0.5)
    };
    rows = rows_by_carrier
        .iter()
        .flat_map(|v| v.iter().cloned())
        .collect();
    assert_eq!(
        att.text.matched, overall.matched,
        "报告口径与评测口径必须同一把尺子（{} vs {}）",
        att.text.matched, overall.matched
    );
    let per_sum: usize = att.per_carrier.iter().map(|c| c.matched).sum();
    assert!(
        att.text.matched <= att.spanned.matched && att.spanned.matched <= per_sum,
        "汇总命中必须落在 [文本口径, 各载体命中之和] 之间（跨载体去重只会往下压）：{} / {} / {}",
        att.text.matched,
        att.spanned.matched,
        per_sum
    );
    eprintln!("\n=== 真实重演·按载体口径 ===");
    for c in &att.per_carrier {
        eprintln!(
            "  {}：参与位置口径 {} 行，命中 {} 条（位置口径 {} 条），gold 原文未定位 {} 条",
            c.label, c.ours, c.matched, c.positional, c.gold_unlocated
        );
    }
    // 闭卷漏报 → 反思中心靶子（真实重演回填了触发规则，靶子可以是 rule_tune）
    let credited: std::collections::BTreeSet<u32> = gold
        .items
        .iter()
        .filter(|g| rows.iter().any(|r| r.gold_no == Some(g.no)))
        .map(|g| g.no)
        .collect();
    let miss_nos: Vec<u32> = gold
        .items
        .iter()
        .filter(|g| g.counted && !credited.contains(&g.no))
        .map(|g| g.no)
        .collect();
    write_reflection_seeds(
        &replay.join("reflection_seeds.json"),
        "media_gold@2026-10-26 · 真实载体重演",
        &gold.items,
        &credited,
        att.text.matched,
        att.spanned.matched,
        &rule_ids,
    );
    post_seeds_to_reflection(&base, &replay.join("reflection_seeds.json"), &miss_nos).await;
    models.sort();
    models.dedup();
    let meta = ReportMeta {
        title: "文字差错核对表（真实载体重演）".to_string(),
        lang: gold.lang.clone(),
        generated_at: chrono::Utc::now().to_rfc3339(),
        carriers: carrier_meta
            .iter()
            .enumerate()
            .map(|(i, (file, extraction, _))| {
                format!("{}：{}（抽取 {extraction}）", labels[i], file)
            })
            .collect(),
        extractions: carrier_meta.iter().map(|(_, e, _)| e.clone()).collect(),
        models,
        note: format!(
            "真实载体重演：候选 {} 条；命中专家条目：文本口径 {} 条 / 文本+位置口径 {} 条（加权召回 {:.1}%，按位置口径）。\
             每行 `gold_match` 标出口径（text / positional），两个数不得混算；位置口径按**各载体自己的 stitched 坐标**换算，\
             坐标对不上的行不参与（不参与 ≠ 判不上）。复现四维（按位置口径）：原文对齐 {}、位置一致 {}、计错数一致 {}、\
             建议接近 {}，严格复现 {} 条。「计错数」以人工勾选为准。",
            rows.len(),
            att.text.matched,
            att.spanned.matched,
            att.spanned.recall * 100.0,
            repro.original_ok,
            repro.location_ok,
            repro.counted_ok,
            repro.suggestion_ok,
            repro.full_ok
        ),
    };
    media_report::write_candidates(
        &replay.join("candidates.json"),
        &ReportFile {
            meta: meta.clone(),
            rows: rows.clone(),
        },
    )
    .unwrap();
    let files = media_report::write_artifacts(&replay, &rows, &meta).unwrap();
    match media_report::WordCraft::new(None, 180) {
        Ok(wc) => {
            wc.html_to_docx(&files.html, &replay.join("report.docx"))
                .await
                .unwrap();
        }
        Err(e) => eprintln!("  wordcraft-cli 不可用，只出 json/md/html：{e}"),
    }
    eprintln!(
        "重演产物：{}（candidates.json → 勾 `checked` → cargo run -p api-server --bin media-report -- {}/candidates.json）",
        replay.display(),
        replay.display()
    );
}
