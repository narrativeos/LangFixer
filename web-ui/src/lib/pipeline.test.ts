/**
 * pipeline.ts 单元测试（vitest）
 *
 * 覆盖：检测管线层（① 规则 / ② 决策模型（待发布，仅实验开启时显示）/ ②.5 知识召回 /
 * ③ LLM 兜底）的启用状态与 ③ 层按「响应级别」的动态显示。
 */
import { describe, expect, it } from 'vitest'
import {
  applyFlowEvents,
  applyStageEvents,
  computePipelineLayers,
  markStagesCanceled,
  stageViewsFromTimings,
  summarizeRun,
  type PipelineState,
} from './pipeline'
import type { StageEvent } from '../types'

const base: PipelineState = {
  level: 1,
  decisionEnabled: true,
  llmEnabled: true,
  embeddingConfigured: true,
  l3Enabled: false,
}

const find = (s: PipelineState, no: string) =>
  computePipelineLayers(s).find((l) => l.no === no)

describe('computePipelineLayers', () => {
  it('① 本地 ZEN 规则恒为已启用', () => {
    const l1 = computePipelineLayers(base)[0]
    expect(l1.label).toBe('① 本地 ZEN 规则')
    expect(l1.type).toBe('success')
  })

  it('② 决策模型待发布（11-13 注：线不放弃，待更合适的决策模型）：默认（未启用）不显示；实验开启时显示', () => {
    expect(find({ ...base, decisionEnabled: false }, '②')).toBeUndefined()
    expect(find(base, '②')?.label).toBe('② 决策模型 · 已启用')
    expect(find(base, '②')?.type).toBe('success')
  })

  it('②.5 知识召回跟随嵌入引擎配置状态', () => {
    expect(find(base, '②.5')?.label).toBe('②.5 知识召回 · 已配置')
    expect(find({ ...base, embeddingConfigured: false }, '②.5')?.label).toBe('②.5 知识召回 · 未配置')
  })

  it('L1：③ 未启用', () => {
    const l3 = find(base, '③')
    expect(l3?.label).toBe('③ LLM 兜底 · 未启用')
    expect(l3?.type).toBe('info')
  })

  it('L2 + L3 未启用 + 决策模型实验开启 + LLM 已配置：③ 弃权时触发（主色，旧语义回退）', () => {
    const l3 = find({ ...base, level: 2 }, '③')
    expect(l3?.label).toBe('③ LLM 兜底 · 弃权时触发')
    expect(l3?.type).toBe('')
  })

  it('L2 + L3 未启用 + 决策模型实验开启 + LLM 未配置：③ 未配置（warning）', () => {
    const l3 = find({ ...base, level: 2, llmEnabled: false }, '③')
    expect(l3?.label).toBe('③ LLM 兜底 · 未配置')
    expect(l3?.type).toBe('warning')
  })

  it('L2 + L3 未启用 + 决策模型未启用（产品默认）：③ 未启用（等效 L1）', () => {
    const l3 = find({ ...base, level: 2, decisionEnabled: false }, '③')
    expect(l3?.label).toBe('③ LLM 兜底 · 未启用（等效 L1）')
    expect(l3?.type).toBe('info')
  })

  it('L2 + L3 启用 + LLM 已配置：③-F 已启用，③ 仅长尾（L3 弃权时）', () => {
    const layers = computePipelineLayers({ ...base, level: 2, l3Enabled: true })
    expect(find({ ...base, level: 2, l3Enabled: true }, '③-F')).toEqual({
      no: '③-F',
      label: '③-F 特征模型 · 已启用',
      type: 'success',
    })
    expect(layers.find((l) => l.no === '③')?.label).toBe('③ LLM 兜底 · 仅长尾（L3 弃权时）')
  })

  it('L2 + L3 启用 + LLM 未配置：③ 未配置（warning）', () => {
    const l3 = find({ ...base, level: 2, l3Enabled: true, llmEnabled: false }, '③')
    expect(l3?.label).toBe('③ LLM 兜底 · 未配置')
    expect(l3?.type).toBe('warning')
  })

  it('L3 + LLM 已配置：③ 无条件', () => {
    const l3 = find({ ...base, level: 3 }, '③')
    expect(l3?.label).toBe('③ LLM 兜底 · 无条件')
    expect(l3?.type).toBe('success')
  })

  it('L3 + LLM 未配置：③ 未配置（warning）', () => {
    const l3 = find({ ...base, level: 3, llmEnabled: false }, '③')
    expect(l3?.label).toBe('③ LLM 兜底 · 未配置')
    expect(l3?.type).toBe('warning')
  })

  it('L3 + 特征模型启用 + LLM 已配置：③-F 已启用，③ 仅长尾', () => {
    const layers = computePipelineLayers({ ...base, level: 3, l3Enabled: true })
    expect(find({ ...base, level: 3, l3Enabled: true }, '③-F')?.label).toBe('③-F 特征模型 · 已启用')
    expect(layers.find((l) => l.no === '③')?.label).toBe('③ LLM 兜底 · 仅长尾（特征模型弃权时）')
  })

  it('L3 + 特征模型启用 + LLM 未配置：③ 未配置（warning）', () => {
    const layers = computePipelineLayers({ ...base, level: 3, l3Enabled: true, llmEnabled: false })
    expect(layers.find((l) => l.no === '③')?.label).toBe('③ LLM 兜底 · 未配置')
    expect(layers.find((l) => l.no === '③')?.type).toBe('warning')
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

const start = (stages: string[]): StageEvent => ({ kind: 'start', stages })
const stage = (s: string, status: 'ran' | 'partial' | 'unavailable' | 'skipped', ms: number | null = 10): StageEvent => ({
  kind: 'stage',
  stage: s,
  status,
  ms,
})
const end = (total_ms: number): StageEvent => ({ kind: 'end', total_ms })
const chunkBegin = (stage: string, chunk: number, total: number): StageEvent => ({
  kind: 'chunk_begin',
  stage,
  chunk,
  total,
})
const chunkEnd = (
  stage: string,
  chunk: number,
  total: number,
  ok = true,
  ms = 30,
): StageEvent => ({ kind: 'chunk_end', stage, chunk, total, ok, ms })

const byKey = (evs: StageEvent[]) => {
  const v = applyStageEvents(evs)
  return Object.fromEntries(v.stages.map((s) => [s.key, s]))
}

describe('applyStageEvents（分阶段进度条视图）', () => {
  it('无 Start 事件（缓存 / 历史命中、旧服务端）：空骨架不渲染', () => {
    expect(applyStageEvents([])).toEqual({ stages: [], finished: false, totalMs: null })
    const onlyEnd = applyStageEvents([end(7)])
    expect(onlyEnd.stages).toEqual([])
    expect(onlyEnd.finished).toBe(true)
    expect(onlyEnd.totalMs).toBe(7)
  })

  it('Start 即骨架：并行组（rule/decision/feature）开局 running，串行位 pending', () => {
    const v = applyStageEvents([start(['rule', 'knowledge', 'llm'])])
    expect(v.stages.map((s) => s.key)).toEqual(['rule', 'knowledge', 'llm'])
    expect(v.stages.map((s) => s.status)).toEqual(['running', 'pending', 'pending'])
    expect(v.stages[0]).toMatchObject({ no: '①', label: '规则检测', ms: null })
  })

  it('渲染顺序按引擎推进序（rule/decision/feature/knowledge/llm），不受 Start 乱序影响', () => {
    const v = applyStageEvents([start(['llm', 'feature', 'rule'])])
    expect(v.stages.map((s) => s.key)).toEqual(['rule', 'feature', 'llm'])
    expect(v.stages.map((s) => s.no)).toEqual(['①', '③-F', '③'])
  })

  it('落定事件映射：ran→done 带耗时、skipped/unavailable 各归其位', () => {
    const m = byKey([
      start(['rule', 'decision', 'knowledge', 'llm']),
      stage('rule', 'ran', 12),
      stage('decision', 'skipped', null),
      stage('knowledge', 'unavailable', null),
    ])
    expect(m.rule).toMatchObject({ status: 'done', ms: 12 })
    expect(m.decision.status).toBe('skipped')
    expect(m.knowledge.status).toBe('unavailable')
  })

  it('推进位点亮：并行组全落定 → 知识召回 running；再全落定 → LLM running', () => {
    const evs: StageEvent[] = [start(['rule', 'feature', 'knowledge', 'llm']), stage('rule', 'ran')]
    // 并行组未全部落定：召回仍排队
    expect(byKey(evs).knowledge.status).toBe('pending')
    evs.push(stage('feature', 'ran'))
    expect(byKey(evs).knowledge.status).toBe('running')
    evs.push(stage('knowledge', 'ran'))
    expect(byKey(evs).llm.status).toBe('running')
    evs.push(stage('llm', 'ran'), end(99))
    const v = applyStageEvents(evs)
    expect(v.finished).toBe(true)
    expect(v.totalMs).toBe(99)
    expect(v.stages.every((s) => s.status === 'done')).toBe(true)
  })

  it('③-T 定向复查：串行序在知识召回与 LLM 之间，按前序落定依序点亮，未触发记 skipped', () => {
    const evs: StageEvent[] = [
      start(['rule', 'feature', 'knowledge', 'targeted', 'llm']),
      stage('rule', 'ran'),
      stage('feature', 'ran'),
    ]
    expect(byKey(evs).knowledge.status).toBe('running')
    evs.push(stage('knowledge', 'ran'))
    expect(byKey(evs).targeted.status).toBe('running')
    expect(byKey(evs).llm.status).toBe('pending')
    evs.push(stage('targeted', 'ran', 40), stage('llm', 'skipped', null), end(120))
    const v = applyStageEvents(evs)
    expect(v.stages.map((s) => s.key)).toEqual(['rule', 'feature', 'knowledge', 'targeted', 'llm'])
    expect(v.stages.map((s) => s.no)).toEqual(['①', '③-F', '②.5', '③-T', '③'])
    expect(v.stages.map((s) => s.status)).toEqual(['done', 'done', 'done', 'done', 'skipped'])
    expect(v.finished).toBe(true)
    // 在计划但未触发（全文 ③ 覆盖灰区）：引擎尾部补记 skipped，不留悬空节点
    const m = byKey([
      start(['rule', 'feature', 'knowledge', 'targeted', 'llm']),
      stage('rule', 'ran'),
      stage('feature', 'skipped', null),
      stage('knowledge', 'ran'),
      stage('targeted', 'skipped', null),
      stage('llm', 'ran', 90),
    ])
    expect(m.targeted.status).toBe('skipped')
  })

  it('End 事件带 total_ms（桌面端事件路径的唯一总量来源）', () => {
    const v = applyStageEvents([start(['rule']), stage('rule', 'ran', 5), end(42)])
    expect(v).toMatchObject({ finished: true, totalMs: 42 })
  })

  it('计划外阶段标识兜底：仍渲染（no 空、label = 标识）', () => {
    const v = applyStageEvents([start(['rule', 'mystery'])])
    const m = v.stages.find((s) => s.key === 'mystery')
    expect(m).toMatchObject({ no: '', label: 'mystery' })
  })

  it('段级事件聚合：begin 定 total，end 计 done；并发交错不依赖到达顺序', () => {
    const m = byKey([
      start(['rule', 'llm']),
      chunkBegin('llm', 1, 3),
      chunkBegin('llm', 2, 3),
      chunkEnd('llm', 2, 3),
      chunkBegin('llm', 3, 3),
    ])
    expect(m.llm.chunks).toEqual({ done: 1, total: 3 })
    // 失败块同样计 done（本轮已尝试，未复查由 quality 账本另行表达）
    const m2 = byKey([start(['rule', 'llm']), chunkBegin('llm', 1, 2), chunkEnd('llm', 1, 2, false)])
    expect(m2.llm.chunks).toEqual({ done: 1, total: 2 })
  })

  it('无段级事件的层 chunks 为 null；落定后聚合保留', () => {
    const v = applyStageEvents([start(['rule', 'knowledge'])])
    expect(v.stages.every((s) => s.chunks === null)).toBe(true)
    const m = byKey([
      start(['llm']),
      chunkBegin('llm', 1, 2),
      chunkEnd('llm', 1, 2),
      chunkBegin('llm', 2, 2),
      chunkEnd('llm', 2, 2),
      stage('llm', 'ran', 90),
      end(120),
    ])
    expect(m.llm.chunks).toEqual({ done: 2, total: 2 })
    expect(m.llm.status).toBe('done')
  })
})

describe('markStagesCanceled / stageViewsFromTimings（收尾视图）', () => {
  it('未落定（running / pending）→ canceled；已落定保持原样', () => {
    const out = markStagesCanceled(
      applyStageEvents([start(['rule', 'decision', 'llm']), stage('rule', 'ran', 8)]).stages,
    )
    expect(out.map((s) => s.status)).toEqual(['done', 'canceled', 'canceled'])
    expect(out[0].ms).toBe(8)
  })

  it('计时兜底合成：null / 缺失层缺位，其余 done 带耗时', () => {
    const out = stageViewsFromTimings({
      rule_ms: 12,
      nlp_ms: 8,
      decision_ms: null,
      l3_ms: 30,
      recall_ms: undefined,
      llm_ms: 800,
      total_ms: 900,
    })
    expect(out.map((s) => [s.key, s.status, s.ms])).toEqual([
      ['rule', 'done', 12],
      ['feature', 'done', 30],
      ['llm', 'done', 800],
    ])
    expect(out[0].no).toBe('①')
  })
})

const byKeyF = (evs: StageEvent[], opts = {}) =>
  Object.fromEntries(applyFlowEvents(evs, opts).map((s) => [s.key, s]))

describe('applyFlowEvents（数据流视图：S0–S7 投影，与层视图同源事件）', () => {
  it('检测已发起而事件未达：S0·S1 running，其余排队', () => {
    const out = applyFlowEvents([], { active: true })
    expect(out.map((s) => s.key)).toEqual(['s0', 's2', 's3', 's47'])
    expect(out.map((s) => s.status)).toEqual(['running', 'pending', 'pending', 'pending'])
  })

  it('Start 骨架：S0·S1 / S2 已落定，S3 进行中，终节点排队', () => {
    const m = byKeyF([start(['rule', 'decision', 'knowledge', 'llm'])], { active: true })
    expect(m.s0.status).toBe('done')
    expect(m.s2).toMatchObject({ status: 'done', note: '句级 ×1' })
    expect(m.s3).toMatchObject({ status: 'running', note: '0/2 层' })
    expect(m.knowledge.status).toBe('pending')
    expect(m.s47.status).toBe('pending')
  })

  it('层落定聚合进 S3；串行位随层视图同步点亮；End 后终节点落定', () => {
    const evs: StageEvent[] = [
      start(['rule', 'decision', 'knowledge', 'llm']),
      stage('rule', 'ran'),
      stage('decision', 'ran'),
    ]
    const m = byKeyF(evs, { active: true })
    expect(m.s3).toMatchObject({ status: 'done', note: '2/2 层' })
    expect(m.knowledge.status).toBe('running')
    evs.push(stage('knowledge', 'ran', 5), stage('llm', 'ran', 80), end(150))
    const m2 = byKeyF(evs)
    expect(m2.knowledge.note).toBe('5ms')
    expect(m2.llm.note).toBe('80ms')
    expect(m2.s47.status).toBe('done')
  })

  it('段级事件计入 S2 副行：段级 ×预算层数', () => {
    const m = byKeyF([start(['rule', 'llm']), chunkBegin('llm', 1, 3)], { active: true })
    expect(m.s2.note).toBe('句级 ×1 · 段级 ×1')
  })

  it('③-T 在计划才出现；中断时未落定节点转 canceled，已落定保持', () => {
    const planned = byKeyF([start(['rule', 'knowledge', 'targeted', 'llm'])], { active: true })
    expect(planned.targeted.status).toBe('pending')
    expect(applyFlowEvents([start(['rule', 'knowledge', 'llm'])]).map((s) => s.key)).not.toContain('targeted')
    const m = byKeyF([start(['rule', 'decision', 'llm']), stage('rule', 'ran', 8)], {
      active: false,
      canceled: true,
    })
    expect(m.s0.status).toBe('done')
    expect(m.s3.status).toBe('canceled')
    expect(m.llm.status).toBe('canceled')
    expect(m.s47.status).toBe('canceled')
  })

  it('瞬时（缓存 / 历史）：全部落定，终节点标注回放', () => {
    const out = applyFlowEvents([], { instant: true })
    expect(out.every((s) => s.status === 'done')).toBe(true)
    expect(out[out.length - 1].note).toBe('缓存/历史回放')
  })

  it('无事件兜底：由计时合成完成视图（S3 有层即落定）', () => {
    const m = byKeyF([], { active: false, stages: stageViewsFromTimings({ rule_ms: 12, total_ms: 15 }) })
    expect(m.s0.status).toBe('done')
    expect(m.s3.status).toBe('done')
    expect(m.s47.status).toBe('done')
  })

  it('响应完成但事件缺 End：终节点仍落定', () => {
    const m = byKeyF([start(['rule'])], { finished: true })
    expect(m.s47.status).toBe('done')
  })
})