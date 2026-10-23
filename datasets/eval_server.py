#!/usr/bin/env python3
"""测评中心服务（进化中心 P0）：按需基准评测 + 层级归因 + 消融 + A/B 报告。

闭环定位：检测 → 反思 → 训练 → **测试**。本服务是「测试」环节的统一入口：
把原本散落在 CLI（`score_f05.py` / `eval_llm_mucgec.py` / `cargo test --test
mucgec_eval`）的评测能力收进一个 FastAPI 服务，供 api-server 代理（`POST
/gec/eval/run` 等）与 web-ui「进化中心 → 测评」面板调用。

核心能力：

1. **层级归因**（capability 模式）：对 gold 每条源句走 api-server 生产检测路径
   （`POST /gec/detect` + `force_llm`），每个预测错误带 `source`
   （rule / knowledge / decision / feature / statistical / llm）。每个 gold 编辑
   按「级联序首个重叠层」归因 → 各层召回份额；各层 P/R/F0.5（严格 (op, ref, span)
   匹配，`score_f05.py` 同口径）+ span 重叠口径 + 误报 + 句级门控。
2. **消融**（ablation）：经 `PUT /gec/settings` 临时关闭某层（decision / l3 / knowledge）→
   重跑 → 恢复设置；ΔF0.5 / ΔFP = 该层的**边际贡献**（级联效应也测得到）。
3. **换臂 A/B**：`llm_model_id` 切到注册表条目（cec3 / cec4 / generic / custom），
   跑同一基准 → 报告并排对比（UI diff 视图）。
4. **漏斗**（funnel 模式）：level 2 自然级联（L3 门控、LLM 只跑长尾；
   不强制 LLM）→ LLM 触达率 + 级联前缀累计解决率（每层「先解决多少算多少」）。
5. **报告**：JSON + markdown 存 `datasets/eval_reports/`（gitignore），
   逐句结果缓存（`cache/`，中断可续跑；`fresh=true` 清空）。

用法：
  # 启动（api-server 需已运行；默认 127.0.0.1:4801，GEC_API 可覆盖）
  EVAL_PORT=4807 python3 eval_server.py

  # 冒烟（200 句子集）
  curl -s localhost:4807/eval/run -d '{"benchmark":"mucgec","arm":"cec3","subset":200}'
  # 轮询
  curl -s localhost:4807/eval/job/<id>

  # 自检（合成数据，无网络）
  python3 eval_server.py --selftest
"""
import argparse
import concurrent.futures
import hashlib
import json
import os
import random
import re
import sys
import threading
import time
import urllib.error
import urllib.request
from datetime import datetime, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPORTS_DIR = HERE / "eval_reports"
CACHE_DIR = REPORTS_DIR / "cache"

# 级联序（与 gec-core CascadeDetector 一致：① 规则 → ② 决策 → ②.5 知识 → ③-F L3 → ③ LLM）；
# 归因按此序取「首个重叠层」——漏斗中该句该编辑最先由谁解决
CASCADE_ORDER = ["rule", "decision", "knowledge", "feature", "statistical", "llm"]

BENCHMARKS = {
    "mucgec": {"file": "mucgec_gold.json", "lang": "zh", "splits": ["dev"], "beta": 0.5},
    "cscd": {"file": "cscd_gold.json", "lang": "zh", "splits": ["cscd", "sig15", "med", "law"], "beta": 1.0},
    "nacgec": {"file": "nacgec_gold.json", "lang": "zh", "splits": ["nacgec"], "beta": 0.5},
    # CTCDataset 原生（母语者）集：datasets/prepare_ctc.py；拼写集 β=1（Correction F1），
    # 语法/校对集 β=0.5（ChERRANT 口径）。全部源句已入去污染块表（prepare_l3_pool）
    "cscdns": {"file": "cscdns_gold.json", "lang": "zh", "splits": ["cscdns"], "beta": 1.0},
    "nlpcc23": {"file": "nlpcc23_gold.json", "lang": "zh", "splits": ["nlpcc23"], "beta": 1.0},
    "sig13": {"file": "sig13_gold.json", "lang": "zh", "splits": ["sig13"], "beta": 1.0},
    "sig14": {"file": "sig14_gold.json", "lang": "zh", "splits": ["sig14"], "beta": 1.0},
    "cctc": {"file": "cctc_gold.json", "lang": "zh", "splits": ["cctc"], "beta": 0.5},
    "midu": {"file": "midu_gold.json", "lang": "zh", "splits": ["midu"], "beta": 0.5},
    "ctc21": {"file": "ctc21_gold.json", "lang": "zh", "splits": ["ctc21"], "beta": 0.5},
    "jfleg": {"file": "jfleg_gold.json", "lang": "en", "splits": ["dev", "test"], "beta": 0.5},
}


