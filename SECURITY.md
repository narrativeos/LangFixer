# 安全策略

## 报告漏洞

请勿在公开 issue 中报告安全问题。通过 GitHub 私有安全通告报告：

**Security → Report a vulnerability**：<https://github.com/narrativeos/LangFixer/security/advisories/new>

我们将在 7 天内回复确认，并尽快修复。预览版本（v0.x）期间修复随下个预览版发布。

## 支持版本

| 版本 | 支持 |
|---|---|
| v0.1.x（预览） | ✅ |
| < v0.1 | ❌（预览期不保证） |

## 安全相关设计说明

- **API Key**：LLM 端点凭据仅存于本地设置（`langfixer-settings.json` / 桌面端设置目录），不上传、不写入日志；`api-server` 以 `GEC_API_TOKEN` 保护写接口，部署时请置于认证层之后
- **知识库边界**：导入的书籍内容属用户数据，存于本地知识库目录；仓库与 Docker 镜像不分发任何用户导入内容
- **决策引擎**：决策表为只读数据文件（JDM），引擎不执行任意代码
- 预览阶段暂未做安全审计/渗透测试，生产部署前请自行评估
