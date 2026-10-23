# L3 特征模型 — LangFixer 三级响应 level 3 的主力层（三期「规模化」，训练飞轮自进化）

本目录实现 **L3 特征模型层**：在 LLM 之前运行的轻量特征模型服务，
把 LLM 层收缩到**纯长尾**；模型本身经**训练飞轮**持续重训练（LLM 确认
样本 → 重训练 → dev 评测 → 热切换，见下文「模型演进」节）——L3 从静态
模型变为自进化模型，LLM 层同时承担「兜底 + 教师（L3 训练数据源）」双重角色：


```
用户输入（level 3）
   │
   ▼
┌─────────────┐  命中（高置信）
│ 规则引擎     │ ─────────────────────┐
└──────┬──────┘                      │
       │ 未命中                       ▼
       ▼                    ┌───────────────────┐
┌─────────────┐  弃权        │  合并（去重/排序）   │
│ 决策模型     │ ──────────▶  │                   │
└──────┬──────┘              └─────────┬─────────┘
       ▼                                │
┌─────────────┐  弃权 / 宕机            ▼
│ L3 特征模型  │ ──────────▶        最终错误列表
│ (本目录)     │  高置信 → 直接定案
└──────┬──────┘
       │ 明确无错 → 放行（不调用 LLM）
       ▼
┌─────────────┐
│ LLM 层（长尾）│  仅特征模型弃权 / 服务不可用时调用
└─────────────┘
```

**设计原则**（与 `gec-core/src/l3.rs` 对应）：

- **默认开启**：level 2/3 先跑特征模型（P0.5：L3 是唯一的 pre-LLM 门控；
  `GEC_L3_ENABLED=0` / 设置界面可显式关闭，level 2 回退「决策模型弃权」旧语义、
  level 3 回退「无条件 LLM」旧行为）；
- **高置信定案**：`has_error_prob >= abstain_threshold`（默认 0.5）→ 直接产出
  错误项（source = `feature`，含模式化 span 定位与机械修正），**不调用 LLM**；
- **弃权 = 长尾**：概率落在灰区 `[0.25, abstain_threshold)` 或显式 `abstain`
  → 交由 LLM 兜底；服务不可用 → 同样回退 LLM（保持 level 3 契约）；
- **明确无错放行**：概率低于灰区下界 → 不产出错误、不调用 LLM。

## 文件

| 文件 | 作用 |
|------|------|
| `server.py` | FastAPI 服务：`POST /l3-detect`（逐句判定 + span 定位 + 机械修正）；**飞轮端点** `POST /l3-retrain`（合并确认样本重训练 + dev 评测 + 热切换）/ `GET /l3/status`（当前模型 F1 + 重训练元数据）；`FEATURE_MOCK=1` 离线 mock 模式（开发/测试用） |
| `l3_finetune.py` | 训练工作流：`extract`（gold 数据 → 特征）/ `train`（句级二分类 + 类型多分类；vegaml 可用则用之，否则回退 sklearn）/ `eval`（dev 集评测） |
| `correct.py` | 模式化 span 定位 + 机械修正（zh：的/地/得、把字句、时间状语语序、重复字；en：拼写、a/an、时态、助动词、缺介词）——mock 与真实模型模式共用 |
| `prepare_data.py` | 把 `datasets/jfleg_gold.json` / `mucgec_gold.json` 转成训练/评测格式 |
| `prepare_l3_pool.py` | **CEC 混合训练池**（工作流 D）：下载 `twnlp/csc_data` + `cgc_data` + `lang8_hsk` 训练平行对（→ `l3_pool_raw/`，不入库），字级 diff 推导句级任务类型（spelling/grammar/mixed），1:1 正/负例 + 分层 dev（每类型 2500+2500）+ 去污染（MuCGEC/CSCD/NaCGEC 评测集源句剔除），产出 `data/zh_pool/{train,dev}.jsonl` |
| `build_ngram_table.py` | **重建 zh n-gram 干净频率表**（方案 B）：池 ref 干净侧 + 去污染 + min_count/top-N 剪枝 → `ngram_table.json`（1M trigram）+ `bigram_table_zh.json`（300K bigram，hard/soft 判定用）；附覆盖率报告 |
| `eval_l3_funnel.py` | L3 漏斗评测（MuCGEC 句级/段落级 + 定向复查成本模拟 + 真实用户文本门控）；`L3_NGRAM_TABLE`/`L3_BIGRAM_TABLE` 可指向别的表做 A/B 而不改仓库文件 |
| `eval_ngram_table_ab.py` | **n-gram 表 A/B 验收器**（EVOLUTION §13 #1 规定口径）：原生集 OOV p50 / 灰区率 / **AUC(clean vs err)** / **等安全线放行率**；判定线「AUC 不升 或 放行率增幅 <5pp → 拒绝」——**不看灰区率** |
| `eval_l3_data_ab.py` | **L3 训练池数据增益三臂**（EVOLUTION §12.2 复现器）：现有池 vs +候选(筛同音) vs +候选(不筛)，HistGB 代理 vegaml；输出各原生集 AUC + 等安全线放行率 |
| `eval_zh_model_gate.py` | zh 决策模型门控评测（工作流 C）：跨域 AUC + 门控操作点，对照启发式 |
| `eval_pattern_coverage.py` | 原生 gold 集（cscdns/nlpcc23/cctc）编辑粒度评测：pattern 覆盖率 + 门控可见性 + 未覆盖字对 top 榜 |
| `eval_char_pla.py` | ELECTRA CTC 字级异常评测（零样本或微调 checkpoint）：句级 AUC + 门控操作点 + clean p50（含 §I2 confirmed holdout 行），逐字概率落盘供离线聚合搜索 |
| `eval_char_pla_ab.py` | **字级 PLA 模型 A/B 判定器**：读两份 `char_pla_scores_*.json` 做配对比较——AUC / clean p50 / **等错误可见率下的 clean 放行率**（vis=99/97/95/90%）。判定线：原生集多数档增幅 <2pp 或 AUC 下降 → 拒绝；p50 下降而放行率不升 = 纯分数平移，同样判负 |
| `finetune_char_ctc.py` | CTC 头微调（工作流 G）：池对 → 字级 diff 标签 + 干净负例 + 去污染；`--confirmed-repeat` 飞轮 confirmed 过采样负例（域校准）；`--extra-clean <txt/jsonl.gz>` 外部纯干净长文负例（只取干净侧 + 自污染防护 + 块表去污染 + 种子抽样，§13 #5 A/B 用）；`--seed` 双臂同种子；训后打印 dev 字级 loss/AUC 作 GPU 故障 sanity（NaN 即中止）；落 `models/<out>/train_meta.json`（argv/seed/counts/dev 指标，A/B 双臂复产用）；产出 `models/char_ctc_zh*/` |
| `char_gate.py` | 字级门控侧车服务（端口 4811）：`POST /gate {texts[,detail]}` → top-3 字替换概率均值（detail=逐字概率）；server.py 经 `L3_CHAR_GATE_URL` 调用 |
| `eval_char_contrast.py` | 句内对比归一实验（G 遗留域漂移）：diff/ratio/maxdiff 特征 AUC + 策略搜索——**负结果**，归一不解决漂移 |
| `eval_cctc_context.py` | CCTC 跨句语境损失评测：gold span 的 PLA 概率 全文 vs 句级隔离打分对比，量化定向复查门控代价 |
| `requirements.txt` | Python 依赖 |

