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
飞轮重训练）。两者同开时决策模型是冗余层。项目 #1 重评已完成（§11）：
**判决 = 退役出产品路径，2026-11-02 执行落地**——设置界面卡片 / 状态条 /
管线条入口移除，代码保留为实验路径（`GEC_DEC_ENABLED=1` 显式开启）。
**2026-11-13 定位调整**：这条线**不放弃**，由「退役」改为**待发布**——当前模型
（vegaml-0.8b 零样本/fine-tune 谱系）任务适配不足，等找到更合适的决策模型后再上线；
产品面入口与探测口径同步改为「未启用（待发布）」。

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
  β=0.5，cscd β=1）；英文基准两侧 span 先经 `norm_span` 空白不敏感化（JFLEG gold
  的 S/D span 含尾随空白、插入锚点取下词起点，而生产 diff 预测词界精确——不归一
  则英文 strict 命中恒 0，11-12 定版根因；中文无空白 → 恒等变换）；
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
| **项目 #1** | 决策模型重评：用测评中心消融工具在 P0.5 后重跑 vegaml（新版）vs L3-only | P0.5 | ✅ 已评估（本文档 §11）：消融公平性不成立（评测池污染 + 无 ON 臂），改为直接体检；**判决：退役出产品路径**——✅ 2026-11-02 已执行（产品面入口移除，实验代码保留） |

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
- **1.1a 金集评测口径对齐（2026-11-19）**：停用出厂规则后 `media_gold_eval`
  两处隐含前提失效——离线正对照原用 `ZH_PUNCTUATION` 半角逗号（旧出厂
  enabled=true 必命中），改为主用 `ZH_NUMERAL`「万万」词表错（出厂开启、
  确定性命中）；导出表位置口径回填断言由「≥1 行（#13 报道/报到 =
  `ZH_CONFUSABLE`）」改为口径增量恒等式（位置命中数 − 文本命中数），回填
  机制本身由 media_core gold.rs 的 Positional 单测与真实载体重演（--ignored）覆盖
- **1.1b 段级进度事件（2026-11-19，P4 首子项）**：`StageEvent` 新增
  `ChunkBegin/ChunkEnd`（wire kind `chunk_begin`/`chunk_end`）；两条预算块路径推出——
  ③ LLM 整文档分块（`detect_document` 经 `llm_layer_detect` 透传 sink）与 stat 滑窗
  （新 trait 默认方法 `detect_traced`，级联 rule 分支注入，stat 臂覆写逐窗推
  stage=rule）；短文本单块不推，块间并发交错按 chunk 序号配对聚合；前端
  StageView 加 `chunks` 聚合，进行中节点副行显示「段 已检/总」；桌面 detect:stage
  透传天然兼容（枚举直序列化）
- **1.1c 入口收敛（2026-11-19，P4 第二子项）**：api-server 内新增唯一执行入口
  `detect::run_pipeline(spec)`（= `run_detection` + 参数快照 / 版本向量 / 历史回写），
  detect / detect_lang 两 handler 与 analyze 降为其驱动，消除路由层第二份执行实现
  （analyze 原内联的 sink / 历史复用 / 引擎取用段删除，自带 persist 保留并改用
  管线产出的 params 快照 / version）。**analyze 行为收敛**（设计意图即单一实现）：
  自此获得 LRU 缓存命中（level 1 非强制）、issue_type 填充、anomaly 自动采纳与
  L3 干净句后台任务（原内联版没有）；历史写入语义不变（带 analyzed_json，
  quality 门控 map_or(true)）；mcp / media 继续直驱 run_detection，doc-jobs 为
  gec_core 后台 executor 不在本层。api lib 45 + api 58 全绿，无断言修改
- **1.1d 桌面并轨（2026-11-19，P4 第三子项）**：桌面 `commands.rs` 新增与
  api-server 语义同构的唯一执行入口 `run_pipeline`（历史复用含 layers 回放 /
  级联检测 / 抑制过滤 / issue_type / anomaly 采纳 / L3 干净句采集 / 参数快照），
  detect / analyze 两命令降为驱动，消除桌面内联第二实现。**桌面 analyze
  行为收敛**：自此获得 issue_type 填充、anomaly 自动采纳、L3 后台采集与
  zh 归因粗族折叠（§11.11 兼容，en/ja 原样；改变桌面 zh 决策表归因行为，
  对齐 api 为设计意图）；历史表补齐 corrected_text / layers_json 两列
  （旧库轻量迁移），detect / analyze persist 均落两列，历史复用回放 layers。
  **残余差异**：桌面仍为独立实现（非共享 crate）但语义同构；进程级 LRU
  缓存桌面本就无，未补。cargo check --workspace 全绿，桌面 2 / api lib 45 /
  api 58 全绿，无断言修改
- **1.1e 可观测性四件套（2026-11-19，DETECT_PIPELINE §7）**：
  ① **dropped 账本**（gec-core）：`DroppedLedger{ownership, pii_mask, out_of_range,
  out_of_scope}` 挂 `QualityReport.dropped`（serde default + 空则省略）；掩码过滤、
  所有权段外丢弃、分块前缀重映射出块丢弃逐桶计数（并发分块用流循环局部变量
  累加防跨请求串账），assemble 签名不变；② **每请求 summary**（api-server 新模块
  `request_summary.rs`）：`run_pipeline` 在历史写入之后无条件落一行 SQLite 账本
  （detect×2 / analyze 两入口；回放行 quality/status 置 NULL；doc-jobs / media /
  桌面不写，遗余），含版本向量 / quality / chunks / sentences / dropped / 耗时；
  ③ **两告警**（§5 危险象限的机器盯梢）：完整性（24h 内 unowned≠0 或 dropped
  非零的行数 >0，写行处同时 `tracing::error!` 即时点名）+ 质量（grammar_novice
  兜底率 >30% 且样本 ≥20，或 partial∧零检出 ≥5 次），
  `GET /gec/observability/alerts`；④ **看板**：`GET /gec/observability/summary?days=N`
  按 day × rules_fingerprint 内存聚合（请求数 / coverage 均值 / 句复查率 /
  unavailable 率 / cache 命中 / history 复用 / dropped 分原因 / LLM 块完成数代理 /
  总耗时 P95 最近秩），web-ui 进化中心新增「可观测」标签页
  （ObservabilityPanel：两告警卡 + 窗口选择 + 聚合表）。
  gec-core 289 / api lib 51 / api 58 / vitest 126 全绿。
  **遗余**：块级 P95（需块级耗时列，二期）、doc-jobs/media/桌面 summary 写入
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

### 2026-11-12 · 质量指标定版（capability 全集，mucgec + jfleg）

**定版载体**：测评中心 capability 模式（level 1 + force_llm）全量报告，生产配置
快照实例（:4811，生产二进制 + settings 副本 + 隔离 auth.db/core.db）。纪律：
seed 42、workers=2 + 耐心档（超时 240s / 挂钟预算 900s / 退避 15,45）、收敛按
**gold 全集行数**判定、**网关健康门**（断供窗口里的 `llm_ran=false` 是「网关没
服务」不是「产品检不出」，绝不带进定版数字——本轮 repair 轮实际补掉 345 句缺席）。

**量具修复（本刀的正题）**：定版首轮 jfleg 出现 **4489 预测 0 条 strict 命中**的
结构性异常——根因是坐标系错位，不是检测能力：`make_gold.py` 明文口径「JFLEG gold
S/D span 含尾随空白、插入锚点取下词起点」，而生产侧 LLM 句级 diff 的预测 span
词界精确（gold `[56,64]` vs 预测 `[56,63]`），strict 键 (op, ref, span) 永远差一
个空白位。修复 = `eval_server.norm_span` 空白不敏感化（严格键与重叠判定同一坐标
系；中文无空白 → 恒等变换，zh 全集数字与修复前**逐位一致**，selftest 补英文坐标
系用例钉死）。

**定版数字**（报告 `20261112T223729Z_{mucgec,jfleg}_current_capability`，
arm=current / generic 臂 / 网关 Qwen3.8，LLM 触达率均 100%）：

| 基准 | 句数 | gold 编辑 | 严格 P / R / Fβ | 重叠召回 | 句级门控 P/R | LLM 层召回份额 |
|---|---|---|---|---|---|---|
| mucgec（β=0.5） | 1137（gold 句 1082） | 7258 | 0.0204 / 0.0056 / **0.0134** | 0.2268 | **0.975 / 0.850** | 0.473 |
| jfleg（β=0.5） | 1501（gold 句 1434） | 15775 | 0.1559 / 0.0444 / **0.1038** | 0.1835 | **0.954 / 0.944** | 0.518 |

解读立场：句级门控（这句有没有错）双语都在 0.85+，是产品当前能站住的数字；
编辑级严格口径受 generic 零样本臂支配（规则层仅 125/23 条预测、strict TP 2/4），
与 §8 P0 的 cec3 微调臂（F0.5 0.805）不是一个臂，**不可横向比**。3090 上的 gec
新模型出来后按同载体补对比版报告。

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

### 2026-11-13 · L3 特征模型公网上线（先暂关后开闸）

公网实例（47.94.172.217）的 L3 特征模型服务当日完成上线：上午实况是 4806 从未部署
（无监听、模型仅 2KB stub、系统 Python 3.6.8 跑不动 FastAPI），故先 `l3.enabled=false`
暂关并修门控语义（engine.rs `l3_disabled`：显式关闭 → level 2 开闸回退无条件 LLM）。
下午用 `feature-model/l3_data/zh/train.features.jsonl`（45.8 万句）训练真实 sklearn
句级模型（22 维特征；dev 15000 句 acc 0.627 / P 0.694 / R 0.455 / F1 0.549），以
docker（python:3.12-slim + scikit-learn==1.9.1）挂 `langfixer-l3.service`  systemd
单元，绑定 127.0.0.1:4806，`l3.enabled` 原位开回 true。上线当日公网实测抓到并修复
一个门控语义洞：**level 3 定案不得终结全文 LLM**（深度扫描=漏报兜底臂，句级门控
R≈0.45 曾把 3 例内置示例放行 2 例；engine ③-F 加 level 3 例外，level 2 放行不变）。
**决策层**同日由「退役」改
「待发布」（§13 #10 注）：线不放弃，待更合适的决策模型。遗留：3090 gec 新模型出来后
补同载体对比版，并评估是否把 ELECTRA CTC 字级侧车（char_gate）一并上线。

