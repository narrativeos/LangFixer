# 评测通道（网关形态）设计

> 状态：D1（身份层）+ D2（owner 隔离 + 配额计量 + 全局视图收口）+ D3（清理 timer + 公网部署）
> + D4（验证机制：安装初始化超管密钥 + 界面门禁 + 管理面密钥分发）已落地，评测通道已上线。
> 目标：大赛评测要求「公网 URL + 免注册测试账号」，产品主体仍是单机形态——
> 认证只存在于网关形态这一层薄壳，核心引擎一行不动。

## 形态开关

| 形态 | 触发 | 行为 |
|---|---|---|
| 单机（默认） | 不设 `LANGFIXER_GATEWAY` | 默认开启登录门：内建 `admin` 账号密码登录（默认密码 `LangFixer@2026`，首次启动落库并 WARN），未登录的业务请求一律 401 + `login_required`；`LANGFIXER_LOGIN=0` 回到「零变化」直通（与接入前完全一致，仅「设置写入」在 owner 认领后要求登录态，见下节）；已验证超管可在「接入」页（或 `POST /auth/password`）改密，旧密码即时作废 |
| 网关 | `LANGFIXER_GATEWAY=1` | 全部业务端点要求评审令牌；`/admin/*` 只认超管令牌 |

配置见 `.env.example`。进程环境优先于 `.env`；`.env` 不入库（已在 `.gitignore`）。

## 身份模型：一张令牌表 = 唯一身份原语

- **评审令牌** `lf_<40hex>`：超管经 `POST /admin/tokens` 签发，**明文只在签发响应出现一次**，库中只存 SHA-256 摘要；Bearer 直连与「换会话 cookie」两条路共用同一校验；未来 MCP 面（D2）挂同一 middleware，即「web 与 MCP 共同一套 token」。
- **会话**：`POST /auth/session {"token": "…"}` 换 12h httpOnly cookie（免注册、无找回密码、无用户资料）；`/auth/logout` 注销。口令登录 `{"username","password"}` 换的是同一枚会话。
- **用户名账号**（`gw_users`）：**同一枚评审密钥的口令别名**，不是新身份——配额、历史、MCP
  仍按密钥归属；由管理面代建或试用者自助创建（无注册流程）。密钥吊销 / 删除后口令登录
  一并失效（孤立账号行不是登录后门）。详见「用户名账号」。
- **超级管理员密钥**（`LANGFIXER_ADMIN_TOKEN`，`.env`，≥32 字符，`openssl rand -hex 32`；
  **不设则首次启动自动生成 `lf_<40hex>`**，见下节）：
  - **env 设了则全程不进数据库**，内存只留摘要；**自动生成那把必须明文落 `gw_meta.admin_token`**
    （否则重启就换一把，已分发的直链全废）——此时保护边界就是 `auth.db` 的文件权限，
    多用户机器建议显式在 env 里设一把；
  - **重启永不换钥匙**：优先级是 env → `gw_meta.admin_token` → 都没有才生成并落库（只生成这一次）。
    `systemctl restart` 只是重读同一份来源。env 与 `gw_meta` 同时有值时以 env 为准，
    启动日志 WARN 一句「库里那枚（指纹 xxxx）不会被使用」，避免「改了 gw_meta 却没生效」无声发生；
  - 只走 Bearer、**永不下发 cookie**（超管登录产生的会话 cookie 除外）、不占评审归户与配额；
  - 是唯一可达 `/admin/*` 的凭证——评审令牌打 admin 一律 403；
  - **换钥匙是一个显式动作，不是重启 / 改文件的副作用**：见「轮换超级管理员密钥」。
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

# 用户名账号：给已签发的密钥建 web 登录口令（临时口令只回这一次）
curl -X POST -H "Authorization: Bearer $LANGFIXER_ADMIN_TOKEN" \
     -H 'content-type: application/json' -d '{"token_id":"<id>","username":"zhang-laoshi"}' \
     http://127.0.0.1:4801/admin/users
