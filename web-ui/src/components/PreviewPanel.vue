<script setup lang="ts">
import { computed } from 'vue'
import { useGecStore } from '../stores/gec'
import type { ErrorItem } from '../types'

const store = useGecStore()

/** 预览片段：纯文本 或 带高亮标记的片段（对应 store.errors 下标） */
interface Segment {
  text: string
  mark?: { index: number; category: string; correction: string | null }
}

/** 将待检测文本切分为「普通文本 / 高亮片段」序列。
 *  优先使用后端返回的 span 定位；若检测后文本被修改导致 span 失配，回退为按 original 子串查找。
 *  结果过期（resultsStale）时禁用子串回退：indexOf 会把高亮打到错误的位置，只信任 span 精确匹配。 */
const segments = computed<Segment[]>(() => {
  const text = store.text
  if (!text) return []
  if (store.errors.length === 0) return [{ text }]

  const stale = store.resultsStale
  const marks: { start: number; end: number; index: number; category: string; correction: string | null }[] = []
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
    marks.push({ start, end, index: i, category: e.category || 'other', correction: e.correction })
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
</script>

<template>
  <el-card shadow="never" class="preview-card">
    <template #header>
      <div class="card-header">
        <span class="card-title">内容预览</span>
        <span class="sub">检测后高亮相关内容，点击片段定位右侧错误</span>
        <span class="spacer"></span>
        <el-tag v-if="store.resultsStale" type="warning" size="small" round>结果已过期</el-tag>
        <el-tag v-if="hasMarks" type="danger" size="small" round>
          {{ store.errors.length }} 处错误
        </el-tag>
      </div>
    </template>

    <div v-if="!store.text" class="preview-empty">
      <el-empty description="在左侧输入或粘贴待检测文本，此处实时预览" :image-size="72" />
    </div>
    <template v-else>
      <div v-if="hasMarks" class="preview-legend">
        <span class="lg"><i class="sw sw-grammar"></i>语法</span>
        <span class="lg"><i class="sw sw-spelling"></i>拼写</span>
        <span class="lg"><i class="sw sw-typography"></i>标点</span>
        <span class="lg"><i class="sw sw-style"></i>风格</span>
      </div>
      <div class="preview-text">
        <template v-for="(seg, i) in segments" :key="i">
          <mark
            v-if="seg.mark"
            :class="['hl', 'hl-' + seg.mark.category, { stale: store.resultsStale }]"
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
  </el-card>
</template>

<style scoped>
/* 结果过期：高亮降饱和 + 虚线下划线，提示标记基于修改前文本 */
.preview-text mark.stale {
  background: var(--surface-2);
  color: var(--text-2);
  box-shadow: inset 0 -2px 0 0 var(--border-strong);
}
</style>