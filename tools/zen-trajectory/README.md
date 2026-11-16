# zen-trajectory — ZEN 轨迹表首版（decision-tables + ±6 窗口谓词）

EVOLUTION §11.14 补二实验2 定案的落法：must 桶（consistency/cohesion）里
谓词在编辑 span 内只可见 17.7%，**±6 窗口下升至 42–52%**——约一半编辑规则侧
可达，另一半留 LLM/语篇回归层。本工具就是那"可达一半"的载体。

## 形态：轻量账本 + 判定表

- `table.json` 声明行：族（代词/数字/关联词/时间）→ 谓词（regex）→
  通道（cohesion/consistency）+ 检查语义。窗口 `[-6, 6]` 在 meta 声明。
- `trajectory.py` 只做在场命中账（谓词是否出现在编辑 span 的 ±6 窗口），
  产出逐条**轨迹账** `trajectory.jsonl`：`{id,src,a,b,fix,type,span,fires,span_visible}`。
- 首版=在场谓词（路由信号，"该句值得按上下文查一致性/衔接"），**不做冲突
  判定**；v2 谓词（指代矛盾计数、关联词断对、年份时序倒挂）在 rows[].next 挂账。

## 跑法与锚点

```sh
python3 tools/zen-trajectory/trajectory.py
# consistency n=243  ±6可达 52.3%  span内 17.7%
# cohesion    n=509  ±6可达 42.4%  span内 17.7%
```

复现 §11.14 补二实验2 逐位一致（补二账是唯一口径源）。

## 边界

- 打扰率 ≤2% 闸门要等生产接线后按「ZEN 通道产出错误/全部产出错误」实测，
  离线无分母，本工具不算。
- 规则臂逻辑不在这里（那是 `tools/type-layer-experiments/rule_offline.py`
  单一台账）；此处只有 must 桶四族窗口谓词，勿混。
