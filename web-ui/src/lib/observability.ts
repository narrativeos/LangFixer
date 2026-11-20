/**
 * §7 可观测性看板的纯函数面（看板只读展示：格式化 / 聚合，可测）
 *
 * 数据源 = `GET /gec/observability/*`（request_summary 账本）。
 * 这里只放无副作用的换算，供 ObservabilityPanel 与单测共用。
 */
import type { ObservabilityDropped } from '../types'

/** dropped 四桶的人话标签（与 gec-core DroppedLedger 字段一一对应） */
export const DROPPED_LABELS: Record<keyof ObservabilityDropped, string> = {
  ownership: '所有权空洞',
  pii_mask: '掩码过滤',
  out_of_range: '越界丢弃',
  out_of_scope: '块外丢弃',
}

/** 比率（后端已给 0..1 小数）→ 百分数文本；null = 当日无样本 */
export function rate(v: number | null | undefined): string {
  if (v == null || Number.isNaN(v)) return '—'
  return `${(v * 100).toFixed(1)}%`
}

/** 毫秒 → 人类可读；null = 无延迟样本 */
export function ms(v: number | null | undefined): string {
  if (v == null || Number.isNaN(v)) return '—'
  if (v < 1000) return `${Math.round(v)} ms`
  return `${(v / 1000).toFixed(1)} s`
}

/** dropped 四桶合计 */
export function droppedTotal(d: ObservabilityDropped): number {
  return d.ownership + d.pii_mask + d.out_of_range + d.out_of_scope
}

/** dropped 明细一行文本：只列非零桶；全零 = '—' */
export function droppedSummary(d: ObservabilityDropped): string {
  const parts = (Object.keys(DROPPED_LABELS) as (keyof ObservabilityDropped)[])
    .filter((k) => d[k] > 0)
    .map((k) => `${DROPPED_LABELS[k]}×${d[k]}`)
  return parts.length ? parts.join(' · ') : '—'
}

/** 规则指纹短显（聚合表列宽有限；空 / 缺失 = '—'） */
export function shortFingerprint(fp: string | null | undefined): string {
  if (!fp) return '—'
  return fp.length > 12 ? `${fp.slice(0, 12)}…` : fp
}
