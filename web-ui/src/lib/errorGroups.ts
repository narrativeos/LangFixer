/**
 * 检测结果分组（纯函数，可单测）
 *
 * 从 ErrorList.vue 拆出：分类汇总、同类合并、逐条平铺。
 * 保留原始下标 i 作各反馈状态 key —— 过滤 / 合并只影响展示，不影响状态映射。
 */
import type { ErrorItem } from '../types'

export interface MergedItem {
  i: number
  e: ErrorItem
}
export interface MergedGroup {
  key: string
  first: MergedItem
  items: MergedItem[]
}

/** 分类计数（汇总条）：category 缺省归入 other */
export function summarizeCategories(errors: ErrorItem[]): Record<string, number> {
  const m: Record<string, number> = {}
  for (const e of errors) {
    const k = e.category || 'other'
    m[k] = (m[k] ?? 0) + 1
  }
  return m
}

/** 同类合并：相同 error_type + original + correction 的实例并成一组（×N 计数）；
 *  组顺序 = 首个实例出现顺序 */
export function buildMergedGroups(items: MergedItem[]): MergedGroup[] {
  const map = new Map<string, MergedGroup>()
  for (const { i, e } of items) {
    const key = `${e.error_type}\u0000${e.original}\u0000${e.correction ?? ''}`
    let g = map.get(key)
    if (!g) {
      g = { key, first: { i, e }, items: [] }
      map.set(key, g)
    }
    g.items.push({ i, e })
  }
  return [...map.values()]
}

/** 逐条平铺（合并开关关闭）：每条独立成组，模板对 N=1 组渲染逐条卡片 */
export function buildSoloGroups(items: MergedItem[]): MergedGroup[] {
  return items.map(({ i, e }) => ({ key: `solo:${i}`, first: { i, e }, items: [{ i, e }] }))
}
