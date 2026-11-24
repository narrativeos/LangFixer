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

## 发布后检查

- 各源 release 页面资产齐全，`latest.json` 的 version 指向新版本。
- 服务器部署走 `./update.sh server`（见 [OPERATIONS.md](OPERATIONS.md)）。
