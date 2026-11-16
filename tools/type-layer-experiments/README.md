# 分型分层三空白量化实验（EVOLUTION §11.14 补二）

一次性测量脚本，产出方案定案所需三个数字。数据路径硬编码为 ~/tmp 下的真实
标签资产（dataset_v22、true_gold），只读，无副作用。

```
python3 tools/type-layer-experiments/experiments.py
```

- 实验1：句内同族多错频率（presence vs 计数形式之争）→ 61.3%（10 型口径）
- 实验2：must 桶 consistency/cohesion 的规则谓词可达性 → span 17.7% / ±6 窗口 42–52%
- 实验3：规则臂下界，**Python 复刻同一本 `lexicons/zh_gec_lexicon.json`**（两本
  账是禁忌；POS 路径规则未复刻=下界口径）→ recall 0.3% / precision 11.1%，
  punct 子通道语料毒（原文 ASCII 标点集）

注意：实验3 的 anomaly in-quote 守卫为近似实现（句首引号字符判定）。
