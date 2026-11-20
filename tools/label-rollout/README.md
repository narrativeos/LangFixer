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
5. 两步确定性探针（`feature-model/del_probe.py`，char_ctc_zh p_err 边界探测 +
   17 虚词插入重打分）取证：**通道不成立**。D 侧 gold 边界召回仅 19/120
   （θ=0.3，±1 容差；θ=0.2 也仅 24），坐实 27/120；而真干净文本 FP 远大于收益
   （公文/医答域 47/400 句、新闻域 129/400）。双重结构性缺陷：step1 门控 p_err
   对删字边界基本不触发（盲区定性再坐实）；step2 插入改善对错误类型**非特异**
   （别字处的宽边在插虚词后 p_err 同样回落，如「由为重要」+「给」）。
   附带两笔数据卫生发现：其一，旧「干净公文」集 /tmp/clean_cscd.txt 实为 CSCD
   测试集 wrong+correct 双列拼接去重（≈一半是错句），以它为 clean 参照的历史
   FP 数字均为保守估计；其二，p_err 句内基线随文体剧变（窄公文子集 0.001 /
   CSCD 法律+医答净句 0.045 / CSCDNS 新闻净句 0.156），② p_err 门控若在
   开放域输入上按公文校准阈值会大面积误触，② owner 需单独复核。

## 漏字盲区外部方案调研（A 标注 / B 生成式差分 / C 边界接受）

判决背景（747fad7）：漏字坐实为架构真实盲区——③ 判定 D 召回≈11%、p_err 两步
探针边界 19/120 且净句 FP 公文 47/新闻 129 每400、池内 I 型监督极稀
（cscdns I=0.3%，EVOLUTION §12）。三路候选取证如下。

### A 句级序列标注 insert 打标：无现成破法，暂关

1. 取证：TW-NLP 官方打标就是 difflib 对齐，且自认「纯 insert 行在 source 侧
   标签可全 0」——业界标准打标与我们的盲区同构，insert 监督学不到漏字定位；
2. 监督稀：公开池 I 型≈0.3%（cscdns 3 万抽样，EVOLUTION §12），S 型占绝对主体；
3. 死锁：合成行按同源审计不入池，真实流量漏字率又极低——打标飞轮循环依赖，
   需先有非循环的 insert 监督源才谈得上训练。
4. 旁证：TW-NLP 现役架构=ELECTRA 字级门控+4B LLM 生成，与我们的②门控+③LLM
   同构，即 B 路线存在业界先例（其门控同样不含 insert 能力）。

### B 生成式订正差分：1.5B 实测不达标，整线关闭

臂1（本地，`tools/label-rollout/eval_ctc.py`）：shibing624/chinese-text-correction-1.5b
（Qwen2.5-1.5B，Apache-2.0，百度真实场景订正含多字少字；模型卡协议
「文本纠错：\n{句}」chat template，do_sample=False，repetition_penalty=1.08；
hf-mirror 下载 3.1GB，MPS bf16 batch=8）：

| 集 | 指标 | 数值 |
|---|---|---|
| synthD 120 行 | 句级 exact | 21/120=17.5% |
| synthD 120 行 | 漏字位边界（difflib src→clean ±2 容差） | 25/120=20.8% |
| 公文/医答净句 400 | changed 句 | 12=3.0% |
| 公文/医答净句 400 | insert 型 FP 句 | 1=0.25% |
| 新闻净句 400 | changed 句 | 155=38.8% |
| 新闻净句 400 | insert 型 FP 句 | 35=8.8% |

勘误：首跑报的 boundary_ins=1/120 是指标 bug——`edits[].span` 坐标系与 src
不符，上表边界已改按 difflib(src→clean) 现算；exact 数字不受影响。

构成与形态：新闻域 155 changed 中 replace-only 改写 117（模型在改文风，非纠错）、
纯 insert 13、纯 delete 10、ins+del 8、ins+replace 4，另有 1 条漂移出英文词
（幻觉例）。数据卫生：新闻「决不仅→绝不仅」类坐实说明 cscdns correct 列自带
残留错字，clean 参照非绝对干净。

臂2（网关零样本差分对照，`tools/label-rollout/probe_gendiff.py`，Qwen3.8，
temperature=0，SYS「只修错字与缺失字…一字不改」，已跑完）：

| 集 | 指标 | 数值 |
|---|---|---|
| synthD 120 行 | 句级 exact | 53/120=44.2% |
| synthD 120 行 | 漏字位边界（difflib ±2） | 59/120=49.2% |
| 公文/医答净句 400 | changed 句 | 88=22.0% |
| 公文/医答净句 400 | insert 型 FP 句 | 34=8.5% |
| 新闻净句 400 | changed 句 | 112=28.0% |
| 新闻净句 400 | insert 型 FP 句 | 46=11.5% |

