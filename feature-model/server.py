#!/usr/bin/env python3
"""L3 特征模型 FastAPI 服务 — 三级响应 level 3 的主力层（三期「规模化」）。

契约（与 gec-core/src/l3.rs 的 L3FeatureEngine 对应）：

    POST /l3-detect
    请求:  {"text": "...", "lang": "zh"}
    响应:  {"has_error": bool, "has_error_prob": float,
            "error_type": str, "abstain": bool,
            "errors": [{"span": [s, e], "type": str, "original": str,
                       "correction": str|null, "message": str, "confidence": float}]}

- `errors[].span` 为**字符偏移**（Python `re` / `len` 语义）；
  Rust 侧（gec-core `char_span_to_bytes`）负责换算为字节偏移后入 `Error.span`
  （CJK 文本两者相差约 3 倍，直接透传会导致 span 重叠去重对中文误判）
- 高置信（prob >= abstain_threshold）且**模式库命中** → 产出错误项（span 定位 + 机械修正），
  Rust 侧不再调用 LLM；高置信但**无模式命中** → 同样弃权（LLM 长尾裁定）——
  句级聚合特征无法可靠区分「语病句 vs 干净句」（CEC 工作流 D 实验：三类概率分布重叠），
  合成句级误报会在干净句上产生假错误，LLM 才是正确裁定者
- 灰区 `[0.25, abstain_threshold)` / 显式 abstain → 弃权，Rust 侧回退 LLM 长尾；
- 明确无错（prob < 0.25）→ 放行，不调用 LLM
- 健康检查: GET /health → {"status": "ok", "mode": "mock"|"model"|"heuristic-fallback"}

运行：

    FEATURE_MOCK=1 python server.py    # 离线 mock（启发式判断，开发/测试）
    python server.py                    # 真实模型（需 models/<lang>/ 下训练产物）

环境变量：
    FEATURE_MOCK=1    强制 mock 模式（不依赖训练模型）
    L3_PORT           监听端口（默认 4806，与 GEC_L3_ENDPOINT 默认值一致）
    L3_HOST           监听地址（默认 127.0.0.1）
    L3_MODELS_DIR     模型目录（默认 ./models）
    L3_ABSTAIN_THRESHOLD  句级概率弃权阈值（默认 0.5）
    L3_GRAY_ZONE_LOW      灰区下界（默认 0.30；低于此 → 直接放行，见 correct.zh_heuristic_prob）
    L3_ABSTAIN_RATIO      段落级整段弃权阈值（默认 0.80：灰区句占比 > 80% 才全文
                          送 LLM；否则部分定案 + 灰区句定向复查 unreviewed_spans）
    L3_CHAR_GATE_URL      字级判别侧车地址（如 http://127.0.0.1:4811/gate；空=关闭）
    L3_CHAR_GATE_TAU      PLA 救援阈值（默认 0.004 = v2 模型等风险操作点；灰区句 top-3 字替换概率均值
                          低于此 → 放行）

判定流程：
    1. 句级 has_error 概率——真实模型：**批量**特征抽取 + 一次 predict_proba
       （O(1) 次模型推理，而非 O(句数)）；mock 模式：correct.find_hits
       启发式命中 → 0.9；未命中 → 0.4 灰区（每句 find_hits 只算一次）
    2. prob >= 阈值 → 错误类型（模型多分类，仅高置信句批量 predict / 启发式）
       + 模式化 span 定位 + 机械修正（可修正类型才给 correction）
       高置信但无模式命中 → 弃权（LLM 裁定），不合成句级误报
    3. 灰区 → 弃权；prob < 0.25 → 明确无错
"""

from __future__ import annotations

import os
import re
import sys
import json
import threading
from dataclasses import dataclass, field
from typing import Any

from fastapi import FastAPI, Request
from fastapi.responses import JSONResponse
import uvicorn

sys.path.insert(0, os.path.dirname(__file__))
import correct  # noqa: E402
from l3_finetune import FEATURE_NAMES as _FEATURE_NAMES  # noqa: E402

HOST = os.environ.get("L3_HOST", "127.0.0.1")
PORT = int(os.environ.get("L3_PORT", "4806"))
MODELS_DIR = os.environ.get(
    "L3_MODELS_DIR", os.path.join(os.path.dirname(__file__), "models")
)
MOCK = os.environ.get("FEATURE_MOCK", "0") == "1"
ABSTAIN_THRESHOLD = float(os.environ.get("L3_ABSTAIN_THRESHOLD", "0.5"))
GRAY_ZONE_LOW = float(os.environ.get("L3_GRAY_ZONE_LOW", "0.30"))

