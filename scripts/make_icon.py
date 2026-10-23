#!/usr/bin/env python3
"""生成 Tauri 所需全套图标（LangFixer 主题：文本行 + 错误高亮 + 纠正对勾）。

设计（macOS 风格圆角方形，四角透明）：
  - 白 → 浅灰垂直渐变圆角方形背景
  - 三条左对齐"文本行"（圆角条），中间一条为红色 —— 检测到的语法错误
  - 右下白色圆形徽章 + 蓝色对勾 —— 纠正 / 归因
按 Apple macOS 图标网格规范：内容 artboard 占画布 ~80%（256 网格留 28 边距），
与 Dock 中其他应用图标尺寸一致。
仅依赖标准库：1536 超采样 SDF 光栅化，box 下采样到各目标尺寸
（16/32/48/64/128/256/512 均为 1536 的因子）。
输出到 desktop-app/icons/：32x32.png / 128x128.png / 128x128@2x.png /
512x512.png / icon.png(256) / icon.icns / icon.ico
"""
import math
import os
import struct
import zlib

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', 'desktop-app', 'icons')
SS = 6            # 256 设计空间 → 1536 母版的超采样倍数
S = 256 * SS      # 母版边长 1536（16/32/48/64/128/256/512 的公共倍数）

# 设计空间（256x256）配色
BG_TOP = (250, 252, 255)
BG_BOT = (222, 228, 236)
BAR_GRAY = (199, 206, 216)
BAR_RED = (228, 82, 72)
BADGE_WHITE = (255, 255, 255)
CHECK_BLUE = (46, 116, 240)


def clamp01(v):
    if v <= 0.0:
        return 0.0
    if v >= 1.0:
        return 1.0
    return v


def sd_rrect(px, py, cx, cy, hw, hh, r):
    """圆角矩形 SDF（中心 cx,cy，半宽 hw,hh，圆角 r）。"""
    dx = abs(px - cx) - (hw - r)
    dy = abs(py - cy) - (hh - r)
    ax = dx if dx > 0 else 0.0
    ay = dy if dy > 0 else 0.0
    return math.sqrt(ax * ax + ay * ay) + min(max(dx, dy), 0.0) - r


def sd_circle(px, py, cx, cy, r):
    dx, dy = px - cx, py - cy
    return math.sqrt(dx * dx + dy * dy) - r


def sd_seg(px, py, ax, ay, bx, by):
    """圆头线段 SDF。"""
    vx, vy = px - ax, py - ay
    wx, wy = bx - ax, by - ay
    ww = wx * wx + wy * wy
    t = (vx * wx + vy * wy) / ww if ww > 0 else 0.0
    if t < 0.0:
        t = 0.0
    elif t > 1.0:
        t = 1.0
    dx, dy = vx - t * wx, vy - t * wy
    return math.sqrt(dx * dx + dy * dy)


def over(img, S, x, y, r, g, b, a):
    """整数 src-over 合成。"""
    if a <= 0:
        return
    i = (y * S + x) * 4
    da = img[i + 3]
    if a >= 255 or da == 0:
        img[i] = r
        img[i + 1] = g
        img[i + 2] = b
        img[i + 3] = a
        return
    a2 = a * (255 - da) // 255
    oa = da + a2
    w = da * (255 - a) // 255
    img[i] = (r * a + img[i] * w) // oa
    img[i + 1] = (g * a + img[i + 1] * w) // oa
    img[i + 2] = (b * a + img[i + 2] * w) // oa
    img[i + 3] = oa


