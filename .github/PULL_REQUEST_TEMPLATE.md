## 变更说明

<!-- 这个 PR 做了什么、为什么；关联 issue 用 Fixes #123 -->

## 影响面

- [ ] 公开 API / 数据格式有变动（请先开 issue 讨论，见 CONTRIBUTING）
- [ ] 归因决策表 / 知识库数据变更（须改生成脚本并再生，不手改 JSON）
- [ ] 仅内部实现 / 文档

## 自检

- [ ] `cargo fmt --all` 且 `cargo clippy --workspace -- -D warnings` 通过
- [ ] `cargo test --workspace` 通过（含 feature-gated 测试）
- [ ] web-ui typecheck / test / build 通过（前端改动时）
- [ ] CHANGELOG `[Unreleased]` 已补记录（面向用户的行为变化）
