import importlib.util, json, sys, hashlib
from pathlib import Path

def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m

REPO = Path(__file__).resolve().parents[2]
ROOT = REPO / 'tools'
gs = load('gsERVE', ROOT / 'gold-adjudicate' / 'serve.py')
ls = load('lsERVE', ROOT / 'label-adjudicate' / 'serve.py')

def key(iid):
    return hashlib.md5(('42|' + iid).encode()).hexdigest()

want = sys.argv[1]
out = []
if want in ('auto_real', 'auto_noise'):
    d = gs.Data(Path.home() / 'tmp/grey_zone', Path.home() / 'tmp', Path.home() / 'tmp/gold_adjudicate')
    its = [x for x in d.items if x['bucket'] == want]
    its.sort(key=lambda x: key(x['iid']))
    for x in its[:58]:
        g = x['gold']
        extra = 'gold:' + g['op'] + '->' + g['ref']
        if x['v21fix']:
            extra += ' model:' + x['v21fix']['type'] + ':' + x['v21fix']['fix']
        ctx = x['ctx']
        if len(ctx) > 90:
            p = ctx.find('\u27e6')
            ctx = ctx[max(0, p - 40):p + 50]
        out.append('\t'.join([x['iid'], ctx, extra]))
else:
    d = ls.Data(Path.home() / 'tmp/dataset_v22', Path.home() / 'tmp/dataset_v22',
                REPO / 'lexicons' / 'zh_gec_lexicon.json')
    iids = set(d.sample_ids(want, 58))
    for x in d.items:
        if x['iid'] not in iids:
            continue
        a, b = x['a'], x['b']
        src = x['src']
        win = src[max(0, a - 14):a] + '\u27e6' + (src[a:b] or '\u2205') + '\u27e7' + src[b:b + 14]
        why = (x['why'] or '')[:60].replace('\t', ' ')
        out.append('\t'.join([x['iid'], x['type'], x['signal'] or '-', win, '\u2192' + x['fix'], why]))
print('\n'.join(out))