def now_iso() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def short_id(*parts) -> str:
    h = hashlib.sha1("|".join(parts).encode("utf-8")).hexdigest()
    return h[:10]


# ─────────────────────────────────────────────────────────────────────────────
# api-server 交互（detect / settings）
# ─────────────────────────────────────────────────────────────────────────────

def api_get(server: str, path: str, timeout: float = 30.0):
    with urllib.request.urlopen(f"{server}{path}", timeout=timeout) as r:
        return json.load(r)


def api_put(server: str, path: str, body: dict, timeout: float = 30.0):
    req = urllib.request.Request(
        f"{server}{path}", data=json.dumps(body).encode("utf-8"),
        headers={"Content-Type": "application/json"}, method="PUT",
    )
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.load(r)


def detect(server: str, lang: str, text: str, force_llm: bool,
          level: int = 1, timeout: float = 180.0, attempts: int = 3) -> dict:
    """POST /gec/detect；5xx / 连接错误退避重试，4xx 立即失败（同 eval_llm_mucgec）。

    `level` 恒显式下发（服务端缺省 1 = 纯规则：不传 level 时 funnel 模式会
    静默退化成「只测规则层」，LLM / L3 层结构性为 0，报告失真）。"""
    body = json.dumps(
        {"lang": lang, "text": text, "force_llm": force_llm, "level": level}
    ).encode("utf-8")
    last_err = None
    for i in range(attempts):
        try:
            req = urllib.request.Request(
                f"{server.rstrip('/')}/gec/detect", data=body,
                headers={"Content-Type": "application/json"},
            )
            with urllib.request.urlopen(req, timeout=timeout) as r:
                return json.load(r)
        except (urllib.error.HTTPError, urllib.error.URLError, TimeoutError, OSError) as e:
            code = getattr(e, "code", None)
            if code is not None and 400 <= code < 500:
                raise
            last_err = e
            if i < attempts - 1:
                time.sleep(2 * (i + 1))
    raise last_err


def get_settings(server: str) -> dict:
    return api_get(server, "/gec/settings")


def put_settings(server: str, **fields) -> dict:
    return api_put(server, "/gec/settings", fields)


# ─────────────────────────────────────────────────────────────────────────────
# 计分（与 score_f05.py / mucgec_eval.rs 同口径）
# ─────────────────────────────────────────────────────────────────────────────

def edit_key(e: dict):
    return (e["op"], e.get("ref", ""), tuple(e["span"]))


def fb_score(tp, n_pred, n_gold, beta):
    b2 = beta * beta
    p = tp / n_pred if n_pred else 0.0
    r = tp / n_gold if n_gold else 0.0
    f = (1 + b2) * p * r / (b2 * p + r) if (b2 * p + r) else 0.0
    return p, r, f


def spans_overlap(a, b):
    # 零宽（插入）span：仅同插入点视为重叠；否则用严格不等式区间（相邻不算重叠）
    if a[0] == a[1] and b[0] == b[1]:
        return a[0] == b[0]
    return a[0] < b[1] and b[0] < a[1]


def to_pred_edits(errs):
    """api-server 错误项 → gold 同构编辑（保留 source 供层级归因）。"""
    out = []
    for e in errs:
        span = e.get("span") or [0, 0]
        orig = e.get("original") or ""
        corr = e.get("correction") or ""
        if not orig and not corr:
            continue
        if orig and corr and orig != corr:
            op = "S"
        elif not orig:
            op = "I"
        else:
            op = "D"
        out.append({
            "op": op, "src": orig, "ref": corr, "span": span,
            "source": e.get("source") or "rule",
        })
    return out


def gate_stats(tp, fp, fn, tn):
    prec = tp / (tp + fp) if tp + fp else 0.0
    rec = tp / (tp + fn) if tp + fn else 0.0
    f1 = 2 * prec * rec / (prec + rec) if prec + rec else 0.0
    return {"tp": tp, "fp": fp, "fn": fn, "tn": tn,
             "precision": round(prec, 4), "recall": round(rec, 4), "f1": round(f1, 4)}


# 漏斗前缀（级联序的累积）：每层「先解决多少算多少」
FUNNEL_PREFIXES = [
    ("① 规则", ["rule"]),
    ("①+② 决策", ["rule", "decision"]),
    ("①+②+②.5 知识", ["rule", "decision", "knowledge"]),
    ("①+②+②.5+③-F L3", ["rule", "decision", "knowledge", "feature"]),
    ("全级联", list(CASCADE_ORDER)),
]


