# 决策模型（vegaml）— LangFixer 三层检测级联的中间层

本目录实现 LangFixer 的**决策模型层**：在规则引擎与 LLM 之间插入一个
轻量 vegaml 分类器，对「规则引擎未命中」的句子做错误判断。

```
用户输入
   │
   ▼
┌─────────────┐  命中（高置信）
│ 规则引擎     │ ─────────────────────┐
│ (gec-core)   │                      │
└──────┬──────┘                      │
       │ 未命中（规则无法判断）        ▼
       ▼                    ┌───────────────────┐
┌─────────────┐  检出错误   │  合并（去重/排序）   │
│ vegaml 决策层│ ──────────▶ │  最终错误列表       │
│ (本目录)     │  弃权 → 放行 └─────────┬─────────┘
└──────┬──────┘                        │
       │ 仍不确定 / 需要深度解释          ▼
       ▼                    ┌───────────────────┐
┌─────────────┐  讲解+练习   │  前端展示            │
│ LLM 层       │ ──────────▶ └───────────────────┘
│ (langfixer-llm) │
└─────────────┘
```

**设计原则**（与 `gec-core/src/decision.rs` 对应）：

- **默认关闭**（P0.5：L3 特征模型是唯一的 pre-LLM 门控，决策模型与 L3 同为
  句级二分类门控且 L3 严格更强，两者同开时是冗余层）：opt-in 层——
  `GEC_DEC_ENABLED=1`/`true` 或设置界面（web-ui / 桌面端）显式开启后
  挂载（规则为主 + 决策模型合并）；切换即时生效。
- **弃权优先**：`has_error_prob` 低于 `abstain_threshold`（默认 0.5）视为无错误，
  不产生误报；宁可漏报交给 LLM 层，不可误报污染规则结果。
- **故障回退**：vegaml 服务不可达 / 超时时，检测器自动回退纯规则，不崩溃。
- **span 级优先**：vegaml 返回带 span 的错误时直接映射到具体片段；
  仅句级判断时合成整句错误项（`message` 标注"决策模型检出…"）。

## 文件

| 文件 | 作用 |
|------|------|
| `server.py` | FastAPI 服务：`POST /decide`（vegaml 推理包装）；`VEGAML_MOCK=1` 离线 mock 模式（开发/测试用） |
| `vega_finetune.py` | vegaml 适配工作流：`extract`（gold 数据 → vegaml 特征）/ `train`（微调，模型版本每次自增写入 `models/<lang>/meta.json`）/ `eval`（在 gold 集上评测） |
| `test_version.py` | 版本号逻辑测试（自增 / 旧模型版本未知 / meta 损坏 / `trained_at` 格式）：`.venv/bin/python test_version.py` |
| `prepare_data.py` | 把 `datasets/jfleg_gold.json` / `mucgec_gold.json` 转成 vegaml 训练/评测格式（错误句 vs 干净句） |
| `requirements.txt` | Python 依赖 |

## 快速开始

```bash
cd decision-model
python3 -m venv .venv && .venv/bin/pip install -r requirements.txt

# 1) 离线 mock 模式（无需模型；启发式判断，**非真实模型**，仅验证 Rust 侧链路）
VEGAML_MOCK=1 .venv/bin/python server.py
#    → POST http://127.0.0.1:4804/decide  {"text": "...", "model": "mock"}
#    GET /health 报告 "mode": "mock"；设置界面「服务测试」会明确告知用户非真实模型

# 2) 真实模型（sklearn 回退路径；vegaml 本体未发布到 PyPI，见下节）
.venv/bin/python prepare_data.py --gold ../datasets/jfleg_gold.json --lang en --out data/en
.venv/bin/python vega_finetune.py extract --lang en --data data/en --out vegaml_data/en
.venv/bin/python vega_finetune.py train   --lang en --data vegaml_data/en
.venv/bin/python vega_finetune.py eval    --lang en --data vegaml_data/en
.venv/bin/python server.py   # 加载 models/en/ 下的微调模型（zh 同理）
```

