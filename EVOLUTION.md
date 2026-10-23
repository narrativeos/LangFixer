# 进化中心（Evolution Center）

LangFixer 的**用户可见进化闭环**：检测 → 反思 → 训练 → 测试。

四个环节各对应进化中心的一个面（web-ui「进化中心」导航）：

| 环节 | 面 | 做什么 | 阶段 |
|---|---|---|---|
| **测试** | 测评 | 基准评测 + 层级归因 + 消融 + A/B + 漏斗 | **P0（本文档 §3 已实现）** |
| **反思** | 反思 | 反馈聚合 → 反思报告 → 建议队列 | P1 |
| **训练** | 训练 | 领域池构建 → L3 重训练 → 词表条件化 → LLM 训练数据 | P2 |
| **测试** | 贡献与轮次 | 层级贡献测量（消融 Δ）→ 优化轮次 | P3 |

闭环逻辑：**测评**发现「哪层弱、弱在哪」→ **反思**把用户反馈与测评证据聚成
可执行建议 → **训练**消费建议（重训 L3 / 条件化词表 / 构建 LLM 训练数据）→
**测评**再验证（A/B diff 显示提升）→ 进入下一轮。

## 1. 检测漏斗（重构后）

```
① 规则（18 条 zh + 13 条 en）
② 决策模型（vegaml）——默认关闭、代码保留（见 §2）
②.5 知识召回（嵌入 + 知识源）
③-F L3 特征模型（21 维句级特征 + 模式解析；高置信 → 直接解决）
③ LLM（BYO OpenAI 兼容端点 + 模型注册表臂：generic / cec3 / cec4 / custom）
```

**决策模型（vegaml）移出活动级联路径**（`GEC_DEC_ENABLED` 默认 off，代码保留）：
它与 L3 特征模型同为句级二分类门控，而 L3 严格更强（21 维特征 + 模式解析 +
飞轮重训练）。两者同开时决策模型是冗余层。**不是退役**——闭环建成后作为
「项目 #1」用测评中心的消融工具重评（vegaml 迭代快，当前 sklearn 兜底数字
不构成公平判决）。

## 2. 三大决策（2026-10-16 定案）

1. **词表条件化，不设简单白/黑名单**：所有词表变更走 `EntryCondition`
   （条件化条目：上下文模式 / 词性 / 共现约束），**自动化评估优先**——
   反思产出的词表建议先过测评中心消融验证（目标子集 ΔF0.5 / ΔFP）再入库。
2. **LLM 训练以训练数据构建为主**：现阶段产出 SFT/COT 数据（增强器 +
   飞轮确认样本 + 领域池），模型训练依赖外部算力；后续由测评中心自动评估
   后二选一——**自动训练**（本地/自有算力）或**导出数据集外部训练**
   （产出 business_data 风格 conversations，CEC 管线格式）。
3. **优化轮次：自动评估 + 人工确认**：系统自动评估「积累的数据量 + 训练条件
   （算力/端点/冷却期）」，条件满足时生成轮次提案（哪些层、用什么数据、
   预期 Δ），**人工确认后执行**；执行结果回流测评中心 A/B 验证。

## 3. P0 测评中心（已实现）

| 组件 | 位置 | 说明 |
|---|---|---|
| 评测服务 | `datasets/eval_server.py`（:4807，`EVAL_PORT` 可改） | FastAPI；`POST /eval/run`（benchmark/split/arm/mode/ablation/subset/seed/workers/fresh）→ 后台任务；`GET /eval/job/:id` 进度；`GET /eval/reports`、`/eval/report/:id` 报告 |
| api-server 代理 | `api-server/src/routes/eval.rs`（`/gec/eval/*`） | 透传 + 503 降级（服务未运行不阻断主链路） |
| 桌面端镜像 | `desktop-app/src/commands.rs`（`run_eval` / `eval_job` / `list_eval_reports` / `get_eval_report`） | 同 api-server 代理语义 |
| web-ui | `web-ui/src/components/EvolutionCenter.vue`（「进化中心」导航） | 发起评测（基准/臂/模式/消融/句数）→ 进度 → 总体 + 层级归因表 + 漏斗表 → 历史报告 + A/B diff |

**计分口径**（与 `score_f05.py` / `mucgec_eval.rs` 一致）：

- **严格**：预测编辑 (op, ref, span) 与 gold 编辑全同 = TP；Fβ（mucgec/nacgec
  β=0.5，cscd β=1）；
- **重叠**：span 相交（零宽插入 = 同插入点）= 命中；
- **层级归因**：每个 gold 编辑按级联序归因到**首个重叠层**（= 漏斗中该层
  「最先解决」它）→ 各层召回份额；各层独立 P/R/Fβ + 误报 + 句级门控；
- **漏斗**（funnel 模式 = 自然级联）：LLM 触达率 + 级联前缀累计解决率；
- **消融**：`PUT /gec/settings` 临时关层（decision/l3）→ 重跑 → 恢复；
  ΔF0.5/ΔFP = 该层边际贡献（含级联效应）；
- **换臂**：`llm_model_id` 切注册表条目（cec3/cec4/generic/custom）跑同一基准。
- **基准**（`BENCHMARKS`）：`mucgec` / `cscd`（cscd·sig15·med·law）/ `nacgec` / `jfleg`
  \+ CTCDataset 原生（母语者）集 `cscdns` / `nlpcc23` / `sig13` / `sig14` / `cctc` /
  `midu` / `ctc21`（`datasets/prepare_ctc.py` 转换；逐子集重叠审计 `datasets/ctc_audit.py`，
  全部源句已入去污染块表）。

**缓存**：逐句 detect 结果按 (benchmark, split, arm, ablation, mode) 缓存于
`datasets/eval_reports/cache/`（中断可续跑；`fresh=true` 清空）。
**互斥**：全局设置是共享状态 → 同一时间只允许一个 run 任务（409）。

**验收**（MuCGEC dev 1137 句全量，三臂 A/B）：见 §8 验收记录。

## 4. P1 反思中心（已实现）

| 组件 | 位置 | 说明 |
|---|---|---|
| 反思核心 | `gec-core/src/reflection_center.rs` | 确定性核心：`build_report`（反馈聚合 + 测评证据 → findings + 建议队列）；`parse_eval_report` / `fetch_eval_evidence`（测评证据提取）；4 类建议 `lexicon_condition` / `rule_tune` / `l3_sample` / `llm_arm` |
| 存储 | `lexicon-store/src/reflection_center.rs` | SQLite `reflection_report` + `reflection_suggestion` 表（建议状态机：`pending` → `applied` / `consumed` / `dismissed`） |
| api-server | `api-server/src/routes/reflection_center.rs` | `POST /gec/reflect/run`（生成）/ `GET /gec/reflect/reports` / `GET /gec/reflect/report/:id` / `GET /gec/reflect/suggestions` / `PUT /gec/reflect/suggestion/:id`（P2 消费入口） |
| 桌面端镜像 | `desktop-app/src/commands.rs` | `run_reflect` / `list_reflect_reports` / `get_reflect_report` / `list_reflect_suggestions` / `update_reflect_suggestion` |
| web-ui | `web-ui/src/components/EvolutionCenter.vue`（反思 tab） | 生成报告（语言 / 是否含测评证据）→ 发现 + 反馈统计 + 测评证据 + 建议 → 建议队列（过滤 / 标记已消费 / 忽略）→ 报告历史 |

**建议生成规则**（全部确定性，无 LLM 依赖）：

| 类型 | 触发证据 |
|---|---|
| `rule_tune` | 某规则独立拒绝 ≥ 3，或拒绝占比 ≥ 50%（n≥2）→ 停用 / 收紧 |
| `lexicon_condition` | 规则存在被拒绝文本（或测评证据规则层 FP ≫ TP）→ 条件化排除条目，先过测评中心消融再入库 |
| `l3_sample` | 模型层（feature/decision/statistical）拒绝 ≥ 3 → 重训练；1-2 → 继续积累负样本；测评证据 L3 门控 F1 < 0.9 → 重训练 |
| `llm_arm` | LLM 层拒绝 ≥ 3；或测评证据当前臂 Fβ < 0.3 且 FP ≫ TP → 换臂 / 构建 LLM 训练数据 |

**证据来源**：`feedback_log`（按来源层 / 规则 id 的采纳 / 放弃聚合 + 最近明细）+
测评中心最近报告（层级归因 / 漏斗；`GEC_EVAL_ENDPOINT`；服务不可达时优雅降级为
纯反馈数据，不阻断）。同一 (kind, target) 建议去重（保留高优先级、证据合并）。

## 5. P2 训练中心（已实现）

反思中心（P1）产出建议队列后，训练中心负责「消费建议 → 执行训练动作 → 作业记录」：

