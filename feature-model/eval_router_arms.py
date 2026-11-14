#!/usr/bin/env python3
"""路由三臂评测 harness（EVOLUTION.md §11.10 验收口径 / §11.11 路由口径）。

三臂（等调用率曲线上逐点对比，§11.10：路由臂 gold 召回须逐点 ≥ 另外两臂）：
  histgb  现有 4806 sklearn 句级门控的本地代理——HistGB 在 jev zh_v2 池
          gold 标签（有错/干净）上训练。注意：代理臂训练数据 = 9.8k 金标基座，
          弱于生产门控（45.8 万句特征池），其曲线是路由臂须超越的**下界**；
          真实生产点在 MuCGEC 63.1% 调用率（已入库基准，§11.10）。
  random  均匀随机（seed 固定）——成本相同、信息为零的对照。
  router  路由臂代理——HistGB 在同池 route_label（router_labels.py 产出，
          gold∪LLM∪lexical∪floor 并集）上训练。本批为可学性证据臂；
          终局 = jev route 头（首 token logprob），届时仅替换打分来源。

口径：
- 覆盖 = 规则定案档命中（任何臂零成本覆盖）∪ 被复查句中被检出的 gold 编辑；
  LLM 检出默认 **oracle 上限**（复查即检出该句 gold 编辑；三臂同一假设，
  曲线横向可比）。有真实 LLM 臂缓存时以 --llm-cache 传入，改按缓存重合计。
- 按 §11.11 类型分层报告 gold 编辑数；金标 < --min-gold 的类型**跳过判定**
  （只报数不裁决，防薄金标假信号）。
- skip 如实入覆盖率账（skipped ≠ covered）；level 3 全 LLM 兜底臂不受影响
  （本 harness 只评 level 2 预算路由）。

用法:
    cd feature-model
    python3 router_labels.py --split train           # 先产标签
    python3 eval_router_arms.py                       # mucgec+nacgec+cscd
    python3 eval_router_arms.py --datasets mucgec --grid 10,30,50,63.1,80
"""
from __future__ import annotations

import argparse
import json
import os
import pickle
import random
import sys
import time
from multiprocessing import Pool
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parent
REPO = HERE.parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE / "router_labels.py"))

import correct  # noqa: E402

DS = REPO / "datasets"
DEFAULT_POOL = REPO.parent / "jev-gec" / "data" / "pools" / "zh_v2"
FEAT_CACHE = os.environ.get("ROUTER_FEAT_CACHE", "/tmp/router_arms_feats.pkl")
ANCHOR = 63.1  # HistGB 生产门控已入库基准点（§11.10）

# NaCGEC 句级类型 → v2 类型码（§11.11 T 族）；cscd 为字级语料 → spelling
NACGEC_TYPE = {
    "搭配不当": "collocation", "不合逻辑": "logic", "语序不当": "word_order",
    "成分残缺": "missing_component", "成分赘余": "redundant_component",
    "句式杂糅": "blend", "正确": None,
}
DATASET_DEFAULT_TYPE = {"cscd": "spelling", "cscdns": "spelling", "mucgec": "untyped"}


def byte_to_char(src: str, b: int) -> int:
    return len(src.encode("utf-8")[:b].decode("utf-8"))


# ── 特征（与 eval_l3_data_ab 同法：l3_finetune 22 维，缓存 + 多进程）──────────
def _init_worker():
    sys.path.insert(0, str(HERE))
    global _L
    import l3_finetune as _L  # noqa: F401


def _feat_chunk(texts):
    return [_L.extract_features(t, "zh") for t in texts]


def extract(texts: list[str], nproc: int = 8) -> np.ndarray:
    if os.path.isfile(FEAT_CACHE):
        with open(FEAT_CACHE, "rb") as f:
            cache = pickle.load(f)
    else:
        cache = {}
    todo = [t for t in dict.fromkeys(texts) if t not in cache]
    if todo:
        chunks = [todo[i:i + 2000] for i in range(0, len(todo), 2000)]
        with Pool(nproc, initializer=_init_worker) as p:
            for chunk, feats in zip(chunks, p.map(_feat_chunk, chunks, chunksize=2)):
                cache.update(zip(chunk, feats))
        with open(FEAT_CACHE, "wb") as f:
            pickle.dump(cache, f)
    return np.asarray([cache[t] for t in texts], dtype=np.float32)


