#!/usr/bin/env python3
"""看一轮真实重演产物，不用临时 Python 现编（`MULTIMODAL.md` §15 / §19）。

用法：
    scripts/media_replay_inspect.py                # 默认 target/media-replay
    scripts/media_replay_inspect.py <产物目录>      # 指定某一轮（LANGFIXER_REPLAY_DIR 指过的）

输出：每载体抽取与告警 → 候选行（规则 / 定位 / 命中口径 / 归因）→ 反思靶子
（含 `rule_via`：坐标重合还是原文重叠兜底）。只读，不改任何产物。
"""
import collections
import json
import os
import sys

DEFAULT_DIR = os.path.join("target", "media-replay")


def load(path):
    if not os.path.isfile(path):
        sys.exit(f"缺少 {path}（先跑：LANGFIXER_MEDIA_DIR=<原图目录> "
                 "cargo test -p api-server --test media_gold_eval -- --ignored）")
    with open(path, encoding="utf-8") as f:
        return json.load(f)


def main():
    d = sys.argv[1] if len(sys.argv) > 1 else DEFAULT_DIR
    cands = load(os.path.join(d, "candidates.json"))
    rows = cands["rows"] if isinstance(cands, dict) else cands
    seeds = load(os.path.join(d, "reflection_seeds.json"))

    by_carrier = collections.OrderedDict()
    for r in rows:
        by_carrier.setdefault(r.get("job_id", "?"), []).append(r)

    print(f"=== {d} ===")
    print(f"候选 {len(rows)} 行 / {len(by_carrier)} 个载体")
    for carrier, rs in by_carrier.items():
        located = sum(1 for r in rs if r.get("located"))
        hit = sum(1 for r in rs if r.get("gold_no"))
        pos = sum(1 for r in rs if r.get("gold_match") == "positional")
        print(f"\n[{carrier}] {len(rs)} 行 · 可定位 {located} · 命中 gold {hit}"
              f"（位置口径 {pos}）")
        by_rule = collections.Counter(r.get("rule_id") or "(无规则)" for r in rs)
        print("  按规则: " + ", ".join(f"{k}×{v}" for k, v in by_rule.most_common()))
        for r in rs:
            print(f"  #{r.get('no')} {r.get('location')} | {r.get('original')!r} → "
                  f"{r.get('suggestion')!r} | rule={r.get('rule_id')} "
                  f"conf={r.get('confidence')} gold={r.get('gold_no')} "
                  f"match={r.get('gold_match')} counted={r.get('counted')}")

    misses = seeds.get("misses", [])
    via = collections.Counter(m.get("rule_via") or "(无归因)" for m in misses)
    print(f"\n=== 闭卷漏报 {len(misses)} 条 / gold {seeds.get('gold_total')} 条 ===")
    print("  规则归因: " + ", ".join(f"{k}×{v}" for k, v in via.most_common()))
    for m in misses:
        print(f"  #{m['no']} counted={m['counted']} rule={m.get('rule_id')} "
              f"via={m.get('rule_via')} {m['original'][:32]!r}")

    path = os.path.join(d, "reflection_suggestions.json")
    if os.path.isfile(path):
        for s in load(path):
            print(f"  建议 [{s.get('kind')}] target={s.get('target')} "
                  f"priority={s.get('priority')} · {s.get('summary')}")
    print("\n勾选 candidates.json 的 `checked` 后导出正误表：")
    print(f"  cargo run -p api-server --bin media-report -- {os.path.join(d, 'candidates.json')}")


if __name__ == "__main__":
    main()
