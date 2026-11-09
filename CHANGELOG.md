# 更新日志

本项目遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/) 格式与语义化版本。

## [Unreleased]

## [v1.1.0] - 2026-11-08

桌面端从「能跑」走到「像一台 Mac 应用」：自动更新按发布源烧入更新端点，原生菜单栏 /
右键菜单 / Finder 拖拽落地，更新流程改为先征同意；用户面信息减负——检测层级的人话词表
统一到一张表并收拢三处漂移，引擎细节走渐进披露；编辑器补「试一试」示例文本降低首次使用门槛。

### 新增

- **桌面端自动更新**：编译期按发布源烧入更新端点（`LANGFIXER_RELEASE_SOURCE` = github / gitea / atomgit，默认 github），启动 30 秒后与此后每 6 小时检查一次；macOS / Linux 静默下载安装完成后提示重启（「已装好待重启」版本落盘，冷启动不重复下载与弹框），Windows 先征得同意再下载安装（安装器拉起即退出进程）。配套发布链：`scripts/release.sh` 按源分别打包并断言双源版本号一致、`make_latest_json.py` 由签名生成 `latest.json`、`publish.sh` 多源推送单源失败不中断；前端不调用 updater IPC，故未授予 `updater:default` 权限
- **macOS 原生集成（`docs/desktop-hig.md`）**：原生菜单栏 App / File / Edit / View / Window / Help 全量带快捷键（⌘, 设置、⌘1–7 视图切换、⌘O 导入、⇧⌘E 导出、⌥⌘Z 撤销应用修正、⌥⌘T 主题），经 `menu-action` 事件转发前端路由；检测历史行 / 错误条目 / 载体任务页签三处原生右键菜单；Finder 拖拽导入（按路径读盘，不走 base64）；窗口背景随系统外观 + 手动主题同步（消除深色模式启动闪白）
- **应用级「撤销全部采纳」**：批量采纳后在非编辑区按 ⌥⌘Z（或菜单项）可整批还原为采纳前文本（`store.applySnapshot` 快照）
- **用户面信息减负（渐进披露）**：检测层级的人话词表统一到 `lib/labels.ts`（本地检查 / 快速定案 / 知识查询 / AI 复查），修复结果面板 / 历史 / 载体工作台三处词表各自漂移；主界面只留人话摘要，①②.5③ 层编号与引擎细节收进「详情」popover / tooltip；错误来源徽章、把握程度（→「把握大 / 中 / 小」）、共识文案、修复指引、设置页卡片名与引导文案同表统一；桌面形态的检测管线可用性收进顶栏紧凑 chip（● N/4 项可用，全视图可见），Web 形态保留整行状态条
- **编辑器「试一试」示例文本**：待检测文本标题行右侧提供三个短句示例（错别字 / 多字少字 / 语序颠倒），点击即填入并立即检测，降低首次使用的空白页成本；覆盖非空原文前存快照，⌥⌘Z 可找回原文
- **会话删除入口（C4，`docs/UX_REVIEW.md`）**：设置页「我的档案」新增「删除当前会话」——确认框明示「将移除该会话的 N 条错题；你的复习进度会保留」（N 为该会话错题本实际条数）；确认后级联移除会话 / 犯错流水 / 错题本与会话级掌握度 / 撤销快照 / 检测历史，**全局复习进度（FSRS 记忆）保留**，同一错误日后重现记忆复活；桌面端 `delete_session` 命令同构；删除后当前会话 ID 清空（下次检测自动新建会话）

### 变更

- 桌面端更新流程改为**先征同意**：所有平台先弹框确认再下载 / 安装，「稍后再说」落盘后不再反复打扰，手动检查更新走帮助菜单（macOS HIG「更新须用户知情选择」）
- 桌面端开发启动修复：`beforeDevCommand` 未正确拉起 Vite（端口 4805）导致 dev 加载失败

## [v1.0.0] - 2026-11-03

首个正式版本（前作 v0.1.0-preview）。质量门收拢：clippy 全量清零并转 CI `-D warnings`
阻断、评测基线回归门、vegaml 决策层退役出产品路径；学习闭环闭合（全局错题本 + FSRS
四键复习 + 撤销 + 学习统计）与交互切片 A–D 全部落地。

### 新增

- **交互切片 D（P2，见 `docs/UX_REVIEW.md`）**：学习价值可视化——新增 `learning_events` 事件流水表（采纳 / 复习只增不改，删除会话不清除：学习历史属于全局记忆）与 `GET /gec/stats?days=`（近 N 天逐日采纳/复习计数 + 窗口内评分分布 + 全时累计；桌面 `global_stats` 同构），练习页底部「学习统计 · 近 30 天」堆叠柱图 + 评分分布；采纳价值感——`POST /gec/feedback` 响应新增 `adopt_count`（此类错误累计采纳次数含本次），同类错误再次确认时 toast 显示「第 N 次采纳此类错误」；复习文案 CEFR 适配——复习卡提示语按学习者档案档位变化（A 段具体抓手 / B 段回忆导向 / C 段最小引导，无档案不出提示）
- **交互切片 C（P1，见 `docs/UX_REVIEW.md`）**：画像与术语收拢——练习页卡片标题「错题本 · 跨文稿」显式范围（走查更正：检测页本无画像卡片，「两套画像并存」不成立）；「习惯性」徽章加统计口径 tooltip；术语外化——「画像基于确认错误（误报与未采纳建议不计入）」→「只统计你确认过的错误」、UI 文案去 FSRS 字样；徽章语言统一——「待复习」→「没练过」（与「待复习」到期页签区分，一个概念一个名字）、复习徽章「复习中 · 下次 11-05」。删会话确认文案（C4）暂缓：当前 UI 无删除会话入口，实现时必须说明「错题移除、复习进度保留」
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

- **clippy 存量告警清零（1.0 质量门）**：全 workspace 250 条告警全部消除（机械修复 + `matches!` 化 + doc 列表续行规范化 + 复杂类型别名 + 测试环境守卫 newtype 化 + 有理由的定点 `#[allow]`）；CI clippy 步骤由「不阻断」改为 `-D warnings` 阻断门
- **vegaml 决策模型层退役出产品路径**（EVOLUTION.md §13 #10 判决执行）：设置界面「决策模型（vegaml）」卡片与「启用决策层」开关、系统状态条 ② 层、服务总览行移除；管线提示条默认不再列示 ② 层（仅实验开关开启时显示）；设置 API `decision` 字段保留透传（契约不变，服务端状态不受影响）；实验路径经 `GEC_DEC_ENABLED=1` / 设置文件显式开启（代码保留），检测默认路径统一表述为「规则 → L3 → LLM」
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
