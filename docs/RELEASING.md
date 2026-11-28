# 版本发布流程

一次发布 = 版本号 + CHANGELOG 段落 + Git 标签 + 多源构建产物 + 各源 release 资产。
本文与 `scripts/release.sh`、`scripts/publish.sh` 的实际行为一致。

## 版本号约定

- 遵循 [semver](https://semver.org/lang/zh-CN/)：破坏性接口或数据格式变更升 major，
  新增能力升 minor，修复升 patch。
- 版本号唯一定义在根目录 `Cargo.toml` 的 `[workspace.package].version`；
  `desktop-app/tauri.conf.json` 的 `version` 必须与之一致，
  `scripts/release.sh` 启动时即校验，不一致直接失败。
- 每个发布版本在 [CHANGELOG.md](../CHANGELOG.md) 有对应段落（Keep a Changelog 格式），
  底部链接区补齐 `[vX.Y.Z]` 的 tag / compare 链接。

## 发布步骤

1. **确认版本与 CHANGELOG**：`[Unreleased]` 内容归入 `## [vX.Y.Z] - YYYY-MM-DD`，
   更新底部链接区；把两处版本号改到 `Cargo.toml` 与 `tauri.conf.json`。
2. **提交并打标签**：

   ```sh
   git add -A && git commit -m "chore(release): vX.Y.Z"
   git tag vX.Y.Z
   ```

3. **多源构建**（github / gitea / atomgit 三源，产物互不混用）：

   ```sh
   ./scripts/release.sh
   ```

   - 每次构建前清空 bundle 目录，避免上一版本残留混入本次 dist。
   - 产物落在 `dist/<源>/vX.Y.Z/`（安装包 + `latest.json`，由
     `scripts/make_latest_json.py` 生成）。
   - 自动更新签名密钥默认取 `~/.config/langfixer/updater.key`
     （可用 `TAURI_SIGNING_PRIVATE_KEY_PATH` 覆盖）。无密钥时仍产出安装包，
     但没有 `.sig` 与 `latest.json`，即本次关闭自动更新产物。
     生成密钥：`cargo tauri signer generate -w ~/.config/langfixer/updater.key --password ''`，
     公钥需与 `tauri.conf.json` 一致。
4. **推送分支与标签到各源**：

   ```sh
   ./scripts/publish.sh
   ```

   对配置好的每个 remote 执行 `git push <remote> <branch> --tags`；
   某源领先本地时先 `git pull --rebase <remote> <branch>` 再重跑。
5. **上传 release 资产**：把 `dist/<源>/vX.Y.Z/` 下的安装包与 `latest.json`
   上传为该标签在各源的 release 附件。
6. **补 Windows / Linux 安装包（三源）**：mac 机器打不出跨平台产物，
   由 `.github/workflows/release.yml` 负责。推送标签时自动触发；也可在
   标签已存在时手动运行该工作流（`workflow_dispatch`，填标签名）。
   发布源在 `desktop-app/build.rs` 按源烧入，所以 Windows / Linux 包共打
   6 份（github / gitea / atomgit × Windows / Linux，构建并行），由三个
   publish 任务分别发到对应源的 release。要点：
   - 仓库 secrets：`TAURI_SIGNING_PRIVATE_KEY`（内容 =
     `~/.config/langfixer/updater.key`，密码空则不配
     `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`，缺了 `tauri build` 直接失败）；
     `GITEA_TOKEN`（Gitea 个人设置→应用，write:repository）；
     `ATOMGIT_TOKEN`（atomgit.com 私人令牌，已有）。
   - Gitea / AtomGit 的发布走 `scripts/publish_gitea.py` /
     `scripts/publish_atomgit.py`：release 不存在则创建，先把该标签已有
     资产拉到发布目录，再由 `make_latest_json.py` 重算 `latest.json` 并
     **替换**同名旧清单（其余同名资产跳过，不重复上传）。
   - GitHub 的 publish 任务用 `gh release` 下载-合并-上传，已有 mac 资产
     原样保留。
   - **顺序**：各源 mac 资产先上传（步骤 5）、CI 合并清单收尾。若标签
     推送时 CI 先跑完而 mac 资产还没就位（清单当时缺 darwin 键），mac
     资产传完后再重跑一次 release.yml 即可重算覆盖。

## 发布后检查

- 各源 release 页面资产齐全，`latest.json` 的 version 指向新版本。
- 服务器部署走 `./update.sh server`（见 [OPERATIONS.md](OPERATIONS.md)）。
