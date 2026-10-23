#!/usr/bin/env python3
"""领域冷启动数据增强：14 种语法错误增强（CEC GrammarErrorDat 组件移植）。

移植自 2024 CCL 冠军 ChineseErrorCorrector（TW-NLP/ChineseErrorCorrector，
Apache-2.0，`ChineseErrorCorrector/utils/dat.py` 的 `GrammarErrorDat`），
改为自包含脚本：无需 `pip install ChineseErrorCorrector`。

14 种增强（type → 中文 → 依赖）：
  1.  lack_word            缺字漏字      [LTP cws+pos]  随机名词/动词中删一个字符
  2.  wrong_word           错别字        [混淆字表]      一个字符替换为混淆字
  3.  lack_char            缺少标点      [纯]            删一个标点
  4.  wrong_char           错用标点      [纯]            一个标点替换为另一标点
  5.  unknow_sub           主语不明      [LTP dep]        删主语（SBV）
  6.  unknow_pred          谓语残缺      [LTP pos]        删谓语（v）
  7.  lack_obj             宾语残缺      [LTP dep]        删宾语（VOB/IOB/FOB）
  8.  lack_others          其他成分残缺  [LTP dep+pos]    删其他成分
  9.  red_fun              虚词多余      [LTP pos]        插入 的/地/所…的
  10. red_component        其他成分多余  [纯]             插入多余动词/宾语/定语/状语
  11. red_sub              主语多余      [LTP dep+pos]    复制主语到动词后
  12. wrong_sentence_order 语序不当     [纯]             交换两个分句顺序
  13. wrong_ver_obj        动宾搭配不当  [LTP dep+搭配表] 宾语或动词换为混淆搭配
  14. other_wrong          其他搭配不当  [LTP dep+搭配表] 主语或动词换为混淆搭配

依赖：
  - 标准库 + OpenCC（混淆字表 t2s 归一；缺失时降级为不做归一）
  - 9 种 LTP 依赖型增强需要 `pip install ltp`（首次运行自动下载模型）；
    无 LTP 时自动只启用 5 种纯类型
    （wrong_word / lack_char / wrong_char / red_component / wrong_sentence_order）。

混淆字表 / 搭配表：`cec_dat_data/`（vendor 自 CEC 仓库 `data/dat_data/`，
Apache-2.0；`token_set.txt` 为字符级混淆表，4 个 JSON 为搭配混淆表）。

用法：
  python3 augment_domain.py --input domain_clean.txt --output domain_aug.jsonl \
      --seed 42 --per-sentence 2 --types all --format pairs
  # --format sft --sft-prompt v3|v4 → CEC business_data 风格 conversations（LLM SFT）

输入格式：txt（每行一句干净文本）或 JSONL（取 text/source/sentence/content 字段）。
输出 pairs：{"source": 错误句, "target": 干净句, "error_type": 中文名, "error_label": CEC 标签}
输出 sft：  {"conversations": [{"from":"human","value":<prompt+source>},
                            {"from":"gpt","value":target}]}
"""
from __future__ import annotations

import argparse
import json
import os
import random
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
DATA_DIR = os.path.join(HERE, "cec_dat_data")


def set_seed(seed: int = 42) -> None:
    """可复现（对齐 CEC correct_tools.set_seed 习惯：默认 seed=42）。"""
    random.seed(seed)
    try:
        import numpy as np

        np.random.seed(seed)
    except ImportError:
        pass
    try:
        import torch

        torch.manual_seed(seed)
    except ImportError:
        pass


_ltp_instance = None


def get_ltp():
    """LTP 惰性加载（仅当用到 LTP 依赖型增强时才加载）。"""
    global _ltp_instance
    if _ltp_instance is None:
        try:
            from ltp import LTP
        except ImportError as exc:
            raise RuntimeError(
                "该增强类型需要 LTP（pip install ltp，首次运行自动下载模型）。"
                "无 LTP 时可用纯类型：wrong_word / lack_char / wrong_char / "
                "red_component / wrong_sentence_order"
            ) from exc
        _ltp_instance = LTP()
    return _ltp_instance


def ltp_pipeline(line: str, tasks) -> object:
    return get_ltp().pipeline([line], tasks=tasks)


