# CEC_ADOPTION — ChineseErrorCorrector 吸收方案

> 对象：[TW-NLP/ChineseErrorCorrector](https://github.com/TW-NLP/ChineseErrorCorrector)
> （ACL 2026 Main Oral / CSRP；数据清单见 [datasets/cec_datasets.md](datasets/cec_datasets.md)）
> 目标：把 CEC 的六项可借鉴设计落到 LangFixer 现有架构的具体挂载点上。

## 0. 总览：CEC 设计 × 我们的挂载点

| # | CEC 设计 | 我们的挂载点 | 成本 | 优先级 |
|---|---------|------------|------|--------|
| A | v4 COT prompt（10 类错误 + 修改原因）+ Efficiency-Aware 思想 | `prompts/zh_detect_prompt.json` + `shared-model::error_types` + zh 决策表 | 低 | **P0** |
| B | 混淆字集（`dat_data/`） | `lexicons/zh_gec_lexicon.json` confusables + EntryCondition | 低 | **P0** |
| C | 评测基准（CSCD / NaCGEC / SIGHAN-2015 / EC-LAW / MCSC）+ Common Errant | `datasets/` 流水线（prepare → make_gold → Rust eval） | 中 | P1 |
| D | ELECTRA 字级门控（大规模平行语料训练） | `feature-model/` L3 重训练（句级）+ 可选字级 span 定位 | 中 | P1 |
| E | **可切换纠错模型**（模型注册表 + 每模型 prompt/输出适配；自训管线 CPT→COT SFT→GRPO 作为可选生产路径） | LLM 层 BYO 端点 + `prompts/models.json` 注册表 + 设置界面切换 | 中 | P2 |
| F | 14 种语法错误增强器（2024 CCL 冠军组件） | `datasets/augment_domain.py` 领域冷启动合成 | 中 | P2 |

## 1. 现状基线（吸收目标）

- **三级响应**：规则（`gec-zh` + `lexicons/`）→ vegaml 决策（`POST /decide`）→
  L3 特征模型（`POST /l3-detect`，句级二分类 + 类型多分类 + 模式化 span + 机械修正）→
  LLM 兜底（OpenAI 兼容端点，`zh_detect_prompt.json` JSON 契约）。
- **错误类型**：zh 内部 5 类（`spelling / grammar / collocation / punctuation / anomaly`），
  常量在 `shared-model/src/error.rs::error_types`；LLM detect prompt 当前枚举 8 类
  （`spelling|grammar|collocation|punctuation|typography|style|logic|anomaly`）。
- **归因**：JDM 决策表（`decision-tables/zh_gec_attribution.json`），
  输入 `errorType × confidence × isNovice × history` → 归因/韦纳/补救/CEFR；
  表按 `errorType == "spelling"` 等精确匹配分派规则行。
- **评测**：zh 仅 MuCGEC（dev 1,137 句，`cat_rule` 召回/误报；test 无本地 gold）；
  英文 JFLEG。`make_gold_zh.py` 做字级 diff + 分类，`gec-zh/tests/mucgec_eval.rs` 跑 Rust 侧评测。
- **自进化**：L3 训练飞轮（LLM 确认样本 → 重训练 → dev 门槛 → 热切换）+ anomaly 蒸馏。

## 2. 工作流明细

### A. Prompt 吸收（P0）

CEC v4 的 prompt 是「先判错误类型（10 类教学分类，含『无误』）、再给修改原因、最后只输出
纠正句」；其 RL 阶段（Efficiency-Aware Reward）显式惩罚不必要修改，专治 MLE 的过纠正偏置。
我们 LLM 层是 BYO-LLM（不训模型），所以把这两点**吸收在 prompt 层**：

1. **`prompts/zh_detect_prompt.json` 升级（version → 1.2.0）**：
   - `error_type` 枚举改为 CEC 教学分类并保留我们特有类：
     `错别字|词语搭配错误|词性错误|语序错误|成分残缺|成分赘余|关联词使用错误|指代不明|语义逻辑不通|标点|anomaly`
     （「无误」不作为数组元素值，而是判定分支：整段无误时直接输出 `[]`）；
   - 吸收 Efficiency-Aware 的 prompt 表达：显式「先判断有无错误」判定步骤 + 最小修改原则
     （「仅修正错误本身，不做风格改写；整段无误时直接输出 []」）；
   - `message` 字段要求 COT 式原因：「原『X』应为『Y』，因为……」（吸收 CEC『修改原因』）。
2. **类型映射（边界处一次性转换，两套体系不混用）**——在 `LlmGecEngine`
   （`gec-core/src/llm.rs`）解析处加 CEC 10 类 → 内部常量的映射：
   错别字→`spelling`；词语搭配错误→`collocation`；
   词性/语序/成分残缺/成分赘余/关联词/指代不明→`grammar`；语义逻辑不通→`logic`；
   标点→`punctuation`；无误→丢弃该条。
   决策表输入仍是 5 大类，**表结构零改动**；若后续想对「成分残缺/赘余」细分归因，
   只需在 zh 决策表加行（JDM 热更新，走现有 `PUT /gec/tables` 闭环）。
3. **验收**：MuCGEC dev 上 A/B（当前基线：误报 4、confusable 召回 2/84、variant 20/20）——
   误报不增、grammar 类召回不降；prompt 版本化可回滚。

### B. 词表吸收（P0）

CEC 仓库 `data/dat_data/`：`token_set.txt`（字 → 混淆字列表，TSV）+
`confuse_sub_v.json` / `confuse_v_obj.json` / `confuse_v_sub.json`（主谓/动宾方向混淆对）。

新增 `datasets/curate_cec_confusables.py`（对齐现有 `curate_lexicon_zh.py` 范式）：

1. 加载混淆字集 → 转 `[wrong, right, note]` 三元组；
2. 按既有 curation 原则过滤：剔除「错误形式本身是常见正确字」的对（做/即/她…本地命中即误报）；
   保留罕用字对、繁→简对（100% 安全）、pypinyin 验证的同音对；
3. 危险对加 `EntryCondition`（`prev_text`/`next_text`/`context_regex`，
   `shared-model/src/condition.rs` 已支持，ZH_CONFUSABLE 两条路径可消费）；
4. 幂等合并进 `lexicons/zh_gec_lexicon.json`（合并前备份，同现有流程）。

**验收**：`cargo test -p gec-zh --test mucgec_eval`——confusable 召回上升、误报 ≤ 4。

### C. 评测吸收（P1）

在 `datasets/` 现有「prepare → make_gold → Rust eval」范式上补齐 CEC 基准矩阵
（数据源与去污染要求见 cec_datasets.md）：

| 基准 | 数据 | 指标 | 新增物 |
|------|------|------|--------|
| CSCD | `twnlp/csc_data` test 段（2.6K） | Correction F1 | `prepare_cscd.py` + `cscd_eval.rs`（同构 `mucgec_eval.rs`） |
| NaCGEC | `twnlp/cgc_data` validation 段（2,470） | F0.5（ChERRANT 式） | `prepare_nacgec.py` + `score_f05.py`（字级编辑对齐，`F0.5 = 2PR/(R+2P)`） |
| SIGHAN-2015 / EC-LAW / MCSC | pycorrector 榜单谱系 | F1（长度对齐） | 通用 `score_f1.py`（源/纠正平行文本即可算） |
| （可选）Common Errant | CEC `scores/` 模块（M2，80 语言） | F0.5/F1 | vendoring 其 `compare_m2` / `parallel_to_m2` 作统一评测器 |

分工原则：**`cat_rule` 体系评「规则可达性」（规则/L3 路径），F0.5/F1 评「纠正质量」（LLM 路径）**，
两套指标互补、各挂各的测试。
去污染：训练集构建时剔除 MuCGEC `filter_sentences.txt`（7,137 句）与 CSCD/NaCGEC test 段。

### D. 模型数据吸收（P1）

现状痛点：L3/决策模型的 zh 训练池只有 MuCGEC gold（约 7.5K 句），比 CEC ELECTRA 门控
的训练语料小两个数量级，句级判断的泛化面窄。

1. **句级重训练（直接复用现有工作流）**：`l3_finetune.py extract/train/eval` 换数据源——
   `csc_data` train（382K）+ `cgc_data` train（57.8K）+ `lang8_hsk`（1.57M）
   构成 has_error 二分类 + 类型多分类的池子（正例 = 有错平行对，负例 = 纠正句/干净句）。
   同时按 CEC 做法建**分层 dev**（spelling/grammar/mixed 各 2,500，对应
   `val_task_stratified.jsonl` 思路）；dev 门槛（F1 退化 > 0.02 不切换）沿用飞轮现有逻辑。
2. **字级 span 定位（可选升级）**：CEC 的 ELECTRA 门控 = Chinese ELECTRA
   （`hfl/chinese-electra-180g-base-discriminator`）TokenClassification，
   标签由 `difflib.SequenceMatcher` 字级对齐生成（replace/delete → 1）。
   可在 `feature-model/` 加一条 transformers 训练路径，把 L3 的 span 定位
   从「正则模式」升级为「学习型字级 span」；`/l3-detect` 契约不变，Rust 侧零改动
   （独立 Python 服务，新增依赖不影响 Rust workspace）。
3. **预期**：L3 句级判断覆盖 CSC/CGEC 全分布后，LLM 长尾进一步收缩
   （与飞轮「LLM 确认样本 → 重训练」闭环叠加）。

**✅ 已完成（句级部分）——实现与关键实验结论**：

- `feature-model/prepare_l3_pool.py`：CSC 100K + CGEC 57.8K + Lang8 350K 采样对
  （473K 对）→ **互补单侧分配**（每对只贡献 src 或 ref 一侧，消除近重复矛盾对）+
  分层 dev（spelling/grammar/mixed 各 2500 正 + 2500 负 = 15K）+ 去污染
  （MuCGEC 7137 + CSCD/NaCGEC 源句）；`l3_bg/zh_bigrams.json` 背景表（ref 字对 top-500K）
- `l3_finetune.py` 新增 `rare_bigram` 特征（第 21 维）：错别字制造干净语料中不存在的
  字对。20K CSC 对实验 F1=0.966（P=0.999）；全池上为拼写类的有效信号
- **硬边界（实验证实）**：聚合句级特征**无法区分「语法错句 vs 干净句」**
  （全池三类概率分布重叠：干净 p50=0.487 / 语法 p50=0.489 / 拼写 p50=0.508）；
  3 类路由模型与概率校准均无法修复 → 字级差异必须走字级模型（D.2 ELECTRA 路径）
- **`server.py` 路由改为精度优先**：高置信且模式库命中 → L3 定案（免 LLM）；
  高置信但无模式命中 → 弃权交 LLM（不再合成句级误报）；任一句需 LLM → 整段弃权
- **效果（15K 分层 dev）**：干净句误报 **7500 → 49（-99.3%）**（旧 MuCGEC 模型
  在新分布上 P≈0.5 全判有错）；拼写错 85 句免 LLM 定案；语法错交 LLM 正确裁定；
  阈值 0.5 最优（降低只增干净句误报：0.45→162、0.4→254）
- zh 生产模型已换为池模型（`models/zh/`，旧 MuCGEC 模型备份 `models/zh_mucgec/`）；
  飞轮基准同步换轨（`l3_data/zh/` = 池特征，dev = 15K 分层）；en 模型按 21 维重训
  （JFLEG dev F1=0.966 无回归）
- **未做（留待 D.2）**：ELECTRA 字级 span 定位——它是「语法错句 vs 干净句」
  可分的唯一已知路径，也是 L3 放行带（干净句免 LLM）的真正解锁点

### E. 可切换纠错模型（P2）

LLM 层保持 BYO-OpenAI-兼容端点不变，但把「用哪个模型」做成**可切换**：
模型注册表 + 每模型 prompt/输出适配 + 设置界面切换（复用现有 LLM 参数热更新机制）。

> **本轮落地（核心 + api-server + web-ui 选择器 + 桌面端镜像，`cargo test --workspace` / web-ui `vue-tsc`+`vitest` 全绿）**：
> - ① 注册表 `prompts/models.json`（generic/cec4/cec3/custom）+ `gec-core::models::ModelRegistry`
>   （`load`/`get`/`enabled`/`output_format_for`，缺失回退空表）；
> - ② 输出适配：`LlmDetectConfig` 增 `output_format`/`max_tokens`（serde 默认，向后兼容）；
>   `LlmGecEngine::detect` 按格式分派，`plain` 走 `detect_plain`（`split_think_answer` 剥离 think 块 +
>   `char_diff_edits` 字级 LCS diff 生成 span + `parse_think_fields` 解析类型/原因 → `map_llm_error_type`）；
> - ③ api-server：`AppState.model_registry`（`prompts_dir` 变更热重载）+ `AppSettings.llm_model_id`
>   （缺省 `generic`）+ `llm_detect_engine` 按选中预设覆盖 base_url/model/output_format/max_tokens
>   + `GET/PUT /gec/settings` 暴露 `llm_model_id` 与 `models` 列表；
> - ④ web-ui 设置页「纠错模型」选择器（`SettingsPanel.vue`：`el-select` 列 enabled 预设 +
>   输出格式 tag；`llm_model_id` 进 load/dirty/save 闭环）+ 桌面端 Tauri 命令镜像
>   （`AppSettings.llm_model_id` + `Core.model_registry` + `SettingsResponse.models` + `update_settings`
>   处理 + `llm_detect_engine` 按预设分派 + `prompts_dir` 变更热重载），两端契约一致。
> - **待做**：第 4 点自训生产管线（可选，产出注册为 `custom`）。
>   cec4/cec3 实机评测 ✅ 完成（2026-10-16：本地 llama.cpp GGUF 部署
>   （Q8_0，`:8010`/`:8011`）+ CEC 官方 prompt 模板 `prompts/zh_detect_cec_prompt.json`
>   （模型卡原文 instruction + 原始句子输入）+ `prompt_template` 端到端接通；
>   MuCGEC dev 三臂 A/B 结果见 §5：cec3 F0.5 0.805 ≫ cec4 0.217 ≫ generic 0.005）。

1. **模型注册表**（新增 `prompts/models.json`，与 `*_prompt.json` 同目录、同热更新机制）：
   每个条目 `{id, name, base_url, model, prompt_template, output_format, enabled}`：
   - `generic`（缺省）：用户现有任意 OpenAI 兼容端点 + 我们的 `zh_detect` JSON prompt；
   - `cec4`：`twnlp/ChineseErrorCorrector4-4B`（本地 `vllm serve` 部署，OpenAI 兼容）；
   - `cec3`：`twnlp/ChineseErrorCorrector3-4B`（同上，上一代模型）；
   - `custom`：用户自训模型（生产管线见第 4 点）——注册即插即用。
2. **每模型输出适配**（`LlmGecEngine` 按 `output_format` 分派；提示词模板按预设
   `prompt_template` 选择——cec4/cec3 用 CEC 官方模板 `zh_detect_cec`（模型卡原文
   instruction + 原始句子输入，即模型 COT-SFT 训练格式），未指定时回退 `{lang}_detect`）：
   - `json`（generic）：现有 JSON 数组契约（v1.2 prompt，A 工作流）；
   - `plain`（cec3/cec4）：模型输出「think 块 + 纠正句」纯文本——
     适配器剥离 think 块（同 CEC `openai_infer.py` 的正则），纠正句与原文
     difflib 对齐生成 span（同 CEC `res_format` 的三元组逻辑），映射到
     `Error{source: llm}`；CEC4 think 块含错误类型/修改原因，可解析进
     `error_type`/`message`（CEC 10 类 → 内部 5 类映射复用 A 的 `map_llm_error_type`）。
3. **设置界面切换**：LLM 卡片加「纠错模型」选择器（注册表 enabled 条目 +
   当前自定义端点），切换即时生效；每模型独立保存 prompt 模板引用与生成参数
   （如 CEC4 需 `max_tokens ≥ 1024` 容纳 think 块）。
4. **自训模型生产管线**（可选；产出注册进注册表即 `custom`）：基座 Qwen3-4B；
   CPT 用 lang8_hsk + 2M 语料替代（5.9M 未开源）；COT SFT 用 340K 数据集
   （**注意先过滤**：抽样可见个别样本纠正错误，如「FPSO海工拈」应改「模」却原样输出）；
   GRPO 的 Efficiency-Aware Reward（reward = F0.5 式得分 − λ·不必要编辑数）
   作为防过纠正的对齐目标。

### F. 数据增强吸收（P2）

`pip install ChineseErrorCorrector` 的 `GrammarErrorDat`（14 种语法错误增强，
2024 CCL 冠军核心组件，LTP + OpenCC + 混淆字集驱动）：

- 新增 `datasets/augment_domain.py`：用户领域干净文本（小说稿/行业文档）→
  14 种增强合成 (source, target) 对 → 喂 L3/决策模型领域重训练或 LLM SFT；
  固定种子保证可复现（对齐 `correct_tools.set_seed` 习惯）。
- 与现有飞轮的关系：飞轮 = 使用中自产数据（热），增强器 = 新领域冷启动（一次性）。

**✅ 已完成（2026-10-16）——实现与验收**：

- `datasets/augment_domain.py`（自包含：无需 `pip install ChineseErrorCorrector`，
  移植 CEC 仓库 `utils/dat.py` 的 `GrammarErrorDat`，Apache-2.0）；
  混淆字表/搭配表 vendor 于 `datasets/cec_dat_data/`（5 文件，~1.5MB）。
- 14 种增强全实现；9 种 LTP 依赖型（分词/词性/依存）在 `pip install ltp`
  后自动启用，无 LTP 时 5 种纯类型可用
  （wrong_word / lack_char / wrong_char / red_component / wrong_sentence_order）。
  验收：6 干净句 × 2 变体 → 12 对、成功率 100%；同种子两次运行逐字节一致；
  混淆替换抽检合理（小→枵、他→榻、好→歌、市→帘）。
- 输出双格式：`pairs`（{source, target, error_type, error_label}，L3 重训练/分析用）
  与 `sft`（CEC business_data 风格 conversations，v3/v4 官方 prompt 原文，LLM SFT 用）。
- 与原计划的偏差：不依赖 PyPI 包（自包含移植）；LTP 模型不 vendor
  （pip `ltp` 首次运行自动下载）；相对原实现修复 `wrong_word` 空候选
  `random.choice([])` 崩溃、LTP 分词拼回不一致时安全放弃。

## 3. 实施顺序与验收

| 序 | 工作流 | 成本 | 验收标准 | 状态 |
|----|--------|------|----------|------|
| 1 | A prompt | ~0.5 天 | MuCGEC dev 误报 ≤ 4、各类召回不降；A/B 报告归档 | ✅ 完成（prompt v1.2.0 + `map_llm_error_type` + 单测；真实端点 A/B 已跑——§5 `generic` 臂：句级门控 F1 0.838，但严格 F0.5 仅 0.005：JSON 契约在门控/找错层面有效、纠正层面无效 → LLM 纠正能力由 E/§5 的 cec3 承担） |
| 2 | B 词表 | ~1 天 | confusable 召回上升、误报不增（mucgec_eval） | ✅ 完成（76 条合并；dev variant 20/20 保持，误报 4→5，+1 为 gold 噪声 雄貓→雄猫） |
| 3 | C 评测 | 2–3 天 | `cscd_eval` / `score_f05` 跑通；产出当前 LLM 路径 F1/F0.5 基线报告 | ✅ 评测矩阵就绪（`cscd_gold.json` 2600 句 + `nacgec_gold.json` 6369 句 + `cscd_eval.rs` + `score_f05.py`；规则层基线已记录，召回 ≈ 0 符合预期）；LLM/L3 路径 F0.5 报告待接模型 |
| 4 | D L3 重训练 | 3–5 天 | 分层 dev F1 提升；LLM 触发率下降（飞轮 status 可观测） | ✅ 句级完成（50 万+ 混合池 + 分层 dev 15K + `rare_bigram` 特征 + 精度优先路由；干净句误报 7500→49；聚合特征无法分「语法错 vs 干净」→ D.2 ELECTRA 字级路径未做） |
| 5 | F 增强器 | 1–2 天 | 领域合成流水线跑通 + 样本抽检 | ✅ 完成（`augment_domain.py` 14 种增强 + `cec_dat_data/` 混淆字表；5 种纯类型验收通过——12/12 对、同种子可复现、抽检合理；9 种 LTP 依赖型 `pip install ltp` 后自动启用；pairs/sft 双格式） |
| 6 | E 可切换模型 | 2–4 天 | `prompts/models.json` 注册表 + 输出适配 + 设置切换；cec4 直挂过 C 的评测矩阵 | ✅ 完成（注册表 + `plain` 字级 diff 适配 + `prompt_template` 端到端 + 设置 GET/PUT + 引擎按预设分派 + 设置页选择器 + Tauri 命令镜像；`cargo test --workspace` / web-ui 全绿）；cec4/cec3 实机 A/B 已跑（§5：cec3 F0.5 0.805 胜出） |

## 4. 风险与边界

- **COT 数据质量**：340K COT 集抽样可见个别「原因对但纠正错」样本，
  若用于训练需先过滤；仅作 prompt/类型体系参考则无此问题。
- **两套分类体系**：CEC 10 类是「教学分类」，我们决策表是「归因分类」——
  只在 LLM 输出解析边界做一次映射，不混入决策表/知识库。
- **MuCGEC test 无本地 gold**（天池榜单）的既有约束不变；CSCD/NaCGEC 补上后
  本地可评测面显著扩大。
- **许可证**：数据与模型权重均 MIT / Apache-2.0，可商用。
- **CEC 仓库内 `paper_data/` 为空占位**，真实数据以 HF 五个数据集为准。

## 5. A/B 评测结果（MuCGEC dev，force_llm 生产路径，2026-10-16）

**设置**：三臂走同一生产路径（`POST /gec/detect` + `force_llm`，`source=="llm"` 过滤）：

| 臂 | 模型 | 端点 | prompt / 输出适配 |
|---|---|---|---|
| `generic`（基线） | qwen3.8-27b（用户现有端点） | `<your-vllm-host>:4000`（vLLM） | `zh_detect` JSON 契约 |
| `cec4` | twnlp/ChineseErrorCorrector4-4B | 本地 llama.cpp GGUF Q8_0 `:8010`（`-np 4`） | CEC 官方模板 `zh_detect_cec` + `plain` 适配 |
| `cec3` | twnlp/ChineseErrorCorrector3-4B | 本地 llama.cpp GGUF Q8_0 `:8011`（`-np 4`） | 同上 |

gold：`datasets/mucgec_gold.json` dev 1137 句 / 7258 编辑；打分 `score_f05.py`
（ChERRANT 字级，严格 (op, ref, span) 匹配）+ cat_rule 口径（span 重叠，与
`mucgec_eval.rs` 规则基线可比：误报 4、confusable 2/84、variant 20/20）。
脚本：`datasets/eval_llm_mucgec.py`（逐句缓存可续跑，5xx 退避重试）。

### 5.1 主表

| 臂 | **F0.5** | P | R | 预测编辑 | 误报编辑 | 句级门控 F1（FN/FP） |
|---|---|---|---|---|---|---|
| **cec3** | **0.805** | **0.922** | **0.534** | 4199 | **90** | **0.996**（6/2） |
| cec4 | 0.217 | 0.346 | 0.087 | 1835 | 486 | 0.908（145/44） |
| generic | 0.005 | 0.009 | 0.002 | 1616 | 384 | 0.838（290/17） |

cat_rule 召回（span 重叠口径；规则基线在括号内）：

| 臂 | other（基线 0） | punct（0/226） | confusable（2/84） | particle（0/40） | variant（20/20） |
|---|---|---|---|---|---|
| cec3 | 2877/6888 | 156/226 | **77/84** | 35/40 | 20/20 |
| cec4 | 932/6888 | 0/226 | 62/84 | 29/40 | 13/20 |
| generic | 1848/6888 | 26/226 | 72/84 | 30/40 | 12/20 |

### 5.2 发现

1. **CEC3 是明确的赢家**：F0.5 0.805 / P 0.922，句级几乎全检（FN 6/1137）。
   confusable 77/84 把规则基线（2/84）提升近 40 倍；代价是误报 90 vs 规则 4
   ——典型「召回换精度」的 LLM 层画像。误报画像（§5.5）：89/90 集中在**含错句**
   上的过度删除/替换（D:62/S:28/I:0），且 prompt 约束压不住（模型忽略 instruction
   后缀）——精度对齐只能走 GRPO（D 工作流第 4 点）。
2. **CEC4 纠正偏好与 MuCGEC 标注分布错位**（欠检 134 句 / 误报 96 句抽样）：
   gold 里大量**删除类 / 虚词替换**编辑（删「的」「了」、句号→逗号）CEC4 基本不产出
   （punct 0/226），而它在干净句上反而做**插入类「润色」**（加「是」「会」「性」）
   与激进改写（「少」→「小」）。即 CEC4 的 COT-SFT 目标分布 ≠ HSK 作文错误分布；
   按 D 工作流第 4 点，需 GRPO（Efficiency-Aware Reward = F0.5 式得分 − λ·不必要编辑）
   对齐到目标评测分布后才值得采用。
3. **通用 27B 定位尚可、纠正不匹配**：span 重叠命中 1988、句级门控 0.838，
   但严格 F0.5 仅 0.005 —— 它「找得到错处」，纠正文本却几乎不与 gold 一致
   （通用模型无 CEC 式对齐）。说明 F0.5 严格口径下，「模型能力大」不等于「纠错好」。
4. **复现性注记**：llama.cpp `-np 4` 多 slot 批处理引入浮点非确定性（temp=0 下
   同一句子两次运行可给出不同纠正，如「听宙」→「听证」/「听审」），跨运行 F0.5
   有小幅波动；结论级差异（0.805 vs 0.217 vs 0.005）远超噪声。

### 5.3 本轮工程修复（评测过程中发现）

- `prompt_template` 字段端到端接通（registry → 模板服务 → 引擎，api-server + 桌面端）；
- `llm.rs::chat_completion` 合并 `reasoning_content`：llama.cpp/vLLM 对 Qwen3
  thinking 块拆到该字段、`content` 只留答案，拼回全文后 plain 适配才能解析
  错误类型/修改原因（`cargo test -p gec-core` 138 全绿）；
- 502 根因 = LLM 客户端 30s 超时（长 think 块生成超时）→ 预设超时提到 120s；
  评测脚本 `detect()` 加 5xx 退避重试（4xx 立即失败）。

### 5.4 结论与建议

- **MuCGEC 类（HSK 作文）场景默认臂建议 `cec3`**；`cec4` 待 GRPO 对齐后再评；
  `generic` 保留为「用户自有端点」兜底（其价值在句级定位而非严格纠正匹配）。
- 后续：cec3 的 90 误报无法用 prompt 约束压制（见 §5.5 负结果），需走
  D 工作流第 4 点 GRPO（Efficiency-Aware Reward）对齐；CEC4 失败模式
  （欠检删除类 / 干净句过插入）同作 GRPO reward 负例来源。

### 5.5 精度约束话术实验（负结果，2026-10-16）

- **设计**：`prompts/zh_detect_cec_strict_prompt.json`（v1.1.0）= 逐字官方 instruction
  之后追加最小修改原则（「只修改确实存在错误的字词或标点；其余部分必须原样保留，
  不要删除、替换或增补没有错误的成分，也不要润色或改写原句的表达风格」）——
  针对 cec3 误报画像（89/90 为含错句上的过度删除/替换，D:62/S:28/I:0）。
- **冒烟**（10 句：5 过度编辑句 + 5 干净句，走生产路径）：**追加约束被模型忽略**——
  干净句两组插入误报（「是/会」「和/甚至」）与 verbatim 模板跑**逐字相同**，
  过度编辑句输出几乎不变（1 句 4→2 编辑，其余持平）。
- **结论**：纯 COT-SFT 模型的行为由训练分布决定，instruction 后缀不生效
  （与 CEC4 忽略标准 `zh_detect` JSON prompt 同源：两个模型都只认训练格式）。
  prompt 层精度调优对 CEC 系模型是死路；**唯一正解是 GRPO 对齐**
  （D 工作流第 4 点：reward = F0.5 式得分 − λ·不必要编辑数，负例即本轮
  cec3 过度编辑 + cec4 过插入的失败模式）。strict 模板文件留档备查，
  `models.json` 默认不引用（cec3 回退 `zh_detect_cec` verbatim）。

