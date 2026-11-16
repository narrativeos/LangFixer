#!/usr/bin/env python3
"""金集灰区 · 自动裁决 + 人裁 + 标记通道（配 index.html）。

数据源：
- ~/tmp/grey_zone/gold_grey_chars.jsonl —— v2.2 未命中的 21,257 个 gold 字符
  逐条分诊（并集口径，与 dataset_v22/stats.json 的 model_miss 精确一致）。
- dataset_v21 / dataset_v22 标签文件 —— 只取 model_errors 与 gate_cands，
  为每个灰区字符现算信号（不信任行级计数字段）。

自动分桶（尽量自动裁决，拿不准留给人/后续规则）：
- auto_real  回退：v2.1 模型已给过 fix 且与 gold 相互印证 → 默认「真错」，带旧 fix。
- auto_noise 问了被否：两轮提问（第N字 + 片段±6/整句）均被模型否 → 默认「金集噪声」，
             采纳走分档抽检门（每桶 ≥58 且反判 ≤2%）。
- manual     仅一轮问过被否：边界样本，人裁。
- mark       门控未见：两轮门控都没覆盖，词法通道已证弱——不硬判，默认「标记」，
             导出为规则候选，落进系统后由用户运行时命中判断。

裁决 journal 追加落盘（done 必须落盘续跑），重启即恢复。
用法：python3 serve.py [--grey ~/tmp/grey_zone] [--labels ~/tmp] [--state-dir ~/tmp/gold_adjudicate] [--port 8766]
"""
from __future__ import annotations

import argparse
import json
import re
import sys
import threading
import time
from pathlib import Path
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse, parse_qs

GATE_MIN_REVIEWED = 58
GATE_MAX_ERR = 0.02
BUCKETS = ("auto_real", "auto_noise", "manual", "mark")
BUCKET_LABEL = {"auto_real": "自动·真错", "auto_noise": "自动·噪声",
                "manual": "边界人裁", "mark": "标记→规则通道"}
DEFAULT_VERDICT = {"auto_real": "real", "auto_noise": "noise", "mark": "mark"}


def load_labels(d: Path, ver: str) -> dict:
    rows = {}
    if not d.exists():
        sys.exit(f"未找到标签目录 {d}")
    for fp in sorted(d.glob(f"*_{ver}.jsonl")):
        if "review_queue" in fp.name:
            continue
        with open(fp, encoding="utf-8") as f:
            for line in f:
                r = json.loads(line)
                rows[r["id"]] = {"edits": [(e["a"], e["b"], e.get("fix", ""), e.get("type", ""))
                                           for e in r["model_errors"]],
                                 "cands": set(r["gate_cands"])}
    return rows