def score_sentences(sentences, beta=0.5):
    """sentences: [{src, gold: [edit], preds: [edit+source], llm_ran: bool, llm_ms: int}]

    产出 overall / 各层（CASCADE_ORDER）的：
    - 严格口径（(op, ref, span) 全同，score_f05 口径）：TP / P / R / Fβ
    - 重叠口径（span 相交，mucgec_eval 口径）：TP / R / FP
    - 召回份额：每个 gold 编辑按级联序归因到首个重叠层
    - 句级门控：该层对该句是否产出过错误
    - 漏斗（funnel 模式）：LLM 触达率 + 级联前缀累计解决率
    """
    n_gold = sum(len(s["gold"]) for s in sentences)
    n_gold_sents = sum(1 for s in sentences if s["gold"])

    for s in sentences:
        s["gold_keys"] = {edit_key(e) for e in s["gold"]}
        s["gold_spans"] = [e["span"] for e in s["gold"]]

    layers = {
        name: {
            "n_pred": 0, "tp_strict": 0, "fp_strict": 0,
            "tp_overlap": 0, "fp_overlap": 0,
            "gate": [0, 0, 0, 0],  # tp, fp, fn, tn（该层对该句有无产出）
            "attributed_gold": 0,  # 召回份额（重叠归因）
        }
        for name in CASCADE_ORDER
    }
    overall = {
        "n_pred": 0, "tp_strict": 0, "fp_strict": 0,
        "tp_overlap": 0, "fp_overlap": 0,
        "gate": [0, 0, 0, 0],
    }

    for s in sentences:
        g_has = bool(s["gold"])
        any_pred = False
        s_edit_hit = [False] * len(s["gold"])  # 每个 gold 编辑是否被任一层重叠命中
        for p in s["preds"]:
            overall["n_pred"] += 1
            any_pred = True
            lay = layers.get(p["source"])
            if lay is not None:
                lay["n_pred"] += 1
            hit_gi = None
            for gi, gs in enumerate(s["gold_spans"]):
                if spans_overlap(p["span"], gs):
                    hit_gi = gi
                    break
            if hit_gi is None:
                overall["fp_overlap"] += 1
                if lay is not None:
                    lay["fp_overlap"] += 1
                continue
            overall["tp_overlap"] += 1
            if lay is not None:
                lay["tp_overlap"] += 1
            s_edit_hit[hit_gi] = True
            if edit_key(p) in s["gold_keys"]:
                overall["tp_strict"] += 1
                if lay is not None:
                    lay["tp_strict"] += 1
            else:
                overall["fp_strict"] += 1
                if lay is not None:
                    lay["fp_strict"] += 1
        # 句级门控（overall + 各层）
        for name in CASCADE_ORDER:
            lay = layers[name]
            lay_has = any(p["source"] == name for p in s["preds"])
            if g_has and lay_has:
                lay["gate"][0] += 1
            elif lay_has:
                lay["gate"][1] += 1
            elif g_has:
                lay["gate"][2] += 1
            else:
                lay["gate"][3] += 1
        if g_has and any_pred:
            overall["gate"][0] += 1
        elif any_pred:
            overall["gate"][1] += 1
        elif g_has:
            overall["gate"][2] += 1
        else:
            overall["gate"][3] += 1
        # 召回份额：每个 gold 编辑归因到级联序首个重叠层
        for gi, gs in enumerate(s["gold_spans"]):
            for name in CASCADE_ORDER:
                if any(spans_overlap(p["span"], gs) and p["source"] == name for p in s["preds"]):
                    layers[name]["attributed_gold"] += 1
                    break
        # 漏斗：LLM 触达 + 级联前缀累计解决（按 gold 编辑计数）
        if s.get("llm_ran"):
            s["_llm_ms"] = s.get("llm_ms") or 0
        fh = s.setdefault("_funnel_hit", [0] * len(FUNNEL_PREFIXES))
        for pi, (_, names) in enumerate(FUNNEL_PREFIXES):
            for gi in range(len(s["gold"])):
                if s_edit_hit[gi] and any(
                    spans_overlap(p["span"], s["gold_spans"][gi]) and p["source"] in names
                    for p in s["preds"]
                ):
                    fh[pi] += 1

    # 漏斗汇总
    n_sents = len(sentences)
    llm_ran = sum(1 for s in sentences if s.get("llm_ran"))
    llm_ms = [s["_llm_ms"] for s in sentences if s.get("llm_ran") and "_llm_ms" in s]
    funnel = {
        # 成本一等指标：LLM 调用量（绝对数 + 每百句归一化）
        "llm_calls": llm_ran,
        "llm_calls_per_100": round(llm_ran * 100 / n_sents, 1) if n_sents else 0.0,
        "llm_reach": round(llm_ran / n_sents, 4) if n_sents else 0.0,
        "avg_llm_ms": round(sum(llm_ms) / len(llm_ms)) if llm_ms else 0,
        "cumulative": [
            {
                "prefix": label,
                "resolved": sum(s.get("_funnel_hit", [0] * len(FUNNEL_PREFIXES))[pi] for s in sentences),
                "rate": 0.0,
            }
            for pi, (label, _) in enumerate(FUNNEL_PREFIXES)
        ],
    }
    if n_gold:
        for c in funnel["cumulative"]:
            c["rate"] = round(c["resolved"] / n_gold, 4)

    def pack(d, is_overall=False):
        p_strict = d["tp_strict"] / d["n_pred"] if d["n_pred"] else 0.0
        r_strict = d["tp_strict"] / n_gold if n_gold else 0.0
        b2 = beta * beta
        f = (1 + b2) * p_strict * r_strict / (b2 * p_strict + r_strict) if (b2 * p_strict + r_strict) else 0.0
        out = {
            "n_pred": d["n_pred"], "tp_strict": d["tp_strict"],
            "p_strict": round(p_strict, 4), "r_strict": round(r_strict, 4),
            "f_beta": round(f, 4),
            "tp_overlap": d["tp_overlap"],
            "r_overlap": round(d["tp_overlap"] / n_gold, 4) if n_gold else 0.0,
            "fp_strict": d["fp_strict"],
            "fp_overlap": d["fp_overlap"],
            "gate": gate_stats(*d["gate"]),
        }
        if not is_overall:
            out["recall_share"] = round(d["attributed_gold"] / n_gold, 4) if n_gold else 0.0
            out["attributed_gold"] = d["attributed_gold"]
        return out

    return {
        "n_sentences": n_sents,
        "n_gold_sentences": n_gold_sents,
        "n_gold_edits": n_gold,
        "overall": pack(overall, is_overall=True),
        "layers": {n: pack(layers[n]) for n in CASCADE_ORDER},
        "funnel": funnel,
    }


