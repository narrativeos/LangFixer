#!/usr/bin/env python3
"""候选 1「合成 I 监督臂」数据生成器：干净句 →「删 1 字」行（漏字位已知）。

背景（README 调研节 D 候选 1）：gray380 真实 I 位补字分布 **79% 实词 / 21% 虚词**，
而 synth_nasgec 的 D 操作只删虚词——字分布错位是「合成行训不出 insert 定位」的
已知原因之一。本脚本按真实分布采样删除位，并用 trigram Δ 过滤剔除「可省伪影」
（删完照读 = 比率≈1，会把 ② 定位臂教成在干净句上报 FP；「的」D 类取证同教训）。

与 synth_nasgec 的分工：那边的 D 是判定行验真配方（虚词、LLM 验真）；这边是
**定位臂监督源**（全字集、位级 Δ 过滤、免 LLM——位置由构造已知，监督是位置本身）。

伪影过滤（位级 Δ，复用 `feature-model/ngram_table.json` 1M trigram 计数）：
干净句 …a b [c] d e… 删 c 后 …a b d e…；
    Δ = ln( (cnt(abc)+cnt(bcd)+cnt(cde)+3k) / (cnt(abd)+cnt(bde)+2k) )
真实漏字处破句局部坍塌（Δ 高）；可省字处破句照读（Δ≈0）→ Δ < --tau 丢弃。
毒样本硬闸：c 与左/右邻同字（常|常→常 式）必弃——破句 100% 干净，纯 FP 生成器。

行 schema 与 synth_nasgec 相同（set/src/edits[span,op,ref]/synth 元数据），
op 用 "D"（错句缺字＝漏字；纠错侧为 insert）。--format pairs 出 {source,target}。

用法：
  python3 synth_insert_arm.py --selftest
  python3 synth_insert_arm.py --clean-pool ../feature-model/l3_pool_raw --limit 2000 \
      --out /tmp/insert_arm_rows.jsonl
  评测纪律（继承灰区 A/B 教训）：合成红利≠真实红利——训练用本数据，
  评测必须挂 gray380 真实 I 位（两臂都漏的 401 个）+ 真实净句 FP 口径。
"""
from __future__ import annotations

import argparse
import json
import math
import os
import random

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.environ.get("REPO", os.path.join(HERE, "..", "..")))
NGRAM = os.path.join(REPO, "feature-model", "ngram_table.json")
POOL_DEFAULT = os.path.join(REPO, "feature-model", "l3_pool_raw")

FUNC_CHARS = set("的了着过地得在就也并而或把被让给")   # 与 synth_nasgec DEL_CHARS 同集
FUNC_SHARE = 0.21   # gray380 真实 I 位补字的虚词占比（2026-11-20 取证）
K = 1.0             # trigram 平滑
HAN = lambda c: "\u4e00" <= c <= "\u9fff"


class Table:
    def __init__(self, path: str):
        self.d = json.load(open(path, encoding="utf-8"))

    def cnt(self, tri: str) -> int:
        return self.d.get(tri, 0)


def delta_delete(s: list, i: int, tab: Table) -> float:
    """删 s[i] 的位级 Δ（ln 质量比）；边界窗缺位按 0 计。"""
    def at(j):  # 越界返回 None → 该 trigram 计 0
        return "".join(s[j:j + 3]) if j >= 0 and j + 3 <= len(s) else None

    clean_mass = sum((tab.cnt(at(j)) if at(j) else 0) for j in (i - 2, i - 1, i)) + 3 * K
    b = s[i - 1] if i - 1 >= 0 else None
    d = s[i + 1] if i + 1 < len(s) else None
    e = s[i + 2] if i + 2 < len(s) else None
    broken_mass = 2 * K
    if b is not None and d is not None:
        broken_mass += tab.cnt(s[i - 2] + b + d) if i - 2 >= 0 else 0
        broken_mass += tab.cnt(b + d + e) if e is not None else tab.cnt(b + d)
    return math.log(clean_mass / broken_mass)


def delete_ok(s: list, i: int) -> bool:
    """删除位硬闸：汉字、非重复字邻位（毒样本闸）、删后不成叠字。"""
    c = s[i]
    if not HAN(c):
        return False
    if i > 0 and s[i - 1] == c:
        return False
    if i + 1 < len(s) and s[i + 1] == c:
        return False
    return True


def synth_one(clean: str, tab: Table, tau: float, rng: random.Random, tries: int = 8):
    """一句干净文本 → ≤1 条过闸删字行；失败返 None。"""
    s = list(clean)
    pos = [i for i in range(len(s)) if delete_ok(s, i, )]
    if not pos:
        return None
    func_pool = [i for i in pos if s[i] in FUNC_CHARS]
    cont_pool = [i for i in pos if s[i] not in FUNC_CHARS]
    for _ in range(tries):
        pick_func = func_pool and (not cont_pool or rng.random() < FUNC_SHARE)
        pool = func_pool if pick_func else cont_pool or func_pool
        i = rng.choice(pool)
        if delta_delete(s, i, tab) < tau:
            continue
        broken = "".join(s[:i] + s[i + 1:])
        lo = len("".join(s[:i]).encode("utf-8"))
        row = {"set": "insert_arm", "src": broken,
               "edits": [{"span": [lo, lo], "op": "D", "ref": s[i]}],
               "synth": {"recipe": "insert-arm-v1", "seed": None, "clean": clean,
                         "ops": ["D"], "delta": round(delta_delete(s, i, tab), 3),
                         "pos_c": i}}
        return row, broken
    return None


