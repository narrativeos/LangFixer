import { describe, expect, it } from 'vitest'
import {
  DEFAULT_CAPS,
  sanitizeCaps,
  viewAllowed,
  viewCapKey,
  type Caps,
} from './caps'

describe('sanitizeCaps', () => {
  it('非法输入回落默认（learning 开，proofreading/ops 关）', () => {
    expect(sanitizeCaps(null)).toEqual(DEFAULT_CAPS)
    expect(sanitizeCaps('garbage')).toEqual(DEFAULT_CAPS)
    expect(sanitizeCaps({ learning: 'yes' })).toEqual(DEFAULT_CAPS)
  })

  it('合法布尔字段按值采纳（部分字段）', () => {
    expect(sanitizeCaps({ proofreading: true })).toEqual({
      learning: true,
      proofreading: true,
      ops: false,
    })
    expect(sanitizeCaps({ learning: false, ops: true })).toEqual({
      learning: false,
      proofreading: false,
      ops: true,
    })
  })
})

describe('view gating', () => {
  const caps: Caps = { learning: true, proofreading: false, ops: false }

  it('核心视图（workspace/settings）始终可见', () => {
    expect(viewCapKey('workspace')).toBeNull()
    expect(viewAllowed('workspace', caps)).toBe(true)
    expect(viewAllowed('settings', caps)).toBe(true)
  })

  it('载体审校依赖 proofreading；未开启时不可见', () => {
    expect(viewCapKey('media')).toBe('proofreading')
    expect(viewAllowed('media', caps)).toBe(false)
    expect(viewAllowed('media', { ...caps, proofreading: true })).toBe(true)
  })

  it('规则 / 知识 / 进化依赖 ops', () => {
    for (const v of ['rules', 'knowledge', 'evolution'] as const) {
      expect(viewCapKey(v)).toBe('ops')
      expect(viewAllowed(v, caps)).toBe(false)
      expect(viewAllowed(v, { ...caps, ops: true })).toBe(true)
    }
  })

  it('练习与复习依赖 learning', () => {
    expect(viewAllowed('learning', caps)).toBe(true)
    expect(viewAllowed('learning', { ...caps, learning: false })).toBe(false)
  })
})