def ltp_ok(cws, line: str) -> bool:
    """LTP 分词拼回后必须与原句一致，否则放弃该句（避免偏移错乱）。"""
    return "".join(cws) == line


def load_confuse_set() -> dict:
    """token_set.txt（`字\\t混淆字串`）→ {简体字: [混淆字]}，OpenCC t2s 归一。"""
    path = os.path.join(DATA_DIR, "token_set.txt")
    cc = None
    try:
        from opencc import OpenCC

        cc = OpenCC("t2s")
    except ImportError:
        print("[warn] OpenCC 不可用，混淆字表跳过 t2s 归一", file=sys.stderr)
    conv = (lambda s: cc.convert(s)) if cc else (lambda s: s)
    confuse_set: dict = {}
    with open(path, encoding="utf-8") as f:
        for line in f:
            parts = line.strip().split("\t")
            if len(parts) != 2:
                continue
            ch, words = parts
            ch = conv(ch)
            if ch in confuse_set:
                continue
            confuse_set[ch] = [w for w in words if w == conv(w)]
    return confuse_set


def load_confuse_json(name: str) -> dict:
    with open(os.path.join(DATA_DIR, name), encoding="utf-8") as f:
        raw = json.load(f)
    return {k: list(set(v)) for k, v in raw.items()}


PUNC_LIST = [
    "！", "？", "｡", "＂", "＃", "＄", "％", "＆", "＇", "（", "）", "＊", "＋",
    "，", "－", "／", "：", "；", "＜", "＝", "＞", "＠", "［", "＼", "］", "＾",
    "＿", "｀", "｛", "｜", "｝", "～", "｟", "｠", "｢", "｣", "､", "、", "〃",
    "》", "「", "」", "『", "』", "【", "】", "〔", "〕", "〖", "〗", "〘", "〙",
    "〚", "〛", "〜", "〝", "〞", "〟", "〰", "‘", "‛", "“", "”", "„", "‟",
    "。", "！", "？", "，", "；", "：", "、",
]
PUNC_SET = set(PUNC_LIST)

COMMON_PUNC = {
    "，": ["。", "、", "；", "？", "："],
    "。": ["，", "！", "？", "：", "；"],
    "？": ["。", "、", "；", "！", "，"],
    "：": ["。", "、", "；", "？", "，"],
    "、": ["。", "；", "，"],
}


def aug_wrong_word(line: str, confuse_set: dict):
    """错别字：一个字符替换为其混淆字。"""
    cands = [i for i, ch in enumerate(line) if confuse_set.get(ch)]
    if not cands:
        return None
    i = random.choice(cands)
    new_ch = random.choice(confuse_set[line[i]])
    if new_ch == line[i]:
        return None
    return {
        "source": line[:i] + new_ch + line[i + 1:],
        "target": line,
        "label": "char_error",
    }


def aug_lack_word(line: str):
    """缺字漏字：随机一个名词/动词中删一个字符。"""
    res = ltp_pipeline(line, ["cws", "pos"])
    cws, pos = res.cws[0], res.pos[0]
    if not ltp_ok(cws, line):
        return None
    targets = [i for i, p in enumerate(pos) if p in ("n", "v")]
    if not targets:
        return None
    i = targets[random.randint(0, len(targets) - 1)]
    word = cws[i]
    if len(word) < 2:
        return None
    j = random.randint(0, len(word) - 1)
    cws[i] = word[:j] + word[j + 1:]
    return {"source": "".join(cws), "target": line, "label": "char_append"}


def aug_lack_char(line: str):
    """缺少标点：删一个标点。"""
    idxs = [i for i, ch in enumerate(line) if ch in PUNC_SET]
    if not idxs:
        return None
    i = random.choice(idxs)
    return {
        "source": line[:i] + line[i + 1:],
        "target": line,
        "label": "char_punc_append",
    }


def aug_wrong_char(line: str):
    """错用标点：一个常用标点替换为另一常用标点。"""
    idxs = [i for i, ch in enumerate(line) if ch in COMMON_PUNC]
    if not idxs:
        return None
    i = random.choice(idxs)
    new_punc = random.choice(COMMON_PUNC[line[i]])
    if new_punc == line[i]:
        return None
    return {
        "source": line[:i] + new_punc + line[i + 1:],
        "target": line,
        "label": "char_punc_error",
    }