## 快速开始

```bash
cd feature-model
python3 -m venv .venv && .venv/bin/pip install -r requirements.txt

# 1) 离线 mock 模式（无需训练，启发式判断；验证 Rust 侧链路）
FEATURE_MOCK=1 .venv/bin/python server.py
#    → POST http://127.0.0.1:4806/l3-detect  {"text": "...", "lang": "zh"}

# 2) 真实模型（gold 数据训练）
.venv/bin/python prepare_data.py --gold ../datasets/jfleg_gold.json --lang en --out data/en/
.venv/bin/python l3_finetune.py extract --lang en --data data/en/ --out l3_data/en
.venv/bin/python l3_finetune.py train   --lang en --data l3_data/en
.venv/bin/python l3_finetune.py eval    --lang en --data l3_data/en
# zh 同理（--gold ../datasets/mucgec_gold.json --lang zh）
.venv/bin/python server.py   # 加载 models/<lang>/ 下训练产物
```

Rust 侧（**默认开启**，直接启动即可；环境变量可覆盖，或经设置界面「L3 特征模型」卡片切换）：

```bash
cargo run -p api-server
# 显式关闭：GEC_L3_ENABLED=0
# 显式指定服务地址：GEC_L3_ENDPOINT=http://127.0.0.1:4806
```

## CEC 混合池（工作流 D：zh 句级门控重训练）

MuCGEC gold（约 7.5K 句）训练池的泛化面太窄，且旧模型在新分布上「几乎全判有错」
（P≈0.5：每句都高置信定案，干净句全部误报）。`prepare_l3_pool.py` 把 twnlp 三个 HF
训练集（CSC 382K / CGEC 57.8K / Lang8+HSK 1.57M，均 MIT）混成 **50 万+ 对**的
has_error 二分类池：

- **互补单侧分配**：每对平行句只贡献一侧（50% 正例 = 源句 / 50% 负例 = 纠正句）。
  若同对双侧入池（src=正 / ref=负），两句近乎重复而标签相反，聚合特征无法区分 →
  模型退化为先验（P≈0.5，实验验证）；单侧分配消除矛盾对
- **句级任务类型**（字级 diff 推导，CEC `val_task_stratified.jsonl` 思路）：
  spelling（仅单字替换）/ grammar（含多字/增删）/ mixed
- **分层 dev**：spelling/grammar/mixed 各 2500 对 → 源句作正例；另 2500 对（不相交）
  → 纠正句作负例 = 15K 句；其余对进 train
- **背景表**（`l3_bg/zh_bigrams.json`，不入库）：全部采样对 ref（干净侧）字对
  top-500K → `rare_bigram` 特征（错别字制造干净语料中不存在的字对）的成员判定表
- **去污染**：MuCGEC `filter_sentences`（7137）+ MuCGEC dev/test + CSCD/NaCGEC 基准
  源句全部剔除；lang8_hsk 的 valid.para / MuCGEC.input / nlpcc18.input（CEC 自己的
  评测输入）不使用

```bash
# 1) 建池（首次下载 ~340MB 到 l3_pool_raw/，不入库；幂等）
.venv/bin/python prepare_l3_pool.py --out data/zh
# 2) 特征 → 训练 → 评测（与 MuCGEC 池同一条 l3_finetune 工作流）
.venv/bin/python l3_finetune.py extract --lang zh --data data/zh --out l3_data/zh
.venv/bin/python l3_finetune.py train   --lang zh --data l3_data/zh
.venv/bin/python l3_finetune.py eval    --lang zh --data l3_data/zh
```