| 组件 | 位置 | 说明 |
|---|---|---|
| 训练核心 | `gec-core/src/training_center.rs` | 确定性核心：`build_domain_pool`（领域池：模型层反馈 + 飞轮样本 + 增强器，互补单侧分配 + 句级去重）；`build_llm_training_data`（LLM 层 adopt 反馈 → SFT JSONL）；`plan_from_suggestion`（反思建议 → 训练作业）；作业状态机 `proposed → confirmed → running → completed / failed` |
| 存储 | `lexicon-store/src/training_center.rs` | SQLite `training_job` 表（作业 + 状态机迁移校验 + 执行结果 JSON）；`ReflectionCenterStore::get_suggestion`（P2 消费入口按 id 取建议） |
| api-server | `api-server/src/routes/training_center.rs` | `POST /gec/train/pool`（领域池）/ `POST /gec/train/llm-data`（LLM 数据）/ `POST /gec/train/l3/retrain`（L3 重训练，与 `/gec/l3/retrain` 共用 `do_retrain`）/ `POST /gec/train/lexicon/apply`（词表条件化，可选测评消融前置）/ **`POST /gec/train/suggestion/:id/run`（P1 → P2 消费入口）** / `GET /gec/train/jobs` / `GET /gec/train/job/:id` |
| 桌面端镜像 | `desktop-app/src/commands.rs` | `train_build_pool` / `train_build_llm_data` / `train_run_l3_retrain` / `train_apply_lexicon` / `train_run_suggestion` / `train_list_jobs` / `train_get_job`（`l3_retrain` 命令体抽成 `l3_retrain_forward` 与训练中心共用） |
| web-ui | `web-ui/src/components/EvolutionCenter.vue`（训练 tab） | 训练作业表（状态过滤）+ 领域池构建（增强器输入）+ L3 重训练 + 词表条件化 + LLM 训练数据卡片；反思 tab 建议队列新增「执行」按钮（`rule_tune` 除外——人工操作） |

**建议 → 作业分派**（`plan_from_suggestion`，P1 → P2 桥）：

| 反思建议 kind | 训练作业 kind | 动作 | 建议终态 |
|---|---|---|---|
| `l3_sample` | `l3_retrain` | 转发 feature-model 飞轮（合并确认样本 → 重训练 → dev 评测 → 热切换） | 成功 → `consumed`；失败（服务不可达）→ 作业 `failed`，建议保持 `pending`（可重试） |
| `lexicon_condition` | `lexicon_condition` | payload 的 `rule_id` + `originals` → 条件化排除条目入库（`append_rule_exclude`；可选先过测评消融，不可达时降级直接入库） | 成功 → `consumed` |
| `llm_arm` | `llm_data` | LLM 层 adopt 反馈（带修正）→ SFT JSONL（作业 result 携带，可导出外部训练） | 成功 → `consumed` |
| `rule_tune` | —（`None`） | 规则调参是人工操作，不产生训练作业（接口返回 400） | 保持 `pending` |

**领域池语义**（`build_domain_pool`）：模型层反馈 `adopt` → `error` 样本（带修正）；
`reject` → `clean` 样本（用户判定模型误报 → 句子实际干净）；飞轮确认样本
（`l3_data/<lang>/confirmed.jsonl`，反馈路由自动汇入）原样并入；增强器干净句（用户自有
语料——基准集句子禁止入池，防污染）→ `clean` 样本。**互补单侧分配**：每句只贡献一个
标签（句级去重，保留首个）——同对平行句双侧入池会产生「近乎重复而标签相反」的矛盾对，
聚合特征无法区分 → 模型退化为先验（`feature-model/prepare_l3_pool.py` 已实验验证）。

**闭环**：测评发现弱层（P0）→ 反思聚成建议（P1）→ 训练消费（P2）→ 测评 A/B 验证
（P0 工具）。验收见 §8「P2 验收」。

## 6. P3 贡献与轮次（已实现）

闭环的「决策」环节：把 P0–P2 的能力接成**周期性优化**——
**层级贡献测量**（消融 Δ 常态化：量化每层为最终 Fβ 净贡献多少）+
**优化轮次**（自动评估积累的数据与训练条件 → 提案 → **人工确认门** →
执行（复用 P2 训练作业）→ 测评 A/B 验证）。

| 组件 | 位置 | 说明 |
|---|---|---|
| 贡献核心 | `gec-core/src/contribution.rs` | `AblationSpec`（消融规格，从报告 `ablation` 字段解析）；`parse_report_summary`（测评报告 → 摘要）；`measure_contributions`（多份报告 → 贡献表：每层边际贡献 = `Fβ(全开) − Fβ(该层关闭)`，同规格多份取最新，缺臂标记 `missing` 不阻断） |
| 轮次核心 | `gec-core/src/optimization_round.rs` | `plan_round`（确定性阈值判定 → 动作集合；全不满足 → `None`）；`no_action_reasons`（「无需开轮」时各指标离阈值差距）；`round_report`（轮次报告：计划动作 × 执行结果按 kind 匹配 → 各动作附完整派生作业 + 人类可读摘要 + 执行后测评证据）；轮次状态机 `round_status`（`proposed → confirmed → running → completed / failed`；`proposed → rejected` 人工门） |
| 存储 | `lexicon-store/src/optimization_round.rs` | SQLite `optimization_round` 表（轮次 + 状态机迁移校验 + 动作 / 证据 / 结果 JSON）；`feedback_log` 新增 `stats_since` / `list_since`（时间窗口——轮次增量只统计上次轮次之后的反馈） |
| api-server | `api-server/src/routes/optimization.rs` | `POST /gec/eval/contributions`（贡献表）/ `POST /gec/round/evaluate`（自动评估 → 提案 201 或「无需行动」200）/ `GET /gec/rounds` / `GET /gec/round/:id` / `POST /gec/round/:id/confirm` / `reject` / `execute` / `GET /gec/round/:id/report`（轮次报告）；`training_center.rs` 分派逻辑抽成 `execute_training_action`（P2 建议消费与 P3 轮次执行共用） |
| 桌面端镜像 | `desktop-app/src/commands.rs` | `eval_contributions` / `round_evaluate` / `round_list` / `round_get` / `round_confirm` / `round_reject` / `round_execute` / `round_report` |
| web-ui | `EvolutionCenter.vue`（贡献与轮次 tab） | 层级贡献表（基准 / split / 臂 / 模式过滤 + 边际 Δ 表 + 缺失臂提示）；优化轮次（自动评估 → 提案 / 无需行动原因 → 轮次表：确认 / 拒绝 / 执行 + 各动作结果 + **报告** dialog：摘要 / 各动作作业细节 / 执行后测评证据） |

**轮次动作阈值**（`plan_round`，确定性；增量窗口 = 上次轮次之后）：

| 动作 kind | 触发条件 | 执行 |
|---|---|---|
| `l3_retrain` | 模型层（feature/decision/statistical）拒绝 ≥ 5；或飞轮确认池 ≥ 20；或测评证据 L3 门控 F1 < 0.9 | 派生 P2 训练作业（feature-model 飞轮；服务不可达 → 该动作 failed） |
| `llm_data` | LLM 层采纳（带修正）≥ 10 | 派生 P2 训练作业（SFT JSONL） |
| `lexicon_condition` | 某规则拒绝 ≥ 3（P1 同阈值）且有被拒文本 | 派生 P2 训练作业（条件化排除条目入库） |
| `rule_tune` | 某规则拒绝 ≥ 5，或拒绝占比 ≥ 50%（n≥2） | **人工**：执行时标记 `human_required`（规则面板操作，不阻断轮次完成） |

**语义要点**：
- **人工确认门**是 P2 → P3 的关键差异：P2 作业自动确认（`proposed → confirmed`
  由系统完成），轮次必须人工 `confirm`（或 `reject`）后才能 `execute`——
  状态机在存储层强制（`confirmed → rejected` 等非法迁移一律拒绝）；
- 执行时每个自动动作**各派生一个独立 P2 训练作业**（走完整作业状态机，
  可在作业列表追溯）；任一自动动作失败 → 轮次 `failed`（各动作结果在
  `result.actions`）；全部成功 → `completed`；
- **增量窗口**：反馈统计 / 明细只统计上次轮次（任意状态）之后的增量
  （`stats_since` / `list_since`；边界时间 RFC3339 → SQLite datetime 归一化），
  避免同一批反馈反复触发相同轮次；飞轮池规模与测评证据为绝对值；
- **贡献测量是纯派生分析**：不新跑测评，只消费已有报告（测评服务不可达 →
  503，与 P0 各端点一致）；同一（benchmark, split, arm, mode）组内各消融规格
  各取最新一份报告；
- **轮次报告**（`GET /gec/round/:id/report`）：任何状态都可查看的聚合视图——
  开轮背景 + 各动作一行摘要（从派生作业 `result` 提取关键数字：dev F0.5 /
  SFT 样本数 / 入库条目数）+ 完整作业 + 执行后该语言最新测评证据（不可达 →
  `null`，优雅降级）；计划 `actions` 与执行 `result.actions` 按 kind 匹配
  （计划里无 `job_id`，`job_id` 只在执行结果里）。

**闭环**：测评（P0）→ 反思建议（P1）→ 训练（P2）→ **贡献测量 + 优化轮次（P3）**
→ 再测评 A/B 验证（P0 工具）。验收见 §8「P3 验收」。

## 7. 阶段计划