### 2026-11-13 · 各层贡献度报告界面（参赛需求）

参赛方要求「展示各层在评测报告中的贡献」。既有 `POST /gec/eval/contributions`
（消融 Δ）与 `GET /gec/eval/reports|report/:id` 都是**转发** Python 评测服务，
生产实例上没有评测常驻进程，等于公网不可用；而定版报告 JSON 本身已完备
（`layers.*` 每层 `n_pred/p_strict/r_strict/f_beta/gate/recall_share/
attributed_gold` + `funnel` + `config` 开关）。故新开端点 `GET /gec/eval/layers`：
api-server **直读**报告档案（目录按 `GEC_EVAL_REPORTS_DIR` > `datasets/eval_reports`
> `src/datasets/eval_reports` 取首个存在者，非报告与坏文件按条跳过），并把该
只读端点加进评审密钥的 fail-closed 白名单——评委能核对「质量来自哪一层」，
但发起评测仍属运营面。前端「各层贡献」视图给报告选择 + 两报告 Δ 对比 +
层级贡献表 + config 开关 chips + 级联漏斗；作为核心视图对网关形态全员开放，
单机形态只在运营位显示入口（数据是本地档案，普通用户看到只会是空页）。

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

### 11.8 零样本臂补测：chinese-decision / Largha（2026-11-05 实测）

背景：决策模型生态复查（GitHub topic:decision-model / systemone 全量检索）后补两臂，
验证「换一个训练分布」能否零样本救活门控。AnyJev（nokia-applied-research，读出/校准
库非模型，验证任务族=语义决策）判不构成臂，不测。jeff 明示 English-only，不测。

- **chdec** = yanqiangmiffy/chinese-decision-0.8b（代码 MIT / 权重 Apache-2.0；
  Qwen3.5-0.8B + CJ-Dataset 1300 万条中文决策 LoRA；官方 `serve.py` hmm/System One
  兼容，MPS p50 190ms；自带校准温度 1.108）
- **largha** = Phocinae-Largha-150M-v1（Apache-2.0；mmBERT-small 144.3M 双语
  en+zh 编码器，权重 288MB；`phocinae-server` `/v1/systemone`，本机 CPU p50 968ms）

同一 `zh_gate_1k`（996 句）、同一两选项 choice schema、同一打分器；结果文件
`gate_eval/results_{chdec,largha}.jsonl` 入库。

| 臂 | zh AUC | ECE raw→cal | 安全操作点 | 延迟 p50 |
|---|---|---|---|---|
| chdec | **0.524** | 0.358→0.036 (T=8) | **无**（漏检率不达标） | 190ms |
| largha | **0.498** | 0.094→0.021 (T=8) | **无** | 968ms |
| kev9b（既有） | 0.765 | 0.205→0.111 | τ=0.12 跳过 2.9% | 367ms |

分源 AUC：chdec cctc 0.402 / nlpcc23 0.445（低于机会）；largha 四源 0.43–0.52。

**结论**：
1. 两臂均机会水平——§11.5「任务族错配」判决在**第三个模型家族**（中文决策 LoRA）
   与**第四个**（双语小编码器）上复现，零样本替换路径就此关闭。
2. **修正 §11.6 归因**：chdec 同为 Qwen3.5 家族却 0.52——「家族/底座是主因」应
   收窄为「**决策训练分布须含可迁移的语言质量信号**」；底座只决定微调上限与工具链，
   不提供零样本 GEC 信号。kev 的零样本信号归因其训练混料，非 Qwen 底座。
3. 微调主线不变（kev，§11.7）。largha 的价值改记为「若微调路径成立，桌面离线
   形态（288MB/CPU）的参照」，不构成采用理由。
4. 校准再证 §11.5 判决 #2：温度校准把 ECE 压到 0.02–0.04 也救不了 AUC≈0.5；
   任何校准/轮换（AnyJev 式）讨论以 AUC 达标为前提。

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

### 11.9 jev-gec 自建线 P0–P3c：蒸馏天花板定位（2026-11-06）

§11.8 后续动作落地：独立仓 `jev-gec`（../jev-gec）P0–P3c 完成，全程同一把
冻结尺（zh_gate_1k，sha256 manifest）。里程碑见该仓 RESULTS.md，此处记判决：

1. **泄漏审计**：原随机 dev 与 train 8-gram 近重复命中率 3.65% → 文档级图聚类
   切分（zh_v2 池）后 ≈0；v2/v3 的 dev 虚高（0.78/0.60 vs 冻结 ≈0.58）即其代价。
2. **微调臂全线 ≤0.60**：encoder 全量 0.580 / head-only 对照 0.578 / 文档切分
   池 0.573——dev−冻结缺口不是过拟合主战场，**池内容与传输方式才是**。
3. **response-based KD 判死（三教师对照）**：kev9b 软标签生产池→0.568；kev9b
   全池 10.9k→0.598；**Qwen3.8（网关，零样本冻结 AUC 0.823，全场最强）全池→0.564**。
   教师 0.765→0.823 学生纹丝不动：瓶颈在「logprob 标量→102M encoder」读出，
   与教师强弱、软标签数量均无关。
4. **下一枪 = 3090 Track-A**：Qwen3.5-0.8B LoRA + 字母读出（与教师协议同构），
   池=zh_v2；训练主体与服务分流已实现，验收仍只在 Mac 冻结集。
5. 顺带资产：`probe_teacher_auc.py`（教师零样本资格探针）、文档级去污染工具、
   无人值守验收链——均入 jev-gec 仓复用。

### 11.10 路由化转向：jev 模型从「判定」改「路由」（2026-11-15 在研方向定案）

**转向本身是降题，不是加难度**。判定要求句级特征与真值可分——这条已被本仓自己
证伪（`feature-model/server.py` 注释：「句级聚合特征无法可靠区分语病句 vs 干净句」，
CEC 工作流 D：三类概率分布重叠；门控实测 R≈0.45，CHANGELOG 11-13 抓到 level 3 两例
被「判定无错」放行）。路由不要求模型知道对错，只要求预测「**这条送 LLM 复查有没有
边际收益**」——目标从真值换成 LLM 行为，而 LLM 行为可以消融跑出来直接当标签。
预测 A 永远比预测真值容易。§13 #1 的验收原则「等漏检下的放行率」本来就是路由曲线：
HistGB 门控在 MuCGEC 63.1% 调用率那个点，就是路由器要打的基准。

**路由空间必须从现有管线里长出来**（每个动作都得是调度器今天就能执行的动作）：

| 路由 | 执行者（已存在） | 成本 |
|---|---|---|
| `rule_decide` | gec-zh 规则命中定案 | 零 |
| `pattern_decide` | L3 模式库机械修正（server.py 已有此分支） | 零 |
| `case_decide` | casebase 开卷命中（`media_core::casebase` 同构） | 零 |
| `llm_review` | LLM 复查（`engine.rs` ③ 兜底） | 主要成本 |
| `skip` | 仅预算模式，**如实记 skipped、不算覆盖率**（docjob 既有立场） | 负 |

`abstain` 三态在路由范式下退役：没有弃权，只有分布；调度器按 p(收益)/成本排序，
预算取前 k。架构侧对应改动 = `engine.rs` 的 ③-F `need_llm` 分支换成这条队列，
排队/预算机器复用 docjob triage（`gec-core/src/triage.rs`）。

**已定案两则**：
1. **路由句级定**（本条为粒度之争的终点）：句级出路由，块级只做排队序——否则一个
   块里一句脏带全块，economical 模式召回会很难看。更长上下文是否改变路由判断，
   后置为回归检查项，不阻塞本方向。
2. **预算先做请求内**（单请求按阈值选路）；跨请求全局预算（评测跑批上限）留给评测
   harness，full/economical 三臂已有这个形状。

**标签配方（不花人工）**：在 L3 池 stratified 子集跑 LLM 臂（credited 记账机器现成），
标签 = 「LLM 在此句检出且与 gold/采纳位置重合」。**必须取 gold ∪ LLM 并集，不能只取
LLM**——纯 LLM 边际标签把路由器的盲区继承给模型：LLM 抓不到的结构性错误会被路由成
「不必复查」，复查也本来就抓不到它，双盲锁死。缓解 = gold 侧标签 + 词典/统计信号
并集兜底（e65ae3c 的 lexical 信号 + sampling floor 正是干这个的；floor 之上路由不得
skip，与「覆盖率不得静默缩水」同一立场）。

**终局形态**：jev 模型即 GEC 修正模型本身——骨干共享，route = 首 token logprob，
decode 只对路由到 `llm_review` 的部分付出。一份模型、两个头、成本 ∝ 路由率，63% 调用率
变成 full/economical 预设上可滑动的旋钮，每格有验收曲线。届时 4806 sklearn 门控退役；
在此之前它继续在岗（11-13 定案：显式关闭与从未配置两态语义不变）。

**外部参照**：DetectorScope（DeepMind+UPenn，EMNLP 2026）同立场先例——预测必须是
具名贡献之和。其机制（白盒残差流挂 SAE）本仓不可迁移（LLM 为网关外部服务、门控为
表格模型），可借鉴的是**归因具名化**：路由的每个信号要可解释（triage.rs 的 `信号名`
已是这个形状）。