# 字级判别侧车（D.2，可选）：如启用，启发式灰区句获「PLA 救援」——
# 微调 ELECTRA CTC 头（char_gate.py 侧车）确证干净（top-3 字替换概率
# 均值 < TAU）→ 直接放行。两信号互补：救援使母语长文本 clean 放行率
# ~2x（评测见 README G），代价 ≤1.7pp 可见性。侧车不可达自动回退纯启发式。
CHAR_GATE_URL = os.environ.get("L3_CHAR_GATE_URL", "")

# 字级门控可观测计数（/l3/status 暴露；上线后救援率/侧车健康的数据来源）
_CHAR_GATE_STATS = {"calls": 0, "fails": 0, "scored": 0, "rescued": 0}
CHAR_GATE_TAU = float(os.environ.get("L3_CHAR_GATE_TAU", "0.004"))

app = FastAPI(title="LangFixer L3 Feature Model", version="0.1.0")

# 分句：中文 。！？； / 英文 . ! ? 及换行
_SENT_RE = re.compile(r"[^。！？；.!?]+[。！？；.!?]*|\n+")


def split_sentences(text: str) -> list[tuple[int, int, str]]:
    """切句 → (start, end, sentence) 列表（字符偏移，与原文一致）"""
    out: list[tuple[int, int, str]] = []
    for m in _SENT_RE.finditer(text):
        s, e = m.start(), m.end()
        if e > s:
            out.append((s, e, m.group(0)))
    return out


@dataclass
class FeatureModel:
    """训练模型加载（joblib 打包：bin 二分类 + types 多分类）；缺失时回退启发式"""

    models: dict[str, Any] = field(default_factory=dict)

    def load(self) -> None:
        try:
            import joblib
        except ImportError:
            print("[feature-model] joblib 未安装，回退启发式判断", file=sys.stderr)
            return
        for lang in ("en", "zh"):
            model_path = os.path.join(MODELS_DIR, lang, "model.joblib")
            if os.path.isfile(model_path):
                try:
                    bundle = joblib.load(model_path)
                    # 兼容旧训练脚本的裸产物格式 {'model': LR, 'f1': ...}：
                    # 归一化为规范 bundle {'bin','types',...}（review P0-2：
                    # 不兼容时 sentence_probs 静默返回 None，重训模型从未生效）
                    if "bin" not in bundle and hasattr(bundle.get("model"), "predict_proba"):
                        bundle = {
                            "bin": bundle["model"],
                            "types": None,
                            "feature_names": bundle.get("features"),
                            "classes": [],
                        }
                        print(
                            f"[feature-model] {lang} 模型为旧格式，已归一化加载"
                            f"（无类型分类器；建议通过 /l3-retrain 重训以固化格式）",
                            file=sys.stderr,
                        )
                    self.models[lang] = bundle
                    print(f"[feature-model] 加载 {lang} 模型: {model_path}", file=sys.stderr)
                except Exception as e:  # noqa: BLE001
                    print(f"[feature-model] 加载 {lang} 模型失败: {e}", file=sys.stderr)

    def sentence_prob(self, lang: str, sentence: str) -> float | None:
        """句级 has_error 概率；无可用模型返回 None（调用方回退启发式）"""
        probs = self.sentence_probs(lang, [sentence])
        return probs[0] if probs else None

    def sentence_type(self, lang: str, sentence: str) -> str | None:
        types = self.sentence_types(lang, [sentence])
        return types[0] if types else None

    @staticmethod
    def _fit_features(estimator: Any, X: list[list[float]]) -> list[list[float]]:
        """把特征向量适配到模型期望的维度（review P0-1：FEATURE_NAMES 增维后
        旧模型 predict 直接抛 ValueError → /l3-detect 500）。
        多出的尾部特征截断、缺少的尾部特征补零（新增特征在旧模型中权重为 0）。"""
        n_exp = getattr(estimator, "n_features_in_", None)
        if not n_exp or not X or len(X[0]) == n_exp:
            return X
        if len(X[0]) > n_exp:
            return [row[:n_exp] for row in X]
        pad = [0.0] * (n_exp - len(X[0]))
        return [row + pad for row in X]

    def sentence_probs(self, lang: str, sentences: list[str]) -> list[float] | None:
        """批量句级 has_error 概率（一次特征抽取 + 一次 predict_proba）；
        无可用模型返回 None（调用方回退启发式）。
        批量推理是 /l3-detect 的主路径：逐句 predict 的 O(句数) 开销
        是长文本延迟的主要来源。"""
        bundle = self.models.get(lang)
        if bundle is None or bundle.get("bin") is None or not sentences:
            return None
        import l3_finetune  # noqa: PLC0415

        X = [l3_finetune.extract_features(s, lang) for s in sentences]
        X = self._fit_features(bundle["bin"], X)
        try:
            probas = bundle["bin"].predict_proba(X)
        except Exception as e:  # noqa: BLE001
            # 推理失败回退启发式（而非 500 让引擎把 L3 当服务不可用）
            print(f"[feature-model] {lang} predict_proba 失败，回退启发式: {e}", file=sys.stderr)
            return None
        return [float(p[1] if len(p) > 1 else 0.0) for p in probas]

    def sentence_types(self, lang: str, sentences: list[str]) -> list[str] | None:
        """批量句级错误类型预测（一次 predict）；无可用模型返回 None"""
        bundle = self.models.get(lang)
        if bundle is None or bundle.get("types") is None or not sentences:
            return None
        import l3_finetune  # noqa: PLC0415

        X = [l3_finetune.extract_features(s, lang) for s in sentences]
        X = self._fit_features(bundle["types"], X)
        try:
            preds = bundle["types"].predict(X)
        except Exception as e:  # noqa: BLE001
            print(f"[feature-model] {lang} types predict 失败，忽略类型预测: {e}", file=sys.stderr)
            return None
        return [str(p) for p in preds]


