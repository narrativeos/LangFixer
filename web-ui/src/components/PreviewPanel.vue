<script setup lang="ts">
import { computed } from 'vue'
import { useGecStore } from '../stores/gec'
import { LEmpty } from '../ui'
import type { ErrorItem } from '../types'

const store = useGecStore()

/** 预览片段：纯文本 或 带高亮标记的片段（对应 store.errors 下标） */
interface Segment {
  text: string
  mark?: { index: number; category: string; confidence: number; correction: string | null }
}

/** 将待检测文本切分为「普通文本 / 高亮片段」序列。
 *  优先使用后端返回的 span 定位；若检测后文本被修改导致 span 失配，回退为按 original 子串查找。
 *  结果过期（resultsStale）时禁用子串回退：indexOf 会把高亮打到错误的位置，只信任 span 精确匹配。 */
const segments = computed<Segment[]>(() => {
  const text = store.text
  if (!text) return []
  if (store.errors.length === 0) return [{ text }]

  const stale = store.resultsStale
  const marks: { start: number; end: number; index: number; category: string; confidence: number; correction: string | null }[] = []
  store.errors.forEach((e: ErrorItem, i: number) => {
    let start = e.span?.[0] ?? -1
    let end = e.span?.[1] ?? -1
    if (start < 0 || end <= start || text.slice(start, end) !== e.original) {
      if (stale) return // 过期结果：span 失配的标记直接放弃，避免错位高亮
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
      correction: e.correction,
    })
  })
  if (marks.length === 0) return [{ text }]

  marks.sort((a, b) => a.start - b.start)
  const out: Segment[] = []
  let cursor = 0
  for (const m of marks) {
    if (m.start < cursor) continue // 重叠区间：保留先出现的标记
    if (m.start > cursor) out.push({ text: text.slice(cursor, m.start) })
    out.push({ text: text.slice(m.start, m.end), mark: m })
    cursor = m.end
  }
  if (cursor < text.length) out.push({ text: text.slice(cursor) })
  return out
})

const hasMarks = computed(() => store.errors.length > 0)

/** 类型定线型由 cat-* 类给出;置信定浓度(ui.css m-low/m-mid) */
function confClass(c: number): string {
  if (c < 0.5) return 'm-low'
  if (c < 0.8) return 'm-mid'
  return ''
}

const legend: { cat: string; label: string }[] = [
  { cat: 'spelling', label: '拼写' },
  { cat: 'grammar', label: '语法' },
  { cat: 'punctuation', label: '标点' },
  { cat: 'style', label: '风格' },
]
</script>

<template>
  <div class="preview-panel">
    <div v-if="!store.text" class="preview-empty">
      <LEmpty text="还没有文本" hint="切到「编辑」输入或粘贴待检测文本，这里会展示带标记的校样。" />
    </div>
    <template v-else>
      <div class="preview-legend" aria-hidden="false">
        <span v-for="l in legend" :key="l.cat" class="lg">
          <mark class="l-canvas-text" :class="`cat-${l.cat}`">{{ l.label }}</mark>
        </span>
        <span v-if="hasMarks" class="lg-count">{{ store.errors.length }} 处错误</span>
      </div>
      <div class="l-canvas-text preview-text" :class="{ 'is-stale': store.resultsStale }">
        <template v-for="(seg, i) in segments" :key="i">
          <mark
            v-if="seg.mark"
            :class="[
              `cat-${seg.mark.category}`,
              confClass(seg.mark.confidence),
              { 'is-stale': store.resultsStale },
            ]"
            :title="seg.mark.correction ? '建议修正：' + seg.mark.correction : '错误位置'"
            role="button"
            tabindex="0"
            @click="store.focusError(seg.mark!.index)"
            @keydown.enter="store.focusError(seg.mark!.index)"
          >{{ seg.text }}</mark>
          <template v-else>{{ seg.text }}</template>
        </template>
      </div>
    </template>
  </div>
</template>

<style scoped>
.preview-panel {
  display: flex;
  flex-direction: column;
  gap: var(--space-3);
  min-height: 0;
  flex: 1;
}
.preview-legend {
  display: flex;
  align-items: center;
  gap: var(--space-4);
  font-size: var(--fs-sm);
  color: var(--muted);
}
.lg {
  display: inline-flex;
  align-items: center;
}
.lg-count {
  margin-left: auto;
  color: var(--danger);
  font-weight: 600;
}
.preview-text {
  padding: var(--space-2) 0;
  min-height: 40vh;
}
html.desktop .preview-text {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  overscroll-behavior: contain;
}
/* 结果过期：标记整体降浓度,提示基于修改前文本 */
.preview-text mark.is-stale {
  opacity: 0.45;
  text-decoration-style: dotted;
}
</style>
