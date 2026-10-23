#!/usr/bin/env python3
"""重新生成 LICENSES-THIRD-PARTY.md（第三方依赖许可清单）。

用法：python3 tools/gen_third_party_licenses.py
数据源：
- Cargo.lock：优先解析本地 ~/.cargo/registry 索引与 .crate 包内 Cargo.toml，
  缺失时回退 crates.io API（version.license 字段，限流退避）。
- web-ui/package-lock.json：仅运行时依赖，查询 npm registry。
"""
import glob, json, os, re, tarfile, time, urllib.request
from collections import defaultdict
from concurrent.futures import ThreadPoolExecutor

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
UA = {'User-Agent': 'langfixer-audit/1.0 (https://github.com/narrativeos/LangFixer)'}
LOCAL_CRATES = {'shared-model', 'gec-core', 'gec-zh', 'gec-en', 'gec-ja', 'attribution-engine',
                'langfixer-llm', 'api-server', 'desktop-app', 'lexicon-store', 'document-parse'}

def fetch_json(url, tries=4):
    for i in range(tries):
        try:
            req = urllib.request.Request(url, headers=UA)
            return json.loads(urllib.request.urlopen(req, timeout=15).read())
        except Exception:
            if i == tries - 1:
                return None
            time.sleep(2 + 3 * i)
    return None

# ---------- cargo ----------
os.chdir(ROOT)
pkgs = re.findall(r'name = "([^"]+)"\nversion = "([^"]+)"', open('Cargo.lock').read())
third = sorted({(n, v) for n, v in pkgs if n not in LOCAL_CRATES})
index_dirs = glob.glob(os.path.expanduser('~/.cargo/registry/index/*/'))
cache_dirs = glob.glob(os.path.expanduser('~/.cargo/registry/cache/*/'))

def index_lookup(name):
    low = name.lower()
    if len(low) == 1: rel = f'1/{low}'
    elif len(low) == 2: rel = f'2/{low}'
    elif len(low) == 3: rel = f'3/{low[0]}/{low}'
    else: rel = f'{low[:2]}/{low[2:4]}/{low}'
    for d in index_dirs:
        p = os.path.join(d, rel)
        if os.path.exists(p):
            return [json.loads(l) for l in open(p) if l.strip()]
    return None

def crate_toml_license(name, version):
    for d in cache_dirs:
        f = os.path.join(d, f'{name}-{version}.crate')
        if not os.path.exists(f):
            continue
        with tarfile.open(f) as t:
            for m in t.getmembers():
                if m.name.endswith('/Cargo.toml'):
                    txt = t.extractfile(m).read().decode('utf-8', 'replace')
                    mm = re.search(r'^license\s*=\s*"([^"]+)"', txt, re.M)
                    if mm: return mm.group(1)
                    mm = re.search(r'^license-file\s*=\s*"([^"]+)"', txt, re.M)
                    if mm: return f'file:{mm.group(1)}'
        return None
    return None

cargo_rows = []
for name, version in third:
    lic = None
    for e in (index_lookup(name) or []):
        if e.get('vers') == version:
            if e.get('license'): lic = e['license']
            elif e.get('license_file'): lic = f'file:{e["license_file"]}'
            break
    if not lic:
        lic = crate_toml_license(name, version)
    cargo_rows.append([name, version, lic or ''])

# 兜底：crates.io API（version.license）
for row in cargo_rows:
    if not row[2]:
        d = fetch_json(f'https://crates.io/api/v1/crates/{row[0]}/{row[1]}')
        ver = (d or {}).get('version') or {}
        row[2] = ver.get('license') or (f'file:{ver["license_file"]}' if ver.get('license_file') else 'UNKNOWN')
        time.sleep(0.4)

# ---------- npm ----------
lock = json.load(open('web-ui/package-lock.json'))
npm = {}
for path, meta in lock.get('packages', {}).items():
    if not path or not path.startswith('node_modules/'):
        continue
    name = path.split('node_modules/')[-1]
    if meta.get('dev') is True or meta.get('devOptional') is True:
        continue
    npm[name] = meta.get('version', '?')

def npm_lic(nv):
    n, v = nv
    d = fetch_json(f'https://registry.npmjs.org/{n}/{v}', tries=3)
    return [n, v, (d or {}).get('license') or 'UNKNOWN']

with ThreadPoolExecutor(8) as ex:
    npm_rows = list(ex.map(npm_lic, sorted(npm.items())))