model = FeatureModel()
if not MOCK:
    model.load()

# 加载混淆字词表（繁→简 100% 安全对）
_L3_LEXICON_DIR = os.environ.get(
    "GEC_LEXICON_DIR",
    os.path.join(os.path.dirname(__file__), "..", "lexicons"),
)
_lexicon_path = os.path.join(_L3_LEXICON_DIR, "zh_gec_lexicon.json")
_n_loaded = correct.load_confusables(_lexicon_path)
if _n_loaded:
    print(f"[feature-model] 加载 {_n_loaded} 条繁→简混淆字", file=sys.stderr)


def _heuristic_hits(sentence: str, lang: str) -> list:
    """模式命中（prob / type / span 定位三条启发式路径共用；
    每句**只算一次**，避免同一句重复扫模式库）"""
    try:
        return correct.find_hits(lang, sentence)
    except Exception:  # noqa: BLE001
        return []


# ─── N-gram 异常分（Phase 2.2；方案 B 扩表 + hard/soft 加权，实现见 correct.py）───
_NGRAM_TABLE: dict[str, float] | None = None
_BG_TABLE: frozenset = frozenset()  # 高频 bigram 表（soft OOV 判定，与 n-gram 同规则）
_NGRAM_LOCK = threading.Lock()  # 保护 _NGRAM_TABLE 的读改写与持久化

def _load_ngram_table() -> int:
    """加载 char 3-gram 频率表 + 高频 bigram 表（build_ngram_table.py 预构建）。返回条目数。"""
    global _NGRAM_TABLE, _BG_TABLE
    here = os.path.dirname(__file__)
    path = os.path.join(here, "ngram_table.json")
    if not os.path.isfile(path):
        return 0
    try:
        _NGRAM_TABLE, _BG_TABLE = correct.load_zh_ngram_tables(
            path, os.path.join(here, "bigram_table_zh.json"))
        return len(_NGRAM_TABLE)
    except (OSError, json.JSONDecodeError) as e:
        # 表损坏 = OOV 特征永久静默失效（review P2：必须可见）
        print(f"[feature-model] n-gram 表加载失败（OOV 特征失效，请检查或重建 {path}）: {e}", file=sys.stderr)
        return 0

def _ngram_oov_ratio(sentence: str) -> float:
    """加权 OOV 3-gram 比例（hard=1.0, soft=0.4，短句平滑；correct.zh_ngram_oov_ratio）"""
    if _NGRAM_TABLE is None:
        return 0.0
    return correct.zh_ngram_oov_ratio(sentence, _NGRAM_TABLE, _BG_TABLE)


def _rebuild_ngram_table() -> dict:
    """重建 n-gram 表：合并 base 语料 + confirmed clean 句子。
    
    返回 {"added": N, "total": M, "swapped": bool}
    """
    global _NGRAM_TABLE
    table_path = os.path.join(os.path.dirname(__file__), "ngram_table.json")
    
    # 加载现有表
    if _NGRAM_TABLE is None:
        _load_ngram_table()
    if _NGRAM_TABLE is None:
        _NGRAM_TABLE = {}
    
    before = len(_NGRAM_TABLE)
    
    # 从 confirmed.jsonl 读 clean 句子
    lang_dir = os.path.join(_l3_data_dir(), "zh")
    confirmed_path = os.path.join(lang_dir, "confirmed.jsonl")
    added = 0
    if os.path.isfile(confirmed_path):
        with open(confirmed_path, encoding="utf-8") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    row = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if int(row.get("label", 1)) != 0:
                    continue  # 只要 clean 句
                text = str(row.get("text", ""))
                zh = [c for c in text if "\u4e00" <= c <= "\u9fff"]
                for i in range(len(zh) - 2):
                    tri = zh[i] + zh[i+1] + zh[i+2]
                    if tri not in _NGRAM_TABLE:
                        _NGRAM_TABLE[tri] = 1.0
                        added += 1
    
    after = len(_NGRAM_TABLE)
    
    # 持久化：tmp + os.replace 原子写（review P2：直接 truncate 写，
    # 中途崩溃会损坏表 → OOV 特征静默失效）
    swapped = False
    if after > before:
        tmp_path = table_path + ".tmp"
        try:
            with open(tmp_path, "w", encoding="utf-8") as f:
                json.dump(_NGRAM_TABLE, f, ensure_ascii=False)
            os.replace(tmp_path, table_path)
            swapped = True
        except OSError as e:
            print(f"[feature-model] n-gram 表持久化失败: {e}", file=sys.stderr)
            try:
                os.remove(tmp_path)
            except OSError:
                pass
    
    return {"added": added, "total": after, "swapped": swapped}


