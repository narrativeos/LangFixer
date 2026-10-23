/**
 * JDM 决策表（zen-engine 2.1.2 方言）↔ 结构化模型
 *
 * 单元格编码约定（与 decision-tables/*.json 一致）：
 * - 条件格：JSON 字符串，内容为比较表达式，如 `== "spelling"` / `>= 0.9` / `== true`
 * - 输出格：JSON 编码的标量（字符串 / 数组），如 `"\"knowledge_gap\""` / `"[\"a → b\"]"`
 *
 * 本模块为纯函数（无 Vue / 网络依赖），可被 vitest 直接单测。
 */

// ─── JDM 原始结构 ───

export interface JdmInput {
  id: string
  name: string
  field: string
}

export interface JdmOutput {
  id: string
  name: string
  field: string
}

/** 一行规则：input id / output id → 单元格原始字符串（条件格为表达式，输出格为 JSON 编码值） */
export type JdmRule = Record<string, string>

export interface JdmTableContent {
  inputs: JdmInput[]
  outputs: JdmOutput[]
  rules: JdmRule[]
  hitPolicy: string
}

export interface JdmNode {
  id: string
  name: string
  type: string
  content?: JdmTableContent
}

export interface JdmDoc {
  nodes: JdmNode[]
  edges: { id: string; sourceId: string; targetId: string }[]
}

// ─── 结构化模型（UI 用） ───

export type CondOp = '==' | '!=' | '>' | '>=' | '<' | '<='

/** 条件格：操作符 + 原始值 token（`spelling` / `0.9` / `true`，不带引号） */
export interface CondCell {
  op: CondOp
  value: string
}

export interface RowModel {
  /** 行键 = matchedRule 输出值（表内唯一） */
  key: string
  /** input id → 条件（null = 任意） */
  conds: Record<string, CondCell | null>
  /** output id → 值（JSON 解码后的原始 token） */
  outputs: Record<string, string>
}

export interface TableModel {
  lang: string
  inputs: JdmInput[]
  outputs: JdmOutput[]
  rows: RowModel[]
  hitPolicy: string
}

export const COND_OPS: CondOp[] = ['==', '!=', '>', '>=', '<', '<=']

const OP_RE = /^(==|!=|>=|<=|>|<)\s*(.+)$/

/** 从 JDM 文档中提取 decisionTableNode 内容 */
export function extractTable(doc: JdmDoc): JdmTableContent {
  const node = doc.nodes.find((n) => n.type === 'decisionTableNode')
  if (!node?.content) throw new Error('未找到 decisionTableNode（JDM 结构不完整）')
  return node.content
}

/** 解析单元格表达式 → CondCell；无法解析时抛错（带上下文） */
export function decodeCell(expr: string, ctx: string): CondCell {
  const m = OP_RE.exec(expr)
  if (!m) throw new Error(`${ctx}：无法解析条件表达式 "${expr}"（应为 == / != / > / >= / < / <= 开头）`)
  const op = m[1] as CondOp
  const tok = m[2].trim()
  let value: string
  if (tok.startsWith('"')) {
    try {
      value = JSON.parse(tok) as string
    } catch {
      value = tok.slice(1, -1)
    }
  } else {
    value = tok
  }
  return { op, value }
}

/** CondCell → 单元格表达式（值按类型编码：布尔/数字原样，字符串加引号） */
export function encodeCell(cell: CondCell): string {
  const v = cell.value
  const tok = v === 'true' || v === 'false' || /^-?\d+(\.\d+)?$/.test(v) ? v : JSON.stringify(v)
  return `${cell.op} ${tok}`
}

/** JSON 解码输出格（`"\"a\""` → `a`；数组等复杂值原样保留） */
export function decodeOutputCell(raw: string, ctx: string): string {
  try {
    const v: unknown = JSON.parse(raw)
    if (typeof v === 'string' || typeof v === 'number' || typeof v === 'boolean') return String(v)
    return raw
  } catch {
    throw new Error(`${ctx}：输出值不是合法 JSON 编码: ${raw.slice(0, 60)}`)
  }
}

/** 输出值 → JSON 编码单元格（字符串加引号；数组 / 对象 / 布尔 / 数字按类型编码） */
export function encodeOutputCell(value: string): string {
  const t = value.trim()
  if (t.startsWith('[') || t.startsWith('{')) return t // 数组 / 对象：原样（已是 JSON 文本）
  if (t === 'true' || t === 'false' || /^-?\d+(\.\d+)?$/.test(t)) return JSON.stringify(t)
  return JSON.stringify(t)
}