# 清单 / 重置口令（清在途会话 + must_change）/ 删别名（密钥保留）
curl -H "Authorization: Bearer $LANGFIXER_ADMIN_TOKEN" http://127.0.0.1:4801/admin/users
curl -X POST -H "Authorization: Bearer $LANGFIXER_ADMIN_TOKEN" http://127.0.0.1:4801/admin/users/zhang-laoshi/reset
curl -X DELETE -H "Authorization: Bearer $LANGFIXER_ADMIN_TOKEN" http://127.0.0.1:4801/admin/users/zhang-laoshi
```

存储：`auth.db`（独立于产品数据 `langfixer.db`，删文件即重置整条评测通道）。

### 轮换超级管理员密钥（泄露处置按来源分两条）

**先确认这把钥匙归谁**（已登录的超管才回这两个字段；匿名 `whoami` 不带，桌面端 invoke 也不带）：

```bash
curl -s -H "Authorization: Bearer $LANGFIXER_ADMIN_TOKEN" http://127.0.0.1:4801/auth/whoami
# {"gateway":true,"admin":true,"authenticated":true,"locked":true,
#  "admin_token_source":"env","admin_token_fp":"5369bceb"}
```

`admin_token_fp` 是**当前活着的那把**的 SHA-256 前 8 位，用来核对「这台实例用的是哪把钥匙」，
不是密钥本身——不用再把它念一遍。密钥分发界面上那行「由 …… 持有 · 指纹 ……」就是它。

| 来源 | 换钥匙的办法 | 说明 |
| --- | --- | --- |
| `env` | 改 `langfixer.env` 里 `LANGFIXER_ADMIN_TOKEN=` 的新值 → `systemctl restart langfixer` | 重启**不是**换钥匙的动作，换的是你写进文件里的新值；改前 `grep` 一眼确认没打错字符 |
| `generated` | `POST /admin/admin-token/rotate`（下面这条 curl） | 明文只回一次，旧密钥与其超管会话即刻失效，`gw_meta` 存新值以便重启复用 |

```bash
# 必须带 confirm —— 换钥匙不接受隐式请求（缺 confirm / 传别的值一律 400）
curl -X POST -H "Authorization: Bearer $LANGFIXER_ADMIN_TOKEN" \
     -H 'content-type: application/json' -d '{"confirm":"ROTATE"}' \
     http://127.0.0.1:4801/admin/admin-token/rotate
