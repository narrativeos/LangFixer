import sys, json, os, re, tempfile
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from vega_finetune import _next_version, _write_meta, FEATURE_NAMES
from server import _model_version

# 场景 1：旧 meta（无 version 字段）→ _next_version 给 v1；_model_version 视为未知（None）
d = tempfile.mkdtemp()
with open(os.path.join(d, 'meta.json'), 'w') as f:
    json.dump({'backend': 'sklearn', 'feature_names': ['a']}, f)
assert _next_version(d) == 'v1', _next_version(d)
assert _model_version(d) is None, _model_version(d)

# 场景 2：连续训练 3 次 → v2, v3, v4
_write_meta(d, 'sklearn', _next_version(d))
assert _next_version(d) == 'v2', _next_version(d)
assert _model_version(d) == 'v1', _model_version(d)
_write_meta(d, 'sklearn', _next_version(d))
assert _next_version(d) == 'v3', _next_version(d)
_write_meta(d, 'sklearn', _next_version(d))
assert _next_version(d) == 'v4', _next_version(d)

# 场景 3：空目录（首次训练）→ v1
d2 = tempfile.mkdtemp()
assert _next_version(d2) == 'v1'
assert _model_version(d2) is None  # 无 meta → 版本未知

# 场景 4：meta 损坏 → v1 / 未知
d3 = tempfile.mkdtemp()
with open(os.path.join(d3, 'meta.json'), 'w') as f:
    f.write('{bad')
assert _next_version(d3) == 'v1'
assert _model_version(d3) is None

# 场景 5：meta 内容更新 + trained_at 为 ISO 8601 带冒号时区
from vega_finetune import FEATURE_NAMES
m = json.load(open(os.path.join(d, 'meta.json')))
assert m['feature_names'] == FEATURE_NAMES, m
assert m['version'] == 'v3'
assert re.match(r'^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}[+-]\d{2}:\d{2}$', m['trained_at']), m['trained_at']
print('ALL_VERSION_TESTS_PASSED')

