# API 参考

自 README 迁出的完整 REST API 端点表。产品总览见 [../README.md](../README.md)，
`level` 三级响应机制见 README「三级响应（level 1 / 2 / 3）」，
缓存与进度机制见 [OPERATIONS.md](OPERATIONS.md)。

| 方法 | 路径 | 说明 |
|---|---|---|
| POST | `/gec/detect` | `{lang, text, level?, refresh?}` → 错误列表（`level` 三级响应：1 缺省 / 2 弃权 LLM 兜底 / 3 强制 LLM；详见 README「三级响应」；`refresh: true` 忽略缓存强制实检，见 OPERATIONS.md「检测缓存与纠正」；可选请求头 `X-Progress-Id` 启用分阶段进度，见下） |
| GET | `/gec/detect/progress?id=` | 检测阶段事件快照 `{found, finished, events[]}`（配合 `X-Progress-Id`：引擎各层推进事件经此轮询；缓存 / 历史命中无事件直接 `finished=true`；机制见 OPERATIONS.md「检测阶段进度」） |
| POST | `/gec/attribute` | 归因上下文 → 归因结果（含 CEFR 分级；提供 `grammar_point` 时知识库富化点级补救/实例 + 知识源引用 `sources[]`） |
| POST | `/gec/analyze` | `{lang, text, student_id?, level?, refresh?}` → 检测 + 归因一站式（英文按错误推断知识点，知识库富化；`level` / `refresh` 同 `/gec/detect`） |
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
| POST | `/gec/cache/clear` | 清空检测缓存 `{history?}`（LRU 必清；`history: true` 连检测历史一并清除；用于规则/模型变更后旧结果无法自动失效时的手动纠正，见 OPERATIONS.md「检测缓存与纠正」） |
| GET | `/student/:id/profile` | 用户画像 |
| POST | `/student/:id/record` | 记录用户犯错历史 |
| GET | `/gec/settings` | 运行时设置（HanLP 地址 + 离线开关 + 决策表 / 词表目录 + LLM 参数 + LLM 请求模式 + vegaml 决策层参数；模式固定：前端流式 / 外部 API 非流式） |
| PUT | `/gec/settings` | 更新设置（部分更新；即时生效：LLM 参数热更新讲解服务，`prompts_dir` 变更重载模板库，HanLP 地址 / 离线开关 / 词表目录 / 决策层 / L3 / `knowledge_enabled` 知识层开关变更热重建 zh/en/ja 检测器，决策表目录变更重载决策表；持久化到设置文件） |

多模态载体审校端点（`/gec/media/*`，默认关闭）见 [../MULTIMODAL.md](../MULTIMODAL.md)。