**验收口径**（harness 未建，建成时按此断言）：等调用率曲线上路由臂的 gold 召回逐点
≥ HistGB 门控臂与随机臂（四维口径，位置优先）；`skip` 率如实入覆盖率账；
level 3 全 LLM 臂仍不受路由影响（漏报兜底立场不变，11-13 教训）。

### 11.11 zh 错误分类法 v2（定案；2026-11-15 复审通过，numeral/quantity_unit/serial_no 通道本批建设）

**动因**：现 zh 内部类型仅 5 类（`shared-model/src/error.rs` `error_types`：
spelling/grammar/collocation/punctuation/anomaly），`llm.rs` `map_llm_error_type` 把 CEC
十分类坍缩成 5 类——成分残缺/赘余/关联词/指代不明/词性/语序全并进 grammar。竞品
校对通（蜜度/九录，2023 应用指南，已考察未入库）按约 40 子类 × 5 通道划分，且**用户
勾选校对项 = 用户侧通道预算开关**。类型粒度直接决定路由标签的配方和验收分层，故先
细化分类法再动 harness。参照系：校对通分类 + CEC（CEC 细分本表直接采用、不再坍缩）。

**设计原则**：①类型是路由的**标签维度与记账维度**，不是路由前置门——分类器前置会
与已证伪的判定模型同病级联（§11.10）；路由头仍句级单头。②每类必须落到一个确定通
道（rule/pattern/case/knowledge/LLM），落不到的标「通道缺」= 待建工作量。③确定性规
则直判类不进可学习路由目标（走 `rule_decide`，进账不进训练）。④事实核查是**独立通
道**：入口由规则引擎抽取时间/地点/事件/关键人物要点，通道内先上下文自查、上下文
不可判才检索知识源（详见 K 族后段）。

**T 族·文字标点差错（zh GEC 主线，入路由标签）**

| 类型码 | 中文 | 定义 | 检测层归属 | 备注 |
|---|---|---|---|---|
| spelling | 错别字 | 音近/形近字、繁简混用 | rule(`ZH_CONFUSABLE`)+LLM 长尾 | 现有 |
| extra_char | 多字 | 字词重复冗余（「去了去了」类） | rule 叠字 pattern+LLM | jev 靶区；**新**，从 grammar/spelling 拆出 |
| missing_char | 少字 | 虚词/结构词缺失 | LLM（n-gram 信号弱） | jev 靶区；**新** |
| collocation | 词语搭配 | 搭配不当 | rule 部分+LLM | 现有 |
| pos_error | 词性误用 | 词性错配 | LLM | CEC 细分不坍缩 |
| word_order | 语序错误 | 状语/定语错位 | LLM | CEC 细分不坍缩 |
| missing_component | 成分残缺 | 缺主语/宾语等 | rule(`ZH_MISSING_SUBJECT` 部分)+LLM | CEC 细分不坍缩 |
| redundant_component | 成分赘余 | 冗余成分 | LLM | CEC 细分不坍缩 |
| blend | 句式杂糅 | 两句式混用 | LLM | 校对通有、CEC 散在；**新** |
| conjunction | 关联词误用 | 关联词错配/缺失 | LLM | CEC 细分不坍缩 |
| reference | 指代不明 | 指称含混 | LLM | CEC 细分不坍缩 |
| logic | 语义逻辑不通 | 前后矛盾/推出失败 | LLM | 现为未入常量的 `"logic"`，**本表转正** |
| semantic_dup | 语义重复 | 同义表述叠加 | LLM | 校对通有、CEC 无；**新** |
| punctuation | 标点差错 | 中英混用/全半角 | rule(`punct_map`)+LLM | 现有 |
| numeral | 数字差错 | 数字用法规范（GB/T 15835） | rule(`ZH_NUMERAL`) 直判 | **本批已建**：万万并置/跨中文数字混合 |
| quantity_unit | 量和单位 | 单位/量值书写规范（GB 3101/GB/T 1.1） | rule(`ZH_QUANTITY_UNIT`) 直判 | **本批已建**：区间漏写单位/中外单位并注 |
| serial_no | 序号检查 | 序号跳号/重号 | rule(`ZH_SERIAL_NO`) 直判 | **本批已建**：同上下文 run 跳号/重号 |

**K 族·知识性差错（事实核查通道，不调 GEC LLM、不入 GEC 路由标签）**

| 类型码 | 中文 | 接入方式 |
|---|---|---|
| kn_person | 姓名职务/讲话引用/领导人排序 | Tolaria 笔记（MCP 只读检索）+LLM 带上下文回判 |
| kn_geo / kn_org / kn_term | 地理/机构/术语与法规名 | 同上 |
| kn_common | 常识差错 | 先上下文自查、不可判再检索知识源，低置信展示 |
| sensitive | 禁用慎用词/时政重点词 | 内置+用户自定义词表，`rule_decide` 直判 |

**事实核查独立通道（11-15 复审补充，未建）**：与 GEC 路由空间正交的三段式——
①**入口**：规则引擎抽取时间/地点/事件/关键人物提及，建立**高敏感事实核查要点**
（规则命中≠判错，仅表示该句进入核查流程）；②**先上下文自查**：一致性问题优先看
文档自身上下文（前后时间顺序、名称/数值一致），零成本解决文档内矛盾；③**上下文
不可判才检索知识源**（Tolaria 笔记，MCP 只读）+带上下文回判。kn_* 类型的产出标签
归此通道，不进 §11.10 路由空间。

**S 族·范围/通道类（不进 GEC 主线）**：dup_check 句子/大段查重（独立相似度通道，
**未建**，是否做另行决策）；anomaly（现有，入路由）；自定义词库七类（用户通道，
`rule_decide` 直判，对标校对通「正词/错词/重点词」，**未建**）；英文校对=既有 en 线；
内容导向风险识别六类**范围外**（产品定位决策，仅记录）。

**代码触点**：`error.rs` 补常量并约定 `category`=族名（text/knowledge/user）；
`llm.rs` 映射表按 CEC 细分重写（**已做**：细类型常量 + `coarse_error_type`
粗族回退函数；归因入口按 zh 门控回退、决策表不改，画像/会话分布查询期
按粗族聚合，DB 不迁移）；**本批已建**：lexicon 新增 `numeric`（数字·量值·序号）
分类与 `ZH_NUMERAL`/`ZH_QUANTITY_UNIT`/`ZH_SERIAL_NO` 三规则（`gec-zh/src/rules.rs`，
token/离线双路径；经 `apply_suppression_filters` 中央过滤，自动继承停用/白名单）；
docjob/mcp 契约加校对通式「校对项范围」参数；**旧 `grammar` 不迁移**——错题本
与统计查询期按族聚合展示，DB 不动。

**路由口径**：路由标签仅取 T 族中入路由行；harness 曲线按类型分层报告（错别字类应
验证「rule 命中即定、LLM 低收益」，杂糅/残缺类应验证「LLM 高收益」——分层曲线本身
即路由可学性证据）；K/S 族有自己的通道与账，不进此空间。§11.10 验收口径不变。

### 11.12 路由 harness 建成首跑（2026-11-15，分支 feat/router-harness）

**机件**（`feature-model/`，标签/特征/记账全部复用现有机器）：
- `router_labels.py`：在 jev zh_v2 池（格式对齐，追加 route_label/signals/
  error_types 字段）产句级路由标签，配方 = §11.10 并集（gold=1 且规则未定案→
  llm_review；规则定案档覆盖→rule_decide；LLM 检出/词典命中→llm_review；
  干净剩余每 3 句采样地板强制复查（对齐 docjob 地板）其余 skip，skipped
  如实入账不算覆盖率）。LLM 臂未直跑时标签 = gold∪lexical∪floor（实测
  train 9780：llm_review 67.5% / rule_decide 1.1% / skip 31.4%，lexical 559、
  floor 1488）。
- `eval_router_arms.py`：三臂等调用率曲线。histgb = 生产门控本地代理（HistGB
  在 zh_v2 池 gold 标签上训）；random = 固定种子；router = 同特征在 route_label
  上训（可学性代理臂，终局换 jev route 头）。覆盖 = 规则定案档命中 ∪ 被复查句
  检出（默认 **oracle 上限**，三臂同假设横向可比；--llm-cache 可接真实检出）。

**污染面（重要）**：gold 集（MuCGEC/NACGEC/CSCD）本就是 zh_v2 金标基座的训练源，
池内重叠无法避免（manifest 仅对 frozen 集断言零重叠）。两臂污染相同→曲线横向可比
但绝对值 optimistic；harness 默认**仅用池外行**出曲线（本次 6,168 句，gold 编辑
9,641），--include-inpool 可关。

**首跑实测**（mucgec+nacgec+cscd，oracle 口径，锚点 63.1%）：总体 router 65.2% ≥
histgb 64.7% ≥ random 64.0%，**总体 PASS**；但优势幅度薄（+0.5pp，且 80% 档回落后
于 histgb），分层 7/8 类 FAIL。归因：LLM 臂未跑，route_label 信息面≈gold 标签本身
（+lexical/floor），「LLM 边际收益」维度缺失——这正是要验证的假设而非缺陷。
另 MuCGEC 池外子集上 random>histgb（74.0% vs 46.9%），印证域外行上句级门控失真。
**下一步**：在池 stratified 子集直跑 LLM 臂（`router_labels.py --llm-run`，credited
记账）补边际收益维度后重赛；分层判定待按全网格逐点展开（当前仅锚点切片）。
复现：`cd feature-model && .venv/bin/python router_labels.py && .venv/bin/python eval_router_arms.py`。

**补：非 oracle 口径首测（同日夜，--llm-run-gold 直跑 + --only-cached 公平子集）**：
机件补齐：LLM 臂并发（--llm-workers）+ 断点续跑；**失败行不再假装已查实干净**；
gold 行直跑模式 `eval_router_arms.py --llm-run-gold`（池外行按 src 分层抽样，
检出落盘）+ `--only-cached` 只在已查实行上出曲线。实跑 400 抽样→已查实 398
（3 行 120s 超时剔除）；端到端实测吞吐 ~4.3s/行（上游串行，workers 只抵超时损耗）。

