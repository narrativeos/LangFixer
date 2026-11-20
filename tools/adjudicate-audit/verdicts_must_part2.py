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
# 判决（每条一句）：
verdicts = [
 ('nacgec-12623#42#42', 'adopt',    '并列缺连词和'),
 ('nacgec-12739#35#41', 'adopt',    '定语次序实改'),
 ('nacgec-13092#25#25', 'reject',   '插入残片复读'),
 ('nacgec-13709#13#24', 'adopt',    '从而培养兴趣错位'),
 ('nacgec-13939#23#24', 'adopt',    '贞观二十余年 年→生'),
 ('nacgec-14150#33#35', 'adopt',    '复指主语删更顺'),
 ('nacgec-14230#62#63', 'reject',   '一字千里不存在 模型造成语'),
 ('nacgec-14591#41#41', 'adopt',    '后半句主语残缺'),
 ('nacgec-15736#57#59', 'adopt',    '小说与文学作品种属并列'),
 ('nacgec-15902#38#38', 'adopt',    '主持人无收视率'),
 ('nacgec-16065#27#28', 'adopt',    '主语异换用句号'),
 ('nacgec-16822#41#41', 'reject',   '插入残片复读'),
 ('nacgec-16879#51#53', 'adopt',    '洗练是一条量词错'),
 ('nacgec-16955#49#51', 'uncertain','高产与牺牲产量矛盾实但fix可疑'),
 ('nacgec-16958#5#5',   'adopt',    '两面对一面补是否'),
 ('nacgec-17013#11#11', 'reject',   '杜撰已全 插入复读'),
 ('nlpcc23-6923#38#40', 'adopt',    '省略号后接谓语改逗号'),
 ('nlpcc23-6984#50#52', 'adopt',    '声像本非纸质矛盾'),
 ('nlpcc23-7161#13#14', 'uncertain','有个座口语非语病 50/50'),
 ('nlpcc23-7573#13#15', 'uncertain','她就通顺 只能更准 50/50'),
 ('sig13-5425#5#6',     'adopt',    '独立句逗号改句号'),
 ('sig13-5518#61#63',   'adopt',    '抽象概念它非他'),
]
ids = [v[0] for v in verdicts]
assert set(ids) <= sample and len(set(ids)) == 22
now = int(time.time())
with open(lab_dir / 'adjudications.jsonl', 'a', encoding='utf-8') as f:
    for iid, v, why in verdicts:
        f.write(json.dumps({'iid': iid, 'verdict': v,
                            'reason': f'agent抽检 must seed=42·{why}', 'at': now},
                           ensure_ascii=False) + '\n')
import collections
c = collections.Counter(v for _, v, _ in verdicts)
print('label must part2 记录', len(verdicts), dict(c))