def _rebuild_ngram_table_safe(blocking: bool = True) -> dict[str, Any] | None:
    """加锁执行 _rebuild_ngram_table（并发重建会互相覆盖）。

    blocking=False：锁被占用时返回 None（调用方按 409 处理）。"""
    if not blocking:
        if not _NGRAM_LOCK.acquire(blocking=False):
            return None
        try:
            return _rebuild_ngram_table()
        finally:
            _NGRAM_LOCK.release()
    with _NGRAM_LOCK:
        return _rebuild_ngram_table()


def _char_gate_scores(texts: list[str]) -> list[float] | None:
    """字级侧车批量打分（不可达/异常 → None 回退纯启发式）。"""
    if not CHAR_GATE_URL or not texts:
        return None
    _CHAR_GATE_STATS["calls"] += 1
    try:
        import urllib.request
        req = urllib.request.Request(
            CHAR_GATE_URL,
            data=json.dumps({"texts": texts[:256]}).encode(),
            headers={"Content-Type": "application/json"})
        with urllib.request.urlopen(req, timeout=5) as r:
            scores = json.loads(r.read())["scores"]
        if len(scores) != len(texts):
            _CHAR_GATE_STATS["fails"] += 1
            return None
        _CHAR_GATE_STATS["scored"] += len(texts)
        return scores
    except Exception:  # noqa: BLE001
        _CHAR_GATE_STATS["fails"] += 1
        return None


def _heuristic_prob(sentence: str) -> float:
    """逐句启发式错误概率（实现见 correct.zh_heuristic_prob，与漏斗评测共用）。

    主信号：hard/soft OOV 分档（方案 B：100 万 trigram 表 + 高频 bigram 加权）。
    已知盲区（review 2026-10-18 + 方案 B 校准）：OOV 只捕捉「表面异常」，
    扩表后是弱检测器（CSCD AUC≈0.59）。不产生 OOV 的错误（成分残缺、语序、
    搭配类）会落在低分区被放行——由 pattern 层与决策模型（C）兜底。
    """
    if _NGRAM_TABLE is None:
        return correct.zh_heuristic_prob(sentence, {}, frozenset())
    return correct.zh_heuristic_prob(sentence, _NGRAM_TABLE, _BG_TABLE)


# 加载 3-gram 频率表（Phase 2.2：字符级异常分）
_n_ngram = _load_ngram_table()
if _n_ngram:
    print(f"[feature-model] 加载 {_n_ngram} 条 char 3-gram 频率表", file=sys.stderr)


def _heuristic_type_from_hits(hits: list) -> str:
    """由模式命中推断错误类型（无命中 → grammar）"""
    if not hits:
        return "grammar"
    from collections import Counter

    dist = Counter(h.error_type for h in hits)
    return dist.most_common(1)[0][0]


def _errors_from_hits(hits: list, etype: str) -> list[dict[str, Any]]:
    """模式化 span 定位 + 机械修正（无命中时返回空 → 上层合成句级错误）；
    优先采用与预测类型一致的命中，否则取全部命中"""
    same = [h for h in hits if h.error_type == etype]
    chosen = same or hits
    out = []
    for h in chosen:
        item: dict[str, Any] = {
            "span": [h.span[0], h.span[1]],
            "error_type": h.error_type,
            "confidence": 0.9,
            "original": h.original,
            "correction": h.correction,
            "message": h.message,
        }
        out.append(item)
    return out


@app.get("/health")
async def health() -> JSONResponse:
    mode = "mock" if MOCK else ("model" if model.models else "heuristic-fallback")
    return JSONResponse({"status": "ok", "mode": mode})