实测（已验证子集 398 句，gold 编辑 621；锚点 63.1%）：总体 **FAIL**
（router 42.5% / histgb 40.9% / **random 48.6%**），分层 FAIL 5/8；LLM 臂自身
检出召回按型严不均：spelling 93.5% / blend 83.1% / …… / **word_order 41.7%**；
clean 行检出非空率 56%（成本信号，span 对齐口径下偏保守）。
归因：gold 存在性代理标签不含「LLM 实际检得出」维度，router 头优先送检的恰是
word_order 重仓句，而 LLM 对该型只捾回四成→预算被低收益句占住；random 无此
系统性错配。这不是路由不可学，而是**标签信息面缺陷的定量实证**。
下一步：池 LLM 臂直跑（llm_hit 入 route_label）后重赛；另「按型条件化
路由」值得单试（word_order 降权或改走规则/专项侧）。本局仅换检出口径、
标签配方不变，oracle 结论不受影响。复现：
`eval_router_arms.py --llm-run-gold --gold-llm-limit 400 --llm-workers 4`（需 LLM_*）
→ `eval_router_arms.py --llm-cache /tmp/router_llm_arm_gold.jsonl --only-cached`。

**补二：Cycle 1 重赛（次日，llm_hit 入标签；池臂查实 744/1200=62%，
服务端 HTTP 500/502 阵发浪 + 超时剔除，重试退避已入库）**：
- oracle 口径：总体 **PASS**，锚点 router 65.4% / histgb 64.7% / random 64.0%，
  优势幅度较首跑拓宽（+0.7/+1.4pp vs 此前 +0.5/+1.0）；分层仍 7/8 FAIL（薄金标）。
- cache 口径（同 398 已查实行）：仍 FAIL（router 44.0% < random 48.6%）但结构
  变清：**≤20% 预算段 router 压倒 histgb/random（+5.5/+1.6pp）**，30% 以上段被
  random 反超——llm_hit 标签教会了头部「LLM 赢得在哪」，但中预算段仍被
  word_order 低召回（41.7%）句占住。分层 FAIL 5→4（collocation/logic 转正）。
- 处方明确→按型条件化路由：route 目标乘 P(LLM 检出 | 型) 收益矩阵（本局实测
  已有首批行），word_order 类降权或改走规则/专项侧；Cycle 2（生产日志
  收益矩阵自更新）是同一处方的数据化版本。
复现追加：`router_labels.py --split train --llm-out /tmp/router_llm_arm_train.jsonl`
→ 重跑两套 eval（cache 集需先 `--llm-run-gold` 制备，已落盘则直接复用）。

**补三：池臂跑满 1200/1200 后的标签饱和信号（同日深夜，退避重试零失败，
456 行≈ 57 分钟≈7.5s/行）**：llm_hit 全覆盖后信号面 **982/1200=82%** 行非空
（印证 clean 行 56% 误报率），池样本 llm_review 饱和到 90.8%：
- oracle 仍 PASS（锚点 65.3%，幅度未再拓宽）；cache 口径反而 44.0%→41.7%。
- 结论：**原始 llm_hit 不能直接入标签**（「LLM 会检出」≠「检出有收益」，
  路由头被引向「几乎全检」）。route_label v2 处方：llm_hit ∩ gold 背书
  （或按型乘收益权重）才入 llm_review；与按型条件化同源，Cycle 2 的
  采纳/拒绝日志就是这条「背书」的天然来源。

**补四：收益矩阵首批成表 + 标签配方迭代曲线（同日，--hit-types/--wo-exempt
入机件，均默认关不改旧行为）**：

按型收益矩阵（gold 验证子集 398 行，样本小当方向读；n=检出数）：

| 型 | LLM 检出召回 | 检出精确率（与 gold 重合） | 处方 |
|---|---|---|---|
| spelling | 93.5% | 81.5% (54) | 入白名单 |
| redundant_component | 81.0% | 90.2% (61) | 入白名单 |
| logic | 71.9% | 84.1% (44) | 入白名单 |
| word_order | **41.7%** | 80.6% (31) | 高精确低召回→规则/专项侧（--wo-exempt），降权不入 LLM 预算 |
| collocation | 72.4% | 60.6% (99) | 入白名单 |
| missing_component | 67.1% | 53.9% (76) | 入白名单（边缘） |
| punctuation(+标点错误) | — | 14.0%/27.3% (61) | 背书滤除 |

标签配方迭代（cache 口径锚点 router / oracle 锚点）：v1 原始 llm_hit 62% 覆盖
44.0/65.4 → 全覆盖 41.7/65.3（饱和）→ +背书白名单 44.1/65.1 → +wo-exempt
**45.7/65.6**；对 random 差距 −6.9→−4.5→−2.9pp，oracle 同步创新高。
残余 gap 归因：word_order 的 58% 漏检在池侧不可见（gold 行无型标签，
漏检行落回 gold-first 入 llm_review），12 行降级只触及可见部分——剩下的
gap 不在标签机件内，要么 gold 侧型标签接入，要么等 Cycle 2 采纳日志/jev 头
自带型信息；另 398 行子集单种子，噪声未消。结论不变：**按型条件化方向已
被数据连续验证**，复现：`router_labels.py --split train --llm-out …
--hit-types <表内白名单> --wo-exempt` → 两套 eval。

**补五：gold 侧型标签接入——gap 从此可见，「全量 wo 降级」判死
（2026-11-16，--gold-types 机件）**：

`tools/true-gold/gold_types.py`：text→v2 型码，两通道并集——datasets 行级
error_type（ast 抽取 eval 侧映射，同源不复制）∪ true_gold 编辑级六类经 v22
行 join 回文本（typo→spelling、grammar→grammar、punct→punctuation、
cohesion→logic、consistency→reference；**fact 独立通道不映射**、
gold-unknown 跳过）。产出 14,167 句有型；池 train label=1 4,870 句文本 join
命中 4,400（90.3%），其中**纯 word_order 110 句** = 补四不可见部分的本体
（其 --wo-exempt 只触及 12 句）。

`router_labels.py --gold-types`：gold 型并入 `error_types`（与 LLM 型并集，
joined 行 4,400/4,400 superset 验证）；--wo-exempt 判定改为「llm 型 ∪ gold 型
纯 word_order」→ 降级 12→106 行（104 行 gold 侧新可见；原 12 行中 10 行因
gold 侧还有别型不再降——并集语义下型证据更诚实）。复现纠偏：补四实际
白名单从 982 行缓存唯一反解 = 含 word_order/reference、不含标点/anomaly
（照笔记清单跑对不上账；对表后标签逐行一致 llm 785/floor 1396）。

迭代曲线（cache/oracle 锚点）：45.7/65.6 → **44.6/64.1**，两口径
−1.1~−1.5pp、总体 FAIL；word_order 层 26.9→25.0（降级连本层都没赚）、
collocation 53.9→60.5、对 random 差距 −2.9→−4.0pp。归因：WO 每句期望仍为正
（召回 41.7% × 精确 80.6%），63.1% 预算处边际收益≈ 1:1，腾预算抵不过跳过
可检句的代价——**标签侧静态降级近乎纯亏**（入 §13 #12）。gold 侧型标签
真正赚在坐标系而非降级：4,400 池行直接带型（jev 头型头的蒸馏目标/收益
矩阵输入），降级处方的正确出口是 Cycle 2 收益矩阵按型条件化与型头。
复现：`gold_types.py` → `router_labels.py --split train --llm-out …
--hit-types spelling,missing_component,redundant_component,collocation,blend,
conjunction,logic,word_order,reference --wo-exempt --gold-types …` → 两套 eval。

**补六：Cycle 2 消费端就位——生产日志→按型收益矩阵（tools/cycle2-matrix）**：

排查发现写侧三本账**早已在产**（不用改一行 Rust）：detect_history.errors_json
（检出+来源层）、layers_json（各层 ran/skipped——路由决策账；旧行无此列）、
feedback_log（adopt/reject 全层留痕，含 LLM 兜底）。缺的只是消费端：
`matrix.py` 把三本账 join 成按型矩阵（precision=adopt/(adopt+reject)；
yield=每千句复查换回的真修正；feedback 无外键，按 original+14 天窗回指，
命中/落空如实两列并报）；达门槛型出 `--hit-types` 串回喂路由（低 precision
型**不降级**，走预算权重/型头——§13 #12 约束内化进报告文案）。
错题本导出 = 被拒 LLM 检出含原句（负例蒸馏原料）。e2e（合成库）：复查句计数
（ran/backfill/skip 三口径）、层账回指命中/落空、rule 反馈不误入、空库不炸
全验。dev 实例实跑：llm 侧反馈 0 条、矩阵全薄——**机件等账，账等生产使用**；
周期表 Cycle 2 行同步改「机件就位」。

**补七：gold 重建后重跑，anchors 不动（2026-11-16，notes §S）**。五桶各抽
58 条均未过 2% 门禁（tools/adjudicate-audit README：auto_real 3.4 /
auto_noise 25.9 / auto 6.9 / must 17.2 / audit 20.7），不整桶采信；adopt
131 进 gold，v2.2 插入残片复读由 merge 新增 dup_artifact 前置拦 1,060
（人工 adopt 覆盖 2 = fix 尾字 off-by-one 真错；单字动词 AA 重叠豁免）。
重跑 gold_types→labels→eval：两锚点 **44.6/64.1 与补五逐位相同**，分层表
同构。归因：gold 重建改的是资产与坐标系，决策面输入（llm 缓存 × 背书
白名单 × 降级集）未变→anchors 不动，#12 再确认；要动 anchors 得动决策面
本身（jev 型头/收益矩阵）。运维坑：eval 的 --llm-cache 必须指
router_llm_arm_gold，错配 train 缓存会把评测集过滤成空集除零崩。

