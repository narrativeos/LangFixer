# 更新日志

本项目遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/) 格式与语义化版本。

## [Unreleased]

### 新增

- **交互切片 B（P1，见 `docs/UX_REVIEW.md`）**：复习评分从两键升级为 **FSRS 四键**——忘了 / 有点难 / 记住了 / 很轻松（`again/hard/good/easy`，间隔粒度直接反映自评；API `rating` 字段优先、旧 `passed` 契约保持兼容，桌面命令同构）；复习节奏可视化——「今日已复习 x/10」进度 + 队列清空后的完成收尾态（本地按日计数）；复习**5 秒撤销**——`review_undo` 快照表存每次复习前的记忆状态（每卡仅最近一次），`POST /gec/mistakes/:gid/review/undo` 与 `POST /gec/sessions/:id/mistakes/:mid/review/undo`（桌面 `undo_global_review` / `undo_review_mistake`）还原排期/状态/稳定性/难度，撤销后卡片重回到期队列；错题本浏览控制——待复习 / 全部 / 已掌握页签 + 关键词搜索 + 错误类型折叠
- **交互切片 A（P0，见 `docs/UX_REVIEW.md`）**：导航「练习与复习」显示今日到期复习徽章；采纳成功提示补「已进入错题本」（批量采纳汇总一次，不再逐条弹）；复习队列改按**到期最久优先**（新卡其次，错题本列表保持最近出现浏览序）；复习卡挖空修复——`original` 不在句中时不再整句展示泄露答案，改为提示文案 + 揭晓后补原句
- **全局错题本（跨会话，学习闭环功能闭合）**：错题本从会话级升级为跨会话全局视图——`GET /gec/mistakes`（按 语言+类型+原文+修正 聚合全部会话，带出现次数与 `sessions_seen` 出现会话数）、`GET /gec/review`（全局到期复习队列）、`POST /gec/mistakes/:gid/review`（FSRS 复习）、`GET /gec/analysis`（全局统一错误画像，确认错误基准）；桌面端同构命令 `global_mistakes / global_review_queue / review_global_mistake / global_analysis`。设计：错题本体单一事实源（`mistakes` 表跨会话投影，无聚合表无双写无回填迁移），FSRS 记忆状态按错题键挂 `global_mastery` 表——**跨会话共享、删除会话不丢，同一错误日后重现记忆复活**；「练习与复习」面板切到全局错题本（不再依赖当前会话，展示「来自 N 个会话」标记）；会话级错题端点保留（契约不变）
- **评测基准与 CI 回归门（P0-1）**：新 workspace crate `tools/gec-eval`——对四份金集（CSCD+SIGHAN/EC-Med/EC-LAW、MuCGEC、NaCGEC、JFLEG）逐句跑规则层（离线确定性，无 LLM），产出 precision / recall / F0.5（GEC 惯例 β=0.5）与每千字符延迟中位数（release）；`--json` 写机器可读报告，`--check <baseline> [--tolerance]` 与基线对比、任一数据集 P/R/F0.5 下降超容差（默认 0.02）退出码 1；基线固化在 `datasets/eval_reports/baseline_rules.json`，CI 新增回归门步骤；README 新增「质量评测」小节（实测表 + 解读 + 定性定位对照）
- **统一错误画像（Phase 3）**：学生错题本 / 间隔复习与审校侧「作者习惯性错误」合并为同一分析模型（`shared-model::profile::ErrorProfile`）——同一主体（会话）随时间沉淀的错误模式的统一投影：类型分布 / 习惯性（主导，占比 ≥ 0.3）错误 + 示例 / 高频改错对 / 掌握度摘要。`GET /gec/sessions/:id/analysis`（桌面端 `session_analysis`）改为该投影：错题本非空时以**确认错误**为基准（`basis = confirmed`，误报与未采纳建议不再污染画像），旧会话回退检测记录（`basis = detected`，向后兼容）；「练习与复习」错题类型按同一口径标记「习惯性」
- **间隔复习与掌握度（Phase 2 第二片）**：错题本新增 `mistake_mastery` 表（`new / reviewing / mastered` + 下次复习调度：连对间隔 1/3/7/14/30 天、连对 6 次 = 已掌握；没记住清零次日再练）；新增 `GET /gec/sessions/:id/review` 与 `POST /gec/sessions/:id/mistakes/:mid/review`（桌面端 `session_review_queue` / `review_mistake`）；「练习与复习」新增「今日复习」练习卡（句子挖空 → 显示答案 → 记住 / 没记住）与掌握度徽章、汇总行
- **错题本（Phase 2 第一片）**：检测反馈「采纳」带会话时自动沉淀进会话错题本（SQLite `mistakes` 表；同类型 + 同原文 + 同修正聚合计数，句级上下文按错误 span 从原文提取）；新增 `GET /gec/sessions/:id/mistakes`（桌面端 `session_mistakes` 命令）；「练习与复习」视图从占位升级为错题本（按类型分组 + 计数 + 修正对照）
- **会话持久化（Phase 1.5）**：会话与学习者画像（母语 / 自评 CEFR）、犯错流水从内存实现迁移到 SQLite（`shared-model` 新增 feature-gated `SqliteSessionStore`，api-server / 桌面端与 detect_history 共用同一 db 文件），重启不丢档案；内存版 `SessionStore` 移除
- **能力位角色门控（Phase 1）**：前端 `lib/caps` 能力位（learning / proofreading / ops，本机持久化）门控顶层导航与设置卡片——载体审校 = 编校扩展、规则管理 / 知识维护 / 进化中心 / 高级设置 = 平台运营扩展；web 形态新增 `POST /gec/ops/unlock`（与决策表热更新共用 `GEC_TABLES_UPDATE_TOKEN` 鉴权）解锁运营扩展；深链打开未解锁视图时展示一次性说明卡而非硬跳转；首次引导增加「我的工作场景」选择
- **学习者档案（Phase 1）**：会话画像新增母语与自评 CEFR（`PUT /gec/sessions/:id/profile`，桌面端 `update_session_profile` 同构）；设置页「我的档案」卡片；LLM 讲解 prompt 注入画像母语（修正此前把会话检测语言误当母语注入的问题），讲解模板缺省 CEFR 档位取画像自评（请求显式指定 > 画像自评 > A1）；「练习与复习」占位视图（学习扩展入口）
- **载体审校（图片 / PDF / Word）**：`media-core` / `media-flow` 内核与编排层；api-server HTTP 面 + 桌面端同构命令；web-ui「载体审校」工作台（探测 → 抽取 → 检测 → 落点画框 → 勾选 → 回写标注 → 正误表导出 docx / html / md / json）
- **载体审校简单 / 高级模式**：模式门（`mode_gate`）与简单模式文档解析；PdfCraft / WordCraft 高级模式适配器；视觉档送端点前压缩
- **反思中心接入金集闭卷证据**：载体金集漏报 → 反思中心可执行靶子（rule_tune / L3 样本 / LLM 层分类）
- **gec-zh 新规则**：12/24 小时制混用、「名称后冒号」；出厂词表 `ZH_CONFUSABLE` / `ZH_MISSING_SUBJECT` 改为出厂开启
- **LLM 网关重试退避与预算 + 思考块 / 答案分离**