FP 形态：标点补插、「的」文风插、空白插入为主（另见真实漏「有限公司」补回例）。

### B' 灰区 ③ 协议 A/B：实测 no-go（真实灰区上判定协议全面占优）

B 节「缺口在协议不在模型」信号系在**合成** synthD 120 句上测得。本轮把它落到
真实 ② 灰区集：MuCGEC funnel 缓存 `llm_ran=true` 380 句（= ② 在生产路径上
真实交给 ③ 的灰区），gold 为字节坐标（与 `make_gold_zh.py` 及 Rust 预测同口径）。
三臂（`eval_diff_protocol.py`）：判定臂 funnel27b=缓存 ③ 预测（带 ② 候选，
模型 qwen3.8-27b）；判定臂 Qwen3.8=capability 缓存 `source==llm` 预测（与差分
臂同模型）；差分臂 Qwen3.8=同句同模型去 ② 候选只换协议（SYS 照搬臂 2，
temperature=0，difflib src→out 字级 diff）。

| 指标 | 判定臂 funnel27b | 判定臂 Qwen3.8 | 差分臂 Qwen3.8 |
|---|---|---|---|
| I-content 句级召回 | 225/311=72.3% | 222/311=71.4% | 92/311=29.6% |
| I-content 召回 | 537/976=55.0% | 517/976=53.0% | 126/976=12.9% |
| I-punct 召回 | 41/74=55.4% | 43/74=58.1% | 10/74=13.5% |
| S 召回 | 615/935=65.8% | 564/935=60.3% | 254/935=27.2% |
| D 召回 | 364/582=62.5% | 353/582=60.7% | 127/582=21.8% |
| FP wide（含零宽插入） | 24 | 57 | 31 |
| FP prod（宽度>0） | 21 | 43 | 20 |

增量口径（gold 级逐条判，funnel 臂 vs 差分臂）：I-content 差分臂独立贡献仅
38/976=+3.9pp（判定臂独得 449、两臂同得 88、双双漏掉 401）；S +4.6pp、
D +4.3pp；代价是差分臂独有 FP 句 23 句（判定臂 18）。

判：**no-go**。三点收获：

1. synthD 差分涨幅（≈49%）不迁移到真实灰区：真实灰区 I 位大多已被 ② 定位
   带进 ③，去掉 ② 候选后模型在真实学习者句上自主发现漏字的能力极低（12.9%）；
   synthD「整句被删字、通读即可补」是灰区里少数形态，协议红利系合成伪影；
2. 两臂都漏的 401 个 I 位才是当前 ② 定位下真实灰区的漏字盲区真身——瓶颈在
   ② 定位与 insert 监督（回 A 线前提），不在 ③ 协议；判定协议 55% 即 ③ 对
   灰区 I 的现实上限；
3. 混合（判定∩差集并集）以 +3.9pp I 换 23 句新 FP，不值。

caveats：funnel 臂模型为 qwen3.8-27b（本轮网关该模型上游不可用，差分臂只能
跑 Qwen3.8，跨模型对比看 capability 臂更净）；I 命中容差 ±2 为字节坐标
（≈字边界严格匹配）；灰区选取=funnel 缓存中 ③ 真实跑过的句。

复现：语料 `gray380.jsonl` 每行 `{text,gold,judge_a,judge_b}`——text/gold 取
`datasets/eval_reports/cache/mucgec_current_none_funnel.jsonl` 中 `llm_ran=true`
的句（gold 对齐 `datasets/mucgec_gold.json` 去重编辑，字节坐标），judge_a=该
缓存 ③ 预测，judge_b=`mucgec_current_none_capability.jsonl` 同句
`preds[source==llm]`；跑 `python3 tools/label-rollout/eval_diff_protocol.py
--model Qwen3.8 --cache <生成缓存>`（生成可断点续跑）；报告
`datasets/eval_reports/20261120T101244Z_mucgec_gray380_diff-protocol.md`。
排障：差分臂首跑 span 用工符坐标与字节坐标 gold 错位，召回被误判为 1-5%；
改字节坐标后重跑。灰区评测务必统一字节坐标（与 747fad7 勘误 `eval_ctc.py`
坐标系 bug 同源）。

判：整句无条件接入两臂均 FP 不过关，**B 直连路线关闭，7B/4B 本地臂不续**；
若未来重启（如 twnlp/ChineseErrorCorrector3-4B，F1 avg 0.8521），门槛=新闻净句
insert 型 FP ≤1%/句 且 D 边界 ≥p_err+10pp。但结论分裂出重要信号：零样本差分
协议 D 侧边界 49.2% 远超同模型 ③ 判定协议（≈11%）与 1.5B（20.8%）——
D 弱点的相当部分是**协议问题而非模型问题**：要模型「指出错误」它定位不到零宽
插入位，要它「输出订正句」它就把句子补对了。

### C 接受边界 + 产品诚实标注：当前兜底