def render_master():
    """渲染 1536x1536 母版（RGBA，未预乘）。

    按 Apple macOS 图标网格规范：内容 artboard 占画布 ~80%
    （256 网格上四周留 28 边距 → 200/256 = 0.78125），居中放置。
    """
    img = bytearray(S * S * 4)
    SC = 200.0 / 256.0   # artboard 缩放（256 基准 → 200 artboard）
    OF = 28.0        # 边距（画布空间，不缩放）

    def T(v):
        """位置变换：256 基准设计坐标 → 画布坐标（缩放 + 边距）。"""
        return v * SC + OF

    def D(v):
        """尺寸变换：只缩放、不加边距偏移（位置用 T，尺寸用 D）。"""
        return v * SC

    # 1) 背景：圆角方形 + 垂直渐变
    for y in range(S):
        v = (y + 0.5) / SS
        t = v / 256.0
        cr = int(BG_TOP[0] + (BG_BOT[0] - BG_TOP[0]) * t)
        cg = int(BG_TOP[1] + (BG_BOT[1] - BG_TOP[1]) * t)
        cb = int(BG_TOP[2] + (BG_BOT[2] - BG_TOP[2]) * t)
        for x in range(S):
            u = (x + 0.5) / SS
            d = sd_rrect(u, v, T(128), T(128), D(128), D(128), D(58))
            a = clamp01(0.5 - d)
            if a > 0:
                over(img, S, x, y, cr, cg, cb, int(a * 255 + 0.5))

    # 2) 三条文本行（左对齐、宽度递减；中间一条红色 = 错误高亮）
    bars = [
        (48, 208, 66, 11, BAR_GRAY),
        (48, 168, 108, 11, BAR_RED),
        (48, 128, 150, 11, BAR_GRAY),
    ]
    for x0, x1, cy, hh, col in bars:
        cx = T((x0 + x1) / 2.0)
        hw = D((x1 - x0) / 2.0)
        cyt = T(cy)
        hht = D(hh)
        X0, X1 = int(T(x0) * SS), int(T(x1) * SS)
        Y0, Y1 = int((cyt - hht) * SS), int((cyt + hht) * SS)
        for y in range(max(0, Y0), min(S, Y1)):
            v = (y + 0.5) / SS
            for x in range(max(0, X0), min(S, X1)):
                u = (x + 0.5) / SS
                d = sd_rrect(u, v, cx, cyt, hw, hht, hht)
                a = clamp01(0.5 - d)
                if a > 0:
                    over(img, S, x, y, col[0], col[1], col[2], int(a * 255 + 0.5))

    # 3) 徽章投影（黑色 20%）
    bcx, bcy, br = T(178.0), T(172.0), D(52.0)
    for y in range(max(0, int((bcy - br) * SS)), min(S, int((bcy + br) * SS))):
        v = (y + 0.5) / SS
        for x in range(max(0, int((bcx - br) * SS)), min(S, int((bcx + br) * SS))):
            u = (x + 0.5) / SS
            d = sd_circle(u, v, bcx, bcy, br)
            a = clamp01(-d) * 0.20
            if a > 0:
                over(img, S, x, y, 0, 0, 0, int(a * 255 + 0.5))

    # 4) 白色圆形徽章
    br2 = D(50.0)
    for y in range(max(0, int((bcy - br2) * SS)), min(S, int((bcy + br2) * SS))):
        v = (y + 0.5) / SS
        for x in range(max(0, int((bcx - br2) * SS)), min(S, int((bcx + br2) * SS))):
            u = (x + 0.5) / SS
            d = sd_circle(u, v, bcx, bcy, br2)
            a = clamp01(0.5 - d)
            if a > 0:
                over(img, S, x, y, BADGE_WHITE[0], BADGE_WHITE[1], BADGE_WHITE[2], int(a * 255 + 0.5))

    # 5) 蓝色对勾（两段圆头线段，线宽 18）
    segs = [(T(152.0), T(172.0), T(176.0), T(196.0)), (T(176.0), T(196.0), T(214.0), T(138.0))]
    for ax, ay, bx, by in segs:
        x0, x1 = min(ax, bx) - D(9), max(ax, bx) + D(9)
        y0, y1 = min(ay, by) - D(9), max(ay, by) + D(9)
        for y in range(max(0, int(y0 * SS)), min(S, int(y1 * SS))):
            v = (y + 0.5) / SS
            for x in range(max(0, int(x0 * SS)), min(S, int(x1 * SS))):
                u = (x + 0.5) / SS
                d = sd_seg(u, v, ax, ay, bx, by)
                a = clamp01(D(9.0) - d)
                if a > 0:
                    over(img, S, x, y, CHECK_BLUE[0], CHECK_BLUE[1], CHECK_BLUE[2], int(a * 255 + 0.5))

    return img


