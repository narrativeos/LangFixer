"""LLM 跨句语境损失评测（H：定向复查的修正质量轴）

G2 的 eval_cctc_context.py 证明**门控层**语境损失 ≤0.2%；本脚本测第二轴：
定向复查下 LLM 只见灰区句，**修正质量**是否下降。

生产口径对齐（prompts/models.json cec4 条目）：
  prompt = zh_detect_cec（system + user={{text}}），output_format = **plain**
  （CEC4 输出纠正后全文，非 JSON——gec-core 在解析边界做 diff 映射）

方法（cctc_gold）：
  full     = 整条 src 送 zh_detect_cec → 纠正全文
  targeted = 灰区句拼接（启发式 ≥0.30，可选扣除 PLA 救援）送同一模板
difflib 对齐 src↔out 得编辑区，对每个 gold 编辑 (a,b,ref)：
  detected = out 相对 src 在 [a,b) 区有改动
  fixed    = src 位置 a 映射到 out 后该处文本 == ref
语境损失 = full fixed 而 targeted（该句确在灰区）未 fixed 的编辑比例；
gold 句不在灰区 → 单独计 not_sent（门控层效应，非修正质量效应）。
过度纠正：clean 行 out != src 的比例。

用法：
  llama-server -m ../models/cec4.Q8_0.gguf --port 8081 --parallel 4 -c 8192 &
  ./.venv/bin/python3 eval_llm_context.py [--limit 400]
"""
import argparse
import concurrent.futures as cf
import difflib
import json
import urllib.error
import urllib.request
from pathlib import Path

import correct

HERE = Path(__file__).resolve().parent
REPO = HERE.parent
import re
SENT_RE = re.compile(r"[^。！？；.!?]+[。！？；.!?]*|\n+")
PROMPT = json.load(open(REPO / 'prompts' / 'zh_detect_cec_prompt.json', encoding='utf-8'))

_LLM_FAILS = 0
CACHE_P = Path('/tmp/llm_cache_plain.json')


def llm(url, text, cache):
    if text in cache:
        return cache[text]
    body = {
        'temperature': 0,
        'max_tokens': 2048,
        'messages': [
            {'role': 'system', 'content': PROMPT['system']},
            {'role': 'user', 'content': PROMPT['user'].replace('{{text}}', text)},
        ],
    }
    req = urllib.request.Request(url + '/chat/completions',
                                 data=json.dumps(body, ensure_ascii=False).encode(),
                                 headers={'Content-Type': 'application/json'})
    raw = None
    for attempt in range(3):
        try:
            raw = json.loads(urllib.request.urlopen(req, timeout=240).read())
            break
        except (urllib.error.HTTPError, urllib.error.URLError, TimeoutError, OSError):
            if attempt == 2:
                global _LLM_FAILS
                _LLM_FAILS += 1
                cache[text] = ''
                return ''
            import time
            time.sleep(3 * (attempt + 1))
    out = str(raw['choices'][0]['message']['content']).strip()
    cache[text] = out
    return out


def char_span(src, s1, s2):
    bts = src.encode('utf-8')
    return len(bts[:s1].decode('utf-8', 'ignore')), len(bts[:s2].decode('utf-8', 'ignore'))


def edit_ops(src, out):
    """difflib 对齐 → 变更操作列表 [(src_i1, src_i2, out_text)]"""
    ops = []
    sm = difflib.SequenceMatcher(None, src, out, autojunk=False)
    for tag, i1, i2, j1, j2 in sm.get_opcodes():
        if tag != 'equal':
            ops.append((i1, i2, out[j1:j2]))
    return ops


def map_pos(src, out, a):
    """src 位置 a → out 中对应位置（对齐映射）"""
    sm = difflib.SequenceMatcher(None, src, out, autojunk=False)
    for tag, i1, i2, j1, j2 in sm.get_opcodes():
        if i1 <= a <= i2:
            if tag == 'equal':
                return j1 + (a - i1)
            return j1
        # 前面的 opcodes 已经调整过；继续找
    return None


def corrected_of(raw: str) -> str:
    """CEC4 plain 输出经验格式：「纠正文本\\t原句回显」（长输入）或纯纠正句（短输入）。
    取首个 \\t 段为纠正句。注意：生产 split_think_answer(llm.rs) 不处理 \\t，
    回显会被 diff 成假插入错误——生产侧待修（G2 评测发现）。"""
    if not raw:
        return ''
    line = [l for l in raw.strip().splitlines() if l.strip()]
    line = line[-1] if line else ''
    return line.split('\t')[0].strip()


