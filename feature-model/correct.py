#!/usr/bin/env python3
"""L3 特征模型的模式化 span 定位 + 机械修正（mock 模式与真实模型模式共用）。

每个 (pattern, error_type) 命中后：
- span = 匹配区间（字符偏移，与 Rust 侧 ErrorItem.span 一致）
- correction = 机械修正结果（可机械修正的类型才给出；None = 仅报告，无修正）
- message = 人类可读说明

设计原则：宁缺毋滥——模式必须足够具体（避免误报污染检测结果）；
无法机械修正的类型只报告（correction=None），由前端展示 message。
"""

from __future__ import annotations

import json
import os
import re
from dataclasses import dataclass
from typing import Optional


@dataclass
class PatternHit:
    """一次模式命中（span 级错误候选）"""

    error_type: str
    span: tuple[int, int]
    original: str
    correction: Optional[str]
    message: str
    # advisory=True：低置信模式（从学习者语料挖掘、母语正常文本可误伤）。
    # 服务端不据此直接定案，仅作为灰区信号把句子交给 LLM 裁定。
    advisory: bool = False


# ─── 中文模式 ───

# 虚词 的/地/得 混用（MuCGEC particle 谱系）
_ZH_PARTICLE = re.compile(r"的(了|着)|了(的|地)|的地(得|的)")
# 把字句误用
_ZH_BA = re.compile(r"把(他|她|它|你|我)(们)?(是|有|在|很|太|最)")
# 时间状语语序（动词后遗留时间词）
_ZH_TIME = re.compile(r"([一-龥]{2,12})(昨天|今天|明天|刚才|昨晚|下午|早上)")
# 连续重复字
_ZH_REPEAT = re.compile(r"([一-龥])\1\1")
# 副词+的 搭配
_ZH_ADV = re.compile(r"(非常|很|太|最)的(是|有|在|好|快|慢|大|小|多|少)")

# 时间词表（修正时用于重排）
_ZH_TIME_WORDS = ("昨天", "今天", "明天", "刚才", "昨晚", "下午", "早上")


def _zh_particle_fix(m: re.Match, s: str) -> Optional[str]:
    """的+了/着 → 去掉的；了+的/地 → 去掉的/地；的地+得/的 → 的地→得"""
    text = s[m.start() : m.end()]
    if text.startswith("的地"):
        return text.replace("的地", "得", 1)
    if text[0] == "的":
        return text[1:]
    return None


def _zh_time_fix(m: re.Match, s: str) -> Optional[str]:
    """「…昨天」→「昨天…」：把句尾时间词移到片段开头；
    若片段内已含该时间词（重复）→ 只删尾部时间词"""
    text = s[m.start() : m.end()]
    head, time_word = m.group(1), m.group(2)
    if time_word in head:
        return head
    return f"{time_word}{head}"


def _zh_repeat_fix(m: re.Match, s: str) -> Optional[str]:
    """三连重复字 → 保留一个"""
    text = s[m.start() : m.end()]
    return m.group(1)


def _zh_adv_fix(m: re.Match, s: str) -> Optional[str]:
    """「(非常|很|太|最)的X」→ 去掉 的"""
    text = s[m.start() : m.end()]
    adv, rest = m.group(1), m.group(2)
    return adv + rest


# ─── 混淆字（繁→简 100% 安全 + 上下文约束同音/形近字） ───

# 繁→简映射（从 lexicons/zh_gec_lexicon.json 加载；note 含「繁体」的条目
# 在简体文本中无合法用法，100% 安全）。此处不内置任何非繁体的单字替换：
# 「吾→五」这类文言合法字必须由词表带条件条目管理，不做无条件定案。
_ZH_TRAD_TO_SIMP: dict[str, str] = {}