| 阶段 | 内容 | 依赖 | 验收 |
|---|---|---|---|
| **P0** | 测评中心（本文档 §3） | — | 复现 A/B（§8）；层级归因表 + 漏斗 + 消融可用 |
| **P0.5** | 漏斗重构落地：`GEC_DEC_ENABLED` 默认 off；level 语义重绑 L3 门控；管线 UI 更新 | P0 | ✅ 已完成：默认路径 = 规则 → L3 → LLM；决策层可手动重开（验收见 §8） |
| **P1** | 反思中心：反馈聚合（feedback_log + 测评证据）→ 反思报告 → 建议队列（词表条件化 / 规则调参 / L3 样本 / LLM 臂） | P0 | ✅ 已完成（本文档 §4）：一条反馈 → 一份带证据与建议的报告；建议队列可被 P2 消费 |
| **P2** | 训练中心：领域池（增强器 + 飞轮样本）→ L3 重训练 → 词表条件化入库 → LLM 训练数据构建（自动训练 / 导出外部训练） | P1 | ✅ 已完成（本文档 §5）：建议队列消费入口 + 4 类训练作业 + 状态机；「训练后」A/B 验证流程见 §8 残留观察 |
| **P3** | 贡献与轮次：层级贡献测量（消融 Δ 常态化）→ 优化轮次（自动评估积累数据与训练条件 → 人工确认 → 执行）+ 轮次报告 | P2 | ✅ 已完成（本文档 §6）：贡献表 + 轮次状态机（人工确认门）+ 8 端点 + 轮次报告；「轮次后」A/B 验证用 P0 工具（§8 P3 验收） |
| **项目 #1** | 决策模型重评：用测评中心消融工具在 P0.5 后重跑 vegaml（新版）vs L3-only | P0.5 | ✅ 已评估（本文档 §11）：消融公平性不成立（评测池污染 + 无 ON 臂），改为直接体检；**建议判决：退役出产品路径**（待确认执行） |

## 8. 验收记录

### P0 验收：MuCGEC dev 三臂 A/B（复现 2026-10-16 上一轮手工 A/B）

| 臂 | F0.5（严格） | 句级门控 F1 | 备注 |
|---|---|---|---|
| cec3 | **0.805**（LLM 层 0.8052） | 0.996 | 报告 `20261016T155142Z_mucgec_cec3_capability`；LLM P=0.923 R=0.534；全量 F0.5 0.7827（含规则层 159 FP） |
| cec4 | **0.216**（LLM 层 0.2158） | 0.919 | 报告 `20261016T162645Z_mucgec_cec4_capability`；LLM P=0.343 R=0.087 FP重叠 756 |
| generic | **0.005**（LLM 层 0.0050） | 0.8095 | 报告 `20261016T184515Z_mucgec_generic_capability`；LLM P=0.0089 R=0.0018 FP重叠 213；LLM 触达率 0.9015（远端 27B 平均 61.5s/句） |

（上一轮数字来自 `eval_llm_mucgec.py` + `score_f05.py` 手工流程；
本轮由测评中心 `eval_server.py` 一键复跑——**三臂全部吻合**：cec3 0.8052≈0.805、
cec4 0.2158≈0.217、generic 0.0048≈0.005，验收通过；逐句结果缓存于
`eval_reports/cache/`。）

### 已知边界

- 测评服务是**开发工具**：打包桌面端不可达时 4 个命令返回友好错误（503 语义）；
- 消融只支持 decision/l3 两层（规则/知识/LLM 层的消融 = 换臂或改注册表，
  后续按需扩展）；
- 句级对齐按 `src` 原文（与 score_f05 一致）；gold 噪声（MuCGEC 个别
  编辑多解）会同时压低各臂 R，A/B 相对比较不受影响。

### P0.5 验收：漏斗重构（2026-10-17）

- **默认翻转**：`DecisionConfig::default_enabled()` / `from_env()` → `false`
  （`GEC_DEC_ENABLED=1`/`true` 或设置文件 `decision.enabled=true` 显式 opt-in）；
  本地 `langfixer-settings.json` 已同步翻转（decision off / l3 on）；
- **level 2 重绑 L3 门控**（`CascadeDetector::detect_level`）：L3 在 level 2/3
  并行运行；`need_llm`（level 2）= 决策弃权 **或** L3 弃权/服务不可用；
  L3 高置信（有错 / 明确无错）→ 直接定案不跑 LLM；L3 未启用时回退旧语义
  （决策模型弃权触发）；level 3 语义不变（L3 门控 + 无条件 LLM）；
- **实测**（运行中服务，decision off / l3 on）：干净句 level 2 →
  L3 弃权（l3_ms=4ms）→ LLM 兜底触发（llm_ms=3491ms）→ LLM 判干净；
  L3 高置信跳过 LLM 的路径由 `cascade_level2_l3_*` 单测覆盖（mock L3 服务）；
- **测试**：gec-core 141 通过（含 3 个新 L2+L3 门控测试 + 翻转后的
  `config_from_env_default_off_opt_in`）；web-ui pipeline 15/15 通过；
- **文档**：根 README（检测段 / `GEC_DEC_ENABLED` 行 / level 表）、
  decision-model / feature-model README、`pipeline.ts` / `gec.ts` 注释同步。

**遗留观察**：当前 L3 模型弃权偏保守（抽查 6 句全部 `abstain: true`，
概率聚集 0.44–0.46）——level 2 实际几乎总是落到 LLM；L3 判别力提升是
P2 重训练的首要目标（反思中心将用测评中心量化其门控价值）。

### P1 验收：反思中心（2026-10-17）

- **一条反馈 → 一份带证据与建议的报告**：10 条反馈（规则 1 采纳 + 3 拒绝、
  feature 3 拒绝、llm 3 拒绝）→ `POST /gec/reflect/run` 产出 1 份报告：
  findings（反馈总量 / 放弃最集中层 / 测评降级说明）+ 3 条建议
  （`rule_tune` target=ZH_HOT、`l3_sample` target=feature、`llm_arm` target=llm，
  均带证据与 payload）；
- **建议队列可被 P2 消费**：建议逐条落库（`pending` 初始态）；
  `PUT /gec/reflect/suggestion/:id` 状态流转可用（consumed / dismissed /
  applied / pending）；非法状态 400、不存在 id 404；
- **优雅降级**：测评服务不可达（`GEC_EVAL_ENDPOINT=http://127.0.0.1:1`）时
  报告生成不阻断，findings 含「测评证据不可用」说明；
- **测试**：gec-core reflection_center 13/13；lexicon-store reflection_center 3/3；
  api-server reflection_api 2/2（端到端：反馈种子 → 报告 → 队列状态流转）；
  web-ui 构建通过（反思 tab：报告生成 / 建议队列 / 报告历史）。

### P2 验收：训练中心（2026-10-17）

- **P1 → P2 消费闭环**：`POST /gec/train/suggestion/:id/run` 把反思建议分派为训练作业
  （`l3_sample` → L3 重训练 / `lexicon_condition` → 词表入库 / `llm_arm` → LLM 数据；
  `rule_tune` 为人工操作，返回 400 不产生作业）；成功 → 建议 `consumed`；
  失败（如 L3 服务不可达）→ 作业 `failed`、建议保持 `pending` 可重试；
- **4 类训练作业**：领域池（反馈 + 飞轮 + 增强器，互补单侧分配去重）/ L3 重训练
  （与 `/gec/l3/retrain` 共用执行体，飞轮 + dev 评测 + 热切换）/ 词表条件化
  （可选测评消融前置，不可达时降级直接入库）/ LLM 训练数据（adopt 反馈 → SFT JSONL）；
  作业状态机 `proposed → confirmed → running → completed / failed`，非法迁移 400；
- **测试**：gec-core training_center 13/13；lexicon-store training_center 31/31；
  api-server 全量 94 项（含 3 个 P2 集成测试：建议分派消费、池 + LLM 数据 + 作业列表、
  rule_tune 拒绝）；web-ui 类型检查 + 39/39 单测 + 构建通过；
- **残留观察**：「训练后」MuCGEC A/B（重训练后跑一轮测评对比目标层 Δ）依赖 feature-model
  服务实跑，作为 P3 优化轮次的验证步骤执行——流程已就绪（测评中心工具 + 训练作业记录）。

### P3 验收：贡献与轮次（2026-10-17）

- **层级贡献测量**（`POST /gec/eval/contributions`）：从测评报告列表按
  （benchmark, split, arm, mode）取最新组，各消融规格（全开 / l3 关 / decision 关 /
  两层皆关）各取最新报告 → 贡献表（每层边际 Δ + 核心基线 + 缺失臂提示）；
  测评服务不可达 → 503（与 P0 各端点一致）；
- **优化轮次生命周期**（集成测试 `round_propose_confirm_execute_lifecycle`）：
  种子反馈（模型层 5 拒 + 规则 3 拒 + LLM 10 采纳）→ `POST /gec/round/evaluate`
  提案 201（4 类动作：l3_retrain / llm_data / lexicon_condition / rule_tune + 证据）→
  未确认时执行 400（人工门）→ confirm → reject 400（确认后不能走拒绝门）→
  execute → 轮次 `failed`（L3 服务不可达 → 该动作 failed + 派生作业 failed；
  llm_data / lexicon_condition 完成 + 派生作业 completed；rule_tune → `human_required`）
  → 词表排除条目确实入库（临时词表副本）→ **增量窗口**：再评估 →「无需行动」
  （已计反馈被窗口排除）→ **轮次报告**（`GET /gec/round/:id/report`）：
  `n_actions=4` / `n_failed=1` + 人类可读摘要（L3 失败原因 / SFT 样本数 /
  入库条目数 / 规则人工项）+ 各动作附完整派生作业（`result` 细节）+
  执行后测评证据（服务不可达 → `null`）+ 不存在轮次 404；
- **拒绝门**（`round_reject_gate`）：proposed → rejected；拒绝后 confirm / execute 均 400；
- **状态机存储层强制**：lexicon-store optimization_round 5/5（含非法迁移拒绝：
  `proposed → running`、`confirmed → rejected`、终态再迁移、不存在 id）；
