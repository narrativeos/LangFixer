import { describe, expect, it } from 'vitest'
import { applyCorrections } from './correct'

/** span 为 UTF-8 字节偏移（与后端 Error.span 口径一致） */
function byteSpan(text: string, substr: string): [number, number] {
  const idx = text.indexOf(substr)
  const byteStart = Buffer.byteLength(text.slice(0, idx), 'utf8')
  const byteEnd = byteStart + Buffer.byteLength(substr, 'utf8')
  return [byteStart, byteEnd]
}

describe('applyCorrections', () => {
  it('中文（3 字节/字）span 换算并替换', () => {
    const text = '他昨天报复很大。'
    const span = byteSpan(text, '报复')
    expect(applyCorrections(text, [{ span, correction: '抱负' }])).toBe('他昨天抱负很大。')
  })

  it('多条修正从后往前替换，互不影响', () => {
    const text = '他昨天报复很大，今天接着报复。'
    const a = byteSpan(text, '报复')
    // 第二处「报复」：按字符位置找到后再换算字节偏移
    const charIdx = text.indexOf('报复', 4)
    const b: [number, number] = [
      Buffer.byteLength(text.slice(0, charIdx), 'utf8'),
      Buffer.byteLength(text.slice(0, charIdx + 2), 'utf8'),
    ]
    const out = applyCorrections(text, [
      { span: a, correction: '抱负' },
      { span: b, correction: '报复' },
    ])
    expect(out).toBe('他昨天抱负很大，今天接着报复。')
  })

  it('重叠区间：保留先出现的修正，跳过后续重叠', () => {
    const text = 'abcdefgh'
    const out = applyCorrections(text, [
      { span: [2, 4], correction: 'X' },
      { span: [3, 5], correction: 'Y' },
    ])
    expect(out).toBe('abXefgh')
  })

  it('correction 为 null / span 非法的条目被忽略', () => {
    const text = '没有问题'
    expect(
      applyCorrections(text, [
        { span: [0, 2], correction: null },
        { span: [2, 2], correction: 'X' },
        { span: [5, 3], correction: 'Y' },
      ]),
    ).toBe('没有问题')
  })

  it('emoji（4 字节 + UTF-16 代理对）边界正确', () => {
    const text = 'a😀bc'
    const span = byteSpan(text, 'b')
    expect(applyCorrections(text, [{ span, correction: 'B' }])).toBe('a😀Bc')
  })

  it('越界 span（超出文本字节长度）被忽略', () => {
    const text = 'abc'
    expect(applyCorrections(text, [{ span: [1, 99], correction: 'X' }])).toBe('abc')
  })
})