def downsample(img, S, f):
    """box 下采样 f x f → S/f 边长。"""
    n = S // f
    out = bytearray(n * n * 4)
    m = f * f
    for oy in range(n):
        sy0 = oy * f
        for ox in range(n):
            sx0 = ox * f
            r = g = b = a = 0
            for dy in range(f):
                row = (sy0 + dy) * S * 4
                for dx in range(f):
                    i = row + (sx0 + dx) * 4
                    r += img[i]
                    g += img[i + 1]
                    b += img[i + 2]
                    a += img[i + 3]
            o = (oy * n + ox) * 4
            out[o] = r // m
            out[o + 1] = g // m
            out[o + 2] = b // m
            out[o + 3] = a // m
    return out
def chunk(t, d):
    return struct.pack('>I', len(d)) + t + d + struct.pack('>I', zlib.crc32(t + d) & 0xFFFFFFFF)


def png_bytes(size, rgba):
    raw = b''
    for y in range(size):
        raw += b'\x00'
        for x in range(size):
            j = (y * size + x) * 4
            raw += bytes(rgba[j:j + 4])
    return (
        b'\x89PNG\r\n\x1a\n'
        + chunk(b'IHDR', struct.pack('>IIBBBBB', size, size, 8, 6, 0, 0, 0))
        + chunk(b'IDAT', zlib.compress(raw, 9))
        + chunk(b'IEND', b'')
    )


def write_png(path, size, rgba):
    with open(path, 'wb') as f:
        f.write(png_bytes(size, rgba))


def write_icns(path, master, sizes):
    # PNG 编码：icp4=16 / icp5=32；icon 族：ic11=64 / ic08=128 / ic09=256 / ic14=512
    codes = {16: b'icp4', 32: b'icp5', 64: b'ic11', 128: b'ic08', 256: b'ic09', 512: b'ic14'}
    body = b''
    for s in sizes:
        data = png_bytes(s, downsample(master, S, S // s))
        body += codes[s] + struct.pack('>I', len(data) + 8) + data
    with open(path, 'wb') as f:
        f.write(struct.pack('>4sI', b'icns', len(body) + 8))
        f.write(body)


def write_ico(path, master, sizes):
    # PNG 编码的 ICO（Windows Vista+ 支持 PNG 内嵌）
    datas = [(s, png_bytes(s, downsample(master, S, S // s))) for s in sizes]
    header = struct.pack('<HHH', 0, 1, len(datas))
    directory = b''
    offset = 6 + 16 * len(datas)
    for s, d in datas:
        w = 0 if s >= 256 else s
        directory += struct.pack('<BBBBHHII', w, w, 0, 0, 1, 32, len(d), offset)
        offset += len(d)
    with open(path, 'wb') as f:
        f.write(header + directory + b''.join(d for _, d in datas))


if __name__ == '__main__':
    import time
    t0 = time.time()
    master = render_master()
    print(f'master {S}x{S} rendered in {time.time() - t0:.1f}s')
    os.makedirs(OUT, exist_ok=True)
    for name, size in [('32x32.png', 32), ('128x128.png', 128), ('128x128@2x.png', 256),
                       ('512x512.png', 512), ('icon.png', 256)]:
        write_png(os.path.join(OUT, name), size, downsample(master, S, S // size))
        print('wrote', name)
    write_icns(os.path.join(OUT, 'icon.icns'), master, [16, 32, 64, 128, 256, 512])
    write_ico(os.path.join(OUT, 'icon.ico'), master, [16, 32, 48, 256])
    print('icons written to', os.path.abspath(OUT))
