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
sample = set(d.sample_ids('audit', 58))
verdicts = [
 ('midu-3368#59#59',    'reject',   '插入残片复读 现已在句中'),
 ('midu-3400#4#4',      'reject',   '插入残片复读 2022已完整'),
 ('mucgec-10450#16#18', 'adopt',    '讲你们删后通顺'),
 ('mucgec-10482#22#22', 'adopt',    '1985年后补年'),
 ('mucgec-10628#32#33', 'adopt',    '航途应为旅途'),
 ('mucgec-10922#1#1',   'reject',   '插入残片复读 走已在句中'),
 ('nacgec-13339#34#35', 'reject',   '赢利同盈利异体非错'),
 ('nacgec-13621#36#39', 'uncertain','蕴藏方法可通 改应对存疑 50/50'),
 ('nacgec-14238#33#34', 'uncertain','90后的天之骄子非错 删的stylistic 50/50'),
 ('nacgec-15277#20#20', 'adopt',    '开展学雷锋活动补谓语'),
 ('nacgec-15445#19#20', 'adopt',    '由己到家 已应为己'),
 ('nacgec-15469#38#38', 'adopt',    '登记制度补中心语'),
 ('nacgec-15606#0#10',  'reject',   '替换掉确字断确认 off-point'),
 ('nacgec-15864#58#58', 'reject',   '主语后插句号致残句'),
 ('nacgec-16490#20#26', 'adopt',    '清华大学生多字删'),
 ('nacgec-16869#56#56', 'uncertain','补的国家若缺成为反致搭配 50/50'),
 ('nacgec-17069#60#60', 'reject',   '插入残片复读 文句已在句中'),
 ('nlpcc23-7470#7#10',  'adopt',    '良好的交流环境定语语序'),
 ('sig13-4663#1#1',     'reject',   '插入残片复读 风已在句中'),
 ('sig13-4877#42#53',   'adopt',    '必须+生活其中实改'),
 ('sig13-4882#12#12',   'adopt',    '我的桌椅漏的'),
 ('sig14-5750#31#51',   'reject',   '照字复读前缀 语序部分未修好'),
 ('sig14-5930#14#31',   'adopt',    '补说+出去语序'),
 ('sig14-5932#5#6',     'adopt',    '听考应为听写'),
 ('sig14-6074#7#54',    'adopt',    '商品+删关系+听到多改均实'),
 ('sig14-6396#12#13',   'adopt',    '听听看赘余删看'),
 ('sig14-6496#20#22',   'adopt',    '从…起就搭配实改'),
 ('sig14-6550#56#57',   'uncertain','fix渲染不明 删有还是换好好 50/50'),
 ('sig14-6601#8#9',     'uncertain','写出来非错 改描述stylistic 50/50'),
]
ids = [v[0] for v in verdicts]
assert set(ids) <= sample, set(ids) - sample
assert len(set(ids)) == 29
now = int(time.time())
with open(lab_dir / 'adjudications.jsonl', 'a', encoding='utf-8') as f:
    for iid, v, why in verdicts:
        f.write(json.dumps({'iid': iid, 'verdict': v,
                            'reason': f'agent抽检 audit seed=42·{why}', 'at': now},
                           ensure_ascii=False) + '\n')
import collections
c = collections.Counter(v for _, v, _ in verdicts)
print('label audit part2 记录', len(verdicts), dict(c))
