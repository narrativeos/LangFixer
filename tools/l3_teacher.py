#!/usr/bin/env python3
"""L3 蒸馏教师批:T2(32B :8090)对 gray 句按 ③ 契约直接出 gold 检出.

用途:补 ③ 召回分布(L2 教训——步数不解决分布)。教师行存 SFT messages 格式,
JSON 可解析且 items 非空的行才入库(教师 [] 行丢弃:沉默是 L1 已会的)。
断点续跑:按 meta.text 去重。temperature 0 + thinking 关,与判官同一把尺。
"""
from __future__ import annotations

import argparse
import json
import urllib.request

PROMPT = "找出以下文本中的错误："
URL = "http://127.0.0.1:8090/v1/chat/completions"


def ask(text: str) -> str:
    body = {"model": "t2", "temperature": 0, "max_tokens": 512,
            "chat_template_kwargs": {"enable_thinking": False},
            "messages": [{"role": "user", "content": PROMPT + text}]}
    req = urllib.request.Request(URL, data=json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=120) as r:
        return json.load(r)["choices"][0]["message"]["content"]


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--gray", required=True, help="gray jsonl(text 字段)")
    ap.add_argument("--out", required=True)
    ap.add_argument("--source", default="gray380")
    args = ap.parse_args()

    done: set[str] = set()
    try:
        with open(args.out, encoding="utf-8") as f:
            for line in f:
                done.add(json.loads(line)["meta"]["text"])
    except FileNotFoundError:
        pass

    n_ok = n_empty = n_bad = 0
    with open(args.out, "a", encoding="utf-8") as fo:
        for line in open(args.gray, encoding="utf-8"):
            text = json.loads(line)["text"]
            if text in done:
                continue
            raw = ask(text).strip().strip("`")
            if raw.lower().startswith("json"):
                raw = raw[4:].strip()
            try:
                items = json.loads(raw)
                assert isinstance(items, list)
            except Exception:
                n_bad += 1
                continue
            if not items:
                n_empty += 1
                continue
            fo.write(json.dumps({"messages": [
                {"role": "user", "content": PROMPT + text},
                {"role": "assistant",
                 "content": json.dumps(items, ensure_ascii=False)}],
                "meta": {"kind": "l3-teacher", "src": args.source, "text": text}},
                ensure_ascii=False) + "\n")
            fo.flush()
            n_ok += 1
    print(f"[l3] teacher_ok={n_ok} empty={n_empty} bad={n_bad} → {args.out}")


if __name__ == "__main__":
    main()
