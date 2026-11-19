# 运维配置（环境变量矩阵）

从 README 迁出的完整环境变量清单（开发常用项见 README「快速开始」）。

## 环境变量

| 变量 | 默认值 | 说明 |
|---|---|---|
| `PORT` | `4801` | API 服务端口 |
| `GEC_TABLES_DIR` | `decision-tables` | 决策表目录 |
| `GEC_KNOWLEDGE_DIR` | `knowledge-base` | 知识库目录（`sources.json` + `points/{lang}.json`）；目录缺失时归因结果降级为纯表级（不含知识源引用）；发布 / 热更新成功后原子写回此目录 |
| `GEC_KNOWLEDGE_ENABLED` | `1` | 知识层总开关（`0` 关闭知识向量召回 + 知识库富化 + 数据驱动归点；测评消融臂 `knowledge: false` 即切此开关，等价设置项 `knowledge_enabled`） |
| `GEC_LEXICONS_DIR` | `lexicons` | 规则词表目录（`{zh,en,ja}_gec_lexicon.json` + 领域术语库 `terminology_{lang}.json`：`protected` 受保护术语（错误 span 与其出现位置重叠即丢弃）+ `forced` 强制更正（`{wrong,right,note}`，出现即注入）；缺失/损坏 = 空库 no-op） |
| `GEC_SETTINGS_FILE` | `langfixer-settings.json` | 运行时设置文件（`GET/PUT /gec/settings` 读写）；**优先级高于下方环境变量**，存在且可解析时整体覆盖 |
| `HANLP_ENDPOINT` | `http://127.0.0.1:8000` | NLP 服务（NarrativeOS narrative-operator-nlp，中文+英文共用） |
| `SUDACHI_ENDPOINT` | `http://localhost:4802` | 日文 Sudachi 分词服务 |
| `GEC_OFFLINE` | — | `1` 时全部检测器强制离线模式 |
| `GEC_ENGINE_{ZH,EN,JA}` | `rule` | 该语言的检测引擎：`rule`（默认）/ `statistical`（统计模型为主 + 规则兜底，结果合并）/ `llm`（预留） |
| `GEC_STAT_ENDPOINT` | `http://127.0.0.1:4803` | 统计 GEC 服务（Harper / HuggingFace transformers server 类，`POST /gec`；MacBERT-CSC 臂见 jev-gec `serve/csc_server.py` :4807）。已纳入依赖监督（`GET /gec/deps` 第 7 项「深度纠错服务」，外部只探测不拉起；未配地址或 `GEC_ENGINE_*` 非 statistical 时显示 off 不报红） |
| `GEC_STAT_TIMEOUT_SECS` | `30` | 统计 GEC 服务请求超时；**实时路径建议 2–3**（服务挂起时超时才触发规则兜底，30s 会把交互式请求白等满） |
| `GEC_STAT_MODEL` | `hf-gec` | 统计模型名（日志 / 元数据） |
| `GEC_DEC_ENABLED` | `0`（默认关闭） | **vegaml 决策模型层**开关——该层**已退役出产品路径**（EVOLUTION.md §13 #10，2026-11-02 执行：设置界面入口移除）；代码保留为实验路径，显式 `1`/`true` 时开启 |
| `GEC_DEC_ENDPOINT` | `http://127.0.0.1:4804` | vegaml 决策模型服务（`POST /decide`；`decision-model/server.py`，`VEGAML_MOCK=1` 可离线 mock） |
| `GEC_DEC_MODEL` | `vega-base` | 决策模型名（日志 / 元数据） |
| `GEC_DEC_TIMEOUT_SECS` | `10` | 决策模型请求超时（超时 = 该句弃权，回退纯规则） |
| `GEC_DEC_ABSTAIN_THRESHOLD` | `0.5` | 弃权阈值：`has_error_prob` 低于该值视为无错误（宁可漏报交 LLM 层，不误报）；服务方输出 `abstain: true` 时直接视为弃权（三级响应 level 2 的触发信号） |
| `GEC_L3_ENABLED` | `1`（默认开启） | **L3 特征模型层**开关（level 3 先跑特征模型，LLM 收缩到纯长尾）：**默认开启**；显式 `0`/`false` 时 level 3 回退「无条件 LLM」 |
| `GEC_L3_ENDPOINT` | `http://127.0.0.1:4806` | L3 特征模型服务（`POST /l3-detect`；`feature-model/server.py`，`FEATURE_MOCK=1` 可离线 mock） |
| `GEC_L3_MODEL` | `feature-0.1` | 特征模型名（日志 / 元数据） |
| `GEC_L3_TIMEOUT_SECS` | `10` | 特征模型请求超时（超时 = 服务不可用，level 3 回退 LLM 兜底） |
| `GEC_L3_ABSTAIN_THRESHOLD` | `0.5` | 弃权阈值：`has_error_prob` 落在 `[0.25, 该值)` 灰区视为弃权 → LLM 长尾兜底；低于 0.25 视为明确无错（放行，不调用 LLM） |
| `GEC_L3_DATA_DIR` | `feature-model/l3_data` | L3 飞轮样本目录（`confirmed.jsonl` 确认样本 + `retrain_meta.json` 重训练元数据；api-server / 桌面端共用） |
| `GEC_L3_ADOPT_COLLECT` | api-server：关；桌面端：开 | **采纳/放弃数据资产沉淀**开关：采纳 → `l3_data/<lang>/adopted.jsonl`（用户确认的句级 `src→dst` 平行语料）；放弃 → `rejected.jsonl`（片段级负偏好）。**api-server 形态默认关**（共享部署不默认把用户数据用于训练），显式 `1`/`true` 开启；桌面端自用默认开（`0`/`false` 关闭）（检测历史 `corrected_text` 列不受此开关影响，属产品功能） |
| `LLM_BASE_URL` | `https://api.openai.com/v1` | LLM 服务（任意 OpenAI 兼容端点） |
| `LLM_API_KEY` | — | 缺省 = LLM 讲解未启用（`/gec/explain*` 返回 503，前端回退静态建议）且三级响应 level 2/3 的 LLM 兜底不可用（触发时返回 503） |
| `LLM_MODEL` | `gpt-4o-mini` | 模型名——必须是该端点 `/v1/models` 里存在的名字（名字错 = 每次调用 HTTP 400，LLM 层整层白跑） |
| `LLM_TIMEOUT_SECS` | `30` | LLM 请求超时（思考模型 / 整载体长文要放宽：实测整载体检测 60s+，30s 必然超时失败并降级 level 1） |
| `LLM_PROMPTS_DIR` | `prompts` | 提示词模板目录（`{lang}_{cefr}_prompt.json`，改文件即热更新；含 `{lang}_detect` 兜底检测模板） |
| `LLM_RETRY_BACKOFF_SECS` | `5,20` | 检测级联 LLM 兜底的退避表（逗号分隔，长度即重试次数；空 = 不重试）。网关 503/429/传输层超时按「没排到」退避重试，4xx 鉴权类不重试 |
| `LLM_RETRY_BUDGET_SECS` | `120` | 上述重试的**挂钟总预算**（含每次请求自身耗时，不是只算睡眠）：必须落在前端 level ≥ 2 请求超时（180s）之内；网关卡死时到点收手 |
| `GEC_LLM_MASK_PII` | `1`（默认开启） | **LLM 出站前 PII 脱敏**：发往 LLM 的文本中等长掩码身份证号 / 手机号 / 固话 / 邮箱本地部分 / 长数字串（掩码后与被掩区间重叠的 LLM 检出丢弃，防幻觉回填）；显式 `0`/`false` 关闭。规则 / L3 / 本地层仍看原文，仅 LLM 出站路径受影响 |
| `GEC_TABLES_UPDATE_TOKEN` | — | 运营写操作鉴权 token；设置后 `PUT /gec/tables/:lang` / `POST /gec/tables/:lang/rollback` / 知识库 PUT / 数据集导入 / `POST /gec/ops/unlock`（前端「平台运营」角色解锁）需携带匹配的 `X-Tables-Update-Token` 头（未设置时不鉴权） |
| `GEC_RULE_STATE_FILE` | `langfixer-rule-state.json` | 规则运行时开关持久化文件（`{rule_id: bool}`；启动时加载，`PUT /gec/rules/:lang/toggles` 后写回） |
| `CORS_ORIGINS` | — | 允许的跨域来源（逗号分隔）；未设置时允许任意来源（本地开发） |


