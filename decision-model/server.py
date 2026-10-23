#!/usr/bin/env python3
"""决策模型（vegaml）FastAPI 服务 — LangFixer 三层检测级联的中间层。

契约（与 gec-core/src/decision.rs 的 DecisionGecEngine 对应）：

    POST /decide
    请求:  {"text": "...", "model": "vega-base"}
    响应:  {"has_error": bool, "has_error_prob": float,
            "error_type": str, "abstain": bool,
            "errors": [{"span": [s, e], "type": str, "message": str}]}

- `errors[].span` 为**字符偏移**（Python `re` / `len` 语义）；
  Rust 侧（gec-core `char_span_to_bytes`）负责换算为字节偏移后入 `Error.span`
  （CJK 文本两者相差约 3 倍，直接透传会导致 span 重叠去重对中文误判）
- `abstain` 为弃权信号：`true` 表示模型不确定（未命中 / 概率低于阈值），
  三级响应 level 2 据此触发 LLM 兜底层；Rust 侧对缺省 `false` 的旧服务
  按概率灰区 `[0.25, abstain_threshold)` 兜底判定
- Rust 侧按 `abstain_threshold` 判定弃权；本服务负责给出概率与候选 span
- 健康检查: GET /health → {"status": "ok", "mode": "mock"|"vegaml"|...,
  "backends": {...}?, "versions": {lang: 版本号}?}
  （版本号来自 `models/<lang>/meta.json`，每次重训练自增；旧模型无版本字段 →
  版本未知（不写入 `versions`）；Rust 侧据此做「重复检测复用历史」的模型侧门槛；
  mock 模式为固定 `"mock"` 标记）

运行：

    VEGAML_MOCK=1 python server.py            # 离线 mock（启发式判断，非真实模型，仅开发/测试）
    python server.py                          # 真实模式：优先加载 models/<lang>/ 下的
                                              # 微调模型（vegaml 原生，或 sklearn 回退模型）

环境变量：
    VEGAML_MOCK=1     强制 mock 模式（不依赖 vegaml 安装）
    DEC_PORT           监听端口（默认 4804，与 GEC_DEC_ENDPOINT 默认值一致）
    DEC_HOST           监听地址（默认 127.0.0.1）
    DEC_MODELS_DIR     微调模型目录（默认 ./models）
"""

from __future__ import annotations

import json
import os
import re
import sys
from dataclasses import dataclass, field
from typing import Any

from fastapi import FastAPI, Request
from fastapi.responses import JSONResponse
import uvicorn

from vega_finetune import extract_features

HOST = os.environ.get("DEC_HOST", "127.0.0.1")
PORT = int(os.environ.get("DEC_PORT", "4804"))
MODELS_DIR = os.environ.get("DEC_MODELS_DIR", os.path.join(os.path.dirname(__file__), "models"))
MOCK = os.environ.get("VEGAML_MOCK", "0") == "1"

app = FastAPI(title="LangFixer Decision Model (vegaml)", version="0.1.0")


# ─── 启发式判断（mock 模式 & vegaml 特征先验共用） ───

# 常见"错误信号"启发式（仅用于 mock 模式演示链路；真实模式由 vegaml 打分）
_EN_PATTERNS: list[tuple[str, str, str]] = [
    # (正则, error_type, message)
    (r"\b(teir|teh|recieve|seperat\w*|occured|writen|begining|comming)\b", "collocation", "拼写/搭配"),
    (r"\b(a)\s+(apple|an orange|an elephant|an hour)\b", "collocation", "冠词搭配"),
    (r"\b(was|were|did|didn't)\s+(go|come|see|make|take|give|know|think|want|need|like|love|help|try|use|get|put|set|run|walk|talk|work|play|read|write|speak|say|tell|ask|answer|call|meet|start|stop|open|close|turn|move|keep|leave|arrive|return|finish|begin|continue|happen|seem|feel|look|sound|taste|smell)\b", "word_order", "时态/语序"),
    (r"\b(yesterday|last|ago)\b.*\b(is|am|are|was)\b|\b(is|am|are)\b.*\b(yesterday|last|ago)\b", "word_order", "时态不一致"),
    (r"\b(I|he|she|it|we|you|they)\s+(go|come|see|make|take|give|know|think|want|need|like|love|help|try|use|get|put|set|run|walk|talk|work|play|read|write|speak|say|tell|ask|answer|call|meet|start|stop|open|close|turn|move|keep|leave|arrive|return|finish|begin|continue|happen|seem|feel|look|sound|taste|smell)\b.*\b(yesterday|last|ago)\b", "word_order", "时态不一致（现在时动词 + 过去时间状语）"),
    (r"\b(he|she|it|they|we|you)\s+(go|come|see|make|take|give|know|think|want|need|like|love|help|try|use|get|put|set|run|walk|talk|work|play|read|write|speak|say|tell|ask|answer|call|meet|start|stop|open|close|turn|move|keep|leave|arrive|return|finish|begin|continue|happen|seem|feel|look|sound|taste|smell)\b", "word_order", "主谓一致/时态"),
]

