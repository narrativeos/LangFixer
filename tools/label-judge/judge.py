#!/usr/bin/env python3
"""SuperVL 二审预筛裁判（§11.14 补七后续：抽检半自动化，人工只复核存疑与分歧）。

独立性论证：标注器=Qwen3.8（本机 vLLM），裁判=nvidia Nemotron Super-VL 系
（非同族、未见过 v2* 标注管线），避开「同模型自审」循环；污染纪律不变——
v2* 仍不得用于评测 Qwen 同族。

纪律（不可违反）：
- 裁判只写 `judge_verdicts.jsonl`（独立 journal，追加式，重启续跑）；
  绝不写 adjudications.jsonl——**gold 升级仍须人工确认**，裁判只是把
  人裁量压到「存疑档 + 每桶审计」。
- 校准先行：先在每桶 ≥58（种子 42，与裁决页 sample_ids 同口径）的闸门
  样本上跑裁判，人工抽审该闸门样本算「裁判-人工一致率」，≥95% 才放量。
- 裁判不给 gold 看（队列定义即非共识），避免锚定；不给其他裁判结果。

用法：
  cd LangFixer
  LLM_BASE_URL=… LLM_API_KEY=… \
    python3 tools/label-judge/judge.py calibrate            # 先跑闸门样本
  python3 tools/label-judge/judge.py report                 # 裁判 vs 人工 一致率
  python3 tools/label-judge/judge.py run --bucket audit     # 校准达标后放量
"""
from __future__ import annotations

import argparse
import json
import os
import sys
import threading
import time
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "tools" / "label-adjudicate"))
import serve as AJ  # noqa: E402  复用队列现算/分桶/种子抽样（不另开口径）

MODEL = os.environ.get("JUDGE_MODEL", "SuperVL")
JUDGE_TAG = "supervl-120b"

# v1 口径（「该处确有错误才 adopt」+宁缺勿滥）与人工金标实测一致率仅 50%，
# 29 条分歧里 18 条是裁判 reject/人工 adopt——裁判把规范化修复（半角→全角、
# 补配对引号、补结构助词等）按「风格偏好」打回。v2 对齐人工口径：判修复本身，
# 不判原文是否必错；gold 升级仍须人工确认，裁判只是预筛，两类误判代价对称。
PROMPT = """你是中文语法错误标注的质检裁判。另一名标注员在下面原文片段处提出了一处「错误+修正」。你的任务是判断这个**修正本身**是否成立。只输出一个 JSON，不要其他文字。

原文如下，〖〗内即被标记片段的确切位置（注意：〖〗是定位符，不是原文内容）：
{src}
被标记片段：「{frag}」
标注员判定：类型={typ}，建议修正为「{fix}」{empty_note}，理由：{why}

判定标准（对齐人工金标口径）：
- adopt：修正方向正确且完整，修完后不劣于原文。规范化与可读性改善（半角→全角标点、补全配对引号、顿号/逗号误用调整、补结构助词「的」等）即使原文算可接受表达，也判 adopt；
- reject：修正本身有问题——把原本正确的地方改错、修正方向相反（如把右引号改成左引号）、修复不完整会引入新错位、答非所问；
- uncertain：仅凭该片段无法判断（片段疑似截断、需看整句/标题还是正文、需外部事实或领域知识）。

注意：你是预筛裁判，最终是否进 gold 由人工逐条确认，误拦好修正与误放行噪声代价对称。请判「这个修正好不好」，而不是「原文是否一定是错的」。

输出格式：{{"verdict":"adopt|reject|uncertain","reason":"一句话依据"}}"""


def marked_src(item: dict) -> str:
    """v3：把被标记范围直接括进原文。v2 实测两类错拦（引号/问号盯错重复字符位置）
    皆因原文与片段分开给、模型对位失败；〖〗就地定位修掉这类错案根因。"""
    s, a, b = item["src"], item["a"], item["b"]
    return s[:a] + "〖" + s[a:b] + "〗" + s[b:]

_lock = threading.Lock()