def aug_unknow_sub(line: str):
    """主语不明：删一个主语（SBV）。"""
    res = ltp_pipeline(line, ["cws", "dep"])
    cws, label = res.cws[0], res.dep[0]["label"]
    if not ltp_ok(cws, line):
        return None
    subs = [i for i, l in enumerate(label) if l == "SBV"]
    if not subs:
        return None
    cws[random.choice(subs)] = ""
    return {"source": "".join(cws), "target": line, "label": "miss_sub"}


def aug_unknow_pred(line: str):
    """谓语残缺：删一个谓语（v）。"""
    res = ltp_pipeline(line, ["cws", "pos"])
    cws, pos = res.cws[0], res.pos[0]
    if not ltp_ok(cws, line):
        return None
    vs = [i for i, p in enumerate(pos) if p == "v"]
    if not vs:
        return None
    cws[random.choice(vs)] = ""
    return {"source": "".join(cws), "target": line, "label": "miss_pre"}


def aug_lack_obj(line: str):
    """宾语残缺：删一个宾语（VOB/IOB/FOB）。"""
    res = ltp_pipeline(line, ["cws", "dep"])
    cws, label = res.cws[0], res.dep[0]["label"]
    if not ltp_ok(cws, line):
        return None
    objs = [i for i, l in enumerate(label) if l in ("VOB", "IOB", "FOB")]
    if not objs:
        return None
    cws[random.choice(objs)] = ""
    return {"source": "".join(cws), "target": line, "label": "miss_obj"}


def aug_lack_others(line: str):
    """其他成分残缺：删一个非主/谓/宾/WP 的成分。"""
    res = ltp_pipeline(line, ["cws", "dep", "pos"])
    cws, label, pos = res.cws[0], res.dep[0]["label"], res.pos[0]
    if not ltp_ok(cws, line):
        return None
    others = [
        i
        for i in range(len(cws))
        if label[i] not in ("VOB", "IOB", "FOB", "SBV", "WP") and pos[i] != "v"
    ]
    if not others:
        return None
    cws[random.choice(others)] = ""
    return {"source": "".join(cws), "target": line, "label": "miss_other"}


def aug_red_fun(line: str):
    """虚词多余：在 形/副/数 词后插 的/地，或在动词前插 所…的。"""
    res = ltp_pipeline(line, ["cws", "dep", "pos"])
    cws, pos = res.cws[0], res.pos[0]
    if not ltp_ok(cws, line):
        return None
    cands = []
    for j in range(len(cws)):
        if pos[j] in ("a", "d", "q") and j < len(cws) - 1:
            if cws[j + 1] not in ("的", "地"):
                cands.append(j)
        elif pos[j] == "v" and j > 0:
            if cws[j - 1] != "所":
                cands.append(j)
    if not cands:
        return None
    j = random.choice(cands)
    out = []
    for k in range(len(cws)):
        if k == j:
            if pos[k] in ("a", "q"):
                out.append(cws[k] + "的")
            elif pos[k] == "d":
                out.append(cws[k] + "地")
            else:  # v
                out.append("所" + cws[k] + "的")
        else:
            out.append(cws[k])
    return {"source": "".join(out), "target": line, "label": "redu_emp"}


RED_VERBS = [
    "看着", "尝试", "进行", "思考", "理解", "感知",
    "欣赏", "研究", "探索", "尊重", "忽略", "遵循",
    "挑战", "鼓励", "想象", "建议", "计划", "决定",
]
RED_OBJECTS = [
    "事情", "问题", "情况", "决策", "计划", "挑战",
    "机会", "梦想", "设想", "疑问", "方式", "策略",
    "可能性", "尝试", "方案", "部分", "结果", "选择",
]
RED_ADJECTIVES = [
    "美丽的", "快乐的", "悲伤的", "寂静的", "优秀的",
    "明智的", "勇敢的", "复杂的", "微妙的", "决定性的",
    "深份的", "清晰的", "困惑的", "先进的", "古代的",
    "完美的", "普通的", "特别的", "有趣的", "重要的",
]
RED_ADVERBS = [
    "显然", "突然", "可能", "实际上", "简单地",
    "最终", "同样地", "特别地", "逐渐", "轻易地",
    "积极地", "惊讶地", "明确地", "完全地", "立刻",
    "频繁地", "大约", "恰好", "轻松地", "当然",
]