# 上下文约束混淆字（(regex, 修正字, 说明, advisory)）——仅当后随字匹配时才触发。
# 分两档（review 2026-10-18：从 MuCGEC gold 挖掘的 pattern 对母语正常文本
# 大面积误伤，如「我在商场」「第二次」「工作了很多年」「没发现」）：
# - advisory=False（定案档）：错字组合在正常中文中几乎不出现，可机械修正；
# - advisory=True（灰区档）：存在合法用法（词内误击/句法歧义），只作为
#   「该句可疑」的信号交给 LLM 裁定，不直接产出修正。
_ZH_CONTEXT_CONFUSABLES: list[tuple[re.Pattern, str, str, bool]] = [
    # ── 定案档：组合本身几乎必错 ──
    (re.compile(r"名(?=[种式样])(?!次)"), "各", "名→各（各种/各式/各样）", False),
    (re.compile(r"至令"), "至今", "至令→至今", False),
    (re.compile(r"(?<!命)令(?=[天])"), "今", "令→今（今天；排除命令天）", False),
    (re.compile(r"坏(?=[境])"), "环", "坏→环（坏境→环境）", False),
    (re.compile(r"岐(?!山|黄)"), "歧", "岐→歧（排除岐山/岐黄）", False),
    (re.compile(r"根(?=[我爸儿])"), "跟", "根→跟（根我/根爸/根儿）", False),
    # ── 灰区档（advisory）：存在合法用法，仅提示 LLM ──
    (re.compile(r"的(?=[积有客步茫量])"), "地", "的→地（修饰动词/补语）", True),
    (re.compile(r"(?<![工操制合])作(?=[饭了关])"), "做", "作→做（做饭/做了）", True),
    (re.compile(r"之(?=[念数关事])"), "的", "之→的（之念/之数量）", True),
    (re.compile(r"西(?=[个养])"), "四", "西→四（四个）", True),
    (re.compile(r"态(?=[发上])"), "体", "态→体（体发/体上）", True),
    (re.compile(r"(?<![一])箱(?=[要一])"), "想", "箱→想（想要）", True),
    (re.compile(r"得(?=[说来话])"), "的", "得→的（的说/的话）", True),
    (re.compile(r"跟(?=[传原])"), "和", "跟→和（和传统）", True),
    (re.compile(r"多(?=[来])"), "高", "多→高（高来）", True),
    (re.compile(r"理(?=[就舒])"), "里", "理→里（里就/里舒）", True),
    (re.compile(r"小(?=[抽了])"), "少", "小→少（少抽/少了）", True),
    (re.compile(r"在(?=[商挪])"), "去", "在→去（去商/去挪）", True),
    (re.compile(r"风头"), "广告", "风头→广告", True),
    (re.compile(r"的(?=[喜欢感动分析学习研究讨论发展])"), "地", "的→地（修饰动词）", True),
    (re.compile(r"个(?=[个方])"), "各", "个→各（各个/各方）", True),
    (re.compile(r"(?<![第十])二(?=[个次])"), "两", "二→两（两个/两次；排除第二/十二）", True),
    (re.compile(r"没(?=[发成熟蓝听])"), "不", "没→不（否定状态/形容词）", True),
    (re.compile(r"对(?=[别人社周])"), "给", "对→给（给予间接宾语）", True),
]


def context_confusable_count() -> int:
    """上下文约束混淆 pattern 条数（评测脚本展示用，避免访问私有变量）"""
    return len(_ZH_CONTEXT_CONFUSABLES)


def load_confusables(lexicon_path: str) -> int:
    """从词表 JSON 加载混淆字（繁→简对），返回加载数量。
    词表格式: {"confusables": [["時","时","..."], ...]}
    仅加载 100% 安全的繁→简对（wrong 字在简体文本中无合法用法）。"""
    global _ZH_TRAD_TO_SIMP
    try:
        with open(lexicon_path, encoding="utf-8") as f:
            data = json.load(f)
    except (OSError, json.JSONDecodeError):
        return 0
    conf = data.get("confusables", [])
    new_count = 0
    for entry in conf:
        # 兼容两种格式：["wrong", "right", "note"] 或 {"wrong":..., "right":...}
        if isinstance(entry, dict):
            # dict 格式带 condition → 需要上下文约束，不适合无条件替换
            continue
        if not isinstance(entry, (list, tuple)) or len(entry) < 2:
            continue
        wrong, right = entry[0], entry[1]
        note = entry[2] if len(entry) >= 3 else ""
        # 仅收真正的繁→简对（note 含「繁体字」）：
        # 这些字在简体文本中无合法用法，100% 安全
        if "繁体" not in note:
            continue
        # 仅收单字对（多字词对不适合字符级替换）
        if len(wrong) == 1 and len(right) == 1 and wrong not in _ZH_TRAD_TO_SIMP:
            _ZH_TRAD_TO_SIMP[wrong] = right
            new_count += 1
    return new_count


def find_zh_confusable_hits(text: str) -> list[PatternHit]:
    """查找混淆字命中：繁→简（无条件）+ 上下文约束同音/形近字。"""
    hits: list[PatternHit] = []
    # 繁→简（100% 安全，无需上下文）
    for i, ch in enumerate(text):
        if ch in _ZH_TRAD_TO_SIMP:
            fix = _ZH_TRAD_TO_SIMP[ch]
            hits.append(
                PatternHit(
                    "collocation",
                    (i, i + 1),
                    ch,
                    fix,
                    f"繁简: {ch}→{fix}",
                )
            )
    # 上下文约束混淆字（定案档 / 灰区档见 _ZH_CONTEXT_CONFUSABLES 注释）
    for pat, fix, msg, advisory in _ZH_CONTEXT_CONFUSABLES:
        for m in pat.finditer(text):
            hits.append(
                PatternHit(
                    "collocation",
                    (m.start(), m.end()),
                    m.group(0),
                    fix,
                    msg,
                    advisory=advisory,
                )
            )
    return hits


