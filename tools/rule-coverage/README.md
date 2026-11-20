# 规则覆盖（rule-coverage）

`python3 tools/rule-coverage/cluster_fcgec.py` —— 把 true gold 编辑级真错按
**FCGEC 七类**（语序不当/搭配不当/成分缺失/成分赘余/结构混乱/不合逻辑/语意不明，
Findings of EMNLP 2022）聚类，对照 `langfixer.db` 的 zh `rule_defs` 现状出
**grammar_novice 兜底的规则点缺口清单**（方案 A，2026-11-20 GitHub 调研定案）。

聚类是**编辑形状启发式**（D/I/R + 互嵌对 + 同字重组），不是人工标注：
SC/ILL/AM 三者形状不可分时并入 `rewrite_review` 待人工细分。FCGEC 只借
分类法对照，**其语料（非商业许可）不入生产库**。

## 首跑数字（2026-11-20，true_gold 12,671 条编辑）

grammar 桶 4,765 条的七类分布：

| 桶 | grammar | 现成规则点 |
|---|---:|---|
| 成分赘余 CR（多字） | 2,066 | **无** ← 头号靶子 |
| 句级改写待细分 | 751 | 无（先人工细分） |
| 搭配不当 IWC | 1,177 | **无** |
| 语序不当 IWO | 228 | 无（rollout/知识通道靶子） |
| 虚词功能字 funcword | 268 | **ZH_DE_CONFUSE ✓**（ZH_DE_DA_DE 维持停用） |
| 结构混乱 SC | 159 | 无 |
| 成分缺失 CM | 116 | ZH_MISSING_SUBJECT ✗停用 |

zh 结构类七类内 3,791 条（69%）。CR 桶高频删除文本直指规则点：
`左右`107/`的`78/`以上`62/`约`40/`多`33/`近`25 → **约数词并置**（约…左右）；
`不`45/`不再`30 → **否定词冗余**（避免…不再）；`使`45/`由于`25 → 介词淹没主语。
IWC 单字替换 top：为→的27、倍→半14、于↔与12 → 功能字混用表可扩。

## 产出

- `~/tmp/rule_coverage/clustered.jsonl`：逐条编辑 {id, span, deleted, fix, bucket, how}
- `~/tmp/rule_coverage/report.md`：分布矩阵 + 缺口优先级 + 样本

## 补规则点的靶子顺序（下一迭代依此）

1. 约数词并置（`约|近|大约` 与 `左右|以上|多|余` 同现）≈250+
2. 否定冗余（`避免|防止|杜绝|切忌` × `不|不再`）≈75+
3. 功能字混用表扩条目（为/的、于/与、在/从…）≈290+
4. ✅ ZH_DE_DA_DE 复启用评审：维持停用（Phase-1 审计 18/18 误报，POS 标签噪声大），
   其 funcword 桶收益由新规则点 ZH_DE_CONFUSE 吸收（含单字动词左侧集，的→得子空间
   原位覆盖 0→15）
5. `使/由于`+淹没主语并入 ZH_MISSING_SUBJECT 模式面
