# 评测通道部署（网关形态）

公网评测入口的落地件：systemd 单元 + 数据保留清理 + Caddy 站点片段。
设计红线与身份模型见 [`docs/GATEWAY_AUTH.md`](../../docs/GATEWAY_AUTH.md)。

## 布局约定

| 路径 | 内容 | 备注 |
| --- | --- | --- |
| `/srv/langfixer/bin/api-server` | release 二进制 | `cargo build --release -p api-server` |
| `/srv/langfixer/{decision-tables,lexicons,prompts}` | 运行时资产 | `models/` 8G gguf 与决策模型**不上传**（`GEC_OFFLINE=1`、`GEC_DEC_ENABLED=0`） |
| `/srv/langfixer/data/{langfixer.db,auth.db}` | 产品库 / 身份库 | 首次启动自动建表 |
| `/srv/langfixer/langfixer.env` | 环境文件 | 模板 `langfixer.env.example`，`chmod 600` |
| `/dnmp/www/langfixer/` | web-ui/dist 静态 | 在 Caddy 容器挂载的 web 根内，**只放 dist** |

源码编译放 `/srv/langfixer/src`（web 根之外——`/dnmp/www` 被 Caddy 挂载，源码、`.env`、
数据库一律不得入内）。

## 步骤

1. **二进制**：服务器本机编译（交叉编译路径已废弃：zig 与 cargo-zigbuild 对
   edition-2024 工具链不兼容），或任何 x86_64-linux 机器构建后拷贝：

   ```sh
   cd /srv/langfixer/src && cargo build --release -p api-server
   install -D target/release/api-server /srv/langfixer/bin/api-server
   ```

2. **资产**：从仓库拷 `decision-tables/ lexicons/ prompts/` 与 `web-ui/dist/*`；
   dist 进 `/dnmp/www/langfixer/`，其余进 `/srv/langfixer/`。

3. **环境**：`cp langfixer.env.example /srv/langfixer/langfixer.env`，
   填 `LANGFIXER_ADMIN_TOKEN`（`openssl rand -hex 32`），`chmod 600`。

4. **服务**：拷 `langfixer.service`、`langfixer-retention.{sh,service,timer}` 到
   `/etc/systemd/system/`（sh 落 `/srv/langfixer/bin/`，`chmod +x`）：

   ```sh
   systemctl daemon-reload
   systemctl enable --now langfixer langfixer-retention.timer
   ```

5. **入口**：把 `Caddyfile.snippet` 追加进 `/dnmp/services/caddy/config/caddyfile`，
   `docker exec caddy caddy reload --config /config/caddyfile`。

6. **签发评审令牌**（超管面，只在服务器本机经 4801 端口执行）：

   ```sh
   curl -s -X POST -H "Authorization: Bearer $LANGFIXER_ADMIN_TOKEN" \
     -H 'content-type: application/json' -d '{"label":"评审-甲","quota_chars":200000}' \
     http://172.18.0.1:4801/admin/tokens
   ```

   明文只在响应里出现一次，填入参赛材料「测试账号」栏；吊销即连踢会话。

## 冒烟清单（公网）

- `GET https://<host>/` → 200，web-ui 可打开；
- 无凭证 `POST /gec/detect` → 401；`POST /mcp initialize` 带评审 Bearer → 200；
- 评审 cookie 免注册：`POST /auth/session` 带 Bearer → Set-Cookie → 后续免头可用；
- A 令牌的会话看不到 B 令牌历史（跨账号 403/404）；
- 超管 `GET /admin/stats` 正常，评审令牌访问 `/admin/*` → 403；
- 配额：同令牌 web+MCP 合计越线 → 429。

## 数据红线

- 网关形态下清空/批删/全局视图类端点一律 403；评审侧只有「写档返回提案」；
- 检测文本保留 `LANGFIXER_RETENTION_DAYS`（默认 7）天，由 retention timer 每日清理；
- 服务器联调必须显式 `GEC_DB_FILE` 指向隔离库——仓库根默认 `langfixer.db`
  是真实产品库，`DELETE /gec/history` 是真实破坏性操作（见 GATEWAY_AUTH.md 事故记录）。
