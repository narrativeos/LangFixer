# 参与贡献

LangFixer 已发布 **v1.x 正式版**（版本号见 Cargo.toml 与 CHANGELOG）：核心链路稳定，接口按 semver 演进。欢迎提交 issue 与 PR。

## 开发环境

- Rust（stable，edition 2024）
- Node.js 22 + npm（web-ui，Vue 3 + Vite）
- 无需任何 API Key 即可跑全部测试（外部服务均有本地 mock）

### VS Code 调试

`.vscode/launch.json` 内置三组配置（Rust 侧需安装 [CodeLLDB](https://marketplace.visualstudio.com/items?itemName=vadimcn.vscode-codelldb) 扩展，首次打开会提示安装）：

| 配置 | 说明 |
|---|---|
| `api-server (Rust)` | 构建并调试 API 服务（:4801），断点打在 Rust 源码 |
| `web-ui (Vite dev server)` | 内置 node 调试器直接 launch Vite（:4805），断点打在 vite 配置 / 中间件 |
| `desktop-app (Tauri)` | preLaunch 自动构建 + 确保 Vite 在跑（`scripts/ensure-vite.sh`），然后调试桌面端。前端在 Tauri 内 `invoke()` 直连 Rust 命令，**完全离线**、不依赖 api-server；Vite 仅提供页面与 HMR |
| `LangFixer (API + 前端)` | compound：同时调试 API 服务 + Vite |

## 构建与测试

```bash
# 后端（workspace 全部 crate）
cargo test --workspace

# 前端
cd web-ui
npm ci
npm run typecheck   # vue-tsc
npm test            # vitest
npm run build       # vite 构建
```

运行开发实例：`api-server`（`cargo run -p api-server`，REST :4801）+ `web-ui`（`npm run dev`）；
桌面端 `cargo tauri dev`（desktop-app）。外部依赖（HanLP NLP 服务、LLM 端点）通过设置面板/环境变量配置，缺省时相应功能降级，不影响编译与测试。

## 代码约定

- `cargo fmt --all` 格式化（CI 强制）；命名与注释以中文为主，标识符英文
- 归因决策表是数据文件（`decision-tables/*.json`，JDM 格式），由 `decision-tables/generate_tables.py` 生成——修改规则请改生成脚本并再生，不要手改 JSON
- 知识库（`knowledge-base/`）内置的是自撰示例源（`src-sample-en`）；请勿向仓库提交任何第三方书籍/教材的原文或派生数据（用户导入内容属用户数据，运行时目录不进版本库）
- 两端同构：`api-server` 与 `desktop-app` 共享 `attribution-engine`/`gec-*` 等核心 crate，能力变更应同时保持两端可用

## 提交与 PR

- 提交信息：Conventional Commits（`feat:` / `fix:` / `refactor:` / `chore:` …），描述可用中文
- 涉及公开 API 或数据格式变动的 PR，请先开 issue 讨论方向
- CI 必须通过：`cargo fmt --check`、`cargo test --workspace`、web-ui typecheck/test/build

## 版本与发布

- 版本遵循 [semver](https://semver.org/lang/zh-CN/)，变更记录写入 [CHANGELOG.md](CHANGELOG.md)（Keep a Changelog 格式）
- 版本号唯一定义在根目录 `Cargo.toml` 的 `[workspace.package].version`；发布前必须与 `desktop-app/tauri.conf.json` 一致（`scripts/release.sh` 会校验）
- 发布流程：CHANGELOG 归段 → 提交并打 `vX.Y.Z` 标签 → `./scripts/release.sh`（多源构建安装包与 `latest.json`）→ `./scripts/publish.sh`（推送分支与标签到各源）→ 上传 dist 资产
- 完整流程与产物说明见 [docs/RELEASING.md](docs/RELEASING.md)

## 许可

贡献即表示同意你的贡献以 Apache-2.0 许可随项目分发（见 LICENSE 与 NOTICE）。
