# 评测通道部署（网关形态）

公网评测入口的落地件：systemd 单元 + 数据保留清理 + Caddy 站点片段。
设计红线与身份模型见 [`docs/GATEWAY_AUTH.md`](../../docs/GATEWAY_AUTH.md)。

## 日常就用两条命令

```sh
./setup.sh local   # 本机首次：网关后端 :4801 + Vite :4805，密钥与隔离库落在 .dev/
./update.sh local  # 本机日常：重编译 + 重启后端（Vite 不动，HMR 自己跟上）

./setup.sh server  # 服务器首次：铺齐 /srv/langfixer 并 enable --now langfixer
./update.sh server # 服务器日常：二进制 + 前端 dist + 运行时资产全同步，失败自动回滚
```

`./update.sh server --dry-run` 只报「这次会带来什么 + 哪些运行时资产与 HEAD 有差异」，
不动任何东西；`--no-web` / `--no-assets` 用于出问题时缩小范围。共用件在
`scripts/deploy-common.sh`，可覆盖的目标用 `LF_SSH / LF_URL / LF_DIR / LF_SRC / LF_WEB_ROOT /
LF_SERVICE / LF_PORT` 传入。

脚本刻意**不做**的三件事，都是显式决定：

- **不换密钥**。`systemctl restart` 永远复用同一把（优先级 env → `gw_meta.admin_token`）。
  换钥匙是显式动作：env 持有就改 `/srv/langfixer/langfixer.env` 再重启（此时 rotate 端点回
  409），`gw_meta` 持有就 `POST /admin/admin-token/rotate`。首装脚本会打印一次明文密钥，
  此后所有输出只给来源与指纹。
- **不动反向代理与外部容器**。Caddy 站点片段见下方「步骤 5」，中文分词 sidecar 见
  「中文 NLP sidecar」；不起 sidecar 时 zh 管线自动退化为纯规则离线模式。
- **不覆盖已有落地件**。已存在的 `langfixer.env` 与 `/etc/systemd/system/*.service` 一律不改；
  单元确实要按仓库覆写时显式 `LF_FORCE_UNITS=1`。

两条实现上的坑写在这里，免得下次再踩：编译判据只用日志哨兵（`tail` 编译日志找
`Finished .*profile` / 行首 `error`），**不能**用 `pgrep`/`pkill -f "cargo build --release"`——
那条命令自身的命令行里就含这串字，会匹配到自己；`git archive | ssh 'tar -x && cargo build &'`
会把整条管道一起后台化导致 tar 被截断，必须分两步。

## 手工步骤（脚本失败或第一次装机时的兜底）

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

## 中文 NLP sidecar（narrative-operator-nlp）

中文检测管线的分词走 `HANLP_ENDPOINT`，由 narrative-operator-nlp 容器提供：

```sh
docker run -d --name narrative-operator-nlp --network dnmp_default \
  -p 172.18.0.1:8000:8000 -v narrative-operator-nlp-models:/root/.hanlp \
  -e HF_ENDPOINT=https://hf-mirror.com -e HANLP_MODEL_SET=MTL \
  --restart unless-stopped narrative-operator-nlp:latest fastapi
```

- 同样只绑网桥宿主侧地址，不出公网；LangFixer env 配
  `HANLP_ENDPOINT=http://172.18.0.1:8000`、`GEC_OFFLINE=0`；
- 模型卷需预置（`file.hankcs.com` 在云服务器上可能不可达）：从已有环境
  `docker run --rm -v <src-vol>:/d alpine tar cf - -C /d .` 管道搬运；
- 该容器异常时 LangFixer 不瘫痪：zh 管线自动退化为纯规则离线模式。

## 冒烟清单（公网）

- `GET https://<host>/` → 200，web-ui 可打开；
- 无凭证 `POST /gec/detect` → 401；`POST /mcp initialize` 带评审 Bearer → 200；
- 评审 cookie 免注册：`POST /auth/session`（body `{"token":"lf_..."}`）→ Set-Cookie → 后续免头可用；
  logout 带 cookie 调用即失效（whoami 401）；
- A 令牌的会话看不到 B 令牌历史（跨账号 403/404）；
- 超管 `GET /admin/stats` 正常，评审令牌访问 `/admin/*` → 403；
- 配额：同令牌 web+MCP 合计越线 → 429。

## 数据红线

- 网关形态下清空/批删/全局视图类端点一律 403；评审侧只有「写档返回提案」；
- 检测文本保留 `LANGFIXER_RETENTION_DAYS`（默认 7）天，由 retention timer 每日清理；
- 服务器联调必须显式 `GEC_DB_FILE` 指向隔离库——仓库根默认 `langfixer.db`
  是真实产品库，`DELETE /gec/history` 是真实破坏性操作（见 GATEWAY_AUTH.md 事故记录）。