@app.post("/l3-detect")
async def l3_detect(request: Request) -> JSONResponse:
    body = await request.json()
    text = str(body.get("text", ""))
    lang = str(body.get("lang", "en"))
    if not text.strip():
        return JSONResponse({"error": "text 不能为空"}, status_code=400)

    sentences = split_sentences(text)
    if not sentences:
        return JSONResponse(
            {"has_error": False, "has_error_prob": 0.0, "error_type": "", "abstain": False, "errors": []}
        )

    # 批量推理（真实模型模式）：全部句子一次特征抽取 + 一次 predict_proba，
    # 错误类型只对高置信句批量预测——避免逐句 predict 的 O(句数) 开销
    batch_probs: list[float] | None = None
    batch_types: dict[int, str] = {}
    if not MOCK:
        batch_probs = model.sentence_probs(lang, [s for _, _, s in sentences])
        if batch_probs is not None:
            hi = [i for i, p in enumerate(batch_probs) if p is not None and p >= ABSTAIN_THRESHOLD]
            if hi:
                types_pred = model.sentence_types(lang, [sentences[i][2] for i in hi])
                if types_pred is not None:
                    batch_types = {i: t for i, t in zip(hi, types_pred)}

    errors: list[dict[str, Any]] = []
    max_prob = 0.0
    n_abstained = 0
    primary_type = ""
    unreviewed: list[list[int]] = []  # 部分定案时未复查的句子 span（诚实上报）
    # 字级救援分数（D.2）：整段一次批量打分；None = 未启用/侧车不可达
    char_scores = _char_gate_scores([s for _, _, s in sentences]) if lang == "zh" else None

    for i, (start, end, sentence) in enumerate(sentences):
        # ── 模式优先：所有句子先查模式库（不依赖概率门控） ──
        # 注意：定案档 pattern 命中不计入 n_abstained——它已确定有错且可机械
        # 修正，不需要 LLM 参与。灰区档（advisory）命中与「无命中 + 概率灰区」
        # 同样计入弃权，交给 LLM 裁定。
        hits = _heuristic_hits(sentence, lang)
        safe_hits = [h for h in hits if not getattr(h, "advisory", False)]
        if safe_hits:
            # 定案档命中 → 直接定案（无需概率，无需 LLM）
            etype = batch_types.get(i) if not MOCK else None
            if not etype:
                etype = _heuristic_type_from_hits(safe_hits)
            if not primary_type:
                primary_type = etype
            span_errors = _errors_from_hits(safe_hits, etype)
            if span_errors:
                for e in span_errors:
                    e["span"] = [start + e["span"][0], start + e["span"][1]]
                    e["original"] = text[e["span"][0] : e["span"][1]]
                errors.extend(span_errors)
                continue  # 本句已定案，跳到下一句
            # 模式命中但无法生成 span 级修正 → 仍弃权（安全）
            n_abstained += 1
            unreviewed.append([start, end])
            continue
        if hits:
            # 仅灰区档命中（如「我在商场」误击「在→去」）：不定案，
            # 作为「该句可疑」信号强制进灰区 → LLM 裁定
            max_prob = max(max_prob, GRAY_ZONE_LOW)
            n_abstained += 1
            unreviewed.append([start, end])
            continue

        # ── 无模式命中 → 概率判定 ──
        prob: float | None = batch_probs[i] if batch_probs is not None else None
        if prob is None:
            prob = 0.4
        # 启发式概率替代（生产模式开启；评测模式 L3_HEURISTIC_PROB=0 关闭回退模型）
        if os.environ.get("L3_HEURISTIC_PROB", "1") == "1":
            prob = _heuristic_prob(sentence)
        max_prob = max(max_prob, prob)
        if prob < GRAY_ZONE_LOW:
            continue  # 明确无错
        if char_scores is not None and char_scores[i] < CHAR_GATE_TAU:
            _CHAR_GATE_STATS["rescued"] += 1
            continue  # 字级救援：启发式灰区但字级模型确证干净
        # 灰区 / 高置信但无模式命中 → 弃权交 LLM
        n_abstained += 1
        unreviewed.append([start, end])

    # ── 多数决：弃权比例 ≤ 阈值 → 部分定案；否则整段弃权 ──
    abstain_ratio = n_abstained / len(sentences) if sentences else 0
    max_abstain_ratio = float(os.environ.get("L3_ABSTAIN_RATIO", "0.80"))
    if abstain_ratio > max_abstain_ratio:
        # 超过 80% 句需 LLM 裁定 → 整段弃权（错误密集段保留全文语境复查；
        # 其余段部分定案 + 灰区句定向复查——A 已保证灰区句必审，无静默丢弃）
        return JSONResponse(
            {
                "has_error": False,
                "has_error_prob": max_prob,
                "error_type": "",
                "abstain": True,
                "errors": [],
            }
        )
    # ≤ 20% 弃权 → 接受部分定案。弃权句 span 通过 unreviewed_spans 上报，
    # Rust 引擎（gec-core）据此做**定向 LLM 复查**：只送灰区句（成本 ≈ 灰区
    # 句长度），定案部分不再送 LLM；LLM 未配置时灰区句本轮放行（记 debug 日志）。
    if errors:
        return JSONResponse(
            {
                "has_error": True,
                "has_error_prob": max_prob,
                "error_type": primary_type or (errors[0]["error_type"] if errors else ""),
                "abstain": False,
                "errors": errors,
                "unreviewed_spans": unreviewed,
            }
        )
    return JSONResponse(
        {
            "has_error": False,
            "has_error_prob": max_prob,
            "error_type": "",
            "abstain": False,
            "errors": [],
            "unreviewed_spans": unreviewed,
        }
    )


