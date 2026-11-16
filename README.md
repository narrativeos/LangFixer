# train 侧 LLM 缓存扩容（p̂(t) 测量面）

router_ev 臂的 p̂(t) 在首跑时饥饿（§11.14 补三）：train 缓存只盖到测量面
5,070 行中的 611 行，p̂ 除 spelling/grammar 外全走先验。本工具把测量面补齐。

- 测量面 = zh_v2 池 train ∩ `gold_counts.jsonl`(spans 非空)，含池 label=0
  漏标侧（p̂ 按编辑真实型分型，两侧都要测）。
- 断点续跑/重试全走 `router_labels.run_llm_arm` 原机件；扩容先写
  `<cache>.expand`，完成后与主缓存按池序原子合并，中途被杀不丢账。
- 端点仅从 `LLM_BASE_URL` / `LLM_MODEL` / `LLM_API_KEY` 环境变量读，不落文件。
- 串行吞吐 ~4.3s/行（上游限制，workers 只抵超时损耗）：待跑 ~4.5k 行 ≈ 5h+。

跑完用 `expand.py` docstring 尾部的 eval 命令重出 p̂ 与 router_ev。
