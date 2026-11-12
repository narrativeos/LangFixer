# 评测通道（网关形态）设计

> 状态：D1（身份层）+ D2（owner 隔离 + 配额计量 + 全局视图收口）已落地；N 天自动清理与 Caddy 部署在 D3。
> 目标：大赛评测要求「公网 URL + 免注册测试账号」，产品主体仍是单机形态——
> 认证只存在于网关形态这一层薄壳，核心引擎一行不动。

## 形态开关

| 形态 | 触发 | 行为 |
|---|---|---|
| 单机（默认） | 不设 `LANGFIXER_GATEWAY` | 鉴权整体关闭，与接入前完全一致 |
| 网关 | `LANGFIXER_GATEWAY=1` | 全部业务端点要求评审令牌；`/admin/*` 只认超管令牌 |

配置见 `.env.example`。进程环境优先于 `.env`；`.env` 不入库（已在 `.gitignore`）。

## 身份模型：一张令牌表 = 唯一身份原语

- **评审令牌** `lf_<40hex>`：超管经 `POST /admin/tokens` 签发，**明文只在签发响应出现一次**，库中只存 SHA-256 摘要；Bearer 直连与「换会话 cookie」两条路共用同一校验；未来 MCP 面（D2）挂同一 middleware，即「web 与 MCP 共同一套 token」。
- **会话**：`POST /auth/session {"token": "…"}` 换 12h httpOnly cookie（免注册、无找回密码、无用户资料）；`/auth/logout` 注销。
- **超级管理员令牌**（`LANGFIXER_ADMIN_TOKEN`，`.env`，≥32 字符，`openssl rand -hex 32`）：
  - 唯一**不进数据库**的凭证，内存只留摘要；
  - 只走 Bearer、**永不下发 cookie**、不占评审归户与配额；
  - 是唯一可达 `/admin/*` 的凭证——评审令牌打 admin 一律 403；
  - 泄露处置 = 改 `.env` 值 + 重启（改后即失效，开关即断）。
- **吊销即断**：revoke 一个令牌连带踢掉其全部在途会话（冒烟已验证）。

## 管理面速查

```bash
# 签发（明文 token 只出现这一次，抄进参赛材料的测试账号栏）
curl -X POST -H "Authorization: Bearer $LANGFIXER_ADMIN_TOKEN" \
     -H 'content-type: application/json' -d '{"label":"评审A","quota_chars":200000}' \
     http://127.0.0.1:4801/admin/tokens
# 列表 / 吊销 / 删除 / 概览
curl -H "Authorization: Bearer $LANGFIXER_ADMIN_TOKEN" http://127.0.0.1:4801/admin/tokens
curl -X POST -H "Authorization: Bearer $LANGFIXER_ADMIN_TOKEN" http://127.0.0.1:4801/admin/tokens/<id>/revoke
curl -X DELETE -H "Authorization: Bearer $LANGFIXER_ADMIN_TOKEN" http://127.0.0.1:4801/admin/tokens/<id>
curl -H "Authorization: Bearer $LANGFIXER_ADMIN_TOKEN" http://127.0.0.1:4801/admin/stats
```

存储：`auth.db`（独立于产品数据 `langfixer.db`，删文件即重置整条评测通道）。

## D2：owner 隔离与配额（已落地）

- **身份键到业务的两个通道**：gate 为每个放行请求注入 `Tenant`（`Extension<Tenant>`）——
  评审身份携带 `token_id`，超管与单机形态恒 None = 全部守卫直通；
- **会话归属**（auth.db `gw_session_owner`）：首次使用（创建 / 读写任一 `:id` 端点）即绑定当前账号，
  他人访问同一会话一律 **403**（fail-closed，含伪造他人 session_id 的 detect/analyze 请求体）；
- **历史归属**：网关形态下 detect / detect_lang 把结果写入账号伪会话 `s_gw:<token_id>`
  （首次自动绑定），历史列表按归属过滤后内存分页，详情 / 删除跨账号一律 404（不泄露存在性）；
  无归属行 fail-closed 隐藏；