**关键结论（实验记录）**：

- `rare_bigram` 是拼写错误的强信号：20K CSC 对实验 F1=0.966（P=0.999）；
  但**聚合特征无法区分「语法错句 vs 干净句」**（全池三类概率分布重叠：
  干净 p50=0.487 / 语法 p50=0.489 / 拼写 p50=0.508）——字级差异需要字级模型
  （CEC 的 ELECTRA discriminator，见 `CEC_ADOPTION.md` D.2，未做）
- 因此 `server.py` 的路由改为**精度优先**：高置信且**模式库命中** → L3 定案
  （span 定位 + 机械修正，免 LLM）；高置信但**无模式命中** → 弃权交 LLM（不再合成
  句级误报）；任一句需 LLM 裁定 → 整段弃权（避免「部分定案 + 部分静默丢弃」）
- 效果（15K 分层 dev）：干净句误报 **7500 → 49**（-99.3%）；拼写错 85 句免 LLM
  定案；语法错全部交 LLM 正确裁定；阈值 0.5 为最优（降低阈值只增加干净句误报：
  0.45→162、0.4→254）

## zh n-gram 干净频率表（方案 B，2026-10-18 扩表）

句级启发式门控（`L3_GRAY_ZONE_LOW`）的主信号是 char 3-gram OOV。旧表仅
3.4 万条（≈1.4k clean 句），正常文本大面积假 OOV → MuCGEC 灰区率 91.5%
（≈全文送 LLM，无漏斗可言）。`build_ngram_table.py` 从 `l3_pool_raw/` 四个池
文件的 **ref 干净侧**（去污染块表复用 `prepare_l3_pool.load_blocklist()`）重建：

- `ngram_table.json`：**1,001,100 条** trigram（min_count≥2 后 top-1M +
  confirmed clean 句补入），16.4 MB（入库；重跑脚本可复现，seed 固定）
- `bigram_table_zh.json`：**300,000 条** 高频 bigram（min_count≥5 top-300K），
  3 MB——用于 hard/soft OOV 区分：OOV trigram 的两个组成 bigram 都高频 →
  soft（"常见成分的新组合"，权重 0.4），否则 hard（表面异常，权重 1.0）；
  另加短句平滑（分母 +2）
- 抽取规则三处一致：`build_ngram_table.py` / `correct.zh_oov_counts` /
  旧 `l3_bg/zh_bigrams.json`（原文含标点口径）**不复用**——规则不同

**校准事实（重要，评测口径变更）**：扩表后句级 OOV 是**弱检测器**——
CSCD AUC(hard_ratio)=0.59 / NaCGEC AUC(hard)=0.59 / MuCGEC AUC(hard)=0.67；
旧表上 MuCGEC "OOV≈0.8 vs clean≈0" 的高区分度是小表假象（一切皆 OOV）。
启发式因此定位为「表面异常门」，不产生 OOV 的错误（语序/残缺/抽象搭配）
由 pattern 层与决策模型（工作流 C）兜底。

操作点（`eval_l3_funnel.py`，2026-10-18，MuCGEC + 真实用户文本门控段）：

| `L3_GRAY_ZONE_LOW` | MuCGEC 弃权 | MuCGEC FN | confirmed clean 放行 | CSCD 病句漏检 | NaCGEC 病句漏检 | LLM 输入 |
|---|---|---|---|---|---|---|
| 旧表 0.30 | 91.5% | ~8% | ≈0% | — | — | 基线 |
| **0.30（默认）** | 63.1% | 30.6% | 70% | 10% | 5% | **-26%** |
| 0.40 | 48.4% | 44.7% | 75% | 16% | 13% | -36% |
| 0.55 | 31.1% | 61.3% | 86% | 39% | 35% | -31% |

> MuCGEC（95% 病句的学习者语料）的 FN 是保守上界；母语用户文本（CSCD/NaCGEC，
> 已从表去污染）上默认档漏检仅 5-10%。confirmed 放行率含飞轮稳态效应
> （confirmed 句已在表内，与生产运行时增量入表一致）。

```bash
# 重建表（改参数/语料后；~35s，原子写，报告覆盖率与 MuCGEC 区分度）
.venv/bin/python build_ngram_table.py --top 1000000 --min-count 2

# A/B 换表而不改仓库文件（评测/构建各一对环境变量）
L3_NGRAM_TABLE=/tmp/tbl_3M.json L3_BIGRAM_TABLE=/tmp/tblbg_3M.json \
  .venv/bin/python eval_l3_funnel.py
L3_NGRAM_OUT=/tmp/tbl_3M.json L3_BIGRAM_OUT=/tmp/tblbg_3M.json \
  .venv/bin/python build_ngram_table.py --top 3000000 --bigram-top 700000
```

**已测否定项（2026-10-19，EVOLUTION §12.1）：不要用外部干净语料扩这张表来"降灰区率"。**
`--extra-clean <jsonl.gz>`（只取干净侧，过滤占位符/去重/块表去污染）已实现，但用
midu2022 初赛 100 万伪数据的干净侧（932,452 句，distinct trigram 11.8M）实测是一条
**成本↔安全单调滑动条**，没有任何档位改善判别比：

