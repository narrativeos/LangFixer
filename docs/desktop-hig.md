# 桌面端设计规范对照（macOS HIG）

本文件记录 LangFixer 桌面端（Tauri 2）对 macOS 人机界面指南的落地与取舍，
与 `desktop-app/src/menu.rs` / `update.rs` / `main.rs` 及 `web-ui` 的桌面分支对应。

## 已落地

| 规范条目 | 落地方式 |
|---|---|
| 标准菜单栏（App/File/Edit/View/Window/Help） | `menu.rs::build`；命令项全部带快捷键，经 `menu-action` 事件转发前端路由 |
| ⌘, 设置 / ⌘1–7 视图切换 / ⌘O 导入 / ⇧⌘E 导出 / ⌥⌘Z 撤销应用修正 / ⌥⌘T 主题 | 原生菜单快捷键（Web 端不重复监听，避免双触发） |
| 右键上下文菜单 | `menu.rs::show_context_menu`：检测历史行 / 错误条目 / 载体任务三处，原生弹出 |
| Finder 拖放导入 | `tauri.conf.json dragDropEnabled` + `tauri://drag-drop` 事件 → `media_create_job_from_path`（按路径读盘，不走 base64） |
| 更新须用户知情选择 | `update.rs`：所有平台先弹框同意再下载/安装；「稍后再说」落盘 `update-skipped.txt` 不再反复打扰；帮助菜单「检查更新…」手动触发 |
| 暗色模式 | 前端 `html.dark`（localStorage 优先，跟随系统兜底）；原生窗口背景色按外观预设 + `set_window_theme` 启动/切换对齐 |
| 窗口管理 | 可缩放 + 最小尺寸；位置/大小/最大化持久化（`window_state.rs`）；标准窗口装饰 |
| 键盘 | ⌘Enter 检测；⌘F 聚焦历史搜索；非编辑区 ⌘Z 撤销「替换为修正文本」（应用级快照 `store.applySnapshot`） |
| 全局状态可见性 | 检测管线可用性是应用级事实：桌面形态收进 header 紧凑 chip（● N/4 项可用，全视图可见），详情与「测试全部」走 popover；Web 形态保留检测页整行状态条 |
| 信息减负（普通用户面） | 按 UX_REVIEW「术语说人话 + 渐进披露」：主界面只留人话摘要（「检测方式：本地快速定案 + AI 复查…」），①②.5③-F③ 层编号、决策/特征模型、LLM、HanLP 等引擎细节收进「详情」popover / tooltip；层级展示名收敛到 `lib/labels.ts` 统一人话词表（本地检查 / 快速定案 / 知识查询 / AI 复查），修复了 ResultsPanel / HistoryPanel / MediaWorkspace 三处词表各自漂移的问题。第二轮收拢：错误来源徽章 / 置信度（→「把握大/中/小」）/ 共识文案 / 修复指引（guide.ts）/ 设置页卡片名（人话主名 + 括号技术名）/ 引导文案同表统一；进化中心 / 知识维护为运营面（面向维护者）保留内部术语 |
| 原生对话框 | 导出「另存为」与更新确认均用 `tauri-plugin-dialog` |

## 明确取舍

- **单窗口**：本应用是「单一工作区 + 共享会话状态」形态（Pinia store / SQLite 会话 /
  后台 job 表都是进程级单例），多窗口需要引入跨窗状态同步，收益低于风险。
  接受与「Cmd+N 多窗口」惯例的偏差；若未来引入「文档」概念（多份独立文稿），再评估。
- **顶栏导航**：Web 壳沿用 header 按钮导航（与 Web 形态同一份 UI），同时把全部视图
  做进 View 菜单 + ⌘1–7，保证菜单栏可发现性与键盘可达性。
- **Web 形态降级**：右键菜单 / 拖放 / 原生菜单相关命令在浏览器形态为 no-op，
  不报错、不影响主流程。

## 已知限制

- 右键菜单弹出后按 Esc 关闭不会回传事件，由 30 秒超时兜底（用户无感知）。
- 菜单快捷键在 Web 形态无效（浏览器会把这些组合键交给页面），Web 端保留原交互。
