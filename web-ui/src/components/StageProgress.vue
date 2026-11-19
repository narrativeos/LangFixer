<script setup lang="ts">
/**
 * 分阶段检测进度条（header 与工作台 main 之间）。
 *
 * 数据面 = store.progress：beginDetect 起新 id，Web 端按 180ms 轮询
 * GET /gec/detect/progress、桌面端经 detect:stage 事件累加（均收敛到
 * pipeline.applyStageEvents 纯函数），这里只做渲染与计时展示：
 * - 并行组（①规则 / ②决策 / ③-F特征）开局即 running，串行位按推进位点亮；
 * - running 阶段显示已用时（100ms ticker 重绘），落定阶段显示该阶段耗时；
 * - 进度条填充 = 已落定阶段数 / 计划阶段数（End / 完成 = 100%）；
 * - 缓存 / 历史命中无阶段事件 → 「瞬时返回」；未收到 End 即中断 → 「已中断」；
 * - 完成（或中断）后常驻展示各阶段耗时，直到下一次检测开始重建。
 */
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import { useGecStore } from '../stores/gec'
import type { StageView } from '../lib/pipeline'

const store = useGecStore()

const p = computed(() => store.progress)
const running = computed(() => p.value.active)
const visible = computed(() => p.value.active || p.value.finished)

/** 100ms 心跳仅在检测进行中运行：驱动 running 阶段的「已用时」重绘 */
const now = ref(Date.now())
let ticker: number | undefined
function setTicker(on: boolean) {
  if (on && ticker === undefined) {
    now.value = Date.now()
    ticker = window.setInterval(() => {
      now.value = Date.now()
    }, 100)
  } else if (!on && ticker !== undefined) {
    clearInterval(ticker)
    ticker = undefined
  }
}
watch(running, setTicker, { immediate: true })

onBeforeUnmount(() => {
  setTicker(false)
})

const settledCount = computed(
  () =>
    p.value.stages.filter((s) => s.status === 'done' || s.status === 'unavailable' || s.status === 'skipped')
      .length,
)

const percent = computed(() => {
  if (p.value.instant || p.value.finished) return 100
  const n = p.value.stages.length
  // 刚发出请求、Start 事件尚未到达：细条示「进行中」，避免空条像卡死
  if (!n) return 6
  return Math.round((settledCount.value / n) * 100)
})

/** 右侧耗时读数：进行中实时已用；结束后总量（中断按已用时收尾） */
const totalText = computed(() => {
  const q = p.value
  if (q.instant) return ''
  if (!q.active) return `${q.canceled ? '已中断' : '共'} ${q.totalMs ?? 0}ms`
  return `已用 ${Math.max(0, now.value - q.startedAt)}ms`
})

function chipSuffix(s: StageView): string {
  switch (s.status) {
    case 'running':
      return '…'
    case 'done':
      return s.ms != null ? `${s.ms}ms` : '✓'
    case 'skipped':
      return '已跳过'
    case 'unavailable':
      return '✕ 不可用'
    case 'canceled':
      return '已中断'
    default:
      return '等待'
  }
}

const stateText = computed(() => {
  const q = p.value
  if (q.instant) return '瞬时返回（缓存 / 历史）'
  if (q.canceled) return '检测已中断'
  if (!q.active) return '检测完成'
  if (!q.stages.length) return '检测启动中…'
  return ''
})
</script>

<template>
  <div
    v-if="visible"
    class="sp"
    :class="{ 'sp-running': running, 'sp-finished': !running && !p.canceled, 'sp-canceled': p.canceled }"
    role="status"
    aria-label="检测进度"
  >
    <div class="sp-track" :aria-valuenow="percent" aria-valuemin="0" aria-valuemax="100" role="progressbar">
      <div class="sp-fill" :style="{ width: percent + '%' }"></div>
    </div>
    <div class="sp-chips">
      <span
        v-for="s in p.stages"
        :key="s.key"
        class="sp-chip"
        :class="'sp-' + s.status"
        :title="`${s.no} ${s.label}：${chipSuffix(s)}`"
      >
        <b class="sp-no">{{ s.no }}</b>
        <span class="sp-label">{{ s.label }}</span>
        <span class="sp-ms">{{ chipSuffix(s) }}</span>
      </span>
      <span v-if="stateText" class="sp-note">{{ stateText }}</span>
      <span class="sp-total">{{ totalText }}</span>
    </div>
  </div>
</template>

<style scoped>
.sp {
  display: flex;
  flex-direction: column;
  gap: 2px;
  padding: 3px var(--space-3) 5px;
  border-bottom: 1px solid var(--border);
  background: var(--surface-2);
  font-size: var(--fs-sm);
  color: var(--text-2);
}
.sp-track {
  height: 3px;
  border-radius: 2px;
  background: var(--border);
  overflow: hidden;
}
.sp-fill {
  height: 100%;
  border-radius: 2px;
  background: var(--primary, #4d6bfe);
  transition: width 0.2s ease;
}
.sp-finished .sp-fill {
  background: var(--ok);
}
.sp-canceled .sp-fill {
  background: var(--muted);
}
.sp-chips {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  flex-wrap: wrap;
  min-height: 18px;
}
.sp-chip {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 0 6px;
  border-radius: 9px;
  border: 1px solid var(--border);
  background: var(--bg);
  white-space: nowrap;
  line-height: 16px;
}
.sp-no {
  font-weight: 600;
  color: var(--muted);
}
.sp-chip.sp-running {
  border-color: var(--primary, #4d6bfe);
}
.sp-chip.sp-done .sp-ms {
  color: var(--ok);
}
.sp-chip.sp-running .sp-ms,
.sp-chip.sp-pending .sp-ms {
  color: var(--muted);
}
.sp-chip.sp-skipped,
.sp-chip.sp-unavailable,
.sp-chip.sp-canceled {
  opacity: 0.75;
}
.sp-chip.sp-skipped .sp-ms,
.sp-chip.sp-unavailable .sp-ms,
.sp-chip.sp-canceled .sp-ms {
  color: var(--muted);
}
.sp-note {
  color: var(--muted);
}
.sp-total {
  margin-left: auto;
  color: var(--muted);
  font-variant-numeric: tabular-nums;
}
</style>
