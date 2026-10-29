# 更新日志

本项目遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/) 格式与语义化版本。

## [Unreleased]

### 新增

- **能力位角色门控（Phase 1）**：前端 `lib/caps` 能力位（learning / proofreading / ops，本机持久化）门控顶层导航与设置卡片——载体审校 = 编校扩展、规则管理 / 知识维护 / 进化中心 / 高级设置 = 平台运营扩展；web 形态新增 `POST /gec/ops/unlock`（与决策表热更新共用 `GEC_TABLES_UPDATE_TOKEN` 鉴权）解锁运营扩展；深链打开未解锁视图时展示一次性说明卡而非硬跳转；首次引导增加「我的工作场景」选择
- **学习者档案（Phase 1）**：会话画像新增母语与自评 CEFR（`PUT /gec/sessions/:id/profile`，桌面端 `update_session_profile` 同构）；设置页「我的档案」卡片；LLM 讲解 prompt 注入画像母语（修正此前把会话检测语言误当母语注入的问题），讲解模板缺省 CEFR 档位取画像自评（请求显式指定 > 画像自评 > A1）；「练习与复习」占位视图（学习扩展入口）
- **载体审校（图片 / PDF / Word）**：`media-core` / `media-flow` 内核与编排层；api-server HTTP 面 + 桌面端同构命令；web-ui「载体审校」工作台（探测 → 抽取 → 检测 → 落点画框 → 勾选 → 回写标注 → 正误表导出 docx / html / md / json）
- **载体审校简单 / 高级模式**：模式门（`mode_gate`）与简单模式文档解析；PdfCraft / WordCraft 高级模式适配器；视觉档送端点前压缩
- **反思中心接入金集闭卷证据**：载体金集漏报 → 反思中心可执行靶子（rule_tune / L3 样本 / LLM 层分类）
- **gec-zh 新规则**：12/24 小时制混用、「名称后冒号」；出厂词表 `ZH_CONFUSABLE` / `ZH_MISSING_SUBJECT` 改为出厂开启
- **LLM 网关重试退避与预算 + 思考块 / 答案分离**

### 变更

- 检测页「响应级别」（L1/L2/L3）改为用户语言「检查模式」：快速检查 / 标准检查 / 深度扫描；「漏报检测」按钮更名「深度重查」（内部 `level` 参数与 API 契约不变）
- `GEC_L3_ADOPT_COLLECT`（采纳 / 放弃样本沉淀）在 **api-server 形态默认改为关闭**——共享部署不默认把用户数据用于训练，显式 `=1` 开启；桌面端保持默认开（本机单用户）
- 设置页 LLM 卡片与首次引导增加数据流向告知（检测文本会发送到所配置的模型端点）
- README 顶部新增「这是什么 / 给谁用 / 能做什么 / 做不到什么」产品叙事；完整环境变量矩阵迁至 `docs/OPERATIONS.md`

### 移除

- `/student/:id/*` 遗留路由（与 `/gec/sessions/:id/*` 重复，前端从未调用）及 `routes/student.rs` 孤儿文件

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
