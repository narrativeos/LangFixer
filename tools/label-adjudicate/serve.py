#!/usr/bin/env python3
"""模型新增标注 · 分档抽检裁决页（label-adjudicate）

背景：dataset_v21 的 `model_errors` 中有「模型提出、金集没有」的新增标注，
按纪律（每桶 ≥58 条抽检通过）才能升级为可依赖标签。本工具把队列按
横向分型分档（auto=标点可规则复核 / must=语义高危 / audit=量大型按型抽审），
提供逐条裁决 + 桶级门槛 + 导出。

口径要点：
- 队列在启动时从 *_v21.jsonl **按逐条口径现算**（共识/新增以逐条重合判定），
  与 pi 已更正的「共识 12,789 / 新增 12,970」同口径；旧 review_queue 文件
  仅作对照，不作数据源（其数为累计口径，已废弃）。
- 逐条判定规则：行内 model_new==len(model_errors) → 全部新增；==0 → 全部共识；
  否则按字符区间与 gold_edits 的重合启发切分，并统计与行计数不一致的行数。
- 裁决 journal 追加落盘（done 必须落盘续跑——运维教训），重启即恢复。

用法：python3 serve.py [--data-dir ~/tmp/dataset_v21] [--state-dir 同上] [--port 8765]
"""
from __future__ import annotations

import argparse
import hashlib
import json
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlparse

BUCKET_OF_TYPE = {
    "punct": "auto",                      # 标点：规则可复核
    "fact": "must",                       # 事实/一致性/衔接：语义高危，逐条必审
    "consistency": "must",
    "cohesion": "must",
    "typo": "audit",                      # 量大型：按型抽审
    "grammar": "audit",
}
BUCKET_DESC = {
    "auto": "标点类——可被规则层复核；抽审通过后整桶自动采信",
    "must": "fact/consistency/cohesion——低量高危，逐条必审后才采信",
    "audit": "typo/grammar——量大型，按型随机抽审",
    "audit-other": "词表之外的类型码——并入抽审",
}
GATE_MIN_REVIEWED = 58   # 每桶最少抽检条数（≈95% CI 纪律，与 zh_route_1k 同源）
GATE_MAX_ERR = 0.02      # 抽检放弃率 ≤2% 才允许整桶升级
SEED = "42"
# 来源：rollout 报告的逐条口径（已更正版）。本工具同定义现算后若与它有小额差，
# 属共识↔新增边界归属差异（总标注数一致），页面上并排展示不静默。
REPORT_REF = {"new": 12970, "consensus": 12789}


def bucket_of(t: str) -> str:
    return BUCKET_OF_TYPE.get(t, "audit-other")


def overlaps(a: int, b: int, g: dict) -> bool:
    """逐条口径（与 label_rollout 字符集同定义、去掉累计 union）：
    模型字符集 range(a,b)（插入型为空→必为新增）× gold 字符集
    range(ga, max(gb, ga+1))（插入型占一字符位）是否相交。"""
    if a >= b:
        return False
    gb = max(g["b"], g["a"] + 1)
    return a < gb and g["a"] < b