# ─── 飞轮（自学习闭环）：确认样本汇入 → 重训练 → 热切换 ─────────────────────
#
# Rust 侧（api-server / desktop-app）把「确认采纳的 LLM 检测结果」（正样本）
# 与「LLM 复查后确认干净的句子」（负样本）持续汇入 l3_data/<lang>/confirmed.jsonl；
# 积累到阈值后 Rust 侧提示可重训练（前端设置界面「重训练」按钮 → 本端点）：
#   1. 合并 confirmed 样本进训练集（重抽特征）
#   2. 重训练（vegaml 可用则用之，否则 sklearn 回退）
#   3. dev 集评测：整体 F1 未显著退化（< 0.02）且逐错误类型召回未显著退化
#      （防飞轮固化 LLM 盲区）→ 热切换（内存 + 磁盘）；退化 → 保留旧模型
#
# 与二期 anomaly 蒸馏同源的自学习思路：特征模型随使用变强，LLM 长尾持续收缩。

RETRAIN_LOCK = threading.Lock()
def _l3_data_dir() -> str:
    """飞轮数据目录：`L3_DATA_DIR` 环境变量覆盖；缺省 = 本服务目录下的 `l3_data`。

    Rust 采集端（api-server / desktop-app）的缺省目录与解析逻辑见
    gec-core `default_l3_data_dir`（开发 = 仓库内 `feature-model/l3_data`，
    打包应用 = `~/.langfixer/l3_data`）；两侧目录不一致时重训读不到新样本，
    部署时可用 `L3_DATA_DIR`（本服务）与 `GEC_L3_DATA_DIR`（Rust 侧）对齐。
    """
    d = os.environ.get("L3_DATA_DIR") or os.path.join(os.path.dirname(__file__), "l3_data")
    os.makedirs(d, exist_ok=True)
    return d


RETRAIN_META = os.path.join(_l3_data_dir(), "retrain_meta.json")
# F1 退化超过该幅度 → 拒绝切换（保留旧模型）
RETRAIN_REGRESSION_LIMIT = 0.02
# 逐错误类型召回退化门：某类召回下降超过该幅度 → 拒绝切换。
# L3 负样本来自「LLM 复查后确认干净」——若 LLM 对某类错误存在系统性盲区，
# 这些句子会成为 L3 的训练负样本，重训后的模型继承该盲区；
# 整体 F1 门（0.02 容差）无法保护逐类漂移，故加逐类召回门。
PER_CLASS_REGRESSION_LIMIT = 0.05
# 参与逐类门的最小 dev 样本数（样本太少时召回波动大，不具统计意义）
PER_CLASS_MIN_SAMPLES = 5


def _per_class_regressions(
    old_report: dict[str, Any] | None, new_report: dict[str, Any] | None
) -> list[str]:
    """逐错误类型召回退化检查：返回退化描述列表（空 = 无退化）"""
    if not old_report or not new_report:
        return []
    old_pc: dict[str, dict[str, float]] = old_report.get("per_class") or {}
    new_pc: dict[str, dict[str, float]] = new_report.get("per_class") or {}
    out: list[str] = []
    for et, old_stats in old_pc.items():
        if old_stats.get("n", 0) < PER_CLASS_MIN_SAMPLES:
            continue
        new_stats = new_pc.get(et)
        if not new_stats:
            continue
        if new_stats["recall"] < old_stats["recall"] - PER_CLASS_REGRESSION_LIMIT:
            out.append(f"{et} 召回 {old_stats['recall']:.2f}→{new_stats['recall']:.2f}")
    return out


def _load_confirmed(lang: str) -> list[dict[str, Any]]:
    path = os.path.join(_l3_data_dir(), lang, "confirmed.jsonl")
    if not os.path.isfile(path):
        return []

    rows: list[dict[str, Any]] = []
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line:
                rows.append(json.loads(line))
    return rows


def _read_retrain_meta() -> dict[str, Any]:
    if os.path.isfile(RETRAIN_META):
        try:
            with open(RETRAIN_META, encoding="utf-8") as f:
                return json.load(f)
        except Exception:  # noqa: BLE001
            pass
    return {}


def _write_retrain_meta(meta: dict[str, Any]) -> None:
    os.makedirs(os.path.dirname(RETRAIN_META), exist_ok=True)
    with open(RETRAIN_META, "w", encoding="utf-8") as f:
        json.dump(meta, f, ensure_ascii=False, indent=2)


