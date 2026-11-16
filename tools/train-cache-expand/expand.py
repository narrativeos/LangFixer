#!/usr/bin/env python3
"""train 侧 LLM 缓存扩容（router_ev p̂(t) 测量面，§11.14 补三遗留）。

p̂ 饥饿的根因是 train 缓存只盖到测量面一角：测量面 = zh_v2 池 train ∩
gold_counts(spans 非空)（≈5,070 行，其中池 label=1 4,404 + label=0 漏标侧
666——漏标侧同样要测，p̂ 按编辑真实型分型），现已缓存的 1,200 行里只有
611 落在测量面内。本脚本把测量面补齐。

机件全部复用 router_labels.run_llm_arm（端点浪 502 的三重退避重试、失败行
不入账下次自动重试、每 25 行落盘的断点续跑）；扩容先写
`<cache>.expand`，跑完与主缓存按池序合并——中途被杀不丢已完成条目，重启
后 expand 文件即 seed 续跑。端点只从 LLM_BASE_URL / LLM_MODEL / LLM_API_KEY
环境变量读，任何端点/密钥不落文件。

用法（夜跑）:
    cd LangFixer/feature-model
    LLM_BASE_URL=... LLM_MODEL=... LLM_API_KEY=... \
      .venv/bin/python3 ../tools/train-cache-expand/expand.py --workers 4

    # 跑完重出 p̂ 与 router_ev（p̂ 分化应开始上场）:
    .venv/bin/python3 eval_router_arms.py \
      --labels ~/tmp/true_gold/router_labels_train_newgold.jsonl \
      --llm-cache /tmp/router_llm_arm_gold.jsonl --only-cached \
      --counts ~/tmp/true_gold/gold_counts.jsonl \
      --gain-cache /tmp/router_llm_arm_train.jsonl

--dry-run 只报数不请求；--limit N 分批试跑（如先 50 行验端点健康）。
"""
from __future__ import annotations

import argparse
import json
import os
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent        # tools/train-cache-expand → LangFixer
sys.path.insert(0, str(REPO / "feature-model"))

import router_labels as RL       # noqa: E402  run_llm_arm / load_jsonl

DEFAULT_CACHE = "/tmp/router_llm_arm_train.jsonl"


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--pool-dir", default=str(RL.DEFAULT_POOL_DIR))
    ap.add_argument("--counts", default="~/tmp/true_gold/gold_counts.jsonl")
    ap.add_argument("--cache", default=DEFAULT_CACHE)
    ap.add_argument("--workers", type=int, default=4)
    ap.add_argument("--limit", type=int, default=0, help="本次最多新跑 N 行（0=全量）")
    ap.add_argument("--dry-run", action="store_true")
    a = ap.parse_args()

    pool = RL.load_jsonl(Path(a.pool_dir) / "train.jsonl")
    gc_texts = {r["text"] for r in RL.load_jsonl(Path(a.counts).expanduser())
                if r.get("spans")}
    cache_p = Path(a.cache)
    done: dict[str, list] = {}
    if cache_p.is_file():
        for r in RL.load_jsonl(cache_p):
            done[r["id"]] = r.get("errors", [])
    expand_p = Path(str(cache_p) + ".expand")
    seed_new: dict[str, list] = {}
    if expand_p.is_file():                      # 上次合并前被杀：expand 即 seed
        for r in RL.load_jsonl(expand_p):
            seed_new[r["id"]] = r.get("errors", [])

    todo = [r for r in pool if r["text"] in gc_texts
            and r["id"] not in done and r["id"] not in seed_new]
    n_face = sum(1 for r in pool if r["text"] in gc_texts)
    n_done_face = sum(1 for r in pool if r["id"] in done and r["text"] in gc_texts)
    print(f"池 train {len(pool):,} 行；测量面 {n_face:,}；"
          f"已入账 {len(done):,}（面内 {n_done_face:,}）；"
          f"expand 中已跑 {len(seed_new):,} → 待跑 {len(todo):,}")
    if a.limit:
        todo = todo[:a.limit]
        print(f"--limit {a.limit} → 本次 {len(todo)} 行")
    if a.dry_run:
        return
    if not todo:
        print("测量面已齐，直接重跑 eval 即可。")
        return
    if not os.environ.get("LLM_BASE_URL") or not os.environ.get("LLM_MODEL"):
        sys.exit("需 LLM_BASE_URL / LLM_MODEL（/ LLM_API_KEY）环境变量")

    t0 = time.time()
    got = RL.run_llm_arm(todo, len(todo), str(expand_p),
                         seed_out=seed_new, workers=a.workers)
    merged = dict(done)
    merged.update({k: v for k, v in got.items() if k not in done})
    tmp = str(cache_p) + ".merged.tmp"
    with open(tmp, "w", encoding="utf-8") as f:   # 池序稳定，整文件原子替换
        for r in pool:
            if r["id"] in merged:
                f.write(json.dumps({"id": r["id"], "text": r["text"],
                                    "errors": merged[r["id"]]},
                                   ensure_ascii=False) + "\n")
    os.replace(tmp, cache_p)
    if expand_p.is_file():
        expand_p.unlink()
    n_face_after = sum(1 for r in pool
                       if r["text"] in gc_texts and r["id"] in merged)
    print(f"[expand] 本轮新增 {len(got) - len(seed_new):,} 行，耗时 "
          f"{(time.time() - t0) / 60:.0f} 分钟；测量面覆盖 {n_face_after}/{n_face}"
          f"（{n_face_after / max(1, n_face):.1%}）→ {cache_p}")
    print("[expand] 重出 p̂/router_ev 见本文件 docstring 尾部命令。")


if __name__ == "__main__":
    main()