- **配额计量**（auth.db `gw_usage` 按 UTC 日聚合）：detect / detect_lang / analyze 在跑引擎前按字符数记账；
  令牌 `quota_chars > 0` 且本次越线 → **429**（不进引擎、不计入用量），`0` = 只记账不限速；
- **网关形态收口的端点**（403 + 说明文案）：清空全部历史、按语言批量删会话、
  全局错题本 / 复习队列 / 学习统计 / 全局画像——评测通道按账号隔离；
- **跨账号确定性旁路封堵**：网关形态跳过进程级 LRU 缓存与历史复用
  （`GATEWAY_MODE` 进程闸；单机形态复用链零变化）——否则 B 复用 A 的同文本记录会
  「复用即不写新历史」，在 B 的归属账上留下空洞。

## 部署红线

- 冒烟 / 联调必须用 `GEC_DB_FILE=/tmp/….db` 与 `LANGFIXER_AUTH_DB` 隔离两个库——
  默认路径是 CWD 相对的 `langfixer.db`，在仓库根直接起服务会写进真实产品数据（有过教训）；
- 网关形态 `LANGFIXER_BIND=127.0.0.1`，公网入口只由 Caddy（HTTPS + 限流）持有；
- 每评审一令牌（一人一 `label`），既是隔离单位也是后续 owner 过滤与用量的键；
- 页面与材料声明「评测文本 N 天后自动清理、不用于模型训练」——清理任务在 D3。

## 后续切片

- D2：请求体计入 `quota_chars` 并超限 429；业务查询按 `RequestScope.token_id` 过滤（owner 隔离）；MCP `/mcp` 复用同一 gate。✅（已完成）
- D3：过期文本 systemd timer 清理；Caddy 站点与部署件（`deploy/gateway/`）。✅（部署件已入库，公网落地见其 README）

## 公网部署形态（deploy/gateway/）

- 二进制在**服务器本机编译**（x86_64-unknown-linux-gnu；zig 交叉编译路径因 edition-2024
  工具链兼容性废弃）；运行时不依赖 `models/` gguf（`GEC_OFFLINE=1`、`GEC_DEC_ENABLED=0`）；
- api-server 绑 **docker 网桥宿主侧地址**（`LANGFIXER_BIND=172.18.0.1:4801`）：只有同网桥的
  Caddy 容器可达，公网与宿主其他进程不可见；公网入口只由 Caddy(443) 持有；
- **源码、`.env`、数据库一律放 `/srv/langfixer`**——`/dnmp/www` 被 Caddy 挂载为 web 根，
  只放 `web-ui/dist`，任何敏感件不得入内；
- 数据保留：`langfixer-retention.timer` 每日删除 N 天（默认 7）前的检测文本与配额日账，
  并收敛已吊销令牌的会话归属行。

## MCP 面（`POST /mcp`）

JSON-RPC 2.0 无状态形态（`initialize` / `tools/list` / `tools/call` / `ping`），与 web 面
同一道 gate、同一套 token、同一本配额账：

- **工具集** = `proofread` / `explain`（检测管线）+ `agent_tools.rs` 全量 ManagedTool
  （规则库 / 知识 / 导入草稿 / 反馈统计与写档），单机形态全部直接执行；
- **网关形态档位纪律**：只读白名单（`proofread` `explain` `list_rules` `get_rule`
  `search_knowledge` `feedback_stats`）直接执行，**写档一律返回提案不执行**
  （`structuredContent.executed:false` + `[confirm-required]` 前缀，与 web 侧 Confirm gate 同源叙事）
  ——评审令牌共享同一份产品状态，写没有账号隔离，执行权属于产品界面里的用户；
- 配额与 web 共用（已实测：MCP 用 55 + web 8 > 60 → 429）；超管不占配额；
- 网关形态 `tools/list` 的写档描述自动标注「评测通道：调用时返回提案不执行」。
