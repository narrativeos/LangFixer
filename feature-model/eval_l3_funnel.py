#!/usr/bin/env python3
"""L3 漏斗评测脚本——一键跑 MuCGEC 出完整漏斗报告。

用法:
    cd feature-model
    python3 eval_l3_funnel.py                    # 默认参数
    L3_GRAY_ZONE_LOW=0.35 python3 eval_l3_funnel.py  # 自定义阈值
    L3_ABSTAIN_RATIO=0.30 python3 eval_l3_funnel.py  # 自定义段落阈值
    L3_HEURISTIC_PROB=0 python3 eval_l3_funnel.py     # 关闭启发式（用模型）

输出: Markdown 格式漏斗报告（可存为 eval_reports/ 下的文件）
"""
from __future__ import annotations

import json
import os
import re
import sys
import time
from collections import Counter
from datetime import datetime, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent
sys.path.insert(0, str(HERE))

import correct  # noqa: E402

# ─── 配置 ───
GRAY_ZONE_LOW = float(os.environ.get("L3_GRAY_ZONE_LOW", "0.30"))
ABSTAIN_RATIO = float(os.environ.get("L3_ABSTAIN_RATIO", "0.80"))
HEURISTIC = os.environ.get("L3_HEURISTIC_PROB", "1") == "1"
GOLD_PATH = os.environ.get("L3_EVAL_GOLD", str(REPO / "datasets" / "mucgec_gold.json"))
CHAR_GATE_URL = os.environ.get("L3_CHAR_GATE_URL", "")
CHAR_GATE_TAU = float(os.environ.get("L3_CHAR_GATE_TAU", "0.004"))


def char_gate_scores(texts: list[str]) -> dict[str, float] | None:
    """字级侧车批量打分（D.2 救援）；不可达 → None。"""
    if not CHAR_GATE_URL:
        return None
    import urllib.request
    out: dict[str, float] = {}
    try:
        uniq = list(dict.fromkeys(texts))
        for i in range(0, len(uniq), 64):
            req = urllib.request.Request(
                CHAR_GATE_URL,
                data=json.dumps({"texts": uniq[i:i + 64]}).encode(),
                headers={"Content-Type": "application/json"})
            with urllib.request.urlopen(req, timeout=10) as r:
                for t, s in zip(uniq[i:i + 64], json.loads(r.read())["scores"]):
                    out[t] = s
        return out
    except Exception as e:  # noqa: BLE001
        print(f"WARN char-gate unavailable: {e}", file=sys.stderr)
        return None

# ─── 加载词表 ───
lexicon_path = REPO / "lexicons" / "zh_gec_lexicon.json"
n_loaded = correct.load_confusables(str(lexicon_path))

# ─── 加载 3-gram 频率表 + 高频 bigram 表（方案 B：与 server.py 同源实现）───
# L3_NGRAM_TABLE / L3_BIGRAM_TABLE 可指向别的表文件做 A/B（如旧表快照），默认用仓库当前表。
_NGRAM_TABLE, _BG_TABLE = correct.load_zh_ngram_tables(
    os.environ.get("L3_NGRAM_TABLE", str(HERE / "ngram_table.json")),
    os.environ.get("L3_BIGRAM_TABLE", str(HERE / "bigram_table_zh.json")))

# ─── 工具函数 ───
def byte_to_char(src: str, b: int) -> int:
    return len(src.encode("utf-8")[:b].decode("utf-8"))

def cspan(src: str, bspan: list) -> tuple[int, int]:
    return (byte_to_char(src, bspan[0]), byte_to_char(src, bspan[1]))

def spans_overlap(a: tuple, b: tuple) -> bool:
    return a[0] <= b[1] and b[0] <= a[1]

def heuristic_prob(s: str) -> float:
    """与 server.py _heuristic_prob 同源（correct.zh_heuristic_prob）"""
    return correct.zh_heuristic_prob(s, _NGRAM_TABLE, _BG_TABLE)