# ── 数据装载 ────────────────────────────────────────────────────────────────
def load_pool(pool_dir: Path, split: str) -> list[dict]:
    p = pool_dir / f"{split}.jsonl"
    return [json.loads(l) for l in p.read_text(encoding="utf-8").splitlines() if l.strip()]


def load_labels(path: Path) -> dict[str, int]:
    """route_label → 可学习路由目标 1/0（llm_review=1；rule_decide/skip=0）。"""
    out = {}
    for l in path.read_text(encoding="utf-8").splitlines():
        if l.strip():
            r = json.loads(l)
            out[r["id"]] = int(r.get("route_label") == "llm_review")
    return out


def load_eval(name: str) -> list[dict]:
    """gold 集 → 句级行：text / gold 编辑 char span / 规则覆盖 / 类型。"""
    rows = []
    for e in json.load(open(DS / f"{name}_gold.json", encoding="utf-8")):
        src = e["src"]
        edits = [tuple(byte_to_char(src, s) for s in ed["span"]) for ed in e.get("edits", [])]
        et = e.get("error_type")
        typ = NACGEC_TYPE.get(et, DATASET_DEFAULT_TYPE.get(name, "untyped")) if et \
            else DATASET_DEFAULT_TYPE.get(name, "untyped")
        if et == "正确":
            typ, edits = None, []
        hits = correct.find_zh_hits(src)
        safe = [h for h in hits if not getattr(h, "advisory", False)]
        cover_rule = [any(h.span[0] <= ce[1] and ce[0] <= h.span[1] for h in safe)
                      for ce in edits]
        rows.append({"text": src, "edits": edits, "cover_rule": cover_rule,
                     "type": typ if edits else typ, "src_name": name})
    return rows


def llm_cache_load(path: str) -> dict[str, list[tuple[int, int]]]:
    """{id/text → 检出 span(char)}；重合口径在 cache_cover 里用。"""
    out: dict[str, list[tuple[int, int]]] = {}
    for l in Path(path).read_text(encoding="utf-8").splitlines():
        if not l.strip():
            continue
        r = json.loads(l)
        text = r.get("text", "")
        spans = []
        for it in r.get("errors", []):
            frag = it.get("original", "")
            if frag:
                spans.extend((m, m + len(frag))
                             for m in _find_all(text, frag))
        key = r.get("text", r.get("id", ""))
        out.setdefault(key, []).extend(spans)
    return out


def _find_all(text: str, frag: str):
    i = text.find(frag)
    while i >= 0:
        yield i
        i = text.find(frag, i + 1)


# ── 臂与曲线 ────────────────────────────────────────────────────────────────
def recall_at(rows: list[dict], reviewed: set[int], oracle: bool,
              llm: dict | None, edit_filter=None) -> tuple[int, int]:
    """(covered gold edits, total gold edits)；edit_filter(ed→bool) 做类型分层。"""
    cov = tot = 0
    for i, r in enumerate(rows):
        for j, ce in enumerate(r["edits"]):
            if edit_filter and not edit_filter(r, j):
                continue
            tot += 1
            if r["cover_rule"][j] or i in reviewed:
                if oracle:
                    cov += 1
                else:
                    spans = (llm or {}).get(r["text"], [])
                    if any(s <= ce[1] and ce[0] <= e for s, e in spans):
                        cov += 1
            # skipped：不计覆盖（skipped ≠ covered）
    return cov, tot