def dedupe_overlapping_hits(hits: list[PatternHit]) -> list[PatternHit]:
    """重叠命中去重：最长命中优先，后到的重叠命中丢弃。

    例：「至令天气」同时命中「至令→至今 (0,2)」与「令→今 (1,2)」——
    两条相互冲突的修正进入 merge 会产生矛盾结果，只保留最长的一条。"""
    ordered = sorted(hits, key=lambda h: (h.span[0], -(h.span[1] - h.span[0])))
    kept: list[PatternHit] = []
    last_end = -1
    for h in ordered:
        if h.span[0] >= last_end:
            kept.append(h)
            last_end = h.span[1]
    return kept


def find_zh_hits(text: str) -> list[PatternHit]:
    """在文本中查找所有中文模式命中（按 span 起点排序）"""
    hits: list[PatternHit] = []
    for m in _ZH_PARTICLE.finditer(text):
        fix = _zh_particle_fix(m, text)
        hits.append(
            PatternHit(
                "collocation",
                (m.start(), m.end()),
                m.group(0),
                fix,
                "虚词 的/地/得 混用",
            )
        )
    for m in _ZH_BA.finditer(text):
        hits.append(
            PatternHit(
                "word_order",
                (m.start(), m.end()),
                m.group(0),
                None,
                "把字句结构误用（把+人称+是/有/在…）",
            )
        )
    for m in _ZH_TIME.finditer(text):
        if m.group(2) not in _ZH_TIME_WORDS:
            continue
        fix = _zh_time_fix(m, text)
        hits.append(
            PatternHit(
                "word_order",
                (m.start(), m.end()),
                m.group(0),
                fix,
                "时间状语语序（应置于动词前）",
            )
        )
    for m in _ZH_REPEAT.finditer(text):
        hits.append(
            PatternHit(
                "word_order",
                (m.start(), m.end()),
                m.group(0),
                _zh_repeat_fix(m, text),
                "连续重复字",
            )
        )
    for m in _ZH_ADV.finditer(text):
        hits.append(
            PatternHit(
                "collocation",
                (m.start(), m.end()),
                m.group(0),
                _zh_adv_fix(m, text),
                "副词+的 搭配",
            )
        )
    # 混淆字（繁→简 + 上下文约束的同音/形近字）
    hits.extend(find_zh_confusable_hits(text))
    # 重叠命中去重（最长优先）：避免同一 span 产生相互冲突的修正
    return dedupe_overlapping_hits(hits)

# ─── 英文模式 ───

# 常见拼写错误 → 修正
_EN_SPELLING = {
    "teh": "the", "recieve": "receive", "occured": "occurred",
    "writen": "written", "begining": "beginning", "comming": "coming",
    "wich": "which", "becuase": "because", "tommorow": "tomorrow",
    "untill": "until", "seperate": "separate", "seperated": "separated",
    "seperates": "separates", "seperating": "separating",
}
_EN_SPELLING_RE = re.compile(
    r"\b(" + "|".join(re.escape(w) for w in sorted(_EN_SPELLING, key=len, reverse=True)) + r")\b",
    re.I,
)

# a/an 冠词搭配
_EN_AN_AFTER_A = re.compile(
    r"\ba\s+(apple|orange|elephant|hour|old|once|one|eye|honest|unique|uniform|useful)\b", re.I
)
_EN_A_AFTER_AN = re.compile(
    r"\ban\s+(cat|dog|car|bus|book|boy|big|good|new|red|white|black|tall|small|happy|smart|young|plan|problem|project|city|country|day|fact|game|group|hand|idea|job|king|land|man|name|note|room|shop|story|time|tree|unit|word)\b",
    re.I,
)

