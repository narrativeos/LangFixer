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
# 38-58：除 sig14-5616（句号+「如」合法，非误用）外均判 adopt：
# 4-4并列短语顿号(兴旺发达、长治久安)、半角.→。、随着A、B顿号、--→——、!。删重、
# 谓语并列顿号、引用引导逗号、！”后不加逗号(GB15834)、引文讹字多标点删正、
# 因为后逗号删、和X、Y顿号、为了X，Y逗号、因果逗号、蒜姜/朋友同学/地名/动宾并列顿号、
# 除了A、B以外顿号、电影名书名号、引文句号后不加点、省略号规范
part2 = ['nacgec-14815#64#65','nacgec-15571#64#65','nacgec-15737#12#12','nacgec-15798#29#31',
         'nacgec-16069#30#32','nlpcc23-7084#17#21','nlpcc23-7151#6#6','sig13-5095#44#45',
         'sig13-5276#37#43','sig13-5435#100#101','sig13-5538#54#55','sig14-5602#30#31',
         'sig14-5616#21#22','sig14-5754#8#9','sig14-6125#51#52','sig14-6179#45#46',
         'sig14-6197#38#43','sig14-6324#12#16','sig14-6334#23#24','sig14-6477#22#23',
         'sig14-6497#36#39']
assert set(part2) <= sample and len(set(part2)) == 21
reject = {'sig14-5616#21#22'}
now = int(time.time())
with open(lab_dir / 'adjudications.jsonl', 'a', encoding='utf-8') as f:
    for i in part2:
        v = 'reject' if i in reject else 'adopt'
        f.write(json.dumps({'iid': i, 'verdict': v,
                            'reason': 'agent抽检 auto seed=42', 'at': now}, ensure_ascii=False) + '\n')
print('label auto part2 记录', len(part2), 'reject', len(reject))