### 11.14 分型分层架构小结与自学习周期（2026-11-16，在研方向定调）

**架构一句话**：横向分型（§11.11 的 17 类 T 族细分）× 纵向分层（规则定案档 →
句级路由预算档 → level-3 全 LLM 兜底档），K/S 族独立通道不进路由空间。

**分型的价值不在展示，在收益矩阵**：§11.12 补非 oracle 首测实证的不是「路由
输了」，而是路由目标应为**期望净收益 = P(有错) × P(LLM 检得出·改得对 | 型)
− 成本**，而非常规「这句有没有错」。LLM 检出召回按型严重不均（spelling
93.5% vs word_order 41.7%）= 收益矩阵的首次真实测量；分型分层就是这个矩阵的
坐标系。router 臂 FAIL 输在标签信息面，不是架构输了。

**纵向分层分工（定案口径不变）**：规则定案档 = 零成本覆盖（高确定性型）；
路由预算档 = level-2 预算模式按期望净收益分配（按型条件化）；兜底档 =
level-3 全 LLM，立场不变不砍（§13）；LLM 低召回型（word_order 类）属规则/
专项侧或按型降权，是否开试待 Cycle 1 重赛。

**自学习周期路线**（每圈以 harness 三臂等调用率曲线+分层判定为质量闸：
PASS 入档，FAIL 归因入 §13，账目不靠手感）：

| 周期 | 回路形态 | 状态 |
|---|---|---|
| Cycle 0 | 规则+词典+gold 代理标签→三臂 harness 入库（oracle 口径） | 已就位 |
| Cycle 1 | LLM 臂直跑→llm_hit 入 route_label→重赛（路由器学预测「复查这句有收益」） | 已重赛（oracle 幅度拓宽；cache 口径处方=按型条件化，§11.12 补二） |
| Cycle 2 | 生产日志（路由决策+LLM 检出+用户采纳/拒绝）→错题本/偏好沉淀→按型收益矩阵直接更新，低收益型由数据发现修正（word_order 偏置即实证） | 机件就位（写侧已在产；消费端 tools/cycle2-matrix，§11.12 补六；dev 库 llm 侧零反馈，账等生产喂） |
| Cycle 3 | jev route 头（首 token logprob）替换代理头，自学习标签即蒸馏目标，路由内进生成模型一遍过 | 终局形态 |

**基础设施教训（ Cycle 1 实跑）**：自学习周期的瓶颈是**标签制备吞吐与可靠性**：
端到端 ~4.3s/行（上游串行，workers 只抵超时损耗），夜间服务端 HTTP 500 437 次
+超时 162 次→单晚失败率 50%；断点续跑+失败不假装干净是最低保障，后续周期
必须保持（标签可廉价重生＝周期可重试）。得失是一时的，坐标系（分型）
与闭环（harness+标签机件）是沉下来的资产。

**补：分型分层三修正（同日，接用户「一句多错/句后聚合」两约束）**：
① **型是编辑的属性，路由的宿主是句子**，二者靠型多重集连接：预算单位=句
（一次调用同查多编辑），路由键=句型多重集，EV(句)=Σ n̂_t×EV(t) 而非单型映
射。jev 型头由此定为**多标签头**（每型二值），softmax 单型会把别字+语序压
成一类、丢路由信息；gold_types text→[型…] 与 labels 侧并集语义已按此就位，
缺的只是打分公式从白名单命中换期望求和。
② **粒度三层与分层正交**：编辑层（字符级规则：confusables/标点/dup 质检，
零成本）→句子层（路由+LLM）→语篇回归层；规则定案档主要落在编辑层。
③ **语篇回归层是「长上下文后面再查」的正解，账目独立**：句后聚合实体/
时间/数字/人称轨迹，consistency·cohesion·事实一致在此层；流程按事实通道
定案——先按上下文检查（规则档+ZEN 要点表），上下文自身判不了才检索知识
源（K/S 不进路由空间不变）。其触发精确率单独过 ≤2% 闸门，不入句子级三臂
曲线；L3 兜底是句子级的，兜不住跨句错，二者互补不互替。cycle2-matrix 按编
辑记账天然兼容一句多错，只需保证日志行带句内编辑序不缺位。

**补二：空白三量化与方案定案（同日，「先研究清楚再定方案」，三实验先于形态
之争）**：

- **实验1（n̂_t 形态，空白三）**：true_gold 10,646 句，≥2 错句 13.4%，其中同族
  多错 **61.3%**（gold_types text 联接的 10 型口径；粗型口径直接按 true_gold
  型分组为 81.6%）。presence 形式 n̂_t∈{0,1} 会对约 8% 语料系统性低估
  EV(句)=Σ n̂_t·EV(t)。**定案：方案 A′+C——每型期望计数头（HistGB 回归，目标
  =true_gold 句内该型编辑数，自 gold_types/true_gold 蒸馏）+ 稀型先验兜底**；
  jev 多标签头相应为计数式多标签，非二值非 softmax。（口径注：E>T 联接含少量
  未吸收行噪声，真实率略低，但离 10% 阈值一个量级，不影响定案。）
- **实验2（must 桶规则可达性，空白二）**：consistency 243 条/cohesion 509 条，
  谓词在编辑 span 内可见各仅 17.7%；**±6 窗口下升至 52.3%/42.4%**（命中族：代
  词 76、数字 42、关联词 25、时间 2，有交叉）。**定案：ZEN 轨迹表谓词一律写成
  ±6 窗口谓词**；约一半编辑规则侧不可达，留 LLM/语篇回归层，轨迹表只吃可达半
  （与静态规则回收 mark 桶 0.6% 的旧账共同支持「轨迹表=轻量账本+判定表」形态）。
- **实验3（规则臂下界，空白一）**：Python 离线复刻同一本
  `lexicons/zh_gec_lexicon.json`（confusables 232 条含 2 条条件条、punct_map 8
  对、anomaly patterns 7 条；**POS 路径规则未复刻=下界**）：对原始金集字符 recall
  89/33,357=**0.3%**，字符 precision（∩true_gold）928/8,345=11.1%。分通道：
  conf 87/758=11.5% TP 率；**punct 2/7,587≈0%——语料毒**（cscd/cscdns/nacgec
  原文通篇 ASCII 半角标点，规则在评测语料上是 FP 发生器；生产文本若全角为主则
  不发作）；anomaly 零命中（服务话术不入语料）。**定案：规则臂=前置扣除层+高敏
  感事实核查挂钩，不是召回主力**；EV(t) 以实测小值入 cycle2-matrix 收益矩阵
  （ZEN 接线 #1 保留、预期调低）；punct 规则只进提示档且加**文档标点风格门**
  （全角占优文档才启用），定案档=高确定 confusables+anomaly 内 strong 条。

**架构方案定案（三实验合流，落法）**：① 型头 A′+C 就位后 harness 打分公式从
白名单命中切 EV(句)=Σ n̂_t·EV(t) 重赛；② 规则臂以「前置扣除」入收益矩阵：
规则已定案编辑先扣、预算曲线在残差上算，打扰账定案档/提示档分列；③ ZEN 承
载轨迹表与 ±6 窗口谓词，语篇回归层按双重门（语篇提示+上下文判不了才检索），
打扰率单过 ≤2% 闸门不入三臂曲线。

**补三：方案 A′ 落地首跑——router_ev 臂非 oracle 首次 PASS（同日）**：

- 机件：gold_types.py 加 --counts-out（同一次扫描数出型多重集
  {text,counts,spans}，14,235 句，不另起第二本账；编辑级 10,590 句优先，行级
  只补 3,645）；eval_router_arms.py 加第四臂 router_ev：每型 Poisson 计数头
  （HistGBRegressor，正例 <head-min-pos=100 退先验常数；blend 76/reference 79/
  redundant 38 三型退先验）× p̂(t)（train 侧 spans×train LLM 缓存重合；
  <50 span 退全体均值 0.880）；打分 EV(句)=Σ p̂(t)·n̂_t。旧 router 臂保留同表
  A/B；不带 --counts 时三臂曲线与补七锚点逐位一致（重构零行为变更）。
- **缓存口径锚点：总体 PASS**——router_ev 54.6% / router 44.6% / histgb 40.9% /
  random 48.6%（gold 621，已验证子集；分层 FAIL 3：blend/redundant/spelling
  小 cells）= 非 oracle 口径路由臂首次 PASS，且低预算段优势最大
  （20–50% 调用率 +8~+21% vs histgb）。对比 oracle 口径：router_ev 65.2%
  仍 PASS 但仅 +0.5 vs histgb（分层 FAIL 5）——oracle 下排序价值收敛于
  P(有错)，histgb 本就在排它；EV 臂的真增量在「LLM 改得对」维度，只有缓存
  口径看得见。两口径均坐实：**路由目标该是期望净收益而非命中金句**。
- **p̂(t) 测量面饥饿（待补）**：train LLM 缓存仅 1,200 行，实测量只有
  grammar 0.961 / spelling 0.886，余型全走 0.880 先验——即本次胜利主要由计数头
  Σ n̂_t（多重集信息，= 实验1 预告的 61.3% 多错信息）贡献，p̂ 分化尚未上场；
  word_order 降权（wo_exempt 的 EV 化替代表达）需 p̂(word_order) 真测量才生效。
  下一步：扩 train 侧 LLM 缓存（端点夜跑）后重赛，p̂ 增益与计数头增益分开入账。
- 产物：~/tmp/true_gold/{gold_counts.jsonl, eval_router_ev_cache.txt,
  eval_router_ev_oracle.txt}。

**补四：规则臂前置扣除入 cycle2-matrix（同日）**：

