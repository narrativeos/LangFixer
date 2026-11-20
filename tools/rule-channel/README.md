# 规则通道（rule-channel）

true gold 合并器的 `rule_candidates.jsonl`（门控未见 ~10,222）的消费端。
四条约定（tools/true-gold/README）的实现件：

- `rules.py` —— 规则族谓词：punct_stream（文档惯例自适应）/ punct_dup /
  digit_time（严式时间紧邻）/ place_hint（地名+数字/介词共现）。
- `mine_rules.py` —— 量化：族×（候选召回/语料点火/已标免问/纯打扰每万字），
  出 `rule_family_map.jsonl` 与 `rules_report.md`。
- `adjudicate.py --check --id <rid> --src <文本或文件>` —— 运行时命中即问：
  输出「金集判改X ⟦上下文⟧ 模型两轮均否」的裁决问题（stdout jsonl，供 UI 消费）。
- `adjudicate.py --record <id>#<pos> real|noise|mark` —— 用户判断回写
  `~/tmp/gold_adjudicate/gold_verdicts_journal.jsonl`（与 gold-adjudicate 页
  同 iid 同格式），`true-gold/merge.py` 重跑自动吸收，不另开账本。

## 量化结论（本语料首跑，rules_report.md）

静态规则对 mark 桶只回收 **0.6%**——主体是漏字/多字/搭配/语序类字符问题，
规则族结构上够不着。规则通道的定位因此明确：**未来文本的高敏感事实
核查要点**（时间/地点/标点惯例，纯打扰 ≤31/万字），对本语料的回收只作
副产物。mark 主体的两条正路：下一轮 rollout 配方改进、知识源通道
（Tolaria 路线）。

- 2026-11-20 重跑（merge 后候选 10,222，语料 118.2 万字）：结论不变——
  族合计命中 59（0.58%）、完全未覆盖 99.4%；四族判「留」作未来文本要点。
  digit_time 纯打扰 31.15/万字为四族最高，价值在时间事实核查要点，
  不作检测门控；grammar_novice 兜底（占错误归因 50%）的规则点覆盖
  属后续迭代，rule_defs 现 26 条与四族谓词是两套东西，本通道不直接补。

## 跑序

```bash
python3 ../true-gold/merge.py        # 出 rule_candidates.jsonl
python3 mine_rules.py               # 量化 + 族索引
python3 adjudicate.py --check --id nacgec-123 --src 行文本
python3 adjudicate.py --record nacgec-123#7 real --note "运行时命中用户判"
```