# ─────────────────────────────────────────────────────────────────────────────
# run 编排：设置快照 → 换臂/消融 → 逐句检测（缓存 + 并发）→ 恢复 → 报告
# ─────────────────────────────────────────────────────────────────────────────

def load_gold(benchmark: str, split: str | None):
    spec = BENCHMARKS[benchmark]
    items = json.loads((HERE / spec["file"]).read_text(encoding="utf-8"))
    if split:
        items = [it for it in items if it.get("split") == split]
    return items, spec


def abl_fp(ablation) -> str:
    if not ablation:
        return "none"
    parts = []
    if ablation.get("decision") is False:
        parts.append("dec-off")
    if ablation.get("l3") is False:
        parts.append("l3-off")
    if ablation.get("knowledge") is False:
        parts.append("know-off")
    return "+".join(parts) or "none"


def cache_path(benchmark: str, split: str | None, arm: str, ablation, mode: str) -> Path:
    name = f"{benchmark}" + (f"_{split}" if split else "") + f"_{arm}_{abl_fp(ablation)}_{mode}"
    return CACHE_DIR / f"{name}.jsonl"


def load_cache(path: Path) -> dict:
    out = {}
    if path.exists():
        for line in path.read_text(encoding="utf-8").splitlines():
            if not line.strip():
                continue
            try:
                rec = json.loads(line)
                out[rec["text"]] = rec
            except (json.JSONDecodeError, KeyError):
                continue
    return out




def append_cache(path: Path, rec: dict):
    with open(path, "a", encoding="utf-8") as f:
        f.write(json.dumps(rec, ensure_ascii=False) + "\n")


