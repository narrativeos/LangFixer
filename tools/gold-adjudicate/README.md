# 金集灰区裁决页（gold-adjudicate）

对 v2.2 未命中的 **21,257** 个 gold 字符（`~/tmp/grey_zone/gold_grey_chars.jsonl`，
并集口径，与 `dataset_v22/stats.json` 的 model_miss 精确一致）做**尽量自动裁决**；
拿不准的走标记通道，后续以规则/运行时命中交给用户判断。

## 自动分桶（原则：gold 是参照不是裁决）

| 桶 | 判据 | 默认判 | 规模 |
|---|---|---|---|
| 自动·真错 | 回退：v2.1 模型已给过 fix，与 gold 相互印证 | real（带 v2.1 的 fix/type） | 2,386 |
| 自动·噪声 | 两轮提问（第N字、片段±6+整句）均问过且均被否 | noise | 8,649 |
| 边界人裁 | 仅一轮问过被否 | （无默认，逐条裁） | 0* |
| 标记→规则通道 | 两轮门控均未见 | mark | 10,222* |

\* 门控未见里被 v2.2 第二通道问过又否的 158 条仍留标记桶（它的出路是规则通道，不是裁决结论）。

## 运行

```bash
python3 tools/gold-adjudicate/serve.py  # http://127.0.0.1:8766
```

- 数据源：`--grey ~/tmp/grey_zone --labels ~/tmp`（须含 dataset_v21/ dataset_v22/）。
- 裁决 journal 追加落盘 `~/tmp/gold_adjudicate/gold_verdicts_journal.jsonl`，重启即恢复。
- 快捷键 `R` 真错 / `N` 噪声 / `M` 标记；批量按钮按桶默认判一次采纳当前筛选。
- 抽检门与模型侧同构：每桶 ≥58 已裁且反判 ≤2% 可升级（mark 桶不设门——它的产物是规则候选，不是 gold 结论）。

## 导出

- `/api/export`：裁决结论。`action=gold-real` 带 `ref`，回退条目附 v2.1 的 `fix`/`type`
  （即 gold 侧类型标签来源）；`action=gold-noise` 供金集噪声剥离。
- `/api/export-marks`：标记桶 + 手动标 mark 的条目，`action=rule-candidate`，
  带 ctx 与两轮 asked 轨迹——落进规则引擎后由用户运行时命中判断。