| 表 | trigram | MuCGEC LLM 调用率 | MuCGEC FN | cscdns clean/病句放行 |
|---|---|---|---|---|
| 现表 | 1.00M | 63.1% | **30.6%** | **3% / 1%** |
| +midu @1.5M | 1.50M | 60.8% | 32.7% | 5% / 5% |
| +midu @3M | 3.00M | 44.6% | 48.5% | 14% / 12% |

灰区率下降（cscdns 68.4%→19.3%）是 clean 与 err **同步平移**（加权 OOV 的
clean-vs-err AUC 仅 0.47），母语判别比 3:1 → 1.2:1，且阈值下调救不回漏检。
结论：`--extra-clean` 保留为可选开关，**默认不用**；现表规模（1M/300K）保持。

注意：换表后 `l3_data/zh/{train,dev}.features.jsonl` 的 `ngram_oov` 特征分布已变
（`l3_finetune._ngram_oov` 故意保持未加权口径以与已训练模型一致）——zh 模型重训练
（工作流 C）时须用新表重新 extract。

### 工作流 C 结论（2026-10-18：新表特征重训练 + 门控评测）

用新表重新 extract（47 万句）+ 重训练 zh 模型：池 dev F1 **0.478 → 0.549**。
但 `eval_zh_model_gate.py` 跨域门控评测证明**模型不能做门控兜底**：

- 跨域 AUC（err vs clean）：CSCD 0.63 / NaCGEC 0.50 / MuCGEC 0.50——
  聚合特征在池 dev 上的微弱信号不泛化（印证上文「聚合特征无法区分语法错句
  vs 干净句」）
- 模型概率整体偏高（几乎全部 ≥0.4）：任何阈值下 clean 放行率 ≈0%，
  max(模型, 启发式) 被模型吞没——dev F1 掩盖了「概率分布不分离」的本质
- **决策维持**：门控保持启发式（`correct.zh_heuristic_prob`，默认
  `L3_GRAY_ZONE_LOW=0.30`）；zh 模型仅保留类型分类用途（`types` 头）。
  进一步放宽弃权率（0.40 档 -36%）的前置条件不是本模型，而是**字级判别
  模型**（CEC ELECTRA discriminator，`CEC_ADOPTION.md` D.2，未做）或
  灰区句全量定向复查（A 已保证成本只按灰区句计）

### D：`L3_ABSTAIN_RATIO` 放宽 0.20 → 0.80（2026-10-18）

A（定向复查）落地后，段落级「任一句需裁定 → 整段全文送 LLM」的旧保守规则
只剩成本没有正确性价值（灰区句已必审、无静默丢弃）。放宽扫描
（`eval_l3_funnel.py`，MuCGEC 最坏情形——63% 句灰区）：

| `L3_ABSTAIN_RATIO` | 全文复查段 | LLM 输入 | Δ vs 0.20 |
|---|---|---|---|
| 0.20（旧默认） | 94% | 48062 字符 | — |
| 0.60 | 41% | 37911 | -21.1% |
| **0.80（新默认）** | **10%** | **35178** | **-26.8%** |
| 1.01（永不全文） | 0% | 35178 | -26.8% |

0.80 与「永不全文」成本相同，但保留错误密集段（>80% 句灰区）的全文语境
复查——跨句语境（指代/连接词类错误）只在定向复查中丢失，该兜底覆盖最可能
出现此类错误的场景。MuCGEC 段落级 L3 定案率 **5.7% → 90.4%**。

### E：接入 CTCDataset 原生评测集（2026-10-18）——修正成本预期

`~/github/CTCDataset`（开源中文纠错数据集合集，统一 `{source,target,label}`
jsonl）经重叠核验后接入三个**完全未污染**的评测集（与 l3_pool_raw 及既有
gold 零重叠；`datasets/prepare_ctc.py` 转换，schema 同 cscd_gold）：

| gold 文件 | 来源 | n | 特性 |
|---|---|---|---|
| `cscdns_gold.json` | cscd-ns/test | 5,000 | 原生微博母语者长句（真实流量最佳代理） |
| `nlpcc23_gold.json` | NLPCC2023 task8 | 1,000 | 人工标注拼写检查 50/50 |
| `cctc_gold.json` | CCTC test | 2,309 | **跨句**母语者纠错（语境评测用） |
| `sig13/sig14_gold.json` | yacsc sighan_revised 13/14 | 1,000 / 1,062 | 修订版 SIGHAN（泛化性，长句密错） |
| `midu_gold.json` | midu2022 preliminary_val | 1,010 | 真实场景长文 50/50（蜜度 2022） |
| `ctc21_gold.json` | CTC2021 val_revised | 969 | 真实场景长文校对集 |

全部已加入 `prepare_l3_pool.load_blocklist()`（永不进 n-gram 表/训练池），并已注册进
测评中心（`eval_server.BENCHMARKS`：`cscdns / nlpcc23 / sig13 / sig14 / cctc / midu / ctc21`）
→ 层级归因、消融、漏斗、A/B 可直接在这些原生集上跑（此前只有离线脚本能碰）。

重叠判断已由 `datasets/ctc_audit.py` 实测取代估计（2026-10-19）：
- 已进训练资产（不可再作评测）：Wang271k 98.9%、lemon 92.8–99.9%、cscd-ns train/dev
  99.7/99.6%、ECSpell 80.3–87.4%、sighan14/15 train 96–97.6%、**MCSCSet test/valid 100%**；