_ZH_PATTERNS: list[tuple[str, str, str]] = [
    (r"的(了|着)|了(的|地)|的地(得|的)", "collocation", "虚词 的/地/得 混用"),
    (r"把(他|她|它|你|我)(们)?(是|有|在|很|太|最)", "word_order", "把字句结构"),
    (r"(非常|很|太|最)的(是|有|在)", "collocation", "副词+的 搭配"),
    (r"(他|她|它|我|你)(们)?[一-龥](了|过)(昨天|今天|明天|刚才)", "word_order", "时间状语语序（应置于动词前）"),
    (r"([一-龥])\1\1", "word_order", "连续重复字"),
]


def _is_chinese(text: str) -> bool:
    cjk = sum(1 for c in text if "\u4e00" <= c <= "\u9fff")
    return cjk > len(text) * 0.3


def heuristic_decide(text: str) -> dict[str, Any]:
    """启发式判断（mock 模式）：命中模式 → 高置信错误；否则低置信弃权。

    注意：mock 模式是**启发式规则，不是真实模型**；
    Rust 侧「服务测试」（gec-core/src/settings_test.rs::test_decision）
    会经 `GET /health` 的 `mode` 字段向用户明确提示。
    """
    patterns = _ZH_PATTERNS if _is_chinese(text) else _EN_PATTERNS
    for pat, etype, msg in patterns:
        m = re.search(pat, text)
        if m:
            return {
                "has_error": True,
                "has_error_prob": 0.85,
                "error_type": etype,
                "abstain": False,
                "errors": [
                    {
                        "span": [m.start(), m.end()],
                        "error_type": etype,
                        "type": etype,
                        "original": text[m.start() : m.end()],
                        "confidence": 0.85,
                        "message": msg,
                    }
                ],
            }
    # 未命中任何模式 → 弃权（低置信；Rust 侧 abstain=true 触发 LLM 兜底层）
    return {
        "has_error": False,
        "has_error_prob": 0.2,
        "error_type": "",
        "abstain": True,
        "errors": [],
    }


# ─── 模型适配层（真实模式） ───

class _SklearnPredictor:
    """sklearn 句级二分类器（回退模型）→ `/decide` 契约适配。

    句级分类器无 span 级输出：判定有错（prob >= 0.5）时合成整句 span，
    Rust 侧（gec-core/src/decision.rs）将其直接映射为句级错误项。
    """

    def __init__(self, model: Any) -> None:
        self.model = model

    def predict(self, text: str) -> dict[str, Any]:
        x = extract_features(text)
        prob = float(self.model.predict_proba([x])[0, 1])
        errors: list[dict[str, Any]] = []
        if prob >= 0.5:
            errors.append(
                {
                    "span": [0, len(text)],
                    "error_type": "grammar",
                    "confidence": prob,
                    "message": "决策模型判定该句存在错误（句级判断）",
                }
            )
        return {"score": prob, "errors": errors}


def _model_version(model_path: str) -> str | None:
    """读模型目录 `meta.json` 的版本号（`vega_finetune.py` 训练时写入）。

    旧模型（meta 无 `version` 字段）/ meta 缺失或损坏 → `None`（版本未知；
    不写入 `GET /health` 的 `versions`，Rust 侧保守处理：与携带版本的历史不匹配）。
    """
    meta_path = os.path.join(model_path, "meta.json")
    if os.path.isfile(meta_path):
        try:
            with open(meta_path, encoding="utf-8") as f:
                v = str(json.load(f).get("version", "")).strip()
            if v:
                return v
        except Exception:  # noqa: BLE001
            pass
    return None


