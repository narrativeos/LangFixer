<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { ElMessage } from 'element-plus'
import { summarizeRun } from '../lib/pipeline'
import { layerStatusLabel } from '../lib/labels'
import { useGecStore } from '../stores/gec'
import type { LayerReport } from '../types'
import ErrorList from './ErrorList.vue'
import Attribution from './Attribution.vue'
import LlmExplain from './LlmExplain.vue'

const store = useGecStore()
const view = ref<'errors' | 'attribution' | 'llm'>('errors')

/** 「本次管线」摘要（T1-3）：检测完成后展示各层实际执行情况（跑了什么 / 各层耗时）。
 *  数据来自后端 timings（缓存命中等未实际检测时不返回 → 不显示本行） */
const runLayers = computed(() => (store.timings ? summarizeRun(store.timings) : []))

/** 「检测路径」摘要：优先用后端层级贡献摘要（每层状态 + 检出数），
 *  无 layers（缓存命中 / 旧服务端）时回退 timings 管线行 */
const LAYER_LABELS: Record<string, string> = {
  rule: '规则',
  decision: '决策模型',
  knowledge: '知识召回',
  feature: '特征模型',
  llm: 'LLM',
  statistical: '统计模型',
}
function layerText(l: LayerReport): string {
  const base = LAYER_LABELS[l.layer] ?? l.layer
  if (l.status === 'ran') return l.found > 0 ? `${base} ${l.found} 条` : `${base} 未命中`
  if (l.status === 'decided') return l.found > 0 ? `${base} 定案 ${l.found} 条` : `${base} 定案（无错）`
  return `${base} ${layerStatusLabel(l.status)}`
}
function layerTagType(l: LayerReport): 'success' | 'warning' | 'info' {
  if (l.status === 'decided' || (l.status === 'ran' && l.found > 0)) return 'success'
  if (l.status === 'abstained' || l.status === 'unavailable') return 'warning'
  return 'info'
}
const runPath = computed(() => store.layers ?? [])

// 新结果到达时自动切到对应视图：仅在「检测完成」（resultsVersion 变化）时触发，
// 用户手动切换 tab 后不会被中途打断（旧实现 watch length，任何长度变化都会抢 tab）
watch(
  () => store.resultsVersion,
  () => {
    if (store.analyzed.length > 0) view.value = 'attribution'
    else if (store.errors.length > 0) view.value = 'errors'
  },
)
// 预览高亮联动：点击中列高亮片段 → 切回「检测结果」标签（ErrorList 负责滚动定位）
watch(
  () => store.focusErrorNonce,
  (n) => {
    if (n > 0) view.value = 'errors'
  },
)
// 逐错误讲解：归因卡片「讲解」按钮 → 切到 LLM tab（LlmExplain 监听 nonce 触发生成）
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
    ElMessage.success('修正后全文已复制到剪贴板')
  } catch {
    ElMessage.warning('复制失败：请手动选择文本复制（浏览器权限限制）')
  }
}
function applyCorrected() {
  store.text = store.correctedText
  store.clearResults()
  ElMessage.success('已替换为修正文本（全部修正已应用；如需回退可用 Ctrl/Cmd+Z）')
}
</script>

<template>
  <el-card shadow="never" class="results-card">
    <el-alert
      v-if="store.resultsStale && (store.errors.length > 0 || store.analyzed.length > 0)"
      type="warning"
      :closable="false"
      class="stale-hint"
      show-icon
      title="文本或语言已变更，以下结果基于修改前的检测"
    >
      <el-button size="small" type="primary" plain :disabled="store.loading" @click="store.analyze(store.sessionId || undefined)">
        重新检测
      </el-button>
    </el-alert>
    <div v-if="store.hasCorrections && !store.resultsStale" class="fix-actions" aria-label="修正出口">
      <el-button size="small" type="primary" plain @click="copyCorrected">复制修正全文</el-button>
      <el-button size="small" plain @click="applyCorrected">替换为修正文本</el-button>
      <span class="fix-hint">应用全部「建议修正」后的完整文本</span>
    </div>
    <el-alert
      v-if="store.detectCompleted && store.fromHistory"
      type="info"
      :closable="false"
      class="from-history-hint"
      show-icon
      title="本次结果来自历史复用（同文本 + 参数一致 + 无 LLM），未重新执行检测"
    />
    <div v-if="runPath.length" class="run-pipeline" aria-label="本次检测层级贡献摘要">
      <span class="rp-label">检测路径</span>
      <template v-for="(l, i) in runPath" :key="l.layer">
        <span v-if="i > 0" class="rp-arrow">→</span>
        <el-tooltip
          :content="`层：${LAYER_LABELS[l.layer] ?? l.layer}，状态：${layerStatusLabel(l.status)}，检出 ${l.found} 条`"
          placement="top"
        >
          <el-tag size="small" effect="plain" :type="layerTagType(l)">{{ layerText(l) }}</el-tag>
        </el-tooltip>
      </template>
    </div>
    <div v-else-if="runLayers.length" class="run-pipeline" aria-label="本次检测管线执行情况">
      <span class="rp-label">本次管线</span>
      <el-tag
        v-for="l in runLayers"
        :key="l.no"
        size="small"
        effect="plain"
        :type="l.no === 'Σ' ? '' : l.ran ? 'success' : 'info'"
      >
        {{ l.label }}
      </el-tag>
    </div>
    <el-tabs v-model="view">
      <el-tab-pane name="errors">
        <template #label>
          <span class="tab-label">
            检测结果
            <el-tag v-if="store.errors.length > 0" size="small" round class="tab-count">
              {{ store.errors.length }}
            </el-tag>
          </span>
        </template>
        <ErrorList />
      </el-tab-pane>
      <el-tab-pane name="attribution">
        <template #label>
          <span class="tab-label">
            错误归因
            <el-tag v-if="store.analyzed.length > 0" size="small" round class="tab-count">
              {{ store.analyzed.length }}
            </el-tag>
          </span>
        </template>
        <Attribution />
      </el-tab-pane>
      <el-tab-pane label="LLM 个性化讲解" name="llm">
        <LlmExplain />
      </el-tab-pane>
    </el-tabs>
  </el-card>
</template>

<style scoped>
.tab-label { display: inline-flex; align-items: center; gap: 6px; }
/* 结果过期提示 + 修正出口（复制 / 替换） */
.stale-hint { margin-bottom: 8px; }
.fix-actions {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 8px 12px 0;
}
.fix-hint { font-size: 12px; color: var(--el-text-color-secondary); }
/* 「本次管线」摘要行：各层实际执行情况（跑了什么 / 各层耗时） */
.run-pipeline {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 6px;
  padding: 8px 12px 2px;
}
.rp-label {
  font-size: 12px;
  color: var(--el-text-color-secondary);
  margin-right: 2px;
}
.rp-arrow { color: var(--el-text-color-placeholder); font-size: 12px; }
</style>