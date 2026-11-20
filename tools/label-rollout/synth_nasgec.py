#!/usr/bin/env python3
"""v2.4 合成配方：NaSGEC 式四操作（S/D/I/T）已知位置注入 → label_rollout 判定行。

背景（notebook §P）：mark 主体是漏字/多字/搭配/语序，静态规则够不着；正路之一是
下一轮 rollout 配方。本脚本是那一条的子轨：在**干净句**上做 NaSGEC 式四类规则注入，
位置已知（§281 教训：位置已知才可安全标注），产出 label_rollout.py 的 SYNTH 行，
由 LLM 判定环节验真——判「是错且修回」= 注入坐实；判「无错」= 注入不真实，丢弃。

四操作（全部纯路径，无 LTP）：
  S 同音/混淆字替换   —— 混淆字表复用 datasets/augment_domain.py（vendor Apache-2.0）
  D 漏字              —— 删一个虚词字（的/了/着/过/地/得/在/就/也/并/而/或/把/被/让/给）
  I 多字              —— 在两个汉字之间插入一个虚词字
  T 语序              —— 交换两个「，」分句；无分句时交换相邻两个汉字

行 schema（label_rollout 行 + synth 元数据）：
  {"set": "synth", "src": 错句,
   "edits": [{"span": [错句字节 lo, hi], "op": "S|D|I|T", "ref": 干净替换}...],
   "synth": {"recipe": "nasgec-like-v24", "seed": 42, "clean": 干净句, "ops": [...]}}

纪律（同 label-rollout README）：
  1. 进池前先跑 `datasets/gold_error_dist.py --pairs` 对照（错误生成机制与真实流量
     同源判定，§13 #2/#4）；--format pairs 即该审计的输入。
  2. provenance 随数据传递（synth.recipe/seed 在行内）。
  3. LLM 验真的坐实项仍按每桶 ≥58 抽检后才入 gold。

用法：
  python3 synth_nasgec.py --clean clean.txt --out synth_rows.jsonl --seed 42
  python3 synth_nasgec.py --clean clean.txt --format pairs --out synth_pairs.jsonl
  VER=v2.4 SYNTH=~/tmp/synth_rows.jsonl GP_DATA=~/tmp/gate-probe OUT=~/tmp/dataset_v24 \\
    LANGFIXER_LLM_KEY=… python3 tools/label-rollout/label_rollout.py
  python3 synth_nasgec.py --selftest
"""
from __future__ import annotations

import argparse
import difflib
import json
import os
import random
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.environ.get("REPO", os.path.join(HERE, "..", "..")))

# S/T 借用 datasets/augment_domain.py 的纯路径注入器与混淆字表（同仓 vendor 数据）
sys.path.insert(0, os.path.join(REPO, "datasets"))
try:
    import augment_domain as AD
except Exception as _e:  # 仓库结构异常时给出明确报错，而不是静默降级
    AD = None
    _AD_ERR = _e

# 虚词字集：真实漏字/多字的主体是功能词（NaSGEC 的 D/I 也以字符级增删为主）
DEL_CHARS = list("的了着过地得在就也并而或把被让给")
INS_CHARS = list("的了着过地得所")
HAN = lambda c: "\u4e00" <= c <= "\u9fff"


def op_S(clean: str):
    """混淆字替换（复用 CEC 混淆字表）。"""
    if AD is None:
        return None
    res = AD.aug_wrong_word(clean, _confuse_set())
    return res["source"] if res else None


def op_D_pick(clean: str):
    idxs = [i for i, c in enumerate(clean) if c in DEL_CHARS]
    if not idxs:
        return None
    i = random.choice(idxs)
    return clean[:i] + clean[i + 1:]


def op_I(clean: str):
    idxs = [i for i in range(1, len(clean)) if HAN(clean[i - 1]) and HAN(clean[i])]
    if not idxs:
        return None
    i = random.choice(idxs)
    ch = random.choice(INS_CHARS)
    if clean[max(0, i - 2):i + 2].count(ch) >= 2:   # 别造「的的」式连续双写，太假
        return None
    return clean[:i] + ch + clean[i:]


def op_T(clean: str):
    parts = clean.split("，")
    if len(parts) >= 2:
        a, b = random.sample(range(len(parts)), 2)
        parts[a], parts[b] = parts[b], parts[a]
        return "，".join(parts)
    idxs = [i for i in range(len(clean) - 1) if HAN(clean[i]) and HAN(clean[i + 1])
            and clean[i] != clean[i + 1]]
    if not idxs:
        return None
    i = random.choice(idxs)
    return clean[:i] + clean[i + 1] + clean[i] + clean[i + 2:]


