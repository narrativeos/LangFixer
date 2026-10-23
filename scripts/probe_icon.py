#!/usr/bin/env python3
"""探针验证 make_icon.py 产物：解码 PNG 并抽查设计关键点像素。"""
import os
import struct
import zlib

icons = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', 'desktop-app', 'icons')


def decode_png(path):
    d = open(path, 'rb').read()
    assert d[:8] == b'\x89PNG\r\n\x1a\n'
    off = 8
    idat = b''
    w = h = 0
    while off < len(d):
        ln = struct.unpack('>I', d[off:off + 4])[0]
        typ = d[off + 4:off + 8]
        payload = d[off + 8:off + 8 + ln]
        if typ == b'IHDR':
            w, h = struct.unpack('>II', payload[:8])
        elif typ == b'IDAT':
            idat += payload
        off += 12 + ln
    raw = zlib.decompress(idat)
    px = bytearray(w * h * 4)
    for y in range(h):
        src = y * (1 + w * 4)
        dst = y * w * 4
        # 本脚本生成的 PNG 均为 filter 0
        px[dst:dst + w * 4] = raw[src + 1:src + 1 + w * 4]
    return w, h, px


def probe(name, x, y, expect):
    w, h, px = decode_png(os.path.join(icons, name))
    i = (y * w + x) * 4
    r, g, b, a = px[i], px[i + 1], px[i + 2], px[i + 3]
    ok = expect(r, g, b, a)
    print(f'{name} ({x},{y}) = ({r},{g},{b},{a})', 'OK' if ok else f'EXPECT {expect.__name__}')
    return ok


fail = 0
# 256 画布；artboard 映射：像素 = 设计坐标 * 0.78125 + 28
fail |= not probe('icon.png', 128, 59, lambda r, g, b, a: a > 250 and r > 230)         # 背景顶部（近白）
fail |= not probe('icon.png', 128, 197, lambda r, g, b, a: a > 250 and b > r)           # 背景底部（渐变偏蓝灰）
fail |= not probe('icon.png', 128, 79, lambda r, g, b, a: a > 250 and abs(r - 199) < 25)  # 灰文本行
fail |= not probe('icon.png', 112, 112, lambda r, g, b, a: a > 250 and r > 200 and g < 130)  # 红色错误行
fail |= not probe('icon.png', 96, 145, lambda r, g, b, a: a > 250 and abs(r - 199) < 25)   # 灰文本行
fail |= not probe('icon.png', 167, 162, lambda r, g, b, a: a > 250 and r > 240 and g > 240)  # 徽章中心（白）
fail |= not probe('icon.png', 166, 181, lambda r, g, b, a: a > 250 and b > 200 and r < 120)  # 对勾（蓝）
fail |= not probe('icon.png', 38, 38, lambda r, g, b, a: a < 10)                       # artboard 边距区透明
fail |= not probe('icon.png', 4, 4, lambda r, g, b, a: a < 10)                         # 画布角透明
print('RESULT:', 'FAIL' if fail else 'ALL OK')
raise SystemExit(1 if fail else 0)