def run_eval_job(job: dict, server: str, cfg: dict):
    """执行一次评测（同步；由 FastAPI 线程池调用）。"""
    benchmark = cfg["benchmark"]
    split = cfg.get("split")
    arm = cfg.get("arm") or "current"
    mode = cfg.get("mode") or "capability"  # capability = force_llm；funnel = level 2 自然级联；rules = level 1 纯规则
    ablation = cfg.get("ablation")  # {"decision": false, "l3": false, "knowledge": false} | None
    subset = cfg.get("subset")
    seed = cfg.get("seed", 42)
    workers = max(1, min(16, int(cfg.get("workers", 8))))
    fresh = bool(cfg.get("fresh"))

    def fail(msg: str):
        job["status"] = "error"
        job["error"] = msg
        job["done_at"] = now_iso()

    try:
        items, spec = load_gold(benchmark, split)
        lang = spec["lang"]
        beta = spec["beta"]
        if subset:
            rng = random.Random(f"{seed}:{benchmark}:{split}")
            items = rng.sample(items, min(subset, len(items)))
        if not items:
            return fail(f"gold 为空（benchmark={benchmark} split={split}）")

        # 1) 设置快照
        settings = get_settings(server)
        snapshot = {
            "llm_model_id": settings.get("llm_model_id", ""),
            "decision_enabled": settings.get("decision", {}).get("enabled"),
            "l3_enabled": settings.get("l3", {}).get("enabled"),
            "knowledge_enabled": settings.get("knowledge_enabled", True),
        }
        # 2) 应用目标配置（换臂 + 消融）
        put_body = {}
        if arm != "current":
            put_body["llm_model_id"] = arm
        if ablation:
            if ablation.get("decision") is False:
                put_body["decision"] = {"enabled": False}
            if ablation.get("l3") is False:
                put_body["l3"] = {"enabled": False}
            if ablation.get("knowledge") is False:
                put_body["knowledge_enabled"] = False
        if put_body:
            put_settings(server, **put_body)
        eff = get_settings(server)
        job["config"] = {
            "llm_model_id": eff.get("llm_model_id"),
            "llm_model": eff.get("llm", {}).get("model"),
            "llm_enabled": eff.get("llm", {}).get("enabled"),
            "decision_enabled": eff.get("decision", {}).get("enabled"),
            "l3_enabled": eff.get("l3", {}).get("enabled"),
            "knowledge_enabled": eff.get("knowledge_enabled", True),
            "embedding_configured": bool(eff.get("embedding", {}).get("endpoint")),
        }
    except Exception as e:  # noqa: BLE001
        return fail(f"设置应用失败: {e}")

    cpath = cache_path(benchmark, split, arm, ablation, mode)
    CACHE_DIR.mkdir(parents=True, exist_ok=True)
    if fresh:
        if cpath.exists():
            cpath.unlink()
    cache = load_cache(cpath)
    force = mode == "capability"
    # funnel = 自然级联（level 2：L3 门控，LLM 只跑长尾）；
    # capability = force_llm（level 1 + 强制 LLM，测各层能力）
    level = 2 if mode == "funnel" else 1

    def one(text: str) -> dict:
        rec = cache.get(text)
        if rec is not None:
            return rec
        resp = detect(server, lang, text, force_llm=force, level=level)
        timings = resp.get("timings") or {}
        llm_ms = 0
        for k in ("llm_ms", "llm"):
            if isinstance(timings.get(k), (int, float)):
                llm_ms = int(timings[k])
                break
        preds = to_pred_edits(resp.get("errors") or [])
        rec = {
            "text": text,
            "errors": resp.get("errors") or [],
            "preds": preds,
            "llm_ran": llm_ms > 0 or any(p["source"] == "llm" for p in preds),
            "llm_ms": llm_ms,
            "decision_abstained": resp.get("decision_abstained", False),
            "ts": now_iso(),
        }
        append_cache(cpath, rec)
        cache[text] = rec
        return rec

    # 3) 逐句检测（并发 + 进度）
    job["status"] = "running"
    job["progress"] = {"done": 0, "total": len(items)}
    results, failed = [], 0
    try:
        with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as pool:
            futs = {pool.submit(one, it["src"]): it for it in items}
            for fut in concurrent.futures.as_completed(futs):
                it = futs[fut]
                try:
                    results.append(fut.result())
                except Exception as e:  # noqa: BLE001
                    failed += 1
                    import traceback
                    traceback.print_exc()
                    results.append({
                        "text": it["src"], "preds": [], "llm_ran": False,
                        "llm_ms": 0, "errors": [], "failed": True,
                        "error": f"{type(e).__name__}: {e}",
                    })
                job["progress"]["done"] += 1
    finally:
        # 4) 恢复设置（无论成败）
        try:
            restore = {}
            if arm != "current":
                restore["llm_model_id"] = snapshot["llm_model_id"]
            if ablation:
                if ablation.get("decision") is False and snapshot["decision_enabled"]:
                    restore["decision"] = {"enabled": True}
                if ablation.get("l3") is False and snapshot["l3_enabled"]:
                    restore["l3"] = {"enabled": True}
                if ablation.get("knowledge") is False and snapshot["knowledge_enabled"]:
                    restore["knowledge_enabled"] = True
            if restore:
                put_settings(server, **restore)
        except Exception:  # noqa: BLE001
            job.setdefault("warnings", []).append("设置恢复失败，请手动检查 /gec/settings")

    # 5) 计分（按 gold 顺序对齐）
    by_text = {r["text"]: r for r in results}
    sentences = []
    for it in items:
        rec = by_text.get(it["src"])
        if rec is None:
            continue
        sentences.append({
            "src": it["src"],
            "gold": it.get("edits") or [],
            "preds": rec.get("preds") or [],
            "llm_ran": rec.get("llm_ran", False),
            "llm_ms": rec.get("llm_ms", 0),
        })
    scored = score_sentences(sentences, beta=beta)

    report_id = (
        f"{datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%SZ')}"
        f"_{benchmark}" + (f"_{split}" if split else "")
        + f"_{arm}" + (f"_abl_{abl_fp(ablation)}" if ablation else "")
        + f"_{mode}"
    )
    report = {
        "id": report_id,
        "ts": now_iso(),
        "benchmark": benchmark,
        "split": split,
        "lang": lang,
        "beta": beta,
        "arm": arm,
        "mode": mode,
        "ablation": ablation,
        "config": job.get("config"),
        "sentences": {
            "total": len(items),
            "ok": len(items) - failed,
            "failed": failed,
        },
        **scored,
    }
    REPORTS_DIR.mkdir(parents=True, exist_ok=True)
    rpath = REPORTS_DIR / f"{report_id}.json"
    rpath.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
    mpath = REPORTS_DIR / f"{report_id}.md"
    mpath.write_text(render_markdown(report), encoding="utf-8")
    report["report_md"] = mpath.name
    job["status"] = "done"
    job["report_id"] = report_id
    job["done_at"] = now_iso()
    return report