- 机件三件套：① `rule_offline.py` 单一台账复刻（experiments 补二锚点与探针
  共用；punct_mode legacy|gated，gated=文档标点风格门全角占比≥0.5）；
  ② `rule_probe.py` 离线 EV → `~/tmp/true_gold/rule_probe.json`；
  ③ matrix.py 分臂（矩阵键 arm/type；whitelist/yield 只出自 llm 臂，规则臂
  入矩阵不出白名单不算 yield，打扰账由离线 EV 嵌入 `rule_arm_offline`）；
  ④ eval `--rule-subtract`（规则定案档 safe 非 advisory 命中的编辑从曲线扣除
  只评残差；提示档打扰账入报告头）。不带 flag 时三臂与补三锚点逐位一致。
- **风格门纠偏（补二预期写反了一处）**：实测 22,456 docs 中 91% 过门（门开），
  legacy punct 7,587 命中中 84% 来自被门挡掉的 9% 半角风格文档——语料毒在
  门外不在门内；门后 punct 通道精度 2/7,587（0.03%）→ ~9.5%。
- 离线 EV（dataset_v22 全量）：recall vs 原始金 88/33,357=**0.3%**；
  precision vs true_gold 207/1,977=**10.5%**；定案档打扰 6.4 字符/万字，
  提示档 10.3；TP 分型 typo 122/grammar 52/punct 27/cohesion 4/consistency 2。
- matrix 分臂实跑（dev 库）：规则臂检出 grammar 926/punctuation 533/
  spelling 64/anomaly 8/word_order 5，反馈 6 条（anomaly adopt×2、
  grammar/punct reject×6）——旧版 `source='llm'` 过滤把规则臂反馈全部隐藏，
  分臂后首次入账。
- **残差曲线结论（已验证子集 398 句，gold 621）**：规则定案扣除仅 **1/621
  编辑（0.2%）**≈探针下界；提示档打扰 13.5 字符/万字、∩金标 1/29=3.4%；
  残差锚点 router_ev 54.5% PASS（=全量−0.1，分层 FAIL 3 不变）。
  → 规则臂前置扣除的**预算节省≈ 0**：它挣的不是预算而是零成本确定性（免
  端点依赖、免打扰用户）+高敏感事实核查挂钩；真正吃 LLM 预算的是 advisory
  灰区（转弃权）——打扰账自此在报告头常驻。价值主张从「省预算」改校为
  「免费定案+挂钩」，路由预算分配不受它影响。
- 回归：不带 flag 三臂逐位=补三锚点；p̂ 测量面随夜跑分批合并 611→791 行，
  锚点未动（胜利仍由计数头主导，p̂ 分化增益待夜跑收齐后重赛归因）。
- 产物：`tools/type-layer-experiments/{rule_offline.py,rule_probe.py}`、
  `~/tmp/true_gold/{rule_probe.json, eval_router_ev_cache_rulesub.txt}`。

**补五：归因分账与 ZEN 轨迹表首版（同日）**：

- **归因（--ev-uniform-p）**：p̂(t) 全设全体均值常数重赛，锚点与真 p̂ 逐位同
  （均 54.6%）→ 补三胜利 **100% 由计数头贡献，p̂ 分化增益当前为 0**（测量面
  饥饿：grammar/spelling 两型真 p̂ 与先验差不到 0.08）；夜跑收齐后重赛修正。
- **ZEN 轨迹表首版（tools/zen-trajectory/）**：table.json 声明行（四族→
  ±6 窗口谓词→通道）+ trajectory.py 只做在场命中账，落逐条轨迹账
  `{id,src,span,fires,span_visible}`；must 桶复算与补二实验2 逐位一致
  （consistency 243 可达 52.3%，cohesion 509 可达 42.4%，span 内均 17.7%）。
  首版=路由信号非冲突判定；v2 谓词（指代矛盾/关联词断对/年份时序）在
  rows[].next 挂账；打扰率 ≤2% 闸门待生产接线后实测。

**补六：ZEN v2 冲突谓词首跑——不获得独立路由通道（同日）**：

- table.json v2 落四冲突谓词（straddle_pair/num_same_unit_diff/year_pair/
  conj_pair_break；conj 取整句 scope=对 ±6 定案的显式偏离，关联词对跨分句常态）；
  trajectory.py 双锚定模式（error-anchored=must桶/金标位；standalone=干净语料
  每 token 打扰账），present 口径逐位不变（52.3%/42.4%/17.7% 复现）。
- 三段校验（must 752 / gold CC 714 / clean 21,129 行 110.9 万字）：
  must 召回 pron 0.3%、num 0.0%、conj 3.1%、year 0.4%；gold 锚定 vs clean 基线：
  pron 0.1%<1.87%（反信号）、conj 4.3%<7.57%（反信号）、num 0%<0.35%、
  **year 1.8%>0.18%（唯一 10× lift，但召回仅 0.4%）**；打扰任一谓词命中行
  占比全量对 9.64%、强配对子集（剔因为…所以/如果…就弱义务对）4.75%，均超 ≤2%。
- **定案**：中文邻近异指代词/缺配对关联词是常态噪音，冲突谓词无 lift 不养通道；
  ZEN 价值保持 present 口径=给 LLM 裁决喂 ±6 上下文证据（conflicts 字段随行进
  上下文不作判定）；year_pair 留作事实核查钩子的候选（与规则臂高敏感要点同族，
  §11.14 补四出口）。收紧过程如实入账：conj 弱/强拆是对打扰账的第一轮响应，
  拆后仍不过闸即判死，不再调参找绿灯。

**补七：夜跑收工——p̂ 测量面 96.2%，归因终版定账（2026-11-17，工具链
`tools/tonight-queue/{run_tonight.sh,run_recovery.sh}`）**：

- **测量面补齐**：train LLM 缓存扩容三轮夜跑/回收（本机 vLLM Qwen3.8 tp4）：
  第一轮 +2,159 行（429 限流失败 2,248）、第二轮回收 +1,033 行（超时失败 193）、
  第三轮回收 +191 行（仅剩 2 行顽固超时 gold-train-9835 / prod-1ba9f1b2b062，
  其文本已被重复行盖到，eval 侧测量面实为 **5,070/5,070 满**）——
  611→**5,068/5,070（99.96%）**，缓存 5,657 行。断点修复（.expand 落盘集=
  测量面全量自包含集）经两次中断实踩验证：第二轮 seed 3,844 完整续跑，
  零丢失（对照 11-16 旧逻辑 kill 丢 1,644 行）。
- **归因修正（补五欠账清偿）**：eval 侧 p̂ 测量面满（5,070 行）后重赛两遍——真 p̂
  锚点 router_ev **55.2% PASS**，uniform-p 对照 **54.6%** → **p̂ 分化增益 =
  +0.6pp**（补五「增益当前为 0」修正为「+0.6pp」；96.2% 与 99.96% 两次重赛
  锚点逐位同，数字对尾部缺口不敏感）；计数头增益 +10.0pp
  （54.6−44.6）仍为主体（router_ev 总增量 +10.6 中约 94% 是计数头）。
  锚点 histgb 40.9 / random 48.6 / router 44.6 逐位未动；分层 FAIL 仍 3
  （blend/redundant/spelling 小 cells）。
- **p̂(t) 终版分型表（attribute_p 分侧，全体先验 0.916）**：grammar 0.955 /
  spelling 0.934 / reference 0.932（先验转正）/ punct 0.908 / logic 0.860 /
  missing 0.770 / blend 0.733 / collocation 0.689 / redundant 0.543（⚠先验，
  span 35 不足）/ **word_order 0.541（门盲侧 0.000）**。→ **wo_exempt 退役
  判定坐实**：p̂(word_order) 显著低于先验，EV 已自动降权，标签侧开关无必要
  （与 §13#12 同向）。p̂ 对总分的直接贡献虽小（+0.6pp），其价值兑现点在
  **分型侧定权**而非总体曲线。
- **v2.3 rollout 收账**：共识 9,202 · 新增 6,733（队列待抽检）· 漏标 22,313 ·
  自动判无需处理 4,718；类型分布 grammar 36,780 / typo 26,547 / cohesion 5,301 /
  punct 5,033 / consistency 1,720 / fact 405；产物 `~/tmp/dataset_v23/`（9 集
  + `review_queue_new_errors.jsonl`）。
- 产物：`~/tmp/true_gold/{eval_router_ev_cache_v3.txt, eval_router_ev_uniformp_v3.txt,
  p_attribution_v3.txt}`；复现 `feature-model/eval_router_arms.py`（docstring 尾部
  命令）+ `tools/type-layer-experiments/attribute_p.py`。

### 11.15 模型定位终版：一决策、一改错（2026-11-18 定案）

由用户提问「jev 类决策模型是否应重定位为直接判错/改错的兜底模型（等价于把
Qwen3.8-27b 微调成专门找错改错模型）」触发，本轮以外部文献交叉核对后定版：
**两个角色分开，一决策、一改错**。

- **决策模型（jev/kev 线）= 闸，不是手**。外部定义一致：决策模型
  "return a probability for each predefined answer option rather than generate
  text"（arXiv 2610.34626）、"lightweight non-generative classifiers that
  return typed, calibrated verdicts"（arXiv 2609.28940，JEV/Laya System One）；
  文献最佳实践即级联——决策模型先判、低置信路由给生成式 LLM，F1 反升
  1.9–4.0pp（同上），"routing low-confidence items to an LLM matches or
  exceeds the LLM alone at a quarter to half of its cost"（arXiv 2610.18610）
  ——与 §11.10 路由化转向及 router_ev 非 oracle PASS（补三/补七）同构。
  职责 = 谁零成本定案、谁值得花 LLM 预算（EV 路由 + 多标签计数头，§11.14）；
  决策模型不持触发权——level-3 全文 LLM 兜底无条件（§13 / 11-13 教训不变；
  arXiv 2610.29961 的 abstain 实验同证：此类模型适合管**采纳策略**，不适合
  终结深度扫描）。
