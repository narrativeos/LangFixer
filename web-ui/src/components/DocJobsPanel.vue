<template>
  <div class="doc-jobs-panel">
    <LSection title="新建文档审校作业" desc="长文档（10 万字+）切块并行 LLM 深度复查：断点续跑 / 块缓存 / 风险优先">
      <textarea
        v-model="text"
        class="paste-area"
        rows="6"
        placeholder="粘贴整篇文档，提交后转为后台作业，可随时离开本页"
      ></textarea>
      <div class="toolbar">
        <span>字符数：{{ text.length }}</span>
        <span class="sep"></span>
        <span>预算</span>
        <LSelect v-model="budgetSel" :options="BUDGET_OPTIONS" class="budget-select" />
        <LPopover>
          <template #trigger="{ toggle }">
            <LButton size="sm" variant="ghost" aria-label="预算说明" @click="toggle">?</LButton>
          </template>
          经济模式跳过低风险块：更省 LLM 调用，但覆盖率与召回如实下降
        </LPopover>
        <span class="spacer"></span>
        <LButton
          variant="primary"
          :loading="creating"
          :disabled="text.trim().length === 0"
          @click="create"
        >
          创建审校作业
        </LButton>
      </div>
    </LSection>

    <LSection title="作业列表">
      <template #actions>
        <LButton size="sm" :loading="listing" @click="refreshList">刷新</LButton>
      </template>
      <LEmpty
        v-if="!jobs.length"
        text="暂无作业"
        hint="在上方粘贴文档并点「创建审校作业」，即可跑第一个长文档深度复查"
      />
      <LTable
        v-else
        :columns="jobCols"
        :rows="jobs"
        row-key="id"
        compact
        :selected="selectedId ? [selectedId] : []"
        @row-click="(row: DocJobListItem) => selectJob(row)"
      >
        <template #col-job="{ row }">
          <code>{{ row.id.slice(0, 8) }}</code>
        </template>
        <template #col-status="{ row }">
          <span class="status-cell"><LDot :tone="statusTone(row.status)" />{{ statusLabel(row.status) }}</span>
        </template>
        <template #col-progress="{ row }">
          <LProgress class="row-progress" :value="progressPct(row.progress)" />
        </template>
        <template #col-coverage="{ row }">{{ (row.coverage * 100).toFixed(0) }}%</template>
        <template #col-results="{ row }">
          <LButton size="sm" variant="ghost" @click.stop="openResults(row)">
            {{ row.id === selectedId && results ? results.errors.length : '查看' }}
          </LButton>
        </template>
        <template #col-actions="{ row }">
          <LButton
            v-if="row.status === 'running' || row.status === 'created'"
            size="sm"
            variant="ghost"
            @click.stop="cancel(row.id)"
          >
            取消
          </LButton>
          <LButton
            v-if="row.status === 'done' || row.status === 'cancelled'"
            size="sm"
            variant="ghost"
            @click.stop="retry(row.id)"
          >
            重跑失败块
          </LButton>
        </template>
      </LTable>
    </LSection>

    <LSection v-if="results" title="作业结果">
      <template #actions>
        <code class="mono">{{ selectedId?.slice(0, 8) }}</code>
        <LTag v-if="results.complete" tone="ok">全量复查完成</LTag>
        <LTag v-else tone="warn">结果不完整（覆盖率 {{ (results.coverage * 100).toFixed(0) }}%）</LTag>
        <LButton size="sm" @click="loadResults(selectedId)">刷新结果</LButton>
      </template>
      <div v-if="!results.complete" class="l-banner banner-warn" role="status">{{ incompleteReason }}</div>
      <LSegmented v-model="resultTab" :options="resultTabOptions" />
      <template v-if="resultTab === 'errors'">
        <LEmpty v-if="!results.errors.length" text="未检出错误" hint="该作业复查范围内没有问题，可在「块状态」核对覆盖率" />
        <LTable v-else :columns="errCols" :rows="results.errors" compact>
          <template #col-pos="{ row }">{{ row.span[0] }}–{{ row.span[1] }}</template>
          <template #col-suggest="{ row }">{{ row.suggestions?.[0]?.text ?? row.correction }}</template>
          <template #col-type="{ row }">{{ errorTypeLabel(row.error_type) }}</template>
        </LTable>
      </template>
      <template v-else>
        <LTable :columns="chunkCols" :rows="results.chunks" compact>
          <template #col-status="{ row }">
            <span class="status-cell"><LDot :tone="chunkTone(row.status)" />{{ chunkLabel(row.status) }}</span>
          </template>
          <template #col-risk="{ row }">
            <LProgress v-if="row.risk >= 0" class="row-progress" :value="row.risk" />
            <span v-else>—</span>
          </template>
          <template #col-owner="{ row }">{{ row.owner[0] }}–{{ row.owner[1] }}</template>
        </LTable>
      </template>
    </LSection>
  </div>
</template>


