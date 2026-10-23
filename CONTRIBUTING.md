# 参与贡献

LangFixer 处于 **v0.1.0-preview 预览阶段**：核心链路可用，但接口与数据格式仍可能演进。欢迎提交 issue 与 PR。

## 开发环境

- Rust（stable，edition 2024）
- Node.js 22 + npm（web-ui，Vue 3 + Vite）
- 无需任何 API Key 即可跑全部测试（外部服务均有本地 mock）

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

运行开发实例：`api-server`（`cargo run -p api-server`，REST :27661）+ `web-ui`（`npm run dev`）；
桌面端 `cargo tauri dev`（desktop-app）。外部依赖（HanLP NLP 服务、LLM 端点）通过设置面板/环境变量配置，缺省时相应功能降级，不影响编译与测试。

## 代码约定

- `cargo fmt --all` 格式化（CI 强制）；命名与注释以中文为主，标识符英文
- 归因决策表是数据文件（`decision-tables/*.json`，JDM 格式），由 `decision-tables/generate_tables.py` 生成——修改规则请改生成脚本并再生，不要手改 JSON
- 知识库（`knowledge-base/`）内置的是自撰示例源（`src-sample-en`）；请勿向仓库提交任何第三方书籍/教材的原文或派生数据（用户导入内容属用户数据，运行时目录不进版本库）
- 两端同构：`api-server` 与 `desktop-app` 共享 `attribution-engine`/`gec-*` 等核心 crate，能力变更应同时保持两端可用

## 提交与 PR

- 提交信息：Conventional Commits（`feat:` / `fix:` / `refactor:` / `chore:` …），描述可用中文
- PR 请先开 issue 讨论方向（预览阶段接口变动较大）
- CI 必须通过：`cargo fmt --check`、`cargo test --workspace`、web-ui typecheck/test/build

## 许可

贡献即表示同意你的贡献以 Apache-2.0 许可随项目分发（见 LICENSE 与 NOTICE）。
