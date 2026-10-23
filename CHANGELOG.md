# 更新日志

本项目遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/) 格式与语义化版本。

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