Rust 侧（**默认开启**，直接启动即可；环境变量可覆盖，或经设置界面「决策模型」卡片切换）：

```bash
cargo run -p api-server
# 显式关闭：GEC_DEC_ENABLED=0
# 显式指定服务地址：GEC_DEC_ENDPOINT=http://127.0.0.1:4804
```

## 与 vegaml 的关系

[vegaml](https://github.com/vegamllm/vegaml) 是面向文本纠错的轻量模型框架
（特征抽取 + 分类/序列标注）。本目录不硬依赖其 API 细节——
`vega_finetune.py` 中的 `VegaAdapter` 是适配层：

- vegaml 可用时：`extract` 产出其特征格式，`train` 调用其训练入口；
- vegaml 不可用（未安装 / API 变动）：`extract` 退化为通用
  「句级二分类」特征（编辑距离先验 + 词长/标点/重复词等启发式），
  训练入口回退到 sklearn LogisticRegression（`VEGAML_FALLBACK=1` 强制）。

`server.py` 的 `/decide` 契约是稳定的（见下），与 vegaml 内部解耦：
真实模式下 `VegaAdapter` 优先加载 vegaml 原生模型，否则加载
`vega_finetune.py` 训练出的 sklearn 回退模型（`models/<lang>/model.joblib`）；
两者都缺失时回退启发式（`/health` 的 `mode` 字段如实报告当前模式：
`mock` / `vegaml` / `sklearn` / `mixed` / `heuristic-fallback`）。

## `/decide` 契约

请求：

```json
{ "text": "I go to school yesterday.", "model": "vega-base" }
```

响应：

```json
{
  "has_error": true,
  "has_error_prob": 0.92,
  "error_type": "word_order",
  "abstain": false,
  "errors": [
    { "span": [2, 19], "error_type": "word_order", "type": "word_order",
      "original": "…", "confidence": 0.92, "message": "…" }
  ]
}
```

- `errors[].error_type` / `original` 为 Rust 侧（`DecisionGecEngine`）解析所需字段
  （`type` 为兼容旧服务的别名，`original` 为 span 对应原文）

- `abstain` 弃权信号：`true` 表示模型不确定（未命中 / 概率低于阈值），
  三级响应 level 2 据此触发 LLM 兜底层；Rust 侧对缺省 `false` 的旧服务
  按概率灰区 `[0.25, abstain_threshold)` 兜底判定
- `has_error_prob < abstain_threshold`（Rust 侧判定）→ 弃权，不产生错误项
- `error_type` 映射（Rust 侧 `map_vegaml_type`）：
  `collocation`→COLLOCATION，`word_order`→WORD_ORDER，
  `missing`/`structure`/`logic`→GRAMMAR，其他→GRAMMAR
- `errors[].span` 为**字符偏移**（与 Rust 侧 `ErrorItem.span` 一致）

## `/health` 契约

```json
{
  "status": "ok",
  "mode": "mock" | "vegaml" | "sklearn" | "mixed" | "heuristic-fallback",
  "backends": { "en": "sklearn", "zh": "sklearn" },
  "versions": { "en": "v2", "zh": "v1" }
}
```

- `mode`：服务运行模式（设置界面「服务测试」据此向用户区分真实模型 / mock / 启发式回退）
- `versions`：各语言模型版本号——`vega_finetune.py` 每次 `train` 自增
  （`v1, v2, ...`，写入 `models/<lang>/meta.json`）；旧模型无版本字段 → 版本未知
  （不写入 `versions`，Rust 侧保守处理）；
  该语言无模型时缺省。mock 模式为固定 `"mock"` 标记（启发式非训练模型）
- Rust 侧（`DecisionGecEngine::model_version`）查询该字段并缓存，存入检测历史
  参数快照（`DetectParams.model_version`）：**重训练 → 版本变化 → 同文本历史
  不再被「重复检测复用历史」复用**（与规则侧版本指纹同级的模型侧门槛）