- 仍是增量（重叠 ≈0%）：**CTC2021/train_large_v2 217,617 对**（OCR/形近合成噪声 → 只喂
  字级判别侧车与 n-gram 干净侧，不入 L3 句级池）、midu2022/final 2,907、CCTC/train 2,150、
  MCSCSet/train 157k、NLPCC23/grammar/HSK 未覆盖的约 6.1 万对；
- ⚠️ 同批修正：`cscd_gold.json` 的 **sig15 split 换成 yacsc 校对版**（补漏标 67 句、
  编辑 644→792）→ 下表与本文件引用的 sig15 数字需重跑。

门控矩阵（`eval_l3_funnel.py` 真实用户段，`GRAY_ZONE_LOW=0.30`）：

| 数据 | clean 直接放行 | 病句漏检 |
|---|---|---|
| confirmed（生产 511） | 70% | — |
| cscd_gold | 21% | 10% |
| cctc（跨句长文） | 10% | 4% |
| nlpcc23 | 9% | 3% |
| nacgec | 8% | 5% |
| **cscdns（微博长句）** | **3%** | **1%** |

**成本预期修正**：confirmed 70% 放行率偏短偏模板化，不代表自然长文本——
原生长句上启发式门控几乎不放行（3-10%），句级跳过收益趋零但漏检也极低
（1-4%，安全侧）。长文本域的实际节省主要来自 **A（定向复查）+ D（段落
放宽）**；句级门控要在此域产生收益，唯一路径是字级判别模型（待办不变，
优先级升高）。CCTC 同时是「定向复查丢失跨句语境」的评测工具（待做）。

### F：pattern 层覆盖 / 门控可见性评测（`eval_pattern_coverage.py`，编辑粒度）

对三个原生 gold 集的全部 gold 编辑（非句子）测三件事——pattern span 重叠、
严格覆盖（hit.correction 含 gold ref，可本地定案）、门控可见（所在句
pattern 命中或启发式灰区 → 会送 LLM）：

| 数据 | char-sub 严格覆盖 | 全部类型门控可见 |
|---|---|---|
| cscdns | 5/2271 (0%) | 97-99% |
| nlpcc23 | 3/528 (1%) | 97-100% |
| cctc | 17/844 (2%) | 96-97% |

结论：
- **母语者错误几乎全在 pattern 层盲区**（覆盖 0-2%），但门控可见性 96-99%
  ——系统对原生文本「安全但贵」：错误基本都送 LLM，干净文本也基本都送
- 未覆盖 char-sub 高度集中且系统性：的→地/的→得（cctc 318 条）、做↔作、
  在↔再、象→像、带→戴、那→哪——全部依赖 POS/语境，正则词表不可安全达
  （印证 `cat_rule=particle/other` 分类）；pattern 层的价值在**高精度定案**
  而非错误探测，扩充应走 advisory 灰区信号 + 字级模型路线
- 评测噪音记录：cscdns `惟→唯`×53 为 cscd-ns 语料归一化产物（非真实错误），
  该集错误率实际略低于 46%

### G：字级判别模型（D.2 落地）——ELECTRA CTC 微调 + PLA 救援门控

**路径**：`hfl/chinese-electra-180g-base-discriminator` 的 CTC 头天然输出
每字「被替换过」概率（`ElectraForPreTraining`），无需换架构。

1. **零样本基线**（`eval_char_pla.py`，句级分数 = top-3 字概率均值）：
   AUC cscd 0.835 / MuCGEC 0.785 / nlpcc23 0.731 / cctc 0.686 / nacgec 0.641 /
   **cscdns 0.529**（原生微博 ≈ 随机；长度分桶平坦 → 领域失配非长度稀释）
2. **微调**（`finetune_char_ctc.py`，`.venv_char` = py3.12 + torch + MPS）：
   池 77K 对（去污染块表过滤 + 长度 ≤110），标签 = SequenceMatcher 字级对齐
   （replace/delete→1），干净侧 1:1 负例校准，1 epoch（~58min）→
   `models/char_ctc_zh/`（不入库）。AUC：**cscdns 0.737**（+0.21）/
   cctc 0.798 / nacgec 0.830 / MuCGEC 0.845 / cscd 0.877 / nlpcc23 0.717
3. **校准发现**：分数分布随域漂移——confirmed（短口语生产文本）clean p50
   0.125 vs cscdns clean p50 0.079。**τ_hi（PLA 高分直接进灰区）因此不启用**：
   会把已放行的生产文本拉回灰区（域校准未解，风险不对称）
4. **上线策略 = 救援门控**：启发式灰区句若 PLA < τ（默认 0.01）→ 放行。
   两信号互补（OOV 表面异常 vs 语境字级合理性），全集合严格占优或持平：

| 数据（funnel 真实用户段） | clean 放行 off→on | 病句漏检 off→on |
|---|---|---|
| cscd | 21% → **31%** | 10% → 10% |
| cctc | 10% → **17%** | 4% → 5% |
| nlpcc23 | 9% → **15%** | 3% → 4% |
| cscdns | 3% → **8%** | 1% → 3% |
| nacgec | 8% → **10%** | 5% → 5% |
| confirmed | 70% → 70% | — |

   MuCGEC（学习者病句）救援仅 0.2%——PLA 正确地不救真错误。

**架构**：`char_gate.py` 侧车（`.venv_char`，端口 4811，MPS 推理 <50ms/段）；
`server.py` 经 `L3_CHAR_GATE_URL`/`L3_CHAR_GATE_TAU` 启用，侧车不可达自动
回退纯启发式；`/l3-detect` 契约与 Rust 侧零改动。

