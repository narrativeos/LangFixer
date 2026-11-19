<script setup lang="ts">
/**
 * §7 可观测性看板（DETECT_PIPELINE.md）
 *
 * 数据源 = request_summary 账本（每次 detect / analyze 一行）。两路实时告警
 * （①完整性：所有权空洞 / dropped 非零；②质量：兜底率 / 零检出的 partial 模式）
 * + day × 规则指纹聚合表，回答「最近这批改动把管线指标动没动」。
 *
 * 纯只读；运营台（ops）门控随宿主 EvolutionCenter。桌面端不写 summary，
 * Web 形态下账本为空时表格显示空态。
 */
import { computed, onMounted, ref, watch } from 'vue'
import { LButton, LDot, LEmpty, LSection, LSegmented, LTable, LTag } from '../ui'
import type { LColumn } from '../ui'
import { toast } from '../ui/toast'
import { errMsg } from '../lib/err'
import { api } from '../api/client'
import {
  droppedSummary,
  droppedTotal,
  ms,
  rate,
  shortFingerprint,
} from '../lib/observability'
import type { ObservabilityAlerts, ObservabilityDayRow } from '../types'

const days = ref(14)
const DAYS_OPTIONS: { value: number; label: string }[] = [
  { value: 7, label: '7 天' },
  { value: 14, label: '14 天' },
  { value: 30, label: '30 天' },
  { value: 90, label: '90 天' },
]

const alerts = ref<ObservabilityAlerts | null>(null)
const rows = ref<ObservabilityDayRow[]>([])
const loading = ref(false)
const loaded = ref(false)

async function refresh() {
  loading.value = true
  try {
    const [a, r] = await Promise.all([
      api.observabilityAlerts(),
      api.observabilitySummary(days.value),
    ])
    alerts.value = a
    rows.value = r
    loaded.value = true
  } catch (e) {
    toast(`观测数据读取失败：${errMsg(e)}`, 'danger')
  } finally {
    loading.value = false
  }
}
onMounted(refresh)
// 窗口切换即重拉（聚合窗口跟着 days 走）
watch(days, refresh)

const integrityOk = computed(() => !alerts.value?.integrity.triggered)
const qualityOk = computed(() => !alerts.value?.quality.triggered)

/** 窗口内 dropped 总量（告警是 24h 口径，这里给所选窗口的账本合计） */
const windowDropped = computed(() =>
  rows.value.reduce((s, r) => s + droppedTotal(r.dropped), 0),
)

const cols: LColumn<ObservabilityDayRow>[] = [
  { key: 'day', label: '日期' },
  { key: 'fp', label: '规则指纹' },
  { key: 'requests', label: '请求', align: 'right' },
  { key: 'coverage', label: '覆盖率', align: 'right' },
  { key: 'sentences', label: '句复查', align: 'right' },
  { key: 'unavailable', label: 'LLM 不可用', align: 'right' },
  { key: 'cache', label: '缓存命中', align: 'right' },
  { key: 'history', label: '历史复用', align: 'right' },
  { key: 'dropped', label: 'dropped' },
  { key: 'llm', label: 'LLM 块', align: 'right' },
  { key: 'p95', label: 'P95', align: 'right' },
]
</script>

