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

多人协作（产品级标准功能）：
- `--room-key` 房间口令：启用登录页，裁决记录逐条署名 `who`（HMAC 签名 cookie）；
  `--bind` 非本机时强制要求口令，编辑老师局域网内同地址登录。
- 每人每 iid 一票（同名重裁覆盖自己，不覆盖他人）；`--quorum K`（默认 1）：
  门槛只统计 ≥K 人有效裁决的条目，生效裁决=多数票，平票→存疑。
- 条目软领取（`/api/claim`，15 分钟 TTL）防两人同时裁一条；页内展示彼此票与领取状态。

用法：python3 serve.py [--data-dir ~/tmp/dataset_v22] [--state-dir 同上] [--port 8765]
      [--bind 127.0.0.1] [--room-key 口令] [--quorum 1] [--owner 房主]
支持 dataset_v2x 任意图本（从文件名尾缀识别，队列现算不信任行级字段）。
"""

import argparse
import hashlib
import hmac
import json
import os
import re
import sqlite3
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path
from typing import Dict, List, Optional, Tuple
from urllib.parse import parse_qs, unquote, urlparse

try:
    from http.server import ThreadingHTTPServer
except ImportError:  # python3.6（server 公网实例）
    from socketserver import ThreadingMixIn

    class ThreadingHTTPServer(ThreadingMixIn, HTTPServer):
        daemon_threads = True

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
CLAIM_TTL = 15 * 60      # 条目软领取过期（秒）
# 预筛信号（自动档的真实依据，不造规则）：
#   no_error    — fix==片段且 why 判「此处无误/保留」：判无错记录误入队列，可批量放弃
#   del_intent  — fix==片段但 why 讲「删除/多字/重复」：真错误、意图是删，导出归一为 delete
#   noop_contra — fix==片段且 why 讲实质改错：rollout 解析缺陷（fix 未置空/未取真修正），
#                 永不批量采纳，导出标 manual，待 v2.2 修归一化
#   lex_ok      — 被易混词典/punct 映射背书（词表小，命中即硬信号）
#   lex_contra  — 同片段词典另有权威修正：与模型矛盾，逐条看
#   lex_blind   — 全角标点互换：词典无判据，维持原桶
NO_ERROR_PAT = re.compile(r"无误|无错|没有错误|无需修改|无需调整|保留|原字正确|用字正确")
DEL_INTENT_PAT = re.compile(r"删|多余|冗余|多出|重复")
LEXICON_DEFAULT = Path(__file__).resolve().parents[2] / "lexicons" / "zh_gec_lexicon.json"


class Lex:
    def __init__(self, path: Path):
        self.conf_pairs: set = set()
        self.conf_char: dict = {}
        self.punct_equiv: set = set()
        self.punct_chars: set = set()
        if not path.exists():
            return
        lex = json.loads(path.read_text(encoding="utf-8"))
        for w, r, *_ in lex.get("confusables", []):
            self.conf_pairs.add((w, r))
            if len(w) == 1 and len(r) == 1:
                self.conf_char.setdefault(w, set()).add(r)
        for a, b in lex.get("punct_map", []):
            self.punct_equiv.add(frozenset((a, b)))
            self.punct_chars |= {a, b, chr(ord(a) - 0xFEE0) if "\uFF01" <= a <= "\uFF5E" else a}

    def signal(self, frag: str, fix: str, why: str, typ: str) -> str:
        if frag and frag == fix:
            why = why or ""
            if NO_ERROR_PAT.search(why):
                return "no_error"
            return "del_intent" if DEL_INTENT_PAT.search(why) else "noop_contra"
        if typ == "typo" and frag:
            if (frag, fix) in self.conf_pairs:
                return "lex_ok"
            alts = self.conf_char.get(frag)
            if alts:
                return "lex_ok" if fix in alts else "lex_contra"
            # 多字词条不做逐条背书（frag 只是词内一字，词级对映在条级别无判据）
        if typ == "punct":
            if frozenset((frag, fix)) in self.punct_equiv:
                return "lex_ok"
            if frag in self.punct_chars and fix in self.punct_chars:
                return "lex_blind"
        return ""
# 来源：rollout 报告的逐条口径（已更正版）。本工具同定义现算后若与它有小额差，
# 属共识↔新增边界归属差异（总标注数一致），页面上并排展示不静默。
REPORT_REF = {"v21": {"new": 12970, "consensus": 12789},   # rollout 更正后逐条口径
               "v22": {"new": 7376, "consensus": 10156}}  # 片段+第二通道配方


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
    def __init__(self, data_dir: Path, state_dir: Path, lexicon: Path,
                 owner: str = "房主", room_key: Optional[str] = None, quorum: int = 1,
                 token_db: Optional[str] = None):
        self.data_dir, self.state_dir = data_dir, state_dir
        self.owner, self.room_key, self.quorum = owner, room_key, max(1, quorum)
        self.token_db = Path(token_db).expanduser() if token_db else None
        self.lex = Lex(lexicon)
        self.items: List[dict] = []
        self.sum_consensus = 0
        self.rows_seen = 0
        self.count_mismatch_rows = 0
        self.sum_row_new = 0
        # iid -> {who: rec}：每人一票，同名重裁覆盖自己（追加式 journal 全量留痕）
        self.verdicts: Dict[str, Dict[str, dict]] = {}
        self.promoted: Dict[str, dict] = {}
        self.report_ref: dict = {}
        self.claims: Dict[str, Tuple[str, float]] = {}
        self.lock = threading.Lock()
        self.state_dir.mkdir(parents=True, exist_ok=True)
        self._load()

    def token(self, name: str) -> str:
        return hmac.new((self.room_key or "").encode(), name.encode(),
                        "sha256").hexdigest()[:16]

    # ── 网关 token 模式（沿用 api-server 同一张 access_tokens）──────
    def _secret(self) -> str:
        p = self.state_dir / "session_secret"
        if p.exists():
            return p.read_text(encoding="utf-8").strip()
        tok = os.urandom(16).hex()
        p.write_text(tok, encoding="utf-8")
        p.chmod(0o600)
        return tok

    def auth_token(self, token: str):
        """校验评审令牌：返回 (token_id, label)，废/不存在→None。"""
        if not self.token_db or not token:
            return None
        h = hashlib.sha256(token.encode()).hexdigest()
        con = sqlite3.connect(f"file:{self.token_db}?mode=ro", uri=True)
        try:
            return con.execute("select id,label from access_tokens where hash=? and revoked_at is null",
                               (h,)).fetchone()
        finally:
            con.close()

    def sid_label(self, token_id: str):
        """会话续签：令牌已吊销即失效（吊销即断，与网关一致）。"""
        con = sqlite3.connect(f"file:{self.token_db}?mode=ro", uri=True)
        try:
            row = con.execute("select label, revoked_at from access_tokens where id=?",
                              (token_id,)).fetchone()
        finally:
            con.close()
        return None if not row or row[1] else row[0]

    def make_sid(self, token_id: str) -> str:
        exp = int(time.time()) + 12 * 3600  # 与网关会话同 12h
        sig = hmac.new(self._secret().encode(), f"{token_id}|{exp}".encode(),
                       "sha256").hexdigest()[:32]
        return f"{token_id}.{exp}.{sig}"

    def check_sid(self, sid: str):
        """验签+验过期+验吊销；通过返回 (token_id, label)。"""
        try:
            token_id, exp, sig = sid.rsplit(".", 2)
            if int(exp) < time.time():
                return None
            want = hmac.new(self._secret().encode(), f"{token_id}|{exp}".encode(),
                            "sha256").hexdigest()[:32]
            if not hmac.compare_digest(sig, want):
                return None
            label = self.sid_label(token_id)
            return (token_id, label) if label else None
        except ValueError:
            return None

    # ── 装载 ─────────────────────────────────────────────
    def _load(self):
        files = sorted(f for f in self.data_dir.glob("*_v2*.jsonl") if "review_queue" not in f.name)
        if not files:
            sys.exit(f"未找到 {self.data_dir}/*_v2*.jsonl")
        m = re.search(r"_(v\w+)\.jsonl$", files[0].name)
        self.report_ref = REPORT_REF.get(m.group(1) if m else "", {})
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
                        who = rec.get("who") or self.owner  # 旧 journal 无署名→归房主
                        rec["who"] = who
                        self.verdicts.setdefault(rec["iid"], {})[who] = rec
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
            frag = row.get("src", "")[e["a"]:e["b"]]
            self.items.append({
                "iid": f"{row['id']}#{e['a']}#{e['b']}",
                "id": row["id"],
                "set": row.get("set", ""),
                "src": row.get("src", ""),
                "a": e["a"],
                "b": e["b"],
                "frag": frag,
                "signal": self.lex.signal(frag, e.get("fix", ""), e.get("why", ""), e.get("type", "")),
                "type": e.get("type", "other"),
                "fix": e.get("fix", ""),
                "why": e.get("why", ""),
                "bucket": bucket_of(e.get("type", "other")),
                "gold_n": len(gold),
                "tier": row.get("tier", ""),
                "prompt_sha": prov.get("prompt_sha", ""),
                "labeler": prov.get("labeler", ""),
            })

    # ── 多人裁决聚合与领取 ────────────────────────────
    def eff_verdict(self, iid: str) -> Tuple[Optional[str], int]:
        """生效裁决=多数票（平票→存疑）；返回 (verdict, 人数)。"""
        recs = self.verdicts.get(iid)
        if not recs:
            return None, 0
        votes = [r["verdict"] for r in recs.values()]
        mx = max(votes.count(v) for v in set(votes))
        top = [v for v in sorted(set(votes)) if votes.count(v) == mx]
        return (top[0] if len(top) == 1 else "uncertain"), len(recs)

    def claim(self, iid: str, who: str) -> Tuple[Optional[str], bool]:
        """软领取：返回 (当前持有人, 是否成功)。他人未过期领取则失败。"""
        with self.lock:
            c = self.claims.get(iid)
            if c and c[0] != who and time.time() - c[1] < CLAIM_TTL:
                return c[0], False
            self.claims[iid] = (who, time.time())
            return who, True

    def claim_of(self, iid: str) -> Optional[str]:
        c = self.claims.get(iid)
        if c and time.time() - c[1] < CLAIM_TTL:
            return c[0]
        return None

    def crew(self) -> dict:
        """每位裁决人的逐条计数（审计面板）。"""
        out: Dict[str, Dict[str, int]] = {}
        for recs in self.verdicts.values():
            for rec in recs.values():
                w = out.setdefault(rec["who"], {"total": 0, "adopt": 0, "reject": 0,
                                                "uncertain": 0, "latest": 0})
                w["total"] += 1
                w[rec["verdict"]] += 1
                w["latest"] = max(w["latest"], rec["at"])
        return out

    # ── 桶与门槛 ─────────────────────────────────────────
    def bucket_stats(self):
        out = {}
        for b in ("auto", "must", "audit", "audit-other"):
            its = [x for x in self.items if x["bucket"] == b]
            # 门槛只计 ≥quorum 人有效裁决的条目；生效裁决=多数票
            eff = [self.eff_verdict(x["iid"]) for x in its]
            rev = [v for v, n in eff if n >= self.quorum and v]
            rejects = sum(1 for v in rev if v == "reject")
            uncertain = sum(1 for v in rev if v == "uncertain")
            voters = sum(1 for _, n in eff if n > 0)
            p = self.promoted.get(b)
            out[b] = {
                "total": len(its),
                "by_type": _type_dist(its),
                "reviewed": len(rev),
                "any_voters": voters,
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

    def record(self, iid: str, verdict: str, reason: str, who: Optional[str] = None):
        if not any(x["iid"] == iid for x in self.items):
            raise KeyError(iid)
        if verdict not in ("adopt", "reject", "uncertain"):
            raise ValueError(f"bad verdict: {verdict}")
        rec = {"iid": iid, "verdict": verdict, "reason": reason,
               "who": who or self.owner, "at": int(time.time())}
        with self.lock:
            with open(self.state_dir / "adjudications.jsonl", "a", encoding="utf-8") as f:
                f.write(json.dumps(rec, ensure_ascii=False) + "\n")
            self.verdicts[iid] = rec

    PROTECTED_ADOPT = ("no_error", "del_intent", "noop_contra")  # fix==片段的三档，批量采纳一律跳过

    def bulk(self, its: list, verdict: str, reason: str, who: Optional[str] = None) -> int:
        """批量裁决当前筛选集（本人已裁跳过；fix==片段三档采纳保护，须逐条）。"""
        n = 0
        who = who or self.owner
        with self.lock:
            with open(self.state_dir / "adjudications.jsonl", "a", encoding="utf-8") as f:
                for x in its:
                    if who in self.verdicts.get(x["iid"], {}):
                        continue
                    if verdict == "adopt" and x["signal"] in self.PROTECTED_ADOPT:
                        continue
                    rec = {"iid": x["iid"], "verdict": verdict, "reason": reason,
                           "who": who, "at": int(time.time())}
                    f.write(json.dumps(rec, ensure_ascii=False) + "\n")
                    self.verdicts[x["iid"]] = rec
                    n += 1
        return n

    def promote(self, bucket: str, note: str):
        st = self.bucket_stats()[bucket]
        if not st["gate_ok"] or st["promoted_at"]:
            raise ValueError("门槛未满足或已升级")
        with self.lock:
            self.promoted[bucket] = {"promoted_at": int(time.time()), "note": note, "stats": st}
            (self.state_dir / "buckets_promoted.json").write_text(
                json.dumps(self.promoted, ensure_ascii=False, indent=1), encoding="utf-8")


def _signal_dist(its):
    m: Dict[str, int] = {}
    for x in its:
        m[x["signal"] or "none"] = m.get(x["signal"] or "none", 0) + 1
    return dict(sorted(m.items(), key=lambda kv: -kv[1]))


def _type_dist(its):
    m: Dict[str, int] = {}
    for x in its:
        m[x["type"]] = m.get(x["type"], 0) + 1
    return dict(sorted(m.items(), key=lambda kv: -kv[1]))


DATA: Data  # set in main()


def filtered_items(qs, who: Optional[str] = None) -> List[dict]:
    bucket = qs.get("bucket", [""])[0]
    setf = qs.get("set", [""])[0]
    text = qs.get("q", [""])[0]
    status = qs.get("status", ["all"])[0]
    signal = qs.get("signal", [""])[0]
    sample = qs.get("sample", [""])[0]
    its = DATA.items
    if bucket:
        its = [x for x in its if x["bucket"] == bucket]
    if signal:
        its = [x for x in its if (x["signal"] == "" if signal == "none" else x["signal"] == signal)]
    if setf:
        its = [x for x in its if x["set"] == setf]
    if text:
        its = [x for x in its if text in x["id"] or text in x["src"] or text in x["fix"]]
    if status == "todo":
        its = [x for x in its if who not in DATA.verdicts.get(x["iid"], {})]
    elif status == "done":
        its = [x for x in its if who in DATA.verdicts.get(x["iid"], {})]
    if sample and bucket:
        n = int(qs.get("n", [str(GATE_MIN_REVIEWED)])[0])
        order = {iid: i for i, iid in enumerate(DATA.sample_ids(bucket, n))}
        its = sorted((x for x in its if x["iid"] in order), key=lambda x: order[x["iid"]])
    return its


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def _send(self, code: int, body: bytes, ctype="application/json; charset=utf-8",
              extra: Optional[dict] = None):
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Cache-Control", "no-store")
        for k, v in (extra or {}).items():
            for vv in ([v] if isinstance(v, str) else v):
                self.send_header(k, vv)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    # ── 身份：网关 token 会话（lf_adj）或房间口令（neo_sid）─────
    def _who(self) -> Optional[str]:
        ck = self.headers.get("Cookie", "")
        if DATA.token_db:
            m = re.search(r"lf_adj=([^;]+)", ck)
            got = DATA.check_sid(m.group(1)) if m else None
            return got[1] if got else None
        if not DATA.room_key:
            return DATA.owner  # 单机模式：匿名即房主，行为与旧版一致
        m = re.search(r"neo_sid=([^;]+)", ck)
        if not m:
            return None
        tok, _, name = m.group(1).partition("|")
        name = unquote(name)
        return name if hmac.compare_digest(tok, DATA.token(name)) else None

    def do_GET(self):
        u = urlparse(self.path)
        qs = parse_qs(u.query)
        try:
            if u.path in ("/", "/index.html"):
                html = (Path(__file__).parent / "index.html").read_bytes()
                return self._send(200, html, "text/html; charset=utf-8")
            who = self._who()
            if u.path == "/api/whoami":
                return self._send(200, json.dumps(
                    {"who": who, "room": bool(DATA.room_key or DATA.token_db),
                     "token_mode": bool(DATA.token_db)}, ensure_ascii=False).encode())
            if DATA.room_key and who is None:
                return self._send(401, json.dumps({"error": "请先登录"}, ensure_ascii=False).encode())
            if u.path == "/api/state":
                bs = DATA.bucket_stats()
                return self._send(200, json.dumps({
                    "total_new": len(DATA.items),
                    "report_ref": DATA.report_ref,
                    "total_labels": len(DATA.items) + DATA.sum_consensus,
                    "sum_row_model_new": DATA.sum_row_new,
                    "rows": DATA.rows_seen,
                    "count_mismatch_rows": DATA.count_mismatch_rows,
                    "buckets": bs,
                    "sets": sorted({x["set"] for x in DATA.items}),
                    "signals": _signal_dist(DATA.items),
                    "reviewed_total": len(DATA.verdicts),
                    "gate": {"min_reviewed": GATE_MIN_REVIEWED, "max_err": GATE_MAX_ERR,
                             "quorum": DATA.quorum},
                    "crew": DATA.crew(),
                    "me": who,
                }, ensure_ascii=False).encode())
            if u.path == "/api/items":
                its = filtered_items(qs, who)
                off = int(qs.get("offset", ["0"])[0])
                lim = int(qs.get("limit", ["20"])[0])
                page = its[off:off + lim]
                out = []
                for x in page:
                    y = dict(x)
                    recs = DATA.verdicts.get(x["iid"], {})
                    y["mine"] = recs.get(who)
                    y["votes"] = sorted(recs.values(), key=lambda r: r["at"])
                    ev, n = DATA.eff_verdict(x["iid"])
                    y["eff"], y["nvotes"] = ev, n
                    cq = DATA.claim_of(x["iid"])
                    y["claim"] = cq if cq and cq != who else None
                    out.append(y)
                return self._send(200, json.dumps({"total": len(its), "offset": off, "items": out}, ensure_ascii=False).encode())
            if u.path == "/api/export":
                adopted = []
                for x in DATA.items:
                    ev, _n = DATA.eff_verdict(x["iid"])
                    if ev != "adopt":
                        continue
                    y = x | {"verdict": DATA.verdicts[x["iid"]], "eff": ev}
                    if x["frag"] and x["frag"] == x["fix"]:
                        # fix==原文的采纳：删除意图归一为 delete，其余标 manual 待修
                        y["action"] = "delete" if x["signal"] == "del_intent" else "manual"
                        if y["action"] == "delete":
                            y["fix"] = ""
                    adopted.append(y)
                lines = [json.dumps(x, ensure_ascii=False) for x in adopted]
                return self._send(200, ("\n".join(lines) + "\n").encode(),
                                  "application/x-ndjson; charset=utf-8")
            return self._send(404, b'{"error":"404"}')
        except Exception as e:  # noqa: BLE001
            return self._send(500, json.dumps({"error": str(e)}).encode())

    def do_HEAD(self):
        u = urlparse(self.path)
        if u.path in ("/", "/index.html"):
            n = (Path(__file__).parent / "index.html").stat().st_size
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Cache-Control", "no-store")
            self.send_header("Content-Length", str(n))
            return self.end_headers()
        self.send_response(200)
        self.send_header("Cache-Control", "no-store")
        self.send_header("Content-Length", "0")
        self.end_headers()

    def do_POST(self):
        u = urlparse(self.path)
        n = int(self.headers.get("Content-Length", "0"))
        try:
            body = json.loads(self.rfile.read(n) or b"{}")
            if u.path == "/api/login":
                if DATA.token_db:  # 网关形态：一人一枚评审令牌，姓名即 label
                    got = DATA.auth_token(str(body.get("token", "")))
                    if not got:
                        return self._send(403, json.dumps({"error": "令牌无效或已吊销"}, ensure_ascii=False).encode())
                    sid = DATA.make_sid(got[0])
                    ck = f"lf_adj={sid}; Path=/; Max-Age=43200; HttpOnly; SameSite=Lax"
                    return self._send(200, json.dumps({"who": got[1]}).encode(),
                                      extra={"Set-Cookie": ck})
                name = (body.get("name") or "").strip()[:24]
                if not name or not DATA.room_key:
                    raise ValueError("参数不足")
                if not hmac.compare_digest(str(body.get("key", "")), DATA.room_key):
                    return self._send(403, json.dumps({"error": "口令不对"}, ensure_ascii=False).encode())
                from urllib.parse import quote
                tok = DATA.token(name)
                ck = f"neo_sid={tok}|{quote(name)}; Path=/; Max-Age=86400; HttpOnly; SameSite=Lax"
                return self._send(200, json.dumps({"who": name}).encode(),
                                  extra={"Set-Cookie": ck})
            who = self._who()
            if DATA.room_key and who is None:
                return self._send(401, json.dumps({"error": "请先登录"}, ensure_ascii=False).encode())
            if u.path == "/api/logout":
                return self._send(200, b'{"ok":true}',
                                  extra={"Set-Cookie": ["neo_sid=; Path=/; Max-Age=0",
                                                        "lf_adj=; Path=/; Max-Age=0"]})
            if u.path == "/api/claim":
                holder, ok = DATA.claim(body["iid"], who)
                return self._send(200, json.dumps({"claim": holder, "ok": ok}).encode())
            if u.path == "/api/verdict":
                DATA.record(body["iid"], body["verdict"], body.get("reason", ""), who)
                return self._send(200, b'{"ok":true}')
            if u.path == "/api/bulk":
                n = DATA.bulk(filtered_items(parse_qs(u.query), who), body["verdict"],
                              body.get("reason", "预筛批量"), who)
                return self._send(200, json.dumps({"done": n}).encode())
            if u.path == "/api/promote":
                DATA.promote(body["bucket"], body.get("note", ""))
                return self._send(200, b'{"ok":true}')
            return self._send(404, b'{"error":"404"}')
        except Exception as e:  # noqa: BLE001
            return self._send(400, json.dumps({"error": str(e)}).encode())


def main():
    global DATA
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--data-dir", default="~/tmp/dataset_v22")
    ap.add_argument("--state-dir", default=None, help="裁决 journal 目录（默认 = data-dir）")
    ap.add_argument("--lexicon", default=str(LEXICON_DEFAULT))
    ap.add_argument("--port", type=int, default=8765)
    ap.add_argument("--bind", default="127.0.0.1", help="非本机绑定时必须 --room-key（局域网多人）")
    ap.add_argument("--room-key", default=None, help="房间口令：局域网协作用（与 --token-db 二选一）")
    ap.add_argument("--token-db", default=None,
                    help="网关 auth.db：用 access_tokens 登录（一人一枚令牌，署名=label）")
    ap.add_argument("--owner", default="房主", help="无署名旧 journal 的归属名")
    ap.add_argument("--quorum", type=int, default=1, help="门槛条目所需最少裁决人数（多人模式建议 2）")
    a = ap.parse_args()
    if a.bind not in ("127.0.0.1", "localhost", "::1") and not (a.room_key or a.token_db):
        sys.exit("非本机绑定必须 --room-key 或 --token-db，否则裁决无法署名/无法防冲突")
    DATA = Data(Path(a.data_dir).expanduser(), Path(a.state_dir or a.data_dir).expanduser(),
                Path(a.lexicon).expanduser(), owner=a.owner, room_key=a.room_key,
                quorum=a.quorum, token_db=a.token_db)
    print(f"新增队列 {len(DATA.items)} 条（行新增计数和 {DATA.sum_row_new}，切分存疑行 {DATA.count_mismatch_rows}）")
    for b, s in DATA.bucket_stats().items():
        print(f"  {b:11s} {s['total']:6d}  {s['by_type']}")
    mode = "网关令牌模式（沿用 access_tokens）" if a.token_db else ("多人登录模式" if a.room_key else "单机模式（匿名=房主）")
    print(f"{mode} quorum={a.quorum}  http://{a.bind}:{a.port}  data={DATA.data_dir} state={DATA.state_dir}")
    if a.room_key:
        import socket
        lan = socket.gethostbyname(socket.gethostname()) if a.bind == "0.0.0.0" else a.bind
        print(f"编辑老师访问地址： http://{lan}:{a.port}")
    ThreadingHTTPServer((a.bind, a.port), Handler).serve_forever()


if __name__ == "__main__":
    main()
