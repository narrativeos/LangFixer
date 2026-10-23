#!/usr/bin/env python3
"""校验 make_icon.py 产物：PNG 尺寸 / icns 容器 / ico 目录（一次性验证脚本）。"""
import os
import struct

icons = os.path.join(os.path.dirname(__file__), '..', 'desktop-app', 'icons')
fail = 0

for p, exp in [('32x32.png', 32), ('128x128.png', 128), ('128x128@2x.png', 256),
               ('512x512.png', 512), ('icon.png', 256)]:
    d = open(os.path.join(icons, p), 'rb').read()
    w, h = struct.unpack('>II', d[16:24])
    ok = (w, h) == (exp, exp) and d.startswith(b'\x89PNG')
    fail |= not ok
    print(p, f'{w}x{h}', 'OK' if ok else 'MISMATCH')

data = open(os.path.join(icons, 'icon.icns'), 'rb').read()
assert data[:4] == b'icns'
off, total = 8, struct.unpack('>I', data[4:8])[0]
assert total == len(data), (total, len(data))
expected = {b'icp4': 16, b'icp5': 32, b'ic11': 64, b'ic08': 128, b'ic09': 256, b'ic14': 512}
while off < len(data):
    typ = data[off:off + 4]
    ln = struct.unpack('>I', data[off + 4:off + 8])[0]
    payload = data[off + 8:off + 8 + ln - 8]
    w, h = struct.unpack('>II', payload[16:24])
    ok = payload.startswith(b'\x89PNG') and (w, h) == (expected[typ],) * 2
    fail |= not ok
    print('icns', typ.decode(), f'{w}x{h}', 'OK' if ok else 'BAD')
    off += ln

d = open(os.path.join(icons, 'icon.ico'), 'rb').read()
n = struct.unpack('<H', d[4:6])[0]
prev_end = 6 + 16 * n
for i in range(n):
    e = 6 + 16 * i
    w = d[e]
    ln = struct.unpack('<I', d[e + 8:e + 12])[0]
    start = struct.unpack('<I', d[e + 12:e + 16])[0]
    png = d[start:start + 8].startswith(b'\x89PNG')
    ok = png and start >= prev_end and start + ln <= len(d)
    fail |= not ok
    prev_end = start + ln
    print(f'ico[{i}] {w or 256}px data@{start} len={ln} png={png}', 'OK' if ok else 'BAD')

print('RESULT:', 'FAIL' if fail else 'ALL OK')
raise SystemExit(1 if fail else 0)