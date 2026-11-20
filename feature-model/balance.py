"""重训增量配平：飞轮 confirmed 绝大多数是 clean 样本（实测 527:7），
整份并入 21 维模型训练集会教出「见句就判干净」的漏报倾向。error 样本
恒全保留；clean 按 1 error : RATIO clean 封顶、不足 error 时保底 FLOOR 条；
超出时按文档序等距保留（确定性、跨时间均匀铺开，末条必留——它最新）。

口径与 server.l3_retrain 一致：label==1 为 error，其余（含缺省）为 clean；
缺 label 的行沿用 server.py 的默认（按 error 处理，不静默丢弃）。
"""

from __future__ import annotations

from typing import Any

RATIO = 5
FLOOR = 20


def balance_confirmed(
    rows: list[dict[str, Any]], ratio: int = RATIO, floor: int = FLOOR
) -> tuple[list[dict[str, Any]], dict[str, int]]:
    errors = [r for r in rows if int(r.get("label", 1)) == 1]
    clean = [r for r in rows if int(r.get("label", 1)) != 1]
    cap = max(ratio * len(errors), floor)
    if len(clean) <= cap:
        kept_clean = clean
    else:
        if cap <= 1:
            kept_clean = clean[-1:]
        else:
            step = (len(clean) - 1) / (cap - 1)
            kept_clean = [clean[i] for i in sorted({round(j * step) for j in range(cap)})]
    info = {
        "error_kept": len(errors),
        "clean_in": len(clean),
        "clean_kept": len(kept_clean),
        "clean_cap": cap,
    }
    return errors + kept_clean, info