- **测试**：gec-core 185 通过（contribution 7 + optimization_round 12 新增，含
  轮次报告 3：failed 态摘要 / 提案态 / 拒绝态）；
  lexicon-store 36 通过（optimization_round 5 + feedback_log stats_since 1 新增）；
  api-server 98 项（含 4 个 P3 集成测试）；desktop-app 编译通过（8 命令镜像）；
  web-ui vue-tsc 干净 + 39/39 单测 + 构建通过（贡献与轮次 tab：贡献表 + 轮次表
  确认 / 拒绝 / 执行 / 报告 dialog）；
- **残留观察**：「轮次后」MuCGEC A/B（执行轮次后跑一轮测评，对比目标层 Δ 与
  贡献表预测）依赖 feature-model 服务实跑——与 P2 残留观察同一条验证路径，
  工具已就绪（贡献表给出「训练哪层收益最大」的先验，A/B 给出实测）。
- **Tauri invoke 契约修复（2026-10-17，桌面端实跑发现）**：P2/P3 的 8 个查询 /
  操作命令（`train_list_jobs` / `train_get_job` / `round_list` / `round_get` /
  `round_confirm` / `round_reject` / `round_execute` / `round_report`）Rust 侧曾是
  单个 `body` 参数，而前端按项目惯例平铺传参（`{ id }` / `{ lang, status, limit }`）
  ——Tauri 按参数名绑定，invoke 直接报 `missing required key body`（集成测试只走
  api-server HTTP 路径，Tauri 绑定层是验收盲区）。修复：8 个命令改为独立参数
  （对齐 P0/P1 的「查询平铺 / 写操作 body」惯例，前端零改动）；新增
  `desktop-app/tests/command_contract.rs` 契约测试（解析命令签名参数名，
  断言查询类平铺 / body 类 9 个命令保持 `body`），防再犯。
- **用户体验引导增强（2026-10-17，整体 review 后落地，纯前端）**：
  ① 检测页顶部「系统状态」总览条（`PipelineStatus.vue`）：管线各层
  可用 / 未启用 / 未配置 一眼可见，点项跳设置，「测试全部」一键探测；
  ② 首次启动三步引导（`OnboardingDialog.vue`，localStorage 只弹一次）；
  ③ 结果面板「本次管线」摘要（`summarizeRun` 纯函数 + 单测：从 timings
  推断各层实际执行情况，与 Editor 检测前提示互补）；
  ④ 反思「执行建议」成功后自动切「训练」tab（store `navTab` 导航）+
  消息附作业结果摘要；⑤ 常见错误 → 修复指引（`lib/guide.ts` + 7 单测：
  决策模型 / 特征模型 / LLM / HanLP / 嵌入 / 数据目录，具体短语优先于
  宽泛词），Editor 错误 alert 附「如何修复 + 去设置」；
  ⑥ 进化中心闭环导览（el-steps：检测积累 → 反思 → 训练 → 测评，按真实
  状态动态高亮建议下一步）；⑦ 小修：会话 ID placeholder、建议队列 /
  检测历史空态引导、LlmExplain 未配置 LLM 提示、轮次「确认」流程预告
  tooltip。验证：vue-tsc 干净 + 55/55 单测 + 构建通过。
- **用户体验引导增强 II（2026-10-17，Tier 2 剩余项，纯前端）**：
  ① 测评表单收「高级」：默认只显示 基准 / 子集 / 句数 + 「开始评测」，
  臂 / 模式 / 消融 / fresh 收进折叠「高级选项」；新增「一键默认评测」
  （MuCGEC dev · 当前臂 · 能力 · 200 句，首次使用推荐）；
  ② 设置页「服务总览」卡：7 行服务全景（进程内 / 外部 / 本地目录 ×
  已配置 / 未启用 / 未配置 × 最近测试结论）+ 页首「测试全部」快捷入口；
  ③ 「响应级别」术语修正（P0.5 重绑后旧文案误导）：L2 标签 / 状态标签 /
  警告改为按 l3Enabled 动态（L3 启用时门控是特征模型而非决策模型），
  L3 标签从「强制 LLM」改为「特征模型定案，LLM 收长尾」。
  验证：vue-tsc 干净 + 55/55 单测 + 构建通过。
- **测评默认模式改为漏斗（2026-10-17）**：评测表单初始值 / 「一键默认评测」
  默认参数 / 贡献测量 mode placeholder 均由 capability 改为 funnel——
  漏斗（自然级联）贴近真实使用路径，作为常态化基线；能力模式（强制 LLM）
  保留在高级选项，用于测层级能力上限。
- **贡献测量错误文案去内部名 + 报告组下拉（2026-10-17）**：
  ① 「无匹配测评报告（请先跑测评：run_eval / POST /gec/eval/run）」
  两处（desktop-app / api-server）统一改为用户可操作指引；
  ② 贡献表单 4 个自由输入框改为「报告组」下拉：候选从已有测评报告
  按 (benchmark, split, arm, mode) 去重派生，默认「自动（取最新组）」。
- **修复：Tauri 贡献测量显式 null 过滤恒失配（2026-10-17）**：
  前端选「split 为 null 的报告组」时显式传 `split: null`，
  旧 Rust 匹配器把 `Some(Null)` 当「必须匹配字符串」→ 恒 false →
  报告明明存在却误报「无匹配测评报告」（Web 路径经 `Option<String>`
  反序列化不受影响）。匹配逻辑抽为 `report_field_matches`
  （null/缺失 = 不过滤）+ 2 项回归单测。
- **修复：消融报告详情 404（id 含 `+` 被消毒剥掉）（2026-10-17）**：
  消融报告 id 含 `+`（`abl_dec-off+l3-off`，由 `abl_fp` 生成），
  但 eval_server 的 `/eval/report/{id}` 消毒正则 `[^A-Za-z0-9_.-]`
  把 `+` 剥掉 → 文件名失配 → 404（列表读文件内容不受影响，
  故「列表可见、详情 404」）。允许集补 `+`（无路径穿越风险）；
  13 份真实报告 id 全量回归 0 失配。**只需重启 eval_server 进程**，
  无需重建桌面端。
- **消融指引修正：逐层 Δ 需「单层关闭」报告（2026-10-17）**：
  `measure_contributions` 逐层匹配要求 decision-only-off / l3-only-off
  各一份；同时关两层只产出核心基线（f_beta_core）。
  「去跑消融」预填改为一次只勾一层 + 两层都缺时 warning 提示分两次跑；
  说明卡同步「一次只关一层」。
- **贡献测量收敛为「一键测量层级贡献」（2026-10-17）**：
  普通用户不应被要求「跳页 → 勾框 → 跑两次 → 回来再点」。
  贡献 tab 主按钮改为「一键测量层级贡献」：自动检查所选报告组缺哪些
  报告（全开 / 决策层关 / L3 层关）→ 后台顺序代跑缺失项（3s 轮询进度，
  逐条显示 运行中 x/y 句 / 已有跳过 / 失败原因）→ 完成自动刷新并出表；
  无任何报告时用当前「测评」表单参数连基线一起代跑；「重新测量」保留
  手动重算。手动消融入口（测评页高级选项）保留给高级用户。

### 自学习轮次验收：首个完整闭环实跑（2026-10-17）

用现有数据集（MuCGEC dev，200 句 / seed 42 / current 臂 / capability）
完整跑了一轮「测评 → 反思 → 训练 → 再测评」：

- **基线**（报告 `20261017T071437Z_mucgec_current_capability`）：
  总体 Fβ=0.7894（P=0.9126 / R=0.5125，744 预测 / 679 命中 / 192 金标）；
  层级归因：**LLM 层扛全部 679 TP**（Fβ=0.8074，门控 F1=0.9948）；
  **规则层 24 预测 / 0 命中 / 5 FP**（纯噪声，门控 F1=0.1784）；
  L3 门控 0/192（弃权偏保守，F1=0.000）；
- **反思**（报告 #3，6 条新建议）：`rule_tune` ×2（ZH_MISSING_SUBJECT
  4 拒 / ZH_PUNCTUATION 2 拒）、`lexicon_condition` ×3（两条带具体被拒文本
  「靠」「——」「）、」，一条无具体文本）、`l3_sample` ×1；
- **裁决（人工确认门）**：采纳 2 条（带具体被拒文本的条件化排除）；
  驳回 14 条——`l3_sample` ×3（轮次 #2 已实测重训零效果：
  F1 0.5457 → 0.5457，509 条确认样本已合并，问题在模型容量不在样本量）；
  `rule_tune` ×5（人工操作类，收紧已由排除条目覆盖，复测后再裁决）；
  无具体文本 ×3、重复项 ×3；
- **训练**：2 条建议消费（作业 #4 / #5）——**幂等跳过**（排除条目
  06:57 已写入 `lexicons/zh_gec_lexicon.json` 的 `rules[].excludes`）；
  活体验证：含「靠」句「靠窗坐着，很舒服。」→ 0 错误（排除生效）、
  对照句「学习。」正常触发 ZH_MISSING_SUBJECT；
- **验证复测**（`20261017T072410Z`，fresh 同参数）：与基线**完全一致**
  （Fβ 0.7894 / 规则层 FP 5 / LLM 679 TP）——3 个排除词不在这 200 句
  子集的 FP 集合里，无可测差异；**闭环收敛**：当前反馈能支撑的学习
  （规则收紧）已全部落地；
- **轮次**：#3 自动评估再提案 `l3_retrain`（池 509 ≥ 20 + 门控 F1 0.000）
  → 人工驳回留痕（轮次报告：`L3 重训练（未执行）`）；