class Data:
    def __init__(self, data_dir: Path, state_dir: Path):
        self.data_dir, self.state_dir = data_dir, state_dir
        self.items: list[dict] = []
        self.sum_consensus = 0
        self.rows_seen = 0
        self.count_mismatch_rows = 0
        self.sum_row_new = 0
        self.verdicts: dict[str, dict] = {}
        self.promoted: dict[str, dict] = {}
        self.lock = threading.Lock()
        self.state_dir.mkdir(parents=True, exist_ok=True)
        self._load()

    # ── 装载 ─────────────────────────────────────────────
    def _load(self):
        files = sorted(self.data_dir.glob("*_v21.jsonl"))
        if not files:
            sys.exit(f"未找到 {self.data_dir}/*_v21.jsonl")
        for fp in files:
            with open(fp, encoding="utf-8") as f:
                for line in f:
                    line = line.strip()
                    if line:
                        self._row(json.loads(line))
        jf = self.state_dir / "adjudications.jsonl"
        if jf.exists():
            with open(jf, encoding="utf-8") as f:
                for line in f:
                    if line.strip():
                        rec = json.loads(line)
                        self.verdicts[rec["iid"]] = rec
        pf = self.state_dir / "buckets_promoted.json"
        if pf.exists():
            self.promoted = json.loads(pf.read_text(encoding="utf-8"))

    def _row(self, row: dict):
        self.rows_seen += 1
        gold = row.get("gold_edits", [])
        errs = row.get("model_errors", [])
        # 逐条口径：每条 model_error 独立与 gold 判重合。行级 consensus/model_new
        # 字段是旧的累计口径（共识高估 3,685 / 新增低估同源），只对照不信任。
        new_flags = [
            not any(overlaps(e["a"], e["b"], g) for g in gold)
            for e in errs
        ]
        n_new = row.get("model_new") or 0
        self.sum_row_new += n_new
        self.sum_consensus += sum(1 for f in new_flags if not f)
        if sum(new_flags) != n_new:
            self.count_mismatch_rows += 1
        prov = row.get("provenance", {})
        for e, is_new in zip(errs, new_flags):
            if not is_new:
                continue
            self.items.append({
                "iid": f"{row['id']}#{e['a']}#{e['b']}",
                "id": row["id"],
                "set": row.get("set", ""),
                "src": row.get("src", ""),
                "a": e["a"],
                "b": e["b"],
                "type": e.get("type", "other"),
                "fix": e.get("fix", ""),
                "why": e.get("why", ""),
                "bucket": bucket_of(e.get("type", "other")),
                "gold_n": len(gold),
                "tier": row.get("tier", ""),
                "prompt_sha": prov.get("prompt_sha", ""),
                "labeler": prov.get("labeler", ""),
            })

    # ── 桶与门槛 ─────────────────────────────────────────
    def bucket_stats(self):
        out = {}
        for b in ("auto", "must", "audit", "audit-other"):
            its = [x for x in self.items if x["bucket"] == b]
            rev = [self.verdicts.get(x["iid"]) for x in its]
            rev = [r for r in rev if r]
            rejects = sum(1 for r in rev if r["verdict"] == "reject")
            uncertain = sum(1 for r in rev if r["verdict"] == "uncertain")
            p = self.promoted.get(b)
            out[b] = {
                "total": len(its),
                "by_type": _type_dist(its),
                "reviewed": len(rev),
                "rejected": rejects,
                "uncertain": uncertain,
                "err_rate": (rejects / len(rev)) if rev else 0.0,
                "gate_ok": len(rev) >= GATE_MIN_REVIEWED and rejects / max(len(rev), 1) <= GATE_MAX_ERR,
                "promoted_at": p.get("promoted_at") if p else None,
                "desc": BUCKET_DESC[b],
            }
        return out

    def sample_ids(self, bucket: str, n: int):
        its = [x for x in self.items if x["bucket"] == bucket]
        its.sort(key=lambda x: hashlib.md5(f"{SEED}|{x['iid']}".encode()).hexdigest())
        return [x["iid"] for x in its[:n]]

    def record(self, iid: str, verdict: str, reason: str):
        if not any(x["iid"] == iid for x in self.items):
            raise KeyError(iid)
        rec = {"iid": iid, "verdict": verdict, "reason": reason, "at": int(time.time())}
        with self.lock:
            with open(self.state_dir / "adjudications.jsonl", "a", encoding="utf-8") as f:
                f.write(json.dumps(rec, ensure_ascii=False) + "\n")
            self.verdicts[iid] = rec

    def promote(self, bucket: str, note: str):
        st = self.bucket_stats()[bucket]
        if not st["gate_ok"] or st["promoted_at"]:
            raise ValueError("门槛未满足或已升级")
        with self.lock:
            self.promoted[bucket] = {"promoted_at": int(time.time()), "note": note, "stats": st}
            (self.state_dir / "buckets_promoted.json").write_text(
                json.dumps(self.promoted, ensure_ascii=False, indent=1), encoding="utf-8")


