# LangFixer

[![CI](https://github.com/narrativeos/LangFixer/actions/workflows/ci.yml/badge.svg)](https://github.com/narrativeos/LangFixer/actions/workflows/ci.yml)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache--2.0-blue.svg)](LICENSE)

**LangFixer：把「语法纠错」升级为「语法教学」。**
检测中英文日文文本的语法错误之后，它回答教育上真正重要的问题——**这个错误为什么犯**（归因：知识缺口 / 习惯固化 / 规则缺失 / 规则遗忘 / 搭配薄弱 + 韦纳三维），**该用什么补救**（知识点讲解、CEFR 分级练习），并让系统随使用**越用越准**（特征模型训练飞轮）。

- **检测**：中文、英文（HanLP 服务 + Rust 侧规则）、日文（Sudachi + 本地规则），每种语言均带**离线兜底**（外部 NLP 服务不可达时自动降级为本地词典/规则检测）；可选挂载 **vegaml 决策模型层**（`decision-model/`）形成三层级联「规则 → 决策 → LLM」——规则未命中的句子交由 vegaml 判断，弃权则放行、服务不可用自动回退纯规则（**默认关闭**，为 opt-in 层：L3 特征模型是唯一的 pre-LLM 门控，设置界面 / `GEC_DEC_ENABLED=1` 可显式开启）；**三级响应**（API `level` 参数）在前两层之上按需触发 LLM 兜底：L3 特征模型（`feature-model/`，**默认开启**）门控——level 2 高置信时直接定案、弃权 / 服务不可用时（L3 未启用时回退决策模型弃权信号）对全文跑 LLM 复查（level 2）；level 3 无条件强制 LLM——特征模型高置信时直接定案（含模式化 span 定位 + 机械修正），**LLM 层收缩到纯长尾**（前端「漏报检测」按钮 = force_llm，仍无条件 LLM）。级联与三级响应的完整机制见下文「三级响应」。
- **词表**：各语言的规则词表（混淆词、过去式表、助词组合等）外置于 `lexicons/*.json`，不重新编译即可扩充；文件缺失/损坏时回退到内置默认词表。
- **归因**：ZEN Engine（GoRules JDM 标准）决策表，输出归因标签（知识缺口 / 习惯固化 / 规则缺失 / 规则遗忘 / 搭配薄弱）+ 韦纳三维度（内外源 × 稳定性 × 可控性）+ **CEFR 难度分级（A1-C2）** + 补救措施 + 决策追踪。
- **交付**：Axum API 服务（Docker 化）+ Vue 3 Web UI + Tauri 2 桌面端（离线模式）。

## 目录结构

```
LangFixer/
├── Cargo.toml            # workspace（11 个 Rust crate）
├── shared-model/         # 统一数据模型（Error / 归因 / 用户 / 决策表元数据）
├── gec-core/             # GECDetector trait + GecEngine 引擎路由（rule/statistical/llm）+ 语言路由
├── gec-zh/               # 中文检测器（HanLP /analyze + 规则，离线兜底）
├── gec-en/               # 英文检测器（HanLP /analyze + 规则，离线兜底）
├── gec-ja/               # 日文检测器（Sudachi /parse + 规则，离线兜底）
├── attribution-engine/   # ZEN 决策表引擎 + 热更新 TableStore + 知识库 KnowledgeStore + 韦纳映射
├── langfixer-llm/        # LLM 扩展（OpenAI 兼容客户端 + SSE 流式 + 提示词模板）
├── decision-model/       # 决策模型（vegaml）：FastAPI 服务（POST /decide，支持 VEGAML_MOCK 离线 mock）+ 微调工作流（extract/train/eval）+ gold 数据转换
├── feature-model/        # L3 特征模型：FastAPI 服务（POST /l3-detect，支持 FEATURE_MOCK 离线 mock）+ 训练工作流 + 模式化 span 定位 / 机械修正（correct.py）+ **训练飞轮**（LLM 确认样本 → 重训练 → 热切换，自进化）
├── api-server/           # Axum API 服务（Dockerfile 内置）
├── desktop-app/          # Tauri 2 桌面端（离线运行）
├── web-ui/               # Vue 3 + Vite + Pinia 前端
├── lexicons/             # zh / en / ja 三张 GEC 规则词表（JSON，可热扩充）
├── prompts/              # LLM 提示词模板（3 语言 × CEFR 分级 + generic + 3 语言 detect 兜底检测模板，版本化）
├── decision-tables/      # zh / en / ja 三张 JDM 决策表（纯规则：错误类型 × 新手/置信度 → 归因策略）
├── knowledge-base/       # 知识库：知识源注册表（sources.json）+ 知识点集合（points/{lang}.json）
│                         #   知识点 → 知识源 1:N 关联；仓库内置仅自撰示例源（Apache-2.0），
│                         #   用户导入的书籍属用户数据，不随发行物分发（见下文「IP 边界」）
└── scripts/              # build.sh / dev.sh / release.sh
```

### 规则与知识分离（知识库架构）

- **决策表 = 可提取的规则**：三张表结构同构（4 输入 + 7 输出），只承载
  「错误类型 × 新手/置信度 → 归因策略 + 通用补救 + CEFR」，可随产品发布；
- **知识库 = 知识内容 + 知识源**：`knowledge-base/sources.json`（知识源注册表，
  仅元数据：书名/作者/版次/许可范围）+ `knowledge-base/points/{lang}.json`
  （知识点：语言内稳定 ID + 讲解/例句/改错对）。知识点与知识源按 **1:N** 关联
  （`point.sources[]`）——同一知识点可被多个知识源背书，新增源只需追加关联，
  点 ID 与决策表规则均不变；
- **运行时富化**：归因流水线 = 决策表求值（纯规则）→ 知识库按 `grammar_point`
  富化 `AttributionResult`（点级补救/CEFR/实例 + 结构化 `sources[]` 引用）；
  部署环境未提供知识库目录（`GEC_KNOWLEDGE_DIR`）时优雅降级为纯表级结果；
- **IP 边界**：`license_scope != public` 的知识源内容只在服务端内存中流转，
  不进入 Docker 镜像（`Dockerfile` 不 COPY、`.dockerignore` 排除）；
  回归测试守护「决策表 JSON 不含任何书源内容」（`attribution-engine` / `api-server` 测试）。

### 知识源导入管线（新增一本书的一键导入，全链路）

把「一本书从人工提炼到入库」产品化为可重复管线（核心 `attribution-engine/src/ingest.rs`）：

```
POST /gec/knowledge/ingest（书正文 + 源元数据）
  → 正文分块 → 内置 agent 逐块挖掘（prompts/{lang}_ingest_prompt.json，en/zh/ja 三语）
      工具面：list_knowledge_points（对齐既有点）/ stage_points（草稿入区，逐条校验）
      单块失败自动重试一次；连续 3 块失败熔断（会话状态 running → mined / failed）
  → 草稿区（staging，持久化于 {GEC_KNOWLEDGE_DIR}/staging，发布前不影响归因；
      重启后未完成的会话标记为 failed，可放弃或重新导入）
  → GET  /gec/knowledge/ingest/:id/drafts 草稿明细（逐条归并预判 endorse / create / covered）
  → GET  /gec/knowledge/ingest/:id/plan 发布预览（确定性归并：既有点 1:N 背书 /
      新概念造新点 {errorType}.{max+1} / 本源已背书 already_covered）
  → POST /gec/knowledge/ingest/:id/publish 人工一键发布
      （知识库热更新 + 写回 {GEC_KNOWLEDGE_DIR}（sources.json / points/{lang}.json，
      原子写，重启后存活），归因富化即时生效；与知识库 PUT 共用 token 鉴权；
      发布同时把新点改错对汇入「改错对补充」建议队列，见下文反哺闭环）
```

- **点 ID 不随书变**：新书优先给既有点追加 `sources[]` 背书（agent 显式 `match_point_id`
  或确定性标题匹配），只有新概念造新点——决策表与既有规则不动；同一 `source_id`
  可重复导入（增量背书，已覆盖草稿在 plan 中标 `already_covered`，注册表幂等）；
- **LLM 只做抽取，代码做校验/归并/ID 分配**：非法草稿拒收，防幻觉污染数据集；
- **IP 守卫**：导入/发布默认只写知识库，不触碰 `decision-tables/`（无规则草稿的会话
  逐字节断言不变）；仅当会话经「归因规则派生」产出规则草稿且人工确认发布时，
  才写入 `_meta` 标记该源的决策表行（见下文「归因规则派生」），产品内置行与
  其他知识源行绝不覆盖；且不内置任何第三方书籍正文（冒烟脚本使用自写材料）；
- **UI 向导**：知识库面板「导入知识源」五步向导（登记源 → 挖掘进度轮询 → 草稿审阅
  → 归并预览 → 一键发布）；进行中的会话可在面板恢复查看或放弃；
- **桌面端同构**：`start_ingest` / `ingest_sessions` / `ingest_session` / `ingest_drafts` /
  `ingest_plan` / `publish_ingest` / `discard_ingest` Tauri 命令（本地操作无需 token）；
- agent 工具两端同构（api-server / 桌面端）：`list_knowledge_points` /
  `register_ingest_source` / `stage_points` / `ingest_status` / `discard_ingest`（自主级），
  `publish_knowledge`（Confirm 级，经 `POST /gec/agent/confirm` 确认后执行）；
- **反哺闭环**：发布新点的 `corrections[]` 自动汇入进化中心「改错对补充」
  （`lexicon_add`）建议队列（pending，幂等去重）；人工「采纳入召回库」后写入
  学习知识（向量召回层）即时生效；已发布点的改错对同时参与**数据驱动归点**——
  新导入的书无需改代码即可让 `(wrong → right)` 精确命中归到该点（未命中回退
  静态映射 `gec-en/src/point.rs`）；
- **冒烟**：`BASE=http://127.0.0.1:4801 ./scripts/ingest_smoke.sh`（真 LLM 全链路，
  自写语法材料；默认跑到归并预览后放弃，`PUBLISH=1` 才真实发布）。

### 文档解析与归因规则派生（一本书端到端自学习闭环）

把「书本文件 → 知识点 → 决策表规则」全链路内置化，不再需要外部智能体预处理：

```
上传 .epub/.txt/.md/.docx/.pdf（≤64MB）
  → POST /gec/knowledge/parse-preview 解析预览（document-parse crate）
      epub：container.xml → OPF（标题/作者/语言）→ spine 顺序抽 XHTML，一章一节
      docx：word/document.xml 段落 + 标题样式切章
      txt/md：UTF-8 → GBK 编码兜底；markdown 按 ATX 标题切章
      pdf：仅支持有文本层 PDF；扫描版明确报错（NoTextLayer），不做假解析
  → POST /gec/knowledge/ingest/upload（勾选章节 → chunk_chapters 章节感知分块 → 逐块挖掘）
  →（挖掘完成 mined 后）POST /gec/knowledge/ingest/:id/derive-rules
      按 error_type 分批跑内置 agent（prompts/{lang}_rules_ingest_prompt.json，三语）
      工具面：stage_rule_drafts（自主级；语义草稿，只进会话草稿区）
  → GET  /gec/knowledge/ingest/:id/rules-plan 规则归并预览
  → POST /gec/knowledge/ingest/:id/publish 一键发布：
      预检（决策表合并结果过 ZEN 校验，失败整体中止）→ 知识点归并发布
      → 决策表热更新（TableStore.update + 版本历史 + 原子写回文件）
```

- **LLM 只出语义，代码生成 JDM**：agent 草稿只有
  `{error_type, novice, min_confidence, attribution, weiner*, cefr, remedial}`；
  JDM 条件表达式（`== "article"` / `== true` / `>= 0.8`）、行 ID
  （`{error_type}_{novice|hand|any}`）、溯源标记（`_meta=source_id`）全部由
  `attribution_engine::ingest::build_rule_row` 生成，且只填目标表实际存在的列；
- **冲突策略保守**：同 `_id` 行属产品内置（无 `_meta`）或其他知识源 → `conflict`
  跳过不覆盖；本源重派生 → `replace` 原地替换；新行插入到通用兜底行之前
  （保持 hit-policy first 行序不变量）；
- **资源包生命周期自动兼容**：派生规则行带 `_meta=source_id`——导出包携带、
  删除资源包按源摘除、导入按 `_meta` 合并，与既有资源包机制同一套纯函数；
- **UI 向导扩展**：登记源步骤支持文件选择 + 解析预览（章节勾选 / 告警展示）；
  草稿审阅步骤新增「派生归因规则」；归并预览同时展示规则计划（新增/替换/冲突）；
- **桌面端同构**：`parse_preview` / `start_ingest_file` / `derive_rules` /
  `ingest_rules_plan` Tauri 命令；agent 工具 `stage_rule_drafts` 两端同构。

### 知识源资源包（整理盘点 / 打包导出 / 导入 / 删除）

一个知识源（如内置示例源 `src-sample-en`）= 一个**资源包**单元，组件 =
知识数据（注册表条目 + 该源背书的全部知识点 + 未发布草稿）+ 归因规则（决策表中
`_meta` 标记该源的规则行）+ 决策数据（`decision-tables/{lang}_points.json` /
`{lang}_point_index.json` 源提取数据，条目带 `source_id` 归属标记）。三种操作两端同构
（REST / Tauri `dataset_summary` / `dataset_export` / `dataset_import` /
`dataset_import_bytes` / `dataset_clean`）：

```
GET  /gec/knowledge/dataset/:id          盘点（点数 / 规则行数 / aux / 草稿清单；删除前预览）
POST /gec/knowledge/dataset/:id/export   打包导出（?format=zip = zip 压缩包附件下载，单条目
                                         pack.json deflate 压缩；缺省返回 JSON。自描述格式：
                                         format / format_version / exported_at / checksum /
                                         source / components）
POST /gec/knowledge/dataset/import       导入（请求体自适应：zip 压缩包或裸 JSON；校验和不符 /
                                         格式版本不符 → 整体拒收 400）
POST /gec/knowledge/dataset/:id/clean    删除资源包（?keep_orphan_points=1 保留失去全部背书的点）
```

- **导出即下载**：web-ui 导出按钮直接下载 `{source_id}-resource-pack.zip` 压缩包（成功提示含
  文件名与大小）；桌面端弹**原生「另存为」对话框**选择保存位置（`tauri-plugin-dialog`，
  取消即中止），保存后提示完整文件地址（未指定路径时兜底写 `{知识库目录}/resource-packs/`）；
  导入可直接上传该 zip（服务端解压 + 校验和验证），也兼容裸 JSON；

- **规则溯源**（资源包按源摘除/合并的前提）：`decision-tables/generate_tables.py`
  给每条规则行写 JDM 行级元数据 `_id`（规则稳定 ID）与 `_meta`（派生该规则的
  知识源 source_id；无 `_meta` = 产品内置通用规则，删除资源包不触碰）。
  zen-engine 2.1.2 方言要求规则行全部为字符串键值，故 `_meta` 为字符串 source_id；
  aux 决策数据文件同理按**条目级** `source_id` 归属（同语言多知识源共存时，
  导出/删除某源的资源包不误伤其他源条目；无标记的旧格式文件整体视为导出源所有，
  导出时自动注入标记）；
- **导入幂等**：注册表 upsert；知识点按 `point_id` 归并——既有点只追加 `sources[]`
  背书、**不覆盖内容**（与导入管线「点 ID 不随书变」一致），新概念才造新点；
  规则行按 `_id` 替换/插入（保持「源派生行在前、通用兜底行最后」行序）；
  aux 条目级合并（该源以资源包为权威，其他源条目保留）；
  staging 草稿仅当无同 ID 会话时恢复；
- **删除语义**：**删除前默认自动导出 zip 备份**（web 端下载到浏览器下载目录；桌面端写
  `{知识库目录}/resource-packs/`，同名自动加时间戳；备份失败则中止删除，可在确认框
  取消勾选）→ 预检（各表摘除结果先通过引擎校验，失败整体中止）→ 注册表条目移除 →
  各点剥离该源引用 → 失去全部背书的点默认删除（`keep_orphan_points=1` 保留）→
  决策表摘除该源标记行（通用行保留，归因自动回落 default 行）→ aux 文件条目级摘除
  该源条目（摘空后删文件）与草稿删除；决策表变更走 `TableStore::update` 校验 +
  版本历史留痕（可回滚）+ 原子写回文件（跨重启存活）；
- **鉴权与 IP 边界**：import/clean 与知识库 PUT 共用 `X-Tables-Update-Token`。
  与导入管线守卫不同：资源包 import/clean 是**显式管理员级资源单元生命周期操作**，
  按设计会写回 `decision-tables/`（留痕可回滚）；导入管线仍绝不触碰决策表。
  运行时不需要任何清理脚本——按源清理走 clean 接口（web-ui 知识库面板
  「导出 / 删除（盘点预览确认）/ 导入资源包」）；开发期重建仍用
  `decision-tables/generate_tables.py`。

## 快速开始

```bash
# 1. 本地开发（API :4801 + 前端 :4805，Vite 代理 /gec /student）
./scripts/dev.sh

# 2. 全量构建
./scripts/build.sh

# 3. 发布（release + 桌面安装包）
./scripts/release.sh
```

### VS Code 调试

`.vscode/launch.json` 内置三组配置（Rust 侧需安装 [CodeLLDB](https://marketplace.visualstudio.com/items?itemName=vadimcn.vscode-codelldb) 扩展，首次打开会提示安装）：

| 配置 | 说明 |
|---|---|
| `api-server (Rust)` | 构建并调试 API 服务（:4801），断点打在 Rust 源码 |
| `web-ui (Vite dev server)` | 内置 node 调试器直接 launch Vite（:4805），断点打在 vite 配置 / 中间件 |
| `desktop-app (Tauri)` | preLaunch 自动构建 + 确保 Vite 在跑（`scripts/ensure-vite.sh`，带 PID 存活检查，进程被回收会自动重新拉起），然后调试桌面端。前端在 Tauri 环境内自动切换为 `invoke()` 直连 Rust 命令（detect/analyze/决策表/explain），**完全离线**，不依赖 api-server；Vite 仅提供页面与 HMR |
| `LangFixer (API + 前端)` | compound：同时调试 API 服务 + Vite |

### 桌面端窗口

- 首次启动窗口居中（`tauri.conf.json` `center: true`），默认 1280×860，最小尺寸 960×640（与前端布局下限一致）；
- 窗口位置 / 大小 / 最大化状态自动持久化（`app_config_dir()/window-state.json`，macOS 为 `~/Library/Application Support/com.langfixer.desktop/`），下次启动恢复；保存位置已离屏（如外接显示器断开）时回退为居中（`desktop-app/src/window_state.rs`）；
- 前端在 Tauri 环境内自动切换桌面自适应布局（`<html class="desktop">`）：应用壳式全高布局（body 不滚动、标签内容区内部滚动）、检测页双独立滚动列（输入列 5:7 比例随窗口伸缩，结果列限宽 900px）、文本输入区加高；
- 「设置」页在桌面端展示进程内运行说明（默认离线），且表单所有项（HanLP 地址 / 离线开关 / LLM 参数 / 决策表与词表目录）均可直接修改：保存后即时生效（热重建 zh/en/ja 检测器 / 重建 LLM 讲解服务 / 重载决策表），并持久化到本地设置文件（`GEC_SETTINGS_FILE`，默认 `langfixer-settings.json`，优先级高于环境变量，重启后仍生效）。

### 环境变量

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
| `GEC_L3_ADOPT_COLLECT` | `1`（默认开启） | **采纳/放弃数据资产沉淀**开关：采纳 → `l3_data/<lang>/adopted.jsonl`（用户确认的句级 `src→dst` 平行语料）；放弃 → `rejected.jsonl`（片段级负偏好）。默认开（自用产品定位）；**多租户 / 隐私敏感部署显式 `0` 或 `false` 关闭**（检测历史 `corrected_text` 列不受此开关影响，属产品功能） |
| `LLM_BASE_URL` | `https://api.openai.com/v1` | LLM 服务（任意 OpenAI 兼容端点） |
| `LLM_API_KEY` | — | 缺省 = LLM 讲解未启用（`/gec/explain*` 返回 503，前端回退静态建议）且三级响应 level 2/3 的 LLM 兜底不可用（触发时返回 503） |
| `LLM_MODEL` | `gpt-4o-mini` | 模型名——必须是该端点 `/v1/models` 里存在的名字（名字错 = 每次调用 HTTP 400，LLM 层整层白跑） |
| `LLM_TIMEOUT_SECS` | `30` | LLM 请求超时（思考模型 / 整载体长文要放宽：实测整载体检测 60s+，30s 必然超时失败并降级 level 1） |
| `LLM_PROMPTS_DIR` | `prompts` | 提示词模板目录（`{lang}_{cefr}_prompt.json`，改文件即热更新；含 `{lang}_detect` 兜底检测模板） |
| `GEC_TABLES_UPDATE_TOKEN` | — | 决策表热更新鉴权 token；设置后 `PUT /gec/tables/:lang` / `POST /gec/tables/:lang/rollback` 需携带匹配的 `X-Tables-Update-Token` 头（未设置时不鉴权） |
| `GEC_RULE_STATE_FILE` | `langfixer-rule-state.json` | 规则运行时开关持久化文件（`{rule_id: bool}`；启动时加载，`PUT /gec/rules/:lang/toggles` 后写回） |
| `CORS_ORIGINS` | — | 允许的跨域来源（逗号分隔）；未设置时允许任意来源（本地开发） |

> 说明：上表 `HANLP_ENDPOINT` / `GEC_OFFLINE` / `GEC_TABLES_DIR` / `GEC_LEXICONS_DIR` / `LLM_*` 等值也可在 Web 端「设置」界面（`GET/PUT /gec/settings`）或桌面端「设置」界面（Tauri `get_settings` / `update_settings` 命令，与 Web 端同构）直接修改——即时生效（热重建检测器 / 重载模板库与决策表）并持久化到 `GEC_SETTINGS_FILE`，无需依赖环境变量。桌面端离线开关默认开启（`GEC_OFFLINE` 未设置 = 离线），与 api-server 的默认在线相反。

### 端口约定

本仓库自有服务统一使用 **4800+ 空闲段**（避免与常见服务端口冲突；各服务默认仅绑定本机）：

| 端口 | 服务 | 配置来源 |
|---|---|---|
| `4801` | api-server（Axum API） | `PORT`（Docker `EXPOSE 4801`） |
| `4802` | 日文 Sudachi 分词服务 | `SUDACHI_ENDPOINT` |
| `4803` | 统计 GEC 服务（Harper / HF transformers server 类） | `GEC_STAT_ENDPOINT` |
| `4804` | vegaml 决策模型服务（`POST /decide`） | `GEC_DEC_ENDPOINT` / `decision-model` 的 `DEC_PORT` |
| `4805` | Vite dev server（前端开发 + Tauri devUrl） | `web-ui/vite.config.ts` / `tauri.conf.json` |
| `4806` | L3 特征模型服务（`POST /l3-detect`；飞轮：`POST /l3-retrain` / `GET /l3/status`） | `GEC_L3_ENDPOINT` / `feature-model` 的 `L3_PORT` |

外部依赖（不在本仓库、端口不纳入本约定）：HanLP NLP 服务（NarrativeOS，`HANLP_ENDPOINT`，默认 `127.0.0.1:8000`）、LLM 服务（`LLM_BASE_URL`，远端 OpenAI 兼容端点）。

## API

| 方法 | 路径 | 说明 |
|---|---|---|
| POST | `/gec/detect` | `{lang, text, level?}` → 错误列表（`level` 三级响应：1 缺省 / 2 弃权 LLM 兜底 / 3 强制 LLM；详见「三级响应」） |
| POST | `/gec/attribute` | 归因上下文 → 归因结果（含 CEFR 分级；提供 `grammar_point` 时知识库富化点级补救/实例 + 知识源引用 `sources[]`） |
| POST | `/gec/analyze` | `{lang, text, student_id?, level?}` → 检测 + 归因一站式（英文按错误推断知识点，知识库富化；`level` 同 `/gec/detect`） |
| POST | `/gec/explain` | LLM 个性化讲解（按 CEFR 选模板 + 用户画像；可选 `target_language` 指定讲解语言，缺省跟随检测语言）；未配置 LLM 返回 503，客户端回退静态建议 |
| POST | `/gec/explain/stream` | 同上，SSE 流式（`meta` → `token`* → `done`） |
| GET | `/gec/tables` | 决策表元数据列表（真实规则数 + 加载/更新时间） |
| GET | `/gec/tables/:lang` | 获取决策表原始 JDM JSON |
| PUT | `/gec/tables/:lang` | 热更新决策表（校验失败保留旧表；成功后自动存版本） |
| POST | `/gec/tables/:lang/validate` | dry-run 校验（不替换当前表，返回 `{ok, errors}`） |
| POST | `/gec/tables/:lang/try` | 用给定归因上下文对当前内存表试算（编辑器「改行 → 试算」闭环） |
| GET | `/gec/tables/:lang/versions` | 版本历史（新 → 旧，`?limit=` 默认 50） |
| POST | `/gec/tables/:lang/rollback` | 回滚到指定版本（重新走校验 + 原子替换；回滚本身也存新版本） |
| GET | `/gec/knowledge/sources` | 知识源注册表（含关联知识点数统计；目录缺失时为空列表） |
| GET | `/gec/knowledge/points?lang=&source_id=` | 知识点列表（可按知识源过滤，1:N 关联） |
| PUT | `/gec/knowledge/sources` | 热更新知识源注册表（校验失败保留旧数据；成功后写回知识库目录，跨重启存活；与决策表热更新共用 `X-Tables-Update-Token` 鉴权） |
| PUT | `/gec/knowledge/points/:lang` | 热更新某语言知识点集合（同上鉴权与写回） |
| POST | `/gec/knowledge/ingest` | 一键导入知识源：注册源 + 正文分块 + 后台逐块 agent 挖掘（LLM 未配置 / 缺 ingest 模板 → 400） |
| POST | `/gec/knowledge/parse-preview` | 书本文件解析预览（`?filename=`，请求体 = 文件字节 ≤64MB；返回格式/标题/作者/章节/告警，不落任何状态） |
| POST | `/gec/knowledge/ingest/upload` | 文件上传导入（`?filename=&source_id=&lang=&chapters=0,2-6`，请求体 = 文件字节 ≤64MB；解析 → 章节选择 → 章节感知分块 → 后台挖掘） |
| GET | `/gec/knowledge/ingest` | 导入会话摘要列表（含状态 `running` / `mined` / `failed`） |
| GET | `/gec/knowledge/ingest/:id` | 会话详情（进度 / 草稿 / 错误 / 状态 / 规则草稿与派生状态） |
| GET | `/gec/knowledge/ingest/:id/drafts` | 草稿明细（逐条归并预判 `endorse` / `create` / `covered`） |
| POST | `/gec/knowledge/ingest/:id/derive-rules` | 开启归因规则派生（知识点挖掘完成后；按 error_type 分批跑内置 agent，草稿只进会话） |
| GET | `/gec/knowledge/ingest/:id/rules-plan` | 规则归并预览（新增 / 本源替换 / 冲突跳过，不落库） |
| GET | `/gec/knowledge/ingest/:id/plan` | 发布预览（归并计划，不落库） |
| POST | `/gec/knowledge/ingest/:id/publish` | 人工一键发布（知识库热更新 + 规则草稿一并写入决策表，预检失败整体中止；同上鉴权；改错对汇入建议队列） |
| POST | `/gec/knowledge/ingest/:id/discard` | 放弃会话（丢弃草稿，知识库不受影响） |
| GET | `/gec/knowledge/dataset/:id` | 资源包盘点（点数 / 规则行数 / 决策数据 / 草稿清单；删除前预览） |
| POST | `/gec/knowledge/dataset/:id/export` | 打包导出资源包（`?format=zip` 返回 zip 压缩包附件；知识数据 + 决策表快照 + 决策数据 + 草稿，自描述 JSON 含校验和） |
| POST | `/gec/knowledge/dataset/import` | 导入资源包（请求体自适应 zip 压缩包 / 裸 JSON；校验通过后知识库热更新 + 决策表按 `_meta` 合并 + 决策数据写回 + 草稿恢复；同上鉴权） |
| POST | `/gec/knowledge/dataset/:id/clean` | 删除资源包（知识库 + 决策表该源规则行 + 决策数据条目级摘除该源条目 + 草稿；`?keep_orphan_points=1` 保留孤儿点；同上鉴权） |
| GET | `/gec/rules?lang=` | 规则列表（`enabled` 为运行时状态：持久化文件 > 词表默认 > 启用） |
| PUT | `/gec/rules/:id/toggle?lang=&enabled=` | 单条规则开关（持久化） |
| PUT | `/gec/rules/:lang/toggles` | 批量规则开关 `{toggles: {id: bool}}`（持久化，重启不丢） |
| GET | `/student/:id/profile` | 用户画像 |
| POST | `/student/:id/record` | 记录用户犯错历史 |
| GET | `/gec/settings` | 运行时设置（HanLP 地址 + 离线开关 + 决策表 / 词表目录 + LLM 参数 + LLM 请求模式 + vegaml 决策层参数；模式固定：前端流式 / 外部 API 非流式） |
| PUT | `/gec/settings` | 更新设置（部分更新；即时生效：LLM 参数热更新讲解服务，`prompts_dir` 变更重载模板库，HanLP 地址 / 离线开关 / 词表目录 / 决策层 / L3 / `knowledge_enabled` 知识层开关变更热重建 zh/en/ja 检测器，决策表目录变更重载决策表；持久化到设置文件） |

## 三级响应（level 1 / 2 / 3）

检测接口（`/gec/detect`、`/gec/:lang/detect`、`/gec/analyze`）支持 `level` 参数（缺省 1），在前述「规则 → 决策」级联之上按需触发 LLM 兜底层：

| level | 行为 | LLM 触发条件 |
|---|---|---|
| 1（缺省） | 规则（+ 决策模型，若显式启用） | 不触发 |
| 2 | 1 级 + **L3 特征模型门控**（若启用；L3 是唯一的 pre-LLM 门控） | L3 **弃权**（灰区 `[0.25, GEC_L3_ABSTAIN_THRESHOLD)`）或**服务不可用**时触发；L3 高置信（有错 / 明确无错）时直接定案，**不调用 LLM**；L3 未启用时回退旧语义：决策模型**明确弃权**（`abstain: true`，或无 `abstain` 字段时概率落在灰区 `[0.25, abstain_threshold)`） |
| 3 | 1 级 + **L3 特征模型**（若启用）+ LLM 兜底 | **无条件**触发（「漏报」兜底）；特征模型高置信时直接定案，**不调用 LLM**（LLM 收缩到纯长尾） |

- LLM 兜底引擎按请求从运行时 LLM 参数 + 检测模板（`prompts/{lang}_detect_prompt.json`）现构建，支持设置热更新；
- LLM 产出的错误标记 `source: "llm"`（前端错误列表展示「LLM 兜底」标签）；特征模型产出的错误标记 `source: "feature"`（前端展示「特征模型」标签，含模式化定位 + 机械修正）；
- LLM 结果**不进 LRU 缓存**（非确定性 + 成本高）；level 1 结果照常缓存；
- 触发条件命中但 LLM 未配置（无 API Key / 缺检测模板）→ **503**（提示去设置界面配置）；LLM 服务调用失败 → **502**；
- 前端：编辑器提供「响应级别」选择器（L1/L2/L3）与「漏报检测」按钮（= level 3）。
- **训练飞轮（闭环）**：确认采纳的 LLM 检测结果（正样本）与 LLM 复查确认干净的句子（负样本）
  持续汇入 `feature-model/l3_data/<lang>/confirmed.jsonl`（`POST /gec/l3/collect`；桌面端 `l3_collect` 命令）；
  积累到阈值（新增 ≥ 10 / 首次 ≥ 30，冷却 7 天）后设置界面「L3 训练飞轮」卡片提示重训练
  （`POST /gec/l3/retrain` → feature-model `POST /l3-retrain`：合并样本重训练 + dev 评测，
  F1 未显著退化则**热切换**模型，无需重启）——特征模型随使用变强，LLM 长尾持续收缩。
- **采纳资产沉淀（数据资产管线）**：检测时同步写入「应用全部建议后」的纠正版
  （`detect_history.corrected_text` 列，产品级历史恢复能力）；用户**采纳**时前端送
  原文 + 累积修正全文，服务端切句对齐沉淀句级平行对 `adopted.jsonl`（去重、编辑密度
  >0.4 打 `weak`、上限 20000）；用户**放弃**时沉淀片段级负偏好 `rejected.jsonl`。
  训练侧用 `feature-model/prepare_adopted.py` 做治理（去 weak / 去基准污染 / md5 稳定
  holdout 切分）。开关 `GEC_L3_ADOPT_COLLECT`（默认开，多租户置 `0`）。

## 测试

```bash
cargo test --workspace   # 各 crate 单元/集成测试
cd web-ui && npm test    # 前端单元测试（vitest：JDM 解析/序列化/diff）
cd web-ui && npm run typecheck && npm run build
```

## 路线图

当前实现聚焦**语法层**（检测 + 归因 + 决策表热更新）。后续增强方向：

1. **LLM 扩展**（✅ 已完成）：`langfixer-llm` crate（OpenAI 兼容 API + SSE 流式 + 提示词版本管理），
   将决策表产出的静态补救措施升级为按用户画像（CEFR 水平/母语/历史错误分布）生成的个性化讲解与练习；
   未配置 `LLM_API_KEY` 时 API 返回 503 / 桌面端报错，前端自动回退决策表静态建议。
2. **统计 GEC 模型**（✅ 挂载点就位）：`gec-core` 引擎路由抽象（`GecEngine` / `GecEngineKind`，
   rule / statistical / llm 按语言经 `GEC_ENGINE_{LANG}` 分发）+ `StatisticalGecEngine` 统计服务客户端
   （Harper / HuggingFace 序列模型，`POST /gec`）；统计引擎为主、规则引擎兜底合并（按 span 去重），
   突破纯规则引擎的召回率上限——接入真实模型服务只需配置 `GEC_ENGINE_*` + `GEC_STAT_*`。
3. **vegaml 决策模型层**（✅ 挂载点就位）：`gec-core` 新增 `DecisionGecEngine`（`POST /decide`
   客户端）+ `GecEngineKind::Decision`，`compose_detector` 在启用决策层时以「规则 + 决策」组合
   （`CompositeDetector`，span 去重合并）；vegaml 对规则未命中句子做判断——检出错误合成错误项
   （`collocation`→COLLOCATION、`word_order`→WORD_ORDER、其余→GRAMMAR），弃权（prob 低于
   `GEC_DEC_ABSTAIN_THRESHOLD`）放行，服务不可用自动回退纯规则。Python 侧 `decision-model/`
   提供 FastAPI 服务（`VEGAML_MOCK=1` 离线 mock）+ 微调工作流（extract/train/eval，
   复用 `datasets/` 的 JFLEG / MuCGEC gold 数据）。
4. **持久化**：`StudentStore` 从内存实现迁移到 SQLite（api-server 与桌面端共用）。
5. **游戏化层**（后续加强）：区域进阶、考试副本、成长系统，依赖 LLM 出题 + CEFR 难度分级 +
   用户进度持久化就位后启动。
6. **ChineseErrorCorrector 吸收**（规划中）：CEC（ACL 2026）六项设计的落地方案
   （prompt 教学分类 / 混淆字集 / 评测基准矩阵 / L3 大规模重训练 / 专用模型 / 数据增强），
   见 [CEC_ADOPTION.md](CEC_ADOPTION.md)；数据清单见 [datasets/cec_datasets.md](datasets/cec_datasets.md)。
7. **多模态审校**（P0 图片·结构层档 + P1 图片·视觉档已落地）：输入面从纯文本扩展到图片 / PDF / Word——
   外部协作工具（PhotoCraft / PdfCraft / WordCraft CLI）承担「打开载体 + 回写标注」两端，
   检测内核（`GECDetector::detect(&str)`，`Error.span` 仍是字节偏移）保持不变，
   外面加「文本单元投影 + 偏移回映射」层（新 crate [`media-core/`](media-core)：
   `TextUnit` / `Region` / `Stitched` / `Annotation` + `stitch`/`remap` 纯函数 + 载体适配器；
   编排层 [`media-flow/`](media-flow)：job 状态机 + 抽取档位 + 回写 + 正误表，
   api-server 与桌面端都只是宿主，各自注入自己的检测级联）。
   默认回写模式 = 叠加标注（不改原文）；每个 job 独立目录，**永不改原件**。
   后端端点：`POST /gec/media/probes` · `POST /gec/media/inspect` ·
   `POST|GET /gec/media/jobs` · `GET /gec/media/jobs/:id` · `GET /gec/media/jobs/:id/preview` ·
   `POST /gec/media/jobs/:id/apply` · `POST /gec/media/report` ·
   `GET /gec/media/reports/:id?format=json|md|html|docx`（开关 `GEC_MEDIA_ENABLED=1`，默认关闭）。
   **A 档（结构层）取不到文本时自动降级 B 档**：多模态 LLM 逐字转录（`GEC_MEDIA_VISION=1` +
   `GEC_MEDIA_VISION_MODEL`，提示词 `prompts/media_vision_prompt.json`），
   bbox 尺度自动判定，低置信度 / 无定位一律 `unlocated`（只进列表不画框）。
   **送端点前先压缩**：默认降到长边 2000px / 1.5MB / JPEG q80（`GEC_MEDIA_VISION_MAX_EDGE` /
   `GEC_MEDIA_VISION_MAX_BYTES` / `GEC_MEDIA_VISION_JPEG_QUALITY`），实测 4.9MB 实拍图短请求
   108s→2.6s、整载体转录由「必 503」变首轮即成功；bbox 是归一化坐标，压缩不影响回写定位，
   压了多少会写进 job 告警。
   验收金集：`datasets/media_gold.json`（专家正误表 15 项，`scripts/media_gold_extract.sh` 抽取），
   打分管道 `media_core::gold::score`，评测 harness `api-server/tests/media_gold_eval.rs`：

   ```bash
   # 上限评测（离线零凭据）：gold 原文当「完美抽取」喂进检测级联
   cargo test -p api-server --test media_gold_eval -- --nocapture
   # 真实端到端评测：原图 → 视觉抽取 → 检测 → 打分
   LANGFIXER_MEDIA_DIR=<原图目录> cargo test -p api-server --test media_gold_eval -- --ignored --nocapture
   ```

   实测：本地规则层对这份金集**召回 0%**（正对照稳定检出 → 管线没问题）——
   瓶颈在中文检测覆盖（`ZH_CONFUSABLE` 出厂关闭 + 词典缺条目），不在抽取。

   **重演 → 勾选 → 输出正误表**（把软件推到「能产出与专家同构的验收物」这一步）：

   ```bash
   ./scripts/media_expert_replay.sh replay    # ① 推演：载体 → candidates.json（含溯源与命中的专家条目号）
   $EDITOR target/media-replay/candidates.json # ② 勾选：checked=false 剔除，各列内容可改
   ./scripts/media_expert_replay.sh export    # ③ 导出：report.docx / .md / .json / .html
   ```

   导出表的 6 列与专家正误表一致（`序号 | 页码及位置 | 错误或待核原文 | 正确或处理建议 |
   计错数 | 备注及判定依据`），且能被 `gold::parse_gold_rows` 原样读回——
   「重演出的表」与「评测召回」是同一把尺子；Word 由 `wordcraft-cli convert` 生成，
   缺该 CLI 时降级为告警而不是谎报。设计与实测见 MULTIMODAL.md §18。

   **界面（web-ui 顶层导航「载体审校」，深链 `?view=media`）**：探测卡片（PhotoCraft 版本 /
   视觉模型 / 压缩口径 `≤2000px ≤1.5MB JPEG q80` / 未启用原因与补法）→ 选图（**串行**：视觉档
   共用一个网关，并发只会互相排队并 503）→ 阶段进度 + 全部告警（压缩、重试、跨单元丢弃、
   LLM 降级、抽取为空）→ 预览画布按 `annotations.json` 落点画框（点框 ↔ 点行联动；
   `unlocated` 只进列表，**不画假框**）→ 勾选 / 计错数 / 改写建议 → 回写（默认叠加标注，
   扁平位图上「改原文」不可用）→ 导出正误表（docx / html / md / json 下载）。
   开启方式：设置页「载体审校（图片 / PDF / Word）」卡片打开总开关与抽取档位，或在设置文件里加
   `"media": { "enabled": true, "use_vision": true }` 后重启服务
   ——设置文件存在时 `GEC_MEDIA_*` 环境变量会整体失效（见 MULTIMODAL.md §8）。
   **桌面端同一条链路**：`media-flow` 的编排 + 桌面端本地检测级联，命令面 `media_probes` /
   `media_inspect` / `media_create_job` / `media_jobs` / `media_job` / `media_preview` /
   `media_apply` / `media_report` / `media_report_file`（字节走 base64 JSON，导出走原生「另存为」）。

   设计与已实测的命令契约见 [MULTIMODAL.md](MULTIMODAL.md)（§15 金集 / §16 P0 / §17 P1 / §18 重演导出），
   可执行契约基线：`./scripts/media_smoke.sh`（图片）· `./scripts/media_smoke_p2.sh`（PDF + Word）；
   回归测试：`media-core/tests/gold_dataset.rs` · `media-flow/tests/lifecycle.rs`（两端共用的全链路冒烟：
   mock 检测级联 + mock 视觉网关）· `api-server/tests/media_api.rs` ·
   `api-server/tests/media_vision_api.rs` · `api-server/tests/media_report_api.rs`。

## 许可

- **代码与内置资产**（`decision-tables/`、`lexicons/`、`prompts/` 等产品自带内容）：
  [Apache-2.0](LICENSE)（全文见仓库根目录 `LICENSE`）。
- **第三方依赖**：许可清单见 [LICENSES-THIRD-PARTY.md](LICENSES-THIRD-PARTY.md)
  （按 `Cargo.lock` / `package-lock.json` 生成，均为 MIT / Apache-2.0 / BSD / ISC 等
  与 Apache-2.0 兼容的宽松许可）。
- **外部语料**（`datasets/`）：**第三方研究语料**，保留各自原始许可（JFLEG 为
  非商业研究语料、MuCGEC 为 Apache-2.0、CSCD / NaCGEC / CTCDataset 衍生集为研究
  用途等），**不在本项目 Apache-2.0 授权范围内**；仅用于本地评测，产品发行物不含
  本目录，逐集许可清单见 [datasets/README.md](datasets/README.md)。
- **知识源内容**（`knowledge-base/`）：仓库内置的唯一知识源是**自撰示例源**
  `src-sample-en`（LangFixer 示例英语语法指南，234 个示例知识点），随仓库以
  Apache-2.0 授权分发。用户经导入管线摄取的书籍属**用户数据**：权利归原权利人，
  本仓库不授予任何教材内容许可；`license_scope != public` 的源不得随发行物分发
  （机制见上文「IP 边界」）。

