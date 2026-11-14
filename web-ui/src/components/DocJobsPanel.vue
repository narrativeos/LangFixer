<template>
  <div class="doc-jobs-panel">
    <el-card shadow="never">
      <template #header>
        <div class="card-header">
          <span class="card-title">新建文档审校作业</span>
          <span class="hint">长文档（10 万字+）切块并行 LLM 深度复查：断点续跑 / 块缓存 / 风险优先</span>
        </div>
      </template>
      <el-input
        v-model="text"
        type="textarea"
        :rows="6"
        placeholder="粘贴整篇文档，提交后转为后台作业，可随时离开本页"
      />
      <div class="toolbar">
        <span>字符数：{{ text.length }}</span>
        <el-divider direction="vertical" />
        <span>预算</span>
        <el-select v-model="budget" style="width: 230px">
          <el-option label="跟随设置默认" value="" />
          <el-option label="全量深度复查（full）" value="full" />
          <el-option label="经济模式（economical）" value="economical" />
        </el-select>
        <el-tooltip
          content="经济模式跳过低风险块：更省 LLM 调用，但覆盖率与召回如实下降"
          placement="top"
        >
          <el-icon class="hint-icon"><QuestionFilled /></el-icon>
        </el-tooltip>
        <span class="spacer"></span>
        <el-button
          type="primary"
          :loading="creating"
          :disabled="text.trim().length === 0"
          @click="create"
        >
          创建审校作业
        </el-button>
      </div>
    </el-card>

    <el-card shadow="never">
      <template #header>
        <div class="card-header">
          <span class="card-title">作业列表</span>
          <span class="spacer"></span>
          <el-button size="small" plain :loading="listing" @click="refreshList">刷新</el-button>
        </div>
      </template>
      <el-table
        :data="jobs"
        size="small"
        highlight-current-row
        empty-text="暂无作业——粘贴文档创建第一个作业"
        @current-change="selectJob"
      >
        <el-table-column label="作业" width="110">
          <template #default="{ row }">
            <code>{{ row.id.slice(0, 8) }}</code>
          </template>
        </el-table-column>
        <el-table-column prop="lang" label="语言" width="64" />
        <el-table-column label="状态" width="92">
          <template #default="{ row }">
            <el-tag :type="statusType(row.status)" size="small">{{ statusLabel(row.status) }}</el-tag>
          </template>
        </el-table-column>
        <el-table-column label="进度（处理块）" min-width="200">
          <template #default="{ row }">
            <el-progress
              :percentage="progressPct(row.progress)"
              :status="row.status === 'done' ? 'success' : row.status === 'cancelled' ? 'warning' : undefined"
              :stroke-width="10"
            />
          </template>
        </el-table-column>
        <el-table-column label="覆盖率" width="84">
          <template #default="{ row }">{{ (row.coverage * 100).toFixed(0) }}%</template>
        </el-table-column>
        <el-table-column label="结果" width="72">
          <template #default="{ row }">
            <el-button link type="primary" size="small" @click.stop="openResults(row)">
              {{ row.id === selectedId && results ? results.errors.length : '查看' }}
            </el-button>
          </template>
        </el-table-column>
        <el-table-column label="操作" width="120">
          <template #default="{ row }">
            <el-button
              v-if="row.status === 'running' || row.status === 'created'"
              link
              type="warning"
              size="small"
              @click.stop="cancel(row.id)"
            >
              取消
            </el-button>
            <el-button
              v-if="row.status === 'done' || row.status === 'cancelled'"
              link
              type="primary"
              size="small"
              @click.stop="retry(row.id)"
            >
              重跑失败块
            </el-button>
          </template>
        </el-table-column>
      </el-table>
    </el-card>

    <el-card v-if="results" shadow="never">
      <template #header>
        <div class="card-header">
          <span class="card-title">作业结果 <code>{{ selectedId?.slice(0, 8) }}</code></span>
          <el-tag v-if="results.complete" type="success" size="small">全量复查完成</el-tag>
          <el-tag v-else type="warning" size="small">
            结果不完整（覆盖率 {{ (results.coverage * 100).toFixed(0) }}%）
          </el-tag>
          <span class="spacer"></span>
          <el-button size="small" plain @click="loadResults(selectedId)">刷新结果</el-button>
        </div>
      </template>
      <el-alert
        v-if="!results.complete"
        type="warning"
        :closable="false"
        class="cov-alert"
        :title="incompleteReason"
      />
      <el-tabs v-model="resultTab">
        <el-tab-pane :label="`检出错误（${results.errors.length}）`" name="errors">
          <el-table :data="results.errors" size="small" empty-text="未检出错误">
            <el-table-column label="位置" width="110">
              <template #default="{ row }">{{ row.span[0] }}–{{ row.span[1] }}</template>
            </el-table-column>
            <el-table-column prop="original" label="原文" width="140" />
            <el-table-column label="建议" width="140">
              <template #default="{ row }">{{ row.suggestions?.[0]?.text ?? row.correction }}</template>
            </el-table-column>
            <el-table-column prop="error_type" label="类型" width="90" />
            <el-table-column prop="message" label="说明" min-width="200" />
          </el-table>
        </el-tab-pane>
        <el-tab-pane :label="`块状态（${results.chunks.length}）`" name="chunks">
          <el-table :data="results.chunks" size="small">
            <el-table-column prop="index" label="#" width="56" />
            <el-table-column label="字节区间" width="140">
              <template #default="{ row }">{{ row.owner[0] }}–{{ row.owner[1] }}</template>
            </el-table-column>
            <el-table-column label="状态" width="110">
              <template #default="{ row }">
                <el-tag :type="chunkType(row.status)" size="small">{{ chunkLabel(row.status) }}</el-tag>
              </template>
            </el-table-column>
            <el-table-column label="风险" width="150">
              <template #default="{ row }">
                <el-progress
                  v-if="row.risk >= 0"
                  :percentage="Math.round(row.risk * 100)"
                  :color="riskColor(row.risk)"
                  :stroke-width="8"
                />
                <span v-else>—</span>
              </template>
            </el-table-column>
            <el-table-column prop="error" label="块错误" min-width="160" />
          </el-table>
        </el-tab-pane>
      </el-tabs>
    </el-card>
  </div>
