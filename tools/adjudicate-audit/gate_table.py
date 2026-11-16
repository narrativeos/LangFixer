import json, importlib.util
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]

def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m

gserv = load('gserv', str(REPO / 'tools' / 'gold-adjudicate' / 'serve.py'))
lserv = load('lserv', str(REPO / 'tools' / 'label-adjudicate' / 'serve.py'))

GMIN, GMAX = 58, 0.02

print('== gold (: grey_zone) ==')
gd = gserv.Data(Path.home()/'tmp/grey_zone', Path.home()/'tmp', Path.home()/'tmp/gold_adjudicate')
try:
    stats = gd.bucket_stats()
except Exception as e:
    stats = None; print('bucket_stats fail:', e)
if stats:
    for b in ('auto_real','auto_noise','manual','mark'):
        if b in stats:
            s = stats[b]
            print(b, s)

print('== label (: dataset_v22) ==')
ld = lserv.Data(Path.home()/'tmp/dataset_v22', Path.home()/'tmp/dataset_v22',
                REPO / 'lexicons' / 'zh_gec_lexicon.json')
try:
    stats = ld.bucket_stats()
except Exception as e:
    stats = None; print('bucket_stats fail:', e)
if stats:
    for b in ('auto','must','audit'):
        if b in stats:
            print(b, stats[b])
