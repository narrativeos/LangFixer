import { describe, expect, it } from 'vitest'
import { buildMarks, buildSegments, type HighlightableError } from './highlight'

/** 构造错误项：span 一律 UTF-8 字节偏移（与后端/ Rust 切片口径一致） */
function err(
  span: [number, number],
  original: string,
  extra: Partial<HighlightableError> = {},
): HighlightableError {
  return { span, original, correction: null, confidence: 1, ...extra }
}

describe('buildMarks', () => {
  it('ASCII：字节 = UTF-16 索引，span 精确匹配', () => {
    const marks = buildMarks('I recieve the letter.', [err([2, 9], 'recieve')], false)
    expect(marks).toHaveLength(1)
    expect(marks[0].start).toBe(2)
    expect(marks[0].end).toBe(9)
    expect(marks[0].index).toBe(0)
  })

  it('CJK：span 为字节偏移，先换算再切片', () => {
    // 武(3B)大(3B)报(3B)复(3B)：「报复」字节 [6,12) → UTF-16 [2,4)
    const marks = buildMarks('武大报复很大', [err([6, 12], '报复')], false)
    expect(marks[0].start).toBe(2)
    expect(marks[0].end).toBe(4)
  })

  it('span 失配且未过期 → 按 original indexOf 回退', () => {
    const marks = buildMarks('他昨天报复很大。', [err([0, 2], '报复')], false)
    expect(marks[0].start).toBe(3)
    expect(marks[0].end).toBe(5)
  })

  it('结果过期 → span 失配的标记放弃（不做 indexOf 回退）', () => {
    const marks = buildMarks('他昨天报复很大。', [err([0, 2], '报复')], true)
    expect(marks).toHaveLength(0)
  })

  it('span 缺失 → 回退 indexOf；original 不在文本中则跳过', () => {
    const e1: HighlightableError = { original: '图书管', correction: '图书馆' }
    const e2: HighlightableError = { original: '不存在词', correction: null }
    const marks = buildMarks('我在图书管看书', [e1, e2], false)
    expect(marks).toHaveLength(1)
    expect(marks[0].index).toBe(0)
    expect(marks[0].start).toBe(2)
  })

  it('emoji（4 字节码点）：字节与 UTF-16 索引都正确', () => {
    // a😀报复：a=1B 😀=4B 报=3B 复=3B；「报复」字节 [5,11) → UTF-16 [3,5)
    const marks = buildMarks('a😀报复', [err([5, 11], '报复')], false)
    expect(marks[0].start).toBe(3)
    expect(marks[0].end).toBe(5)
  })

  it('多个标记按 start 升序（与入参顺序无关）', () => {
    const marks = buildMarks(
      'ab cd ab',
      [err([3, 5], 'cd'), err([0, 2], 'ab')],
      false,
    )
    expect(marks.map((m) => m.start)).toEqual([0, 3])
    expect(marks[0].index).toBe(1) // start=0 属于入参第 2 项
  })
})

describe('buildSegments', () => {
  it('空文本 → 空数组；无错误 → 整段', () => {
    expect(buildSegments('', [], false)).toEqual([])
    expect(buildSegments('hello', [], false)).toEqual([{ text: 'hello' }])
  })

  it('普通片段与标记片段按序交错', () => {
    const segs = buildSegments('他干一番大报复。', [err([15, 21], '报复')], false)
    expect(segs.map((s) => s.text)).toEqual(['他干一番大', '报复', '。'])
    expect(segs[1].mark?.index).toBe(0)
  })

  it('重叠区间只保留先出现的标记', () => {
    // 「报复」[6,12) 与「报」[6,9) 重叠 → 后者整段跳过
    const segs = buildSegments('武大报复', [err([6, 12], '报复'), err([6, 9], '报')], false)
    expect(segs.map((s) => s.text)).toEqual(['武大', '报复'])
  })

  it('全部标记都无法定位 → 整段返回', () => {
    const segs = buildSegments('abc', [err([0, 2], 'zzz')], true)
    expect(segs).toEqual([{ text: 'abc' }])
  })
})