</template>


<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { ElMessage } from 'element-plus'
import { QuestionFilled } from '@element-plus/icons-vue'
import { api } from '../api/client'
import { useGecStore } from '../stores/gec'
import type { DocJobListItem, DocJobProgress, DocJobResults } from '../types'

const store = useGecStore()
const text = ref('')
const budget = ref<'' | 'full' | 'economical'>('')
const creating = ref(false)
const listing = ref(false)
const jobs = ref<DocJobListItem[]>([])
const selectedId = ref<string | null>(null)
const results = ref<DocJobResults | null>(null)
const resultTab = ref('errors')

let es: EventSource | null = null
let pollTimer: ReturnType<typeof setInterval> | null = null

async function refreshList() {
  listing.value = true
  try {
    jobs.value = (await api.docJobList()).jobs
  } catch (e) {
    ElMessage.error(`作业列表加载失败：${(e as Error).message}`)
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
    ElMessage.success(`作业已创建：${r.chunks} 块并行复查中`)
    text.value = ''
    await refreshList()
    const row = jobs.value.find((j) => j.id === r.job_id)
    if (row) selectJob(row)
  } catch (e) {
    ElMessage.error(`创建失败：${(e as Error).message}`)
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
    ElMessage.error(`结果加载失败：${(e as Error).message}`)
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
    ElMessage.info('已请求取消：已开跑块完成后停止，未开跑块保持待跑')
  } catch (e) {
    ElMessage.error(`取消失败：${(e as Error).message}`)
  }
}

async function retry(id: string) {
  try {
    const r = await api.docJobRetry(id)
    ElMessage.success(`已重排队 ${r.reset_chunks} 个失败块`)
    void refreshList()
  } catch (e) {
    ElMessage.error(`重跑失败：${(e as Error).message}`)
  }
}

function progressPct(p: DocJobProgress): number {
  if (!p.total) return 0
  return Math.round(((p.done + p.cached + p.skipped + p.failed + p.parse_failed) / p.total) * 100)
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

function statusType(s: string) {
  return s === 'done'
    ? 'success'
    : s === 'cancelled'
      ? 'warning'
      : s === 'running'
        ? 'primary'
        : 'info'
}
function statusLabel(s: string) {
  return { created: '待跑', running: '进行中', done: '完成', cancelled: '已取消' }[s] ?? s
}
function chunkType(s: string) {
  return s === 'done'
    ? 'success'
    : s === 'cached'
      ? 'primary'
      : s === 'pending' || s === 'skipped'
        ? 'info'
        : 'danger'
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
function riskColor(r: number): string {
  return r >= 0.6
    ? 'var(--el-color-danger)'
    : r >= 0.2
      ? 'var(--el-color-warning)'
      : 'var(--el-color-success)'
}

onMounted(() => void refreshList())
onBeforeUnmount(detachEvents)
</script>

<style scoped>
.doc-jobs-panel {
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.toolbar {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-top: 10px;
  font-size: 13px;
  color: var(--el-text-color-secondary);
}
.hint-icon {
  color: var(--el-text-color-secondary);
}
.card-header {
  display: flex;
  align-items: center;
  gap: 8px;
}
.card-title {
  font-weight: 600;
}
.hint {
  font-size: 12px;
  color: var(--el-text-color-secondary);
}
.spacer {
  flex: 1;
}
.cov-alert {
  margin-bottom: 10px;
}
</style>

