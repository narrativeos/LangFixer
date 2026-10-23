#!/usr/bin/env python3
"""LLM A/B 评测（MuCGEC dev）：对 gold 每条源句走 api-server 生产检测路径
（`POST /gec/detect` + `force_llm`），按 `source == "llm"` 过滤 LLM 层产出，
双口径打分并与规则基线（`gec-zh --test mucgec_eval`）对照：

1. **cat_rule 口径**（与 `mucgec_eval.rs` 一致，span 重叠判定）：
   各 cat_rule 召回 + 误报（未命中任何 gold 编辑的 LLM 预测）——
   与 A.3 基线（误报 4、confusable 2/84、variant 20/20）直接可比；
2. **F0.5 口径**（ChERRANT 字级，`score_f05.py`）：严格 (op, ref, span) 匹配，
   本脚本只负责产出 pred JSON（gold 同构），打分由 `score_f05.py` 完成。

用法（api-server 需已启动且设置已切到目标臂，如 `llm_model_id=cec4`）：
  python3 eval_llm_mucgec.py --arm cec4 --server http://127.0.0.1:4801 \
      --gold mucgec_gold.json --out pred_cec4.json --workers 8
  python3 score_f05.py --gold mucgec_gold.json --pred pred_cec4.json --beta 0.5

- 逐句结果缓存于 `<out>.cache.jsonl`（中断可续跑；`--fresh` 清空）
- `--limit N` 冒烟；`--verbose` 打印误报明细
"""
import argparse
import concurrent.futures
import json
import os
import time
import urllib.error
import urllib.request


def detect(server, text, timeout=180, attempts=3):
    """POST /gec/detect（force_llm）；5xx / 连接错误退避重试，4xx 立即失败。"""
    body = json.dumps({"lang": "zh", "text": text, "force_llm": True}).encode("utf-8")
    last_err = None
    for i in range(attempts):
        try:
            req = urllib.request.Request(
                f"{server.rstrip('/')}/gec/detect",
                data=body,
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


def to_pred_edits(errs):
    """api-server 错误项 → gold 同构编辑（仅 LLM 层）。"""
    out = []
    for e in errs:
        if e.get("source") != "llm":
            continue
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
            "op": op,
            "src": orig,
            "ref": corr,
            "span": span,
            "cat": e.get("error_type") or "",
            "cat_rule": "llm",
        })
    return out


def overlaps(p, g):
    return p[0] < g[1] and g[0] < p[1]


def score_cat_rule(gold, preds_by_src, verbose=False):
    """cat_rule 召回 + 误报（mucgec_eval.rs 口径：span 重叠；零宽 gold 不计）。"""
    per_cat = {}
    n_sent = 0
    n_pred = 0
    n_fp = 0
    total_hit = 0
    for item in gold:
        n_sent += 1
        errs = preds_by_src.get(item["src"], [])
        n_pred += len(errs)
        for e in item["edits"]:
            entry = per_cat.setdefault(e.get("cat_rule", "?"), [0, 0])
            entry[1] += 1
            if e["span"][1] > e["span"][0] and any(
                p["span"][1] > p["span"][0] and overlaps(p["span"], e["span"]) for p in errs
            ):
                entry[0] += 1
                total_hit += 1
        for p in errs:
            if p["span"][1] > p["span"][0] and not any(
                g["span"][1] > g["span"][0] and overlaps(p["span"], g["span"])
                for g in item["edits"]
            ):
                n_fp += 1
                if verbose:
                    print(f"  [FP] {item['src'][:40]} | {p['src']!r} -> {p['ref']!r} | {p['cat']}")
    return {
        "sentences": n_sent,
        "pred_edits": n_pred,
        "fp_edits": n_fp,
        "total_hit": total_hit,
        "per_cat": per_cat,
    }


def main():
    ap = argparse.ArgumentParser(description="LLM A/B 评测（MuCGEC dev，force_llm 生产路径）")
    ap.add_argument("--arm", required=True, help="臂名（报告标签，如 cec4 / generic）")
    ap.add_argument("--server", default="http://127.0.0.1:4801", help="api-server 地址")
    ap.add_argument("--gold", default="mucgec_gold.json")
    ap.add_argument("--out", required=True, help="pred JSON 输出（score_f05.py 输入）")
    ap.add_argument("--workers", type=int, default=8)
    ap.add_argument("--limit", type=int, default=0, help="只跑前 N 句（冒烟）")
    ap.add_argument("--fresh", action="store_true", help="忽略缓存重跑")
    ap.add_argument("--verbose", action="store_true")
    args = ap.parse_args()

    with open(args.gold, encoding="utf-8") as f:
        gold = [g for g in json.load(f) if g.get("split") == "dev"]
    if args.limit:
        gold = gold[: args.limit]
    print(f"gold dev 句数: {len(gold)}（臂: {args.arm}）")

    cache_path = args.out + ".cache.jsonl"
    cache = {}
    if not args.fresh and os.path.exists(cache_path):
        with open(cache_path, encoding="utf-8") as f:
            for line in f:
                rec = json.loads(line)
                cache[rec["src"]] = rec
        print(f"缓存命中 {len(cache)} 句")

    def work(item):
        src = item["src"]
        if src in cache:
            return cache[src]
        resp = detect(args.server, src)
        rec = {"src": src, "edits": to_pred_edits(resp.get("errors", [])),
               "llm_ms": (resp.get("timings") or {}).get("llm_ms")}
        with open(cache_path, "a", encoding="utf-8") as f:
            f.write(json.dumps(rec, ensure_ascii=False) + "\n")
        cache[src] = rec
        return rec

    preds_by_src = {}
    done = 0
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.workers) as pool:
        for rec in pool.map(work, gold):
            preds_by_src[rec["src"]] = rec["edits"]
            done += 1
            if done % 100 == 0:
                print(f"  进度 {done}/{len(gold)}")

    pred_items = [{"split": "dev", "src": g["src"], "edits": preds_by_src.get(g["src"], [])}
                  for g in gold]
    with open(args.out, "w", encoding="utf-8") as f:
        json.dump(pred_items, f, ensure_ascii=False, indent=1)

    st = score_cat_rule(gold, preds_by_src, verbose=args.verbose)
    print(f"\n=== 臂 {args.arm}：cat_rule 口径（span 重叠；与 mucgec_eval.rs 基线可比） ===")
    print(f"句 {st['sentences']} / LLM 预测编辑 {st['pred_edits']} / 误报编辑 {st['fp_edits']} / 命中 {st['total_hit']}")
    for cat in sorted(st["per_cat"], key=lambda c: -st["per_cat"][c][1]):
        hit, tot = st["per_cat"][cat]
        print(f"  {cat:<12} 召回 {hit}/{tot}")
    print(f"\npred 已写 {args.out}（下一步：score_f05.py --gold {args.gold} --pred {args.out} --beta 0.5）")


if __name__ == "__main__":
    main()