<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import {
  LButton,
  LEmpty,
  LPopover,
  LProgress,
  LSection,
  LSegmented,
  LSelect,
  LTable,
  LTag,
  LDot,
  toast,
} from '../ui'
import type { LColumn } from '../ui'
import { api } from '../api/client'
import { errorTypeLabel } from '../lib/labels'
import { useGecStore } from '../stores/gec'
import type { DocJobListItem, DocJobProgress, DocJobResults } from '../types'

type JobErrorRow = DocJobResults['errors'][number]
type ChunkRow = DocJobResults['chunks'][number]

const store = useGecStore()
const text = ref('')
const budget = ref<'' | 'full' | 'economical'>('')
const BUDGET_OPTIONS = [
  { value: '', label: '跟随设置默认' },
  { value: 'full', label: '全量深度复查（full）' },
  { value: 'economical', label: '经济模式（economical）' },
]
/** LSelect 统一吐 string，回到窄联合类型再进请求体 */
const budgetSel = computed({
  get: () => budget.value,
  set: (v: string | number) => {
    budget.value = v === 'full' || v === 'economical' ? v : ''
  },
})
const creating = ref(false)
const listing = ref(false)
const jobs = ref<DocJobListItem[]>([])
const selectedId = ref<string | null>(null)
const results = ref<DocJobResults | null>(null)
const resultTab = ref<'errors' | 'chunks'>('errors')
const resultTabOptions = computed(() => [
  { value: 'errors' as const, label: `检出错误（${results.value?.errors.length ?? 0}）` },
  { value: 'chunks' as const, label: `块状态（${results.value?.chunks.length ?? 0}）` },
])

const jobCols: LColumn<DocJobListItem>[] = [
  { key: 'job', label: '作业', width: '110px' },
  { key: 'lang', label: '语言', width: '64px' },
  { key: 'status', label: '状态', width: '92px' },
  { key: 'progress', label: '进度（处理块）' },
  { key: 'coverage', label: '覆盖率', width: '84px' },
  { key: 'results', label: '结果', width: '72px' },
  { key: 'actions', label: '操作', width: '120px' },
]
const errCols: LColumn<JobErrorRow>[] = [
  { key: 'pos', label: '位置', width: '110px' },
  { key: 'original', label: '原文', width: '140px' },
  { key: 'suggest', label: '建议', width: '140px' },
  { key: 'type', label: '类型', width: '110px' },
  { key: 'message', label: '说明' },
]
const chunkCols: LColumn<ChunkRow>[] = [
  { key: 'index', label: '#', width: '56px' },
  { key: 'owner', label: '字节区间', width: '140px' },
  { key: 'status', label: '状态', width: '120px' },
  { key: 'risk', label: '风险', width: '150px' },
  { key: 'error', label: '块错误' },
]

let es: EventSource | null = null
let pollTimer: ReturnType<typeof setInterval> | null = null

async function refreshList() {
  listing.value = true
  try {
    jobs.value = (await api.docJobList()).jobs
  } catch (e) {
    toast(`作业列表加载失败：${(e as Error).message}`, 'danger', { label: '重试', onClick: () => void refreshList() })
  } finally {
    listing.value = false
  }
}

async function create() {
  creating.value = true
  try {
    const body: { lang: string; text: string; budget?: 'full' | 'economical' } = {
      lang: store.lang,
      text: text.value,
    }
    if (budget.value) body.budget = budget.value
    const r = await api.docJobCreate(body)
    toast(`作业已创建：${r.chunks} 块并行复查中`, 'ok')
    text.value = ''
    await refreshList()
    const row = jobs.value.find((j) => j.id === r.job_id)
    if (row) selectJob(row)
  } catch (e) {
    toast(`创建失败：${(e as Error).message}`, 'danger')
  } finally {
    creating.value = false
  }
}

function selectJob(row: DocJobListItem | null) {
  if (!row) return
  selectedId.value = row.id
  attachEvents(row.id)
  if (row.status === 'done' || row.status === 'cancelled') void loadResults(row.id)
}

function openResults(row: DocJobListItem) {
  selectedId.value = row.id
  attachEvents(row.id)
  void loadResults(row.id)
}

async function loadResults(id: string | null) {
  if (!id) return
  try {
    results.value = await api.docJobResults(id)
  } catch (e) {
    toast(`结果加载失败：${(e as Error).message}`, 'danger', { label: '重试', onClick: () => void loadResults(id) })
  }
}

