#!/usr/bin/env python3
"""CEC 混淆字集 → zh_gec_lexicon.json confusables 策展脚本（幂等）。

来源：TW-NLP/ChineseErrorCorrector（ACL 2026）仓库 data/dat_data/
- token_set.txt / new_cofuse_set.txt：`键 \\t 混淆字列表`（TSV，列表为单字串）。
  方向语义（见 CEC dat.py wrong_word）：键 = 真实（正确）句中的字（常见字），
  列表 = 被替换进去的字 → 候选方向 wrong=列表字, right=键。

策展原则（与 curate_lexicon_zh.py 的人工策展一致，宁缺毋滥）：
1. 方向判定：wrong = 非键字，right = 键字。
   双方都是键（都常见）→ 方向不可判 → 剔除（防「常见正确字对」误报）；
   双方都不是键 → 同样剔除。
2. 分层：right 同时是两份文件的键（tier 2，最常见）> 仅一份（tier 1）。
   --apply 默认只合并 tier 2，最保守。
3. 主读音校验：两字主读音（音节+声调）须完全一致（pypinyin；
   全异读集含古音/方言音会误判，不用）；
   繁简过滤：OpenCC t2s——wrong 为繁体/异体字时，right 必须是它的
   简体形（如 於→于）；映射到同音他字（於→余）或用字目标为繁体
   （俠→峽）的对剔除。
4. 部首 / 非汉字剔除。
5. 去重：与词表现有 confusables 及候选自身去重。
6. 输出 [wrong, right, note] 三元组追加进词表（合并前自动备份）。

用法：
    .venv/bin/python curate_cec_confusables.py --src <dat_data 目录>
        # dry-run：打印统计 / 分层样本 / 被剔除原因，不写词表
    .venv/bin/python curate_cec_confusables.py --src <dat_data 目录> --apply
        # 合并 tier 2 进 ../lexicons/zh_gec_lexicon.json（自动备份）
    候选全量清单另写 cec_confusable_candidates.tsv 供人工复核。
"""

import argparse
import json
import re
import shutil
import sys
from datetime import datetime
from pathlib import Path

try:
    from pypinyin import pinyin, Style
except ImportError:  # pragma: no cover
    sys.exit("缺少 pypinyin：请先 .venv/bin/pip install pypinyin")

try:
    import opencc

    T2S = opencc.OpenCC("t2s.json")
except ImportError:  # pragma: no cover
    T2S = None
    print("警告：缺少 opencc，跳过「繁体→繁体」过滤（.venv/bin/pip install opencc）")

_PINYIN_RE = re.compile(r"^[^\W\d_]+$", re.UNICODE)


def primary(ch: str) -> str:
    """主读音（音节+声调符号，如 diū）；查不到或非法返回空串。

    不用 heteronym 全读音：pypinyin 的异读集含古音/方言音（如 宁 含 zhù、
    平 含 bìng），会把 佇→宁、併→平 这类现代不同音的对误判为同音。
    校验：读音须为纯字母序列（带声调符号元音如 ū/é 亦属字母），
    以排除 pypinyin 对未收录字回退返回原字的情况。
    """
    try:
        r = pinyin(ch, style=Style.TONE)[0][0]
    except Exception:
        return ""
    return r if _PINYIN_RE.match(r) else ""

# 康熙部首 / 部件字：不作为错字也不作为纠正目标
RADICALS = set(
    "讠廾龺釒钅镸糹纟饣艹氵扌犭刂礻衤疒耒辶鱼麦鹵鹡鹳鹨鹘鹀鹠髟"
)


def is_cjk(ch: str) -> bool:
    o = ord(ch)
    return (
        0x4E00 <= o <= 0x9FFF
        or 0x3400 <= o <= 0x4DBF
        or 0x20000 <= o <= 0x2A6DF
    )


def load_pool(path: Path) -> dict:
    d = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        if "\t" not in line:
            continue
        k, v = line.split("\t", 1)
        k = k.strip()
        if len(k) != 1 or not is_cjk(k):
            continue
        d[k] = {c for c in v.strip() if is_cjk(c)}
    return d