def arm_indices(scores: np.ndarray, rate: float, rng: random.Random,
                floor_every: int = 0) -> set[int]:
    n = len(scores)
    k = round(n * rate / 100.0)
    if floor_every > 0:
        floor = set(range(0, n, floor_every))
        order = np.argsort(-scores)
        top = list(order[:k])
        sel = set(top) | floor
        # 地板已含的名字从 top 里扣：调低 k 使总量贴住目标调用率
        for idx in reversed(top):
            if len(sel) > k and idx in floor:
                sel.discard(idx)
        return sel
    return set(np.argsort(-scores)[:k].tolist())


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--datasets", default="mucgec,nacgec,cscd")
    ap.add_argument("--pool-dir", default=str(DEFAULT_POOL))
    ap.add_argument("--labels", default="/tmp/router_labels_train.jsonl",
                    help="router_labels.py 产物（路由臂训练标签）")
    ap.add_argument("--grid", default="5,10,20,30,40,50,63.1,70,80,100")
    ap.add_argument("--min-gold", type=int, default=30, help="分层判定的最少金标编辑数")
    ap.add_argument("--llm-cache", default="", help="真实 LLM 臂检出缓存（替代 oracle 上限）")
    ap.add_argument("--floor-every", type=int, default=0,
                    help="路由臂采样地板：每 N 句强制 1 句复查（0=关，成本计入调用率）")
    ap.add_argument("--seed", type=int, default=20261115)
    ap.add_argument("--include-inpool", action="store_true",
                    help="含训练池内评测句（污染绝对值 optimistic；仅看横向对照时用）")
    ap.add_argument("--nproc", type=int, default=8)
    args = ap.parse_args()

    t0 = time.time()
    correct.load_confusables(str(REPO / "lexicons" / "zh_gec_lexicon.json"))
    names = [d.strip() for d in args.datasets.split(",") if d.strip()]
    rows: list[dict] = []
    for nm in names:
        rows += load_eval(nm)
    pool_train = load_pool(Path(args.pool_dir), "train")

    # 污染披露：gold 集（MuCGEC/NACGEC/CSCD）本就是 zh_v2 金标基座的训练源，
    # 与池重叠无法避免（manifest 仅对 frozen 集断言零重叠）。两臂污染相同，
    # 曲线横向可比；默认仅在**池外行**上出曲线（--include-inpool 可关）。
    pool_texts = {r["text"] for r in pool_train}
    for r in rows:
        r["in_pool"] = r["text"] in pool_texts
    n_in = sum(r["in_pool"] for r in rows)
    if not args.include_inpool:
        rows = [r for r in rows if not r["in_pool"]]

    labels_p = Path(args.labels)
    if not labels_p.is_file():
        sys.exit(f"缺路由标签 {labels_p}；先跑 router_labels.py --split train")
    labels = load_labels(labels_p)
    routed = [r for r in pool_train if r["id"] in labels]
    if len(routed) < 0.9 * len(pool_train):
        print(f"WARN 标签覆盖池 {len(routed)}/{len(pool_train)} 行", file=sys.stderr)

    Xp = extract([r["text"] for r in routed], args.nproc)
    Xe = extract([r["text"] for r in rows], args.nproc)
    y_gate = np.asarray([int(r["label"]) for r in routed])
    y_route = np.asarray([labels[r["id"]] for r in routed])

    from sklearn.ensemble import HistGradientBoostingClassifier
    gate = HistGradientBoostingClassifier(random_state=0, max_iter=250, learning_rate=0.1)
    gate.fit(Xp, y_gate)
    route = HistGradientBoostingClassifier(random_state=0, max_iter=250, learning_rate=0.1)
    route.fit(Xp, y_route)
    s_gate = gate.predict_proba(Xe)[:, 1]
    s_route = route.predict_proba(Xe)[:, 1]
    rng = random.Random(args.seed)
    s_rand = np.asarray([rng.random() for _ in rows])

    oracle = not args.llm_cache
    llm = llm_cache_load(args.llm_cache) if args.llm_cache else None
    grid = [float(x) for x in args.grid.split(",")]
    gold_types = sorted({r["type"] for r in rows if r["edits"] and r["type"]})
    type_gold = {t: sum(len(r["edits"]) for r in rows if r["edits"] and r["type"] == t)
                 for t in gold_types}

    # ── 报告 ──
    ts = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
    print(f"# 路由三臂评测 {ts}\n")
    print(f"| 配置 | 值 |")
    print(f"|---|---|")
    print(f"| 评测集 | {', '.join(names)}（{len(rows)} 句） |")
    print(f"| 训练池 | {Path(args.pool_dir).name}/train ∩标签 = {len(routed)} |")
    print(f"| 污染面 | gold 集在池内 {n_in} 句；本次曲线用池外 {len(rows)} 句"
          f"{'（含池内）' if args.include_inpool else ''} |")
    print(f"| LLM 检出口径 | {'oracle 上限（三臂同假设，横向可比）' if oracle else '真实缓存 ' + args.llm_cache} |")
    print(f"| 路由臂地板 | {'每 ' + str(args.floor_every) + ' 句强制复查' if args.floor_every else '关'} |")
    print(f"| 基准锚点 | HistGB 生产门控 {ANCHOR}% 调用率（§11.10） |\n")

    print("## 金标分层")
    print("| 类型 | gold 编辑 | 判定 |")
    print("|---|---|---|")
    for t, c in sorted(type_gold.items(), key=lambda x: -x[1]):
        print(f"| {t} | {c} | {'分层判定' if c >= args.min_gold else f'金标 < {args.min_gold}，跳过判定'} |")
    print()

    def curve(rate):
        out = {}
        for arm, sc in (("histgb", s_gate), ("random", s_rand), ("router", s_route)):
            reviewed = arm_indices(sc, rate, random.Random(args.seed),
                                   args.floor_every if arm == "router" else 0)
            eff = 100.0 * len(reviewed) / len(rows)
            cov, tot = recall_at(rows, reviewed, oracle, llm)
            out[arm] = (cov, tot, eff)
        return out

    print("## 等调用率曲线（总体 gold 编辑召回）")
    print(f"| 调用率 | histgb | random | router | router−histgb | router−random |")
    print("|---|---|---|---|---|---|")
    for rate in grid:
        c = curve(rate)
        rc = [c[a][0] / max(1, c[a][1]) for a in ("histgb", "random", "router")]
        mark = " ←锚" if abs(rate - ANCHOR) < 1e-9 else ""
        print(f"| {rate:g}%{mark} | {rc[0]:.1%} | {rc[1]:.1%} | **{rc[2]:.1%}** | "
              f"{rc[2]-rc[0]:+.1%} | {rc[2]-rc[1]:+.1%} |")
    print(f"> skip 率 = 1 − 调用率（预算模式；**skipped ≠ covered**，如实入覆盖率账）。\n")

    print(f"## 锚点 {ANCHOR}% 分层判定（router ≥ 另两臂？）")
    scores = {"histgb": s_gate, "random": s_rand, "router": s_route}

    def reviewed_of(arm: str, rate: float) -> set[int]:
        return arm_indices(scores[arm], rate, random.Random(args.seed),
                           args.floor_every if arm == "router" else 0)

    print("| 类型 | gold | histgb | random | router | 判定 |")
    print("|---|---|---|---|---|---|")
    fails = 0
    for t in sorted(gold_types, key=lambda x: -type_gold[x]):
        if not type_gold[t]:
            continue
        rr = []
        for arm in ("histgb", "random", "router"):
            cov, tot = recall_at(rows, reviewed_of(arm, ANCHOR), oracle, llm,
                                 edit_filter=lambda r, j, t=t: r["type"] == t)
            rr.append(cov / max(1, tot))
        if type_gold[t] < args.min_gold:
            verdict = f"金标 {type_gold[t]} < {args.min_gold}，跳过"
        else:
            ok = rr[2] + 1e-12 >= rr[0] and rr[2] + 1e-12 >= rr[1]
            fails += not ok
            verdict = "PASS" if ok else "FAIL"
        print(f"| {t} | {type_gold[t]} | {rr[0]:.1%} | {rr[1]:.1%} | {rr[2]:.1%} | {verdict} |")
    c = curve(ANCHOR)
    rec = {a: c[a][0] / max(1, c[a][1]) for a in scores}
    ok_all = rec["router"] >= rec["histgb"] - 1e-12 and rec["router"] >= rec["random"] - 1e-12
    print(f"\n**总体判定：{'PASS' if ok_all else 'FAIL'}**（锚点召回 router {rec['router']:.1%} / "
          f"histgb {rec['histgb']:.1%} / random {rec['random']:.1%}，"
          f"gold {c['router'][1]}；分层 FAIL {fails}）")
    print(f"\n> level 3 全 LLM 兜底臂不在本 harness 空间（漏报兜底立场不变，§11.10）；")
    print(f"> histgb 为本地代理臂（9.8k 金标基座训练），弱于生产门控——PASS 是下界意义，")
    print(f"> 上线前按 §11.10 与生产门控同数据重赛。")
    print(f"\n耗时 {time.time() - t0:.0f}s")


if __name__ == "__main__":
    main()