@dataclass
class VegaAdapter:
    """模型推理适配层。

    按语言加载 `models/<lang>/` 下的微调模型：
    - vegaml 可用时优先加载其原生模型；
    - 否则加载 `vega_finetune.py` 训练的 sklearn 回退模型（model.joblib）；
    - 两者都不可用（模型缺失）时 `decide()` 返回 None，调用方回退启发式，
      保证服务始终可用。

    每个语言目录的 `meta.json` 携带模型版本号（`vega_finetune.py` 每次训练
    自增 `v1, v2, ...`）；本层加载时读取并经 `GET /health` 的 `versions`
    字段对外报告（旧模型无版本字段 → 版本未知，不写入）——Rust 侧据此做
    「重复检测复用历史」的模型侧门槛（重训练 → 版本变化 → 旧历史失效）。
    """

    models: dict[str, Any] = field(default_factory=dict)
    backends: dict[str, str] = field(default_factory=dict)
    versions: dict[str, str] = field(default_factory=dict)

    def load(self) -> None:
        vegaml = None
        try:
            import vegaml  # type: ignore[import]
        except ImportError:
            print(
                "[decision-model] vegaml 未安装，尝试加载 sklearn 回退模型（models/<lang>/model.joblib）",
                file=sys.stderr,
            )
        for lang in ("en", "zh"):
            model_path = os.path.join(MODELS_DIR, lang)
            if not os.path.isdir(model_path):
                continue
            if vegaml is not None:
                try:
                    self.models[lang] = vegaml.load(model_path)
                    self.backends[lang] = "vegaml"
                    v = _model_version(model_path)
                    if v is not None:
                        self.versions[lang] = v
                    print(f"[decision-model] 加载 {lang} 模型（vegaml，版本 {v or '未知'}）: {model_path}", file=sys.stderr)
                    continue
                except Exception as e:  # noqa: BLE001
                    print(f"[decision-model] 加载 {lang} vegaml 模型失败（{e}），尝试 sklearn 回退", file=sys.stderr)
            self._load_sklearn(lang, model_path)

    def _load_sklearn(self, lang: str, model_path: str) -> None:
        joblib_path = os.path.join(model_path, "model.joblib")
        if not os.path.isfile(joblib_path):
            return
        try:
            import joblib

            bundle = joblib.load(joblib_path)
            self.models[lang] = _SklearnPredictor(bundle["model"])
            self.backends[lang] = "sklearn"
            v = _model_version(model_path)
            if v is not None:
                self.versions[lang] = v
            print(f"[decision-model] 加载 {lang} 模型（sklearn，版本 {v or '未知'}）: {joblib_path}", file=sys.stderr)
        except Exception as e:  # noqa: BLE001
            print(f"[decision-model] 加载 {lang} sklearn 模型失败: {e}", file=sys.stderr)

    def decide(self, text: str) -> dict[str, Any] | None:
        """返回 /decide 响应；无可用模型时返回 None（调用方回退启发式）。"""
        if not self.models:
            return None
        model = self.models.get("zh" if _is_chinese(text) else "en")
        if model is None:
            return None
        result = model.predict(text)
        # 模型输出归一化到 /decide 契约（span 级错误 + 句级概率）
        prob = float(result.get("score", result.get("prob", 0.0)))
        errors = []
        for err in result.get("errors", []):
            span = err.get("span")
            if span is None:
                continue
            s, e = int(span[0]), int(span[1])
            etype = str(err.get("error_type", err.get("type", "grammar")))
            errors.append(
                {
                    "span": [s, e],
                    "error_type": etype,
                    "type": etype,
                    "original": text[s:e],
                    "confidence": float(err.get("confidence", prob)),
                    "message": str(err.get("message", "")),
                }
            )
        return {
            "has_error": prob >= 0.5 and bool(errors),
            "has_error_prob": prob,
            "error_type": errors[0]["error_type"] if errors else "",
            # 概率低于决策阈值 = 模型不确定 → 弃权（Rust 侧 abstain=true 触发 LLM 兜底层）
            "abstain": prob < 0.5,
            "errors": errors,
        }


adapter = VegaAdapter()
if not MOCK:
    adapter.load()


# ─── 路由 ───


@app.get("/health")
async def health() -> JSONResponse:
    if MOCK:
        mode = "mock"
        # mock 启发式是固定代码（非训练模型）：给固定版本标记，
        # 使 mock ↔ 真实模型切换时 Rust 侧历史门槛可区分
        versions = {"en": "mock", "zh": "mock"}
    else:
        backends = set(adapter.backends.values())
        mode = (
            "vegaml"
            if backends == {"vegaml"}
            else "sklearn"
            if backends == {"sklearn"}
            else "mixed"
            if backends
            else "heuristic-fallback"
        )
        # 各语言模型版本号（meta.json；该语言无模型时缺省 → Rust 侧版本未知）
        versions = adapter.versions or None
    return JSONResponse(
        {"status": "ok", "mode": mode, "backends": adapter.backends or None, "versions": versions}
    )


@app.post("/decide")
async def decide(request: Request) -> JSONResponse:
    body = await request.json()
    text = str(body.get("text", ""))
    if not text.strip():
        return JSONResponse({"error": "text 不能为空"}, status_code=400)
    if MOCK:
        return JSONResponse(heuristic_decide(text))
    result = adapter.decide(text)
    if result is None:
        return JSONResponse(heuristic_decide(text))
    return JSONResponse(result)


if __name__ == "__main__":
    print(
        f"[decision-model] 监听 http://{HOST}:{PORT} "
        f"({'MOCK 模式（启发式，非真实模型）' if MOCK else '真实模式'}, "
        f"{len(adapter.models) if not MOCK else 0} 个模型: {adapter.backends or '无'})",
        file=sys.stderr,
    )
    uvicorn.run(app, host=HOST, port=PORT, log_level="warning")

