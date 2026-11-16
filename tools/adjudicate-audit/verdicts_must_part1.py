import json, time
from pathlib import Path
import importlib.util
spec = importlib.util.spec_from_file_location(
    'lserv', str(Path(__file__).resolve().parents[2] / 'tools' / 'label-adjudicate' / 'serve.py'))
lserv = importlib.util.module_from_spec(spec)
spec.loader.exec_module(lserv)
lab_dir = Path.home() / 'tmp/dataset_v22'
d = lserv.Data(lab_dir, lab_dir,
               Path(__file__).resolve().parents[2] / 'lexicons' / 'zh_gec_lexicon.json')
sample = set(d.sample_ids('must', 58))
order1 = ['cctc-8232#39#41','cctc-8296#5#12','cctc-8630#16#31','cctc-8768#13#14','cctc-8961#10#18',
'cctc-9056#12#30','cctc-9664#14#33','cscd-535#20#20','cscd-602#2#18','cscd-740#15#16','cscd-1532#24#28',
'cscd-1884#17#20','cscdns-17688#79#82','cscdns-17799#36#36','cscdns-19356#14#15','cscdns-20426#4#5',
'cscdns-20529#22#23','cscdns-21195#33#40','cscdns-21557#52#53','cscdns-21661#25#26','cscdns-21915#48#48',
'ctc21-3852#14#16','ctc21-3970#15#17','ctc21-4138#34#34','ctc21-4182#14#18','ctc21-4196#25#32',
'ctc21-4390#14#14','midu-2901#5#6','midu-3187#24#25','mucgec-10273#0#0','mucgec-10556#18#19',
'mucgec-10784#26#26','nacgec-11146#38#38','nacgec-11661#22#22','nacgec-11810#47#60','nacgec-12327#44#45']
assert set(order1) <= sample, set(order1) - sample
reject = {'cscdns-17799#36#36','cscdns-19356#14#15','cscdns-21915#48#48','ctc21-4138#34#34',
          'midu-3187#24#25','mucgec-10784#26#26'}
unc = {'nacgec-12327#44#45'}
now = int(time.time())
with open(lab_dir / 'adjudications.jsonl', 'a', encoding='utf-8') as f:
    for i in order1:
        v = 'reject' if i in reject else ('uncertain' if i in unc else 'adopt')
        f.write(json.dumps({'iid': i, 'verdict': v,
                            'reason': 'agent抽检 must seed=42', 'at': now}, ensure_ascii=False) + '\n')
print('label must part1 记录', len(order1), 'reject', len(reject), 'uncertain', len(unc))