- **改错模型（Qwen3.8-27b 微调线）= 手，新线**。找错+改错模型会生成文本，
  按定义是**生成式 GEC 模型**，占兜底 / llm_review 臂的**执行者**位——这不是
  决策模型的重定位，而是一条新线。依据：CEC_ADOPTION §5（generic 提示在纠正
  侧 F0.5≈0.005 vs cec3 0.805）说明兜底臂需要专用模型；验收口径 = 测评中心
  对标 cec3 0.805。新线两个方向都不继承：§11 决策模型线的退役/待发布判决
  不适用它，kev 微调线的背书也不转给它。
- **命名纪律**：勿把微调 Qwen 叫「决策模型」（混名会把 §13 #10/#11 的否定
  结论错误套到它头上）；勿指望 jev 头做修正（正是 §11.10 转向避免的事）。

定案一句话：**决策模型管谁值得花钱，改错模型管怎么改；一闸一手，不互换。**

### 11.16 Qwen3.8-27b 改错模型微调：数据配方与评测计划（2026-11-18 起草，待执行）

**定位**（§11.15 改错模型线）：生成式专用 GEC 模型，占兜底 / llm_review 臂
执行者位。主验收门 = 测评中心 mucgec dev `force_llm` 严格 **F0.5 ≥ 0.805**
（cec3 基线，报告 `20261016T155142Z_mucgec_cec3_capability`）；generic 臂
（zh_detect JSON 契约）保留为对照，其句级定位价值不丢。

**数据配方（四源分桶，provenance 全程随行）**：

| 源 | 入训 | 不入训 | 依据 |
|---|---|---|---|
| 人标 gold 系 | mucgec / nacgec / cscd / yacsc / midu gold（人工修正 1,010 句）的句级 src→dst 平行对 | — | 唯一可审计的「人怎么改」监督信号 |
| dataset_v23 | 共识 9,202 + 新增抽检通过项（每桶 ≥58 纪律，`review_queue_new_errors.jsonl` 6,733 先行） | **模型漏标 22,313 与单源 model_errors 桶**、解析失败 131 行 | v23 标注器 = Qwen3.8 本身（README 规则①）：漏标桶入训 = 自蒸馏复现自家盲区；污染隔离同时约束训练侧 |
| 产品飞轮 | 资产 C `adopted.jsonl` 句对（`prepare_adopted.py` 治理：去 weak、基准块表，上限 2 万） | 资产 B `rejected.jsonl`（DPO 料，SFT 不入） | 真实用户确认分布，生产最近；§9 |
| 领域增强 | `augment_domain.py` 14 种在用户域干净文本（小说稿/行业文档）上定向增强，固定种子 | 注入式合成源：midu2022 初赛、CTC2021 train_large_v2 | §13 #2/#6：注入分布（S/D/I=45/32/23、OCR 形近）与真实流量非同源，会教错误修正习惯；增强后注入比例先过 `gold_error_dist.py` 对照（真实同音 ~99% 口径） |

混合比上限：人标 gold + 飞轮为主干；v23 共识桶 ≤40%、增强 ≤30%。训练句对与各
评测基准 dev gold 做 md5 句级去重（测评中心 fresh 口径），防泄漏。

**输出形态**：plain「纠正后句子」（cec3 同口径，字级 diff 由适配层承担，
`score_f05.py` 打分与基线表直接可比）；zh_detect JSON 契约作对照臂非主训目标。

**两阶段**：先 SFT（LoRA，对齐 kev 线现有 LoRA 工具链）；偏好对齐二期再定——
料已就位：`rejected.jsonl` + 被拒 LLM 导出（§11.12 负蒸馏料）作 DPO 对，GRPO
按 CEC_ADOPTION D4 Efficiency-Aware Reward（F0.5 式得分 − λ·不必要编辑数）。
ce4 教训前置：对齐阶段在 SFT 过 F0.5 门后才上，不反序。**算力已定：外租**——
句级短序列（≤512）把激活钳死，27B bf16 LoRA 训练 = 8×H100 或 4×A100-80G
单次任务 1–2 天（~10 万对×3 ep ≈ 50M token），低频按轮次租；不碰 3090
Track-A（小模型蒸馏专用）与 vLLM 服务机 tp4——checkpoint 不顶替 4000 端点
的 qwen3.8-27b 服务位（dataset_v23 README 规则①：评测基准臂须是原版；
评测时 base/finetuned 双模型名同挂 A/B）。真正的持续成本在回归评测端点：
force_llm 全集单轮已 19h+（61.5s/句），评测复用 tp4 夜间窗口按夜跑纪律办
（断点续跑是底线，§11.14 实踩）或届时加一份 tp4 级端点。

**评测门（按序，均在测评中心）**：

1. **契约门**：dev 1,137 句输出 100% 解析，适配层字级 diff 正常；
2. **主门**：mucgec dev F0.5 ≥ 0.805，误报编辑量级不劣于 cec3（=90），P/R 并报；
3. **泛化门**：capability 全集（cscd·sig15·med·law / nacgec / jfleg / midu gold）
   无单基准崩塌（11-12 量具修复口径）；**v23 不得作 Qwen3.8 系评测集**
   （README 规则①），生产真实文本另 fresh 抽样；
4. **接线**：`prompts/models.json` 注册新臂（plain 适配复用），设置页可切；
   仅作 llm_review/兜底执行者替换——只换执行体，§11.15 触发权与兜底立场不变。

**失败归因预案**：F0.5 不过但句级定位强 → 该臂退回 generic 定位（定位有价值、
纠正弱），配方侧重做；误报集中某型 → 先查标签桶（漏标桶入训是头号嫌疑，
provenance 字段就是为这一步留的）。

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
| 10 | vegaml 决策模型回到产品路径 | **退役**（保留实验代码；产品面入口已于 2026-11-02 移除） | 选型未落地、95% 正类 + 评测池自训练、被 L3 严格覆盖、生产 11555 次检测贡献 0 | §11.4 |
| 11 | 零样本换决策模型救活门控（laya/chdec/largha/AnyJev 等生态臂） | **否**（路径关闭；微调主线 kev 不变） | 四个家族零样本 zh AUC 均 0.50±0.02（kev 唯一 0.75+）；校准救不了判别；同底座（Qwen3.5）不保证信号——症结在决策训练分布无语言质量信号 | §11.5、§11.8；**后续**：自建 `jev-gec` P0–P3c 已完成——response-based KD 判死（三教师对照 ≤0.60，见 §11.9），转 3090 Track-A LoRA |
| 12 | word_order 在标签侧全量降级 rule_decide（--wo-exempt × gold 侧型标签，补四处方的完整形态） | **否** | 降级 12→106 行后 45.7/65.6 → 44.6/64.1（cache/oracle 锚点，均 FAIL）；word_order 分层自身 26.9→25.0；WO 每句期望为正（41.7%×80.6%），63.1% 预算处腾预算≈边际 1:1 抵不过跳过可检句 | §11.12 补五；降级收益的正确出口 = Cycle 2 收益矩阵按型条件化/型头，非标签开关 |
| 13 | ZEN v2 冲突谓词（straddle_pair/num_same_unit_diff/year_pair/conj_pair_break）作独立路由/定案通道 | **否**（通道不养；present 证据口径保留） | must 召回 0–3.1%；gold 锚定 vs clean 基线无 lift（pron 0.1%<1.87%、conj 4.3%<7.57% 反信号；仅 year_pair 1.8%>0.18% 但召回 0.4%）；打扰任一命中行 9.64%（强配对 4.75%）超 ≤2% 闸门 | §11.14 补六；复现 `python3 tools/zen-trajectory/trajectory.py` |
| 14 | jev 类决策模型重定位为「直接判错/改错的兜底模型」（含「微调 Qwen 找错改错模型 = 决策模型」的叫法） | **否**（角色拆分：微调 Qwen = 生成式 GEC，占兜底/llm_review 臂执行者位；决策模型维持路由闸，不持兜底触发权） | 外部定义锁死决策模型 = 非生成、题型问题→校准概率；兜底触发权已定 level-3 全 LLM（11-13 教训）；微调 Qwen 属兜底执行位新线（generic 提示 F0.5≈0.005 vs cec3 0.805） | §11.15；外部核对 arXiv 2610.34626、2609.28940、2610.29961、2610.18610；CEC_ADOPTION §5 |
| 15 | 生成式订正差分直连作漏字通道（本地 shibing624/chinese-text-correction-1.5b；零样本网关差分臂；7B/4B 本地臂；灰区 ③ 协议 A/B） | **否**（直连关闭；本地大臂不续；灰区协议改造 no-go） | 两臂 FP 不过关：1.5B 新闻净句 insert 型 FP 8.8%/句 + 29% replace-only 改写（公文域 3.0%/0.25%）；零样本 Qwen3.8 公文 8.5%/新闻 11.5% insert 型；D 侧 1.5B 边界 25/120=20.8% 与 p_err 同水位 | `tools/label-rollout/README.md`「漏字盲区外部方案调研」；复现 `eval_ctc.py`/`probe_gendiff.py`；**后续已跑**：灰区 ③ 协议 A/B（funnel llm_ran 380 句）no-go——差分臂 I-content 12.9% vs 判定臂 53-55%，gold 级增量仅 +3.9pp I，synthD 协议红利坐实为合成伪影；瓶颈回到 ② 定位与 insert 监督，两臂都漏的 401 个 I 位是后续主攻面（`eval_diff_protocol.py`；报告 `datasets/eval_reports/20261120T101244Z_mucgec_gray380_diff-protocol.md`） |
| 16 | 句级序列标注 insert 打标补漏字盲区（② 门控重训获 insert 能力） | **挂起**（无现成破法） | TW-NLP 官方 difflib 打标自认纯 insert 行 source 侧标签可全 0（业界打标同盲区）；池内 I 型监督≈0.3%（cscdns，§12）；合成行不入池 + 真实流量漏字率极低→飞轮死锁；重启需先有非循环 insert 监督源 | 同上节；TW-NLP/ELECTRA 门控与我们②同构 |
| 17 | ② 定位层外接业界方案补漏字定位（GitHub 取证 pycorrector/xmnlp/funNLP 等；MLM gap-filling / deletion-augmentation 论文路线） | **否**（外接引进线关闭；瓶颈改道自造监督：合成 I 监督臂立项待跑） | 内部取证 `gec-zh` 无 insert 候选生成器（盲区在定位侧）；业界检测清一色 S 型（词表/词频/ELECTRA 门控），insert 定位无现成开源实现；LLM 整段重写差分无人做字级管线；arXiv 两路线（deletion-augmentation、gap-filling×spell checking）零命中不引 | `tools/label-rollout/README.md`「D ② 定位外部方案调研」；候选 1 合成 I 监督臂（synthD 删 1 字行训 ② 定位臂，评测挂 gray380 真实 401 I 位+真实 FP 口径）主推、候选 2 位级 Δ-PPL gap-filling 探针并行（与 §13 #1 句级 OOV 旧案划界）、候选 3 差分独得提名弃。**补（同日）**：候选 1 数据 v1 就绪 `synth_insert_arm.py`（字分布按真实 I 位取证、位级 Δ 剔可省伪影 τ=1.1、叠字毒位硬闸；首批 5 万行秒级可重生）；训练方案 v2 成稿 jev-gec `PLAN_v2.md`（盘点发现现池旧世代+转换丢 span），待拍板起跑 |

