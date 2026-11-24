# 规则与知识库设计

本文档自 README「项目结构」迁出，描述规则与知识的分离架构、知识源导入管线、
文档解析与归因规则派生、知识源资源包的完整机制。产品总览与快速上手见
[../README.md](../README.md)。

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