# 时态：现在时动词 + 过去时间状语
_EN_PAST_WORDS = {
    "go": "went", "come": "came", "see": "saw", "make": "made",
    "take": "took", "give": "gave", "run": "ran", "find": "found",
    "get": "got", "tell": "told", "think": "thought", "know": "knew",
    "feel": "felt", "leave": "left", "hold": "held", "read": "read",
    "say": "said", "bring": "brought", "write": "wrote", "speak": "spoke",
}
_EN_TIME = r"(?:yesterday|last\s+(?:night|week|month|year)|\d+\s+(?:minutes?|hours?|days?|weeks?|months?|years?)\s+ago)"
_EN_TENSE = re.compile(
    r"\b(I|he|she|it|we|you|they)\s+(" + "|".join(_EN_PAST_WORDS) + r")\b.*\b" + _EN_TIME + r"\b",
    re.I,
)
# 助动词后误用过去式
_EN_AUX_PAST = (
    "went|came|saw|made|took|gave|ran|found|got|told|thought|knew|felt|left|held|wrote|spoke|said|brought"
)
_EN_AUX = re.compile(
    r"\b(was|were|did|didn't|will|would|can|could|should)\s+(" + _EN_AUX_PAST + r")\b", re.I
)
_EN_AUX_BASE = {
    "went": "go", "came": "come", "saw": "see", "made": "make",
    "took": "take", "gave": "give", "ran": "run", "found": "find",
    "got": "get", "told": "tell", "thought": "think", "knew": "know",
    "felt": "feel", "left": "leave", "held": "hold", "wrote": "write",
    "spoke": "speak", "said": "say", "brought": "bring",
}
# 缺介词：go/come/get + 地点
_EN_PREP = re.compile(
    r"\b(go|come|get)\s+(school|home|work|bed|church|college|university|hospital|town|office|station|airport)\b",
    re.I,
)


def _case_adjust(template: str, original: str, replacement: str) -> str:
    """替换后按原词首字母大小写调整（大写开头则结果也大写开头）"""
    if original and original[0].isupper():
        return replacement[0].upper() + replacement[1:]
    return replacement


def find_en_hits(text: str) -> list[PatternHit]:
    """在文本中查找所有英文模式命中（按 span 起点排序）"""
    hits: list[PatternHit] = []
    for m in _EN_SPELLING_RE.finditer(text):
        word = m.group(1).lower()
        fix = _EN_SPELLING.get(word)
        if fix:
            hits.append(
                PatternHit(
                    "collocation",
                    (m.start(1), m.end(1)),
                    m.group(1),
                    _case_adjust(fix, m.group(1), fix),
                    "拼写错误",
                )
            )
    for m in _EN_AN_AFTER_A.finditer(text):
        hits.append(
            PatternHit(
                "collocation",
                (m.start(), m.end()),
                m.group(0),
                "an " + m.group(1),
                "a/an 冠词搭配（元音开头词用 an）",
            )
        )
    for m in _EN_A_AFTER_AN.finditer(text):
        hits.append(
            PatternHit(
                "collocation",
                (m.start(), m.end()),
                m.group(0),
                "a " + m.group(1),
                "a/an 冠词搭配（辅音开头词用 a）",
            )
        )
    for m in _EN_TENSE.finditer(text):
        verb = m.group(2)
        past = _EN_PAST_WORDS.get(verb.lower())
        fix = None
        if past:
            fix = m.group(0).replace(verb, _case_adjust(past, verb, past), 1)
        hits.append(
            PatternHit(
                "word_order",
                (m.start(), m.end()),
                m.group(0),
                fix,
                "时态不一致（现在时动词 + 过去时间状语）",
            )
        )
    for m in _EN_AUX.finditer(text):
        past = m.group(2)
        base = _EN_AUX_BASE.get(past.lower())
        fix = None
        if base:
            fix = m.group(0).replace(past, _case_adjust(base, past, base), 1)
        hits.append(
            PatternHit(
                "word_order",
                (m.start(), m.end()),
                m.group(0),
                fix,
                "助动词后误用过去式（应用原形）",
            )
        )
    for m in _EN_PREP.finditer(text):
        hits.append(
            PatternHit(
                "missing",
                (m.start(), m.end()),
                m.group(0),
                m.group(1) + " to " + m.group(2),
                "缺少介词 to",
            )
        )
    hits.sort(key=lambda h: h.span[0])
    return hits


def find_hits(lang: str, text: str) -> list[PatternHit]:
    """模式化定位入口（按语言分派）：返回 span 级错误候选（含机械修正）"""
    if lang == "zh":
        return find_zh_hits(text)