检测路径文案明确「漏字型错误（少字）为当前覆盖限制」，与③-F 特征模型槽位
并存；待有合格 insert 监督源（真实流量积累或外部授权数据）再回 A。

### 推荐

短期 C（零风险）；A 挂起等同源 insert 监督源出现；B 直连已按实测关闭；
B'（③ 协议改造）已实测 no-go（见上节），接入槽位作废。盲区后续回到
② 定位能力与 insert 监督源两条主线。

另：本次排障发现旧会话落盘脚本里 Authorization 字面量被脱敏层写成 `"***"`
（已在 `9fc01e0` 修正），提醒：**凡是旧会话写的带凭据字面量脚本，用前先查盘上字节**。

### D ② 定位外部方案调研（GitHub 实测，2026-11-20）

内部取证：`gec-zh/**/*.rs` 全库 grep `insert|漏字` **零匹配**——② 层目前没有任何
insert 候选生成器，盲区坐实在定位侧（不在判定侧；B' 已排除判定协议因素）。
与 A 线结论互证：TW-NLP difflib 自动打标对纯 insert 行 source 侧标签可全 0，
业界监督语料本身就只标 S 型，公开数据里 I 型监督≈0（池内 0.3%，见 §12）。

外部取证（GitHub search API 实测；中文关键词噪音巨大，按 description/README 逐仓判定，
不按搜索 stars 序下结论）：

| 仓库 | ★ | 技术路线 | 与 I 定位的关系 |
|---|---|---|---|
| pycorrector | 6.5k | 检测=词表/词频+词图+PPL(Kenlm)、ELECTRA 门控；订正=MacBERT MLM / T5 / ChatGLM3 | 检测清一色 S 型；订正侧有 MLM 填槽但无人做字级 insert 候选定位 |
| xmnlp | 1.3k | 词典/规则+拼音特征文本纠错 | S 型 |
| text_analysis_tools | 0.7k | 文本工具集内词典/规则纠错 | S 型 |
| funNLP | 8.4万 | 资源列表（含「中文文本纠错模块代码」条目；汉字 featurizer 发音+字形特征路线） | 是资源不是方案；字形/拼音特征路线可记档 |
| bob-plugin-openai-translator | – | LLM 整段重写 | 非纠错器；印证「LLM 差分」业界只以整段重写存在 |
| Chinese_spelling_Correction | 7 | notebook 练习 | 不可用 |
| cgec-pivot | 1 | 改写式 pivot | 不可用 |

结论：**业界纠错检测层全部只做 S 型**（词表/词频/ELECTRA 门控），insert 定位无任何
现成开源实现；「LLM 出订正句再差分」只以整段重写形式存在、无人做字级差分纠错管线；
公开 insert 监督源不存在（与 #16 挂起原因一致）。论文路线 arXiv 复查
（`"deletion augmentation"`×GEC、`"gap-filling"`×spell checking 两查询）当日均零命中，
故本节**只引仓库证据、不引论文**。

定位实验候选（下一步待拍板）：

1. **合成 I 监督臂（主推）**：复用 synthD 生成器（`synth_nasgec.py`）从干净池
   （`l3_pool_raw/`）造「删 1 字」行，训练 ②/③-F 定位臂，在 gray380 上两臂都漏的
   401 个 I 位评 I 定位召回；FP 口径挂真实集（新闻净句 gap FP 或 gray380 负句）。
   这等于把 A 线「等 insert 监督源出现」改写成「自造监督源」；本轮灰区 A/B 的教训
   依旧有效：synthD 伪影会制造合成红利，评测必须挂真实灰区。
2. **n-gram gap-filling 探针（零新模型）**：用 1M trigram 表（`feature-model/ngram_table.json`）
   对每个字符间隙算 PPL(补最优单字 vs 原句)，Δ 过阈提名 insert 候选。与 §13 #1
   已判否旧案划清界限：旧案是**句级**加权 OOV 门控（AUC 0.47），本探针是**位级**
   Δ-PPL 候选提名，不是旧案重提。
3. **差分独得提名+判定确认**：funnel 缓存差分臂独得 38 个 I 位作 ③ 判定候选。
   已知代价 23 句独得 FP，收益太薄，弃。

倾向 1（合成删字行直接命中 ② 定位臂的监督缺口）；2 可作零训练便宜探针并行；3 弃。

## 纪律（不可违反）

1. 污染隔离：该标注器见过公开集，v2* 不得用于评测 Qwen 同族；
2. provenance 随数据传递；
3. review_queue 新增按每桶 ≥58 抽检后才能入 gold；
4. gold 是参照不是裁决（灰区进人裁，见 gold-adjudicate）。

历次数字与配方账在 notebook
`LangFixer-notes/2026-11-14_4B批量吞吐与prompt对照-zh_route_1k草稿.md` §J–M。
