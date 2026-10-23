import { describe, expect, it } from 'vitest'
import { buildMergedGroups, buildSoloGroups, summarizeCategories } from './errorGroups'
import type { ErrorItem } from '../types'

function mk(over: Partial<ErrorItem>): ErrorItem {
  return {
    error_type: 'grammar',
    span: [0, 3],
    original: 'x',
    correction: null,
    message: '',
    confidence: 0.9,
    source: 'rule',
    ...over,
  } as ErrorItem
}

describe('summarizeCategories', () => {
  it('缺省 category 归入 other', () => {
    const m = summarizeCategories([mk({}), mk({ category: 'grammar' })])
    expect(m).toEqual({ other: 1, grammar: 1 })
  })
})

describe('buildMergedGroups', () => {
  it('相同 error_type + original + correction 合并成组，保留原始下标', () => {
    const items = [
      { i: 0, e: mk({ original: '报复', correction: '抱负' }) },
      { i: 1, e: mk({ original: '的', correction: '地' }) },
      { i: 2, e: mk({ original: '报复', correction: '抱负' }) },
    ]
    const gs = buildMergedGroups(items)
    expect(gs).toHaveLength(2)
    expect(gs[0].items.map((x) => x.i)).toEqual([0, 2])
    expect(gs[0].first.i).toBe(0)
  })
  it('correction 不同不合并；组顺序 = 首现顺序', () => {
    const items = [
      { i: 0, e: mk({ original: 'a', correction: 'x' }) },
      { i: 1, e: mk({ original: 'b', correction: 'y' }) },
      { i: 2, e: mk({ original: 'a', correction: 'z' }) },
    ]
    const gs = buildMergedGroups(items)
    expect(gs).toHaveLength(3)
  })
})

describe('buildSoloGroups', () => {
  it('每条独立成组，key 唯一', () => {
    const items = [
      { i: 0, e: mk({ original: 'a' }) },
      { i: 1, e: mk({ original: 'a' }) },
    ]
    const gs = buildSoloGroups(items)
    expect(gs).toHaveLength(2)
    expect(gs.map((g) => g.key)).toEqual(['solo:0', 'solo:1'])
  })
})