class Data:
    def __init__(self, grey_dir: Path, labels_dir: Path, state_dir: Path):
        self.jrn_path = state_dir / "gold_verdicts_journal.jsonl"
        self.promo_path = state_dir / "gold_promoted.json"
        self.verdicts: dict[str, dict] = {}
        self.promoted: dict[str, dict] = {}
        self.lock = threading.Lock()
        state_dir.mkdir(parents=True, exist_ok=True)
        gp = grey_dir / "gold_grey_chars.jsonl"
        if not gp.exists():
            sys.exit(f"未找到灰区清单 {gp}（先跑 dump_grey.py）")
        v21 = load_labels(labels_dir / "dataset_v21", "v21")
        v22 = load_labels(labels_dir / "dataset_v22", "v22")
        self.items: list[dict] = []
        with open(gp, encoding="utf-8") as f:
            for line in f:
                x = json.loads(line)
                rid, c = x["id"], x["pos"]
                e21, a21 = v21.get(rid, {"edits": [], "cands": set()})["edits"], v21.get(rid, {}).get("cands", set())
                a22 = v22.get(rid, {}).get("cands", set()) if rid in v22 else set()
                hit21 = any(a <= c < b for a, b, _, _ in e21)  # 模型插入=空range，恒不算命中（逐条口径）
                vfix = next(({"fix": fx, "type": t, "a": a, "b": b}
                             for a, b, fx, t in e21 if a <= c < b), None)
                asked21, asked22 = c in a21, c in a22
                if hit21:
                    bkt = "auto_real"
                elif x["partition"] == "问了被否" and asked21 and asked22:
                    bkt = "auto_noise"
                elif x["partition"] == "问了被否":
                    bkt = "manual"
                else:
                    bkt = "mark"
                x.update({"iid": f"{rid}#{c}", "bucket": bkt,
                          "sig": {"hit21": hit21, "asked21": asked21, "asked22": asked22},
                          "v21fix": vfix})
                self.items.append(x)
        self._load_state()

    # ── 状态 ─────────────────────────────────────────────
    def _load_state(self):
        if self.jrn_path.exists():
            with open(self.jrn_path, encoding="utf-8") as f:
                for line in f:
                    rec = json.loads(line)
                    self.verdicts[rec["iid"]] = rec
        if self.promo_path.exists():
            self.promoted = json.loads(self.promo_path.read_text(encoding="utf-8"))

    def record(self, iid: str, verdict: str, reason: str = ""):
        rec = {"iid": iid, "verdict": verdict, "reason": reason, "at": int(time.time())}
        with self.lock:
            with open(self.jrn_path, "a", encoding="utf-8") as f:
                f.write(json.dumps(rec, ensure_ascii=False) + "\n")
            self.verdicts[iid] = rec

    def bulk(self, its: list[dict], verdict: str | None, reason: str = "") -> int:
        n = 0
        with self.lock:
            with open(self.jrn_path, "a", encoding="utf-8") as f:
                for x in its:
                    if x["iid"] in self.verdicts:
                        continue
                    v = verdict or DEFAULT_VERDICT.get(x["bucket"])
                    if not v:
                        continue
                    rec = {"iid": x["iid"], "verdict": v, "reason": reason, "at": int(time.time())}
                    f.write(json.dumps(rec, ensure_ascii=False) + "\n")
                    self.verdicts[x["iid"]] = rec
                    n += 1
        return n

    # ── 统计 ─────────────────────────────────────────────
    def bucket_stats(self) -> dict:
        bs = {b: {"total": 0, "reviewed": 0, "err": 0, "promoted": self.promoted.get(b)}
              for b in BUCKETS}
        for x in self.items:
            bs[x["bucket"]]["total"] += 1
        for x in self.items:
            v = self.verdicts.get(x["iid"])
            if not v:
                continue
            b = bs[x["bucket"]]
            b["reviewed"] += 1
            d = DEFAULT_VERDICT.get(x["bucket"])
            if d and v["verdict"] != d:
                b["err"] += 1
        for b in bs.values():
            b["err_rate"] = (b["err"] / b["reviewed"]) if b["reviewed"] else 0.0
            b["gate_ok"] = b["reviewed"] >= GATE_MIN_REVIEWED and b["err_rate"] <= GATE_MAX_ERR
        return bs

    def promote(self, bucket: str, note: str):
        bs = self.bucket_stats()
        if bucket not in bs:
            raise ValueError(f"未知桶 {bucket}")
        if bucket == "mark" or not bs[bucket]["gate_ok"]:
            raise ValueError(f"{bucket} 未过门（需 ≥{GATE_MIN_REVIEWED} 已裁且反判 ≤{GATE_MAX_ERR:.0%}）")
        self.promoted[bucket] = {"note": note, "at": int(time.time()),
                                 "reviewed": bs[bucket]["reviewed"], "err": bs[bucket]["err"]}
        self.promo_path.write_text(json.dumps(self.promoted, ensure_ascii=False, indent=1), encoding="utf-8")

    def export_lines(self) -> list[dict]:
        out = []
        by_iid = {x["iid"]: x for x in self.items}
        for iid, v in sorted(self.verdicts.items()):
            x = by_iid.get(iid)
            if not x:
                continue
            g = x["gold"]
            if v["verdict"] == "real":
                y = {"id": x["id"], "pos": x["pos"], "set": x["set"], "a": g["a"], "b": g["b"],
                     "op": g["op"], "ref": g["ref"], "action": "gold-real",
                     "source": "grey-zone", "bucket": x["bucket"]}
                if x["v21fix"]:
                    y["fix"] = x["v21fix"]["fix"]
                    y["type"] = x["v21fix"]["type"]
                out.append(y)
            elif v["verdict"] == "noise":
                out.append({"id": x["id"], "pos": x["pos"], "set": x["set"], "a": g["a"], "b": g["b"],
                            "op": g["op"], "ref": g["ref"], "action": "gold-noise",
                            "source": "grey-zone", "bucket": x["bucket"]})
        return out

    def mark_lines(self) -> list[dict]:
        out = []
        for x in self.items:
            v = self.verdicts.get(x["iid"])
            if x["bucket"] != "mark" and not (v and v["verdict"] == "mark"):
                continue
            if v and v["verdict"] in ("real", "noise"):
                continue  # 已裁为裁决结论的不进规则通道
            out.append({"id": x["id"], "pos": x["pos"], "set": x["set"], "char": x["char"],
                        "ctx": x["ctx"], "gold": x["gold"], "bucket": x["bucket"],
                        "action": "rule-candidate",
                        "asked": [k for k in ("asked21", "asked22") if x["sig"][k]]})
        return out


DATA: Data


