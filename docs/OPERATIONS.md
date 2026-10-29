# 运维配置（环境变量矩阵）

从 README 迁出的完整环境变量清单（开发常用项见 README「快速开始」）。

## 环境变量

| 变量 | 默认值 | 说明 |
|---|---|---|
| `PORT` | `4801` | API 服务端口 |
| `GEC_TABLES_DIR` | `decision-tables` | 决策表目录 |
| `GEC_KNOWLEDGE_DIR` | `knowledge-base` | 知识库目录（`sources.json` + `points/{lang}.json`）；目录缺失时归因结果降级为纯表级（不含知识源引用）；发布 / 热更新成功后原子写回此目录 |
| `GEC_KNOWLEDGE_ENABLED` | `1` | 知识层总开关（`0` 关闭知识向量召回 + 知识库富化 + 数据驱动归点；测评消融臂 `knowledge: false` 即切此开关，等价设置项 `knowledge_enabled`） |
| `GEC_LEXICONS_DIR` | `lexicons` | 规则词表目录（`{zh,en,ja}_gec_lexicon.json`） |
| `GEC_SETTINGS_FILE` | `langfixer-settings.json` | 运行时设置文件（`GET/PUT /gec/settings` 读写）；**优先级高于下方环境变量**，存在且可解析时整体覆盖 |
| `HANLP_ENDPOINT` | `http://127.0.0.1:8000` | NLP 服务（NarrativeOS narrative-operator-nlp，中文+英文共用） |
| `SUDACHI_ENDPOINT` | `http://localhost:4802` | 日文 Sudachi 分词服务 |
| `GEC_OFFLINE` | — | `1` 时全部检测器强制离线模式 |
| `GEC_ENGINE_{ZH,EN,JA}` | `rule` | 该语言的检测引擎：`rule`（默认）/ `statistical`（统计模型为主 + 规则兜底，结果合并）/ `llm`（预留） |
| `GEC_STAT_ENDPOINT` | `http://127.0.0.1:4803` | 统计 GEC 服务（Harper / HuggingFace transformers server 类，`POST /gec`） |
| `GEC_STAT_TIMEOUT_SECS` | `30` | 统计 GEC 服务请求超时 |
| `GEC_STAT_MODEL` | `hf-gec` | 统计模型名（日志 / 元数据） |
| `GEC_DEC_ENABLED` | `0`（默认关闭） | **vegaml 决策模型层**开关（三层级联：规则 → 决策 → LLM）：**默认关闭**（L3 特征模型是唯一的 pre-LLM 门控，决策模型是 opt-in 层）；显式 `1`/`true` 时开启 |
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
| `GEC_TABLES_UPDATE_TOKEN` | — | 决策表热更新鉴权 token；设置后 `PUT /gec/tables/:lang` / `POST /gec/tables/:lang/rollback` 需携带匹配的 `X-Tables-Update-Token` 头（未设置时不鉴权） |
| `GEC_RULE_STATE_FILE` | `langfixer-rule-state.json` | 规则运行时开关持久化文件（`{rule_id: bool}`；启动时加载，`PUT /gec/rules/:lang/toggles` 后写回） |
| `CORS_ORIGINS` | — | 允许的跨域来源（逗号分隔）；未设置时允许任意来源（本地开发） |

> 说明：上表 `HANLP_ENDPOINT` / `GEC_OFFLINE` / `GEC_TABLES_DIR` / `GEC_LEXICONS_DIR` / `LLM_*` 等值也可在 Web 端「设置」界面（`GET/PUT /gec/settings`）或桌面端「设置」界面（Tauri `get_settings` / `update_settings` 命令，与 Web 端同构）直接修改——即时生效（热重建检测器 / 重载模板库与决策表）并持久化到 `GEC_SETTINGS_FILE`，无需依赖环境变量。桌面端离线开关默认开启（`GEC_OFFLINE` 未设置 = 离线），与 api-server 的默认在线相反。