```bash
# 复现：零样本 → 微调 → 评测 → 侧车
HF_ENDPOINT=https://hf-mirror.com ./.venv_char/bin/python eval_char_pla.py
HF_ENDPOINT=https://hf-mirror.com ./.venv_char/bin/python finetune_char_ctc.py   # ~1h
HF_ENDPOINT=https://hf-mirror.com ./.venv_char/bin/python eval_char_pla.py --model models/char_ctc_zh
./.venv_char/bin/python char_gate.py &
L3_CHAR_GATE_URL=http://127.0.0.1:4811/gate ./.venv/bin/python3 eval_l3_funnel.py
```

### G2：域漂移深挖 + 定向复查语境验证（2026-10-18 续）

**句内对比归一（`eval_char_contrast.py`）——负结果**：假设「top-3 相对
本句中位数的反常度」域无关。实测 diff/maxdiff AUC 无增益（cscdns
0.736/0.752 vs t3 0.737），confirmed 漂移依旧（diff p50 0.116 vs 0.078）。
结论：漂移源于**训练分布**（口语碎片在 LM 下确实更反常），非分数尺度
→ 后处理归一失败，正解是域数据进训练。

**CCTC 语境损失（`eval_cctc_context.py`）——定向复查门控安全**：对 1078
个 gold span 比较全文 vs 句级隔离的 span-max PLA 概率：语境损失
（full 可检出而 iso 不可）**≤0.2%**（τ=0.05~0.30 全档）。原生跨句错误
基本是句内局部字级错误，按句打分的门控（启发式 + PLA）几乎无代价。
（注：这只量化**门控/检出**层；LLM 跨句**修正质量**损失是另一轴，
待真 LLM 评测。）

**v2 域校准微调（已跑）**（`finetune_char_ctc.py --confirmed-repeat 20`）：飞轮
confirmed 511 句 ×20 作干净负例（10180 例，77K→87K，75min），输出
`models/char_ctc_zh_v2/`（不入库，侧车默认已指向）。

- **漂移修复**：confirmed clean p50 **0.125 → 0.0055**（过采样强势回落；
  原生集 p50 仅微降 0.079→0.075）。注意 conf 数字含记忆效应（509 句进
  了训练），真实生产口语收益应打折看。
- **AUC 互有胜负**：cscd 0.877→0.897 / nlpcc23 0.717→0.762 / nacgec
  0.830→0.843；cscdns 0.737→0.725 / cctc 0.798→0.786（口语负例挤占的
  小幅代价，可接受）。
- **τ 网格（funnel 真实用户段，clean放行/err漏检）**：

| 数据 | off | τ=0.002 | **τ=0.004** | τ=0.007 | τ=0.01 |
|---|---|---|---|---|---|
| cscd | 21/10 | 23/10 | **27/10** | 35/10 | 42/11 |
| nacgec | 8/5 | 8/5 | **8/5** | 10/5 | 15/5 |
| cscdns | 3/1 | 3/1 | **4/2** | 7/2 | 10/4 |
| nlpcc23 | 9/3 | 10/3 | **12/3** | 18/5 | 26/7 |
| cctc | 10/4 | 11/4 | **15/4** | 22/5 | 29/6 |
| confirmed | 70 | 72 | **78** | 87 | 91 |

- **决策**：默认 τ=0.004（**等风险点**：漏检与 off 全平，仅 cscdns +3
  句；放行显著上升）。τ=0.007 为成本压力下的进取档（漏检 +1~2pp）。
  v1 时代的 τ=0.01 在 v2 分数尺度下已偏激进，废弃。
- **τ_hi（PLA 高分直接灰区）仍不启用**：v2 下 τ_hi=0.3 表面免费
  （cscd 检出 90.6→94.3%，conf 放行不变 70%），但 conf 不受伤是记忆
  效应假象（p50 0.0055 是被训进去的分数），真实口语流量上会误伤已
  放行文本——等真实流量回流再探。

**部署口径（G 门控，opt-in）**：
```bash
./start_l3.sh                                     # 标准启动：侧车→健康检查→server
# 或手动分步：
./.venv_char/bin/python char_gate.py &            # 默认 v2 模型，端口 4811
L3_CHAR_GATE_URL=http://127.0.0.1:4811/gate ...   # server/eval 启用（默认关）
# τ 默认 0.004，无需显式设置；侧车不可达自动回退纯启发式
```
`GET /l3/status` 的 `char_gate` 字段暴露 enabled/url/tau + calls/fails/
scored/rescued 计数——上线后救援率与侧车健康可观测（τ_hi 最终决策的
真实流量数据来源）。
**MPS 共驻注意**：llama-server（CEC4）与 char_gate 侧车/CTC 训练共用
Metal 会致 ggml abort——同机部署需错开（评测时串行），或侧车用 CPU
推理（`CHAR_GATE_DEVICE=cpu`，慢 ~4× 仍 <200ms/句）。

### H：LLM 跨句语境损失（`eval_llm_context.py`，2026-10-18）

方法：CEC4（`models/cec4.Q8_0.gguf`，llama-server :8081）走生产 **plain**
契约（`prompts/zh_detect_cec_prompt.json`；纠正句↔原文 difflib 对齐生成
编辑，镜像 gec-core `detect_plain`）。full = 整条送审 vs targeted = 仅
启发式灰区句送审；对每个 gold 编辑判检出/修正正确。

