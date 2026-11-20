import { describe, expect, it } from 'vitest'
import { byteToU16Map } from './span'

describe('byteToU16Map', () => {
  it('CJK：字节 3/字符位置 1', () => {
    const m = byteToU16Map('武大。x')
    expect(m.get(0)).toBe(0)
    expect(m.get(3)).toBe(1) // 武(3B)
    expect(m.get(6)).toBe(2) // 大(3B)
    expect(m.get(9)).toBe(3) // 。(3B)
    expect(m.get(10)).toBe(4) // x(1B)，亦即 sentinel = text.length
  })

  it('4 字节码点（emoji）按码点计', () => {
    const m = byteToU16Map('a😀b')
    expect(m.get(0)).toBe(0)
    expect(m.get(1)).toBe(1) // 😀 起
    expect(m.get(5)).toBe(3) // b（JS 索引 2，代理对占 2）——map 值为 UTF-16 索引
    expect(m.get(6)).toBe(4)
  })
})