def load_clean(path: str, limit_files: int = 0) -> list:
    """池目录/文件 → 干净句列表。TSV 第 2 列=干净侧；纯文本整行。"""
    files = []
    if os.path.isdir(path):
        files = [os.path.join(path, f) for f in sorted(os.listdir(path)) if not f.startswith(".")]
    else:
        files = [path]
    out = []
    for fp in files:
        with open(fp, encoding="utf-8") as f:
            for line in f:
                line = line.rstrip("\n")
                if not line:
                    continue
                cols = line.split("\t")
                s = cols[1] if len(cols) >= 2 and cols[1] else (cols[0] if len(cols) == 1 else "")
                if s:
                    out.append(s)
        if limit_files and len(out) >= limit_files:
            break
    return out


def selftest() -> None:
    class Fake:
        def cnt(self, tri):
            return 0 if "X" in tri else 100
    tab = Fake()
    s = list("他很爱学习")
    assert delete_ok(s, 2), "普通位应可删"
    s2 = list("常常回家")
    assert not delete_ok(s2, 1) and not delete_ok(s2, 0), "叠字邻位必须判毒"
    r, broken = synth_one("他很爱学习。", Fake(), tau=-1.0, rng=random.Random(1))
    assert len(broken) + 1 == len("他很爱学习。")
    e = r["edits"][0]
    lo = e["span"][0]
    # 字节 span 往返：lo 处插入 ref 即还原
    s2b = broken.encode("utf-8")
    back = (s2b[:lo] + e["ref"].encode("utf-8") + s2b[lo:]).decode("utf-8")
    assert back == r["synth"]["clean"], f"还原失败: {back!r}"
    print("[selftest] ok: 毒位闸 + 字节 span 还原通过")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--clean-pool", default=POOL_DEFAULT, help="池目录或文件（TSV 第2列=干净侧）")
    ap.add_argument("--out", help="输出 jsonl")
    ap.add_argument("--format", choices=["rows", "pairs"], default="rows")
    ap.add_argument("--seed", type=int, default=42)
    ap.add_argument("--tau", type=float, default=1.1, help="Δ 门槛（ln 比），低于=可省伪影丢弃")
    ap.add_argument("--limit", type=int, default=0, help="最多产出行数")
    ap.add_argument("--max-sent", type=int, default=0, help="最多尝试干净句数（smoke 用）")
    ap.add_argument("--selftest", action="store_true")
    a = ap.parse_args()
    if a.selftest:
        selftest()
        return
    assert a.out, "--out 必填（或 --selftest）"
    rng = random.Random(a.seed)
    tab = Table(NGRAM)
    clean_lines = load_clean(a.clean_pool, limit_files=a.max_sent or 0)
    if a.max_sent:
        clean_lines = clean_lines[:a.max_sent]
    stat = {"rows": 0, "try": 0, "rej_delta": 0, "rej_dead": 0, "func": 0, "cont": 0}
    deltas = []
    chars = {}
    with open(a.out, "w", encoding="utf-8") as f:
        for s in clean_lines:
            if a.limit and stat["rows"] >= a.limit:
                break
            stat["try"] += 1
            got = synth_one(s, tab, a.tau, rng)
            if not got:
                continue
            row, broken = got
            d = row["synth"]["delta"]
            c = row["edits"][0]["ref"]
            if d < a.tau:
                stat["rej_delta"] += 1
                continue
            stat["rows"] += 1
            stat["func" if c in FUNC_CHARS else "cont"] += 1
            deltas.append(d)
            chars[c] = chars.get(c, 0) + 1
            row["synth"]["seed"] = a.seed
            if a.format == "rows":
                f.write(json.dumps(row, ensure_ascii=False) + "\n")
            else:
                f.write(json.dumps({"source": broken, "target": row["synth"]["clean"]},
                                   ensure_ascii=False) + "\n")
    deltas.sort()
    p50 = deltas[len(deltas) // 2] if deltas else 0
    p95 = deltas[int(len(deltas) * .95)] if deltas else 0
    fn = stat["func"] + stat["cont"]
    print(f"[insert-arm] 句 {stat['try']:,} · 行 {stat['rows']:,} · Δp50={p50:.2f} p95={p95:.2f}")
    print(f"[insert-arm] 虚词 {stat['func']:,} ({100*stat['func']/max(fn,1):.1f}%) / 实词 {stat['cont']:,} · top10 {sorted(chars.items(), key=lambda kv: -kv[1])[:10]}")
    print("[insert-arm] 纪律：评测必须挂 gray380 真实 401 I 位 + 真实净句 FP 口径（合成红利≠真实红利）")


if __name__ == "__main__":
    main()