OPS = {"S": op_S, "D": op_D_pick, "I": op_I, "T": op_T}
_confuse_cache = {}


def _confuse_set():
    if "set" not in _confuse_cache:
        _confuse_cache["set"] = AD.load_confuse_set() if AD else {}
    return _confuse_cache["set"]


def diff_edits(broken: str, clean: str, intended: list):
    """字符级 diff → label_rollout edits（字节 span 落在错句上；ref 为干净替换）。

    单编辑行按 diff 签名定 op；多操作行里 diff 无法区分 T 的拆段，T 行的非 equal
    编辑统一标 T（行级 synth.ops 仍是权威来源）。
    """
    edits = []
    for tag, i1, i2, j1, j2 in difflib.SequenceMatcher(None, broken, clean).get_opcodes():
        if tag == "equal":
            continue
        if len(intended) == 1:
            op = intended[0]               # 注入器意图是权威：重复字句里 diff 对齐会走样，签名不可靠
        elif tag == "delete":
            op = "I"                       # 错句比干净句多出来的内容
        elif tag == "insert":
            op = "D"                       # 错句比干净句缺的内容
        elif i2 - i1 == j2 - j1 == 1:
            op = "S"                       # 单字替换
        elif "T" in intended:
            op = "T"                       # 分句交换在 diff 里可能拆成长度不等的段
        else:
            op = "I" if i2 - i1 > j2 - j1 else "D"
        edits.append({
            # 字节 span 落在错句上（label_rollout 的 b2c 口径：字节→字需先解码前缀）
            "span": [len(broken[:i1].encode("utf-8")), len(broken[:i2].encode("utf-8"))],
            "op": op,
            "ref": clean[j1:j2],
            "_c": [i1, i2],
        })
    return edits


def synth_sentence(clean: str, ops: list, per: int, set_name: str, seed: int):
    """一句干净文本 → ≤per 条单错/多错行；注入失败的操作自动换下一 op 重试。"""
    out = []
    for k in range(per):
        order = [ops[(k + j) % len(ops)] for j in range(len(ops))]   # 失败自动换下一 op
        broken, used = clean, []
        for op in order:
            nb = OPS[op](clean)
            if nb and nb != clean:
                broken, used = nb, used + [op]
                break
        if not used:
            continue
        edits = diff_edits(broken, clean, used)
        if not edits:
            continue
        row = {"set": set_name, "src": broken,
               "edits": [{"span": e["span"], "op": e["op"], "ref": e["ref"]} for e in edits],
               "synth": {"recipe": "nasgec-like-v24", "seed": seed, "clean": clean, "ops": used}}
        out.append((row, edits, broken))
    return out


def apply_reverse(broken: str, edits: list, clean: str) -> bool:
    s = broken
    for e in sorted(edits, key=lambda x: x["_c"][0], reverse=True):
        s = s[:e["_c"][0]] + e["ref"] + s[e["_c"][1]:]
    return s == clean


SELFTEST_CLEAN = [
    "武汉大学图书馆以其历史悠久、藏书丰富、建筑宏伟而闻名于世。",
    "图书馆前身为19世纪末湖广总督张之洞创办的湖北自强学堂图书室。",
    "2000年8月，武汉大学与武汉水利电力大学合并，四校图书馆合并为新的图书馆。",
    "全校现有图书馆馆舍面积为七万七千多平方米，资料室为十四万余平方米。",
    "文献资源总量达二百六十五十五万册，其中图书六百一十五万册。",
    "图书馆服务功能完备，拥有多种现代化的服务手段，可为读者提供外借、阅览等服务。",
    "读者可通过手机、平板、台式机等各类终端在线检索馆藏纸本及电子资源。",
    "百年名校，人文荟萃；珞珈山下，卷帙飘香。",
    "馆内收藏有自1907年以来近一个世纪的生物医学检索信息。",
    "已经形成印刷型文献、数字化文献和网上文献信息三位一体的资源保障体系。",
    "其中，文献借阅中心主要收藏哲学、历史、经济、政治、法律等类文献。",
    "工学分馆主要收藏水利、电工、动力、土木建筑、电子、机械等类文献。",
    "全校30个院系、研究中心及实验室设有图书资料室，共同组成文献服务网络。",
    "图书馆藏有古籍二十余万册，有二百六十多种收入中国古籍善本书目。",
    "医学分馆主要收藏基础医学、临床医学、护理学、口腔医学等类文献。",
    "现代信息服务环境下的图书馆员将以忠诚的敬业精神引导大家度过求学时光。",
]