- **结论**：闭环机制端到端可用（反思 → 裁决 → 消费 → 复测 → 轮次留痕
  全程无故障）；当前剩余弱点是**结构性的**——L3 门控 F1=0（弃权聚集
  0.44–0.46）与漏斗门控召回 10.9%，都不是现有训练动作空间（重训 /
  词表 / 数据构建）能修的，需要模型级演进（更大容量 / 更强特征）；
  下一轮自学习的增量来自新反馈与新数据，而非重复消费既有建议。

### 2026-10-17 · 测评 funnel level 修复 + 层贡献度分布真貌

- **Bug**：eval_server funnel 模式未下发 `level` → 服务端缺省 1（纯规则）→
  「漏斗」实际只测规则层，LLM / L3 结构性为 0。06:24 funnel（Fβ=0、LLM 门
  192/192 却 0 TP）与 07:33 funnel（LLM 触达 0）均为该 bug 的产物，两者互斥、
  都不是真实级联的测量。
- **修复**：`datasets/eval_server.py`——`detect()` 恒显式下发 `level`
  （funnel=2：L3 门控、LLM 只跑长尾；capability=1+force_llm）；旧 funnel
  缓存已清，4 份 funnel 变体（full / dec-off / l3-off / 双关）fresh 重跑。
- **真实分布**（funnel level 2，`20261017T074213Z`）：overall Fβ 0.7878
  （P 0.9112 / R 0.5109）；LLM 677/677 严格 TP（Fβ 0.8065，触达 99.5%）；
  规则 0 TP / 5 FP（P=0，用户 6 次拒绝全部来自该层）；L3 弃权 191/192，
  仅 1 次自信判定（且错）。
- **贡献表（实测）**：L3 Δ=+0.7878（= 全部 Fβ）——拓扑假象：现配置决策层
  关闭，L3 的「贡献」是「不挡 LLM」（全弃权→全放行），而非「自己解决」；
  decision Δ=0；核心基线（双关）Fβ=0。
- **结论**：分布退化——100% 价值集中在最贵层，两个廉价层为 0（其一为负）。
  方向不是「调分布」而是：① 规则层收缩/停用（P=0、拒绝来源）；② L3 需要
  真正开始判定（弃权聚集 0.44–0.46 是模型容量问题，在当前训练动作空间之外）。

### 2026-10-17 · Phase 0 诊断 + Phase 1 执行（规则停用 / 重训无效标记 / 贡献表诚实化）

**Phase 0.1 — 全量（1137 句）level-1 纯规则评估**（eval_server 新增 `rules` 模式：
level 1 无 LLM，即 web-ui 生产默认配置）：

- 规则层 **185 预测 / 0 TP / 185 FP**（P=0，全量口径；200 句子集上为 24/0/5）
- 逐规则拆解（4 条 zh 规则，其余 14 条零触发）：

  | rule_id | pred | TP | FP |
  |---|---|---|---|
  | ZH_MISSING_SUBJECT | 107 | 0 | 107 |
  | ZH_PUNCTUATION | 52 | 0 | 52 |
  | ZH_DE_DA_DE | 18 | 0 | 18 |
  | ZH_CONFUSABLE | 8 | 0 | 8 |

  **没有一条规则值得保留** → Phase 1.1 全部停用。

**Phase 0.2 — L3 置信度全量 dump（1137 句直调 4806 服务）**：

- **显式弃权 1132/1137 = 99.6%**——弃权不是灰区阈值算出来的，是服务端的
  `abstain` 标志位直接给出的
- 机制（`feature-model/server.py`）：`P(error) < 0.25` → 明确无错；
  `[0.25, 0.50)` → 灰区弃权；`≥ 0.50` → **模式库命中才定案，未命中 → 弃权**；
  任一句弃权 → 整段弃权
- **概率头是常数函数**：P(error) 全量铺在 [0.40, 1.0]，`< 0.40` 的句子 **0 句**
  ——模型永远说不出「明确无错」；高置信带（485 句，97.7% 有 gold）与灰区带
  （647 句，93.2% 有 gold）的 gold 率几乎相同 → **零判别力**
  （此前「0.44–0.46 聚集」实为以 0.45 为中心的宽平台）
- **模式库只有 5 条中文 pattern**（虚词/把字句/时间序/重复字/副词+的）——
  490 句高置信句中仅 5 句被模式命中定案
- **Phase 1.2（阈值 A/B）判死**：降 `abstain_threshold` 只会把灰区句挪进
  「高置信但未命中模式」→ 照样弃权，**零效果**；重训无效（round #2 已证）
  的根因在此——瓶颈是概率平台化 + 模式库覆盖，不是分类器样本量

**Phase 1 执行**：

- **1.1 规则停用**：4 条 zh 规则经 `PUT /gec/rules/:id/toggle` 全部停用
  （DB + 词表文件双端一致；toggle 路由补了 `sync_rules_to_file` 防文件漂移）。
  200 句 capability 复测（同种子）：**Fβ 0.7894 → 0.8074（+0.018）、
  P 0.9126 → 0.9431、FP(重叠) 46 → 27（−19）**，TP/召回不变（规则本就 0 TP）。
  用户 6 次拒绝全部来自该层 → 摩擦源已除
- **1.3 重训无效标记**：`reflection_center` 新增 `L3RetrainStateIn`
  （最近 completed `l3_retrain` 作业的 gate_f1/report_id + 当前报告复测 F1）；
  复测 F1 未超过重训前值 +0.05 → 不再提议 `l3_sample`，改出 findings
  「已重训且复测无改善 → 标记重训无效」。api-server 与 desktop-app 两个
  调用点同源组装；2 个回归测试（无效抑制 / 同报告未复测仍提议）
- **1.4 贡献表诚实化**：`LayerContribution` 新增 `tp_self`（全开报告中该层
  自身定案 TP）+ `topological`（自身零定案且 Δ>0.05）；web-ui 贡献表加
  「自决 TP」列 + 「拓扑」标签 + 假象说明；2 个回归测试
- **1.5 成本指标**：报告 funnel 新增 `llm_calls` / `llm_calls_per_100`
  （一等字段），web-ui 测评页展示
- **1.6 反思中心两处诚实化（执行中发现的同类问题）**：
  - 测评证据原先取「该语言最新报告」——补跑消融期间会引用 Fβ=0 的
    l3-off 消融报告当「当前状态」证据（得出「L3 门控 F1=0」的假证据）。
    改为 `select_evidence_report_id`：优先最新**非消融**报告（可单测）
  - 反馈侧 `rule_tune` 会对**已停用**规则再提「建议停用」（历史拒绝数据
    触发）→ 路由侧按词表停用状态过滤，findings 说明剔除
- **贡献表实测（规则停用后，4 报告同种子）**：L3 Δ=+0.8058 但 **tp_self=0
  → topological=True**（web-ui 显示「自决 TP 0」+「拓扑」标签+说明）；
  decision Δ=0；核心基线 Fβ=0。表不再把拓扑效应记在 L3 名下
- **Phase 2 优先级更新**：原 2.2「L3 特征工程」之前插入 **2.1 模式库扩展**
  ——485 句高置信弃权的 gold 编辑分布：插入 1378 / 删除 780 / 词替换 668 /
  字替换 562 / 标点 121；模式库是「模型置信 → 机械定案」的最后一环，
  且与词表 CEC 混淆字同源（词表 → L3 模式桥接）；模型容量（概率平台化）
  仍是 2.4 的独立问题
- **遗留**：`l3_sample` 建议仍会触发（门控 F1=0.010 < 0.9）——重训无效标记
  是前瞻性的（需一次「eval 证据触发的重训作业」被消费并复测后才生效；
  历史作业的 payload 无 gate_f1 字段）。且「门控 F1」本身度量的是级联行为
  而非分类器判别力（概率平台化问题它测不到）——指标重定义归 Phase 2

## 9. 数据资产沉淀（已实现）

纠正类产品最被浪费的部分是「用户确认后的正确文本」从未落盘。本次补齐三层资产：

- **资产 A（产品级）**：`detect_history.corrected_text` 列——检测时由
  `shared_model::apply_corrections(text, errors)` 确定性生成「应用全部建议后」文本；
  旧库自动 `ALTER TABLE` 迁移；历史面板可恢复纠正版全文。不受隐私开关影响（产品功能）。
- **资产 B（偏好级）**：`l3_data/<lang>/rejected.jsonl`——每次「放弃」沉淀片段级
  负偏好（fragment + 被拒修正 + 原因 + 来源层），未来 DPO/偏好训练的原料；
  当前只沉淀不消费。
- **资产 C（训练级）**：`l3_data/<lang>/adopted.jsonl`——采纳时前端送
  `src_text + corrected_text`（前端本地累积应用修正，与服务端 apply_corrections
  同构），服务端切句对齐成句级 `src→dst` 平行对（去重、编辑密度 >0.4 打 `weak`、
  上限 20000）。`feature-model/prepare_adopted.py` 做治理：去 weak、去基准块表
  （与 prepare_l3_pool 同源）、md5 稳定 20% holdout → `adopted_train/holdout.jsonl`。

**决策记录**：① 默认开启采集（自用产品定位；多租户部署 `GEC_L3_ADOPT_COLLECT=0`
显式关闭）；② 负偏好（reject）同样沉淀；③ 产品级全文与训练级句对**都做**。
`/gec/l3/status` 新增 `adopted` / `rejected` 计数（桌面端 `l3_status` 同构）。

## 10. 检测透明度：层级贡献摘要 + 来源徽章 + 置信度等级 + 多层共识（已实现）

**目标**：让用户在前台看清每条错误「是谁检出的」、每次检测「哪些层实际参与了、
各层贡献多少」，而不是只看到一列不可比的置信度数字。

