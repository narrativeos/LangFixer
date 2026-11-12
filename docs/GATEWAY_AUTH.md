# 评测通道（网关形态）设计

> 状态：D1 已落地（身份层）；配额计量、owner 数据隔离、N 天自动清理在 D2/D3。
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

## 部署红线

- 网关形态 `LANGFIXER_BIND=127.0.0.1`，公网入口只由 Caddy（HTTPS + 限流）持有；
- 每评审一令牌（一人一 `label`），既是隔离单位也是后续 owner 过滤与用量的键；
- 页面与材料声明「评测文本 N 天后自动清理、不用于模型训练」——清理任务在 D3。

## 后续切片

- D2：请求体计入 `quota_chars` 并超限 429；业务查询按 `RequestScope.token_id` 过滤（owner 隔离）；MCP `/mcp` 复用同一 gate。
- D3：过期文本 cron 清理；Caddyfile 与部署脚本。