```

- 来源是 `env` 时该端点回 **409** 并说明改 env 的流程。**故意不做**「库里换一把、env 那把仍然赢」
  的行为：那样 `gw_meta` 与内存摘要会分裂，下次从 env 删掉变量时突然换成另一把，比不换更糟。
- 顺序是**先落库再换内存摘要**：库写失败时内存不动（回 500，密钥未变更），不会出现
  「内存是新的、重启后回退成旧的」。
- 连带清掉超管会话（`gw_sessions.token_id = '~admin'`）：那些 cookie 是旧钥匙换来的，
  留着等于旧钥匙仍有一条有效通路。评审密钥与其会话**不受影响**，参赛报名材料里那枚不用动。
- 日志只记指纹（`tracing::warn!`），不记明文。

### 用户名账号（密钥的口令别名，供 web 口令登录）

token 直链适合发链接，但收藏夹里没有 token 的试用用户需要「用户名 + 密码」登录；
MCP 继续用密钥。一张 `gw_users` 把两者接起来：

- **一个密钥至多一个用户名**（`token_id UNIQUE`）；用户名全局唯一，规则为
  2–32 位小写字母/数字/`._-` 且含字母数字，保留名 `admin`（入库前 `normalize_username`）；
- **口令存储**与超管同口径（带盐 SHA-256，`s1$`），拒默认密码，≥ 8 字符；
- **两条创建路径**：管理面代设（签发带 `username` 或 `POST /admin/users`，临时口令只回一次，
  `must_change=1`）与试用者自助（`POST /auth/credentials`，需登录态；首次创建带
  `username`，已有账号带 `old_password`；自设 `must_change=0`）；超管走自助端点回 400；
- **核验顺序**：口令登录先比内建 admin（超管），再查 `gw_users`；密钥已吊销 / 已删则
  口令登录 401（LEFT JOIN 后 token 行不存在也算死）；
- **作废链**：重置口令 / 自助改密连带清该身份全部在途会话（旧口令外泄的那批 cookie
  不能幸存；自助改密当场重发新会话）；revoke 密钥清会话（既有行为）；
  `DELETE /admin/users/:username` 只删别名，密钥与 MCP 不受影响；删密钥连带删别名行；
- `whoami` 对评审身份带 `username` / `must_change`（临时口令未改时前端黄条催改）；
  口令登录响应在临时口令时带 `must_change: true`。

界面入口：管理面「密钥分发」每行的「账号」列（建 / 重置）；评审侧「接入」页「我的账号」
（创建 / 改口令）；门禁页网关形态可切「用户名 + 口令」登录。

### 用量统计（密钥分发表上那一列）

`GET /admin/tokens` 每枚密钥除 `id/label/quota_chars/revoked` 外带出日账读数，
`GET /admin/stats` 带出合计，界面「密钥分发」直接渲染（今日用量 + 进度条 + 今日剩、累计 / 最近一天）：

| 字段 | 口径 |
| --- | --- |
| `used_today` | 该枚密钥当日（UTC 日界）已计字符数，与配额判定同一行账 |
| `used_retained` | **保留期内**累计（`gw_usage` 还在的日账之和）——清理 timer 按 `LANGFIXER_RETENTION_DAYS`（默认 7 天）删旧账，**不是历史总量**，界面按此如实标注 |
| `days_used` / `last_day` | 有记账的天数 / 最近一天（`gw_usage.day` 是 UTC 日序，读出时 `CAST` 成整数） |
| `chars_today` / `chars_retained` / `retention_days` | `/admin/stats` 的实例级合计与账窗，老服务端无这三个字段时界面退回逐令牌求和 |
| `calls_today` / `calls_retained` | **调用级日志**（`gw_call_log`）口径的次数：当日（UTC）与保留期内。含被拒请求——401/403/429 也是有人敲过门；越权尝试记在核验成功的令牌名下，不洗成匿名 |
| `calls_today` / `calls_retained`（stats） | `/admin/stats` 的实例级调用合计（含匿名与超管调用：`token_id` 为空的行计入合计、不计入任何令牌行） |

调用级日志已落地（`gw_call_log`，auth.db）：鉴权门对每个网关请求记一行
`ts / token_id / endpoint / status / latency_ms`——谁在几点调了哪个端点、被拒的
401/403 尝试从此可查。**隐私红线**：只存路径本身，不带 query string（查询串可能
含用户文本）、不带请求体、不带 header；`token_id` 是身份不是凭证。保留期与日账
同源，`langfixer-retention.timer` 一并删除超期行。单机形态不落调用日志（零新增
写盘，也不把私人使用记成「调用」）。Caddy access log 仍不开——网关侧已自足。

## D2：owner 隔离与配额（已落地）

- **身份键到业务的两个通道**：gate 为每个放行请求注入 `Tenant`（`Extension<Tenant>`）——
  评审身份携带 `token_id`，超管与单机形态恒 None = 全部守卫直通；
- **会话归属**（auth.db `gw_session_owner`）：首次使用（创建 / 读写任一 `:id` 端点）即绑定当前账号，
  他人访问同一会话一律 **403**（fail-closed，含伪造他人 session_id 的 detect/analyze 请求体）；
- **历史归属**：网关形态下 detect / detect_lang 把结果写入账号伪会话 `s_gw:<token_id>`
  （首次自动绑定），历史列表按归属过滤后内存分页，详情 / 删除跨账号一律 404（不泄露存在性）；
  无归属行 fail-closed 隐藏。公网实例两枚临时密钥对打实测：B 的历史列表看不到 A 的正文（0 行）、
  B 拿 A 的 history id 直取详情与删除均 **404**、A 反向同样看不到 B 的正文，落库行归属逐条正确；
- **超管会话不是评审身份**（`Tenant.token_id = None`）：因此不走归属过滤——「检测历史」对超管是
  **全局视图**（运维口径，与直接读库等价），而超管自己打的 detect 落 `session_id` 为空的行，
  对所有评审 fail-closed 隐藏。评审密钥之间严格互不可见；
- **配额计量**（auth.db `gw_usage` 按 UTC 日聚合）：detect / detect_lang / analyze 在跑引擎前按字符数记账；
  令牌 `quota_chars > 0` 且本次越线 → **429**（不进引擎、不计入用量），`0` = 只记账不限速；
- **网关形态收口的端点**（403 + 说明文案）：清空全部历史、按语言批量删会话、
  全局错题本 / 复习队列 / 学习统计 / 全局画像——评测通道按账号隔离；
- **评审身份可达面 = fail-closed 白名单**（`middleware::auth::review_denied`，在鉴权门
  生效，早于任何业务 handler）：只放行「检测 + 本人历史 + 只读说明」——
  `/auth/*`、`/mcp*`、`POST /gec/detect`、`POST /gec/:lang/detect`、`POST /gec/analyze`、
  `POST /gec/explain`(`/stream`)、`/gec/history*`、`/gec/sessions*`、
  `GET /gec/settings`、`GET /gec/languages`；其余一律 403。为什么不是逐端点补守卫：
  11-12 走查实测评审密钥原先可直接打通 `POST /gec/l3/retrain`（重训模型）、
  `POST /gec/agent/run`（自主档写工具跑批）、`POST /gec/eval/run`（长跑打满 LLM 网关）、
  `POST /gec/llm-feedback`（`adopt` → `adopt_as_rule` 改写共享规则库）、
  `POST /gec/ops/unlock`（未设 `GEC_TABLES_UPDATE_TOKEN` 时直接回 `unlocked:true`，
  等于把规则管理 / 知识维护 / 进化中心整面交给评委）。参赛密钥是**全体评委共享**的
  一枚，共享身份不得改写共享状态——与 MCP 把写档工具改成提案信封同一个立场。
  界面侧随之收起评审身份下必然 403 的「测试全部」按钮（`PipelineStatus`）；
  验收 `api-server/tests/gateway_review_surface.rs`；
- **跨账号确定性旁路封堵**：网关形态跳过进程级 LRU 缓存与历史复用
  （`GATEWAY_MODE` 进程闸；单机形态复用链零变化）——否则 B 复用 A 的同文本记录会
  「复用即不写新历史」，在 B 的归属账上留下空洞。

## D4：验证机制（安装初始化 + 界面门禁，已落地）

产品形态是「装完即用」，参赛要求是「不验证不露界面」，两条都靠这层薄壳守住，核心引擎零改动。

1. **超管密钥安装初始化**：env 未设 `LANGFIXER_ADMIN_TOKEN` 时首次启动自动生成
   `lf_<40hex>` 并持久化到 auth.db `gw_meta.admin_token`——**重启复用不漂移**（否则用户手上
   那把会失效），明文只在生成那一次的启动日志出现一次；env 设了则以 env 为准
   （设了但 <32 字符仍按配置错误拒绝启动）。库里其余凭证只有摘要。
    `./setup.sh server` 首装时在服务器的 `langfixer.env` 里生成 64 hex 并打印一次明文——即这把
    归 **env** 管（轮换端点会回 409）；若刻意删掉该变量再重启，才回到上面的启动自动生成路径。
   **换钥匙是显式动作**：`POST /admin/admin-token/rotate`（`confirm:"ROTATE"` 必填）只在
   `gw_meta` 来源可用，env 来源回 409；`whoami` 对已验证超管回 `admin_token_source` /
   `admin_token_fp` 指明这把归谁、活的是哪把。详见「轮换超级管理员密钥」。
2. **验证前不露界面**：`GET /auth/whoami` 匿名可用（不进 gate），回
   `{gateway, admin, token_id, authenticated, locked, password_login}`（已验证超管额外带密钥来源与指纹；
   已设口令的评审身份额外带 `username` / `must_change`）。
   `locked` = 界面是否需要验证：
   网关形态恒 `true`；单机形态登录门开启即 `true`，关闭时看 `gw_meta.owner_claimed`。前端 `showGate` / `showApp` 二选一，
   未验证时整个产品界面（header 与主区）不渲染；密钥只进 `sessionStorage`（关标签页即失效，
   不落 localStorage），`?token=` 直链登录后立即从地址栏剥掉。
3. **单机默认登录门**：登录门开启时业务端点要求会话 cookie 或超管 Bearer，
   `/auth/*` 探测与 `/gec/deps` 体检保持公开；`LANGFIXER_LOGIN=0` 时回到旧口径——
   业务端点全开放、**只有设置写入**（`PUT /gec/settings`、`/gec/settings/test`）在 owner 认领后要求已验证凭证。
   桌面端 `invoke` 不走 HTTP，两种情况下都完全不受门禁影响。
   认领动作 = 超管首次成功换会话时置 `owner_claimed`；单机形态未认领时「接入」页是唯一的
   「认领并锁定设置」入口（此时界面照常敞开，不增加任何安装摩擦）。
4. **配置面收口**：网关形态配置写入与探测仅超管可达（其余 **403**）；非超管 `GET /gec/settings`
   时 LLM / embedding 的 `api_key` 回 `***`（超管读明文，用于自检）。
5. **两种分发形态**（管理界面 `接入` 页，服务端 `/admin/tokens` 同一张表）：
   **Web 直链** `https://…/?token=<密钥>` 打开即自动进入；**MCP 配置** JSON
   （`mcpServers.langfixer = {url: /mcp, headers.Authorization: Bearer …}`）。
   两者共用同一枚密钥与同一份配额。普通密钥用户只能导出**自己这一枚**的配置——
   签发入口只对超管出现，服务端同样只认超管进 `/admin/*`。
6. 超管会话在 `gw_sessions` 用伪 id `~admin`，可进管理面但不发配额、不进 owner 归户；
   桌面端（Tauri）恒视为已验证超管。

冒烟口径：两形态 17 项（安装生成 + 激活前后设置写入 + 脱敏 + 403/401/403 + 会话与 admin 面）
见 `docs/GATEWAY_AUTH.md` 提交说明；界面口径见「两形态浏览器实测」——网关未登录只见门禁页、
评审零 403 零 console error、单机装完即用无门禁。

## 部署红线

- 冒烟 / 联调必须用 `GEC_DB_FILE=/tmp/….db` 与 `LANGFIXER_AUTH_DB` 隔离两个库——
  默认路径是 CWD 相对的 `langfixer.db`，在仓库根直接起服务会写进真实产品数据（有过教训）；
- 网关形态 `LANGFIXER_BIND=127.0.0.1`，公网入口只由 Caddy（HTTPS + 限流）持有；
- 每评审一令牌（一人一 `label`），既是隔离单位也是后续 owner 过滤与用量的键；
- 页面与材料声明「评测文本 N 天后自动清理、不用于模型训练」——清理任务在 D3。

## 后续切片

- D2：请求体计入 `quota_chars` 并超限 429；业务查询按 `RequestScope.token_id` 过滤（owner 隔离）；MCP `/mcp` 复用同一 gate。✅（已完成）
- D3：过期文本 systemd timer 清理；Caddy 站点与部署件（`deploy/gateway/`）。✅（公网已上线验证：https://langfixer.jiulu.ltd，web/MCP 同一令牌；LLM 兜底经 litellm 隧道接 Qwen3.8；中文分词由 narrative-operator-nlp sidecar 提供）
- D4：验证机制——安装初始化超管密钥、token 门禁页、管理界面密钥分发（Web 直链 + MCP 配置）。✅（见上节）

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
