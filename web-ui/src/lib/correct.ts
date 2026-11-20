/**
 * 修正应用（纯函数，可单测）
 *
 * 与服务端 `apply_corrections` 同构：span 为 UTF-8 字节偏移 → 转 UTF-16 索引
 * （口径转换唯一实现见 lib/span）；排序 + 跳过重叠区间 + 从后往前替换。
 * 用途：① 采纳沉淀（src_text → corrected_text 送服务端）；
 *      ② 「复制修正全文 / 替换为修正文本」（结果面板一键出口）。
 */
import { byteToU16Map } from './span'

export interface Correctable {
  span: [number, number]
  correction: string | null
}

export function applyCorrections(text: string, errors: Correctable[]): string {
  const byteToU16 = byteToU16Map(text)
  const items = errors
    .filter((e) => e.correction != null && e.span[1] > e.span[0])
    .map((e) => ({
      a: byteToU16.get(e.span[0]) ?? -1,
      b: byteToU16.get(e.span[1]) ?? -1,
      c: e.correction as string,
    }))
    .filter((it) => it.a >= 0 && it.b >= 0 && it.a < it.b && it.b <= text.length)
    .sort((x, y) => x.a - y.a || x.b - y.b)
  const sel: typeof items = []
  let lastEnd = 0
  for (const it of items) {
    if (it.a >= lastEnd) {
      sel.push(it)
      lastEnd = it.b
    }
  }
  let out = text
  for (let i = sel.length - 1; i >= 0; i--) {
    const it = sel[i]
    out = out.slice(0, it.a) + it.c + out.slice(it.b)
  }
  return out
}
