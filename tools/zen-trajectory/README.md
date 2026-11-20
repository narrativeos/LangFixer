# zen-trajectory — ZEN 轨迹表（decision-tables + ±6 窗口谓词；v2=在场账+冲突判定）

EVOLUTION §11.14 补二实验2 定案的落法：must 桶（consistency/cohesion）里
谓词在编辑 span 内只可见 17.7%，**±6 窗口下升至 42–52%**——约一半编辑规则侧
可达，另一半留 LLM/语篇回归层。本工具就是那"可达一半"的载体。

## 形态：轻量账本 + 判定表

- `table.json` 声明行（version 2）：
  - `kind=present`：族（代词/数字/关联词/时间）→ regex → 通道。首版口径，
    路由信号="该句值得按上下文查一致性/衔接"。
  - `kind=conflict`（v2 挂账落地）：结构化谓词=真冲突判定，非 mere presence：
    - `straddle_pair` 窗口内同组异值代词分居编辑位左右（他↔她/它、这↔那）
    - `num_same_unit_diff` 窗口内数字对同单位异数值（三年…二年 / 12次…13次）
    - `year_pair` 窗口内双年份 token（数值不同）
    - `conj_pair_break` 关联词前半在场、后半**整句**缺席。
      **显式偏离 ±6**：关联词对跨分句是常态，±6 装不下，scope 取整句。
      `weak_pairs`（因为…所以、如果…就）= 同句合法省略高发的弱义务对，
      打扰账单列强子集 `conj_break_strong` 考核。
- `trajectory.py` 逐条**轨迹账** `trajectory.jsonl`：
  `{id,src,a,b,fix,type,span,fires,span_visible,conflicts}`——present 字段与
  首版逐位不变，`conflicts` 为 v2 追加。
- 锚定模式：error-anchored（编辑 span+fix 为锚：must 桶/true_gold 金标位）；
  standalone（干净语料每 token 为锚：打扰账）。pair 按左锚归属去重，不重复计数。

## 跑法与锚点

```sh
python3 tools/zen-trajectory/trajectory.py
# consistency n=243  ±6可达 52.3%  span内 17.7%
# cohesion    n=509  ±6可达 42.4%  span内 17.7%   ← 补二实验2 逐位复现
```

## 三段校验结论（v2 首跑，§11.14 补六）

| 谓词 | ①must 召回 | ②gold 锚定 | ③clean 打扰(行) | 判定 |
|---|---|---|---|---|
| pron_straddle | 0.3% | 0.1% | 1.87% | **反信号**（gold<clean） |
| num_samediff | 0.0% | 0.0% | 0.35% | 无召回 |
| conj_break | 3.1% | 4.3% | 7.57% | 反信号（gold<clean） |
| conj_break_strong | 0.9% | 2.5% | 2.50% | 无增益（gold≈clean） |
| time_yearpair | 0.4% | 1.8% | 0.18% | **唯一有增益**（10×lift）但召回极小 |

任一谓词命中行占比：全量对 9.64% / 强配对 4.75%，均超 ≤2% 闸门。

**定案**：冲突谓词**不获得独立路由通道**。中文邻近异指代词/缺配对关联词是
常态噪音；gold 命中率对 clean 基线无 lift（唯 year_pair 10× 但召回 0.4%，
不够养通道）。ZEN 的价值保持在 present 口径=给 LLM 裁决喂 ±6 上下文证据
（trajectory.jsonl 的 `conflicts` 字段同样作证据随行进上下文，不作判定）。

## 边界

- 打扰闸门（本工具 standalone 口径=干净行被任一谓词命中的占比）v2 已实测：
  9.64%/4.75% FAIL。生产接线后的「ZEN 通道产出错误/全部产出错误」口径另测。
- 规则臂逻辑不在这里（那是 `tools/type-layer-experiments/rule_offline.py`
  单一台账）；此处只有 must 桶四族窗口谓词，勿混。