def render_markdown(r: dict) -> str:
    lines = [
        f"# 测评报告 {r['id']}",
        "",
        f"- 基准：{r['benchmark']}" + (f" / {r['split']}" if r.get("split") else "")
        + f"（{r['lang']}，β={r['beta']:g}）",
        f"- 模式：{r['mode']}"
        + (f"；消融：{json.dumps(r['ablation'], ensure_ascii=False)}" if r.get("ablation") else ""),
    ]
    cfg = r.get("config") or {}
    arm_line = f"- 臂：{r['arm']}"
    if cfg.get("llm_model"):
        arm_line += f"（{cfg['llm_model']}）"
    arm_line += f"；decision={cfg.get('decision_enabled')} l3={cfg.get('l3_enabled')} knowledge={cfg.get('knowledge_enabled')}"
    lines += [arm_line,
              f"- 句数：{r['n_sentences']}（gold 句 {r['n_gold_sentences']}，gold 编辑 {r['n_gold_edits']}）",
              "",
              "## 总体",
              "",
              "| 指标 | 严格 (op,ref,span) | span 重叠 |",
              "|---|---|---|",
              f"| 预测数 | {r['overall']['n_pred']} | {r['overall']['n_pred']} |",
              f"| TP | {r['overall']['tp_strict']} | {r['overall']['tp_overlap']} |",
              f"| P | {r['overall']['p_strict']} | — |",
              f"| R | {r['overall']['r_strict']} | {r['overall']['r_overlap']} |",
              f"| F{r['beta']:g} | {r['overall']['f_beta']} | — |",
              f"| FP（重叠） | {r['overall']['fp_overlap']} | |",
              f"| 句级门控 F1 | {r['overall']['gate']['f1']} | |",
              "",
              "## 层级归因（级联序：规则 → 决策 → 知识 → L3 → LLM）",
              "",
              "| 层 | 预测 | TP严格 | P | R | Fβ | 召回份额 | FP重叠 | 门控F1 |",
              "|---|---|---|---|---|---|---|---|---|",
              ]
    for name in CASCADE_ORDER:
        l = r["layers"][name]
        lines.append(
            f"| {name} | {l['n_pred']} | {l['tp_strict']} | {l['p_strict']} | "
            f"{l['r_strict']} | {l['f_beta']} | {l['recall_share']} | "
            f"{l['fp_overlap']} | {l['gate']['f1']} |"
        )
    f = r["funnel"]
    lines += [
        "",
        "## 漏斗",
        "",
        f"- LLM 触达率：{f['llm_reach']}（平均 LLM 耗时 {f['avg_llm_ms']}ms）",
        "",
        "| 级联前缀 | 解决 gold 编辑 | 累计解决率 |",
        "|---|---|---|",
    ]
    for c in f["cumulative"]:
        lines.append(f"| {c['prefix']} | {c['resolved']} | {c['rate']} |")
    lines.append("")
    return "\n".join(lines)


# ─────────────────────────────────────────────────────────────────────────────
# FastAPI 服务
# ─────────────────────────────────────────────────────────────────────────────