结果（cctc_gold 400 错行 + 150 clean 行，n=332 编辑，318 已送审）：
- 检出：full 16.6% vs targeted 16.0%；修正正确：full 13.0% vs targeted 12.3%
- **语境损失 0.0%**（0/332）——LLM 修正质量轴无回退
- clean 行过度纠正：full 19.3% vs targeted **15.3%**（定向更低：送审句少，
  乱改机会少）
- 14 个 gold 编辑在未灰区句（门控层效应，与 G2 的 ≤0.2% 一致）

结论：**定向复查在门控 + 修正两层语境损失均可忽略，成本决策端到端成立**
（CEC4 绝对能力低是本地基线属性，不影响 full-vs-targeted 相对结论）。

**评测副产品（生产 bug，已修 `f0827d1`）**：CEC4 对长输入输出「纠正句\t原句回显」，
而 gec-core `split_think_answer` 只按行/答案标记切分、不处理 `\t`——回显
会被字级 diff 成假插入错误。评测侧按「取首个 `\t` 段」口径；生产侧
已加 `resolve_tab_echo`（llm.rs，以原文为锚剔除回显段）+ 4 个单测。

### I2：holdout 纪律实验（v3 / v3b，2026-10-18）

confirmed 按 md5 稳定切 20% holdout（`l3_data/zh/confirmed_holdout.json`，
93 句，跨版本一致），不进训练，供去记忆化校准。

- **v3**（holdout，无 repeat）：原生集救援全面弱于 v2（cscd 22 vs 27，
  cctc 11 vs 15）——弃。
- **v3b**（holdout + trainsub×20 repeat，83K pairs / 4067s）：
  - AUC（vs v2）：cscd 0.875/0.897、nlpcc23 0.723/0.762、nacgec
    0.825/0.843 略降；cscdns 0.741/0.725、cctc 0.787/0.786 略升。
  - funnel 救援（τ=0.004，clean放行/err漏检）：cscd 24/10（v2 27/10）、
    cctc 12/4（v2 15/4）、nlpcc23 10/3（v2 12/3）——**均不及 v2**，
    但仍优于 off（cscd 21、cctc 10）。
  - **诚实口径关键数**：holdout（未见过的生产口语）clean p50 = **0.107**
    （trainsub 记忆参照 0.008）→ **域校准微调对未见口语几乎不泛化**，
    v2 的 conf 放行 78% 里口语部分基本是记忆效应。
  - holdout 救援放行 72.0% = heur-only 基线 72.0% → **PLA 救援在真实
    未见生产口语上增益 ≈ 0**；救援的真实价值在原生集（未见、增益真实：
    cscd clean 放行 +6pp、cctc +5pp）。
  - τ_hi 在 holdout 上误伤明确（τ_hi=0.3 送审 28%→37.6%）→ **τ_hi
    维持关闭**（G2 的「记忆假象」担忧被证实）。

**决策**：生产默认保持 **v2 + τ=0.004**（原生集救援最强；conf 数字
按记忆效应打折理解）。v3/v3b 仅留作校准纪律实验。真实口语流量的
PLA 增益预期为小，上线后以 `/l3/status.char_gate` rescued 计数复核。

### I3：`--extra-clean` 双臂 A/B（abA / abB，2026-10-19）——§13 #5 关闭

midu2022 初赛干净侧作**字级**判别侧车 clean 负例（§12.3 的「唯一尚未排除路径」）。
双臂落 `models/char_ctc_zh_abA` / `models/char_ctc_zh_abB`，基线 = 生产默认 **v2**。

⚠ **双臂档不可考**：训练时还没有 `train_meta.json`，`/tmp` 训练日志已被系统清理，
`--extra-clean-max/repeat` 具体档无法从资产读回。可复核的只有两个 checkpoint、
两份 scores 和下面的配对结论；重跑该实验现在会自动落 meta（见本节末）。

配对评测（`eval_char_pla_ab.py char_ctc_zh_v2 <arm>`，22,549 句，9 原生 + MuCGEC + holdout）：

| 臂 | 原生 AUC | clean p50 | 等漏检放行率（vis 99/97/95/90 档 Δpp 均值） | 判定 |
|---|---|---|---|---|
| abA | 8/9 集**下降**（cscd 0.901→0.889、nacgec 0.843→0.832、cctc 0.786→0.778；sig13 0.840→0.861 唯一升） | 9/9 集平移 **up**（cscd 0.027→0.053） | +2.33 / −0.36 / +1.03 / −0.38 | **判负**（仅 1/4 档 ≥2pp + AUC 净降 = 纯分数平移） |
| abB | 7/9 集下降且崩：midu 0.792→**0.529**、nlpcc23 0.762→0.591、ctc21 0.821→0.718（cscdns 0.725→0.733 唯一升，cctc 持平） | 方向不一（cscdns 0.0745→0.0885 升 / midu 0.026→0.012 降，2 升 7 降） | −5.99 / −10.13 / −10.42 / −11.47 | **判负**（放行率全档大幅负） |

结论：干净长文负例只是把模型「母语长文打 clean 更高/更低」的**分数分布整体平移**，
判别比不改善——与 §12.1 n-gram 表同形态结论（成本↔安全滑动条）重复在字级侧。
**生产 default 保持 v2**；abA/abB 仅留作 A/B 记录（`models/char_ctc_zh_ab*`）。

