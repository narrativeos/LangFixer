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
        adv = [h for h in hits if getattr(h, "advisory", False)]
        cover_rule = [any(h.span[0] <= ce[1] and ce[0] <= h.span[1] for h in safe)
                      for ce in edits]
        rows.append({"text": src, "edits": edits, "cover_rule": cover_rule,
                     "adv_spans": [h.span for h in adv],
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
    ap.add_argument("--llm-run-gold", action="store_true",
                    help="先在池外 gold 行上直跑 LLM 臂并落盘缓存（需 LLM_* 环境变量），然后退出")
    ap.add_argument("--gold-llm-limit", type=int, default=400,
                    help="--llm-run-gold 抽样上限（按 src 分层；支持从缓存断点续跑）")
    ap.add_argument("--llm-cache-out", default="/tmp/router_llm_arm_gold.jsonl",
                    help="--llm-run-gold 检出缓存落盘路径")
    ap.add_argument("--llm-workers", type=int, default=1)
    ap.add_argument("--only-cached", action="store_true",
                    help="非 oracle 曲线只在已查实行上计（已验证子集，公平口径；需 --llm-cache）")
    ap.add_argument("--floor-every", type=int, default=0,
                    help="路由臂采样地板：每 N 句强制 1 句复查（0=关，成本计入调用率）")
    ap.add_argument("--seed", type=int, default=20261115)
    ap.add_argument("--counts", default="",
                    help="gold_counts.py 产物 {text,counts,spans}：加第四臂 router_ev"
                         "（每型计数头 n̂_t × p̂(t) 求和打分，§11.14 补二方案 A′）")
    ap.add_argument("--gain-cache", default="",
                    help="train 侧 LLM 检出缓存（测 p̂(t) 用；配 --counts。"
                         "不可用 gold 侧缓存：那是评测集，泄密）")
    ap.add_argument("--head-min-pos", type=int, default=100,
                    help="计数头最少正例句数，不足则退稀型先验（常数均值）")
    ap.add_argument("--p-min-n", type=int, default=50,
                    help="p̂(t) 最少可测 span 数，不足则退全体均值先验")
    ap.add_argument("--rule-subtract", action="store_true",
                    help="规则臂前置扣除：规则定案档（safe 非 advisory）已覆盖的"
                         "编辑从曲线扣除，只评残差；头部报扣除量与提示档打扰账"
                         "（§11.14 补四）")
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

    # ── gold 行 LLM 臂直跑（非 oracle 口径的数据制备） ──
    if args.llm_run_gold:
        import router_labels as RL
        for i, r in enumerate(rows):
            r.setdefault("id", r["text"])
        seed_out: dict[str, list[dict]] = {}
        cache_p = Path(args.llm_cache_out)
        if cache_p.is_file():
            for r in RL.load_jsonl(cache_p):
                seed_out[r.get("text", r.get("id", ""))] = r.get("errors", [])
        rng_g = random.Random(args.seed)
        strata: dict[str, list[dict]] = {}
        for r in rows:
            strata.setdefault(r["src_name"], []).append(r)
        picked: list[dict] = []
        for k in sorted(strata):
            xs = strata[k]
            kk = max(1, round(args.gold_llm_limit * len(xs) / len(rows)))
            picked.extend(rng_g.sample(xs, min(kk, len(xs))))
        rng_g.shuffle(picked)
        picked = picked[:args.gold_llm_limit]
        # 并上历史已查实行，保证落盘缓存不因换 cap 而丢旧条目
        seen = {r["text"] for r in picked}
        rows_arg = picked + [r for r in rows
                             if r["text"] in seed_out and r["text"] not in seen]
        n_new = sum(1 for r in picked if r["text"] not in seed_out)
        print(f"[gold-llm] 抽样 {len(picked)} 行（已查实 {len(picked) - n_new}，"
              f"待跑 {n_new}）→ {cache_p}", file=sys.stderr)
        RL.run_llm_arm(rows_arg, len(rows_arg), str(cache_p),
                       seed_out=seed_out, workers=args.llm_workers)
        print(f"[gold-llm] 完成；评测时 --llm-cache {cache_p}"
              f"（建议叠 --only-cached 出公平曲线）", file=sys.stderr)
        return

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

    # ── router_ev 臂（方案 A′）：EV(句)=Σ p̂(t)·n̂_t，计数头从 gold_counts 蒸馏 ──
    s_ev = None
    p_report: list[tuple] = []
    if args.counts:
        if not args.gain_cache:
            sys.exit("--counts 需 --gain-cache（p̂(t) 必须 train 侧，gold 侧泄密）")
        import router_labels as RL_mod
        gc: dict[str, dict] = {}
        for l in Path(args.counts).expanduser().read_text(encoding="utf-8").splitlines():
            if l.strip():
                r2 = json.loads(l)
                gc[r2["text"]] = r2
        gain = llm_cache_load(args.gain_cache)
        v2t = sorted(RL_mod.V2_PASSTHROUGH)
        tot_t = {t: 0 for t in v2t}
        cov_t = {t: 0 for t in v2t}
        n_meas = 0
        for r in routed:
            e = gc.get(r["text"])
            if not e or r["text"] not in gain:
                continue
            gs = gain[r["text"]]
            n_meas += 1
            for a0, b0, t in e.get("spans", []):
                tot_t[t] += 1
                if any(s <= b0 and a0 <= ee for s, ee in gs):
                    cov_t[t] += 1
        all_p = sum(cov_t.values()) / max(1, sum(tot_t.values()))
        p_hat = {t: (cov_t[t] / tot_t[t] if tot_t[t] >= args.p_min_n else all_p)
                 for t in v2t}
        from sklearn.ensemble import HistGradientBoostingRegressor
        s_ev = np.zeros(len(rows), dtype=np.float64)
        for t in v2t:
            y = np.asarray([gc.get(r["text"], {}).get("counts", {}).get(t, 0)
                            for r in routed], dtype=float)
            n_pos = int((y > 0).sum())
            if n_pos >= args.head_min_pos:
                h = HistGradientBoostingRegressor(loss="poisson", max_iter=250,
                                                  learning_rate=0.1, random_state=0)
                h.fit(Xp, y)
                n_hat = h.predict(Xe)
                mode = "poisson头"
            else:
                n_hat = np.full(len(rows), float(y.mean()))
                mode = "先验常数"
            s_ev += p_hat[t] * np.maximum(n_hat, 0.0)
            if n_pos or tot_t[t]:
                p_report.append((t, tot_t[t], cov_t[t], p_hat[t], n_pos, mode))
        print(f"[router_ev] p̂(t) 测量面：train 池∩缓存∩计数标签 {n_meas}/{len(routed)} 行；"
              f"全体先验 p̂={all_p:.3f}", file=sys.stderr)

    oracle = not args.llm_cache
    llm = llm_cache_load(args.llm_cache) if args.llm_cache else None
    if args.only_cached:
        if not llm:
            sys.exit("--only-cached 需搭配 --llm-cache")
        n_all = len(rows)
        rows = [r for r in rows if r["text"] in llm]
        print(f"WARN 已验证子集口径：{len(rows)}/{n_all} 句（其余行 LLM 臂未查实，不入曲线）",
              file=sys.stderr)
    grid = [float(x) for x in args.grid.split(",")]

    # ── 规则前置扣除口径（§11.14 补四）：残差过滤 + 打扰账 ──
    resid = None
    resid_note = ""
    if args.rule_subtract:
        resid = lambda r, j: not r["cover_rule"][j]
        n_edits = sum(len(r["edits"]) for r in rows)
        n_sub = sum(sum(r["cover_rule"]) for r in rows)
        adv_chars = adv_gold = 0
        for r in rows:
            goldc = {c for a, b in r["edits"] for c in range(a, max(b, a + 1))}
            for s, e in r["adv_spans"]:
                adv_chars += e - s
                adv_gold += len(goldc & set(range(s, e)))
        resid_note = (f"残差曲线：已扣规则定案 {n_sub}/{n_edits} 编辑"
                      f"（{n_sub / max(1, n_edits):.1%}）；提示档（advisory）打扰 "
                      f"{1e4 * adv_chars / max(1, sum(len(r['text']) for r in rows)):.1f}"
                      f" 字符/万字，∩金标 "
                      f"{adv_gold}/{adv_chars}={adv_gold / max(1, adv_chars):.1%}")

    gold_types = sorted({r["type"] for r in rows if r["edits"] and r["type"]})
    type_gold = {t: sum(1 for r in rows if r["type"] == t
                        for j in range(len(r["edits"]))
                        if (resid is None or resid(r, j)))
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
    print(f"| 基准锚点 | HistGB 生产门控 {ANCHOR}% 调用率（§11.10） |")
    if args.rule_subtract:
        print(f"| 规则前置扣除 | {resid_note} |")
    print()

    print("## 金标分层" + ("（残差：已扣规则定案覆盖）" if resid else ""))
    print("| 类型 | gold 编辑 | 判定 |")
    print("|---|---|---|")
    for t, c in sorted(type_gold.items(), key=lambda x: -x[1]):
        print(f"| {t} | {c} | {'分层判定' if c >= args.min_gold else f'金标 < {args.min_gold}，跳过判定'} |")
    print()

    def curve(rate):
        out = {}
        arms = {"histgb": s_gate, "random": s_rand, "router": s_route}
        if s_ev is not None:
            arms["router_ev"] = s_ev
        for arm, sc in arms.items():
            reviewed = arm_indices(sc, rate, random.Random(args.seed),
                                   args.floor_every if arm == "router" else 0)
            eff = 100.0 * len(reviewed) / len(rows)
            cov, tot = recall_at(rows, reviewed, oracle, llm, edit_filter=resid)
            out[arm] = (cov, tot, eff)
        return out

    if p_report:
        print("## 收益矩阵 p̂(t)（train 侧：计数 spans × train LLM 缓存重合）+ 计数头")
        print("| 型 | 可测 span | 检出 | p̂ | 头正例句 | 头形态 |")
        print("|---|---|---|---|---|---|")
        for t, n_t, c_t, p, n_pos, mode in sorted(p_report, key=lambda x: -x[3]):
            flag = "" if n_t >= args.p_min_n else " ⚠先验"
            print(f"| {t} | {n_t} | {c_t} | {p:.3f}{flag} | {n_pos} | {mode} |")
        print()

    arms_all = ["histgb", "random", "router"] + (["router_ev"] if s_ev is not None else [])
    ref_arm = "router_ev" if s_ev is not None else "router"
    print("## 等调用率曲线（总体 gold 编辑召回）")
    print("| 调用率 | " + " | ".join(arms_all)
          + f" | {ref_arm}−histgb | {ref_arm}−random |")
    print("|---" * (len(arms_all) + 3) + "|")
    for rate in grid:
        c = curve(rate)
        rc = {a: c[a][0] / max(1, c[a][1]) for a in arms_all}
        mark = " ←锚" if abs(rate - ANCHOR) < 1e-9 else ""
        cells = " | ".join(
            (f"**{rc[a]:.1%}**" if a == ref_arm else f"{rc[a]:.1%}") for a in arms_all)
        print(f"| {rate:g}%{mark} | {cells} | "
              f"{rc[ref_arm]-rc['histgb']:+.1%} | {rc[ref_arm]-rc['random']:+.1%} |")
    print(f"> skip 率 = 1 − 调用率（预算模式；**skipped ≠ covered**，如实入覆盖率账）。\n")

    print(f"## 锚点 {ANCHOR}% 分层判定（{ref_arm} ≥ 另两臂？）")
    scores = {"histgb": s_gate, "random": s_rand, "router": s_route}
    if s_ev is not None:
        scores["router_ev"] = s_ev

    def reviewed_of(arm: str, rate: float) -> set[int]:
        return arm_indices(scores[arm], rate, random.Random(args.seed),
                           args.floor_every if arm == "router" else 0)

    print("| 类型 | gold | " + " | ".join(arms_all) + " | 判定 |")
    print("|---" * (len(arms_all) + 3) + "|")
    fails = 0
    for t in sorted(gold_types, key=lambda x: -type_gold[x]):
        if not type_gold[t]:
            continue
        rr = []
        for arm in arms_all:
            cov, tot = recall_at(rows, reviewed_of(arm, ANCHOR), oracle, llm,
                                 edit_filter=lambda r, j, t=t: r["type"] == t
                                 and (resid is None or resid(r, j)))
            rr.append(cov / max(1, tot))
        if type_gold[t] < args.min_gold:
            verdict = f"金标 {type_gold[t]} < {args.min_gold}，跳过"
        else:
            ok = rr[arms_all.index(ref_arm)] + 1e-12 >= rr[0] and rr[arms_all.index(ref_arm)] + 1e-12 >= rr[1]
            fails += not ok
            verdict = "PASS" if ok else "FAIL"
        print(f"| {t} | {type_gold[t]} | " + " | ".join(f"{x:.1%}" for x in rr) + f" | {verdict} |")
    c = curve(ANCHOR)
    rec = {a: c[a][0] / max(1, c[a][1]) for a in scores}
    for a in ["router", "router_ev"]:
        if a not in rec:
            continue
        ok_a = rec[a] >= rec["histgb"] - 1e-12 and rec[a] >= rec["random"] - 1e-12
        print(f"\n**总体判定（{a}）：{'PASS' if ok_a else 'FAIL'}**（锚点召回 "
              + " / ".join(f"{k} {rec[k]:.1%}" for k in arms_all)
              + f"，gold {c[a][1]}；分层 FAIL {fails}）")
    print(f"\n> level 3 全 LLM 兜底臂不在本 harness 空间（漏报兜底立场不变，§11.10）；")
    print(f"> histgb 为本地代理臂（9.8k 金标基座训练），弱于生产门控——PASS 是下界意义，")
    print(f"> 上线前按 §11.10 与生产门控同数据重赛。")
    print(f"\n耗时 {time.time() - t0:.0f}s")


if __name__ == "__main__":
    main()