JOBS: dict = {}
JOBS_LOCK = threading.Lock()
# 全局设置（llm_model_id / 层开关）是 api-server 共享状态：
# 两个 run 任务并发会互相切换臂/开关 → 同时只允许一个 run 任务
RUN_LOCK = threading.Lock()


def build_app(server: str | None = None):
    from fastapi import FastAPI, HTTPException
    from pydantic import BaseModel

    server = (server or os.environ.get("GEC_API") or "http://127.0.0.1:4801").rstrip("/")
    app = FastAPI(title="LangFixer 测评中心")

    class RunRequest(BaseModel):
        benchmark: str
        split: str | None = None
        arm: str = "current"
        mode: str = "capability"  # capability（force_llm）/ funnel（level 2 自然级联）/ rules（level 1 纯规则，无 LLM）
        ablation: dict | None = None  # {"decision": false, "l3": false, "knowledge": false}
        subset: int | None = None
        seed: int = 42
        workers: int = 8
        fresh: bool = False

    @app.get("/health")
    def health():
        return {"status": "ok", "api_server": server,
                "benchmarks": {k: v["splits"] for k, v in BENCHMARKS.items()}}

    @app.post("/eval/run")
    def run(req: RunRequest):
        if req.benchmark not in BENCHMARKS:
            raise HTTPException(400, f"未知基准: {req.benchmark}（可用: {list(BENCHMARKS)}）")
        if req.split and req.split not in BENCHMARKS[req.benchmark]["splits"]:
            raise HTTPException(400, f"未知 split: {req.split}")
        if req.mode not in ("capability", "funnel", "rules"):
            raise HTTPException(400, "mode 必须是 capability / funnel / rules")
        if not RUN_LOCK.acquire(blocking=False):
            raise HTTPException(409, "已有评测任务在运行（全局设置互斥）；请等待其完成")
        try:
            job_id = short_id(req.benchmark, req.arm or "", now_iso())
            job = {
                "id": job_id, "status": "pending", "config": None,
                "progress": {"done": 0, "total": 0}, "report_id": None,
                "error": None, "created_at": now_iso(),
            }
            with JOBS_LOCK:
                JOBS[job_id] = job
            threading.Thread(target=_run_guarded, args=(job, server, req.model_dump()),
                           daemon=True).start()
            return {"job_id": job_id, "status": job["status"]}
        except BaseException:
            RUN_LOCK.release()
            raise


    @app.get("/eval/job/{job_id}")
    def job_status(job_id: str):
        job = JOBS.get(job_id)
        if job is None:
            raise HTTPException(404, f"任务不存在: {job_id}")
        return job

    @app.get("/eval/reports")
    def reports():
        if not REPORTS_DIR.exists():
            return {"reports": []}
        out = []
        for p in sorted(REPORTS_DIR.glob("*.json"), reverse=True):
            try:
                r = json.loads(p.read_text(encoding="utf-8"))
                out.append({
                    "id": r["id"], "ts": r["ts"], "benchmark": r["benchmark"],
                    "split": r.get("split"), "arm": r["arm"], "mode": r["mode"],
                    "ablation": r.get("ablation"),
                    "n_sentences": r.get("n_sentences"),
                    "n_gold_edits": r.get("n_gold_edits"),
                    "f_beta": r.get("overall", {}).get("f_beta"),
                    "p_strict": r.get("overall", {}).get("p_strict"),
                    "r_strict": r.get("overall", {}).get("r_strict"),
                    "llm_reach": r.get("funnel", {}).get("llm_reach"),
                })
            except (json.JSONDecodeError, KeyError):
                continue
        return {"reports": out[:200]}

    @app.get("/eval/report/{report_id}")
    def report(report_id: str):
        # 允许集须覆盖 id 生成端字符集（含消融指纹的 '+'，见 abl_fp）；
        # 只剥离不允许字符（防路径穿越），不得整体改写 id，否则消融报告 404
        p = REPORTS_DIR / f"{re.sub(r'[^A-Za-z0-9_.+-]', '', report_id)}.json"
        if not p.exists():
            raise HTTPException(404, f"报告不存在: {report_id}")
        return json.loads(p.read_text(encoding="utf-8"))

    return app


def _run_guarded(job: dict, server: str, cfg: dict):
    """run 任务包装：无论成败释放全局互斥锁。"""
    try:
        run_eval_job(job, server, cfg)
    finally:
        RUN_LOCK.release()


# ─────────────────────────────────────────────────────────────────────────────
# 自检（合成数据，无网络）
# ─────────────────────────────────────────────────────────────────────────────