### 变更

- **复习调度升级为 FSRS（P0 对标成熟度）**：错题本间隔复习从手搓「连对 1/3/7/14/30 天阶梯 + 连对 6 次 = 已掌握」升级为 **FSRS 算法**（`fsrs-rs`，Anki 内置同款；基于 DSR 记忆模型的稳定性/难度双参数调度，目标保留率 0.9）：记住 → Good 按钮按记忆状态计算个性化递增间隔，下次间隔 ≥30 天 = 已掌握；没记住 → Again 按钮、间隔钳到次日。`mistake_mastery` 新增 `stability / difficulty` 记忆状态列（旧库打开自动增列迁移），旧「连对阶梯」数据经 SM-2 近似自动迁移（Anki 同款迁移路径）；API / 桌面命令 / UI 契约不变
- 检测页「响应级别」（L1/L2/L3）改为用户语言「检查模式」：快速检查 / 标准检查 / 深度扫描；「漏报检测」按钮更名「深度重查」（内部 `level` 参数与 API 契约不变）
- `GEC_L3_ADOPT_COLLECT`（采纳 / 放弃样本沉淀）在 **api-server 形态默认改为关闭**——共享部署不默认把用户数据用于训练，显式 `=1` 开启；桌面端保持默认开（本机单用户）
- 设置页 LLM 卡片与首次引导增加数据流向告知（检测文本会发送到所配置的模型端点）
- README 顶部新增「这是什么 / 给谁用 / 能做什么 / 做不到什么」产品叙事；完整环境变量矩阵迁至 `docs/OPERATIONS.md`

### 移除

- `/student/:id/*` 遗留路由（与 `/gec/sessions/:id/*` 重复，前端从未调用）及 `routes/student.rs` 孤儿文件
- `SessionPanel.vue` 孤儿组件（「会话分析」独立卡片从未被任何视图引用；习惯性错误分析并入统一错误画像，入口收敛到「练习与复习」与 `GET /gec/sessions/:id/analysis`）

## [v0.1.0-preview] - 2026-10-23

首个公开预览版。

### 新增

- **英文 GEC 检测与归因**：规则引擎（`gec-en`）+ 韦纳归因三维标注（能力/努力/可控性），归因决策表（zh/en/ja，JDM 格式，zen-engine 执行）
- **知识库与知识源体系**：知识点（point）建模与静态归点；内置自撰示例源 `src-sample-en`（Apache-2.0）；书籍经文档解析 + 摄取管线导入（`document-parse` + `attribution-engine/ingest`）
- **知识源资源包**：盘点 / zip 导出 / 导入 / 删除，两端同构；`license_scope` IP 边界
- **个性化补救**：错误组聚合、补救练习与复习闭环（lexicon-store 训练中心 / 优化轮次）
- **LLM 长尾兜底**：OpenAI 兼容端点可配置（不内置任何 Key），L3 弃权 → LLM 级联
- **形态**：`api-server`（REST）+ `web-ui`（Vue 3）+ `desktop-app`（Tauri）
- **许可与合规**：Apache-2.0（LICENSE/NOTICE），第三方许可清单（LICENSES-THIRD-PARTY.md）

### 已知限制（预览）

- 未做安全审计；`api-server` 应部署在认证层之后
- 数据格式（决策表 / 知识库 / 资源包）可能随版本演进，暂不提供迁移工具
- 中文/日文（gec-zh / gec-ja）能力为早期阶段

[unreleased]: https://github.com/narrativeos/LangFixer/compare/v0.1.0-preview...HEAD
[v0.1.0-preview]: https://github.com/narrativeos/LangFixer/releases/tag/v0.1.0-preview
