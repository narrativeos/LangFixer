#!/usr/bin/env python3
"""CEC（TW-NLP/ChineseErrorCorrector）评测数据共享工具：

- download()：HuggingFace 文件下载（hf-mirror 镜像优先、官网兜底；幂等，
  已存在且非空则跳过；可用 HF_ENDPOINT 覆盖镜像地址）
- detokenize()：还原字级切分行（单字 token 以空格分隔 → 去空格）
- is_variant()：繁体→简体判定（OpenCC t2s；不可用时回退内置繁体集）
- make_edit()：生成 gold 编辑条目（字节 span + cat + cat_rule），
  分类口径与 make_gold_zh.py 一致（punct / particle / variant / confusable / other）

供 prepare_cscd.py / prepare_nacgec.py 使用。
"""
import os
import re
import urllib.request

import analyze_mucgec as A
import make_gold_zh as G

# 镜像优先（huggingface.co 在部分网络环境不可达）；HF_ENDPOINT 可覆盖
MIRRORS = [os.environ.get("HF_ENDPOINT", "https://hf-mirror.com"),
           "https://huggingface.co"]


def download(dataset: str, filename: str, dest: "os.PathLike") -> str:
    """下载 `datasets/<dataset>/<filename>` 到 dest；幂等。"""
    dest = str(dest)
    if os.path.exists(dest) and os.path.getsize(dest) > 0:
        return dest
    os.makedirs(os.path.dirname(dest), exist_ok=True)
    last_err = None
    for base in MIRRORS:
        url = f"{base}/datasets/{dataset}/resolve/main/{filename}"
        try:
            with urllib.request.urlopen(url, timeout=300) as r:
                data = r.read()
            # 跟随重定向失败时镜像会返回 HTML 提示页
            if data[:1] == b"<" or b"Redirecting" in data[:200]:
                last_err = f"{base} 返回非数据内容"
                continue
            with open(dest, "wb") as f:
                f.write(data)
            print(f"  下载 {dataset}/{filename} → {dest}（{len(data)//1024} KB）")
            return dest
        except Exception as e:  # noqa: BLE001
            last_err = f"{base}: {e}"
    raise RuntimeError(f"下载 {dataset}/{filename} 失败：{last_err}")


def detokenize(s: str) -> str:
    """字级切分行（`完 善 检 剑 …`）→ 连续文本；普通行原样返回。"""
    if " " not in s:
        return s
    tokens = s.split()
    if tokens and all(len(t) <= 2 for t in tokens):
        return "".join(tokens)
    return s


try:
    from opencc import OpenCC
    _T2S = OpenCC("t2s")
except Exception:  # noqa: BLE001  opencc 未安装时回退内置集
    _T2S = None


def is_variant(aw: str, bw: str) -> bool:
    """繁体→简体判定：aw 的 t2s 转换恰为 bw（aw 本身含繁体字）。"""
    if not aw or not bw or aw == bw:
        return False
    if _T2S is not None:
        return _T2S.convert(aw) == bw
    return (any(c in G.TRADITIONAL for c in aw)
            and not any(c in G.TRADITIONAL for c in bw))


def make_edit(op: str, aw: str, bw: str, src: str, cspan) -> dict:
    """与 make_gold_zh.py 同构的 gold 编辑条目（span 为源句字节区间）。"""
    cat = A.classify(aw, bw)
    if cat == "punct":
        cat_rule = "punct"
    elif op in ("D", "I"):
        cat_rule = "other"
    elif (aw, bw) in G.PARTICLE_PAIRS:
        cat_rule = "particle"
    elif len(aw) <= 4 and G.py(aw) == G.py(bw):
        if is_variant(aw, bw):
            cat_rule = "variant"
        else:
            cat_rule = "confusable"
    else:
        cat_rule = "other"
    return {
        "op": op,
        "src": aw,
        "ref": bw,
        "span": G.char_span_to_bytes(src, cspan[0], cspan[1]),
        "cat": cat,
        "cat_rule": cat_rule,
    }


def diff_edits(src: str, ref: str):
    """src vs ref 的字级编辑（去重键含 span），返回 gold 编辑列表。"""
    seen = set()
    edits = []
    for op, aw, bw, cspan in A.diff_spans(src, ref):
        key = (op, aw, bw, tuple(cspan))
        if key in seen:
            continue
        seen.add(key)
        edits.append(make_edit(op, aw, bw, src, cspan))
    edits.sort(key=lambda e: e["span"][0])
    return edits


if __name__ == "__main__":
    # 冒烟：detokenize / is_variant
    assert detokenize("完 善 检 剑 体 系") == "完善检剑体系"
    assert detokenize("你好！我是张爱文。") == "你好！我是张爱文。"
    assert is_variant("時", "时") and not is_variant("剑", "验")
    print("cec_common 冒烟通过")