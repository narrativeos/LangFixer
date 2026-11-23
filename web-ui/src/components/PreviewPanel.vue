<script setup lang="ts">
import { computed, nextTick, ref, watch } from 'vue'
import { useGecStore } from '../stores/gec'
import { buildSegments } from '../lib/highlight'
import { LEmpty } from '../ui'

const store = useGecStore()

/** 将待检测文本切分为「普通文本 / 高亮片段」序列。
 *  定位与回退规则见 lib/highlight（span 字节→UTF-16 换算、失配回退、过期放弃）；
 *  与编辑态 backdrop 镜像层共用同一实现，两态高亮位置必然一致。 */
const segments = computed(() => buildSegments(store.text, store.errors, store.resultsStale))

const hasMarks = computed(() => store.errors.length > 0)

/** 类型定线型由 cat-* 类给出;置信定浓度(ui.css m-low/m-mid) */
function confClass(c: number): string {
  if (c < 0.5) return 'm-low'
  if (c < 0.8) return 'm-mid'
  return ''
}

// 列表→原文正向联动：点检测结果卡片 → 焦点 mark 短暂强调并滚动居中
const previewRef = ref<HTMLElement | null>(null)
const focusedIdx = ref(-1)
let focusTimer: ReturnType<typeof setTimeout> | null = null
watch(
  () => store.focusErrorNonce,
  (n) => {
    if (n === 0) return
    const idx = store.focusErrorIndex
    nextTick(() => {
      // 标签页内容可能刚挂载（v-show 切换），再等一帧确保 DOM 就绪
      requestAnimationFrame(() => {
        const el = previewRef.value?.querySelector<HTMLElement>(`[data-idx="${idx}"]`)
        if (!el) return
        el.scrollIntoView({ behavior: 'smooth', block: 'center' })
        focusedIdx.value = idx
        if (focusTimer) clearTimeout(focusTimer)
        focusTimer = setTimeout(() => {
          if (focusedIdx.value === idx) focusedIdx.value = -1
        }, 1600)
      })
    })
  },
)

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
      <div ref="previewRef" class="l-canvas-text preview-text" :class="{ 'is-stale': store.resultsStale }">
        <template v-for="(seg, i) in segments" :key="i">
          <mark
            v-if="seg.mark"
            :class="[
              `cat-${seg.mark.category}`,
              confClass(seg.mark.confidence),
              { 'is-stale': store.resultsStale, 'is-active': focusedIdx === seg.mark.index },
            ]"
            :data-idx="seg.mark.index"
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
/* 列表→原文联动：焦点 mark 短暂强调（is-active 底色见 ui.css，这里叠加呼吸闪烁） */
.preview-text mark.is-active {
  animation: mark-focus-flash 1.6s ease;
}
@keyframes mark-focus-flash {
  0%, 55% { box-shadow: 0 0 0 2px color-mix(in srgb, var(--accent) 45%, transparent); }
  100% { box-shadow: 0 0 0 2px transparent; }
}
</style>
