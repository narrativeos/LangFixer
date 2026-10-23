/**
 * jdm.ts 单元测试（vitest）
 *
 * 覆盖：单元格编解码 round-trip、真实决策表解析、序列化往返、diff、分组。
 */
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'
import {
  buildTryContext,
  columnLabel,
  decodeCell,
  decodeOutputCell,
  diffTables,
  distinctValues,
  encodeCell,
  encodeOutputCell,
  groupRows,
  inferInputType,
  parseTable,
  serializeTable,
  smartGroups,
  validateModel,
  type RowModel,
  type TableModel,
} from './jdm'

function fixture(lang: string): string {
  return readFileSync(fileURLToPath(new URL(`../../../decision-tables/${lang}_gec_attribution.json`, import.meta.url)), 'utf8')
}

describe('单元格编解码', () => {
  it('字符串条件 round-trip', () => {
    const c = decodeCell('== "spelling"', 'test')
    expect(c).toEqual({ op: '==', value: 'spelling' })
    expect(encodeCell(c)).toBe('== "spelling"')
  })

  it('数字 / 布尔条件 round-trip', () => {
    expect(decodeCell('>= 0.9', 't')).toEqual({ op: '>=', value: '0.9' })
    expect(encodeCell({ op: '>=', value: '0.9' })).toBe('>= 0.9')
    expect(decodeCell('== true', 't')).toEqual({ op: '==', value: 'true' })
    expect(encodeCell({ op: '==', value: 'true' })).toBe('== true')
    expect(decodeCell('> 1', 't')).toEqual({ op: '>', value: '1' })
  })

  it('字符串值加引号、布尔/数字不加', () => {
    expect(encodeCell({ op: '==', value: 'article.1' })).toBe('== "article.1"')
    expect(encodeCell({ op: '<', value: '0.9' })).toBe('< 0.9')
    expect(encodeCell({ op: '!=', value: 'false' })).toBe('!= false')
  })

  it('非法表达式抛错（带上下文）', () => {
    expect(() => decodeCell('spelling', '第 3 行 errorType')).toThrow(/第 3 行 errorType/)
  })

  it('输出格编解码 round-trip', () => {
    expect(decodeOutputCell('"knowledge_gap"', 't')).toBe('knowledge_gap')
    expect(encodeOutputCell('knowledge_gap')).toBe('"knowledge_gap"')
    // 数组原样保留
    const arr = '["a → b"]'
    expect(encodeOutputCell(arr)).toBe(arr)
    expect(decodeOutputCell(JSON.stringify(arr), 't')).toBe(arr)
  })
})

describe('真实决策表解析', () => {
  it('zh 表：10 行 / 4 输入，条件解析正确', () => {
    const m = parseTable(fixture('zh'), 'zh')
    expect(m.rows).toHaveLength(10)
    expect(m.inputs.map((i) => i.id)).toEqual(['errorType', 'confidence', 'history', 'isNovice'])
    const r0 = m.rows[0]
    expect(r0.conds.errorType).toEqual({ op: '==', value: 'spelling' })
    expect(r0.conds.confidence).toEqual({ op: '>=', value: '0.9' })
    expect(r0.outputs.matchedRule).toBe('spelling_high_conf_old_hand')
    expect(r0.outputs.attribution).toBe('habit_solidified')
  })

  it('en 表：31 行纯规则，与 zh/ja 结构同构（4 输入 / 7 输出）', () => {
    const m = parseTable(fixture('en'), 'en')
    expect(m.rows).toHaveLength(31)
    expect(m.inputs.map((i) => i.id)).toEqual(['errorType', 'confidence', 'history', 'isNovice'])
    expect(m.outputs.map((o) => o.id)).toContain('matchedRule')
    expect(m.rows[0].outputs.matchedRule).toBe('article_novice')
    // matchedRule 唯一
    const keys = new Set(m.rows.map((r) => r.key))
    expect(keys.size).toBe(m.rows.length)
  })

  it('序列化 → 再解析：语义一致（round-trip）', () => {
    for (const lang of ['zh', 'en', 'ja']) {
      const m1 = parseTable(fixture(lang), lang)
      const m2 = parseTable(serializeTable(m1), lang)
      expect(m2.rows).toHaveLength(m1.rows.length)
      for (const [a, b] of m1.rows.map((r, i) => [r, m2.rows[i]] as const)) {
        expect(b.key).toBe(a.key)
        for (const input of m1.inputs) {
          const ca = a.conds[input.id]
          const cb = b.conds[input.id]
          expect(cb).toEqual(ca)
        }
        for (const out of m1.outputs) {
          expect(b.outputs[out.id]).toBe(a.outputs[out.id])
        }
      }
    }
  })

  it('分组：grammarPoint 分组 + 兜底组（合成模型）', () => {
    // 用合成模型验证 grammarPoint 分组逻辑（真实 en 表已无 grammarPoint 输入）
    const m: TableModel = {
      lang: 'en',
      inputs: [
        { id: 'errorType', name: 'errorType', field: 'errorType' },
        { id: 'grammarPoint', name: 'grammarPoint', field: 'grammarPoint' },
      ],
      outputs: [{ id: 'matchedRule', name: 'matchedRule', field: 'matchedRule' }],
      rows: [
        {
          key: 'p1',
          conds: { errorType: { op: '==', value: 'article' }, grammarPoint: { op: '==', value: 'article.1' } },
          outputs: { matchedRule: 'p1' },
        },
        {
          key: 'p2',
          conds: { errorType: { op: '==', value: 'article' }, grammarPoint: { op: '==', value: 'article.2' } },
          outputs: { matchedRule: 'p2' },
        },
        {
          key: 'fb',
          conds: { errorType: { op: '==', value: 'article' }, grammarPoint: null },
          outputs: { matchedRule: 'fb' },
        },
      ],
      hitPolicy: 'first',
    }
    const groups = groupRows(m, 'grammarPoint')
    expect(groups.length).toBe(3)
    const fallback = groups.find((g) => g.name === '兜底 / 其他')
    expect(fallback).toBeDefined()
    expect(groups.reduce((n, g) => n + g.rows.length, 0)).toBe(m.rows.length)
  })

  it('类型推断：isNovice=boolean / confidence=number / errorType=string', () => {
    const m = parseTable(fixture('zh'), 'zh')
    expect(inferInputType('isNovice', m.rows)).toBe('boolean')
    expect(inferInputType('confidence', m.rows)).toBe('number')
    expect(inferInputType('errorType', m.rows)).toBe('string')
    expect(distinctValues(m.rows, 'errorType').sort()).toEqual(
      ['collocation', 'grammar', 'punctuation', 'spelling'],
    )
  })
})

