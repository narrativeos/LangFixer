import { describe, expect, it } from 'vitest'
import { errorTypeLabel, layerStatusLabel, modeLabel } from './labels'

describe('errorTypeLabel', () => {
  it('已收录类型返回中文', () => {
    expect(errorTypeLabel('spelling')).toBe('拼写错误')
    expect(errorTypeLabel('collocation')).toBe('搭配不当')
    expect(errorTypeLabel('particle')).toBe('助词错误')
    expect(errorTypeLabel('anomaly')).toBe('文本污染')
  })
  it('未收录类型回落原文（不吞信息）', () => {
    expect(errorTypeLabel('weird_future_type')).toBe('weird_future_type')
  })
})

describe('modeLabel', () => {
  it('level → 人话模式名，越界钳位不吞信息', () => {
    expect(modeLabel(1)).toBe('快速检查')
    expect(modeLabel(2)).toBe('标准检查')
    expect(modeLabel(3)).toBe('深度扫描')
    expect(modeLabel(0)).toBe('快速检查')
    expect(modeLabel(9)).toBe('深度扫描')
  })
})

describe('layerStatusLabel', () => {
  it('状态汉化', () => {
    expect(layerStatusLabel('ran')).toBe('已执行')
    expect(layerStatusLabel('abstained')).toBe('弃权')
    expect(layerStatusLabel('unavailable')).toBe('不可用已回退')
    expect(layerStatusLabel('skipped')).toBe('未触发')
  })
  it('未知状态回落原文', () => {
    expect(layerStatusLabel('whatever')).toBe('whatever')
  })
})