**契约（全部向后兼容的增量字段）**：
- `Error.source`：规则层错误在引擎层统一补标 `rule`（决策 / L3 / 召回 / LLM
  层产出时已自标）；前端对全部来源展示徽章 + tooltip。
- `Error.also_found_by`：`merge_errors` 合并去重时，落选层的来源记入胜出者
  （多层共识，如「规则 + LLM 双重确认」）；前端展示「共识 ×N」徽章。
- `LayerReport` + `DetectOutcome.layers`：每层 `{layer, status, found}`；
  status ∈ ran / decided / abstained / unavailable / skipped。API
  `DetectResponse.layers`、桌面 `DetectResult.layers` 透传；
  `detect_history` 新增 `layers_json` 列（历史详情与「历史复用」路径回放）。
  前端 ResultsPanel 新增「检测路径」行：`规则 2 → 特征模型 定案 → LLM 未触发`。
- `llm_confirmed_clean` 透传到响应：空结果区分「仅规则层未查出」与
  「LLM 复查确认无错误」两种文案。
- **置信度等级而非裸数值**：各层数值口径不同（规则=启发式常数、特征模型=
  分类概率、LLM 无校准输出），前端按来源映射 高/中/低 等级（LLM 显示
  「LLM 判断」不给等级），原始数值放 tooltip。原则：**不给 LLM 编造数值置信度**。

**顺带修复（冒烟时发现，master 同样存在）**：HanLP `/analyze` 返回**字符偏移**
（Python 语义），zh 规则层直接当字节偏移用于 `Error.span`——CJK 文本 span 错位
约 3 倍，`apply_corrections` 在非字符边界切片 panic（既有集成测试
`put_settings_updates_hanlp_endpoint_and_rebuilds_detectors` 即因此失败）。
修复：`gec-zh` 检测器入口 `tokens_to_byte_spans` 统一经
`gec_core::char_span_to_bytes` 换算，附回归测试；修复后该测试转绿。

## 11. 项目 #1 重评：决策模型（vegaml）选型 / 架构作用 / 实际贡献（2026-10-18 测量记录）

**背景**：§7 项目 #1 = 「用测评中心消融工具在 P0.5 后重跑 vegaml vs L3-only，
判决：并入级联 / 继续观察 / 退役」。本轮完成系统评估（含新测量，工具：
`decision-model/probe_discrimination.py`，与 Phase 0.2 L3 体检同构）。

### 11.1 选型评估：vegaml 从未真正落地

- 设计选型的 vegaml（vegamllm/vegaml）**未发布到 PyPI**（README 已注明），
  实际运行的是适配层回退：sklearn **LogisticRegression + 16 个表面统计特征**
  （len/words/cjk_ratio/unique_ratio/repeat_ratio/punct_ratio/upper_ratio/
  digit_ratio + pat0–7 启发式标志）——无词法/句法/词汇化信号；
- 配置默认模型名 `vegaml-0.8b` 与实际后端（sklearn）不符，`models/*/meta.json`
  无 version/trained_at（历史复用的版本比对退化为 unknown）；
- 与 L3 特征模型（feature-model：21 维特征 + 模式库 + 机械修正 + 飞轮重训练 +
  版本化）相比是**能力子集**，且无独立数据管线。

### 11.2 架构作用（代码实况）

- 级联第二层，与规则/L3 **并行**执行（tokio::join）；启用时合并进错误列表
  （merge_errors，参与共识 also_found_by）；规则引擎故障时作为回退源；
- level 2 门控：`need_llm = 决策弃权 ∨ L3 弃权/不可用`——**L3 在场时决策弃权
  无法减少 LLM 调用**（L3 弃权率 99.6%，OR 短路）；仅 L3 缺席时决策才独任门控；
- sklearn 后端的产出形态 = **句级旗标**：`span=[0,len]`、error_type 恒
  grammar、**无 correction**——不产生可执行修正，只产生「此句有问题」标记；
- 熔断器 + 超时 + 故障回退纯规则；layers/decision_ms/decision_abstained 可观测。

### 11.3 实际贡献（实测）

| 证据 | 数值 |
|---|---|
| 生产历史库（langfixer.db，11555 条检测） | source=decision 错误 **0 条**；4804 服务当前未运行 |
| 既有消融「decision Δ=0」 | **平凡真**：eval_server 消融只能把 decision 关（无 ON 臂），默认已 off → 两臂相同 |
| 自带 dev 指标（zh） | acc 0.627 / P 0.978 / R 0.622 / F1 0.761——**base-rate 假象**：池内 95% 有错（zh dev 干净句仅 11） |
| 训练数据 | zh 909 句（干净 **44**）/ en 603 句（干净 **25**）——负类几乎缺席；真实用户文本（多数干净）上必然大面积 FP |
| **数据污染** | zh 训练+dev = 1137 句 = **MuC 评测池本身**（train 909 = 评测池 80% 被背记）；en 同源 JFLEG dev。任何 MuC/JFLEG 上的 A/B 对该层系统性偏袒 |
| 概率体检（probe_discrimination.py） | zh AUC=0.775 / en AUC=0.807（污染池上）；最低概率带 [0,0.25) gold 率仍 0.73–0.79 → **识别不出干净句**；弃权带 89% 实际有错（zh 65/73）→ 门控语义下「安全跳过 LLM」不可证 |

### 11.4 判决（建议）

**退役出产品路径**（从设置 UI 与引擎组装移除挂载点，保留 `decision-model/`
目录与 `decision.rs` 作实验代码）：

1. 选型未落地（sklearn LR 表面特征，判别上限低）；
2. 数据模型不可修复地缺陷（95% 正类 + 评测池自训练）——重训需先解决独立语料；
3. 架构位置被 L3 严格覆盖（同为句级门控，L3 能力超集；L3 在场时决策零杠杆）；
4. 生产 11555 次检测贡献为 0，从未有用户可见价值；
5. 公平 A/B 的前置成本（ablation ON 臂 + 独立语料重训 + LLM 预算）不值得——
   结论翻转概率极低。

**保留路径（可选）**：若未来想留作「独立共识票」（also_found_by 已支持展示），
前置条件 = 独立语料（非评测池）+ 平衡负类重训 + 概率校准，届时再评。

**工具缺口**（顺带记录）：`eval_server.py` 消融只支持 decision/l3 → off；
若要支持「开启」臂需扩展 `abl_fp`/`put_body`（`{"decision": true}`）。

### 11.5 替换试点：开源决策模型零样本门控 A/B（2026-10-19 实测）

背景：2026 开源「决策模型」生态（Laya / Kev / StartLux / Clef / Intern-Decision /
Torchcast）经核查属实且活跃（Laya 33.3k★ Apache-2.0；Kev 9.3k★ Apache-2.0；
StartLux 677★ 但 **CC BY-NC 排除主路径**；Clef = Cloudflare Apache-2.0）。
生态事实标准 API = Jev `/v1/systemone`（state + typed questions → 校准概率）。
本轮验证「换一个预训练决策模型」能否救活门控层——它确实绕开了旧模型的
数据死穴（预训练，不依赖被污染的 MuC 池）。

**方法**（工具已入库 `decision-model/gate_eval*`）：
- 干净评测集 `gate_eval/zh_gate_1k.jsonl`：996 句（498 有错 / 498 干净），
  CSCD / CSCD-NS / NLPCC23 / CCTC 分层抽样，与 MuC dev 污染池零重叠；
  另 `en_gate_500.jsonl`（JFLEG 317 句）作英文 sanity 臂。
- 臂：`laya_en`（ollama laya:latest，421M ModernBERT）、`laya_ml`
  （laya-serve :11435，laya-multilingual 322M mmBERT，含 zh）、
  `sklearn`（旧模型对照）；统一两选项 choice schema（模型卡：noul 低估 true）。
- 指标：AUC / ECE / 折半温度校准 / clean-skip 操作点（漏检率预算下的跳过率）。

**结果（zh_gate_1k）**：

| 臂 | AUC | ECE raw→cal | 操作点（漏检≤2%） | 延迟 p50 |
|---|---|---|---|---|
| sklearn（旧） | 0.520 | 0.227→0.060 | 跳过 0.2% | ~0ms |
| laya_en | 0.537 | 0.118→0.017 (T=4.7) | 跳过 0.3% | 31ms |
| laya_ml | 0.501 | 0.267→0.033 (T=8) | 无（跳过 0） | 24ms |

英文 sanity（JFLEG）：laya_en AUC **0.517**、laya_ml AUC 0.648——
**laya_en 连英文 GEC 都是机会水平** → 症结是任务族错配（这些模型训练分布 =
分类/路由/安全/意图，不含语法错误检测），不是语言迁移问题。与模型卡自警一致：
「zero-shot 未见任务族 near chance；fine-tune 才是能力来源」。

**判决**：
1. **零样本替换失败**——瓶颈从「模型容量」移到「任务适配」；§11.4 退役结论
   不变且被本轮加强。
2. 温度校准有效（ECE 0.27→0.03）但救不了零判别；任何「校准」讨论以 AUC 达标为前提。
3. kev-4B 臂当时未跑成：ollama 0.40.3 报 `unsupported decision encoding "kev"`；
   升级 0.40.6 在本机失败（brew/GitHub 资产为 adhoc 签名；官方签名版同样在
   macOS 27.0.1 beta (26A434) 下 dyld 挂起——连 `-v` 都挂，属系统级问题）。
   **副作用已解除：系统校验状态解除后，官方签名 Ollama 0.40.6 恢复正常，
   kev encoding 获支持，§11.6 据此补测。**
