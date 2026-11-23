/**
 * 原文高亮切段（纯函数，可单测）——「列表 ⇄ 原文」双向联动的唯一定位实现。
 *
 * 从 PreviewPanel.vue 拆出并修正口径：检测 span 是 UTF-8 字节偏移（lib/span 注释），
 * 必须经 byteToU16Map 转 UTF-16 索引再切片；直接按字节 slice 在 CJK 文本上必然
 * 失配，会误走 indexOf 回退（只命中首个出现位置）。
 *
 * 定位优先级：span 换算精确匹配 original → 采用；失配且结果未过期 → 按 original
 * 子串查找回退；结果已过期（resultsStale）→ 放弃该标记（错位高亮比没有更糟）。
 * 区间重叠：保留先出现的标记。校样态 <mark> 与编辑态 backdrop 镜像层共用本切段。
 */
import { byteToU16Map } from './span'

/** 参与切段的错误项最小字段面（store.errors / analyzed.error 均满足） */
export interface HighlightableError {
  span?: [number, number]
  original: string
  category?: string
  confidence?: number
  correction: string | null
}

/** 定位到 text 上某区间的错误标记（index = store.errors 下标，联动主键） */
export interface ErrorMark {
  start: number
  end: number
  index: number
  category: string
  confidence: number
  correction: string | null
}

export interface TextSegment {
  text: string
  mark?: ErrorMark
}

/** 将错误项定位到 text 上（UTF-16 索引），按 start 升序；无法定位的跳过 */
export function buildMarks(
  text: string,
  errors: HighlightableError[],
  stale: boolean,
): ErrorMark[] {
  const b2u = byteToU16Map(text)
  const marks: ErrorMark[] = []
  errors.forEach((e, i) => {
    let start = -1
    let end = -1
    const a = e.span ? b2u.get(e.span[0]) : undefined
    const b = e.span ? b2u.get(e.span[1]) : undefined
    if (a !== undefined && b !== undefined && a < b && text.slice(a, b) === e.original) {
      start = a
      end = b
    } else {
      if (stale) return // 过期结果：span 失配的标记放弃，indexOf 会打到错误位置
      const idx = e.original ? text.indexOf(e.original) : -1
      if (idx < 0) return
      start = idx
      end = idx + e.original.length
    }
    marks.push({
      start,
      end,
      index: i,
      category: e.category || 'other',
      confidence: e.confidence ?? 1,
      correction: e.correction ?? null,
    })
  })
  return marks.sort((x, y) => x.start - y.start)
}

/** 将 text 切为「普通片段 / 错误标记片段」序列（无错误时整段返回） */
export function buildSegments(
  text: string,
  errors: HighlightableError[],
  stale: boolean,
): TextSegment[] {
  if (!text) return []
  if (errors.length === 0) return [{ text }]
  const marks = buildMarks(text, errors, stale)
  if (marks.length === 0) return [{ text }]
  const out: TextSegment[] = []
  let cursor = 0
  for (const m of marks) {
    if (m.start < cursor) continue // 重叠区间：保留先出现的标记
    if (m.start > cursor) out.push({ text: text.slice(cursor, m.start) })
    out.push({ text: text.slice(m.start, m.end), mark: m })
    cursor = m.end
  }
  if (cursor < text.length) out.push({ text: text.slice(cursor) })
  return out
}
