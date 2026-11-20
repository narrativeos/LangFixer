<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { summarizeRun } from '../lib/pipeline'
import { layerStatusLabel, plainLayerLabel } from '../lib/labels'
import { isVerdictOnly } from '../lib/errorDisplay'
import { useGecStore } from '../stores/gec'
import { toast, LButton, LSegmented } from '../ui'
import type { LayerReport } from '../types'
import ErrorList from './ErrorList.vue'
import Attribution from './Attribution.vue'
import LlmExplain from './LlmExplain.vue'

const store = useGecStore()
type PanelView = 'errors' | 'attribution' | 'llm'
const view = ref<PanelView>('errors')

/** 三个页签:标签带计数,数量即工作台的注意力信号 */
const viewOptions = computed(() => [
  { value: 'errors' as const, label: store.errors.length ? `检测结果 ${store.errors.length}` : '检测结果' },
  { value: 'attribution' as const, label: attributableCount.value ? `错误归因 ${attributableCount.value}` : '错误归因' },
  { value: 'llm' as const, label: 'AI 讲解' },
])

/** 句级怀疑（决策模型待复查项）不进归因面（服务端已 skip，此处兜底旧服务端数据），
 *  页签计数与自动切换都按可归因数计 */
const attributableCount = computed(() => store.analyzed.filter((a) => !isVerdictOnly(a.error)).length)

/** 「本次管线」摘要（T1-3）：检测完成后展示各层实际执行情况（跑了什么 / 各层耗时）。
 *  数据来自后端 timings（缓存命中等未实际检测时不返回 → 不显示本行） */
const runLayers = computed(() => (store.timings ? summarizeRun(store.timings) : []))

/** 「检测路径」摘要：优先用后端层级贡献摘要（每层状态 + 检出数），
 *  无 layers（缓存命中 / 旧服务端）时回退 timings 管线行。
 *  展示名走统一人话词表（plainLayerLabel），内部键名只进 tooltip;层级标签一律中性,红只留给真实错误 */
function layerText(l: LayerReport): string {
  const base = plainLayerLabel(l.layer)
  if (l.status === 'ran') return l.found > 0 ? `${base} ${l.found} 条` : `${base} 未命中`
  if (l.status === 'decided') return l.found > 0 ? `${base} 定案 ${l.found} 条` : `${base} 定案（无错）`
  return `${base} ${layerStatusLabel(l.status)}`
}
const runPath = computed(() => store.layers ?? [])

/** 降级原因人话化：后端编码（"{layer}_unavailable" 等）→ 中文文案 */
function qualityReasonText(r: string): string {
  if (r.endsWith('_unavailable')) return `${plainLayerLabel(r.slice(0, -'_unavailable'.length))}不可用`
  if (r.endsWith('_partial')) return `${plainLayerLabel(r.slice(0, -'_partial'.length))}部分执行`
  if (r.startsWith('coverage:')) return `复查覆盖 ${r.slice(9)}`
  if (r.startsWith('unowned_chars:')) return `所有权空洞 ${r.slice(14)} 字节`
  return r
}

/** 质量徽标：quality ≠ full 时展示（降级不得伪装成正常结果）；
 *  full / 回放（quality=null）/ 旧服务端不显示；悬浮提示降级原因与缓存资格 */
const qualityBadge = computed(() => {
  const q = store.quality
  if (!q || q.status === 'full') return null
  const label = q.status === 'degraded' ? '降级运行' : '部分完成'
  const reasons = (q.degraded_reasons ?? []).map(qualityReasonText)
  const parts: string[] = []
  if (reasons.length) parts.push(`原因：${reasons.join('；')}`)
  if (q.coverage !== undefined && q.coverage < 1) {
    parts.push(`已复查 ${Math.round(q.coverage * 100)}%`)
  }
  if (q.unowned_chars > 0) parts.push(`所有权空洞 ${q.unowned_chars} 字节（完整性异常）`)
  parts.push('本次结果不会写入缓存 / 历史复用')
  return { label, degraded: q.status === 'degraded', title: parts.join('。') }
})

// 新结果到达时自动切到对应视图：仅在「检测完成」（resultsVersion 变化）时触发，
// 用户手动切换页签后不会被中途打断（旧实现 watch length，任何长度变化都会抢 tab）
watch(
  () => store.resultsVersion,
  () => {
    if (attributableCount.value > 0) view.value = 'attribution'
    else if (store.errors.length > 0) view.value = 'errors'
  },
)
// 预览高亮联动：点击校样高亮片段 → 切回「检测结果」（ErrorList 负责滚动定位）
watch(
  () => store.focusErrorNonce,
  (n) => {
    if (n > 0) view.value = 'errors'
  },
)
// 逐错误讲解：归因卡片「讲解」按钮 → 切到 AI 讲解（LlmExplain 监听 nonce 触发生成）
watch(
  () => store.explainNonce,
  (n) => {
    if (n > 0) view.value = 'llm'
  },
)