@app.post("/l3-retrain")
async def l3_retrain(request: Request) -> JSONResponse:
    """飞轮重训练：合并 confirmed 样本 → 重训练 → dev 评测 → F1 未退化则热切换"""
    body = await request.json()
    lang = str(body.get("lang", "en"))
    if lang not in ("en", "zh"):
        return JSONResponse({"error": f"不支持的语言: {lang}"}, status_code=400)
    if not RETRAIN_LOCK.acquire(blocking=False):
        return JSONResponse({"error": "重训练进行中，请稍后再试"}, status_code=409)
    try:
        import l3_finetune
        from l3_finetune import compute_f1

        data_dir = os.path.join(_l3_data_dir(), lang)
        confirmed = _load_confirmed(lang)

        # ① 合并训练集：base（gold，特征或原始格式）+ confirmed（原始文本 → 重抽特征）
        base_src = os.path.join(data_dir, "train.features.jsonl")
        if not os.path.exists(base_src):
            base_src = os.path.join(data_dir, "train.jsonl")
        if not os.path.exists(base_src):
            return JSONResponse(
                {"error": f"找不到基准训练集: {data_dir}/train*.jsonl（先运行 prepare_data + extract）"},
                status_code=409,
            )
        base_rows = l3_finetune.load_rows(base_src)
        merged = list(base_rows)
        for r in confirmed:
            text = str(r.get("text", "")).strip()
            if not text:
                continue
            merged.append(
                {
                    "x": l3_finetune.extract_features(text, lang),
                    "y": 1 if int(r.get("label", 1)) == 1 else 0,
                    "error_type": r.get("error_type") or "grammar",
                    "text": text,
                }
            )
        merged_path = os.path.join(data_dir, "train.merged.jsonl")
        with open(merged_path, "w", encoding="utf-8") as f:
            for r in merged:
                f.write(json.dumps(r, ensure_ascii=False) + "\n")

        # ② 重训练（vegaml 优先，回退 sklearn）
        out_dir = os.path.join(MODELS_DIR, lang)
        X = [r["x"] for r in merged]
        y = [r["y"] for r in merged]
        vegaml_used = l3_finetune._train_vegaml(X, y, out_dir)

        # ③ dev 评测：vegaml 路径无法本地评测 → 跳过对比直接切换；
        #    sklearn 路径 F1 显著退化 → 保留旧模型
        dev_path = os.path.join(data_dir, "dev.features.jsonl")
        if not os.path.exists(dev_path):
            dev_path = os.path.join(data_dir, "dev.jsonl")
        old_report = compute_f1(model.models.get(lang) or {}, dev_path)
        new_report = None
        swapped = False
        if vegaml_used:
            swapped = True
        else:
            import joblib

            new_bundle = joblib.load(os.path.join(out_dir, "model.joblib"))
            new_report = compute_f1(new_bundle, dev_path)
            class_regressions = _per_class_regressions(old_report, new_report)
            overall_regressed = (
                old_report is not None
                and new_report is not None
                and new_report["f1"] < old_report["f1"] - RETRAIN_REGRESSION_LIMIT
            )
            if new_report is None or overall_regressed or class_regressions:
                if new_report is None:
                    reason = "dev 集缺失无法评测"
                elif class_regressions:
                    reason = "逐错误类型召回显著退化（防盲区固化）: " + "，".join(class_regressions)
                else:
                    reason = "整体 F1 显著退化"
                return JSONResponse(
                    {
                        "swapped": False,
                        "reason": reason,
                        "before_f1": old_report["f1"] if old_report else None,
                        "after_f1": new_report["f1"] if new_report else None,
                        "per_class_regressions": class_regressions,
                        "samples": {"confirmed": len(confirmed), "base": len(base_rows), "total": len(merged)},
                    }
                )
            model.models[lang] = new_bundle
            swapped = True

        # ④ 记录元数据（Rust 侧 /l3/status 读取「上次重训练时间」；RFC 3339 带冒号时区）
        from datetime import datetime

        meta = _read_retrain_meta()
        meta[lang] = {
            "ts": datetime.now().astimezone().isoformat(timespec="seconds"),
            "confirmed": len(confirmed),
            "total": len(merged),
            "f1": new_report["f1"] if new_report else None,
            "swapped": swapped,
        }
        _write_retrain_meta(meta)
        
        # ⑤ n-gram 表：默认不自动重建（表膨胀会降低 OOV 判别力）。
        #    如需手动触发，调用 POST /l3-rebuild-ngram（独立端点）。
        #    仅在 L3_NGRAM_AUTO_REBUILD=1 环境变量开启时才自动重建。
        ngram_result = {"added": 0, "total": 0, "swapped": False}
        if lang == "zh" and os.environ.get("L3_NGRAM_AUTO_REBUILD", "0") == "1":
            r = _rebuild_ngram_table_safe(blocking=True)
            ngram_result = r or {"added": 0, "total": 0, "swapped": False}
            if ngram_result["added"] > 0:
                print(
                    f"[feature-model] n-gram 表更新: +{ngram_result['added']} → {ngram_result['total']} 条",
                    file=sys.stderr,
                )
        
        print(
            f"[feature-model] retrain {lang}: confirmed={len(confirmed)} total={len(merged)} "
            f"before_f1={old_report['f1'] if old_report else 'N/A'} "
            f"after_f1={new_report['f1'] if new_report else 'N/A'} swapped={swapped}",
            file=sys.stderr,
        )
        return JSONResponse(
            {
                "swapped": swapped,
                "model": (model.models.get(lang) or {}).get("model", "feature-0.1"),
                "before_f1": old_report["f1"] if old_report else None,
                "after_f1": new_report["f1"] if new_report else None,
                "samples": {"confirmed": len(confirmed), "base": len(base_rows), "total": len(merged)},
                "ngram": ngram_result,
            }
        )
    except Exception as e:  # noqa: BLE001
        print(f"[feature-model] retrain {lang} 失败: {e}", file=sys.stderr)
        return JSONResponse({"error": f"重训练失败: {e}"}, status_code=500)
    finally:
        RETRAIN_LOCK.release()


