import { describe, expect, it } from 'vitest'
import type { ErrorItem, StageEvent } from '../types'
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

  it('单元格：命中→hit 计条数；层 skipped→灰；无 layers 报告时命中仍 hit、未命中 pending', () => {
    const text = '第一块文第二块文'
    const units = { spans: [[0, 4], [4, 8]] as [number, number][], origins: ['punct', 'pause'] as const }
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

  it('预览折叠空白并截断，字符数按码点计', () => {
    const rows = buildChunkMatrix({
      text: '短\n\n句   子' + '长'.repeat(50),
      units: { spans: [[0, 58]], origins: ['punct'] },
      errors: [],
      columns: [],
      layers: null,
    })
    expect(rows[0].preview).toBe('短 句 子' + '长'.repeat(35) + '…')
    expect(rows[0].chars).toBe(8 + 50)
  })
})