/** 完整解析：raw JSON → 结构化模型 */
export function parseTable(raw: string, lang: string): TableModel {
  const doc = JSON.parse(raw) as JdmDoc
  const content = extractTable(doc)
  const rows: RowModel[] = content.rules.map((rule, idx) => {
    const conds: Record<string, CondCell | null> = {}
    for (const input of content.inputs) {
      const cell = rule[input.id]
      if (cell === undefined || cell === '') {
        conds[input.id] = null
      } else {
        conds[input.id] = decodeCell(cell, `第 ${idx + 1} 行 ${input.id}`)
      }
    }
    const outputs: Record<string, string> = {}
    for (const out of content.outputs) {
      const cell = rule[out.id]
      if (cell === undefined) continue
      outputs[out.id] = decodeOutputCell(cell, `第 ${idx + 1} 行 ${out.id}`)
    }
    const key = outputs.matchedRule ?? `row_${idx}`
    return { key, conds, outputs }
  })
  return {
    lang,
    inputs: content.inputs,
    outputs: content.outputs,
    rows,
    hitPolicy: content.hitPolicy ?? 'first',
  }
}

/** 结构化模型 → JDM JSON 字符串 */
export function serializeTable(model: TableModel): string {
  const rules: JdmRule[] = model.rows.map((row) => {
    const rule: JdmRule = {}
    for (const input of model.inputs) {
      const cell = row.conds[input.id]
      if (cell) rule[input.id] = encodeCell(cell)
    }
    for (const out of model.outputs) {
      const v = row.outputs[out.id]
      if (v === undefined) continue
      rule[out.id] = out.id === 'matchedRule' ? JSON.stringify(v) : encodeOutputCell(v)
    }
    return rule
  })
  const content: JdmTableContent = {
    inputs: model.inputs,
    outputs: model.outputs,
    rules,
    hitPolicy: model.hitPolicy,
  }
  const doc: JdmDoc = {
    nodes: [
      { id: 'input', name: 'input', type: 'inputNode' },
      { id: 'table', name: `${model.lang}_gec_attribution`, type: 'decisionTableNode', content },
      { id: 'output', name: 'output', type: 'outputNode' },
    ],
    edges: [
      { id: 'e1', sourceId: 'input', targetId: 'table' },
      { id: 'e2', sourceId: 'table', targetId: 'output' },
    ],
  }
  return JSON.stringify(doc, null, 2)
}

// ─── 展示辅助 ───

/** 条件格人读化：`= spelling` / `≥ 0.9` / `任意` */
export function humanizeCell(cell: CondCell | null | undefined): string {
  if (!cell) return '任意'
  const sym: Record<CondOp, string> = {
    '==': '=',
    '!=': '≠',
    '>': '>',
    '>=': '≥',
    '<': '<',
    '<=': '≤',
  }
  return `${sym[cell.op]} ${cell.value}`
}

/** 输入列类型推断：已知 id 优先，否则按现有值推断 */
export function inferInputType(id: string, rows: RowModel[]): 'string' | 'number' | 'boolean' {
  if (id === 'confidence' || id === 'history') return 'number'
  if (id === 'isNovice') return 'boolean'
  const vals = rows
    .map((r) => r.conds[id])
    .filter((c): c is CondCell => !!c)
    .map((c) => c.value)
  if (vals.length > 0) {
    if (vals.every((v) => v === 'true' || v === 'false')) return 'boolean'
    if (vals.every((v) => /^-?\d+(\.\d+)?$/.test(v))) return 'number'
  }
  return 'string'
}

/** 某输入列出现过的所有取值（供编辑器下拉选项） */
export function distinctValues(rows: RowModel[], inputId: string): string[] {
  const seen = new Set<string>()
  for (const r of rows) {
    const c = r.conds[inputId]
    if (c) seen.add(c.value)
  }
  return [...seen]
}

// ─── diff（保存前展示变更） ───

export interface TableDiff {
  added: RowModel[]
  changed: { before: RowModel; after: RowModel }[]
  removed: RowModel[]
  hasChanges: boolean
}

function rowFingerprint(row: RowModel, inputs: JdmInput[], outputs: JdmOutput[]): string {
  const parts: string[] = []
  for (const i of inputs) {
    const c = row.conds[i.id]
    parts.push(i.id + ':' + (c ? encodeCell(c) : ''))
  }
  for (const o of outputs) parts.push(o.id + ':' + (row.outputs[o.id] ?? ''))
  return parts.join('|')
}

/** 结构化 diff：按行键（matchedRule）对齐，键相同则逐格比较 */
export function diffTables(base: TableModel, next: TableModel): TableDiff {
  const baseMap = new Map(base.rows.map((r) => [r.key, r]))
  const nextMap = new Map(next.rows.map((r) => [r.key, r]))
  const added: RowModel[] = []
  const removed: RowModel[] = []
  const changed: { before: RowModel; after: RowModel }[] = []
  for (const r of next.rows) {
    const b = baseMap.get(r.key)
    if (!b) added.push(r)
    else if (rowFingerprint(b, base.inputs, base.outputs) !== rowFingerprint(r, next.inputs, next.outputs)) {
      changed.push({ before: b, after: r })
    }
  }
  for (const r of base.rows) {
    if (!nextMap.has(r.key)) removed.push(r)
  }
  return {
    added,
    changed,
    removed,
    hasChanges: added.length + changed.length + removed.length > 0,
  }
}

