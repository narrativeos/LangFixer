# label-adjudicate · 模型新增标注分档抽检裁决页

dataset_v21 有 12,970 条「模型提出、金集没有」的新增标注，按纪律
（每桶 ≥58 条抽检、放弃率 ≤2%）才能升级为可依赖标签。本工具做分档裁决。

## 运行

```sh
python3 tools/label-adjudicate/serve.py            # 默认 ~/tmp/dataset_v21 :8765
python3 tools/label-adjudicate/serve.py --state-dir /tmp/x --port 8765
python3 tools/label-adjudicate/serve.py --bind 0.0.0.0 --room-key <口令> --quorum 2   # 多人协作
```

打开 http://127.0.0.1:8765 。左栏是桶与门槛进度，右栏逐条卡片：原文高亮
检出位、建议修正与理由；键盘 A 采纳 / R 放弃 / U 存疑。

## 多人协作（编辑老师共同裁决）

- **登录**：`--room-key` 启用登录页，名字+口令进房；session cookie 为
  HMAC(room_key, name) 签名，逐条裁决署名 `who` 写进 journal（旧无署名记录
  归 `--owner`，默认房主；房主重登请就用该名字，否则历史票不会算作本人）。
- **防冲突**：卡片可「领取」（15 分钟 TTL 软锁），他人列表里显示 🔒；
  每人在自己未裁过的条目上各投一票，同名重裁只覆盖自己。
- **共识门槛**：`--quorum K`（默认 1=旧行为）：门槛/放弃率只统计 ≥K 人有效
  裁决的条目，生效裁决=多数票，平票→存疑；导出取生效=采纳的条目。
  多人双裁建议 2：单人裁完不再单独构成门槛进度（页上显示「待满 quorum」）。
- **审计**：`/api/state.crew` 报每人 adopt/reject/uncertain 计数；
  `judge.py report` 会先报裁决人两两**人人一致率**（逐位/采纳方向），再报
  裁判 vs 人工共识的一致率——金标可信度与裁判可信度分开定账。
- 非本机 `--bind` 强制要求 `--room-key`（无署名/无防冲突的共享直接拒启）。
  局域网 HTTP 只供可信内网；出口公网不归本工具管.

## 公网发布（token 登录，v2.3 编裁实例）

- 形态：`--token-db <网关 auth.db>` 启用 token 模式——登录页只填一枚评审令牌，
  服务端 sha256 比对 `access_tokens`（吊销即断），who=label，签发 HMAC 签名
  cookie `lf_adj`（12h）。与 `--room-key` 二选一；两者互斥于匿名单机模式。
- 实例（47.94.172.217，systemd `langfixer-adjudicate.service`）：
  `python3 serve.py --data-dir /srv/langfixer/adjudicate/data --bind 172.18.0.1
  --port 8765 --token-db /srv/langfixer/data/auth.db --quorum 2 --owner 房主`。
  服务器 CPython 3.6.8：代码需兼容 3.6（无 future annotations；typing 用
  Optional/List/Dict/Tuple；ThreadingHTTPServer 有 socketserver fallback）。
- 入口：Caddy `handle_path /adjudicate/* → reverse_proxy 172.18.0.1:8765` +
  `redir /adjudicate /adjudicate/ 308`；HTTPS 由 Caddy 终结。改 Caddyfile 用
  原地写入并 `docker restart caddy`（单文件 bind mount 被 sed -i 换 inode 后，
  容器里 reload 读到的是旧文件）。
- token 分发沿用网关模式：admin API `POST /admin/tokens {"label":"编辑X",
  "quota_chars":0}` 签发，明文只现一次，一人一枚。历史 journal 归 `--owner`
  的名字，房主登录必须用该 label。
- 裁判（judge.py）只能在能到 LLM 端点的机器跑；服务器 journal 攒批后 scp 回
  本地再 report。

## 分档（横向分型 → 抽检成本）

| 桶 | 型 | 逻辑 |
|---|---|---|
| auto | punct | 规则层可复核，抽审通过整桶升级 |
| must | fact / consistency / cohesion | 低量高危，逐条必审 |
| audit | typo / grammar | 量大型，种子 42 确定性抽审 |

## 口径（重要）

- 队列**启动时从 `*_v21.jsonl` 按逐条口径现算**：每条 model_error 的字符集
  `range(a,b)` 与 gold 字符集 `range(ga, max(gb,ga+1))` 相交即共识，否则新增。
  插入型（a==b）字符集为空 → 必属新增。该口径复现 rollout 更正后数字：
  新增 12,970 / 共识 12,789 / 合计 25,759。
- jsonl 行内 `consensus`/`model_new` 与旧 `review_queue_new_errors.jsonl` 是
  旧**累计口径**（共识高估至 16,474），仅对照不信任；页面上并排展示。
- 裁决 journal `adjudications.jsonl` 与整桶决定 `buckets_promoted.json`
  落在 state-dir（默认 = data-dir）；追加式，重启即续（done 必须落盘续跑）。
- 污染纪律照旧：v21 标注器见过公开集，不得用于评测 Qwen3.8 同族模型。

## 预筛信号（自动档，词表 = lexicons/zh_gec_lexicon.json，可 --lexicon 换）

| 信号 | 判据 | 处置 |
|---|---|---|
| no_error 判无错误入 | fix==片段且 why 讲「无误/保留」 | 批量放弃，不入标签 |
| del_intent 删除意图 | fix==片段且 why 讲「删/多字/重复」 | 真错误；逐条采纳，导出归一 delete（fix 置空） |
| noop_contra 矛盾 | fix==片段但 why 讲实质改错 | rollout 解析缺陷（v2.2 修），永不批量采纳，导出标 manual |
| lex_ok / lex_contra | 易混词典·标点映射背书/矛盾 | 背书可批量采纳；矛盾逐条 |
| lex_blind | 全角标点互换，词典无判据 | 维持原桶 |

批量采纳在服务端一律跳过 fix==片段三档；「批量放弃」对任何信号可用。

## 导出

「导出已采纳」→ 已采纳新增（含 verdict/provenance）ndjson；共识部分不在
队列内，下游按逐条口径从 jsonl 现取。
