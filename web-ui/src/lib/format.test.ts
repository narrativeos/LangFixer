/**
 * format.ts 单元测试（vitest）
 *
 * 覆盖：时间相对格式化、JSON 轻量高亮（转义 + 着色）。
 */
import { describe, expect, it } from 'vitest'
import { formatWhen, highlightJson, randomSessionId } from './format'

describe('formatWhen', () => {
  const now = Date.parse('2026-10-11T12:00:00Z')

  it('刚刚 / 分钟 / 小时 / 天', () => {
    expect(formatWhen('2026-10-11T11:59:00Z', now)).toBe('刚刚')
    expect(formatWhen('2026-10-11T11:30:00Z', now)).toBe('30 分钟前')
    expect(formatWhen('2026-10-11T09:00:00Z', now)).toBe('3 小时前')
    expect(formatWhen('2026-10-01T12:00:00Z', now)).toBe('10 天前')
  })

  it('超过 30 天回退绝对日期时间', () => {
    expect(formatWhen('2026-08-01T03:04:00Z', now)).toMatch(/^\d{4}-\d{2}-\d{2} \d{2}:\d{2}$/)
  })

  it('非法时间戳原样返回', () => {
    expect(formatWhen('not-a-date', now)).toBe('not-a-date')
  })
})

describe('randomSessionId', () => {
  it('格式为 s_ + 8 位十六进制', () => {
    expect(randomSessionId()).toMatch(/^s_[0-9a-f]{8}$/)
  })

  it('rng 可注入（确定性输出）', () => {
    // 恒定返回 0 → 全 0；返回 0.99 → 全 f
    expect(randomSessionId(() => 0)).toBe('s_00000000')
    expect(randomSessionId(() => 0.99)).toBe('s_ffffffff')
  })
})

describe('highlightJson', () => {
  it('字符串 / 数字 / 关键字着色，键名带冒号归 j-key', () => {
    const html = highlightJson('{"a": 1, "b": true, "c": null, "d": "x"}')
    expect(html).toContain('<span class="j-key">"a"</span>:')
    expect(html).toContain('<span class="j-num">1</span>')
    expect(html).toContain('<span class="j-kw">true</span>')
    expect(html).toContain('<span class="j-kw">null</span>')
    // 非键字符串值归 j-str
    expect(html).toContain('<span class="j-str">"x"</span>')
  })

  it('HTML 特殊字符先转义', () => {
    const html = highlightJson('{"a": "<script>&"}')
    expect(html).not.toContain('<script>')
    expect(html).toContain('&lt;script&gt;')
    expect(html).toContain('&amp;')
  })
})