def _type_dist(its):
    m: dict[str, int] = {}
    for x in its:
        m[x["type"]] = m.get(x["type"], 0) + 1
    return dict(sorted(m.items(), key=lambda kv: -kv[1]))


DATA: Data  # set in main()


def filtered_items(qs) -> list[dict]:
    bucket = qs.get("bucket", [""])[0]
    setf = qs.get("set", [""])[0]
    text = qs.get("q", [""])[0]
    status = qs.get("status", ["all"])[0]
    sample = qs.get("sample", [""])[0]
    its = DATA.items
    if bucket:
        its = [x for x in its if x["bucket"] == bucket]
    if setf:
        its = [x for x in its if x["set"] == setf]
    if text:
        its = [x for x in its if text in x["id"] or text in x["src"] or text in x["fix"]]
    if status == "todo":
        its = [x for x in its if x["iid"] not in DATA.verdicts]
    elif status == "done":
        its = [x for x in its if x["iid"] in DATA.verdicts]
    if sample and bucket:
        n = int(qs.get("n", [str(GATE_MIN_REVIEWED)])[0])
        order = {iid: i for i, iid in enumerate(DATA.sample_ids(bucket, n))}
        its = sorted((x for x in its if x["iid"] in order), key=lambda x: order[x["iid"]])
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
                bs = DATA.bucket_stats()
                return self._send(200, json.dumps({
                    "total_new": len(DATA.items),
                    "report_ref": REPORT_REF,
                    "total_labels": len(DATA.items) + DATA.sum_consensus,
                    "sum_row_model_new": DATA.sum_row_new,
                    "rows": DATA.rows_seen,
                    "count_mismatch_rows": DATA.count_mismatch_rows,
                    "buckets": bs,
                    "sets": sorted({x["set"] for x in DATA.items}),
                    "reviewed_total": len(DATA.verdicts),
                    "gate": {"min_reviewed": GATE_MIN_REVIEWED, "max_err": GATE_MAX_ERR},
                }, ensure_ascii=False).encode())
            if u.path == "/api/items":
                its = filtered_items(qs)
                off = int(qs.get("offset", ["0"])[0])
                lim = int(qs.get("limit", ["20"])[0])
                page = its[off:off + lim]
                out = []
                for x in page:
                    y = dict(x)
                    y["verdict"] = DATA.verdicts.get(x["iid"])
                    out.append(y)
                return self._send(200, json.dumps({"total": len(its), "offset": off, "items": out}, ensure_ascii=False).encode())
            if u.path == "/api/export":
                adopted = [x | {"verdict": DATA.verdicts[x["iid"]]} for x in DATA.items if
                           DATA.verdicts.get(x["iid"], {}).get("verdict") == "adopt"]
                lines = [json.dumps(x, ensure_ascii=False) for x in adopted]
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
            if u.path == "/api/promote":
                DATA.promote(body["bucket"], body.get("note", ""))
                return self._send(200, b'{"ok":true}')
            return self._send(404, b'{"error":"404"}')
        except Exception as e:  # noqa: BLE001
            return self._send(400, json.dumps({"error": str(e)}).encode())


def main():
    global DATA
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--data-dir", default="~/tmp/dataset_v21")
    ap.add_argument("--state-dir", default=None, help="裁决 journal 目录（默认 = data-dir）")
    ap.add_argument("--port", type=int, default=8765)
    a = ap.parse_args()
    DATA = Data(Path(a.data_dir).expanduser(), Path(a.state_dir or a.data_dir).expanduser())
    print(f"新增队列 {len(DATA.items)} 条（行新增计数和 {DATA.sum_row_new}，切分存疑行 {DATA.count_mismatch_rows}）")
    for b, s in DATA.bucket_stats().items():
        print(f"  {b:11s} {s['total']:6d}  {s['by_type']}")
    print(f"http://127.0.0.1:{a.port}  data={DATA.data_dir} state={DATA.state_dir}")
    ThreadingHTTPServer(("127.0.0.1", a.port), Handler).serve_forever()


if __name__ == "__main__":
    main()