def aug_red_component(line: str):
    """其他成分多余：插入多余动词/宾语/定语/状语。"""
    kind = random.choice(["verb", "object", "adjective", "adverb"])
    if kind == "verb":
        w = random.choice(RED_VERBS)
        pos = random.choice(["beginning", "middle", "end"])
        if pos == "beginning":
            source = w + "，" + line
        elif pos == "end":
            source = line + "，" + w
        else:
            parts = line.split("，")
            if len(parts) > 1:
                parts.insert(random.randint(0, len(parts) - 1), w)
                source = "，".join(parts)
            else:
                return None
    elif kind == "object":
        source = line.rstrip("。") + random.choice(RED_OBJECTS) + "。"
    elif kind == "adjective":
        source = random.choice(RED_ADJECTIVES) + line
    else:
        source = random.choice(RED_ADVERBS) + "，" + line
    return {"source": source, "target": line, "label": "redu_comp"}


def aug_red_sub(line: str):
    """主语多余：找到谓语的主语，复制一份到该谓语之后。"""
    res = ltp_pipeline(line, ["cws", "dep", "pos"])
    cws, pos, head, label = (
        res.cws[0],
        res.pos[0],
        res.dep[0]["head"],
        res.dep[0]["label"],
    )
    if not ltp_ok(cws, line):
        return None
    cands = []
    for j in range(len(cws)):
        if pos[j] == "v" and (label[j] == "HED" or label[j] == "COO"):
            temp_j = j - 1
            while temp_j > 0 and label[temp_j] == "ADV":
                temp_j -= 1
            if label[temp_j] != "SBV":
                cands.append(j)
    if not cands:
        return None
    j = random.choice(cands)
    # 沿 COO 链找到顶层谓语，再找其主语（与 CEC 原实现一致）
    temp_index = head[j] - 1
    guard = 0
    while label[temp_index] == "COO" and guard < 32:
        temp_index = head[temp_index] - 1
        guard += 1
    add_sub = ""
    for m in range(len(cws)):
        if label[m] == "SBV" and head[m] == temp_index + 1:
            add_sub = cws[m]
            break
    if not add_sub:
        return None
    out = []
    for k in range(len(cws)):
        out.append(cws[k])
        if k == j:
            out.append(add_sub)
    return {"source": "".join(out), "target": line, "label": "redu_sub"}


def aug_wrong_sentence_order(line: str):
    """语序不当：交换两个分句的顺序。"""
    parts = line.split("，")
    if len(parts) < 2:
        return None
    a, b = random.sample(range(len(parts)), 2)
    parts[a], parts[b] = parts[b], parts[a]
    return {
        "source": "，".join(parts),
        "target": line,
        "label": "word_order",
    }


def aug_wrong_ver_obj(line: str, con_v_obj: dict, con_obj_v: dict):
    """动宾搭配不当：宾语换为该动词的混淆宾语，或动词换为该宾语的混淆动词。"""
    res = ltp_pipeline(line, ["cws", "dep"])
    cws, head, label = res.cws[0], res.dep[0]["head"], res.dep[0]["label"]
    if not ltp_ok(cws, line):
        return None
    objs = [i for i, l in enumerate(label) if l in ("VOB", "IOB", "FOB")]
    if not objs:
        return None
    i = random.choice(objs)
    v, obj = cws[head[i] - 1], cws[i]
    new_cws = list(cws)
    if random.random() < 0.5:
        cands = [w for w in con_v_obj.get(v, []) if w != obj]
        if not cands:
            return None
        new_cws[i] = random.choice(cands)
    else:
        cands = [w for w in con_obj_v.get(obj, []) if w != v]
        if not cands:
            return None
        new_cws[head[i] - 1] = random.choice(cands)
    return {"source": "".join(new_cws), "target": line, "label": "coll_vobj"}