4. 若未来复活决策层，唯一可行路径 = **微调**（`laya-train` / Kev LoRA），
   语料 = gold 池 + 飞轮真实反馈（accept/reject 弱标签已在积累）；
   laya-multilingual 24ms p50 的延迟证明基础设施不是障碍。

### 11.6 补测：Kev 规模臂——第一个显著超机会的零样本臂（2026-10-19 实测）

环境恢复后（Ollama 0.40.6 官方签名版，kev encoding 获支持），补跑 kev 两臂，
同一 `zh_gate_1k` / `en_gate_500` 集、同一 choice schema、同一打分器：

| 臂 | zh AUC | en(JFLEG) AUC | ECE raw→cal | 操作点（zh 漏检≤5%） | 延迟 p50 |
|---|---|---|---|---|---|
| kev4b（turintech/kev-4b-artemis） | **0.751** | **0.892** | 0.211→0.123 (T=2.65) | τ=0.10 跳过 0.3% | 265ms |
| kev9b（turintech/kev-9b-artemis） | **0.765** | **0.921** | 0.205→0.111 (T=2.10) | τ=0.12 跳过 2.9% | 367ms |

kev zh 分源 AUC（4b/9b）：cscd 0.879/0.867、nlpcc23 0.797/0.785、
cctc 0.757/0.758、cscdns 0.644/0.682。

**修正与结论**：
1. §11.5「零样本替换失败」需收窄：**失败的是 laya 家族与旧 sklearn；
   kev 家族（Qwen3.5 底座 + 决策头）在中文 GEC 门控上零样本即有真实信号
   （AUC 0.75–0.77），英文更强（0.89–0.92）**。「任务族错配」仍解释 laya，
   但 kev 的训练分布显然包含可迁移的语言质量判断。
2. 规模轴（4B→9B）仅 +0.014 zh / +0.029 en——**规模不是主因，家族/底座才是**。
   「更大底座能否救零样本 GEC」的假设就此关闭，无需再测 torchcast-27b
   （且其 CC BY-NC 许可本就进不了产品路径；Intern-Decision 属多模态界面
   理解族，任务族最远，同样不测）。
3. **仍未达产品门控标准**：zh 漏检≤2% 时跳过率仅 1.3%（kev9b 最好）；
   cscdns（最难干净集）AUC 最低 0.64–0.68；raw 概率过自信（ECE≈0.2）；
   kev9b 367ms p50 处于门控预算边缘。零样本 kev 可作信号源，不可作跳过依据。
4. 产品决策不变（决策层退役/默认禁用不变），但**复活路径的微调底座定为
   Kev**（Apache-2.0、ollama 原生、LoRA 可行、零样本 0.765 即起点），
   不再考虑 Laya。验收仍走 `gate_eval.py`：AUC、安全操作点、漏检预算、延迟。

### 11.7 微调试点启动：语料就绪 + MPS 全链路 smoke 通过（2026-10-19）

官方训练路径读实（jaredpalmer/kev，9.3k★ Apache-2.0）：`kev.train --data
<JSONL> --init_from jaredpalmer/kev-9b`（warm start LoRA+pointer head，官方
对照从 base 训 0.33 vs init_from 0.83）；`--device auto` 支持 CUDA→MPS→CPU；
训完 `kev.serve` 起的就是 `/v1/systemone`——**gate_eval.py 即现成验收器**。

- **语料**（`gate_ft_build.py` → `gate_ft/`）：train 9,866（4,933 有错/4,933
  干净，cscd/cscdns/nlpcc23/cctc/nacgec/mucgec 六源）+ dev 1,000（温度拟合/
  早停用）；与冻结测试集 gate_eval/zh_gate_1k **文本级零重叠（断言自检）**。
  飞轮弱标签仅 2 adopt/6 reject，本试点以 gold 池为主，弱标签继续积累。
- **MPS smoke（0.8B）全链路通过**：warm start（372 lora tensors + head）→
  loss 1.095→0.819 → `kev.serve :8009` → gate_eval 打分正常。300 句训练的
  smoke 模型在冻结集 AUC 0.547（无信号，符合官方「400 条内增益在噪声内」）。
- **环境事实**：HF 直连被墙（HTTP 000），`HF_ENDPOINT=https://hf-mirror.com`
  可用；ollama 0.40.6 正常（kev encoding 支持）。
- **后端测算**：MPS 0.8B ≈1.1s/rec → 9B 全量 ≈40h 不可行；**决定用 3090
  租卡**（24GB：9B bf16 权重 ≈18GB + 短句激活可容纳；`flash-linear-attention`
  +`causal_conv1d` 在 CUDA 可用，去掉 MPS 参考 kernel 惩罚；预计 10–14h
  ≈¥20–30）。Runbook 见 `decision-model/finetune_3090.md`。
- 验收线不变：zh AUC ≥0.85、漏检≤2% 时跳过率 ≥20%、校准后 ECE <0.05、
  延迟达标——全部通过才讨论默认启用。

## 12. 数据评估：midu2022 初赛 100 万伪数据（2026-10-19 实测）

> **结论已锁定（§13 #1–#5）**：不扩 n-gram 表、不进 L3 句级池、不做评测、不扩词表；
> 唯一未排除路径 = 字级判别侧车的 clean 长文负例（未测）。复评须带新证据。

外部文件 `preliminary_train.jsonl.gz`（95.8MB gz / 330MB / 1,000,000 行，
schema `{source,target,label}`，即 CTCDataset README 里 midu2022 初赛伪数据）。
复现工具（已入库，结论复评必须用它们，不要自造口径）：
`datasets/gold_error_dist.py`（错误分布对照，`--pairs` 可跑任意候选语料）、
`feature-model/eval_ngram_table_ab.py`（表 A/B：AUC + 等安全线放行率）、
`feature-model/eval_l3_data_ab.py`（L3 数据增益三臂）、
`feature-model/eval_l3_funnel.py`（配 `L3_NGRAM_TABLE`/`L3_BIGRAM_TABLE` 看 MuCGEC FN）。
形态与重叠审计是一次性脚本，结论已在本节全量记录，无需复跑。

**形态**：去重 src 999,837 / tgt 962,039；**label=1 占 100%（零干净负例）**；
句长 p50=44 / mean=50 / max=733；877 句含 `ued*` 脱敏占位符（须过滤）。

**污染：零**。src ∩ `l3_pool_raw` = 0.00%，src ∩ 全部 gold = 0.00%，
tgt ∩ gold = 0.00%（tgt ∩ pool_ref 仅 467 句）。与 twnlp 那批完全不相干。

**错误分布与真实流量偏 3 倍**（抽样 3 万条字级 diff）：

| 数据 | S | D | I | 单字替换同音占比 |
|---|---|---|---|---|
| midu 伪数据 | 44.7% | 32.0% | 23.3% | **24.5%** |
| cscdns（真实母语微博） | 99.4% | 0.3% | 0.3% | 80.9% |
| cscd / sig15 / med / sig13 / sig14 | 99–100% | ≈0 | ≈0 | 57–71% |
| midu_gold（同源人工修正） | 67.3% | 21.8% | 10.9% | 73.4% |

→ 注入器是 OCR/形近 + 增删（`槭凝→只能`、`坑险→保险`），不是拼写错误。

### 12.1 已测否定项：不用它的干净侧扩 n-gram 表

`build_ngram_table.py` 已加 `--extra-clean <jsonl.gz>`（只取 target 侧，过滤
`ued` + 去重 + 块表去污染；实测入表 932,452 句，distinct trigram 11.8M）。
以 `eval_l3_funnel.py`（MuCGEC 1137 句 + 真实用户文本门控段，`GRAY_ZONE_LOW=0.30`）
扫表规模，得到单调的**成本↔安全滑动条**，没有任何一档改善判别比：

| 表配置 | trigram | MuCGEC LLM 调用率 | MuCGEC 启发式 FN | cscdns clean/病句放行 | cctc clean/病句 | confirmed |
|---|---|---|---|---|---|---|
| **现表（无 midu）** | 1.00M | 63.1% | **30.6%** | **3% / 1%** | 10% / 4% | 70% |
| +midu 同规模 top-1M | 1.00M | 67.5% | 26.4% | 3% / 3% | 12% / 5% | 66% |
| +midu @1.5M/400K | 1.50M | 60.8% | 32.7% | 5% / 5% | 16% / 8% | 72% |
| +midu @2M/500K | 2.00M | 54.9% | 38.4% | 8% / 7% | 20% / 10% | 75% |
| +midu @3M/700K | 3.00M | 44.6% | 48.5% | 14% / 12% | 28% / 14% | 81% |

- 表面收益（cscdns clean 放行 3%→14%、灰区率 68.4%→19.3%）是**整体平移**：
  加权 OOV 对 clean vs err 的 AUC 仅 0.47（midu 0.35，反向），等安全线
  （保留 95% 错误）下 clean 放行率只从 7.3% 升到 8.0%。
- 代价实打实：MuCGEC 漏检 30.6%→48.5%（FN 丢失编辑 1,864→3,260），
  cscdns 病句漏检 1%→12%。**母语判别比（clean/病句放行比）从 3:1 塌到 1.2:1**，
  且阈值扫描救不回来（0.15 档仍 11% 漏检，0.10 档全弃权）。
- 机制：表越大 → 错误句的 OOV trigram 越容易被合法语料覆盖 → 启发式（表面异常门）
  敏感度下降。扩表只移动工作点，不移动 ROC。