# ─── zh n-gram OOV 与逐句启发式概率（server.py / eval_l3_funnel.py 共用）───
#
# 方案 B（2026-10-18）：3-gram 表从 3.4 万扩到 100 万（build_ngram_table.py，
# CEC 平行对 ref 干净侧 + 去污染 + top-N 剪枝），并引入：
# - hard/soft OOV 区分：OOV trigram 的两个组成 bigram 都在高频 bigram 表
#   （bigram_table_zh.json，与 n-gram 同规则抽取）→ "常见成分的新组合"（soft，
#   如母语者正常新搭配、错别字型替换），否则 hard（表面异常，强信号）。
# - 短句平滑：分母 +2，防 3-4 字句单个 OOV 拉满比例。
#
# 校准事实（2026-10-18 实测，扩表后）：句级 OOV 是**弱检测器**——
# CSCD AUC(hard_ratio)=0.59、NaCGEC AUC(hard)=0.59、MuCGEC AUC(hard)=0.67；
# 旧表上 MuCGEC "OOV≈0.8 vs clean≈0" 的高区分度是小表假象（一切皆 OOV）。
# 因此启发式定位为「表面异常门」：hard==0 且 soft 不密集 → 放行；
# 不产生 OOV 的错误（语序/残缺/抽象搭配）由 pattern 层与决策模型（C）兜底。

_OOV_SOFT_WEIGHT = 0.4   # soft OOV 计入加权比例的权重
_OOV_SMOOTH = 2.0        # 短句平滑（分母 +2）


def load_zh_ngram_tables(ngram_path: str, bigram_path: str) -> tuple[dict, frozenset]:
    """加载 3-gram 频率表与高频 bigram 表；缺文件时返回空表（启发式自动降级）。"""
    table: dict = {}
    if os.path.isfile(ngram_path):
        with open(ngram_path, encoding="utf-8") as f:
            table = json.load(f)
    bg: frozenset = frozenset()
    if os.path.isfile(bigram_path):
        with open(bigram_path, encoding="utf-8") as f:
            bg = frozenset(json.load(f))
    return table, bg


def zh_oov_counts(sentence: str, table: dict, bg: frozenset) -> tuple[int, int, int]:
    """返回 (hard, soft, total)：hard/soft OOV 计数与 trigram 总数。
    抽取规则与 build_ngram_table.py 一致：去非中文、句内滑窗。"""
    zh = [c for c in sentence if "\u4e00" <= c <= "\u9fff"]
    if len(zh) < 3:
        return 0, 0, 0
    hard = soft = 0
    total = len(zh) - 2
    for i in range(total):
        tri = zh[i] + zh[i + 1] + zh[i + 2]
        if tri in table:
            continue
        if bg and (zh[i] + zh[i + 1]) in bg and (zh[i + 1] + zh[i + 2]) in bg:
            soft += 1
        else:
            hard += 1
    return hard, soft, total


def zh_ngram_oov_ratio(sentence: str, table: dict, bg: frozenset) -> float:
    """加权 OOV 比例（hard=1.0, soft=_OOV_SOFT_WEIGHT），短句平滑。[0,1)。"""
    hard, soft, total = zh_oov_counts(sentence, table, bg)
    if total == 0:
        return 0.0
    return (hard + _OOV_SOFT_WEIGHT * soft) / (total + _OOV_SMOOTH)


def zh_heuristic_prob(sentence: str, table: dict, bg: frozenset) -> float:
    """逐句启发式错误概率（范围 [0.05, 0.90]）。

    主信号：hard/soft OOV 分档（见上方校准事实）。
    - hard==0 且 soft 不密集 → 0.10（放行）
    - hard==0 但 soft 密集（soft_ratio≥0.35，长句大面积新组合）→ 0.35（灰区）
    - hard≥1：按 hard_ratio 分档 0.35 / 0.50 / 0.65
    辅助信号（叠加）：把字结构 +0.10；虚词（的地得）≥4 个 +0.05。
    无表时降级为粗糙启发式（句长/子句数）。
    """
    if table:
        hard, soft, total = zh_oov_counts(sentence, table, bg)
        if total == 0:
            prob = 0.10
        elif hard == 0:
            prob = 0.35 if soft / total >= 0.35 else 0.10
        else:
            hr = hard / total
            if hr < 0.06:
                prob = 0.35
            elif hr < 0.15:
                prob = 0.50
            else:
                prob = 0.65
    else:
        # 无 n-gram 表时回退到粗糙启发式
        prob = 0.10
        n = len(sentence)
        if n > 40: prob += 0.10
        if n > 60: prob += 0.10
        if n > 80: prob += 0.10
        clauses = sentence.count("，") + sentence.count("、") + sentence.count(",")
        if clauses >= 3: prob += 0.10
        if clauses >= 5: prob += 0.10

    # ── 辅助信号（叠加） ──
    if "把" in sentence and any(p in sentence for p in "他她它你我"):
        prob += 0.10
    particle_count = sentence.count("的") + sentence.count("地") + sentence.count("得")
    if particle_count >= 4:
        prob += 0.05

    return min(prob, 0.90)

    return find_en_hits(text)