# ─── 主评测 ───
def main():
    t0 = time.time()
    gold = json.load(open(GOLD_PATH, encoding="utf-8"))
    n_total = len(gold)
    cscores = char_gate_scores([e["src"] for e in gold])

    # 分类计数
    n_pattern = 0       # 定案档 pattern 命中 → 定案
    n_advisory = 0      # 仅灰区档（advisory）命中 → 弃权交 LLM
    n_no_error = 0      # 启发式无错 → 跳过
    n_abstain = 0       # 弃权 → LLM
    n_tp_edit = 0       # pattern 覆盖的 gold 编辑数
    n_gold_edit = 0
    n_fp = 0            # 无错句被定案档 pattern 命中
    n_fn = 0            # 有错句被启发式跳过
    n_fn_edit = 0       # FN 句中丢失的 gold 编辑数
    n_rescued = 0       # 启发式灰区但字级救援放行（计入 n_no_error）

    # 段落级
    n_paras = 0
    n_paras_llm = 0

    # 定向复查成本模拟（A 优化）：LLM 输入字符量（旧 = 全文兜底 / 新 = 仅灰区句）
    llm_chars_old = 0        # 旧策略：整段弃权 → 全文送 LLM；部分定案 → 0（静默跳过）
    llm_chars_targeted = 0   # 新策略：灰区句送定向复查（ABSTAIN_RATIO 放宽后）
    n_gray_skipped = 0       # 旧策略下部分定案时被静默跳过的灰区句（新策略将复查）
    paras: list[tuple[float, int, int]] = []  # (灰区率, 全文字符, 灰区字符)

    # 按 5 句一组模拟段落
    for i in range(0, n_total, 5):
        chunk = gold[i:i+5]
        para_abstain = 0
        para_gray_chars = 0
        para_full_chars = 0
        for entry in chunk:
            src = entry["src"]
            edits = entry.get("edits", [])
            n_gold_edit += len(edits)
            has_gold = len(edits) > 0
            para_full_chars += len(src)

            # Pattern（两档：定案档直接定案；advisory 档仅弃权——与 server.py 一致）
            hits = correct.find_zh_hits(src)
            safe_hits = [h for h in hits if not getattr(h, "advisory", False)]
            if safe_hits:
                n_pattern += 1
                for e in edits:
                    gs = cspan(src, e["span"])
                    if any(spans_overlap(h.span, gs) for h in safe_hits):
                        n_tp_edit += 1
                if not has_gold:
                    n_fp += 1
                continue
            if hits:
                n_advisory += 1
                n_abstain += 1
                para_abstain += 1
                para_gray_chars += len(src)
                continue

            # Heuristic / Model
            if HEURISTIC:
                p = heuristic_prob(src)
            else:
                p = 0.40  # 模型常数回退

            if p < GRAY_ZONE_LOW:
                n_no_error += 1
                if has_gold:
                    n_fn += 1
                    n_fn_edit += len(edits)
            elif cscores is not None and cscores.get(src, 1.0) < CHAR_GATE_TAU:
                n_rescued += 1
                n_no_error += 1
                if has_gold:
                    n_fn += 1
                    n_fn_edit += len(edits)
            else:
                n_abstain += 1
                para_abstain += 1
                para_gray_chars += len(src)

        n_paras += 1
        ratio = para_abstain / len(chunk)
        paras.append((ratio, para_full_chars, para_gray_chars))
        if ratio > ABSTAIN_RATIO:
            n_paras_llm += 1
            llm_chars_old += para_full_chars
        else:
            # 旧策略：部分定案 → 灰区句静默跳过（不送 LLM）
            n_gray_skipped += para_abstain
        # 新策略（定向复查）：ABSTAIN_RATIO 放宽 → 任何段落都部分定案，
        # LLM 只收灰区句
        llm_chars_targeted += para_gray_chars

    elapsed = time.time() - t0

    # ─── 真实用户文本门控评测（方案 B：启发式门的成本/漏检在母语用户文本上的表现）───
    # MuCGEC 95% 是病句（学习者语料），其弃权率不代表真实流量；CSCD/NaCGEC/confirmed
    # 及 CTCDataset 转换集（cscdns=原生微博长句/NLPCC23/CCTC 跨句）是母语用户文本
    # （均已从 n-gram 表去污染），衡量门控在真实分布上的行为。
    real_rows: list[tuple[str, bool, str]] = []
    for nm in ("cscd_gold.json", "nacgec_gold.json",
               "cscdns_gold.json", "nlpcc23_gold.json", "cctc_gold.json"):
        p = REPO / "datasets" / nm
        if p.is_file():
            real_rows += [(e["src"], bool(e.get("edits")), nm)
                          for e in json.load(open(p, encoding="utf-8"))]
    conf_p = HERE / "l3_data" / "zh" / "confirmed.jsonl"
    if conf_p.is_file():
        for ln in open(conf_p, encoding="utf-8"):
            ln = ln.strip()
            if ln:
                r = json.loads(ln)
                if str(r.get("text", "")).strip():
                    real_rows.append((str(r["text"]), int(r.get("label", 1)) == 1, "confirmed"))
    real_stats: dict[str, dict[str, int]] = {}
    rsc = char_gate_scores([t for t, _, _ in real_rows])
    for src, has_err, nm in real_rows:
        st = real_stats.setdefault(nm, {"clean": 0, "clean_pass": 0, "err": 0,
                                        "err_pass": 0, "pat": 0, "rescued": 0})
        if correct.find_zh_hits(src):
            st["pat"] += 1
            continue
        p = heuristic_prob(src) if HEURISTIC else 0.40
        passed = p < GRAY_ZONE_LOW
        rescued = not passed and rsc is not None and rsc.get(src, 1.0) < CHAR_GATE_TAU
        if passed or rescued:
            st["rescued"] += rescued
        if has_err:
            st["err"] += 1
            if passed or rescued:
                st["err_pass"] += 1
        else:
            st["clean"] += 1
            if passed or rescued:
                st["clean_pass"] += 1

    # ─── 输出报告 ───
    ts = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    print(f"# L3 漏斗评测报告 {ts}\n")
    print(f"## 配置")
    print(f"| 参数 | 值 |")
    print(f"|------|-----|")
    print(f"| GRAY_ZONE_LOW | {GRAY_ZONE_LOW} |")
    print(f"| ABSTAIN_RATIO | {ABSTAIN_RATIO} |")
    print(f"| HEURISTIC_PROB | {'on' if HEURISTIC else 'off'} |")
    print(f"| 字级救援 (char-gate) | {'on ' + CHAR_GATE_URL + f' τ={CHAR_GATE_TAU}' if cscores is not None else 'off'} |")
    print(f"| 词表加载 | {n_loaded} 条繁→简 |")
    print(f"| 上下文 pattern | {correct.context_confusable_count()} 条（定案/灰区两档） |")
    print(f"| 数据 | {GOLD_PATH} ({n_total} 句) |")
    print()

    print(f"## 句级漏斗")
    print(f"| 阶段 | 句数 | 占比 |")
    print(f"|------|------|------|")
    print(f"| Pattern 定案（定案档） | {n_pattern} | {n_pattern/n_total:.1%} |")
    print(f"| 启发式无错跳过 | {n_no_error} | {n_no_error/n_total:.1%} |")
    if cscores is not None:
        print(f"| （其中字级救援放行） | {n_rescued} | {n_rescued/n_total:.1%} |")
    print(f"| 弃权→LLM | {n_abstain} | {n_abstain/n_total:.1%} |")
    print(f"| └ 其中 advisory 命中转弃权 | {n_advisory} | {n_advisory/n_total:.1%} |")
    print(f"| **合计** | **{n_total}** | 100% |")
    print()

    print(f"## 质量指标")
    print(f"| 指标 | 值 |")
    print(f"|------|-----|")
    print(f"| LLM 调用率（句级） | {n_abstain/n_total:.1%} |")
    print(f"| Gold 编辑覆盖 | {n_tp_edit}/{n_gold_edit} ({n_tp_edit/max(1,n_gold_edit):.1%}) |")
    print(f"| Pattern FP（无错句命中） | {n_fp} |")
    print(f"| 启发式 FN（有错被跳过） | {n_fn} ({n_fn/max(1,n_total):.1%}) |")
    print(f"| FN 丢失编辑数 | {n_fn_edit} |")
    print()

    print(f"## 段落级（5 句/段）")
    print(f"| 指标 | 值 |")
    print(f"|------|-----|")
    print(f"| 段落数 | {n_paras} |")
    print(f"| 需 LLM | {n_paras_llm} ({n_paras_llm/n_paras:.1%}) |")
    print(f"| L3 定案 | {n_paras - n_paras_llm} ({(n_paras-n_paras_llm)/n_paras:.1%}) |")
    print()

    print(f"## 真实用户文本门控（无 pattern 命中句，启发式门控行为）")
    print(f"> 母语用户文本（已从 n-gram 表去污染）。clean 通过率=成本收益；病句通过率=漏检风险。")
    print(f"| 数据 | n | pattern 命中 | clean 句 | clean 直接放行 | 病句 | 病句直接放行（漏检） |")
    print(f"|------|---|------|------|------|------|------|")
    for nm, st in real_stats.items():
        n_all = st["clean"] + st["err"] + st["pat"]
        cp = f"{st['clean_pass']}/{st['clean']} ({st['clean_pass']/max(1,st['clean']):.0%})"
        ep = f"{st['err_pass']}/{st['err']} ({st['err_pass']/max(1,st['err']):.0%})"
        print(f"| {nm} | {n_all} | {st['pat']} | {st['clean']} | {cp} | {st['err']} | {ep} |")
    print()

    print(f"## 定向复查成本模拟（unreviewed_spans → 只送灰区句）")
    print(f"| 指标 | 旧策略（全文兜底） | 新策略（定向复查） |")
    print(f"|------|------|------|")
    print(f"| LLM 输入总字符 | {llm_chars_old} | {llm_chars_targeted} |")
    if llm_chars_old:
        print(f"| 输入量变化 | — | {(llm_chars_targeted-llm_chars_old)/llm_chars_old:+.1%} |")
    print(f"| 旧策略静默跳过的灰区句 | {n_gray_skipped} | 0（全部复查） |")
    print(f"> 新策略前提：`L3_ABSTAIN_RATIO` 放宽（定向复查已保证灰区句必审），"
          f"LLM 输入 = 灰区句拼接。")
    print()

    print(f"## ABSTAIN_RATIO 放宽扫描（定向复查下：ratio>R 段全文 / 其余仅灰区句）")
    print(f"| R | 全文复查段 | LLM 输入字符 | Δ vs R=0.20 基线 |")
    print(f"|------|------|------|------|")
    base = None
    for R in (0.20, 0.40, 0.60, 0.80, 1.01):
        chars = 0
        n_full = 0
        for ratio, full_c, gray_c in paras:
            if ratio > R:
                chars += full_c
                n_full += 1
            else:
                chars += gray_c
        if base is None:
            base = chars
        d = f"{(chars - base) / base:+.1%}" if base else "—"
        print(f"| {R:.2f} | {n_full} ({n_full/n_paras:.0%}) | {chars} | {d} |")
    print(f"> R=1.01 = 永不整段弃权（全部定向复查）。上下文代价：部分定案段 LLM 只见灰区句，")
    print(f"> 跨句语境丢失——错误密集段（ratio>R）保留全文复查即覆盖最需语境的场景。")
    print()

    print(f"## 耗时")
    print(f"{elapsed:.2f}s")

if __name__ == "__main__":
    main()
