# cec_datasets — ChineseErrorCorrector 开源数据集清单

来源项目：[TW-NLP/ChineseErrorCorrector](https://github.com/TW-NLP/ChineseErrorCorrector)
（ACL 2026 Main Oral 论文 CSRP；2024 CCL 冠军；2023 NLPCC-NaCGEC 冠军；2022 FCGEC 冠军）。
其模型 CEC4-4B 在 NaCGEC（F0.5 = 50.99）与 CSCD（Correction F1 = 59.61，超 GPT-4）双榜 SOTA，
本清单整理其开源数据，供 LangFixer 后续训练与测试使用。

## 一、twnlp HuggingFace 数据集（可直接 `datasets` 加载）

| # | 数据集 | 链接 | 规模 | 格式 | 许可证 | 用途 |
|---|--------|------|------|------|--------|------|
| 1 | ChinseseErrorCorrectData | [HF](https://huggingface.co/datasets/twnlp/ChinseseErrorCorrectData) | 2,015,950（train 2.01M + validation 6,970） | conversations（human/gpt 两轮，v3 prompt「你是一个文本纠错专家…输入句子为：」），拼写+语法混合 | MIT | **主力 SFT 训练数据**（CEC3-4B 训练数据；737MB） |
| 2 | ChineseErrorCorrector4-4B | [HF](https://huggingface.co/datasets/twnlp/ChineseErrorCorrector4-4B) | 346,378（train 334,432 + test 11,946） | instruction/input/output；output = think 块（错误类型 + 修改原因）+ 纠正句 | Apache-2.0 | **COT SFT 训练数据**（CEC4 论文模型数据；118MB） |
| 3 | csc_data | [HF](https://huggingface.co/datasets/twnlp/csc_data) | 384,500（train 382K + test 2.6K） | 平行文本对（源句/纠正句，长度对齐为主） | MIT | **拼写纠错训练**；test 段即 CSCD 测试集 |
| 4 | cgc_data | [HF](https://huggingface.co/datasets/twnlp/cgc_data) | 60,270（train 57.8K + validation 2,470） | 平行文本对 | MIT | **语法纠错训练**；validation 段即 NaCGEC 测试集 |
| 5 | lang8_hsk | [HF](https://huggingface.co/datasets/twnlp/lang8_hsk) | 1,568,885 | 拼写+语法混合（Lang8 学员作文 / HSK 谱系） | MIT | **大规模混合训练**（229MB） |

### 子集构成（csc_data / cgc_data）

| 子集 | 行数 | 类型 | 说明 |
|------|------|------|------|
| W271K（SIGHAN 谱系） | 279,816 | 拼写 | 新闻语料 |
| Medical | 39,303 | 拼写 | 医疗领域 |
| Lemon | 22,259 | 拼写 | 口语/网络语料 |
| ECSpell | 6,688 | 拼写 | 电商领域 |
| **CSCD** | 35,001（test 2.6K） | 拼写 | **评测基准**（Correction F1） |
| CGED | 20,449 | 语法 | 学员作文 |
| FCGEC | 37,354 | 语法 | 2022 FCGEC 冠军数据 |
| MuCGEC | 2,467 | 语法 | 本仓库已有独立副本 `mucgec/` |
| **NaCGEC** | 7,568（validation 2,470） | 语法 | **评测基准**（F0.5，ChERRANT） |

## 二、评测基准（测试集）——建议的评测矩阵

| 基准 | 指标 | 来源 | 定位 |
|------|------|------|------|
| **CSCD**（csc_data 的 test 段） | Correction F1 | twnlp/csc_data | 拼写主基准（当前 SOTA 59.61）——**已本地化**：`cscd/` + `cscd_gold.json`（同含 SIGHAN-2015/EC-Med/EC-LAW 泛化性测试集） |
| **NaCGEC**（cgc_data 的 validation 段） | F0.5（ChERRANT） | twnlp/cgc_data 或 [masr2000/NaCGEC](https://github.com/masr2000/NaCGEC) | 语法主基准（当前 SOTA F0.5 50.99）——**已本地化**：`nacgec/` + `nacgec_gold.json` |
| MuCGEC | 字级 F0.5 / 本仓库 cat_rule 召回 | 本仓库 `mucgec/` | 多参考中文 GEC（已有本地基准） |
| SIGHAN-2015 | F1 | pycorrector 泛化性榜单 | 拼写泛化性评测（长度对齐） |
| EC-LAW | F1 | pycorrector 泛化性榜单 | 泛化性评测（法律领域） |
| MCSC | F1 | pycorrector 泛化性榜单 | 泛化性评测（新闻/口语混合） |
| FCGEC | F0.5 | 2022 NLPCC FCGEC | 语法（学员作文，可作辅助测试） |
| CEC（字级门控用） | 字级 P/R/F1 + 句级门控 F1 | twnlp 仓库 README_ELECTRA | 字级检错模型训练与验证（cec_train/cec_validation 平行文本；val_task_stratified.jsonl 7,500 句：spelling/grammar/mixed 各 2,500） |

## 三、源项目仓库内本地数据（小数据，可作素材）

- `data/dat_data/`：混淆字集（`token_set.txt`、`confuse_sub_v.json` 等）——
  **可作本仓库 `lexicons/zh_gec_lexicon.json` 的 confusable 扩充素材**
  （需按 `curate_lexicon_zh.py` 的「常见正确字对即误报」原则人工筛选）
- `data/business_data/train.json` / `valid.json`：小样本 demo 格式参考（v3 prompt）
- `data/paper_data/`：空占位文件（0 字节）

## 四、使用建议

1. **训练**（按优先级，对应 LangFixer 的 LLM 路径）：
   - SFT（非 COT）：`ChinseseErrorCorrectData`（2M）+ `csc_data` + `cgc_data` + `lang8_hsk`（1.57M）
   - COT SFT：`ChineseErrorCorrector4-4B` 数据集（340K，含错误类型 + 修改原因 + 纠正句）
   - 测试：CSCD（拼写 F1）+ NaCGEC（F0.5）+ MuCGEC（本地 cat_rule 召回）+ SIGHAN-2015/EC-LAW/MCSC（泛化 F1）
2. **数据去污染**：MuCGEC 的 `filter_sentences.txt`（7,137 句）要求从训练集剔除
   （其 test 来自 NLPCC18/CGED/Lang8 采样）；CSCD/NaCGEC 的 test 段同样不可混入训练。
3. **许可证**：上述 5 个 HF 数据集全部为 MIT 或 Apache-2.0，可商用；使用时保留版权声明。
4. **格式差异**：csc_data/cgc_data 为平行文本（无字级标签）；COT 数据 output 含 think 块，
   推理侧需自动剥离（参考源项目 `openai_infer.py` 的 think 块正则剥离
   + `错误类型：`/`修改原因：` 字段抽取）。
5. **增量预训练（CPT）数据**：源项目论文用 5.9M 均衡样本做 CPT，该数据未单独开源
   （2M 数据集为 SFT 数据）；若需复现 CPT，可用 lang8_hsk + 2M 语料替代。

## 五、源项目值得借鉴的设计要点

1. **三阶段训练**（CSRP，ACL 2026）：CPT（5.9M）→ COT SFT → GRPO +
   **Efficiency-Aware Reward**（显式惩罚不必要修改，解决 MLE 训练的过纠正偏置；
   RL 阶段相对 SFT 基线带来 8% 相对提升）。
2. **两级推理**：ELECTRA 字级门控（字级错误概率 + 句级 `max_p_err` 阈值）先筛句，
   仅疑似有错句调 4B 大模型（A100 门控 100+ 句/秒），显著省算力。
3. **14 种语法错误增强器**（`pip install ChineseErrorCorrector`）：
   缺字漏字/错别字/缺标点/错用标点/主语不明/谓语残缺/宾语残缺/其他成分残缺/
   虚词多余/其他成分多余/主语多余/语序不当/动宾搭配不当/其他搭配不当
   ——领域语料不足时的数据增强方案（2024 CCL 冠军核心组件）。
4. **Common Errant**：支持 80 语言（stanza）的 M2 评测工具，F0.5/F1。
5. **工程**：vLLM serve + OpenAI 兼容接口 + 异步批量（CONCURRENCY=16）；
   v3/v4 prompt 自动适配；统一输出 `source/target/errors`（errors 为 (原字, 新字, 位置) 三元组）。
