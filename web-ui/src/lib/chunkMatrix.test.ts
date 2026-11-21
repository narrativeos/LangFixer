import { describe, expect, it } from 'vitest'
import type { ErrorItem, LayerReport, StageEvent } from '../types'
import { buildChunkMatrix, matrixColumnsFromEvents, unitsFromEvents } from './chunkMatrix'

const err = (span: [number, number], source: string, also?: string[]): ErrorItem => ({
  lang: 'zh',
  error_type: 'typo',
  confidence: 0.9,
  span,
  original: 'x',
  correction: 'y',
  message: '',
  source,
  ...(also ? { also_found_by: also } : {}),
})

const events: StageEvent[] = [
  { kind: 'start', stages: ['rule', 'knowledge', 'targeted', 'llm'] },
  { kind: 'units', spans: [[0, 4], [4, 8]], origins: ['punct', 'fallback'] },
  { kind: 'end', total_ms: 1 },
]

describe('chunkMatrix 投影', () => {
  it('unitsFromEvents 取句表；无 units 返回 null', () => {
    expect(unitsFromEvents(events)).toEqual({
      spans: [[0, 4], [4, 8]],
      origins: ['punct', 'fallback'],
    })
    expect(unitsFromEvents([{ kind: 'start', stages: ['rule'] }])).toBeNull()
  })

  it('列 = start 计划（层视图序），③-T 不单列', () => {
    expect(matrixColumnsFromEvents(events).map((c) => c.key)).toEqual([
      'rule',
      'knowledge',
      'llm',
    ])
  })

  it('无 start（缓存/历史命中）：列从 layers 派生，statistical/targeted 不单列；无 layers 空列', () => {
    const layers: LayerReport[] = [
      { layer: 'rule', status: 'ran', found: 1 },
      { layer: 'knowledge', status: 'ran', found: 0 },
      { layer: 'statistical', status: 'ran', found: 0 },
      { layer: 'targeted', status: 'skipped', found: 0 },
    ]
    // 派生列按 STAGE_ORDER 排序（knowledge 在前、llm 在后，同层视图）
    const noStart: StageEvent[] = [{ kind: 'end', total_ms: 1 }]
    expect(matrixColumnsFromEvents(noStart, layers).map((c) => c.key)).toEqual([
      'rule',
      'knowledge',
    ])
    expect(matrixColumnsFromEvents(noStart, null)).toEqual([])
    expect(matrixColumnsFromEvents(noStart, []).map((c) => c.key)).toEqual([])
  })

  it('单元格：命中→hit 计条数；层 skipped→灰；无 layers 报告时命中仍 hit、未命中 pending', () => {
    // 字节 span：每字 3 字节，两块各 12 字节
    const text = '第一块文第二块文'
    const units = { spans: [[0, 12], [12, 24]] as [number, number][], origins: ['punct', 'pause'] as const }
    const cols = [
      { key: 'rule', no: '①', label: '规则检测' },
      { key: 'knowledge', no: '②.5', label: '知识召回' },
    ]
    const rows = buildChunkMatrix({
      text: text as string,
      units: { spans: units.spans, origins: [...units.origins] },
      errors: [err([1, 2], 'rule'), err([1, 3], 'rule', ['knowledge'])],
      columns: cols,
      layers: [
        { layer: 'rule', status: 'ran', found: 2 },
        { layer: 'knowledge', status: 'skipped', found: 0 },
      ],
    })
    expect(rows).toHaveLength(2)
    expect(rows[0].cells.rule).toEqual({ status: 'hit', hits: 2 })
    expect(rows[0].cells.knowledge).toEqual({ status: 'skipped', hits: 0 })
    expect(rows[1].cells.rule).toEqual({ status: 'pass', hits: 0 })
    expect(rows[0].chars).toBe(4)
    expect(rows[0].origin).toBe('punct')
    expect(rows[1].origin).toBe('pause')
    // 无 layers 报告（进行中）：命中仍 hit，未命中 pending
    const r2 = buildChunkMatrix({
      text,
      units: { spans: units.spans, origins: [...units.origins] },
      errors: [err([1, 2], 'rule')],
      columns: cols,
      layers: null,
    })
    expect(r2[0].cells.rule.status).toBe('hit')
    expect(r2[1].cells.rule.status).toBe('pending')
  })

  it('审计 F3 复查态同步：clean 复查 span 重叠的块在 ③ 列显 recheck；命中仍优先 hit；其余列不受影响', () => {
    const cols = [
      { key: 'rule', no: '①', label: '规则检测' },
      { key: 'llm', no: '③', label: 'LLM 复核' },
    ]
    const layers: LayerReport[] = [
      { layer: 'rule', status: 'ran', found: 1 },
      { layer: 'llm', status: 'skipped', found: 0 },
    ]
    const base = {
      text: '第一块文第二块文',
      units: { spans: [[0, 12], [12, 24]] as [number, number][], origins: ['punct', 'punct'] as const },
      columns: cols,
      layers,
    }
    // 复查无错：命中块仍 hit，被 clean 复查的未命中块→recheck（不论层态 skipped）
    const rows = buildChunkMatrix({
      ...base,
      units: { spans: base.units.spans, origins: [...base.units.origins] },
      errors: [err([1, 2], 'decision')],
      recheckClean: [[12, 24]],
    })
    expect(rows[0].cells.llm.status).toBe('skipped')
    expect(rows[1].cells.llm).toEqual({ status: 'recheck', hits: 0 })
    expect(rows[1].cells.rule.status).toBe('pass')
    // 命中优先：同一块既有复查又有 llm 产出命中→仍计 hit
    const hit = buildChunkMatrix({
      ...base,
      units: { spans: base.units.spans, origins: [...base.units.origins] },
      errors: [err([13, 14], 'llm')],
      recheckClean: [[12, 24]],
    })
    expect(hit[1].cells.llm).toEqual({ status: 'hit', hits: 1 })
  })

  it('归因：statistical 并入 rule，targeted 归因 llm', () => {
    const rows = buildChunkMatrix({
      text: 'aaaaaaaa',
      units: { spans: [[0, 8]], origins: ['punct'] },
      errors: [err([0, 1], 'statistical'), err([2, 3], 'targeted')],
      columns: [
        { key: 'rule', no: '①', label: '规则检测' },
        { key: 'llm', no: '③', label: 'LLM 复核' },
      ],
      layers: [
        { layer: 'rule', status: 'ran', found: 1 },
        { layer: 'llm', status: 'ran', found: 1 },
      ],
    })
    expect(rows[0].cells.rule.hits).toBe(1)
    expect(rows[0].cells.llm.hits).toBe(1)
  })

  it('未执行层复查合流同步：source 属本列→hit 投影；纯共识命中不点亮', () => {
    const cols = [
      { key: 'decision', no: '②', label: '决策模型' },
      { key: 'llm', no: '③', label: 'LLM 复核' },
    ]
    const layers: LayerReport[] = [
      { layer: 'decision', status: 'ran', found: 1 },
      { layer: 'llm', status: 'skipped', found: 0 },
    ]
    // 人工定向复查合流入的 source='llm' 错误：③ 层 skipped 也要如实投影
    const merged = buildChunkMatrix({
      text: 'abcdefgh',
      units: { spans: [[0, 8]], origins: ['punct'] },
      errors: [err([0, 8], 'llm', ['decision'])],
      columns: cols,
      layers,
    })
    expect(merged[0].cells.llm).toEqual({ status: 'hit', hits: 1 })
    expect(merged[0].cells.decision).toEqual({ status: 'hit', hits: 1 })
    // 纯共识命中（also_found_by llm）在不执行的 ③ 列仍灰：保持「本层未执行」语义
    const consensusOnly = buildChunkMatrix({
      text: 'abcdefgh',
      units: { spans: [[0, 8]], origins: ['punct'] },
      errors: [err([0, 8], 'decision', ['llm'])],
      columns: cols,
      layers,
    })
    expect(consensusOnly[0].cells.llm).toEqual({ status: 'skipped', hits: 0 })
  })

  it('CJK 字节 span 预览不错位（字节→UTF-16 口径转换）', () => {
    // 本用例是错位 bug 的回归测试：直接 slice 字节位会在纯中文文本上成倍偏移
    const text = '武汉大学图书馆。' + '东湖风光很好。' // 字节：首句 8 字 24B，次句 7 字 21B
    const rows = buildChunkMatrix({
      text,
      units: { spans: [[0, 24], [24, 45]], origins: ['punct', 'punct'] },
      errors: [],
      columns: [],
      layers: null,
    })
    expect(rows.map((r) => r.preview)).toEqual(['武汉大学图书馆。', '东湖风光很好。'])
    expect(rows.map((r) => r.chars)).toEqual([8, 7])
    expect(rows[1].text).toBe('东湖风光很好。')
  })

  it('纯空白/零宽铺满片段不进矩阵行（与 quality 句覆盖同口径）', () => {
    const text = '一。\n\n\n二。' // 字节：[0,6) [6,9)纯空白 [9,15]
    const rows = buildChunkMatrix({
      text,
      units: { spans: [[0, 6], [6, 9], [9, 15]], origins: ['punct', 'punct', 'punct'] },
      errors: [],
      columns: [],
      layers: null,
    })
    expect(rows.map((r) => r.preview)).toEqual(['一。', '二。'])
    expect(rows[1].origin).toBe('punct')
  })

  it('预览折叠空白并截断，字符数按码点计', () => {
    const rows = buildChunkMatrix({
      text: '短\n\n句   子' + '长'.repeat(50),
      // 字节：短3+换行2+句3+空格3+子3=14，长×50=150 → [0,164)
      units: { spans: [[0, 164]], origins: ['punct'] },
      errors: [],
      columns: [],
      layers: null,
    })
    expect(rows[0].preview).toBe('短 句 子' + '长'.repeat(35) + '…')
    expect(rows[0].chars).toBe(8 + 50)
  })
})
