/**
 * pipeline.ts 单元测试（vitest）
 *
 * 覆盖：检测管线四层（① 规则 / ② 决策模型 / ②.5 知识召回 / ③ LLM 兜底）
 * 的启用状态与 ③ 层按「响应级别」的动态显示。
 */
import { describe, expect, it } from 'vitest'
import { computePipelineLayers, summarizeRun, type PipelineState } from './pipeline'

const base: PipelineState = {
  level: 1,
  decisionEnabled: true,
  llmEnabled: true,
  embeddingConfigured: true,
  l3Enabled: false,
}

describe('computePipelineLayers', () => {
  it('① 本地 ZEN 规则恒为已启用', () => {
    const l1 = computePipelineLayers(base)[0]
    expect(l1.label).toBe('① 本地 ZEN 规则')
    expect(l1.type).toBe('success')
  })

  it('② 决策模型跟随启用状态', () => {
    expect(computePipelineLayers(base)[1].label).toBe('② 决策模型 · 已启用')
    expect(computePipelineLayers({ ...base, decisionEnabled: false })[1].label).toBe('② 决策模型 · 未启用')
    expect(computePipelineLayers({ ...base, decisionEnabled: false })[1].type).toBe('info')
  })

  it('②.5 知识召回跟随嵌入引擎配置状态', () => {
    expect(computePipelineLayers(base)[2].label).toBe('②.5 知识召回 · 已配置')
    expect(computePipelineLayers({ ...base, embeddingConfigured: false })[2].label).toBe('②.5 知识召回 · 未配置')
  })

  it('L1：③ 未启用', () => {
    const l3 = computePipelineLayers(base)[3]
    expect(l3.label).toBe('③ LLM 兜底 · 未启用')
    expect(l3.type).toBe('info')
  })

  it('L2 + L3 未启用 + 决策模型启用 + LLM 已配置：③ 弃权时触发（主色，旧语义回退）', () => {
    const l3 = computePipelineLayers({ ...base, level: 2 })[3]
    expect(l3.label).toBe('③ LLM 兜底 · 弃权时触发')
    expect(l3.type).toBe('')
  })

  it('L2 + L3 未启用 + 决策模型启用 + LLM 未配置：③ 未配置（warning）', () => {
    const l3 = computePipelineLayers({ ...base, level: 2, llmEnabled: false })[3]
    expect(l3.label).toBe('③ LLM 兜底 · 未配置')
    expect(l3.type).toBe('warning')
  })

  it('L2 + L3 未启用 + 决策模型未启用：③ 未启用（等效 L1）', () => {
    const l3 = computePipelineLayers({ ...base, level: 2, decisionEnabled: false })[3]
    expect(l3.label).toBe('③ LLM 兜底 · 未启用（等效 L1）')
    expect(l3.type).toBe('info')
  })

  it('L2 + L3 启用 + LLM 已配置：③-F 已启用，③ 仅长尾（L3 弃权时）', () => {
    const layers = computePipelineLayers({ ...base, level: 2, l3Enabled: true })
    expect(layers[3].label).toBe('③-F 特征模型 · 已启用')
    expect(layers[3].type).toBe('success')
    expect(layers[4].label).toBe('③ LLM 兜底 · 仅长尾（L3 弃权时）')
    expect(layers[4].type).toBe('success')
  })

  it('L2 + L3 启用 + LLM 未配置：③ 未配置（warning）', () => {
    const layers = computePipelineLayers({ ...base, level: 2, l3Enabled: true, llmEnabled: false })
    expect(layers[4].label).toBe('③ LLM 兜底 · 未配置')
    expect(layers[4].type).toBe('warning')
  })

  it('L3 + LLM 已配置：③ 无条件', () => {
    const l3 = computePipelineLayers({ ...base, level: 3 })[3]
    expect(l3.label).toBe('③ LLM 兜底 · 无条件')
    expect(l3.type).toBe('success')
  })

  it('L3 + LLM 未配置：③ 未配置（warning）', () => {
    const l3 = computePipelineLayers({ ...base, level: 3, llmEnabled: false })[3]
    expect(l3.label).toBe('③ LLM 兜底 · 未配置')
    expect(l3.type).toBe('warning')
  })

  it('L3 + 特征模型启用 + LLM 已配置：③-F 已启用，③ 仅长尾', () => {
    const layers = computePipelineLayers({ ...base, level: 3, l3Enabled: true })
    expect(layers[3].label).toBe('③-F 特征模型 · 已启用')
    expect(layers[3].type).toBe('success')
    expect(layers[4].label).toBe('③ LLM 兜底 · 仅长尾（特征模型弃权时）')
    expect(layers[4].type).toBe('success')
  })

  it('L3 + 特征模型启用 + LLM 未配置：③ 未配置（warning）', () => {
    const layers = computePipelineLayers({ ...base, level: 3, l3Enabled: true, llmEnabled: false })
    expect(layers[4].label).toBe('③ LLM 兜底 · 未配置')
    expect(layers[4].type).toBe('warning')
  })

  it('L2 + L3 启用：显示 ③-F 门控层（P0.5：L3 是唯一的 pre-LLM 门控）', () => {
    const layers = computePipelineLayers({ ...base, level: 2, l3Enabled: true })
    expect(layers.map((l) => l.no)).toContain('③-F')
  })

  it('每层 no 唯一（用作列表 key）', () => {
    const layers = computePipelineLayers({ ...base, level: 3, l3Enabled: true })
    const nos = layers.map((l) => l.no)
    expect(new Set(nos).size).toBe(nos.length)
  })
})

describe('summarizeRun（「本次管线」摘要：哪些层实际跑了）', () => {
  it('全部层运行：各层带耗时，规则层含 NLP 细分', () => {
    const rows = summarizeRun({
      rule_ms: 12,
      nlp_ms: 8,
      nlp_warmup_ms: 3,
      decision_ms: 40,
      recall_ms: 5,
      l3_ms: 30,
      llm_ms: 800,
      total_ms: 900,
    })
    expect(rows.map((r) => r.ran).every(Boolean)).toBe(true)
    expect(rows[0].label).toBe('① 规则 12ms（NLP 8ms 含预热 3ms）')
    expect(rows[1].label).toBe('② 决策模型 40ms')
    expect(rows[2].label).toBe('②.5 知识召回 5ms')
    expect(rows[3].label).toBe('③-F 特征模型 30ms')
    expect(rows[4].label).toBe('③ LLM 兜底 800ms')
    expect(rows[5].label).toBe('总耗时 900ms')
  })

  it('仅本地规则（L1 默认）：其余层标记未运行', () => {
    const rows = summarizeRun({ rule_ms: 10, total_ms: 10 })
    expect(rows[0].ran).toBe(true)
    expect(rows[0].label).toBe('① 规则 10ms')
    for (let i = 1; i <= 4; i++) {
      expect(rows[i].ran).toBe(false)
      expect(rows[i].label).toContain('未运行')
    }
  })

  it('计时字段显式 null 与缺失等价（均为未运行）', () => {
    const explicit = summarizeRun({ rule_ms: 5, decision_ms: null, total_ms: 5 })
    const absent = summarizeRun({ rule_ms: 5, total_ms: 5 })
    expect(explicit).toEqual(absent)
  })
})