def main() -> None:
    ap = argparse.ArgumentParser(description="CEC 混淆字集策展（见模块 docstring）")
    ap.add_argument("--src", required=True, help="dat_data 目录（含 token_set.txt 等）")
    ap.add_argument(
        "--lexicon",
        default=str(Path(__file__).resolve().parent.parent / "lexicons" / "zh_gec_lexicon.json"),
    )
    ap.add_argument("--apply", action="store_true", help="合并进词表（默认 dry-run）")
    ap.add_argument("--tier", type=int, choices=(1, 2), default=2,
                    help="合并的最低层级：2=right 须同时是两份文件的键（默认，最保守）")
    ap.add_argument("--max", type=int, default=0, help="合并条数上限（0=不限）")
    args = ap.parse_args()

    src = Path(args.src)
    f1 = src / "token_set.txt"
    f2 = src / "new_cofuse_set.txt"
    if not f1.exists():
        sys.exit(f"缺少 {f1}")
    pools = [load_pool(f1)]
    if f2.exists():
        pools.append(load_pool(f2))
    keys = [set(p) for p in pools]
    keys_union = set().union(*keys)
    # 常见度梯度：键 = 真实（正确）语料句中的字。
    # 两份文件来自不同语料切分——「双文件键」≈ 可靠常见字；
    # 「仅单文件键」≈ 次常见；「非键」≈ 罕用字。
    both = keys[0] & (keys[1] if len(keys) > 1 else keys[0])

    lex_path = Path(args.lexicon)
    lex = json.loads(lex_path.read_text(encoding="utf-8"))
    existing = set()
    for e in lex["confusables"]:
        if isinstance(e, list):
            existing.add((e[0], e[1]))
        else:
            existing.add((e["wrong"], e["right"]))

    # (wrong, right) -> 出现次数（跨文件计票，用于分层）
    cands: dict = {}
    stat = {"raw": 0, "dup": 0, "radical": 0, "both_common": 0}
    for pool, kset in zip(pools, keys):
        for k, lst in pool.items():
            for c in lst:
                if c == k:
                    continue
                stat["raw"] += 1
                if c in RADICALS or k in RADICALS:
                    stat["radical"] += 1
                    continue
                if c in both:
                    # 双方都是可靠常见字：方向不可判 → 剔除（防误报）
                    stat["both_common"] += 1
                    continue
                if (c, k) in cands:
                    stat["dup"] += 1
                cands[(c, k)] = cands.get((c, k), 0) + 1

    # 主读音校验 + 繁简过滤 + 去重
    merged, dropped_pinyin, dropped_existing, dropped_trad = [], 0, 0, 0
    for (w, r), n in sorted(cands.items()):
        pw, pr = primary(w), primary(r)
        if not pw or not pr or pw != pr:
            dropped_pinyin += 1
            continue
        if (w, r) in existing:
            dropped_existing += 1
            continue
        if T2S is not None:
            ws, rs = T2S.convert(w), T2S.convert(r)
            if ws != w and ws != r:
                # wrong 是繁体/异体字，但 right 不是它的简体形：
                # 唯一正当纠正是繁→简本身（如 於→于），映射到同音他字（於→余）
                # 会把用字意图改错 → 剔除
                dropped_trad += 1
                continue
            if rs != r:
                # right 是繁体字：纠正目标应为简体字 → 剔除
                dropped_trad += 1
                continue
        if r in both and w not in keys_union:
            tier = 2  # right 双文件键（常见），wrong 非键（罕用）
        elif r in both and w in keys_union:
            tier = 1  # wrong 也是（单文件）键：次常见，方向信心较低
        else:
            tier = 0  # right 仅单文件键
        if T2S is not None and T2S.convert(w) != w:
            note = f"「{w}」为繁体字，简体文本应写作「{r}」"
        else:
            note = f"「{w}」为与「{r}」同音罕用字，现代文本多为「{r}」之误"
        merged.append({"wrong": w, "right": r, "tier": tier, "votes": n, "note": note})

    t2 = [m for m in merged if m["tier"] == 2]
    t1 = [m for m in merged if m["tier"] == 1]
    t0 = [m for m in merged if m["tier"] == 0]

    print("=== CEC 混淆字集策展统计 ===")
    print(f"  键集合: 文件1 {len(keys[0])}  文件2 {len(keys[1]) if len(keys) > 1 else 0}  "
          f"交集 {len(both)}  并集 {len(keys_union)}")
    for k, v in stat.items():
        print(f"  {k:12s} {v}")
    print(f"  候选(去重后) {len(cands)}  主读音不匹配剔除 {dropped_pinyin}  "
          f"繁体→繁体剔除 {dropped_trad}  词表已有剔除 {dropped_existing}")
    print(f"  可合并 tier2(right 双文件键, wrong 非键) {len(t2)}   "
          f"tier1(wrong 单文件键) {len(t1)}   tier0 {len(t0)}")

    # 候选全量清单（供人工复核）
    out_tsv = Path(__file__).resolve().parent / "cec_confusable_candidates.tsv"
    with out_tsv.open("w", encoding="utf-8") as f:
        for m in merged:
            f.write(f"{m['wrong']}\t{m['right']}\t{m['tier']}\t{m['votes']}\t{m['note']}\n")
    print(f"  候选清单 → {out_tsv}")
    top2 = sorted(t2, key=lambda m: -m["votes"])
    print("\n  tier2 样本（按票数前 20）：")
    for m in top2[:20]:
        print(f"    {m['wrong']} → {m['right']}  (票数 {m['votes']}, {m['note']})")

    if not args.apply:
        print("\n[dry-run] 未写词表；确认无误后加 --apply 合并。")
        return

    pick = sorted((m for m in merged if m["tier"] >= args.tier),
                 key=lambda m: (-m["tier"], -m["votes"]))
    if args.max:
        pick = pick[: args.max]
    if not pick:
        print("无待合并条目。")
        return
    bak = lex_path.with_name(lex_path.name + f".bak-{datetime.now():%Y%m%d%H%M%S}")
    shutil.copy2(lex_path, bak)
    for m in pick:
        lex["confusables"].append([m["wrong"], m["right"], m["note"]])
    lex_path.write_text(json.dumps(lex, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(f"已合并 {len(pick)} 条 tier>={args.tier} → {lex_path}（备份 {bak.name}）")


if __name__ == "__main__":
    main()