def call_judge(base: str, key: str, item: dict, timeout: int = 90) -> tuple[str, str]:
    """返回 (verdict, reason)；解析失败抛异常，由重试兜住。"""
    # thinking_budget 实测：默认无限思考 13s/条且卡线超 120s 超时；预算 1024
    # 后 7.4s/条，三种档位判决逐位一致（§11.14 补七后续实验）
    body = json.dumps({
        "model": MODEL,
        "temperature": 0,
        "chat_template_kwargs": {"thinking": True, "thinking_budget": 1024},
        "messages": [{"role": "user", "content": PROMPT.format(
            src=marked_src(item), frag=item["frag"] or "（插入位）",
            typ=item["type"], fix=item["fix"],
            empty_note="（此处 fix 为空字符串，即建议直接删除该片段，不是改写成其他文字）"
            if item["fix"] == "" else "",
            why=item["why"])}],
    }).encode("utf-8")
    req = urllib.request.Request(
        base.rstrip("/") + "/chat/completions", data=body,
        headers={"Content-Type": "application/json",
                 "Authorization": f"Bearer {key}"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        out = json.loads(r.read().decode("utf-8"))
    txt = (out["choices"][0]["message"].get("content") or "").strip()
    i, j = txt.find("{"), txt.rfind("}")
    v = json.loads(txt[i:j + 1])
    if v.get("verdict") not in ("adopt", "reject", "uncertain"):
        raise ValueError(f"bad verdict: {txt[:120]}")
    return v["verdict"], v.get("reason", "")


class Journal:
    def __init__(self, path: Path):
        self.path, self.lock = path, threading.Lock()
        self.done: set[str] = set()
        if path.is_file():
            for l in path.read_text(encoding="utf-8").splitlines():
                if l.strip():
                    self.done.add(json.loads(l)["iid"])

    def append(self, rec: dict) -> None:
        with self.lock:
            with open(self.path, "a", encoding="utf-8") as f:
                f.write(json.dumps(rec, ensure_ascii=False) + "\n")


def judge_items(base: str, key: str, jr: Journal, items: list[dict],
                workers: int) -> dict:
    todo = [x for x in items if x["iid"] not in jr.done]
    print(f"待裁 {len(todo)}/{len(items)} 条（journal 已存 {len(jr.done)}）→ {MODEL}")
    t0, cnt = time.time(), [0]

    def one(x: dict) -> None:
        for attempt in range(4):
            try:
                verdict, reason = call_judge(base, key, x)
                rec = {"iid": x["iid"], "id": x["id"], "bucket": x["bucket"],
                       "type": x["type"], "signal": x["signal"],
                       "judge": JUDGE_TAG, "verdict": verdict, "reason": reason,
                       "at": int(time.time())}
                jr.append(rec)
                with _lock:
                    cnt[0] += 1
                    if cnt[0] % 25 == 0:
                        print(f"  {cnt[0]}/{len(todo)}（{time.time()-t0:.0f}s）")
                return
            except Exception as e:  # noqa: BLE001
                if attempt == 3:
                    with _lock:
                        print(f"WARN {x['iid']} 裁判失败 3 次：{e}")
                else:
                    time.sleep(5 * (attempt + 1))

    with ThreadPoolExecutor(max_workers=workers) as ex:
        list(ex.map(one, todo))
    print(f"本轮 {cnt[0]} 条，耗时 {(time.time()-t0)/60:.1f} 分钟")
    return {}


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("mode", choices=["calibrate", "run", "report"])
    ap.add_argument("--data-dir", default="~/tmp/dataset_v23")
    ap.add_argument("--state-dir", default=None)
    ap.add_argument("--lexicon", default=str(AJ.LEXICON_DEFAULT))
    ap.add_argument("--bucket", default=None, help="run 模式限定桶")
    ap.add_argument("--workers", type=int, default=4)
    a = ap.parse_args()

    base = os.environ.get("LLM_BASE_URL")
    key = os.environ.get("LLM_API_KEY", "")
    if a.mode in ("calibrate", "run") and not base:
        sys.exit("需 LLM_BASE_URL / LLM_API_KEY 环境变量")

    data_dir = Path(a.data_dir).expanduser()
    state_dir = Path(a.state_dir or a.data_dir).expanduser()
    data = AJ.Data(data_dir, state_dir, Path(a.lexicon).expanduser())
    jr = Journal(state_dir / "judge_verdicts.jsonl")
    by_iid = {x["iid"]: x for x in data.items}

    if a.mode == "calibrate":
        gate = AJ.GATE_MIN_REVIEWED
        sel = [iid for b in ("auto", "must", "audit")
               for iid in data.sample_ids(b, gate)]
        print(f"校准口径：每桶 ≥{gate} 条（种子 {AJ.SEED}，与裁决页同抽法）")
        judge_items(base, key, jr, [by_iid[i] for i in sel], a.workers)
        print("下一步：人工在裁决页裁决这些同一批闸门样本，然后跑 report 算一致率。")
    elif a.mode == "run":
        items = [x for x in data.items if not a.bucket or x["bucket"] == a.bucket]
        judge_items(base, key, jr, items, a.workers)
    else:  # report：裁判-人工共识一致率 + 人人一致率（闸门样本上算）
        votes: dict[str, dict[str, str]] = {}   # iid -> {who: verdict}（同名重裁覆盖自己）
        p = state_dir / "adjudications.jsonl"
        if p.is_file():
            for l in p.read_text(encoding="utf-8").splitlines():
                if l.strip():
                    r = json.loads(l)
                    votes.setdefault(r["iid"], {})[r.get("who", "房主")] = r["verdict"]

        def consensus(vs: dict) -> str:
            arr = list(vs.values())
            mx = max(arr.count(v) for v in set(arr))
            top = [v for v in sorted(set(arr)) if arr.count(v) == mx]
            return top[0] if len(top) == 1 else "uncertain"

        # 人人一致率：两两裁决人在共同裁过的条目上逐位一致（含 adopt/非adopt 方向一致）
        whos = sorted({w for vs in votes.values() for w in vs})
        if len(whos) >= 2:
            import itertools
            print("人人一致率（同条目两人都裁过）：")
            for a, b in itertools.combinations(whos, 2):
                both = [i for i, vs in votes.items() if a in vs and b in vs]
                if not both:
                    print(f"  {a} vs {b}: 无交集"); continue
                exact = sum(1 for i in both if votes[i][a] == votes[i][b])
                direc = sum(1 for i in both
                            if (votes[i][a] == "adopt") == (votes[i][b] == "adopt"))
                print(f"  {a} vs {b}: 交集 {len(both):3d} · 逐位 {exact/len(both):.1%} · "
                      f"采纳方向 {direc/len(both):.1%}")
        human = {i: consensus(vs) for i, vs in votes.items()}
        agree = tot = 0
        disagree, unpicked = [], []
        per_bucket: dict[str, dict] = {}
        for jf in (state_dir / "judge_verdicts.jsonl",):
            if not jf.is_file():
                sys.exit("无 judge_verdicts.jsonl——先 calibrate")
            for l in jf.read_text(encoding="utf-8").splitlines():
                if not l.strip():
                    continue
                r = json.loads(l)
                hv = human.get(r["iid"])
                b = per_bucket.setdefault(r["bucket"],
                                          {"judged": 0, "human": 0, "agree": 0})
                b["judged"] += 1
                if hv:
                    b["human"] += 1
                    tot += 1
                    if hv == r["verdict"]:
                        agree += 1
                        b["agree"] += 1
                    else:
                        disagree.append((r["iid"], r["bucket"], r["type"],
                                         r["verdict"], hv, r["reason"]))
        rate = agree / tot if tot else 0.0
        print(f"人工已裁 ∩ 裁判 = {tot} 条；一致 {agree}（{rate:.1%}）"
              f"{' ✅ ≥95%，可放量' if rate >= 0.95 and tot else ' ❌ <95%，先复盘分歧'}")
        for b, s in per_bucket.items():
            print(f"  {b:11s} judged {s['judged']:4d}  human∩ {s['human']:3d}  "
                  f"agree {s['agree']:3d}")
        if disagree:
            print("分歧清单（裁判/人工）：")
            for iid, b, t, jv, hv, rs in disagree[:40]:
                print(f"  {iid} [{b}/{t}] 裁判={jv} 人工={hv} · {rs[:60]}")


if __name__ == "__main__":
    main()