<template>
  <div class="obs-panel">
    <div class="obs-toolbar">
      <LSegmented v-model="days" :options="DAYS_OPTIONS" />
      <LButton size="sm" :loading="loading" @click="refresh">刷新</LButton>
      <span class="hint obs-window-total">
        窗口内 dropped 合计 {{ windowDropped }}
      </span>
    </div>

    <div class="alert-grid">
      <LSection title="完整性告警（24h）" desc="所有权空洞 / dropped 账本非零的请求行">
        <template #actions>
          <LTag :tone="alerts ? (integrityOk ? 'ok' : 'danger') : 'neutral'">
            {{ alerts ? (integrityOk ? '平静' : '告警') : '—' }}
          </LTag>
        </template>
        <div class="metric-row">
          <div class="metric">
            <span class="k">有丢弃的请求行</span>
            <span class="v">{{ alerts?.integrity.lossy_rows ?? '—' }}</span>
          </div>
        </div>
        <p class="hint">
          检出被掩码 / 所有权 / 越界规则丢弃时逐桶记账；非零即点名（同时服务端打
          error 日志）。持续非零说明管线在静默吞检出。
        </p>
      </LSection>

      <LSection title="质量告警（24h）" desc="兜底率与「partial 却零检出」模式">
        <template #actions>
          <LTag :tone="alerts ? (qualityOk ? 'ok' : 'warn') : 'neutral'">
            {{ alerts ? (qualityOk ? '平静' : '告警') : '—' }}
          </LTag>
        </template>
        <div class="metric-row">
          <div class="metric">
            <span class="k">兜底率（grammar_novice）</span>
            <span class="v">{{ rate(alerts?.quality.fallback_ratio) }}</span>
          </div>
          <div class="metric">
            <span class="k">兜底 / 归因行</span>
            <span class="v">
              {{ alerts?.quality.fallback_sum ?? '—' }} / {{ alerts?.quality.error_sum ?? '—' }}
            </span>
          </div>
          <div class="metric">
            <span class="k">partial ∧ 零检出</span>
            <span class="v">{{ alerts?.quality.partial_empty_runs ?? '—' }} 次</span>
          </div>
        </div>
        <p class="hint">
          归因落到 grammar_novice 兜底规则的比例超 30%（样本 ≥20），或「覆盖率不满却
          零检出」出现 ≥5 次 → 规则 / 分块可能出了系统性问题。
        </p>
      </LSection>
    </div>

    <LSection :title="`day × 规则指纹聚合（近 ${days} 天）`" desc="按规则指纹分桶：改规则前后的指标对比看这里">
      <LTable v-if="rows.length" :columns="cols" :rows="rows" compact>
        <template #col-fp="{ row }">
          <span class="mono" :title="row.rules_fingerprint">{{ shortFingerprint(row.rules_fingerprint) }}</span>
        </template>
        <template #col-coverage="{ row }">{{ rate(row.coverage_avg) }}</template>
        <template #col-sentences="{ row }">{{ rate(row.sentences_reviewed_rate) }}</template>
        <template #col-unavailable="{ row }">{{ rate(row.unavailable_rate) }}</template>
        <template #col-cache="{ row }">{{ rate(row.cache_hit_rate) }}</template>
        <template #col-history="{ row }">{{ rate(row.history_reuse_rate) }}</template>
        <template #col-dropped="{ row }">
          <span :class="droppedTotal(row.dropped) > 0 ? 'obs-lossy' : 'hint'" :title="droppedSummary(row.dropped)">
            {{ droppedTotal(row.dropped) || '—' }}
          </span>
        </template>
        <template #col-llm="{ row }">
          {{ row.llm_calls }}<span class="hint"> / {{ row.chunks_total }}</span>
        </template>
        <template #col-p95="{ row }">{{ ms(row.p95_total_ms) }}</template>
      </LTable>
      <LEmpty v-else-if="loaded" text="窗口内还没有请求记录" />
      <div v-else class="hint">加载中…</div>
      <div class="hint sec-gap-top">
        <LDot tone="muted" />
        LLM 块 = 段级完成块数（LLM 出站调用代理指标）；覆盖率 / 句复查为该行均值或比值；
        P95 取当日总耗时最近秩。
      </div>
    </LSection>
  </div>
</template>

<style scoped>
.obs-toolbar {
  display: flex;
  align-items: center;
  gap: var(--space-3);
  margin-bottom: var(--space-3);
}
.obs-window-total {
  margin-left: auto;
}
.alert-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(320px, 1fr));
  gap: var(--space-3);
  margin-bottom: var(--space-3);
}
.metric-row {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-4);
  margin-bottom: var(--space-2);
}
.metric {
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.metric .k {
  font-size: var(--fs-xs);
  color: var(--muted);
}
.metric .v {
  font-size: var(--fs-lg);
  font-variant-numeric: tabular-nums;
}
.mono {
  font-family: var(--mono, ui-monospace, monospace);
  font-size: var(--fs-xs);
}
.obs-lossy {
  color: var(--warn);
  font-variant-numeric: tabular-nums;
}
.sec-gap-top {
  margin-top: var(--space-2);
}
</style>