def aug_other_wrong(line: str, con_sub_v: dict, con_v_sub: dict):
    """其他搭配不当：主语换为该动词的混淆主语，或动词换为该主语的混淆动词。"""
    res = ltp_pipeline(line, ["cws", "dep"])
    cws, head, label = res.cws[0], res.dep[0]["head"], res.dep[0]["label"]
    if not ltp_ok(cws, line):
        return None
    subs = [i for i, l in enumerate(label) if l == "SBV"]
    if not subs:
        return None
    i = random.choice(subs)
    sub, v = cws[i], cws[head[i] - 1]
    new_cws = list(cws)
    if random.random() < 0.5:
        cands = [w for w in con_v_sub.get(v, []) if w != sub]
        if not cands:
            return None
        new_cws[i] = random.choice(cands)
    else:
        cands = [w for w in con_sub_v.get(sub, []) if w != v]
        if not cands:
            return None
        new_cws[head[i] - 1] = random.choice(cands)
    return {"source": "".join(new_cws), "target": line, "label": "coll_other"}


# type 名 → (中文名, 是否依赖 LTP)
TYPES: dict = {
    "lack_word": ("缺字漏字", True),
    "wrong_word": ("错别字", False),
    "lack_char": ("缺少标点", False),
    "wrong_char": ("错用标点", False),
    "unknow_sub": ("主语不明", True),
    "unknow_pred": ("谓语残缺", True),
    "lack_obj": ("宾语残缺", True),
    "lack_others": ("其他成分残缺", True),
    "red_fun": ("虚词多余", True),
    "red_component": ("其他成分多余", False),
    "red_sub": ("主语多余", True),
    "wrong_sentence_order": ("语序不当", False),
    "wrong_ver_obj": ("动宾搭配不当", True),
    "other_wrong": ("其他搭配不当", True),
}

SFT_PROMPTS = {
    "v3": "你是一个文本纠错专家，纠正输入句子中的语法错误，并输出正确的句子，输入句子为：",
    "v4": (
        "假如你是一名专业的纠错专家，请分析输入句子的语法错误类型和修改原因，"
        "并只输出纠正后的语句，错误类型如下：错别字、词语搭配错误、词性错误、"
        "语序错误、成分残缺、成分赘余、关联词使用错误、指代不明、语义逻辑不通、无误。"
    ),
}


def load_sentences(path: str, fmt: str) -> list:
    """txt：每行一句；jsonl：取 text/source/sentence/content 字段。"""
    with open(path, encoding="utf-8") as f:
        lines = [ln.rstrip("\n") for ln in f]
    if fmt == "auto":
        first = next((ln for ln in lines if ln.strip()), "")
        fmt = "jsonl" if first.lstrip().startswith("{") else "txt"
    out = []
    for ln in lines:
        s = ln.strip()
        if not s:
            continue
        if fmt == "jsonl":
            try:
                obj = json.loads(s)
            except json.JSONDecodeError:
                continue
            s = next(
                (obj[k] for k in ("text", "source", "sentence", "content") if k in obj),
                None,
            )
            if not isinstance(s, str) or not s.strip():
                continue
        out.append(s.strip())
    return out


def make_augmenter(tname: str, ctx: dict):
    """按类型名构造增强器闭包（搭配表/混淆字表经 ctx 注入）。"""
    if tname == "wrong_word":
        return lambda line: aug_wrong_word(line, ctx["confuse_set"])
    if tname == "wrong_ver_obj":
        return lambda line: aug_wrong_ver_obj(line, ctx["con_v_obj"], ctx["con_obj_v"])
    if tname == "other_wrong":
        return lambda line: aug_other_wrong(line, ctx["con_sub_v"], ctx["con_v_sub"])
    return getattr(sys.modules[__name__], "aug_" + tname)