> #1 是最容易复发的一条：**「灰区率下降」不等于收益**。任何扩表/换语料的提案，
> 验收口径必须是「**等漏检下的放行率**」——先跑 `eval_ngram_table_ab.py`（AUC 不升或
> 等安全线放行率增幅 <5pp 即拒绝），再跑 `eval_l3_funnel.py`（`L3_NGRAM_TABLE`/
> `L3_BIGRAM_TABLE` A/B，看 MuCGEC FN 与真实用户文本门控段的 clean/病句放行比）。
> 只看灰区率或 LLM 调用率的报告一律不采信。








### 11.19 生产 accepted edits I 型存量盘点：存量不足，臂 D 转池 I 位过采样（2026-11-20）

背景：#11.18 补二判 MLM 指示/蒸馏两路后，唯一主矛盾收敛为真实 I 位监督密度
（池 train 仅 2,328 位 ≈1:1300 稀疏）。本轮盘生产库 accepted edits 能否补足。

**口径**（`datasets/extract_prod_accepted.py`，已落盘）：root langfixer.db
detect_history corrected_text 非空 1,459 行 + mistakes 10 行；逐行 SequenceMatcher
对齐出净修订，I=单字插入（a==b 坐标）；gray380 重合行按零污染铁律先剔，池 train/dev
重合再剔。

**结果**：采纳 1,469 行中原样采纳 813、gray380 演示命中 625（生产演示跑的北极星
同源文），真实生产修订仅 **31 行**；op 分布 S32/D18/M5/**I9**；含 I 行 8、去重键 6；
虚词比 .556（为5/并2/后1/，1）；I 位句超长(>126字) 5 个。

**判决：生产线存量不足**（9 位 ≪ 1k 起量门槛，也＜预定的「几百」下限），生产
accepted edits 接入路线关；臂 D 落点改为**池 I 位过采样+加权消融单独跑**（jev-gec
`track_b_v2.py --i-oversample`）：池 train 12,359 行中含 I 行 1,643、I 位 2,185，
×4 复制后 I 行占比≈7.5%→≈24%，句头+字头密度同升，pos_weight 自动口径同步。北极星仍
是 gray380 I 位召回非零 + 净句 FP≤2%（dev 面 + gov 干净句面）。若过采样仍 0，
则「监督密度」假设也完，回到定位头架构本身重审。

**补（同日臂 D 出数，重大勘误）：北极星评测脚本 `locate_eval.py` 字概率取数 bug，
此前所有「I 位 0/1050、p90=0.00」「FP 0/729、0/1000」均为假读数**。单行输入 p 形状
[1,n]，squeeze(-1) 后 p[i] 索到 batch 维→候选只剩 CLS 零宽 offset→概率面恒 0。
修后复跑：冻结/臂C/D1 在 τ.5 本就有 I 位信号 41~45%，但句级 FP ≈100%——真实失败
模式是「有信号但分离度撑不住 FP≤2% 闸」，非全暗；域假说/合成取证的全部判决证据
需按新口径重审。**臂 D2（纯池流移 synth + I ×4）：R .773@τ.5；ROC τ.8 R.151/FP.10，
τ.9 R.028/FP dev1.6%/gov0.7%——北极星在 FP≤2% 带内首次非零**。synth 流实锤有害
（易信号抬高全局 p）。生产存量不足的盘点结论不变（仍只有 9 位），但接盘它的
前提从「补密度」变成「在真实漏字分布上拉开工作点」：D2 配方 + 干净侧 hard-negative/
FP 正则下一臂。

### 11.18 臂 C 公文体合成域消融：域假设证伪（2026-11-20）

背景：首轮判决（jev-gec PLAN_v2 §6）判合成流 v1 对 gray380 真实 I 位全域 p90=0，
归因「公文域缺席」。本轮验证：gov.cn 政策文件库 JSON 接口抓 1,400 篇公文→66,138
干净句（`datasets/scrape_govcn.py`，限速/断点/三过滤，正文零脏行，与 gray380 零命中），
synth_insert_arm 换源重生 5 万行（seed 43，虚词 17.5%/实词 82.5%，与 v1 分布一致），
gov-only 消融臂（学习者域流移出，冻结编码器 4000 步）。

**结果**：句头 dev AUC .944 零回退；字头 dev P .052/R .483 持平；净句 FP dev 0/729、
公文面 0/1000 全过；但 **gray380 1050 I 位召回 0/1050，概率分位 p10/p50/p90 全 0**——
与 v1 完全同签名。

**判决：域假设证伪**。换干净句域不改变任何东西——问题不在合成源语料域，而在
监督结构本身：合成注入位是人为 gap，宿主句在注入处**没有任何真实漏字的分布特征**
（真实 I 漏字处邻接字/搭配本就读得出缺，合成句里那个位置读不出）；字头学到的
是「人造 gap 形态」而非「漏字语言学信号」。后续方向：①真实 I 位 vs 合成注入位
邻接特征对比取证（数据侧，先做）；②宿主换真实 learner 干净行；③真实弱监督
（双头分歧/规则灰区 I 型）入池。合成流两路均保留在盘供对照，不再加新域。

**补（同日取证，jev-gec `eval/gap_forensics.py`）**：把漏字位换 [MASK] 交原始
bert MLM 读漏字，假设反方拉正——**gov 合成注入位 p med=0.997/top10=0.996**
（Δ 过滤保留了语境必需位，合成行是「喊缺」的易任务，字头学到的是易信号）；
而 **gray380 真实 I 位 rank med=4/top10=0.548**——原始 MLM 头对我们字头全域
漏掉的 401 位本身就有强信号，丢在微调 encoder+linear 头恢复不出。臂 D 落点：
MLM gap 指示直接当检测器（先测干净文本位 FP 面）或当字头蒸馏教师；
「继续自造合成域」路线彻底关闭。

**补二（同日 FP 面预检，jev-gec `eval/gap_forensics.py` 五组）：MLM gap 指示
检测器判死**。干净句人工删字位 p_del@0.5 = .740（dev）/.862（gov）、句级盲扫
τ=0.9 仍 1.000；gray380 真实 I 位 p_ref@0.5 仅 .300、池真实 I 位 .219——FP 全程
高于召回。机理：**真实漏字恰因「不读也通」才漏**，p 量可预测性分不开真假 gap；
蒸馏教师同理拒（会把 FP 面蒸进去）。I 问题回到监督密度：池 train 真实 I 位仅
2,328 位（≈1:1300 稀疏）；下轮盘点**生产 accepted edits 的 I 型存量**（本侧
生产日志带坐标真实修订，此前生产池只当 label=0 用），真实监督挖真实监督。

### 11.17 P8：Track B 生产产物首次上机验收（2026-11-19）

**训练**（3090，jiulu-GPU，/mnt/data/github/jev-gec）：P7 定案配方——池 zh_v5a1、
bert-base-chinese、lr 2e-5、batch 32、maxlen 256、last-epoch 固定 2 ep。
seed 20261105 → dev AUC 0.836（自检盘上 0.829≈内存 0.828 ✓）；seed 20261106
（副产品，供后续集成实验）→ dev AUC 0.826。日志 /tmp/jev_prod_2026110{5,6}.log。

**回传与验收**（Mac，冻结集 zh_gate_1k，n=996，有错率 0.500）：
- 产物 models/zh v1（track B-encoder，encoder/ 微调底座 + head.pt + 温度 T=1.3）；
  旧 v7 挪至 models/zh_v7_old。
- 冻结 AUC **0.821**，ECE(raw) 0.069；分项 cctc .854 / cscd .891 / cscdns .752 /
  nlpcc23 .772。操作点：漏检≤2% 跳过率 0.8%，漏检≤5% 跳过率 2.0%。
  延迟 p50=11ms p95=20ms（MPS）。对比坏产物时代 v7 的 .57–.60，产物链路修复闭环。
- 与服务器研究数对照：单模型 .821 vs P7 两 seed 集成冻结 .828——差值在预期内；
  集成要成为可部署产物需 serve 侧支持多 head 融合（ensemble_fuse.py 目前只是
  研究出数口径，非落盘格式），列入后续项。
- 本侧 serve/server.py 已同步「encoder/ 优先于 meta.base」修复（服务器 commit
  17acdd1 同款），否则会把微调头接回原底座、数字假低。