def filtered_items(qs) -> list[dict]:
    its = DATA.items
    if (b := qs.get("bucket", [""])[0]):
        its = [x for x in its if x["bucket"] == b]
    if (s := qs.get("set", [""])[0]):
        its = [x for x in its if x["set"] == s]
    if (p := qs.get("part", [""])[0]):
        its = [x for x in its if x["partition"] == p]
    todo = qs.get("done", [""])[0]
    if todo == "0":
        its = [x for x in its if x["iid"] not in DATA.verdicts]
    elif todo == "1":
        its = [x for x in its if x["iid"] in DATA.verdicts]
    if qs.get("sample", [""])[0] == "1" and (b := qs.get("bucket", [""])[0]):
        n = int(qs.get("n", [str(GATE_MIN_REVIEWED)])[0])
        order = {x["iid"]: i for i, x in enumerate(its[:n])}
        its = [x for x in its if x["iid"] in order]
    return its


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def _send(self, code: int, body: bytes, ctype="application/json; charset=utf-8"):
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Cache-Control", "no-store")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        u = urlparse(self.path)
        qs = parse_qs(u.query)
        try:
            if u.path in ("/", "/index.html"):
                html = (Path(__file__).parent / "index.html").read_bytes()
                return self._send(200, html, "text/html; charset=utf-8")
            if u.path == "/api/state":
                return self._send(200, json.dumps({
                    "total": len(DATA.items),
                    "buckets": DATA.bucket_stats(),
                    "sets": sorted({x["set"] for x in DATA.items}),
                    "reviewed_total": len(DATA.verdicts),
                    "gate": {"min_reviewed": GATE_MIN_REVIEWED, "max_err": GATE_MAX_ERR},
                }, ensure_ascii=False).encode())
            if u.path == "/api/items":
                its = filtered_items(qs)
                off = int(qs.get("offset", ["0"])[0])
                lim = int(qs.get("limit", ["20"])[0])
                out = []
                for x in its[off:off + lim]:
                    y = dict(x)
                    y["verdict"] = DATA.verdicts.get(x["iid"])
                    out.append(y)
                return self._send(200, json.dumps({"total": len(its), "offset": off,
                                                   "items": out}, ensure_ascii=False).encode())
            if u.path == "/api/export":
                lines = [json.dumps(y, ensure_ascii=False) for y in DATA.export_lines()]
                return self._send(200, ("\n".join(lines) + "\n").encode(),
                                  "application/x-ndjson; charset=utf-8")
            if u.path == "/api/export-marks":
                lines = [json.dumps(y, ensure_ascii=False) for y in DATA.mark_lines()]
                return self._send(200, ("\n".join(lines) + "\n").encode(),
                                  "application/x-ndjson; charset=utf-8")
            return self._send(404, b'{"error":"404"}')
        except Exception as e:  # noqa: BLE001
            return self._send(500, json.dumps({"error": str(e)}).encode())

    def do_POST(self):
        u = urlparse(self.path)
        n = int(self.headers.get("Content-Length", "0"))
        try:
            body = json.loads(self.rfile.read(n) or b"{}")
            if u.path == "/api/verdict":
                DATA.record(body["iid"], body["verdict"], body.get("reason", ""))
                return self._send(200, b'{"ok":true}')
            if u.path == "/api/bulk":
                k = DATA.bulk(filtered_items(parse_qs(u.query)), body.get("verdict"),
                              body.get("reason", "批量"))
                return self._send(200, json.dumps({"done": k}).encode())
            if u.path == "/api/promote":
                DATA.promote(body["bucket"], body.get("note", ""))
                return self._send(200, b'{"ok":true}')
            return self._send(404, b'{"error":"404"}')
        except Exception as e:  # noqa: BLE001
            return self._send(400, json.dumps({"error": str(e)}).encode())


def main():
    global DATA
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--grey", default="~/tmp/grey_zone")
    ap.add_argument("--labels", default="~/tmp", help="含 dataset_v21/ dataset_v22/ 的目录")
    ap.add_argument("--state-dir", default="~/tmp/gold_adjudicate")
    ap.add_argument("--port", type=int, default=8766)
    a = ap.parse_args()
    DATA = Data(Path(a.grey).expanduser(), Path(a.labels).expanduser(), Path(a.state_dir).expanduser())
    bs = DATA.bucket_stats()
    print(f"灰区字符 {len(DATA.items):,} · " +
          " · ".join(f"{BUCKET_LABEL[b]} {bs[b]['total']:,}" for b in BUCKETS))
    print(f"已裁 {len(DATA.verdicts):,} · 升级 {len(DATA.promoted)} 桶")
    srv = ThreadingHTTPServer(("127.0.0.1", a.port), Handler)
    print(f"金集灰区裁决页 http://127.0.0.1:{a.port}")
    srv.serve_forever()


if __name__ == "__main__":
    main()
