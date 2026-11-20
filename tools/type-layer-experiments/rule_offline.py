#!/usr/bin/env python3
"""规则臂离线复刻（同一本 lexicons/zh_gec_lexicon.json，两本账是禁忌）。

experiments.py §实验3 与 rule_probe.py 共用的唯一实现。POS 路径规则
（ZH_DE_DA_DE 等需 NLP 服务）不复刻 = 下界口径（EVOLUTION §11.14 补二）。

punct_mode:
  legacy — 补二原口径：ASCII 标点紧邻 CJK 即命中（7,587 命中 2 中的语料毒账
           就是这么测出来的，复现锚点用这个）；
  gated  — 补二定案口径：**文档标点风格门**，全角占比 ≥0.5 的文档 punct 才
           启用（生产文本默认；评测语料通篇半角 → punct 臂归零）。

返回 [(a, b, kind, tier)]，tier：定案=confusables+anomaly(strong)，
提示=punct+anomaly(weak)。
"""
from __future__ import annotations

import json
import re
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
LEX = json.load(open(REPO / "lexicons" / "zh_gec_lexicon.json", encoding="utf-8"))

CONF: list[tuple] = []
for _e in LEX["confusables"]:
    if isinstance(_e, dict):
        CONF.append((_e["wrong"], _e["right"], _e.get("condition", {})))
    else:
        CONF.append((_e[0], _e[1], None))
PMAP = {p[0]: p[1] for p in LEX["punct_map"]}
ANOM = [(re.compile(p["pattern"]), bool(p.get("strong", False)))
        for r in LEX["rules"] if r.get("category") == "anomaly"
        for p in r.get("patterns", [])]

_CJK = lambda ch: "\u4e00" <= ch <= "\u9fff"


def fullwidth_share(src: str) -> float:
    fw = sum(src.count(c) for c in "。，！？；：、（）")
    hw = sum(src.count(c) for c in ".,!?;:、()") - src.count("、")
    return fw / max(1, fw + hw)


def rule_hits(src: str, punct_mode: str = "legacy") -> list[tuple]:
    hits: list[tuple] = []
    for wrong, right, cond in CONF:
        start = 0
        while True:
            i = src.find(wrong, start)
            if i < 0:
                break
            ok = True
            if cond:
                if "context_regex" in cond:
                    win = src[max(0, i - 2): i + len(wrong) + 2]
                    ok = re.search(cond["context_regex"], win) is not None
                if ok and "next_text" in cond:
                    j = i + len(wrong)
                    ok = j < len(src) and src[j] in cond["next_text"]
            if ok:
                hits.append((i, i + len(wrong), "conf", "定案"))
            start = i + 1
    punct_on = punct_mode == "legacy" or fullwidth_share(src) >= 0.5
    if punct_on:
        for i, ch in enumerate(src):
            if ch in PMAP and ((i > 0 and _CJK(src[i - 1]))
                               or (i + 1 < len(src) and _CJK(src[i + 1]))):
                hits.append((i, i + 1, "punct", "提示"))
    sents = []
    s = 0
    for i, ch in enumerate(src):
        if ch in "。！？!?\n":
            sents.append((s, i + 1))
            s = i + 1
    if s < len(src):
        sents.append((s, len(src)))
    for a, b in sents:
        seg = src[a:b].lstrip()
        if seg and seg[0] in "「『“\"'《【(":   # in-quote guard（近似）
            continue
        for pat, strong in ANOM:
            if pat.search(seg):
                i0 = a + src[a:b].find(seg)
                hits.append((i0, b, "anomaly", "定案" if strong else "提示"))
                break
    return hits
