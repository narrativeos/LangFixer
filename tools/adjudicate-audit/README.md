# adjudicate-audit — 抽检（agent 代审）脚本

对两个裁决页的桶做批量抽检，判决按 journal 格式落盘，供 `tools/true-gold/merge.py` 消费。

- 数据侧：gold 页读 `~/tmp/grey_zone` + `~/tmp/gold_adjudicate`；label 页读 `~/tmp/dataset_v22`。
- 采样：label 页用 `sample_ids(bucket, 58)`（md5("42|iid") 序）；gold 页同 key 排序取前 58。journal 与顺序无关。
- 门禁：每桶 ≥58 条已审、误判率 ≤2%（gold 误判=翻判 bucket 默认；label 误判=reject；uncertain 计入已审不计误判）。

## 脚本

| 脚本 | 用途 |
|---|---|
| `dump.py <bucket>` | 把桶内样本渲染成紧凑审读行（bucket ∈ auto_real / auto_noise / auto / must / audit） |
| `record.py <cmd> ...` | 通用判决写入器（gold 桶翻判名单式；label auto 桶 adopt/reject/uncertain 三名单式） |
| `verdicts_*.py` | 2026-11 抽检各桶判决的**运行记录**（含逐条一句理由），追加写 journal，**勿直接重跑**（会重复落盘） |
| `gate_table.py` | 用两个 serve 的 `bucket_stats()` 打印五桶门禁表 |

## journal 格式

- gold：`{"iid": "<rowid>#<pos>", "verdict": "real|noise|mark", "reason": "...", "at": <ts>}` → `~/tmp/gold_adjudicate/gold_verdicts_journal.jsonl`
- label：`{"iid": "<rowid>#<a>#<b>", "verdict": "adopt|reject|uncertain", "reason": "...", "at": <ts>}` → `~/tmp/dataset_v22/adjudications.jsonl`

## 本轮结论（2026-11，seed=42）

五桶各审 58 条，全部未过 2% 门禁：auto_real 3.4% / auto_noise 25.9% / label auto 6.9% / must 17.2% / audit 20.7%。
系统性缺陷三类：插入残片复读（v2.2 渲染归一化 bug，reject 主因）、"问了被否"桶约 26% 真错、删除类 D-anchor 少量坏锚点。
处置：不整桶采信；adopt 行以 label=1 进 merge；复读类由 merge 前置校验拦截；uncertain 走 mark→用户命中通道。
