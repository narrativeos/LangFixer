#!/usr/bin/env python3
"""定版快照驱动：直接调 eval_server.run_eval_job（不依赖 FastAPI 栈）。

对评测实例（默认 http://127.0.0.1:4811，生产配置的隔离快照）跑指定基准，
报告落 eval_reports/。用法：python3 eval_run.py <benchmark> [--subset N] [--fresh]
[--workers N] [--repair]

并发纪律（2026-11 定版两次雪崩实测）：网关对生产 key 限 `max_parallel_requests: 5`，
但远端 Qwen3.8 真实吞吐约 1-2 并发（4 并发即排队到 86s+）；评测实例务必用**耐心档**
（LLM 超时 240s + 挂钟预算 900s + 退避 15,45，fail-fast 的 30s 熔断在评测形态是
干扰项）——fail-fast 档下长尾句撞超时即雪崩：超时→重试→排队更糟→批量 unavailable。
`--repair` 清掉缓存里 llm_ran=false 的句子再跑——capability 模式每句都该走 LLM，
降级句必须补测，不能带着缺席定版；收敛判定要按 gold 全集行数，缺席=0 ≠ 跑全。
"""
import json
import sys

import eval_server as ev


def repair_cache(bench: str) -> int:
    """删除 capability 缓存里 LLM 缺席的记录，让本次重跑只补这些句子。"""
    p = ev.cache_path(bench, None, "current", None, "capability")
    if not p.exists():
        return 0
    lines = [ln for ln in p.read_text(encoding="utf-8").splitlines() if ln.strip()]
    keep = [ln for ln in lines if json.loads(ln).get("llm_ran")]
    p.write_text("\n".join(keep) + ("\n" if keep else ""), encoding="utf-8")
    return len(lines) - len(keep)


def main() -> int:
    args = sys.argv[1:]
    bench = args[0]
    subset = None
    if "--subset" in args:
        subset = int(args[args.index("--subset") + 1])
    server = "http://127.0.0.1:4811"
    if "--server" in args:
        server = args[args.index("--server") + 1]
    workers = 2
    if "--workers" in args:
        workers = int(args[args.index("--workers") + 1])
    if "--repair" in args:
        n = repair_cache(bench)
        print(f"repair: 剔除 LLM 缺席记录 {n} 条，本次只补这些句", flush=True)
    job: dict = {"status": "queued", "progress": {"done": 0, "total": 0}}
    cfg = {
        "benchmark": bench,
        "split": None,
        "arm": "current",
        "mode": "capability",
        "subset": subset,
        "seed": 42,
        "workers": workers,
        # subset 有独立缓存路径（cache_path 的 _subsetN 后缀），不再借 --fresh 覆写正式缓存
        "fresh": "--fresh" in args,
    }
    report = ev.run_eval_job(job, server, cfg)
    print(json.dumps({"status": job["status"], "report_id": job.get("report_id"),
                      "error": job.get("error")}, ensure_ascii=False))
    return 0 if job["status"] == "done" else 1


if __name__ == "__main__":
    sys.exit(main())