/** SSE 优先；断线回落 1s 轮询（端点幂等，无服务端游标状态） */
function attachEvents(id: string) {
  detachEvents()
  try {
    es = new EventSource(api.docJobEventsUrl(id))
    es.addEventListener('progress', (ev) => {
      const d = JSON.parse((ev as MessageEvent).data) as {
        status: string
        progress: DocJobProgress
        coverage: number
      }
      const row = jobs.value.find((j) => j.id === id)
      if (row) {
        row.progress = d.progress
        row.coverage = d.coverage
      }
    })
    es.addEventListener('end', (ev) => {
      const d = JSON.parse((ev as MessageEvent).data) as {
        status: string
        progress: DocJobProgress
        coverage: number
      }
      const row = jobs.value.find((j) => j.id === id)
      if (row) {
        row.progress = d.progress
        row.coverage = d.coverage
        row.status = d.status as DocJobListItem['status']
      }
      detachEvents()
      void loadResults(id)
    })
    es.onerror = () => {
      detachEvents()
      startPolling(id)
    }
  } catch {
    startPolling(id)
  }
}

function startPolling(id: string) {
  stopPolling()
  pollTimer = setInterval(async () => {
    try {
      const d = await api.docJobGet(id)
      const row = jobs.value.find((j) => j.id === id)
      if (row) {
        row.progress = d.progress
        row.coverage = d.coverage
        row.status = d.status
      }
      if (d.status === 'done' || d.status === 'cancelled') {
        stopPolling()
        void loadResults(id)
      }
    } catch {
      stopPolling()
    }
  }, 1000)
}

function stopPolling() {
  if (pollTimer) clearInterval(pollTimer)
  pollTimer = null
}

function detachEvents() {
  es?.close()
  es = null
  stopPolling()
}

async function cancel(id: string) {
  try {
    await api.docJobCancel(id)
    toast('已请求取消：已开跑块完成后停止，未开跑块保持待跑')
  } catch (e) {
    toast(`取消失败：${(e as Error).message}`, 'danger')
  }
}

async function retry(id: string) {
  try {
    const r = await api.docJobRetry(id)
    toast(`已重排队 ${r.reset_chunks} 个失败块`, 'ok')
    void refreshList()
  } catch (e) {
    toast(`重跑失败：${(e as Error).message}`, 'danger')
  }
}

function progressPct(p: DocJobProgress): number {
  if (!p.total) return 0
  return Math.round(((p.done + p.cached + p.skipped + p.failed + p.parse_failed) / p.total) * 100) / 100
}

const incompleteReason = computed(() => {
  const p = results.value?.progress
  if (!p) return ''
  const parts: string[] = []
  if (p.skipped) parts.push(`${p.skipped} 块按预算跳过`)
  if (p.failed) parts.push(`${p.failed} 块调用失败`)
  if (p.parse_failed) parts.push(`${p.parse_failed} 块解析失败`)
  if (p.pending) parts.push(`${p.pending} 块未复查`)
  return `结果不完整：${parts.join('，')}——可重跑失败块或改用全量预算重新创建`
})

/** 状态点：红=错误仅此一种含义；created/cancelled 是中性终态 */
function statusTone(s: string): 'ok' | 'warn' | 'danger' | 'muted' {
  if (s === 'done') return 'ok'
  if (s === 'running') return 'warn'
  return 'muted'
}
function statusLabel(s: string) {
  return { created: '待跑', running: '进行中', done: '完成', cancelled: '已取消' }[s] ?? s
}
/** 块状态：失败/解析失败是错误 → danger；缓存命中是来源属性 → 中性 */
function chunkTone(s: string): 'ok' | 'warn' | 'danger' | 'muted' {
  if (s === 'done') return 'ok'
  if (s === 'failed' || s === 'parse_failed') return 'danger'
  return 'muted'
}
function chunkLabel(s: string) {
  return (
    {
      pending: '待复查',
      done: '已复查',
      cached: '缓存命中',
      skipped: '已跳过',
      failed: '失败',
      parse_failed: '解析失败',
    }[s] ?? s
  )
}

onMounted(() => void refreshList())
onBeforeUnmount(detachEvents)
</script>

<style scoped>
.doc-jobs-panel {
  display: flex;
  flex-direction: column;
  gap: var(--space-3);
}
.paste-area {
  display: block;
  width: 100%;
  font: inherit;
  font-size: var(--fs-ui);
  line-height: var(--lh-ui);
  color: var(--text);
  background: var(--surface);
  border: 1px solid var(--border-strong);
  border-radius: var(--radius-ctl);
  padding: var(--space-2);
}
.paste-area:focus {
  border-color: var(--accent);
  outline: none;
  box-shadow: var(--focus-ring);
}
.toolbar {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  margin-top: var(--space-2);
  font-size: var(--fs-ui);
  color: var(--muted);
}
.sep {
  width: 1px;
  height: 16px;
  background: var(--border-strong);
}

.spacer {
  flex: 1;
}
.mono {
  font-family: var(--font-mono);
  font-size: var(--fs-sm);
  color: var(--muted);
}
.status-cell {
  display: inline-flex;
  align-items: center;
  gap: var(--space-2);
  white-space: nowrap;
}
.row-progress {
  min-width: 90px;
}
.banner-warn {
  margin-bottom: var(--space-2);
  color: var(--warn-ink);
  background: var(--warn-soft);
  border-color: var(--warn-line);
}
</style>