def selftest() -> None:
    random.seed(7)
    seen, n, fails = {}, 0, 0
    for i, sent in enumerate(SELFTEST_CLEAN):
        op = "SDIT"[i % 4]                 # 轮流指定操作，保证四操作全覆盖
        rows = synth_sentence(sent, [op], 1, "synth", 7)
        for row, edits, broken in rows:
            n += 1
            if not apply_reverse(broken, edits, sent):
                fails += 1
                print(f"[FAIL] roundtrip: {row['synth']['clean']} → {broken}")
            for e in edits:                       # 字节 span ↔ 字序号往返（b2c 同口径）
                lo, hi = e["span"]
                a = len(broken.encode("utf-8")[:lo].decode("utf-8", "ignore"))
                b = len(broken.encode("utf-8")[:hi].decode("utf-8", "ignore"))
                assert [a, b] == e["_c"], f"span 字节口径错: {broken!r} {e}"
            for e in edits:
                seen[e["op"]] = seen.get(e["op"], 0) + 1
                assert e["op"] in "SDIT"
                assert row["synth"]["ops"][0] == e["op"], \
                    f"op 标签与注入意图不符: {row['synth']['ops']} {e['op']}"
    missing = set("SDIT") - set(seen)
    assert not fails, f"{fails} 行还原失败"
    assert not missing, f"自测未覆盖操作: {missing}"
    print(f"[selftest] ok: {n} 行全部按 edits 还原为干净句；操作分布 {seen}")


def load_clean(path: str) -> list:
    out = []
    with open(path, encoding="utf-8") as f:
        if path.endswith(".jsonl"):
            for line in f:
                d = json.loads(line)
                s = d.get("text") or d.get("source") or d.get("src") or d.get("sentence") or d.get("content")
                if s:
                    out.append(s)
        else:
            for line in f:
                if line.strip():
                    out.append(line.strip())
    return out


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--clean", help="干净文本 .txt（每行一句）或 .jsonl")
    ap.add_argument("--out", help="输出 jsonl（rows 或 pairs 由 --format 决定）")
    ap.add_argument("--format", choices=["rows", "pairs"], default="rows")
    ap.add_argument("--seed", type=int, default=42)
    ap.add_argument("--ops", default="S,D,I,T", help="操作子集，如 S,T")
    ap.add_argument("--per-sentence", type=int, default=1, help="每句最多产出几条注入行")
    ap.add_argument("--set-name", default="synth")
    ap.add_argument("--limit", type=int, default=0, help="最多产出行数")
    ap.add_argument("--selftest", action="store_true")
    a = ap.parse_args()
    if a.selftest:
        selftest()
        return
    assert a.clean and a.out, "--clean 与 --out 必填（或 --selftest）"
    if AD:
        AD.set_seed(a.seed)
    random.seed(a.seed)
    ops = [o.strip().upper() for o in a.ops.split(",") if o.strip()]
    assert ops and set(ops) <= set(OPS), f"--ops 只允许 {list(OPS)}"
    clean_lines = load_clean(a.clean)
    n_skip = 0
    stat = {"rows": 0}
    with open(a.out, "w", encoding="utf-8") as f:
        for si, s in enumerate(clean_lines):
            if a.limit and stat["rows"] >= a.limit:
                break
            rot = ops[si % len(ops):] + ops[:si % len(ops)]   # 句间轮转，保证四操作均衡
            rows = synth_sentence(s, rot, a.per_sentence, a.set_name, a.seed)
            if not rows:
                n_skip += 1
                continue
            for row, _ed, _broken in rows:
                stat["rows"] = stat.get("rows", 0) + 1
                for e in row["edits"]:
                    stat[e["op"]] = stat.get(e["op"], 0) + 1
                if a.format == "rows":
                    f.write(json.dumps(row, ensure_ascii=False) + "\n")
                else:
                    f.write(json.dumps({"source": row["src"], "target": row["synth"]["clean"]},
                                       ensure_ascii=False) + "\n")
    dist = {k: v for k, v in stat.items() if k != "rows"}
    print(f"[synth] 行 {stat.get('rows', 0):,} · 跳过 {n_skip:,} · 编辑分布 {dist}")
    print(f"[synth] 进池前先审同源：python3 datasets/gold_error_dist.py --pairs {a.out}")


if __name__ == "__main__":
    main()
