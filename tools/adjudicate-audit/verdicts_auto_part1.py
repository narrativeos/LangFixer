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
sample = set(d.sample_ids('auto', 58))
dump_order = ['cctc-7738#2#24','cctc-7965#20#26','cctc-8420#89#103','cctc-8445#22#63','cctc-8624#6#10','cctc-8674#17#18','cctc-8696#4#10','cctc-9038#39#40','cctc-9130#14#31','cctc-9447#90#91','cctc-9710#2#34','cctc-9933#4#5','cscd-1980#2#14','cscd-2041#32#115','cscd-2062#40#54','cscdns-17845#17#20','cscdns-18295#43#43','cscdns-18679#25#26','cscdns-18733#41#42','cscdns-19674#17#17','cscdns-20245#21#21','cscdns-20605#8#11','cscdns-21768#11#13','cscdns-22361#15#19','ctc21-3827#12#13','ctc21-3877#29#29','ctc21-3881#52#53','ctc21-4052#17#20','ctc21-4260#81#82','ctc21-4564#1#12','midu-3246#5#6','midu-3277#17#17','mucgec-10257#18#18','nacgec-13288#34#35','nacgec-13643#20#21','nacgec-13780#37#38','nacgec-14071#26#27']
assert set(dump_order) <= sample, set(dump_order) - sample
reject = {'cscdns-18679#25#26', 'cscdns-19674#17#17', 'cscdns-20245#21#21'}
unc = {'ctc21-3827#12#13', 'ctc21-4564#1#12', 'midu-3246#5#6'}
now = int(time.time())
with open(lab_dir / 'adjudications.jsonl', 'a', encoding='utf-8') as f:
    for i in dump_order:
        v = 'reject' if i in reject else ('uncertain' if i in unc else 'adopt')
        f.write(json.dumps({'iid': i, 'verdict': v,
                            'reason': 'agent抽检 auto seed=42', 'at': now}, ensure_ascii=False) + '\n')
print('label auto part1 记录', len(dump_order),
      'adopt', len(dump_order) - len(reject) - len(unc), 'reject', len(reject), 'uncertain', len(unc))
