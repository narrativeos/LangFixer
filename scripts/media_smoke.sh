#!/usr/bin/env bash
# 多模态审校「载体骨架」冒烟测试（不依赖 api-server，直接驱动 PhotoCraft CLI）
#
# 证明 MULTIMODAL.md §11 P0 的骨架成立：
#   造含文字层的 PSD → 读文本（A 档，无需 OCR）→ 无损改字（type.edit）
#   → 叠加标注（shape + type，收进 LangFixer Marks 组）→ 出预览 → 读回断言
# 这是 P0 实现完成前的**契约基线**：任何 adapter 实现都必须能让这些命令通过。
#
# 前提：photocraft-cli 已安装（默认 ~/.local/bin/photocraft-cli，或 PHOTOCRAFT_CLI 指定路径）。
# 用法：[PHOTOCRAFT_CLI=/path/to/photocraft-cli] ./scripts/media_smoke.sh
set -euo pipefail

CANDIDATES=("${PHOTOCRAFT_CLI:-}" "photocraft-cli" "$HOME/.local/bin/photocraft-cli" \
  "/usr/local/bin/photocraft-cli" "/opt/homebrew/bin/photocraft-cli")
PC=""
for c in "${CANDIDATES[@]}"; do
  [ -n "$c" ] && command -v "$c" >/dev/null 2>&1 && { PC="$(command -v "$c")"; break; }
done
[ -n "$PC" ] || { echo "FAIL: 找不到 photocraft-cli（装它或设 PHOTOCRAFT_CLI）" >&2; exit 1; }

fail() { echo "FAIL: $*" >&2; exit 1; }
D="$(mktemp -d "${TMPDIR:-/tmp}/langfixer-media-smoke.XXXXXX")"
trap 'echo "工作目录：$D"' EXIT

echo "CLI: $PC ($("$PC" --version 2>&1 | head -1))"

# ── 1) 造一个含文字层的 PSD（正文故意含错：He'llo wrold）──
"$PC" run --new '{"width":900,"height":300,"background":"white","name":"poster"}' \
  --cmd type.create --params '{"x":60,"y":120,"text":"He'"'"'llo wrold","size":48,"color":"#111111","name":"headline"}' \
  --out "$D/in.psd" >/dev/null || fail "步骤 1：创建带文字层的 PSD 失败"

# ── 2) A 档取文本：info 读文字层（文本 + bounds + layer id）──
INFO="$("$PC" info "$D/in.psd" --compact)" || fail "步骤 2：info 读取失败"
LID="$(printf '%s' "$INFO" | python3 -c '
import sys, json
d = json.load(sys.stdin)
def walk(ls):
    for l in ls:
        if l.get("kind") == "Type":
            print(l["id"]); return
        walk(l.get("children") or [])
walk(d.get("layers") or [])')"
[ -n "$LID" ] || fail "步骤 2：未从 info 中读到文字层（A 档不可用）"
printf '%s' "$INFO" | python3 -c '
import sys, json
d = json.load(sys.stdin)
def find(ls):
    for l in ls:
        if l.get("kind") == "Type":
            return l
        r = find(l.get("children") or [])
        if r: return r
    return None
t = find(d["layers"])["text"]["text"]
assert "He'"'"'llo wrold" == t, f"读回的文本不符：{t!r}"' || fail "步骤 2：文字层文本读回不正确"
echo "  A 档取文本 OK：layer=$LID text=\"He'llo wrold\""

# ── 3) 回写：无损改字 + 叠加标注 + 入组 ──
# 坑（实测）：layer.new.group 建的是空组，后续创建的层不会自动进组；
# 必须用 layer.moveTo {position:"into"} 把它们移进去。
"$PC" run "$D/in.psd" \
  --cmd type.edit --params "{\"layer\":$LID,\"replace\":{\"start\":0,\"end\":6,\"text\":\"Hello\"}}" \
  --cmd layer.new.group --params '{"name":"LangFixer Marks"}' \
  --cmd shape.create --params '{"kind":"rect","rect":[52,72,340,64],"fill":null,"stroke":{"width":3,"color":"#e03131"},"name":"mark-1-box"}' \
  --cmd type.create --params '{"x":56,"y":200,"text":"#1 spelling","size":20,"color":"#e03131","font":"Heiti SC","name":"mark-1-text"}' \
  --out "$D/out.psd" >/dev/null || fail "步骤 3：改字 + 标注失败"

MARKS="$("$PC" info "$D/out.psd" --compact | python3 -c '
import sys, json
d = json.load(sys.stdin)
ids = []
def walk(ls):
    for l in ls:
        if str(l.get("name", "")).startswith("mark-"):
            ids.append(l["id"])
        walk(l.get("children") or [])
walk(d.get("layers") or [])
print(json.dumps(ids))')"
GROUP="$("$PC" info "$D/out.psd" --compact | python3 -c '
import sys, json
d = json.load(sys.stdin)
def find(ls):
    for l in ls:
        if l.get("name") == "LangFixer Marks":
            return l["id"]
        r = find(l.get("children") or [])
        if r: return r
    return ""
print(find(d.get("layers") or []))')"
[ -n "$GROUP" ] || fail "步骤 3：未找到 LangFixer Marks 组"
"$PC" run "$D/out.psd" --cmd layer.moveTo \
  --params "{\"layers\":$MARKS,\"target\":$GROUP,\"position\":\"into\"}" \
  --out "$D/result.psd" >/dev/null || fail "步骤 3：标注入组失败"

# ── 4) 预览（扁平 PNG；实测会 flatten，故必须与 .psd 双产物）──
"$PC" convert "$D/result.psd" "$D/result.png" 2>/dev/null || fail "步骤 4：预览导出失败"

# ── 5) 读回断言：正文已改 + 组内含 2 个标注层 ──
"$PC" info "$D/result.psd" --compact | python3 -c '
import sys, json
d = json.load(sys.stdin)
def find(ls, pred):
    for l in ls:
        if pred(l): return l
        r = find(l.get("children") or [], pred)
        if r: return r
    return None
head = find(d["layers"], lambda l: l.get("name") == "headline")
txt = head["text"]["text"]
assert txt == "Hello wrold", f"改字未生效：{txt!r}"
st = head["text"]
assert st["sizePt"] == 48.0, "改字后字号应保持不变（无损回写的前提）"
g = find(d["layers"], lambda l: l.get("name") == "LangFixer Marks")
kids = [c["name"] for c in (g.get("children") or [])]
assert len(kids) == 2, f"标注层未全部入组：{kids}"
print("  回写 OK：正文=\"Hello wrold\"（字体/字号保留）· 组内标注层=", kids)' \
  || fail "步骤 5：读回断言失败"

echo "PASS：载体 → 取文本 → 检测前处理 → 回写 → 预览 全链路可用"
echo "产物：$D/result.psd（可编辑）· $D/result.png（预览）"
