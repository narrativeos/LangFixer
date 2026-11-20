# label-rollout — 金集标注 rollout（v2 → v2.1 → v2.2 → v2.3 → v2.4）

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

## v2.4 合成配方（NaSGEC 式已知位置注入）

- 生成器 `synth_nasgec.py`：四操作 S（混淆字替换，源 datasets/augment_domain 词表）/
  D（虚词漏字）/ I（虚词多字）/ T（分句语序交换）；每行一个注入，自测
  `--selftest`（roundtrip + 字节 span 往返 + op 标签）；`--format pairs` 可接同源审计。
- 接线：`SYNTH=a.jsonl,b.jsonl`（逗号多文件）；合成行不参与 SUBSET 抽样，id 为
  `set#序号`（与真实行 `set-序号` 不碍撞）；注入 span 即候选，不走门控定位；
  行级 `provenance.synth` 随数据传递，stats/README 增 synth 坐实/漏判段。
- 同源审计：`datasets/gold_error_dist.py --pairs`——合成样本 S30/D40/I30、同音 25%
  vs 真实 gold S~99%、同音 ~65-70% → **非同源坐实**。定位：合成行只用于定位/判定
  校准（坐实率 = 注入真实度 × 判定召回），**不入句级训练池与评测**，不得为过闸门
  而改配方。

- 首轮验真（216 行：CSCD 干净句源，seed 7/42）行级坐实率：S 50/50=100%、
  I 53/57=93%、T 40/50=80%、**D 5/43=12% [5,24]**——判定器对实义字错召回满格，
  对虚词漏字系统性失明（38 漏判行中 28 是漏「的」；归因后经取证勘误见下节：
  「的」主导部分是注入真实性污染，盲区本身仍在）；另 T 漏判 10 行。
  同源审计同轮记录：synth200 编辑分布 S20/D43/I37（S 同音 52%）——与真实 gold
  非同源坐实维持，只用于定位/判定校准。后续方向：给判定 prompt 加虚词漏字
  提示 A/B，或规则点补「的」漏字（现 ZH_DE_CONFUSE 只覆盖混用）。

  后续方向已定（D_HINT A/B，同 216 行）：判定 prompt 加虚词漏字提示后 D 坐实
  **5/46→5/46（+0pp，零效应）**，S -4pp、T -6pp、gold 外新增 31→40——偏置解释被否，
  是真实能力缺口；提示臂放弃（D_HINT 开关保留默认关），杆杆转向规则层
  「的」漏字规则点取证。

## D 类取证判决（「的」漏字规则点：**不立**；盲区定性修正）

1. 存量：NACGEC 学习者真实错漏「的」仅 29/6369 句（0.45%，漏字主体是在/虽然/不仅/对）；
   true_gold 零宽插入补「的」仅 2；CSCD D 占比≈0。可见案例多落在名名修饰/
   公文体可省位——这些位无「的」在母语规范中合法。
2. 规则点判：谓词无法把真漏「的」与文体可省分开（干净公文满版漏「的」合法），
   ≥98% 精度不可达 → **ZH_MISSING_DE 不立**，按残余误报收口。
3. 盲区复测（`--d-exclude 的`，120 行非-的虚词删字，避开了注入真实性问题）：
   坐实 13/120=11%，与 D-de 的 12% 同水位——**D 盲区真实存在**；且分档揭示形态：
   39 行判无错 + 68 行改判它错（mixed，其中 fact/consistency 漂移 21 条）——
   「能感到坏了但定位不到删字位」。叠加 D_HINT 臂 +0pp：判定层对删字型错误
   召回≈11%，非提示可拉；静态规则也够不着（同上）。
4. 对检测路径的含义：漏字类在 mark 主体中坐实为**当前架构真实盲区**（模型判定
   低召回 + 规则不可达），后续要么入句级序列标注训练（合成行仍不入池，用真实
   流量积累），要么接受为检测覆盖边界并在产品文案中诚实标注。

## 纪律（不可违反）

1. 污染隔离：该标注器见过公开集，v2* 不得用于评测 Qwen 同族；
2. provenance 随数据传递；
3. review_queue 新增按每桶 ≥58 抽检后才能入 gold；
4. gold 是参照不是裁决（灰区进人裁，见 gold-adjudicate）。

历次数字与配方账在 notebook
`LangFixer-notes/2026-11-14_4B批量吞吐与prompt对照-zh_route_1k草稿.md` §J–M。
