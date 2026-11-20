#!/usr/bin/env python3
"""规则族谓词：规则通道的「静态规则先行」（tools/rule-channel/README 约定①）。

谓词签名 f(src, pos, ref) -> bool：pos 为疑似点字符位（gold 空 range 时是
插入位，字符可能不在 src 里），ref 为金集建议文本（可空）。
每条规则族要能回答两问：命中 rule-candidate 多少（召回）、在全语料上
额外点火多少（打扰率）——由 mine_rules.py 量化后决定去留。
"""
from __future__ import annotations

import re
import unicodedata

HALF = set(",.;:?!()\"'%")               # 半角标点观察集
FULL = set("，。；：？！（）、")       # 文档惯例探测用
UNIT_CHARS = set("年月日号")            # 时间量纲（严式：仅紧邻数字）
PLACE_CHARS = set("省市区县路街村站桥港国内外")


def is_cjk(c: str) -> bool:
    return "\u4e00" <= c <= "\u9fff"


def punct_stream(src: str, pos: int, ref: str = "") -> bool:
    """半角标点夹在中文语流里。"""
    p = src[pos] if pos < len(src) else ""
    if p not in HALF:
        return False
    left = src[pos - 1] if pos else ""
    right = src[pos + 1] if pos + 1 < len(src) else ""
    return is_cjk(left) or is_cjk(right)


def punct_dup(src: str, pos: int, ref: str = "") -> bool:
    """同一标点叠用。"""
    p = src[pos] if pos < len(src) else ""
    return bool(p) and p in HALF and (
        (pos > 0 and src[pos - 1] == p) or (pos + 1 < len(src) and src[pos + 1] == p))


def digit_time(src: str, pos: int, ref: str = "") -> bool:
    """时间高敏感点（严式）：紧邻量纲字的数字（∂{1,4}年…）或 ref 带数字。"""
    if any(c.isdigit() for c in ref):
        return True
    p = src[pos] if pos < len(src) else ""
    left = src[pos - 1:pos]
    right = src[pos + 1:pos + 2]
    return p in UNIT_CHARS and (left.isdigit() or right.isdigit())


def punct_convention_ok(src: str) -> bool:
    """文档级惯例探测：半角标点占比 ≥20% 即语料自身用半角，不触发 punct 族。"""
    h = sum(1 for c in src if c in HALF)
    f = sum(1 for c in src if c in FULL)
    return h < 0.2 * (h + f) if (h + f) else False


def place_hint(src: str, pos: int, ref: str = "") -> bool:
    """地名/机构后缀线索（粗，仅当与数字或「到/至/在」共现才算，压打扰）。"""
    p = src[pos] if pos < len(src) else ""
    hit = p in PLACE_CHARS or any(c in PLACE_CHARS for c in ref)
    if not hit:
        return False
    lo, hi = max(0, pos - 4), min(len(src), pos + 5)
    return any(c.isdigit() for c in src[lo:hi]) or any(k in src[lo:hi] for k in ("到", "至", "在", "位于"))

FAMILIES = {
    "punct_stream": punct_stream,
    "punct_dup": punct_dup,
    "digit_time": digit_time,
    "place_hint": place_hint,
}