# ---------- report ----------
def bucket(lic):
    u = lic.upper()
    if 'UNKNOWN' in u: return 'UNKNOWN'
    if 'GPL' in u: return 'Copyleft 双许可（选 Apache/MIT 分支，见个案说明）'
    if 'MPL' in u: return 'MPL-2.0（文件级弱 Copyleft，兼容，见个案说明）'
    if 'CDLA' in u: return 'CDLA-Permissive（数据许可，兼容，见个案说明）'
    if 'BSL' in u: return 'Apache-2.0 双许可（含 BSL 分支，选 Apache，见个案说明）'
    if 'APACHE' in u: return 'Apache-2.0（含 MIT/BSD/ISC 双许可）'
    if u.startswith('MIT') or ' MIT' in u or 'MIT-0' in u: return 'MIT'
    if 'UNLICENSE' in u or 'CC0' in u: return 'Public domain（Unlicense/CC0）'
    if 'UNICODE' in u: return 'Unicode-DFS'
    if 'ZLIB' in u: return 'zlib'
    if 'BSD' in u or 'ISC' in u: return 'BSD/ISC'
    return 'Other'

lines = []
lines.append('# 第三方依赖许可清单（THIRD-PARTY LICENSES）')
lines.append('')
lines.append(f'本文件由 `tools/gen_third_party_licenses.py` 依据 `Cargo.lock`（{len(cargo_rows)} 个第三方 crate）')
lines.append('与 `web-ui/package-lock.json`（运行时 npm 依赖）生成，列出每个依赖声明的 SPDX 许可。')
lines.append('本项目以 **Apache-2.0** 发布（见 `LICENSE`）；UNKNOWN 表示上游元数据缺失，需人工核对上游仓库。')
lines.append('')
for title, rows in (('Rust（Cargo.lock）', cargo_rows), ('Node.js（web-ui 运行时依赖）', npm_rows)):
    groups = defaultdict(list)
    for (n, v, l) in rows:
        groups[bucket(l)].append((n, v, l))
    lines.append(f'## {title}')
    lines.append('')
    lines.append(f'共 {len(rows)} 个包。按许可归组：')
    lines.append('')
    for b in sorted(groups, key=lambda b: (-len(groups[b]), b)):
        lines.append(f'- **{b}**：{len(groups[b])}')
    lines.append('')
    lines.append('<details><summary>逐包清单（名称 / 版本 / 声明许可）</summary>')
    lines.append('')
    lines.append('| 包 | 版本 | 许可 |')
    lines.append('|---|---|---|')
    for (n, v, l) in sorted(rows, key=lambda r: r[0].lower()):
        lines.append(f'| {n} | {v} | {l} |')
    lines.append('')
    lines.append('</details>')
    lines.append('')

lines.append('## 特殊许可个案（与 Apache-2.0 的兼容性说明）')
lines.append('')
lines.append('| 依赖 | 许可 | 处理 |')
lines.append('|---|---|---|')
lines.append('| cssparser / cssparser-macros / dtoa-short / option-ext / selectors / smartstring | MPL-2.0(+) | 文件级弱 Copyleft：不修改其源码即无传染性，随作品分发保留其许可声明即可 |')
lines.append('| self_cell | Apache-2.0 OR GPL-2.0-only | 双许可，**选用 Apache-2.0 分支** |')
lines.append('| r-efi | MIT OR Apache-2.0 OR LGPL-2.1-or-later | 三许可，**选用 Apache-2.0 分支** |')
lines.append('| ryu / dragonbox_ecma | Apache-2.0 OR BSL-1.0（OR LLVM-exception） | 双许可，**选用 Apache-2.0 分支** |')
lines.append('| webpki-roots / webpki-root-certs | CDLA-Permissive-2.0 | 数据许可（CA 根证书集），宽松可再分发，保留声明即可 |')
lines.append('')
lines.append('其余依赖均为 MIT / Apache-2.0 / BSD / ISC / Zlib / Unicode / 0BSD / Unlicense 等')
lines.append('宽松许可，与 Apache-2.0 完全兼容。')
lines.append('')
lines.append('## 外部语料（datasets/）')
lines.append('')
lines.append('| 数据集 | 原始许可 | 用途 |')
lines.append('|---|---|---|')
lines.append('| JFLEG（dev/test） | 研究用途（见上游 keirks/jfleg） | 英文 GEC 评测 |')
lines.append('| MuCGEC（dev 子集） | Apache-2.0 | 中文 GEC 评测 |')
lines.append('| CSCD / CEC dat 混淆集 | 见上游 ChineseErrorCorrector 声明（MIT/Apache-2.0） | 中文评测与词表 |')
lines.append('| anomaly_gold / cctc_gold（本仓库自标） | Apache-2.0 | 回归评测 |')
lines.append('')
lines.append('注：语料仅用于本地评测，不随二进制产物分发；再分发前请核对各上游许可条款。')
lines.append('')

open('LICENSES-THIRD-PARTY.md', 'w').write('\n'.join(lines))
unknown = [r for r in cargo_rows + npm_rows if r[2] in ('', 'UNKNOWN')]
print(f'written LICENSES-THIRD-PARTY.md: cargo={len(cargo_rows)} npm={len(npm_rows)} unknown={len(unknown)}')
for r in unknown[:20]:
    print('UNKNOWN', r)