> **LLM 层失败立场（2026-11 演练定版）**：level 2/3 的 LLM 兜底层**运行时**失败
> （超时、排队耗尽、熔断冷却）不把整次 detect 打回 502——规则层结果照常返回，
> 层级摘要记 `llm: unavailable`（前端显示「不可用已回退」）；未配置 LLM 仍明确
> 报错（那是配置问题，不是运行时故障）。LLM 客户端带进程级熔断（连续失败 3 次
> 开断 30s，冷却到期自动探测恢复），卡死网关下第 2 次请求起毫秒级失败而不是每
> 个用户都交一份超时税；演练数字见 CHANGELOG 同日条目。
> 说明：上表 `HANLP_ENDPOINT` / `GEC_OFFLINE` / `GEC_TABLES_DIR` / `GEC_LEXICONS_DIR` / `LLM_*` 等值也可在 Web 端「设置」界面（`GET/PUT /gec/settings`）或桌面端「设置」界面（Tauri `get_settings` / `update_settings` 命令，与 Web 端同构）直接修改——即时生效（热重建检测器 / 重载模板库与决策表）并持久化到 `GEC_SETTINGS_FILE`，无需依赖环境变量。桌面端离线开关默认开启（`GEC_OFFLINE` 未设置 = 离线），与 api-server 的默认在线相反。
