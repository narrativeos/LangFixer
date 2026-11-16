# v2 级 true gold 合并器（true-gold）

`python3 tools/true-gold/merge.py` —— 把两页裁决的产出合成 true gold：
**true gold = 共识 ∪ 已采纳模型新增 ∪ 金集真错**，噪声进 excluded、
拿不准的进规则通道。裁决 journal 为空时出「按桶默认」版，随抽检推进
重跑即收紧（journal 后写覆盖前写）。

## 数据流

```
label-adjudicate (:8765) ─ adjudications.jsonl ─┐
                                                ├→ merge.py → ~/tmp/true_gold/
gold-adjudicate  (:8766) ─ gold_verdicts_journal ┘   ├ true_gold.jsonl
dataset_v22 标注（逐条现算，不信行级计数）            ├ excluded.jsonl
grey_zone 分区（回退/问了被否/门控未见）              ├ rule_candidates.jsonl
                                                    └ stats.json
```

- `unit:"edit"`：共识/已采纳，带 a/b/fix/type，可直接作训练与路由标签。
- `unit:"char"`：金集真错（回退），fix 取 v2.1 重叠编辑（type 同源，即
  gold 侧类型标签的落地形态）。
- `excluded.jsonl`：`gold-noise`（金集噪声候选，训练降权用）与
  `reject`（模型新增被否）。
- `rule_candidates.jsonl`：门控未见 ~10,222 → 规则引擎素材。

## 规则通道的设计约定（下一步实现依此）

1. **静态规则先行**：从 rule-candidates 的 ctx 归纳高敏感事实核查要点
   （时间/地点/事件/关键人物：正则+词典）与标点规则（半角入中文语流、
   叠用）——这部分 v2.2 channel2 已有雏形，可直接复用 `label_rollout.py`
   的 `punct_cands`。
2. **命中即问用户**：运行时规则命中 rule-candidate 位置 → 弹给用户的
   是「金集说这里改 X / 模型两轮都否」的**裁决问题**，不是自动改写。
3. **判断回流 journal**：用户判断按 `id#pos` 写回
   `~/tmp/gold_adjudicate/gold_verdicts_journal.jsonl`（与页面同 iid、
   同格式），merge.py 重跑自动吸收——不需要新账本。
4. 未命中的 rule-candidate 保持 mark 状态，随下一轮 rollout 重新分区。

## 首跑数字（裁决前·按桶默认）

- 标签行 12,542 = 共识编辑 10,156 + 金集真错 2,386（采纳 0，待抽检）
- gold 字符覆盖 20,053/38,924 = **51.5%**；共识编辑足迹另涉 gold 外字符（多字编辑足迹，非独立标签）
- 待抽检模型新增 7,376 · 金集噪声候选 8,649 · 规则候选 10,222
- 类型分布（共识编辑口径）：typo 5,994 / grammar 3,495 / cohesion 324 /
  consistency 205 / punct 99 / fact 39 —— 分型标签即由此喂 `route_label` 重生成。