**判定器口径已修**：`eval_char_pla_ab.py` 旧版按「最好一档」自动判定，abA 会被误报
「通过候选门槛」（+2.33pp 单档）；现按 §13 原文——**多数档 ≥2pp 且 AUC 未净降**才通过，
并同时打印 AUC / p50 的升降集数以暴露「纯平移」。

**复产纪律缺口已修**：`finetune_char_ctc.py` 现在落 `models/<out>/train_meta.json`
（argv / seed / counts / dev loss+AUC / secs）——abA/abB predates it，复该实验须重跑。


## `/l3-detect` 契约

请求：

```json
{ "text": "I go to school yesterday.", "lang": "en" }
```

响应：

```json
{
  "has_error": true,
  "has_error_prob": 0.9,
  "error_type": "word_order",
  "abstain": false,
  "errors": [
    { "span": [2, 27], "error_type": "word_order", "confidence": 0.9,
      "original": "I go to school yesterday", "correction": "I went to school yesterday",
      "message": "时态不一致（现在时动词 + 过去时间状语）" }
  ]
}
```

- `errors[].span` 为**字符偏移**（与 Rust 侧 `ErrorItem.span` 一致）
- `abstain` 弃权信号：`true` 时 Rust 侧触发 LLM 长尾兜底
- `correction` 为 `null` 时 = 模式无法机械修正（仅报告，前端展示 message）
- 健康检查：`GET /health` → `{"status": "ok", "mode": "mock"|"model"|"heuristic-fallback"}`

## 与 vegaml 的关系

与 `decision-model/` 相同的适配策略：`l3_finetune.py` 优先使用 vegaml 训练入口；
vegaml 不可用（未安装 / API 变动）时回退 sklearn `LogisticRegression`
（句级二分类 + 错误类型多分类，joblib 打包）。`server.py` 的 `/l3-detect` 契约
与模型内部解耦。

## 模型演进（训练飞轮，三期闭环）

与二期 anomaly 蒸馏同源的自学习思路：**特征模型随使用变强，LLM 长尾持续收缩**。

```
LLM 检出错误 ──用户「采纳」──▶ l3_data/<lang>/confirmed.jsonl（正样本）
检测无错误（LLM 复查确认干净）──▶ 同上（负样本，source=llm_clean）
                                        │ 按 文本+标签 去重，上限 5000 条
                                        ▼
        GET /gec/l3/status：样本 ≥ 阈值（新增 ≥10 / 首次 ≥30）且冷却 ≥7 天
                                        │  前端设置界面「L3 训练飞轮」卡片提醒
                                        ▼
        POST /gec/l3/retrain → 本服务 POST /l3-retrain
            ① 合并 confirmed 样本进训练集（重抽特征）
            ② 重训练（vegaml 优先，回退 sklearn）
            ③ dev 集评测：整体 F1 退化 > 0.02 或逐错误类型召回退化 > 0.05（防 LLM 盲区固化）
               → 保留旧模型；否则热切换（内存 + 磁盘）
            ④ 写 retrain_meta.json（Rust 侧据此判断冷却期）
```

- 采集入口（Rust 侧，api-server / desktop-app 同构）：
  - `POST /gec/l3/collect`（前端「采纳」LLM 错误时调用；桌面端 `l3_collect` 命令）；
  - 检测未产出错误时 `detect` 路由自动采集干净句为负样本（非阻断）；
- 提醒阈值：上次重训练后样本 ≥ 10（从未训练过则累计 ≥ 30）且距上次重训练 ≥ 7 天；
- 热切换：模型在内存中替换（`model.models[lang]`），后续 `/l3-detect` 立即使用新模型，
  无需重启服务；`retrain_meta.json` 记录每次重训练的时间 / 样本数 / F1。

## 采纳平行语料资产（adopted / rejected）

比 confirmed（句级正/负标签）更值钱的长期资产：**用户亲手确认的 `src→dst` 纠正对**。

```
用户在结果列表「采纳」──▶ 前端送 src_text + 累积修正 corrected_text
        │ 服务端切句对齐（句数一致逐句配对；否则整体兜底）
        ▼
l3_data/<lang>/adopted.jsonl   {"src","dst","weak","source","ts"}（去重 src|dst，上限 20000）
用户「放弃」──▶ l3_data/<lang>/rejected.jsonl（片段级负偏好：fragment + 被拒修正 + 原因）
        │
        ▼  治理（去污染 + 切分）
python prepare_adopted.py --lang zh
        ▼
adopted_train.jsonl（未来 SFT / 继续预训练 / 自定义模型素材）
adopted_holdout.jsonl（md5 稳定 holdout：未来自定义模型评测集）
```

- **weak 标记**：句对编辑密度（1-LCS/max_len）> 0.4 → 多半是用户改写而非纠错，
  默认从训练池剔除（`--include-weak` 保留）；
- **去污染**：与 `prepare_l3_pool` 同一基准块表（MuCGEC/CSCD/NaCGEC/CCTC/NLPCC23
  gold 源句），评测集句子绝不进训练资产；
- **md5 稳定切分**：`md5(src)` 决定 holdout 归属，增量追加不改变已有句对切分；
- **隐私开关**：`GEC_L3_ADOPT_COLLECT=0` 关闭全部采纳/放弃沉淀（默认开，
  自用产品定位；多租户部署必须关闭）；
- `rejected.jsonl` 是未来偏好数据（DPO 式「模型建议 X 但用户拒绝」）的原料，
  当前只沉淀不消费。
