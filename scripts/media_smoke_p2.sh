#!/usr/bin/env bash
# P2 契约基线冒烟：PDF（PdfCraft 批注）+ Word（WordCraft 修订 + 批注）回写闭环
#
# 证明 MULTIMODAL.md §11 P2 的骨架成立（Comment / InPlace+TrackChanges 两条路）：
#   md → pdf/docx → 定位错误短语 → 写原语批注/修订 → 存盘 → 读回断言
# 用法：[PDFCRAFT_CLI=… ] [WORDCRAFT_CLI=…] ./scripts/media_smoke_p2.sh
set -euo pipefail

PCF="${PDFCRAFT_CLI:-$HOME/.local/bin/pdfcraft-cli}"
WCF="${WORDCRAFT_CLI:-$HOME/.local/bin/wordcraft-cli}"
for b in "$PCF" "$WCF"; do command -v "$b" >/dev/null 2>&1 || { echo "FAIL: 找不到 $b" >&2; exit 1; }; done
fail() { echo "FAIL: $*" >&2; exit 1; }

D="$(mktemp -d "${TMPDIR:-/tmp}/langfixer-media-p2.XXXXXX")"
trap 'echo "工作目录：$D"' EXIT

cat > "$D/note.md" <<'MD'
# Proof

The He'llo wrold sample paragraph for LangFixer round trip.
MD

# ── PDF：Comment 模式（squiggly 拼写波浪线 + replace 替换批注）──
"$WCF" convert "$D/note.md" "$D/note.pdf" >/dev/null || fail "生成测试 PDF 失败"
cat > "$D/steps.json" <<JSON
[
  {"tool": "doc_open",  "args": {"path": "$D/note.pdf"}},
  {"tool": "text_find", "args": {"doc": 1, "query": "wrold"}},
  {"tool": "comment_add", "args": {"doc": 1, "page": 1, "type": "squiggly",
    "find": "wrold", "color": "red", "author": "LangFixer", "contents": "#1 spelling: wrold -> world"}},
  {"tool": "comment_add", "args": {"doc": 1, "page": 1, "type": "replace",
    "find": "He'llo", "color": "blue", "author": "LangFixer", "contents": "#2 spelling: He'llo -> Hello"}},
  {"tool": "comment_list", "args": {"doc": 1}},
  {"tool": "doc_save", "args": {"doc": 1, "path": "$D/result.pdf"}}
]
JSON
"$PCF" run --script "$D/steps.json" --root "$D" --report > "$D/pdf_report.json" 2>&1 \
  || fail "PdfCraft 批注回写失败：$(tail -3 "$D/pdf_report.json")"
python3 - "$D/pdf_report.json" <<'PY' || fail "PDF 断言失败"
import json, sys
# 实测 --report 结构：{"ok":bool,"steps":[{"step":n,"tool":id,"status":"ok","result":[{…}]}]}
# result 恒为「单元素数组」；text_find → matches[].rects（points，左上原点）
r = json.load(open(sys.argv[1]))
assert r.get("ok") is True, f"脚本步骤有失败：{r!r}"
steps = {s["tool"]: s for s in r["steps"]}
assert all(s["status"] == "ok" for s in r["steps"]), \
    f"存在非 ok 步骤：{[(s['tool'], s['status']) for s in r['steps'] if s['status'] != 'ok']}"
one = lambda tool: steps[tool]["result"][0]

tf = one("text_find")
assert tf["count"] >= 1 and tf["matches"][0]["rects"], f"text_find 未返回 rects：{tf!r}"

cl = one("comment_list")
comments = cl["comments"]
# 实测：comment_list 返回的类型首字母大写；comment_add type="replace" 落成
# 「StrikeOut 批注 + Caret 回复」两条（PDF 规范无 replace 注释类型）
types = sorted({c["type"] for c in comments})
assert "Squiggly" in types, f"波浪线批注未写入：{types}"
assert any(len(c.get("rect") or []) == 4 for c in comments), "批注未返回落点 rect（无法与 UI 联动）"
replies = [rp for c in comments for rp in (c.get("replies") or [])]
assert replies and replies[0]["contents"], f"replace 批注应带含文本的 Caret 回复：{comments!r}"
print(f"  PDF OK：text_find rects={tf['matches'][0]['rects'][0]} · "
      f"批注 {cl['count']} 条 {types} · 回复 {len(replies)} 条")
PY
[ -s "$D/result.pdf" ] || fail "result.pdf 未生成"

# ── Word：修订（Track Changes）+ 批注 ──
"$WCF" convert "$D/note.md" "$D/in.docx" >/dev/null || fail "生成测试 DOCX 失败"
"$WCF" run --file "$D/in.docx" --author LangFixer \
  --cmd 'review.trackChanges={"value":true}' \
  --cmd 'select.text={"text":"wrold"}' \
  --cmd 'text.insert={"text":"world"}' \
  --cmd 'select.text={"text":"He'"'"'llo"}' \
  --cmd 'review.newComment={"text":"#2 spelling: He'"'"'llo -> Hello"}' \
  --save "$D/result.docx" >/dev/null || fail "WordCraft 修订 + 批注失败"
"$WCF" info "$D/result.docx" > "$D/docx_info.json" 2>&1 || fail "读回 docx 摘要失败"
"$WCF" inspect "$D/result.docx" > "$D/docx_inspect.json" 2>&1 || fail "读回 docx 结构失败"
python3 - "$D/docx_info.json" "$D/docx_inspect.json" <<'PY' || fail "DOCX 断言失败"
import json, sys
info = json.load(open(sys.argv[1]))
insp = json.load(open(sys.argv[2]))
assert info.get("trackChanges") is True, f"修订未开启：{info!r}"
assert info.get("comments", 0) >= 1, f"批注未写入：{info!r}"
# 实测：select.text 选中后 text.insert 在修订模式下形成「ins run + del run」的跟踪替换；
# 段落 text 会把插入与删除的文本拼在一起，故校验必须看 runs 的 props，不能比 text
runs = [r for b in insp.get("blocks", []) for r in (b.get("runs") or [])]
ins = [r for r in runs if "ins" in (r.get("props") or {})]
dele = [r for r in runs if "del" in (r.get("props") or {})]
assert ins and dele, f"未见跟踪替换修订（ins/del run）：{runs!r}"
parts = [p for p in insp.get("parts", []) if p.get("kind") == "comment"]
assert any("spelling" in (p.get("text") or "") for p in parts), f"批注文本读不回：{parts!r}"
print(f"  DOCX OK：trackChanges=true · 批注 {info['comments']} 条 · "
      f"修订 run ins={len(ins)} del={len(dele)} · 批注文本可读回")
print("        （Word/WPS 打开可逐条接受/拒绝；word/comments.xml 为真 OOXML 批注）")
PY
[ -s "$D/result.docx" ] || fail "result.docx 未生成"

# ── OCR 能力探测（PdfCraft 内置，模型缺失时优雅降级）──
"$PCF" run ocr_status > "$D/ocr.json" 2>&1 || true
python3 - "$D/ocr.json" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
langs = [l["code"] for l in d.get("languages", [])]
print(f"  OCR：available={d.get('available')} languages={langs}"
      + ("（无 zh → 中文 OCR 必须走外部引擎）" if "zh" not in langs else ""))
PY

echo "PASS：PDF 批注 + Word 修订/批注 回写闭环可用"
