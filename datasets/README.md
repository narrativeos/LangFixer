# datasets/ — 外部语料与 GEC 基准流水线

> **许可声明**：本目录数据文件为**第三方研究语料及其派生 gold**，各保留其原始许可，
> **不在**本项目的 Apache-2.0 授权范围内，多数为**研究/非商业用途**：
>
> | 数据 | 来源 | 许可 |
> |---|---|---|
> | `jfleg/`、`jfleg_gold.json` | JFLEG（CoNLL-2014 谱系，keirks/jfleg） | 非商业研究语料（CC BY-NC-SA 口径，以上游为准） |
> | `mucgec/`、`mucgec_gold.json` | [MuCGEC](https://github.com/HillZhang1999/MuCGEC) | Apache-2.0 |
> | `cscd/`、`cscd_gold.json` | [twnlp/csc_data](https://huggingface.co/datasets/twnlp/csc_data)（CSCD + SIGHAN-2015/EC-Med/EC-LAW） | 研究用途（上游未明确授予商业再分发） |
> | `nacgec/`、`nacgec_gold.json` | [twnlp/cgc_data](https://huggingface.co/datasets/twnlp/cgc_data)（NaCGEC） | 研究用途 |
> | `*_gold.json`（cscdns / nlpcc23 / cctc / sig13 / sig14 / midu / ctc21） | [CTCDataset](https://github.com/kedarting/CTCDataset) 合集转换 | 各子集许可不一，研究用途 |
> | `cec_dat_data/` | [ChineseErrorCorrector](https://github.com/TW-NLP/ChineseErrorCorrector) | MIT |
>
> 仅用于本地评测与词表治理，**产品发行物（Docker 镜像 / 桌面安装包）不包含本目录**。
> 商用部署前请逐集核对上游许可。本目录脚本（`*.py`）本身按 Apache-2.0 授权。

## jfleg/

[JFLEG](https://github.com/keirks/jfleg) 英文 GEC 数据集（CoNLL-2014 谱系）：
`dev/` 754 句、`test/` 747 句，每句含 4 个人工修订（`{split}.ref0`–`ref3`），
与 `{split}.src` 按行号一一对应（无对齐元数据）。

数据文件直接提交在本仓库（约 880K）；原仓库的 `train/`（10,000 句）、
`EACL_exp/` 与论文 PDF 已移除，仅保留评测用 dev/test 与 `eval/` 脚本。

## mucgec/

[MuCGEC](https://github.com/HillZhang1999/MuCGEC)（NAACL 2022，Apache-2.0）
**多参考多来源中文 GEC 评测集**——JFLEG 的中文版：
- `MuCGEC_dev.txt`：1137 句，每句 `id \t 原句 \t 参考1 \t 参考2 ...`
  （参考数 0–4 不等，「没有错误」表示该修订者认为无误）
- `MuCGEC_test.txt`：6000 句（仅原句，官方答案在天池在线榜单，本地不可评测）
- `filter_sentences.txt`：7137 句，训练集必须剔除（与评测集重叠）
- 来源：NLPCC18 / CGED / Lang8 各采样 2000–3000 句，三人标注 + 专家审核
- 官方指标为字级 F0.5（ChERRANT，ERRANT 风格）；本仓库基准改按
  `cat_rule`（规则可及性）统计召回/误报，见下方流水线

数据文件直接提交在本仓库（约 2.2MB）。

## nlpcc2018/（不入库）

[NLPCC2018](https://github.com/keirks/nlpcc2018) 中文 GEC 数据集（约 170MB），
本地克隆仅供 HanLP 中文管线实验使用，已在根 `.gitignore` 中排除。
需要时自行 `git clone https://github.com/keirks/nlpcc2018 datasets/nlpcc2018`。

## cec_datasets.md（ChineseErrorCorrector 开源数据清单）

[cec_datasets.md](cec_datasets.md) 整理了 [TW-NLP/ChineseErrorCorrector](https://github.com/TW-NLP/ChineseErrorCorrector)
（ACL 2026 Main Oral）开源的 5 个 HuggingFace 数据集（2M SFT / 34 万 COT / 38 万拼写 /
6 万语法 / 157 万混合，均 MIT 或 Apache-2.0）及其评测基准（CSCD、NaCGEC、
SIGHAN-2015/EC-LAW/MCSC 等），作为后续训练与测试的数据来源清单；
其 `dat_data/` 混淆字集已通过 `curate_cec_confusables.py` 导入
`zh_gec_lexicon.json`（76 条，见上方中文基准流水线）。

## 英文基准流水线（JFLEG）

| 脚本 | 作用 |
|------|------|
| `analyze_jfleg.py` | 词级 diff（src vs 4 refs），把每个编辑分类（tense/agreement/article/preposition/adverb/comparative/gerund-inf/spelling/other），输出分布报告 |
| `extract_prep_patterns.py` | 提取介词误用的上下文模式（prev/next 词），评估可否泛化为规则 |
| `expand_lexicon.py` | 从语料高频错词对自动扩充 `lexicons/en_gec_lexicon.json`（misspellings / uncountable / past_tenses / common_verbs） |
| `curate_lexicon.py` | 人工筛选上一步的自动结果：只保留"非真实词→正确词"的高精度拼写对，剔除单复数噪声，补 `EN_SPELLING` 规则元数据 |
| `make_gold.py` | 生成 `jfleg_gold.json`：每句的去重编辑列表，含预计算字节 span、`cat`（完整分类）与 `cat_rule`（规则可及性分类） |

Rust 侧评测：`gec-en/tests/jfleg_eval.rs`（`cargo test --test jfleg_eval`）
读取 `jfleg_gold.json`，对每条源句跑 `detect_local`，统计按 `cat_rule` 的
召回率（预测 span 与 gold span 重叠）与误报率（未命中任何 gold 的预测）。
设置 `JFLEG_VERBOSE=1` 可打印逐条误报明细。

## 中文基准流水线（MuCGEC）

| 脚本 | 作用 |
|------|------|
| `analyze_mucgec.py` | 字级 diff（原句 vs 各参考），把每个编辑分类（punct / char-sub / word-sub / deletion / insertion），输出分布与高频编辑对 |
| `make_gold_zh.py` | 生成 `mucgec_gold.json`：每句去重编辑（去重键含 span，同一错字多次出现各记一条），含字节 span、`cat` 与 `cat_rule`（punct / particle / variant / confusable / other）。需 `datasets/.venv`（pypinyin 判同音） |
| `curate_lexicon_zh.py` | 人工筛选 `lexicons/zh_gec_lexicon.json`：移除"错误形式是常见正确字"的危险条目（做/即/既/按/其它/报复/抱负），新增繁体→简体对（100% 安全）与罕用字同音对（岐/吾） |
| `curate_cec_confusables.py` | 导入 CEC（ACL 2026）`dat_data/` 混淆字集（token_set / new_cofuse_set，51 万原始对）：方向按「键=常见字」梯度判定（双方都是双文件键 = 都常见 → 剔除），主读音精确匹配（pypinyin 主读音；全异读集含古音会误判），OpenCC 过滤「繁体→非对应简体」对（於→余、俠→峽 类），分层（tier 2 = right 双文件键且 wrong 非键；tier 1 = wrong 单文件键）。幂等 + 合并前自动备份；全量候选另写 `cec_confusable_candidates.tsv` 供复核。`--apply --tier 1` 已合并 76 条高精度条目（69 繁→简 + 7 同音罕用字，如 丟→丢、菸→烟、甯→宁） |

Rust 侧评测：`gec-zh/tests/mucgec_eval.rs`（`cargo test --test mucgec_eval`），
结构与 `jfleg_eval.rs` 相同；`MUCGEC_VERBOSE=1` 打印误报明细。
`cat_rule` 分类依据：
- `punct`：标点编辑（当前本地规则仅覆盖 ASCII→全角；全角↔全角 归 LLM 路径）
- `particle`：的/地/得、在/再、象/像/向 等依赖语境的虚词对（POS/LLM 路径）
- `variant`：繁体→简体（词表可 100% 安全命中）
- `confusable`：同音/形近对（拼音相同；词表可命中，需人工筛选安全性）
- `other`：词义替换/增删/长块替换（本地规则不可达）

## CEC 评测基准（CSCD / NaCGEC）

CEC（TW-NLP/ChineseErrorCorrector，ACL 2026）的评测基准已本地化（`cec_datasets.md`
有完整数据清单）：

| 脚本 | 作用 |
|------|------|
| `cec_common.py` | 共享工具：HF 下载（hf-mirror 镜像优先，`HF_ENDPOINT` 可覆盖）、字级切分行还原、OpenCC 繁→简判定、gold 编辑生成（与 `make_gold_zh.py` 同构） |
| `prepare_cscd.py` | 下载 `twnlp/csc_data` 4 个测试文件 → `cscd/`，生成 `cscd_gold.json`（2600 句）：`cscd`（CSCD 500 句，拼写主基准）+ `sig15`（SIGHAN-2015，1100 句）+ `med`（EC-Med，500 句）+ `law`（EC-LAW，500 句）三个泛化性测试集 |
| `prepare_nacgec.py` | 下载 `twnlp/cgc_data/nacgec_all_type.json` → `nacgec/`，生成 `nacgec_gold.json`（6369 句，多参考去重编辑 + `error_type` 语法错误类型标签） |
| `prepare_ctc.py` | 转换本地 `~/github/CTCDataset` 未污染评测集 → `cscdns_gold.json`（原生微博 5000）/ `nlpcc23_gold.json`（NLPCC2023 task8 1000）/ `cctc_gold.json`（CCTC 跨句 2309）/ `sig13_gold.json`（SIGHAN-2013 修订版 1000）/ `sig14_gold.json`（SIGHAN-2014 修订版 1062）/ `midu_gold.json`（蜜度 2022 初赛验证 1010）/ `ctc21_gold.json`（CTC2021 校对验证 969）；schema 同 cscd_gold；全部源句已入去污染块表。默认还把 `cscd_gold.json` 的 **sig15 split 换成 yacsc 校对版**（补漏标 67 句、编辑 644→792；`--keep-sig15` 跳过）。前三个 gold 保持 difflib 历史口径，新集走 `cec_common.diff_edits`（cat_rule 完整，需 `./.venv/bin/python`） |
| `score_f05.py` | **Fβ 计分器**（ChERRANT 字级口径）：`--gold`/`--pred` 同 gold 格式、按 `src` 对齐；编辑级 P/R/Fβ（`--beta 0.5` = NaCGEC 口径，`--beta 1` = CSCD Correction F1）+ 句级门控 F1（ELECTRA 门控口径）；`--selftest` 合成数据自检 |
| `ctc_audit.py` | **CTCDataset 利用率审计**：逐子集与 `l3_pool_raw`（src∪ref）/ 全部 gold 源句做句子级重叠，输出 {去重句数, 有错率, 中位句长, pool%, gold%, 干净句数} → 判定「已污染 / 已用 / 未利用」。结论快照写在脚本 docstring，接入新集前先跑它 |
| `gold_error_dist.py` | **错误分布对照器**：各 gold 的编辑类型（S/D/I）+ 单字替换同音占比，并可对任意候选语料 `{source,target}` 跑同口径 diff → 判定「错误生成机制是否与真实流量同源」。真实流量基准 cscdns S 99.4% / 同音 80.9%；已否定的伪数据 S 44.7% / 同音 24.5%。接入新语料进训练池或扩混淆字词表前先跑它（EVOLUTION §13） |

Rust 侧评测：`gec-zh/tests/cscd_eval.rs`（`cargo test -p gec-zh --test cscd_eval`），
结构与 `mucgec_eval.rs` 相同（`CSCD_VERBOSE` / `NACGEC_VERBOSE` 打印误报明细）。

### CTCDataset 原生评测集（2026-10-19 接入，`prepare_ctc.py` + `ctc_audit.py`）

`~/github/CTCDataset`（开源中文纠错数据集合集，统一 `{source,target,label}` jsonl）经
`ctc_audit.py` 逐子集重叠审计后接入 **7 个零污染评测集**（与 `l3_pool_raw` 及既有 gold
重叠 0%），全部已注册进测评中心（`eval_server.BENCHMARKS` + web-ui「进化中心」）：

| 基准 id | split | 句数 | 含错 | 编辑 | β | 定位 |
|---|---|---|---|---|---|---|
| `cscdns` | cscdns | 5,000 | 46.0% | 2,406 | 1.0 | 原生微博长句——真实流量最佳代理 |
| `nlpcc23` | nlpcc23 | 1,000 | 50.0% | 530 | 1.0 | 人工标注拼写检查 50/50 |
| `sig13` | sig13 | 1,000 | 97.8% | 1,456 | 1.0 | SIGHAN-2013（yacsc 修订版，长句密错） |
| `sig14` | sig14 | 1,062 | 56.7% | 863 | 1.0 | SIGHAN-2014（yacsc 修订版） |
| `cctc` | cctc | 2,309 | 52.4% | 1,301 | 0.5 | CCTC 跨句母语者纠错（语境评测用） |
| `midu` | midu | 1,010 | 50.0% | 550 | 0.5 | 蜜度 2022 初赛验证（真实场景长文 50/50） |
| `ctc21` | ctc21 | 969 | 45.6% | 538 | 0.5 | CTC2021 校对验证集（真实场景长文） |

**sig15 gold 修订**：`cscd_gold.json` 的 sig15 split 原样来自 twnlp `test_sig_15.txt`，
与 yacsc 校对版比对有 **67 句漏标 / 2 句多标 / 52 句编辑改写**（962 句交集），且 137 句
因繁简/空白规范化不同而句集错位。`prepare_ctc.py` 默认把该 split 整体替换为校对版：
含错 542 → **618**、编辑 644 → **792**（+23%）。**此前引用的 sig15 数字全部作废，需重跑。**
（另：`cscdns_gold.json` 的 split 名由 `cscd-ns` 规范为 `cscdns`，与基准 id 一致——
`gate_eval_build.py` 输出的 `split` 字段随之变化，无代码依赖旧名。）

**审计结论（`ctc_audit.py`，勿凭记忆判断重叠）**：
- 已进训练资产、**不可再作评测**：Wang271k 98.9%、lemon 92.8–99.9%、cscd-ns train/dev
  99.7/99.6%、ECSpell 80.3–87.4%、sighan14/15 train 96–97.6%、
  **MCSCSet test/valid 100%**（⚠️ 若将来要报 CEC 的 MCSC 基准，已被自家 n-gram 表/L3 池污染）；
- **仍未利用（重叠 ≈0%）**：`CTC2021/train_large_v2`（217,617 对，抽样为 OCR/形近合成噪声
  ——`郡钩→这`、`煤恒→没有`——只适合喂字级判别侧车 `finetune_char_ctc.py` 与 n-gram
  干净侧，**不入 L3 句级池**）、`midu2022/final`（2,907，83% 有错）、`CTC2021/val`、
  `CCTC/train`（2,150 母语跨句对）、`MCSCSet/train`（157k，用前须与 test/valid 隔离）、
  `NLPCC2023/grammar/HSK`（61% 已覆盖，增量约 6.1 万对）、`yacsc/YACSC`（13.4/38.6% 已污染，
  作评测须按块表过滤）；
- 已接入评测的 gold 源句全部在 `prepare_l3_pool.load_blocklist()`（`prepare_adopted.py`
  同源副本同步）→ 永不进 n-gram 表与训练池。**块表变更后 n-gram 表/池需重建才生效。**
- **已评估、明确不接入（锁定，详见 `EVOLUTION.md` §13）**：
  `midu2022/preliminary_train`（100 万伪数据，外部下载 `preliminary_train.jsonl.gz`）——
  零污染（src/tgt ∩ gold = 0.00%）但 **100% 有错、零负例**，错误分布 S/D/I=45/32/23、
  单字替换同音仅 24.5%（真实流量 99.4% / 80.9%）：**不做评测**（伪 gold，用已接入的
  `midu` gold）、**不扩 n-gram 表**（实测 MuCGEC FN 30.6%→48.5%，判别比 3:1→1.2:1）、
  **不进 L3 句级池**（三臂 AUC 仅 +0.01~0.03）、**不扩混淆字词表**（OCR 形近对）；
  唯一未排除路径 = 字级侧车 `finetune_char_ctc.py` 的 clean 长文负例（未测）。
  通用推论（同样适用于任何外部干净语料）：**扩 n-gram 表只移动「成本↔漏检」工作点，
  不改善判别比**——验收口径必须是「等漏检下的放行率」，不是灰区率。

### 测评中心（进化中心 P0，`eval_server.py`）

按需基准评测服务（闭环「检测 → 反思 → 训练 → 测试」的测试环节，总纲见
仓库根 `EVOLUTION.md`）：

- **启动**：`EVAL_PORT=4807 python3 eval_server.py`（api-server 需已运行；
  `GEC_API` 可覆盖其地址，缺省 `127.0.0.1:4801`）
- **发起**：`POST /eval/run` `{benchmark, split?, arm?, mode?, ablation?, subset?, seed?, workers?, fresh?}`
  → 后台任务（`GET /eval/job/:id` 轮询进度）；`GET /eval/reports` /
  `GET /eval/report/:id` 取报告（JSON + markdown 落 `eval_reports/`，gitignore）
- **层级归因**：每个预测错误带 `source`（rule/decision/knowledge/feature/llm）；
  gold 编辑按级联序归因到首个重叠层 → 各层 P/R/Fβ + 召回份额 + 误报 + 句级门控；
  计分口径与 `score_f05.py`（严格 (op,ref,span)）/ `mucgec_eval.rs`（span 重叠）一致
- **消融**：`ablation: {decision: false}` / `{l3: false}` → 临时 `PUT /gec/settings`
  关层 → 重跑 → 恢复（Δ = 该层边际贡献）；**换臂**：`arm` 切 `llm_model_id`
  （cec3/cec4/generic/custom）跑同一基准
- **漏斗**（`mode: funnel`）：自然级联 → LLM 触达率 + 级联前缀累计解决率
- **互斥**：全局设置是共享状态 → 同一时间仅一个 run 任务（409）
- **消费端**：api-server `/gec/eval/*` 代理（`routes/eval.rs`）+ web-ui「进化中心」
  面板（`EvolutionCenter.vue`）+ 桌面端 4 个 Tauri 命令镜像
- **自检**：`python3 eval_server.py --selftest`（合成数据验证计分/归因/漏斗）

### 规则层基线（2026-10，220 条 confusables 词表）

| 基准 | 句子 | 预测 | 误报 | 召回（cat_rule） |
|------|------|------|------|------------------|
| cscd | 500 | 25 | 25 | confusable 0/247、other 0/95、particle 0/1 |
| sig15 | 1100 | **0** | **0** | confusable 0/450、other 0/267、particle 0/75（yacsc 校对版 gold） |
| med | 500 | 173 | 172 | confusable 1/230、other 0/79、particle 0/1 |
| law | 500 | 111 | 111 | confusable 0/218、other 0/129 |
| nacgec | 6369 | 4 | 4 | other 0/9428、punct 0/19、confusable 0/13 |

**结论**：CSCD/NaCGEC 是 LLM/L3 路径的基准——规则层召回 ≈ 0（拼写/语法编辑以
「常见字同音/词义替换」为主，不在可安全泛化区间）；med/law 的误报主要来自
ASCII→全角标点规则与泛化语料 gold 宽容度差异（与 MuCGEC 的 4 条标点误报同源）。
**sig15 换 yacsc 校对版后 4 条误报归零**（那 4 条本就是 gold 漏标造成的假误报），
同时 gold 编辑 +23%（374/214/56 → 450/267/75）——印证「误报数」在噪声 gold 上不可信。
CTCDataset 原生集（cscdns/nlpcc23/sig13/sig14/cctc/midu/ctc21）的规则层数字走测评中心
（`benchmark=cscdns` 等），尚未跑过。
LLM/L3 路径的 F0.5/F1 用 `score_f05.py` 计分（预测需转成 gold 格式，按 `src` 对齐）。

### 领域冷启动增强（F 工作流，`augment_domain.py`）

CEC 仓库 `GrammarErrorDat` 的移植（2024 CCL 冠军组件，14 种语法错误增强，
Apache-2.0）：用户领域干净文本（txt 每行一句 / JSONL）→ 合成 (source, target) 对。
混淆字表/搭配表 vendor 于 `cec_dat_data/`（5 文件，~1.5MB）。14 种中 9 种依赖 LTP
（`pip install ltp`，首次运行自动下载模型）；无 LTP 时 5 种纯类型可用
（wrong_word / lack_char / wrong_char / red_component / wrong_sentence_order）。
`--format pairs` 输出 {source, target, error_type, error_label}（L3 重训练/分析用）；
`--format sft --sft-prompt v3|v4` 输出 CEC business_data 风格 conversations（LLM SFT 用）。
固定种子可复现（默认 42，对齐 CEC `correct_tools.set_seed` 习惯）。

```bash
python3 augment_domain.py --input domain_clean.txt --output domain_aug.jsonl \
    --seed 42 --per-sentence 2 --types all --format pairs
```

## 词表条目条件范式（`EntryCondition`）

词表条目可附加**逐条上下文条件**：规则仅在条件满足时触发（`shared-model/src/condition.rs`）。
无条件的数组形式与带条件的对象形式**可混用**，旧词表文件无需改动。

```json
// 无条件（传统格式）：命中即触发
["岐", "歧", "「岐」罕用，多为「歧」之误"]

// 带条件（对象形式）：条件满足才触发
{
  "wrong": "象",
  "right": "像",
  "note": "「象」误用于「像」（如：好象→好像）",
  "condition": {
    "prev_text": ["好"],          // 前一 token 文本白名单（POS 路径）
    "next_text": ["使"],           // 后一 token 文本白名单（POS 路径）
    "prev_pos": ["VV"],            // 前一 token POS 白名单（POS 路径）
    "next_pos": ["AD"],           // 后一 token POS 白名单（POS 路径）
    "context_regex": "好象"        // 命中位置周边窗口正则（两条路径均可求值）
  }
}
```

求值语义（子条件之间为 AND；子条件为空 = 不限制）：

| 条件 | 在线（POS）路径 | 离线（本地）路径 |
|------|----------------|------------------|
| `prev_pos` / `next_pos` / `prev_text` / `next_text` | 对邻接 token 求值；邻接缺失 → 不满足 | **无法求值 → 整条跳过**（保守原则：无法验证即不触发） |
| `context_regex` | 对命中位置周边窗口（±4 字符）求值 | 同左 |

已接线的规则：
- `ZH_CONFUSABLE`（`gec-zh`）：token 路径与 scan 路径均支持；
  词表已含两条示范——`象→像`（`context_regex: 好象`，两条路径均可触发）、
  `既→即`（`next_text: 使/便/将/可`，仅 POS 路径触发）
- `EN_SPELLING`（`gec-en`）：本地路径支持 `context_regex`；
  纯 token 条件的条目在本地路径跳过

设计动机：此前 curation 只能"整条移除"危险对（做/即/既…）；条件范式允许把
"仅在特定上下文才是错误"的条目以高精度重新纳入，且离线路径永不因无法验证而误触发。
MuCGEC 基准验证：新增两条条件条目后 dev 误报仍为 4、各类召回不变。

## 当前基线（2026-10，扩充词表后）

### 英文（JFLEG，`gec-en`）

| 类别 | dev 召回 | test 召回 | 说明 |
|------|----------|-----------|------|
| spelling | 100% (26/26) | 100% (27/27) | 词表 misspellings 全覆盖 |
| agreement | 1.5% | 3.3% | he/she/it + 常见动词 -s |
| tense | 1.6% | 1.8% | 需句中出现 yesterday |
| article | 2.2% | 2.6% | 仅 a/an 元音/辅音规则 |
| gerund_inf | 0.8% | 1.7% | 动名词/不定式互误 |
| preposition | 0% | 0% | 介词误用多为上下文相关，无法安全泛化 |
| other | ~0.5-0.7% | — | 词义/改写类编辑，规则不可达 |

误报：dev 5 / test 4（每 split 总预测 59/71 条）。

### 中文（MuCGEC dev，`gec-zh`）

| 类别 | 召回 | 说明 |
|------|------|------|
| variant（繁→简） | 100% (20/20) | 词表 220 条 confusables 全覆盖（144 人工策展 + 76 CEC 导入） |
| confusable（同音） | 2.4% (2/84) | 仅保留罕用字对（岐/吾 + CEC 导入的 甯/吒 等）；常见字对（做/即/她…）本地命中即误报，已移除 |
| punct | 0.4% (1/226) | 仅 ASCII→全角；全角↔全角（。→， 等）归 LLM 路径 |
| particle（的/地/得…） | 0% (0/40) | 依赖 POS/语境，归 LLM 路径 |
| other | 0.1% (10/6888) | 词义替换/增删，规则不可达 |

误报：5（4 条为标点——源句含 ASCII 标点而修订者未改，属 gold 宽容度差异；
1 条为 雄貓→雄猫——gold 参考未修订该繁体字，纠正本身成立，属 gold 噪声）；
curation 前为 115（`做→作` 95 条为主）。

## 已知边界

- 规则引擎的召回上限由"可安全泛化"决定：
  - 英文：介词选择（in↔on↔to↔for）、冠词 a/the 选择、词义替换（can→would）等
    依赖语义，正则/词表无法覆盖
  - 中文：的/地/得 等虚词、全角标点选择（。→，）、词义替换（职业→工作）
    依赖 POS/语境；「常见正确字」同音对（做/即/她/一…）本地命中即误报
  - 条目条件范式（见上节）可把部分「特定上下文才是错」的对以高精度重新纳入
    （如 象→像、既→即）；纯语义对（做/即/她…）仍归 LLM 路径
  - 这些是 LLM 路径（HanLP tokens + 模型）的目标区间
- `jfleg_gold.json` / `mucgec_gold.json` 的 span 由 Python 预计算；若改动
  `make_gold.py` / `make_gold_zh.py` 的 diff 逻辑，需重新生成并确认
  `gec-en` / `gec-zh` 测试仍通过
- 扩充词表前请先备份：
  - `cp lexicons/en_gec_lexicon.json /tmp/lexicon_backup.json`
    （`expand_lexicon.py` 是幂等的，但会累加条目）
  - `cp lexicons/zh_gec_lexicon.json /tmp/zh_lexicon_backup.json`
    （`curate_lexicon_zh.py` 幂等；重跑前确认 REMOVE 集合与当前 JSON 一致）
