import { describe, expect, it } from 'vitest'
import {
  DROPPED_LABELS,
  droppedSummary,
  droppedTotal,
  ms,
  rate,
  shortFingerprint,
} from './observability'
import type { ObservabilityDropped } from '../types'

const zero: ObservabilityDropped = {
  ownership: 0,
  pii_mask: 0,
  out_of_range: 0,
  out_of_scope: 0,
}

describe('observability 纯函数面', () => {
  it('rate：小数 → 百分数；null / NaN → 破折号', () => {
    expect(rate(0.9462)).toBe('94.6%')
    expect(rate(0)).toBe('0.0%')
    expect(rate(1)).toBe('100.0%')
    expect(rate(null)).toBe('—')
    expect(rate(undefined)).toBe('—')
    expect(rate(Number.NaN)).toBe('—')
  })

  it('ms：毫秒与秒两档；无样本 → 破折号', () => {
    expect(ms(412)).toBe('412 ms')
    expect(ms(2345)).toBe('2.3 s')
    expect(ms(null)).toBe('—')
  })

  it('droppedTotal 四桶求和', () => {
    expect(droppedTotal(zero)).toBe(0)
    expect(droppedTotal({ ownership: 1, pii_mask: 2, out_of_range: 0, out_of_scope: 3 })).toBe(6)
  })

  it('droppedSummary 只列非零桶，全零为破折号', () => {
    expect(droppedSummary(zero)).toBe('—')
    expect(
      droppedSummary({ ownership: 2, pii_mask: 0, out_of_range: 1, out_of_scope: 0 }),
    ).toBe(`${DROPPED_LABELS.ownership}×2 · ${DROPPED_LABELS.out_of_range}×1`)
  })

  it('shortFingerprint 超长截断，空值破折号', () => {
    expect(shortFingerprint('abcdefghijklmnop')).toBe('abcdefghijkl…')
    expect(shortFingerprint('fp-test')).toBe('fp-test')
    expect(shortFingerprint(null)).toBe('—')
    expect(shortFingerprint('')).toBe('—')
  })

  it('DROPPED_LABELS 与账本四桶一一对应', () => {
    expect(Object.keys(DROPPED_LABELS).sort()).toEqual([
      'out_of_range',
      'out_of_scope',
      'ownership',
      'pii_mask',
    ])
  })
})