def judge(src, out, a, b, ref):
    """返回 (detected, fixed)：gold 编辑 src[a:b]→ref"""
    if not out:
        return False, False
    ops = edit_ops(src, out)
    det = any(i1 < b and i2 > a for i1, i2, _ in ops)
    p = map_pos(src, out, a)
    if p is None:
        return det, False
    if ref:
        fixed = out[p:p + len(ref)] == ref
    else:
        # 删除类：映射位置不应残留原片段
        fixed = src[a:b] not in out[p:p + (b - a) + 1]
    return det, fixed


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--url', default='http://127.0.0.1:8081/v1')
    ap.add_argument('--limit', type=int, default=400)
    ap.add_argument('--clean-limit', type=int, default=150)
    ap.add_argument('--tau', type=float, default=0.004)
    ap.add_argument('--char-gate', default='')
    args = ap.parse_args()

    table, bg = correct.load_zh_ngram_tables(
        str(HERE / 'ngram_table.json'), str(HERE / 'bigram_table_zh.json'))
    rows = json.load(open(REPO / 'datasets' / 'cctc_gold.json', encoding='utf-8'))
    err_rows = [r for r in rows if r['edits']][:args.limit]
    clean_rows = [r for r in rows if not r['edits']][:args.clean_limit]

    def gray_parts(src):
        """灰区句及其原文偏移"""
        parts = []
        for m in SENT_RE.finditer(src):
            t = m.group(0).strip()
            if len(t) < 2:
                continue
            off = m.start() + (len(m.group(0)) - len(m.group(0).lstrip()))
            parts.append((off, t))
        gray = [(o, t) for o, t in parts
                if correct.zh_heuristic_prob(t, table, bg) >= 0.30]
        if args.char_gate and gray:
            req = urllib.request.Request(args.char_gate, data=json.dumps(
                {'texts': [t for _, t in gray]}).encode(),
                headers={'Content-Type': 'application/json'})
            sc = json.loads(urllib.request.urlopen(req).read())['scores']
            gray = [(o, t) for (o, t), v in zip(gray, sc) if v >= args.tau]
        return gray

    gray_map = {}
    for r in rows:
        src = str(r['src'])
        g = gray_parts(src)
        gray_map[src] = (g, '。'.join(t for _, t in g))

    cache = json.load(open(CACHE_P, encoding='utf-8')) if CACHE_P.is_file() else {}

    def work(job):
        kind, txt = job
        return kind, txt, llm(args.url, txt, cache)

    jobs = []
    for r in err_rows:
        src = str(r['src'])
        jobs.append(('full', src))
        if gray_map[src][1]:
            jobs.append(('tgt', gray_map[src][1]))
    for r in clean_rows:
        src = str(r['src'])
        jobs.append(('clean_full', src))
        if gray_map[src][1]:
            jobs.append(('clean_tgt', gray_map[src][1]))

    results = {}
    done = 0
    with cf.ThreadPoolExecutor(max_workers=4) as ex:
        for kind, txt, out in ex.map(work, jobs):
            results[(kind, txt)] = out
            done += 1
            if done % 64 == 0:
                json.dump(cache, open(CACHE_P, 'w', encoding='utf-8'), ensure_ascii=False)
                print(f'... {done}/{len(jobs)} (fails={_LLM_FAILS})', flush=True)
    json.dump(cache, open(CACHE_P, 'w', encoding='utf-8'), ensure_ascii=False)

    st = {'n': 0, 'det_f': 0, 'fix_f': 0, 'det_t': 0, 'fix_t': 0,
          'not_sent': 0, 'ctx': 0}
    regress = []
    for r in err_rows:
        src = str(r['src'])
        out_f = corrected_of(results.get(('full', src), ''))
        parts, src_t = gray_map[src]
        out_t = corrected_of(results.get(('tgt', src_t), '')) if src_t else ''
        for ed in r['edits']:
            a, b = char_span(src, *ed['span'])
            ref = str(ed.get('ref', ''))
            if not (0 <= a < b) or b - a > 10:
                continue
            st['n'] += 1
            df, ff = judge(src, out_f, a, b, ref)
            st['det_f'] += df; st['fix_f'] += ff
            # 该编辑所在句是否被送审（原文位置被灰区句覆盖）
            in_gray = any(o <= a and b <= o + len(t) for o, t in parts)
            if not in_gray:
                st['not_sent'] += 1
            elif out_t:
                # 在拼接文本中定位该句，再在输出里判修正
                frag = next(t for o, t in parts if o <= a and b <= o + len(t))
                ga = src_t.find(frag) + (a - next(o for o, t in parts
                                                  if o <= a and b <= o + len(t)))
                dt, ft = judge(src_t, out_t, ga, ga + (b - a), ref)
                st['det_t'] += dt; st['fix_t'] += ft
                if ff and not ft:
                    st['ctx'] += 1
                    regress.append((src, src[a:b], ref, out_f, out_t))

    n = st['n']
    sent = n - st['not_sent']
    print(f'\n== gold 编辑级（n={n}，span≤10 字；送审 {sent}，未送审 {st["not_sent"]}）==')
    print(f'full     : 检出 {st["det_f"]/n:.1%}  修正正确 {st["fix_f"]/n:.1%}')
    print(f'targeted : 检出 {st["det_t"]/max(sent,1):.1%}  修正正确 {st["fix_t"]/max(sent,1):.1%}'
          f'（仅送审编辑）')
    print(f'语境损失（full 修对 / targeted 送审但未修对）: {st["ctx"]/n:.1%}')

    over_f = over_t = nclean = 0
    for r in clean_rows:
        src = str(r['src'])
        src_t = gray_map[src][1]
        nclean += 1
        of = corrected_of(results.get(('clean_full', src), ''))
        ot = corrected_of(results.get(('clean_tgt', src_t), '')) if src_t else ''
        if of and of != src:
            over_f += 1
        if ot and ot != src_t:
            over_t += 1
    print(f'\n== 过度纠正（clean 行 n={nclean}，输出≠输入）==')
    print(f'full {over_f/nclean:.1%}  targeted {over_t/nclean:.1%}')

    if regress:
        print('\n== 语境回退样例（前 6）==')
        for src, frag, ref, of, ot in regress[:6]:
            print(f'  「{frag}→{ref}」 src={src[:40]} | full_out={of[:40]} | tgt_out={ot[:40]}')


if __name__ == '__main__':
    main()
