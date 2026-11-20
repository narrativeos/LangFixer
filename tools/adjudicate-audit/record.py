import json, sys, time, hashlib
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]

def key(iid):
    return hashlib.md5(('42|' + iid).encode()).hexdigest()

gold_dir = Path.home() / 'tmp/gold_adjudicate'
lab_dir = Path.home() / 'tmp/dataset_v22'

def gold_sample(bucket, n=58):
    sys.path.insert(0, str(REPO / 'tools' / 'gold-adjudicate'))
    import serve
    d = serve.Data(Path.home() / 'tmp/grey_zone', Path.home() / 'tmp', gold_dir)
    its = [x for x in d.items if x['bucket'] == bucket]
    its.sort(key=lambda x: key(x['iid']))
    return [x['iid'] for x in its[:n]], d

def append(path, recs):
    with open(path, 'a', encoding='utf-8') as f:
        for r in recs:
            f.write(json.dumps(r, ensure_ascii=False) + '\n')

cmd = sys.argv[1]
if cmd == 'gold-auto-real':
    iids, _ = gold_sample('auto_real')
    flip = set(sys.argv[2].split(','))
    now = int(time.time())
    recs = [{'iid': i, 'verdict': 'noise' if i in flip else 'real',
             'reason': 'agent抽检 auto_real seed=42' + ('·翻判' if i in flip else ''), 'at': now}
            for i in iids if i not in flip or True]
    assert len(iids) == 58, len(iids)
    append(gold_dir / 'gold_verdicts_journal.jsonl', recs)
    print('gold auto_real 记录', len(recs), '翻判', len(flip))

if cmd == 'gold-auto-noise':
    iids, _ = gold_sample('auto_noise')
    flip = set(sys.argv[2].split(','))
    missing = flip - set(iids)
    assert not missing, f'翻判id不在样本内: {missing}'
    assert len(iids) == 58, len(iids)
    now = int(time.time())
    recs = [{'iid': i, 'verdict': 'real' if i in flip else 'noise',
             'reason': 'agent抽检 auto_noise seed=42' + ('·翻判' if i in flip else ''), 'at': now}
            for i in iids]
    append(gold_dir / 'gold_verdicts_journal.jsonl', recs)
    print('gold auto_noise 记录', len(recs), '翻判real', len(flip))

if cmd == 'label-auto-partial':
    import importlib.util
    spec = importlib.util.spec_from_file_location(
        'lserv', str(REPO / 'tools' / 'label-adjudicate' / 'serve.py'))
    lserv = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(lserv)
    d = lserv.Data(Path.home() / 'tmp/dataset_v22', lab_dir,
                   REPO / 'lexicons' / 'zh_gec_lexicon.json')
    sample = d.sample_ids('auto', 58)
    by_iid = {x['iid']: x for x in d.items}
    for i in sample:
        assert i in by_iid, i
    adopt_l, reject_l, unc_l = (set(s.split(',')) if s else set() for s in sys.argv[2:5])
    decided = adopt_l | reject_l | unc_l
    assert decided <= set(sample), decided - set(sample)
    now = int(time.time())
    recs = []
    for i in sample:
        if i not in decided:
            continue
        v = 'adopt' if i in adopt_l else ('reject' if i in reject_l else 'uncertain')
        recs.append({'iid': i, 'verdict': v, 'reason': 'agent抽检 auto seed=42', 'at': now})
    append(lab_dir / 'adjudications.jsonl', recs)
    print('label auto 记录', len(recs), 'adopt', len(adopt_l), 'reject', len(reject_l), 'uncertain', len(unc_l))
