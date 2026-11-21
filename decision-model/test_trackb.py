"""契约 v2（trackb 双头）结构不变式测试。

统计行为（召回/FP 双闸）由 jev-gec locate_eval 北极星评测负责，本测试只锁契约：
- 门控产出语义：p≥.5 必有 errors（字级门控通过→字级条目；否则句级合成）；
  p<.5 必须空 errors（适配器据此 abstain）；
- 字级条目 span 为合法字符偏移（非零宽、在文本内）、error_type=missing；
- 门控阈值可经 meta.json `gates` 生效。

真实产物缺失时跳过（models/zh/head.pt 不入库）。
"""
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

MODEL_DIR = os.path.join(HERE, "models", "zh")
if not os.path.isfile(os.path.join(MODEL_DIR, "head.pt")):
    print("SKIP: models/zh/head.pt 缺失（trackb 产物未晋级）")
    sys.exit(0)

from server import _TrackBPredictor  # noqa: E402

gates = {"sent": 0.95, "char": 0.3}
meta_path = os.path.join(MODEL_DIR, "meta.json")
if os.path.isfile(meta_path):
    with open(meta_path, encoding="utf-8") as f:
        gates.update(json.load(f).get("gates", {}))

p = _TrackBPredictor(MODEL_DIR, gates)

sentences = [
    "武汉大学图书馆以其历史悠久、藏书丰富而闻名于世。",
    "他昨天去图馆借书，发现借阅卡没有带。",
    "全校现有图书馆馆舍面积为77,389平方米，资料室为14,242平方米。",
    "图书馆前身为19世纪末湖广总督张之洞创办的湖北自强学堂图书室。",
]

for s in sentences:
    r = p.predict(s)
    score, errors = r["score"], r["errors"]
    assert 0.0 <= score <= 1.0, r
    if score >= 0.5:
        assert errors, f"score={score} 过句级阈但 errors 为空: {s}"
    else:
        assert not errors, f"score={score} 低于阈却产出 errors（破坏 abstain 语义）: {s}"
    for e in errors:
        a, b = e["span"]
        assert e["error_type"] in ("missing", "grammar"), e
        assert 0 <= a < b <= len(s), f"span 非法 {e['span']}: {s}"
        assert 0.0 <= e["confidence"] <= 1.0, e
        # 门控一致性：字级条目只能在句头过门控时出现
        if e["error_type"] == "missing":
            assert score >= gates["sent"], f"句头 {score} 未过 {gates['sent']} 却有字级条目"

# 门控生效：char_gate 拉到 1.1 → 字级条目必然消失，只剩句级合成（p≥.5 时）
p_harsh = _TrackBPredictor(MODEL_DIR, {"sent": gates["sent"], "char": 1.1})
for s in sentences:
    r = p_harsh.predict(s)
    assert all(e["error_type"] != "missing" for e in r["errors"]), r
print("ALL_TRACKB_TESTS_PASSED")