@app.post("/l3-rebuild-ngram")
async def l3_rebuild_ngram() -> JSONResponse:
    """独立重建 n-gram 表（合并 confirmed clean 句子）。
    
    与 /l3-retrain 的区别：只做 n-gram 表更新，不重训练 21 维模型。
    可在飞轮数据积累后随时调用，无需等 500 条阈值。
    并发保护：重建进行中再次调用 → 409（避免并发读改写互相覆盖）。
    注意：表膨胀会稀释 OOV 判别力，仅在确认 confirmed 质量后手动触发。
    """
    result = _rebuild_ngram_table_safe(blocking=False)
    if result is None:
        return JSONResponse({"error": "n-gram 表重建进行中，请稍后再试"}, status_code=409)
    if result["added"] > 0:
        print(f"[feature-model] n-gram 表更新: +{result['added']} → {result['total']} 条", file=sys.stderr)
    return JSONResponse(result)


@app.get("/l3/status")
async def l3_status() -> JSONResponse:
    """当前模型状态：模式 + 各语言 dev F1（前端设置界面「训练飞轮」卡片展示）

    诚实化（review P0-2）：prob_source 标明句级概率的真实来源——
    L3_HEURISTIC_PROB=1（默认）时模型概率被启发式覆盖，模型仅参与
    错误类型预测；「模型已加载」≠「模型概率在生效」。"""
    from l3_finetune import compute_f1

    data_dir = _l3_data_dir()
    heuristic_prob = os.environ.get("L3_HEURISTIC_PROB", "1") == "1"
    models: dict[str, Any] = {}
    for lang in ("en", "zh"):
        bundle = model.models.get(lang)
        dev = os.path.join(data_dir, lang, "dev.features.jsonl")
        if not os.path.exists(dev):
            dev = os.path.join(data_dir, lang, "dev.jsonl")
        report = compute_f1(bundle or {}, dev) if bundle else None
        bin_model = (bundle or {}).get("bin")
        n_exp = getattr(bin_model, "n_features_in_", None)
        models[lang] = {
            "model": "feature-0.1",
            "f1": report["f1"] if report else None,
            "loaded": bin_model is not None,
            # 模型期望特征维 vs 当前特征维（不一致时推理端自动截断/补零适配）
            "n_features_expected": n_exp,
            "n_features_current": len(_FEATURE_NAMES),
            "prob_used_in_detect": bin_model is not None and not heuristic_prob and not MOCK,
            "types_used_in_detect": (bundle or {}).get("types") is not None and not MOCK,
        }
    return JSONResponse(
        {
            "mode": "mock" if MOCK else ("model" if model.models else "heuristic-fallback"),
            "prob_source": "heuristic" if (heuristic_prob or MOCK) else "model",
            "models": models,
            "retrain_meta": _read_retrain_meta(),
            "char_gate": {
                "enabled": bool(CHAR_GATE_URL),
                "url": CHAR_GATE_URL,
                "tau": CHAR_GATE_TAU,
                **_CHAR_GATE_STATS,
            },
        }
    )


if __name__ == "__main__":
    mode_desc = "MOCK 模式" if MOCK else f"模型模式 {len(model.models)} 个模型"
    print(
        f"[feature-model] 监听 http://{HOST}:{PORT} ({mode_desc})",
        file=sys.stderr,
    )
    uvicorn.run(app, host=HOST, port=PORT, log_level="warning")