- **处置：现表保持不变**（本次实验产物全部留在 `/tmp/tbl_*`，未入库）；
  `--extra-clean` 保留为可选开关，默认不用。A/B 用
  `L3_NGRAM_TABLE` / `L3_BIGRAM_TABLE`（`eval_l3_funnel.py`）与
  `L3_NGRAM_OUT` / `L3_BIGRAM_OUT`（`build_ngram_table.py`）。

### 12.2 L3 句级门控三臂（特征用 3M 新表，HistGB 代理 vegaml GBM）

池 = `data/zh/train.jsonl` 458,279 行（label1 228,808）；B/C 各加等量 midu 行
（B 正例仅「全部编辑均为 1-1 且拼音相同」= 全量文件里 96,870 对，占 9.7%；
C 任意注入错误 120,000 对；各自的 target 作负例）。

| 臂 | 训练行数 | cscdns AUC/放行 | midu | cctc | ctc21 | dev(池内) |
|---|---|---|---|---|---|---|
| A 仅现有池 | 458,279 | 0.530 / 5.0% | 0.619 / 17.5% | 0.577 / 7.9% | 0.551 / 4.0% | 0.714 / 19.7% |
| B +midu 筛同音 | 652,019 | 0.545 / 7.3% | 0.641 / 24.8% | 0.547 / 9.3% | 0.522 / 4.6% | 0.708 / 19.1% |
| C +midu 不筛 | 698,279 | 0.550 / 6.9% | 0.652 / 23.2% | 0.561 / 9.8% | 0.551 / 3.8% | 0.707 / 19.4% |

- 收益有限：目标域（midu）放行率 17.5%→~24%、AUC +0.02；cscdns AUC +0.02、
  放行率 +2pp；池内 dev AUC 微降（0.714→0.707）。**远不是「3%→几十个百分点」**。
- **筛同音 vs 不筛几乎无差别**（B 在 cctc/ctc21 还略差）→ 增益主要来自
  96 万条母语长文 **clean 负例**，错误侧贡献可忽略。句级 AUC 本身仍在 0.52–0.65
  的弱区，与 §E「句级门控在母语域需要字级判别模型」的结论一致。

### 12.3 判决

| 用途 | 结论 |
|---|---|
| 扩 n-gram / bigram 背景表（启发式主信号） | **否**（实测净负，见 12.1） |
| 进 L3 句级训练池 | 暂否——增益小（12.2）且错误分布偏 3 倍；若做，只取 clean 侧作负例 |
| 做评测集 | **否**（伪 gold；做评测用已建的 `midu_gold`） |
| 扩混淆字/词表 | **否**（OCR 形近对与真实同音流量不同源） |
| 字级判别侧车（`finetune_char_ctc.py`）的 clean 长文负例 | **否**（实测判负，见 12.4） |

### 12.4 字级侧车 clean 负例双臂 A/B（2026-10-19 实测，12.3 的「未测」→ 关闭）

双臂 abA / abB 落 `models/char_ctc_zh_ab*`，基线 = 生产默认 **v2**；配对评测
`eval_char_pla_ab.py char_ctc_zh_v2 <arm>`（22,549 句，9 原生集 + MuCGEC + holdout，
vis=99/97/95/90 档）。⚠ **双臂的 `--extra-clean-max/repeat` 档不可考**（训练早于
`train_meta.json`，`/tmp` 训练日志已被系统清理）——结论只由两个 checkpoint 的
分数配对支撑，重跑才能复现具体档。

| 臂 | 原生 AUC | clean p50 | 等漏检放行率 4 档 Δpp 均值 | 判定 |
|---|---|---|---|---|
| abA | 8/9 集下降（cscd 0.901→0.889、nacgec 0.843→0.832；sig13 0.840→0.861 唯一升） | 9/9 集 up（cscd 0.027→0.053） | +2.33 / −0.36 / +1.03 / −0.38 | **判负**（仅 1/4 档 ≥2pp + AUC 净降 → 纯分数平移） |
| abB | 7/9 集下降且崩：midu 0.792→0.529、nlpcc23 0.762→0.591、ctc21 0.821→0.718（cscdns 唯一升、cctc 持平） | 2 升 7 降（cscdns 0.0745→0.0885 升 / midu 0.026→0.012 降） | −5.99 / −10.13 / −10.42 / −11.47 | **判负**（全档显著负） |

- 形态同 §12.1（n-gram 表）：加母语长文 clean 负例只**平移分数分布**（abA 全集 p50 up
  而放行率不涨；abB p50 反向且 AUC 崩），判别比不改善——即 clean 负例把模型打成
  「长文→clean 先验」，而不是「错字→异常」。
- **生产 default 保持 v2 + τ=0.004**；abA/abB 留作记录（`models/char_ctc_zh_ab*`）。
- **判定器口径已修**：旧版按「最好一档」自动判定会把 abA 误报「通过候选门槛」
  （+2.33pp 单档）；现按 §13 原文——多数档 ≥2pp 且 AUC 未净降才通过。
- **复产纪律**：`finetune_char_ctc.py` 现落 `models/<out>/train_meta.json`
  （argv/seed/counts/dev 指标）；abA/abB predates it，复该实验须重跑。

## 13. 已测否定项清单（决策锁定，勿重复讨论）

以下结论均已实测定案，**再次提出同一方案时先复核本表并给出新证据**（新数据/新口径/
新评测集），不要重新论证。评测复现命令附在各节。

| # | 方案 | 结论 | 关键实测 | 依据 |
|---|---|---|---|---|
| 1 | **用外部干净语料扩 n-gram / bigram 背景表**以降灰区率（任何来源；midu2022 干净侧 93.2 万句实测 @1M/1.5M/2M/3M 四档） | **否** | 成本↔安全单调滑动条，判别比不改善：MuCGEC LLM 调用率 63.1%→44.6% 但 FN 30.6%→48.5%；cscdns clean 放行 3%→14% 同时病句漏检 1%→12%；加权 OOV 的 clean-vs-err AUC 仅 0.47；阈值下调救不回（0.15 档仍 11% 漏检） | §12.1；复现 `eval_ngram_table_ab.py` + `eval_l3_funnel.py` |
| 2 | midu2022 初赛 100 万伪数据（`preliminary_train.jsonl.gz`）进 L3 句级训练池 | **暂否** | 三臂对照：AUC 仅 +0.01~0.03、放行率 +2~7pp；筛同音 vs 不筛无差别（增益全来自 clean 负例）；错误分布 S/D/I=45/32/23、单字替换同音 24.5% vs 真实流量 99.4% / 80.9% | §12.2；复现 `eval_l3_data_ab.py` + `gold_error_dist.py --pairs` |
| 3 | 该伪数据作评测集 | **否** | gold 由注入器生成（非人工标注）；做评测用已接入的 `midu` gold（人工修正 1,010 句） | §12 |
| 4 | 该伪数据扩混淆字 / 词表 | **否** | 注入器是 OCR/形近（`槭凝→只能`），与真实同音流量不同源 | §12 |
| 5 | 该伪数据干净侧喂字级判别侧车 clean 负例 | **否**（双臂均判负） | abA 等漏检放行率 4 档 +2.33/−0.36/+1.03/−0.38pp 且 8/9 原生集 AUC 下降（= 纯分数平移）；abB 全档 −5.99~−11.47pp、AUC 崩（midu 0.792→0.529、nlpcc23 →0.591、ctc21 →0.718） | §12.4；复现 `eval_char_pla_ab.py char_ctc_zh_v2 char_ctc_zh_abA`（同 v2 对照 abB）。双臂 `--extra-clean` 档不可考（早于 `train_meta.json`），重跑须读该 meta |
| 6 | `CTC2021/train_large_v2`（217,617 对）进 L3 句级池 | **否** | 抽样为 OCR/形近合成噪声（`郡钩→这`、`煤恒→没有`）；其"只喂 n-gram 干净侧"的备选已被本表 #1 覆盖 | `datasets/README.md` 审计节、`ctc_audit.py` |
| 7 | MCSCSet test/valid 作评测基准 | **不可用** | 句级重叠 100%（已在 n-gram 表 / L3 池内）；要用须先隔离 `MCSCSet/train` 并重建资产 | `ctc_audit.py` |
| 8 | 沿用历史 sig15 数字 | **作废** | gold 已整体替换为 yacsc 修订版：含错 542→618、编辑 644→792（+23%），旧 FP 部分是标注噪声 | `datasets/README.md` 审计节 |
| 9 | zh 规则层 4 条规则（MISSING_SUBJECT / PUNCTUATION / DE_DA_DE / CONFUSABLE） | **全部停用** | 全量 1137 句纯规则：185 预测 / 0 TP / 185 FP | §Phase 1（2026-10-17） |
| 10 | vegaml 决策模型回到产品路径 | **退役**（保留实验代码） | 选型未落地、95% 正类 + 评测池自训练、被 L3 严格覆盖、生产 11555 次检测贡献 0 | §11.4 |

> #1 是最容易复发的一条：**「灰区率下降」不等于收益**。任何扩表/换语料的提案，
> 验收口径必须是「**等漏检下的放行率**」——先跑 `eval_ngram_table_ab.py`（AUC 不升或
> 等安全线放行率增幅 <5pp 即拒绝），再跑 `eval_l3_funnel.py`（`L3_NGRAM_TABLE`/
> `L3_BIGRAM_TABLE` A/B，看 MuCGEC FN 与真实用户文本门控段的 clean/病句放行比）。
> 只看灰区率或 LLM 调用率的报告一律不采信。







