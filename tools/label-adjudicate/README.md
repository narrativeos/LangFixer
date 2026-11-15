# label-adjudicate · 模型新增标注分档抽检裁决页

dataset_v21 有 12,970 条「模型提出、金集没有」的新增标注，按纪律
（每桶 ≥58 条抽检、放弃率 ≤2%）才能升级为可依赖标签。本工具做分档裁决。

## 运行

```sh
python3 tools/label-adjudicate/serve.py            # 默认 ~/tmp/dataset_v21 :8765
python3 tools/label-adjudicate/serve.py --state-dir /tmp/x --port 8765
```

打开 http://127.0.0.1:8765 。左栏是桶与门槛进度，右栏逐条卡片：原文高亮
检出位、建议修正与理由；键盘 A 采纳 / R 放弃 / U 存疑。

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
