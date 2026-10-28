#!/usr/bin/env bash
# 专家正误表（docx 表格）→ datasets/media_gold.json
#
# 多模态审校的验收金集：图片是「原图」，docx 是专家给的判定（正误表）。
# 表格六列：序号 | 页码及位置 | 错误或待核原文 | 正确或处理建议 | 计错数 | 备注及判定依据
# 「计错数」为「不计」的条目 = 表达优化/表述不清（真实缺陷但不计硬差错），打分权重 0.5。
#
# 用法：
#   ./scripts/media_gold_extract.sh "<docx 路径>" [输出 json]
# 例：
#   ./scripts/media_gold_extract.sh "$HOME/Documents/校对测试数据1/住院告知文字质检正误表.docx"
set -euo pipefail

DOCX="${1:-}"
OUT="${2:-datasets/media_gold.json}"
[ -n "$DOCX" ] && [ -f "$DOCX" ] || { echo "用法：$0 <正误表.docx> [输出 json]" >&2; exit 1; }

python3 - "$DOCX" "$OUT" <<'PY'
import json, os, sys, zipfile
from xml.etree import ElementTree as ET

docx, out = sys.argv[1], sys.argv[2]
W = '{http://schemas.openxmlformats.org/wordprocessingml/2006/main}'
root = ET.fromstring(zipfile.ZipFile(docx).read('word/document.xml'))
table = next(root.iter(W + 'tbl'))

rows = []
for tr in table.findall(W + 'tr'):
    cells = []
    for tc in tr.findall(W + 'tc'):
        paras = [''.join(n.text or '' for n in p.iter(W + 't')) for p in tc.iter(W + 'p')]
        cells.append('\n'.join(p.strip() for p in paras).strip())
    rows.append(cells)

items = []
for r in rows:
    if not r or not r[0].strip().isdigit():
        continue                      # 表头与空行
    note = r[5] if len(r) > 5 else ''
    tag = next((p.lstrip('【') for p in note.split('】') if p.startswith('【')), '')
    counted = (r[4].strip() if len(r) > 4 else '') != '不计'
    items.append({
        'no': int(r[0]),
        'location': r[1],
        'original': r[2],
        'suggestion': r[3],
        'counted': counted,
        'verdict': tag or ('差错' if counted else '未定'),
        'note': note,
    })

gold = {
    'lang': 'zh',
    'version': '1.0',
    'description': (
        '多模态审校金集：载体为住院告知类图片，判定来自专家正误表（docx 表格）。'
        '「不计」条目（表达优化 / 表述不清）仍计入召回，权重 0.5。'
    ),
    'source_docx': os.path.basename(docx),
    'extracted_by': 'scripts/media_gold_extract.sh',
    'carriers': [],   # 由人工补充：文件名 / 标题 / 尺寸 / 有无文字层（原图为扁平位图时只能走视觉档）
    'items': items,
}
with open(out, 'w', encoding='utf-8') as f:
    json.dump(gold, f, ensure_ascii=False, indent=2)
    f.write('\n')
print(f"OK：{len(items)} 条金标 → {out}"
      f"（计差错 {sum(1 for i in items if i['counted'])} / 不计 {sum(1 for i in items if not i['counted'])}）")
PY
