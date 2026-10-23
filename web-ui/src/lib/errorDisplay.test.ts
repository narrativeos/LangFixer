import { describe, expect, it } from 'vitest'
import {
  CAT_LABELS,
  confBand,
  consensusText,
  instanceContext,
} from './errorDisplay'
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

describe('confBand', () => {
  it('LLM 来源单独标注（数值无校准可比）', () => {
    expect(confBand(mk({ source: 'llm', confidence: 0.99 }))).toEqual({ label: 'LLM 判断', type: 'info' })
  })
  it('按阈值分高/中/低', () => {
    expect(confBand(mk({ confidence: 0.8 })).label).toBe('高置信')
    expect(confBand(mk({ confidence: 0.55 })).label).toBe('中置信')
    expect(confBand(mk({ confidence: 0.54 })).label).toBe('低置信')
  })
})

describe('consensusText', () => {
  it('source + also_found_by 汉化拼接', () => {
    expect(consensusText(mk({ source: 'rule', also_found_by: ['llm'] }))).toBe('规则 / LLM 兜底')
  })
  it('未知层代码回落原文', () => {
    expect(consensusText(mk({ source: 'rule', also_found_by: ['future_layer'] }))).toBe('规则 / future_layer')
  })
})

describe('instanceContext', () => {
  it('中文 span 换算 + 前后 8 字上下文', () => {
    const text = '他昨天报复很大这句话用来测试上下文截取功能是否正常工作'
    const byteStart = Buffer.byteLength('他昨天', 'utf8')
    const byteEnd = byteStart + Buffer.byteLength('报复', 'utf8')
    const out = instanceContext(text, mk({ span: [byteStart, byteEnd], original: '报复' }))
    expect(out).toContain('【报复】')
    expect(out.startsWith('…')).toBe(false) // 起点在前 8 字内
  })
  it('span 越界回落 original（不抛错）', () => {
    expect(instanceContext('abc', mk({ span: [1, 99], original: '??' }))).toBe('??')
  })
})

describe('CAT_LABELS', () => {
  it('覆盖全部已知分类', () => {
    for (const k of ['grammar', 'spelling', 'typography', 'style', 'other']) {
      expect(CAT_LABELS[k]).toBeTruthy()
    }
  })
})
