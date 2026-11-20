# label-rollout — 金集标注 rollout（v2 → v2.1 → v2.2 → v2.3）

`label_rollout.py` 是 dataset_v2* 系列的生成机件（门控定位 → LLM 判定 → 汇总
分档队列）。原稿在 `~/tmp/gate-probe/`（未版本化）；本副本为**权威版本**，
仓库外数据目录用 `GP_DATA` 指回：

```sh
VER=v23 GP_DATA=~/tmp/gate-probe OUT=~/tmp/dataset_v23 \
  LANGFIXER_LLM_KEY=… python3 tools/label-rollout/label_rollout.py
```

`GP_DATA` 下需有 `type_proxy.json`、`gate_charscores.json`（门控分复用，缺则
重跑 stage1 需 torch+本地模型）、journal/log 也落在那边（断点续跑）。
`VER` 选代际：v2.1=整句位置提问；v2.2=片段±6+整句+第二召回通道；
v2.3=v2.2 + ①判无错的 why 预筛单片段重试 ②单片段截断拉高 max_tokens 重试
③parse_fail 口径修正（JSON 解析成否计数，合法全无误行不再入账）。

## v2.3 三个补丁（§M 候选①②③）

- **①why 预筛**：`err:false` 但 why 文本含修正意图（删/漏/语序/搭配…）→
  该片段单片段重试一次（tries 标记防死循环）；why 干净则不浪费调用。
- **②单片段截断**：窗口对半重试到单片段仍 `finish_reason=length` →
  max_tokens 800→1600 再试一次。
- **③parse_fail**：v2.2 里 `nfail += not got` 把「合法全无误」行计成解析失败
  （stats 的 parse_fail 虚高）；现按 JSON 解析成否计，snip_anomaly 只剩
  no-diff 一种。

## 纪律（不可违反）

1. 污染隔离：该标注器见过公开集，v2* 不得用于评测 Qwen 同族；
2. provenance 随数据传递；
3. review_queue 新增按每桶 ≥58 抽检后才能入 gold；
4. gold 是参照不是裁决（灰区进人裁，见 gold-adjudicate）。

历次数字与配方账在 notebook
`LangFixer-notes/2026-11-14_4B批量吞吐与prompt对照-zh_route_1k草稿.md` §J–M。