def main() -> None:
    ap = argparse.ArgumentParser(
        description="14 种语法错误增强（CEC GrammarErrorDat 移植），"
        "领域冷启动合成 (source, target) 对"
    )
    ap.add_argument("--input", required=True, help="干净文本：txt（每行一句）或 JSONL")
    ap.add_argument("--input-format", choices=["auto", "txt", "jsonl"], default="auto")
    ap.add_argument("--output", required=True, help="输出 JSONL 路径")
    ap.add_argument("--seed", type=int, default=42)
    ap.add_argument("--per-sentence", type=int, default=1,
                   help="每句生成的增强变体数（默认 1）")
    ap.add_argument("--types", default="all",
                   help="逗号分隔的类型名，或 all（默认）；可用类型见模块 docstring")
    ap.add_argument("--format", choices=["pairs", "sft"], default="pairs",
                   help="pairs: {source,target,error_type,error_label}；"
                       "sft: CEC business_data 风格 conversations")
    ap.add_argument("--sft-prompt", choices=["v3", "v4"], default="v3",
                    help="sft 格式使用的 instruction（CEC 官方 prompt 原文）")
    ap.add_argument("--report", default=None, help="可选：统计写入该 JSON 文件")
    args = ap.parse_args()

    set_seed(args.seed)

    if args.types == "all":
        wanted = list(TYPES)
    else:
        wanted = [t.strip() for t in args.types.split(",") if t.strip()]
        unknown = [t for t in wanted if t not in TYPES]
        if unknown:
            ap.error(f"未知类型 {unknown}；可用：{', '.join(TYPES)}")

    # LTP 可用性探测：不可用时剔除 LTP 依赖型
    try:
        import ltp  # noqa: F401
    except ImportError:
        ltp_available = False
    else:
        ltp_available = True
    available, skipped = [], []
    for t in wanted:
        if TYPES[t][1] and not ltp_available:
            skipped.append(t)
        else:
            available.append(t)
    if skipped:
        print(
            f"[warn] LTP 不可用（pip install ltp 可启用），"
            f"跳过 {len(skipped)} 种 LTP 依赖型：",
            ", ".join(skipped), file=sys.stderr,
        )
    if not available:
        ap.error("没有可用增强类型（LTP 缺失且所选均为 LTP 依赖型）")

    # 数据表（仅当用到相应类型时加载）
    ctx: dict = {"confuse_set": None}
    if "wrong_word" in available:
        ctx["confuse_set"] = load_confuse_set()
    if "wrong_ver_obj" in available:
        ctx["con_v_obj"] = load_confuse_json("confuse_v_obj.json")
        ctx["con_obj_v"] = load_confuse_json("confuse_obj_v.json")
    if "other_wrong" in available:
        ctx["con_sub_v"] = load_confuse_json("confuse_sub_v.json")
        ctx["con_v_sub"] = load_confuse_json("confuse_v_sub.json")

    sentences = load_sentences(args.input, args.input_format)
    if not sentences:
        ap.error(f"输入无有效句子：{args.input}")

    augmenters = {t: make_augmenter(t, ctx) for t in available}
    stats = {t: {"attempted": 0, "ok": 0, "not_applicable": 0} for t in available}

    n_out = 0
    with open(args.output, "w", encoding="utf-8") as f:
        for sent in sentences:
            produced = 0
            attempts = 0
            max_attempts = max(8, args.per_sentence * 6)
            while produced < args.per_sentence and attempts < max_attempts:
                attempts += 1
                t = random.choice(available)
                rec = augmenters[t](sent)
                stats[t]["attempted"] += 1
                if rec is None or rec["source"] == rec["target"]:
                    stats[t]["not_applicable"] += 1
                    continue
                stats[t]["ok"] += 1
                if args.format == "pairs":
                    f.write(json.dumps(
                        {
                            "source": rec["source"],
                            "target": rec["target"],
                            "error_type": TYPES[t][0],
                            "error_label": rec["label"],
                        },
                        ensure_ascii=False,
                    ) + "\n")
                else:
                    f.write(json.dumps(
                        {
                            "conversations": [
                                {
                                    "from": "human",
                                    "value": SFT_PROMPTS[args.sft_prompt] + rec["source"],
                                },
                                {"from": "gpt", "value": rec["target"]},
                            ]
                        },
                        ensure_ascii=False,
                    ) + "\n")
                n_out += 1
                produced += 1

    report = {
        "input": args.input,
        "sentences_in": len(sentences),
        "pairs_out": n_out,
        "seed": args.seed,
        "per_sentence": args.per_sentence,
        "format": args.format,
        "types": {t: stats[t] for t in available},
        "skipped_no_ltp": skipped,
    }
    print(json.dumps(report, ensure_ascii=False, indent=2))
    if args.report:
        with open(args.report, "w", encoding="utf-8") as f:
            json.dump(report, f, ensure_ascii=False, indent=2)


if __name__ == "__main__":
    main()

