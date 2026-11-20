#!/usr/bin/env python3
"""公文体干净句抓取（政府网政策文件库，PLAN_v2 §6 下一臂「公文域合成源」）。

来源：sousuo.www.gov.cn/search-gov/data 的公开检索接口（t=zhengcelibrary_gw 国务院
文件 6,229 条 / zhengcelibrary_bm 部门文件 12,822 条）→ 文件库文章页正文。
纪律：限速 --delay（默认 0.4s/篇）；断点续（state.tsv 记已处理 URL）；
只取段落文本 → 分句 → 汉字比/长度/去重三过滤；产物是**干净句**（不注入错误）。

用法：
  python3 datasets/scrape_govcn.py --docs 1500 --out ~/tmp/govcn/clean.txt
  重跑即续抓。产出后：tools/label-rollout/synth_insert_arm.py --clean-pool <该文件>

注意：政府公开信息，限速+标识 UA；量够即停，不整库拉取。
"""
from __future__ import annotations

import argparse
import hashlib
import html
import json
import random
import re
import time
import urllib.request
from pathlib import Path

UA = ("Mozilla/5.0 (Macintosh; Intel OS X 10_15_7) AppleWebKit/537.36 "
      "(KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36")
TAG = re.compile(r"<[^>]+>")
P_RE = re.compile(r"<p[^>]*>(.*?)</p>", re.S | re.I)
SENT = re.compile(r"[^。！？；…]*[。！？；…]+")
CJK = lambda c: "\u4e00" <= c <= "\u9fff"


def get(url: str, timeout: int = 25) -> bytes:
    req = urllib.request.Request(url, headers={
        "User-Agent": UA,
        "Accept": "text/html,application/xhtml+xml,application/json;q=0.9,*/*;q=0.8",
        "Accept-Language": "zh-CN,zh;q=0.9",
        "Referer": "https://www.gov.cn/",
    })
    return urllib.request.urlopen(req, timeout=timeout).read()


def list_docs(t: str, cap_pages: int) -> list[tuple[str, str]]:
    out = []
    p = 1
    while p <= cap_pages:
        url = (f"https://sousuo.www.gov.cn/search-gov/data?t={t}&q=&timetype=timeqb"
               f"&sort=pubtime&sortType=1&p={p}&n=100")
        d = None
        for attempt in range(3):
            try:
                d = json.loads(get(url))
                break
            except Exception:
                time.sleep(1.5 * (attempt + 1))
        if d is None:
            print(f"[list] {t} p{p} 连续 3 次失败，停止该源")
            break
        vo = (d.get("searchVO") or {}).get("catMap") or {}
        rows = []
        for v in vo.values():
            rows += (v or {}).get("listVO") or []
        if not rows:
            break
        for it in rows:
            u, ti = it.get("url") or "", (it.get("title") or "").strip()
            if u.startswith("http"):
                out.append((u, ti))
        tp = (d["searchVO"] or {}).get("totalpage") or 1
        if p >= tp:
            break
        p += 1
        time.sleep(0.5)
    return out


def sentences(doc_html: str) -> list[str]:
    body = re.sub(r"<(script|style)[^>]*>.*?</\1>", "", doc_html, flags=re.S | re.I)
    chunks = []
    for p in P_RE.findall(body):
        t = html.unescape(TAG.sub("", p)).replace("\u3000", " ").strip()
        chunks += SENT.findall(t)
    return [s.strip() for s in chunks]


def good(s: str, seen: set) -> bool:
    if not 20 <= len(s) <= 220 or "|" in s or "http" in s:
        return False
    cjk = sum(1 for c in s if CJK(c))
    if cjk / len(s) < 0.55:
        return False
    key = hashlib.md5(re.sub(r"\s+", "", s).encode()).hexdigest()
    if key in seen:
        return False
    seen.add(key)
    return True


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--docs", type=int, default=1000, help="本次抓取的文档页数上限（续抓语义）")
    ap.add_argument("--out", default=str(Path.home() / "tmp" / "govcn" / "clean.txt"))
    ap.add_argument("--delay", type=float, default=0.4)
    ap.add_argument("--seed", type=int, default=7)
    a = ap.parse_args()
    out = Path(a.out)
    state = out.parent / "state.tsv"
    out.parent.mkdir(parents=True, exist_ok=True)
    done = set()
    if state.exists():
        done = {l.split("\t", 1)[0] for l in state.read_text(encoding="utf-8").splitlines() if l}

    docs = list_docs("zhengcelibrary_bm", 130) + list_docs("zhengcelibrary_gw", 70)
    rng = random.Random(a.seed)
    rng.shuffle(docs)
    print(f"[govcn] 清单 {len(docs):,} 篇（已抓 {len(done):,}）")
    seen = set()
    if out.exists():
        for l in out.read_text(encoding="utf-8").splitlines():
            seen.add(hashlib.md5(re.sub(r"\s+", "", l).encode()).hexdigest())
    n_new = n_try = 0
    with open(out, "a", encoding="utf-8") as fo, open(state, "a", encoding="utf-8") as fs:
        for u, title in docs:
            if u in done:
                continue
            if n_try >= a.docs:
                break
            n_try += 1
            try:
                sents = [s for s in sentences(get(u).decode("utf-8", "ignore")) if good(s, seen)]
                for s in sents:
                    fo.write(s + "\n")
                fs.write(f"{u}\t{len(sents)}\t{title[:40]}\n")
                n_new += len(sents)
            except Exception as e:
                fs.write(f"{u}\tERR\t{type(e).__name__}\n")
            if n_try % 100 == 0:
                print(f"[govcn] 已抓 {n_try} 篇 · 新句 {n_new:,} · 池 ≈{len(seen):,}", flush=True)
            time.sleep(a.delay)
    print(f"[govcn] 本次 {n_try} 篇 · 新句 {n_new:,} · 池 {len(seen):,}")


if __name__ == "__main__":
    main()