describe('diff', () => {
  function makeModel(rows: RowModel[]): TableModel {
    return {
      lang: 'zh',
      inputs: [
        { id: 'errorType', name: 'errorType', field: 'errorType' },
        { id: 'isNovice', name: 'isNovice', field: 'isNovice' },
      ],
      outputs: [
        { id: 'matchedRule', name: 'matchedRule', field: 'matchedRule' },
        { id: 'attribution', name: 'attribution', field: 'attribution' },
      ],
      rows,
      hitPolicy: 'first',
    }
  }
  const rowA: RowModel = {
    key: 'r_a',
    conds: { errorType: { op: '==', value: 'spelling' }, isNovice: null },
    outputs: { matchedRule: 'r_a', attribution: 'knowledge_gap' },
  }
  const rowB: RowModel = {
    key: 'r_b',
    conds: { errorType: { op: '==', value: 'grammar' }, isNovice: { op: '==', value: 'true' } },
    outputs: { matchedRule: 'r_b', attribution: 'rule_missing' },
  }

  it('新增 / 修改 / 删除识别', () => {
    const base = makeModel([rowA])
    const next = makeModel([
      { ...rowA, outputs: { ...rowA.outputs, attribution: 'habit_solidified' } },
      rowB,
    ])
    const d = diffTables(base, next)
    expect(d.hasChanges).toBe(true)
    expect(d.added.map((r) => r.key)).toEqual(['r_b'])
    expect(d.changed.map((c) => c.after.key)).toEqual(['r_a'])
    expect(d.removed).toHaveLength(0)
  })

  it('无变更时 hasChanges=false', () => {
    const base = makeModel([rowA, rowB])
    const next = makeModel(JSON.parse(JSON.stringify([rowA, rowB])))
    expect(diffTables(base, next).hasChanges).toBe(false)
  })
})

describe('展示 / 预校验辅助', () => {
  it('columnLabel：已知列给中文名，未知列回退原 id', () => {
    expect(columnLabel('errorType')).toBe('错误类型')
    expect(columnLabel('matchedRule')).toBe('规则 ID')
    expect(columnLabel('grammarPoint')).toBe('知识点')
    expect(columnLabel('someFutureColumn')).toBe('someFutureColumn')
  })

  it('smartGroups：en 表无 grammarPoint 输入 → 单组（点级规则已移出表）', () => {
    const m = parseTable(fixture('en'), 'en')
    const res = smartGroups(m, 'grammarPoint')
    expect(res.ungrouped).toBe(false)
    expect(res.groups).toHaveLength(1)
    expect(res.groups[0].rows).toHaveLength(m.rows.length)
  })

  it('smartGroups：zh 表（4 组）正常分组；none 模式单组', () => {
    const m = parseTable(fixture('zh'), 'zh')
    const res = smartGroups(m, 'errorType')
    expect(res.ungrouped).toBe(false)
    expect(res.groups.length).toBeGreaterThanOrEqual(2)
    const none = smartGroups(m, 'none')
    expect(none.ungrouped).toBe(false)
    expect(none.groups).toHaveLength(1)
  })

  it('buildTryContext：由行条件构造试算上下文（边界值向命中方向微移）', () => {
    const row: RowModel = {
      key: 'r1',
      conds: {
        errorType: { op: '==', value: 'spelling' },
        confidence: { op: '<', value: '0.9' },
        history: { op: '>', value: '3' },
        grammarPoint: { op: '==', value: 'article.1' },
      },
      outputs: { matchedRule: 'r1' },
    }
    expect(buildTryContext(row)).toEqual({
      error_type: 'spelling',
      confidence: 0.89,
      session_error_count: 4,
      grammar_point: 'article.1',
    })
    // 无 errorType == 条件 → 无法构造
    const noEt: RowModel = { key: 'r2', conds: {}, outputs: {} }
    expect(buildTryContext(noEt)).toBeNull()
  })

  it('validateModel：规则 ID 缺失 / 重复检测', () => {
    const model: TableModel = {
      lang: 'zh',
      inputs: [{ id: 'errorType', name: 'errorType', field: 'errorType' }],
      outputs: [{ id: 'matchedRule', name: 'matchedRule', field: 'matchedRule' }],
      rows: [
        { key: 'a', conds: {}, outputs: { matchedRule: 'a' } },
        { key: 'b', conds: {}, outputs: {} },
        { key: 'a', conds: {}, outputs: { matchedRule: 'a' } },
      ],
      hitPolicy: 'first',
    }
    const issues = validateModel(model)
    expect(issues).toHaveLength(2)
    expect(issues[0].message).toContain('缺失')
    expect(issues[1].message).toContain('重复')
    expect(issues[1].key).toBe('a')
  })
})