/** 按 grammarPoint / errorType 分组（无对应条件的行归入「兜底」组） */
export function groupRows(
  model: TableModel,
  mode: 'grammarPoint' | 'errorType' | 'none',
): { name: string; rows: RowModel[] }[] {
  if (mode === 'none') return [{ name: '全部规则', rows: model.rows }]
  const inputId = mode === 'grammarPoint' ? 'grammarPoint' : 'errorType'
  if (!model.inputs.some((i) => i.id === inputId)) return [{ name: '全部规则', rows: model.rows }]
  const groups = new Map<string, RowModel[]>()
  for (const row of model.rows) {
    const c = row.conds[inputId]
    const name = c && c.op === '==' ? c.value : '兜底 / 其他'
    if (!groups.has(name)) groups.set(name, [])
    groups.get(name)!.push(row)
  }
  return [...groups.entries()].map(([name, rows]) => ({ name, rows }))
}

// ─── 展示 / 预校验辅助 ───

/** 列显示名（JDM 的 name/field 均为原始字段名，此处补人读名；未知列回退原 id） */
export const COLUMN_LABELS: Record<string, string> = {
  // 输入列
  errorType: '错误类型',
  confidence: '置信度',
  history: '历史错误数',
  isNovice: '是否新手',
  grammarPoint: '知识点',
  // 输出列
  matchedRule: '规则 ID',
  attribution: '归因',
  weinerLocus: 'Weiner 位点',
  weinerStability: 'Weiner 稳定性',
  weinerControllability: 'Weiner 可控性',
  cefrLevel: 'CEFR 等级',
  remedial: '补救建议',
  source: '知识点来源',
  examples: '典型例句',
}

export function columnLabel(id: string): string {
  return COLUMN_LABELS[id] ?? id
}

/** 智能分组：分组数过多且组均行数过小时分组只是噪音 → 回退单组 */
export function smartGroups(
  model: TableModel,
  mode: 'grammarPoint' | 'errorType' | 'none',
): { groups: { name: string; rows: RowModel[] }[]; ungrouped: boolean; reason?: string } {
  if (mode === 'none') return { groups: [{ name: '全部规则', rows: model.rows }], ungrouped: false }
  const g = groupRows(model, mode)
  const total = model.rows.length
  if (g.length > 40 && total > 0 && g.length / total > 0.5) {
    const label = mode === 'grammarPoint' ? '知识点' : '错误类型'
    return {
      groups: [{ name: '全部规则', rows: model.rows }],
      ungrouped: true,
      reason: `${label}取值过多（${g.length} 组 / ${total} 行），已自动关闭分组`,
    }
  }
  return { groups: g, ungrouped: false }
}

/** 试算上下文：由行条件构造（errorType 须为 == 条件；边界值向命中方向微移） */
export interface TryContext {
  error_type: string
  confidence: number
  session_error_count: number
  grammar_point?: string
}

export function buildTryContext(row: RowModel): TryContext | null {
  const et = row.conds.errorType
  if (!et || et.op !== '==') return null
  let confidence = 0.9
  const conf = row.conds.confidence
  if (conf) {
    const n = parseFloat(conf.value)
    if (!Number.isNaN(n)) confidence = conf.op === '<' || conf.op === '<=' ? Math.max(0, n - 0.01) : n
  }
  let count = 0
  const h = row.conds.history
  if (h) {
    const n = parseInt(h.value, 10)
    if (!Number.isNaN(n)) count = h.op === '>' ? n + 1 : h.op === '>=' ? n : Math.max(0, n - 1)
  }
  const gp = row.conds.grammarPoint
  return {
    error_type: et.value,
    confidence,
    session_error_count: count,
    ...(gp && gp.op === '==' ? { grammar_point: gp.value } : {}),
  }
}

/** 客户端预校验问题（可定位到行；服务端 dry-run 错误另计） */
export interface ModelIssue {
  /** 关联行键（空 = 表级问题） */
  key: string
  message: string
}

/** 客户端预校验：规则 ID（matchedRule）必填且表内唯一 */
export function validateModel(model: TableModel): ModelIssue[] {
  const issues: ModelIssue[] = []
  const seen = new Map<string, number>()
  model.rows.forEach((r, i) => {
    const key = (r.outputs.matchedRule ?? '').trim()
    if (!key) {
      issues.push({ key: r.key, message: `第 ${i + 1} 行：规则 ID（matchedRule）缺失` })
      return
    }
    const n = (seen.get(key) ?? 0) + 1
    seen.set(key, n)
    if (n === 2) issues.push({ key, message: `规则 ID "${key}" 重复出现` })
  })
  return issues
}