def selftest():
    # 2 句（1 有错 1 干净）：规则严格命中 gold 编辑 1；决策层同 span 非严格（重叠 FP）；
    # LLM 严格命中编辑 2 + 1 个 FP；L3 对干净句误报 1
    sentences = [
        {
            "src": "他作天很冷",
            "gold": [
                {"op": "S", "src": "作", "ref": "做", "span": [1, 4]},
                {"op": "I", "src": "", "ref": "，", "span": [4, 4]},
            ],
            "preds": [
                {"op": "S", "src": "作", "ref": "做", "span": [1, 4], "source": "rule"},
                {"op": "S", "src": "作", "ref": "坐", "span": [1, 4], "source": "decision"},
                {"op": "I", "src": "", "ref": "，", "span": [4, 4], "source": "llm"},
                {"op": "S", "src": "天", "ref": "天", "span": [4, 7], "source": "llm"},
            ],
            "llm_ran": True, "llm_ms": 1200,
        },
        {
            "src": "无误句",
            "gold": [],
            "preds": [{"op": "S", "src": "无", "ref": "有", "span": [0, 3], "source": "feature"}],
            "llm_ran": False, "llm_ms": 0,
        },
    ]
    assert abl_fp(None) == "none"
    assert abl_fp({"l3": False}) == "l3-off"
    assert abl_fp({"knowledge": False}) == "know-off"
    assert abl_fp({"decision": False, "knowledge": False}) == "dec-off+know-off"
    r = score_sentences(sentences, beta=0.5)
    o = r["overall"]
    assert o["n_pred"] == 5, o
    assert o["tp_strict"] == 2, o
    assert abs(o["p_strict"] - 0.4) < 1e-9, o
    assert abs(o["r_strict"] - 1.0) < 1e-9, o
    assert o["tp_overlap"] == 3, o  # 编辑 1 被 rule+decision 各命中、编辑 2 被 llm 插入命中
    assert o["fp_overlap"] == 2, o  # llm 的 [4,7]（与零宽 [4,4] 相邻不算重叠）+ feature 干净句误报
    assert o["gate"]["tp"] == 1 and o["gate"]["fp"] == 1, o["gate"]
    assert r["layers"]["rule"]["tp_strict"] == 1 and r["layers"]["rule"]["n_pred"] == 1
    assert r["layers"]["decision"]["tp_strict"] == 0 and r["layers"]["decision"]["fp_strict"] == 1
    # llm：插入严格命中编辑 2；[4,7] 与零宽 gold 相邻不算重叠 → 只计 fp_overlap
    assert r["layers"]["llm"]["tp_strict"] == 1 and r["layers"]["llm"]["fp_strict"] == 0
    assert r["layers"]["llm"]["fp_overlap"] == 1
    assert r["layers"]["rule"]["recall_share"] == 0.5, r["layers"]["rule"]
    assert r["layers"]["llm"]["recall_share"] == 0.5, r["layers"]["llm"]
    assert r["funnel"]["llm_reach"] == 0.5, r["funnel"]
    assert r["funnel"]["cumulative"][-1]["rate"] == 1.0, r["funnel"]["cumulative"]
    assert r["funnel"]["cumulative"][0]["rate"] == 0.5  # 仅规则解决编辑 1
    md = render_markdown({
        "id": "t", "benchmark": "mucgec", "split": "dev", "lang": "zh", "beta": 0.5,
        "arm": "cec3", "mode": "capability", "ablation": None,
        "config": {"llm_model": "m", "decision_enabled": True, "l3_enabled": True, "knowledge_enabled": True},
        "n_sentences": 2, "n_gold_sentences": 1, "n_gold_edits": 2,
        "overall": o, "layers": r["layers"], "funnel": r["funnel"],
    })
    assert "## 层级归因" in md and "| llm |" in md
    print("selftest 通过")
    print(f"  overall: P={o['p_strict']} R={o['r_strict']} F0.5={o['f_beta']} 门控F1={o['gate']['f1']}")
    for name in CASCADE_ORDER:
        l = r["layers"][name]
        if l["n_pred"]:
            print(f"  {name:<10} pred={l['n_pred']} TP严格={l['tp_strict']} 份额={l['recall_share']}")


def main():
    ap = argparse.ArgumentParser(description="测评中心服务（进化中心 P0）")
    ap.add_argument("--port", type=int, default=int(os.environ.get("EVAL_PORT", "4807")))
    ap.add_argument("--selftest", action="store_true", help="合成数据自检（无网络）")
    args = ap.parse_args()
    if args.selftest:
        selftest()
        return
    import uvicorn
    uvicorn.run(build_app(), host="127.0.0.1", port=args.port, log_level="warning")


if __name__ == "__main__":
    main()