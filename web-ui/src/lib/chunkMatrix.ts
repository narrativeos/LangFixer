// 块矩阵视图：句表（块行）× 检测层（列）的命中矩阵投影。
//
// 行来源 = units 事件（后端全局唯一切句器的句表，前端绝不自行切句）；
// 单元格 = 错误项 span 与各块区间的重叠投影（source 产出层 + also_found_by
// 共识层），零后端结果改动。③-T 结果按 llm 归因（layers 不含 targeted，
// 与 StageProgress 层视图同一口径），stat 臂在级联的 rule 位执行（段级事件
// 亦为 stage=rule），故 statistical 并入 rule 列。

import type { ErrorItem, LayerReport, StageEvent } from '../types'
import { STAGE_META, STAGE_ORDER } from './pipeline'

export type ChunkOrigin = 'punct' | 'marker' | 'pause' | 'fallback'

/** 切分来源标签（块行「边界依据」徽章） */
export const ORIGIN_LABELS: Record<ChunkOrigin, string> = {
  punct: '标点',
  marker: '标记词',
  pause: '停顿',
  fallback: '字数兜底',
}

export type CellStatus = 'hit' | 'pass' | 'skipped' | 'pending'

export interface MatrixCell {
  status: CellStatus
  /** 该块被该层命中的错误条数（含共识命中） */
  hits: number
}

export interface MatrixRow {
  span: [number, number]
  chars: number
  origin: ChunkOrigin
  /** 文本预览（空白折叠，超长截断） */
  preview: string
  cells: Record<string, MatrixCell>
}

export interface MatrixColumn {
  key: string
  no: string
  label: string
}

/** 取事件流中的句表（正常一条；重放/重检取最后一条）；无 units（旧服务端）→ null */
export function unitsFromEvents(
  events: StageEvent[],
): { spans: [number, number][]; origins: ChunkOrigin[] } | null {
  let last: { spans: [number, number][]; origins: ChunkOrigin[] } | null = null
  for (const ev of events)
    if (ev.kind === 'units') last = { spans: ev.spans, origins: ev.origins }
  return last
}

/** 层状态 → 单元格底色：未执行（skipped/unavailable/弃权）灰化 */
function layerActive(status: string | undefined): boolean | null {
  if (status === undefined) return null // 本次管线无该层报告 = 尚未落定
  return status === 'ran' || status === 'decided'
}

/** 错误项是否与该块区间重叠（半开区间相交） */
function overlaps(e: [number, number], c: [number, number]): boolean {
  return e[1] > c[0] && e[0] < c[1]
}

/** 该错误计名该层的次数（产出层或共识层；③-T 归因 llm；statistical 并入 rule） */
function countsForLayer(e: ErrorItem, layer: string): boolean {
  const norm = (s: string) => (s === 'targeted' ? 'llm' : s === 'statistical' ? 'rule' : s)
  if (e.source && norm(e.source) === layer) return true
  return (e.also_found_by ?? []).some((s) => norm(s) === layer)
}

export function buildChunkMatrix(args: {
  text: string
  units: { spans: [number, number][]; origins: ChunkOrigin[] }
  errors: ErrorItem[]
  columns: MatrixColumn[]
  layers: LayerReport[] | null
}): MatrixRow[] {
  const statusOf = new Map<string, string>()
  for (const l of args.layers ?? []) statusOf.set(l.layer, l.status)
  return args.units.spans.map(([s, e], i) => {
    const raw = args.text.slice(s, e)
    const preview =
      raw.replace(/\s+/g, ' ').trim().slice(0, 40) +
      (raw.replace(/\s+/g, ' ').trim().length > 40 ? '…' : '')
    const cells: Record<string, MatrixCell> = {}
    for (const col of args.columns) {
      const active = layerActive(statusOf.get(col.key))
      const hits = args.errors.filter((err) => overlaps(err.span, [s, e]) && countsForLayer(err, col.key)).length
      cells[col.key] =
        active === false
          ? { status: 'skipped', hits: 0 }
          : active === null
            ? { status: hits ? 'hit' : 'pending', hits }
            : { status: hits ? 'hit' : 'pass', hits }
    }
    return {
      span: [s, e] as [number, number],
      chars: [...raw].length,
      origin: args.units.origins[i] ?? 'punct',
      preview,
      cells,
    }
  })
}

/** 列 = 本次 start 事件声明的管线层（顺序与层视图一致）；③-T 不单列（归因 llm） */
export function matrixColumnsFromEvents(events: StageEvent[]): MatrixColumn[] {
  let plan: string[] | null = null
  for (const ev of events) if (ev.kind === 'start') plan = ev.stages
  if (!plan) return []
  const ordered = [
    ...STAGE_ORDER.filter((k) => plan!.includes(k)),
    ...plan!.filter((k) => !STAGE_ORDER.includes(k)),
  ]
  const seen = new Set<string>()
  const out: MatrixColumn[] = []
  for (const key of ordered) {
    if (key === 'targeted' || seen.has(key)) continue
    seen.add(key)
    const meta = STAGE_META[key] ?? { no: '', label: key }
    out.push({ key, no: meta.no, label: meta.label })
  }
  return out
}
