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
# 判决（每条一句）：
verdicts = [
 ('cctc-7987#15#44',     'uncertain','fix混标点断句改句号存疑 50/50'),
 ('cctc-8445#3#4',       'adopt',    '半角逗号转全角实错'),
 ('cctc-8563#13#14',     'adopt',    '一如往常修饰形容词删的可通'),
 ('cctc-8713#50#56',     'adopt',    '在后来的时候赘余'),
 ('cctc-8898#10#10',     'adopt',    '浪漫的缺宾语中心语补爱情'),
 ('cctc-9199#147#156',   'adopt',    '拿驾照考试语序改考驾照'),
 ('cctc-9839#100#125',   'adopt',    '喷喷改喷香+时不时地'),
 ('cscd-996#13#35',      'adopt',    '多么的让我非常的杂糅实改'),
 ('cscd-1054#1#1',       'reject',   '插入残片复读 不已在句中'),
 ('cscd-1249#1#1',       'reject',   '插入残片复读 如已在句中'),
 ('cscd-1259#12#13',     'adopt',    '呈现出人均补出+人均'),
 ('cscd-1395#20#21',     'adopt',    '向法院告改起诉'),
 ('cscd-1841#1#2',       'adopt',    'IGA应为IgA大小写'),
 ('cscd-1998#8#17',      'adopt',    '口服语序修饰两者'),
 ('cscdns-17624#0#1',    'adopt',    '若果应为如果'),
 ('cscdns-17643#32#33',  'adopt',    '呈请误用删呈'),
 ('cscdns-17857#15#16',  'adopt',    '部工应为员工'),
 ('cscdns-18443#47#48',  'adopt',    '决不仅应为绝不仅'),
 ('cscdns-18710#49#50',  'adopt',    '剖离应为剥离'),
 ('cscdns-18724#22#23',  'adopt',    '直破网价别字实'),
 ('cscdns-19237#60#63',  'adopt',    '有接触此病搭配改接触了'),
 ('cscdns-19415#5#5',    'adopt',    '危急缺中心语补时刻'),
 ('cscdns-19462#10#10',  'reject',   '插入残片复读 一路口已在句中'),
 ('cscdns-19509#8#9',    'adopt',    '域展应为扩展'),
 ('cscdns-19746#19#20',  'uncertain','仅删就属 stylistic 50/50'),
 ('cscdns-19984#23#25',  'uncertain','删异常与状语语序理由不符 50/50'),
 ('cscdns-20453#38#38',  'adopt',    '地砖后缺的补的'),
 ('ctc21-4168#42#47',    'adopt',    '非常的点睛改点睛之笔'),
 ('ctc21-4550#11#11',    'adopt',    '长状语后补逗号'),
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
print('label audit part1 记录', len(verdicts), dict(c))