/** 修正出口：复制 / 替换（applyCorrections 与采纳沉淀、服务端同构，见 lib/correct.ts） */
async function copyCorrected() {
  const text = store.correctedText
  try {
    await navigator.clipboard.writeText(text)
    toast('修正后全文已复制到剪贴板', 'ok')
  } catch {
    toast('复制失败：请手动选择文本复制（浏览器权限限制）', 'warn')
  }
}
function applyCorrected() {
  store.applySnapshot = store.text
  store.text = store.correctedText
  store.clearResults()
  toast(
    '已替换为修正文本',
    'ok',
    {
      label: '撤销',
      onClick: () => {
        if (store.applySnapshot == null) return
        store.text = store.applySnapshot
        store.applySnapshot = null
      },
    },
    6000,
  )
}
</script>

<template>
  <div class="results-panel">
    <div
      v-if="store.resultsStale && (store.errors.length > 0 || store.analyzed.length > 0)"
      class="l-banner"
      role="status"
    >
      文本或语言已变更，以下结果基于修改前的检测
      <span class="l-spacer"></span>
      <LButton
        size="sm"
        variant="primary"
        :disabled="store.loading"
        @click="store.analyze(store.sessionId || undefined)"
      >
        重新检测
      </LButton>
    </div>
    <div v-if="store.hasCorrections && !store.resultsStale" class="fix-actions" aria-label="修正出口">
      <LButton size="sm" @click="copyCorrected">复制修正全文</LButton>
      <LButton size="sm" @click="applyCorrected">替换为修正文本</LButton>
      <span class="fix-hint">应用全部「建议修正」后的完整文本</span>
    </div>
    <p
      v-if="store.detectCompleted && store.fromHistory"
      class="from-history-hint"
      role="status"
    >
      本次结果来自历史复用（同文本 + 参数一致 + 无 LLM），未重新执行检测
      <LButton size="sm" :disabled="store.loading" @click="store.detect({ refresh: true })">
        忽略缓存重检
      </LButton>
    </p>
    <p
      v-if="store.detectCompleted && qualityBadge"
      class="quality-hint"
      :class="{ degraded: qualityBadge.degraded }"
      role="status"
      :title="qualityBadge.title"
    >
      本次检测结果为{{ qualityBadge.label }}，结论可能不完整
    </p>
    <div v-if="runPath.length" class="l-trace" aria-label="本次检测层级贡献摘要">
      <span class="rp-label">检测路径</span>
      <template v-for="(l, i) in runPath" :key="l.layer">
        <span v-if="i > 0" class="l-trace-arrow">→</span>
        <span
          class="l-trace-seg"
          :class="{ 'l-skip': l.status === 'skipped' || l.status === 'unavailable' }"
          :title="`层：${plainLayerLabel(l.layer)}（${l.layer}），状态：${layerStatusLabel(l.status)}，检出 ${l.found} 条`"
        >
          {{ layerText(l) }}
        </span>
      </template>
    </div>
    <div v-else-if="runLayers.length" class="l-trace" aria-label="本次检测管线执行情况">
      <span class="rp-label">检测方式</span>
      <span
        v-for="l in runLayers"
        :key="l.no"
        class="l-trace-seg"
        :class="{ 'l-skip': l.no !== 'Σ' && !l.ran }"
      >
        {{ l.label }}
      </span>
    </div>

    <LSegmented v-model="view" :options="viewOptions" />
    <!-- 包一层 div：ErrorList 多根节点（首节点可能是 v-if 的注释节点），
         v-show 直挂组件会触发「non-element root」警告且指令失效 -->
    <div v-show="view === 'errors'">
      <ErrorList />
    </div>
    <Attribution v-show="view === 'attribution'" />
    <LlmExplain v-show="view === 'llm'" />
  </div>
</template>

<style scoped>
.results-panel {
  display: flex;
  flex-direction: column;
  gap: var(--space-3);
}
/* 修正出口（复制 / 替换） */
.fix-actions {
  display: flex;
  align-items: center;
  gap: var(--space-2);
}
.fix-hint {
  font-size: var(--fs-sm);
  color: var(--muted);
}
.from-history-hint {
  margin: 0;
  font-size: var(--fs-sm);
  color: var(--muted);
  line-height: var(--lh-body);
}
.quality-hint {
  margin: 0;
  font-size: var(--fs-sm);
  color: var(--warn-ink);
  background: var(--warn-soft);
  border: 1px solid var(--warn-line);
  border-radius: var(--radius-sm);
  padding: var(--space-2) var(--space-3);
  line-height: var(--lh-body);
}
.quality-hint.degraded {
  color: var(--danger);
}
.rp-label {
  font-size: var(--fs-sm);
  color: var(--muted);
}
</style>
