<script setup lang="ts">
import { ref, onMounted, watch } from 'vue'
import { errMsg } from '../lib/err'
import { errorTypeLabel } from '../lib/labels'

import { ElMessage, ElMessageBox } from 'element-plus'
import { api } from '../api/client'
import { useGecStore } from '../stores/gec'
import type { DetectTimings, HistoryDetail, HistoryListItem, LayerReport } from '../types'

const props = defineProps<{ lang: string }>()
const store = useGecStore()

const items = ref<HistoryListItem[]>([])
const loading = ref(false)
const error = ref('')
const expandedId = ref<number | null>(null)
const detail = ref<HistoryDetail | null>(null)
const detailErrors = ref<{ error_type: string; original: string; correction: string | null; message: string }[]>([])
const detailTimings = ref<DetectTimings | null>(null)
const detailLayers = ref<LayerReport[]>([])

/** 服务端全文搜索（q 关键词，防抖 300ms 后重查）+ 分页（offset/limit，总数由服务端给） */
const keyword = ref('')
const total = ref(0)
const page = ref(1)
const pageSize = ref(50)
const PAGE_SIZES = [20, 50, 100, 200]
let searchTimer: ReturnType<typeof setTimeout> | null = null

/** 历史详情的层级贡献摘要文案（与 ResultsPanel「检测路径」同词表） */
const LAYER_LABELS: Record<string, string> = {
  rule: '规则',
  decision: '决策模型',
  knowledge: '知识召回',
  feature: '特征模型',
  llm: 'LLM',
  statistical: '统计模型',
}
function histLayerText(l: LayerReport): string {
  const base = LAYER_LABELS[l.layer] ?? l.layer
  if (l.status === 'ran') return l.found > 0 ? `${base} ${l.found} 条` : `${base} 未命中`
  if (l.status === 'decided') return l.found > 0 ? `${base} 定案 ${l.found} 条` : `${base} 定案（无错）`
  if (l.status === 'abstained') return `${base} 弃权`
  if (l.status === 'unavailable') return `${base} 不可用`
  return `${base} 未触发`
}

/** 历史参数标签（解析 params_json；旧记录无参数快照时显示「未记录参数」） */
function paramsLabel(item: { params_json?: string | null }): string {
  if (!item.params_json) return '未记录参数'
  try {
    const p = JSON.parse(item.params_json) as {
      level?: number
      force_llm?: boolean
      category?: string | null
      model_version?: string | null
      rules_fingerprint?: string
    }
    const parts: string[] = []
    if (p.force_llm) parts.push('强制 LLM')
    else if (p.level && p.level > 1) parts.push(`L${p.level}`)
    else parts.push('L1')
    if (p.category) parts.push(p.category)
    if (p.model_version) parts.push(`模型 ${p.model_version}`)
    return parts.join(' · ')
  } catch {
    return '未记录参数'
  }
}

/** 语言代码 → 展示名（后端语言元信息；未知代码回落原文） */
function langName(code: string): string {
  return store.displayLangs.find((l) => l.lang === code)?.name ?? code
}

async function load() {
  loading.value = true
  error.value = ''
  try {
    const p = await api.listHistory({
      lang: props.lang,
      limit: pageSize.value,
      offset: (page.value - 1) * pageSize.value,
      q: keyword.value.trim() || undefined,
    })
    items.value = p.items
    total.value = p.total
    // 删除/清空后当前页可能越界：回退一页重查（不闪空页）
    if (!p.items.length && p.total > 0 && page.value > 1) {
      page.value = Math.max(1, Math.ceil(p.total / pageSize.value))
      const again = await api.listHistory({
        lang: props.lang,
        limit: pageSize.value,
        offset: (page.value - 1) * pageSize.value,
        q: keyword.value.trim() || undefined,
      })
      items.value = again.items
      total.value = again.total
    }
  } catch (e) {
    error.value = errMsg(e)
  } finally {
    loading.value = false
  }
}

function reloadPage(p: number) {
  page.value = p
  void load()
}
function reloadSize(s: number) {
  pageSize.value = s
  page.value = 1
  void load()
}

/** 历史复用：载入工作区（切回检测视图 + 填文本 + 切语言 + 清旧结果） */
async function loadToWorkspace(id: number) {
  try {
    const d = await api.getHistory(id)
    store.loadText(d.text, d.lang)
    store.requestNav('workspace')
    ElMessage.success('已载入历史文本到工作区')
  } catch (e) {
    error.value = errMsg(e)
  }
}

/** 历史复用：载入并立即重新检测（按当前参数重跑「检测 + 归因」） */
async function loadAndRedetect(id: number) {
  try {
    const d = await api.getHistory(id)
    store.loadText(d.text, d.lang)
    store.requestNav('workspace')
    void store.analyze(store.sessionId || undefined)
  } catch (e) {
    error.value = errMsg(e)
  }
}

async function expand(id: number) {
  if (expandedId.value === id) {
    expandedId.value = null
    detail.value = null
    detailTimings.value = null
    detailLayers.value = []
    return
  }
  expandedId.value = id
  try {
    detail.value = await api.getHistory(id)
    detailErrors.value = JSON.parse(detail.value.errors_json)
    try {
      detailTimings.value = detail.value.timings_json
        ? (JSON.parse(detail.value.timings_json) as DetectTimings)
        : null
    } catch {
      detailTimings.value = null
    }
    try {
      detailLayers.value = detail.value.layers_json
        ? (JSON.parse(detail.value.layers_json) as LayerReport[])
        : []
    } catch {
      detailLayers.value = []
    }
  } catch (e) {
    error.value = errMsg(e)
  }
}

async function remove(id: number) {
  try {
    await ElMessageBox.confirm('确定删除这条记录？', '删除确认', { type: 'warning' })
  } catch {
    return
  }
  try {
    await api.removeHistory(id)
    if (expandedId.value === id) {
      expandedId.value = null
      detail.value = null
    }
    ElMessage.success('已删除')
    await load()
  } catch (e) {
    error.value = errMsg(e)
  }
}

async function clearAll() {
  try {
    await ElMessageBox.confirm('确定清空全部检测历史？此操作不可撤销。', '清空确认', { type: 'warning' })
  } catch {
    return
  }
  try {
    const res = await api.clearHistory()
    ElMessage.success(`已清空 ${res.deleted} 条记录`)
    expandedId.value = null
    detail.value = null
    await load()
  } catch (e) {
    error.value = errMsg(e)
  }
}

onMounted(load)

// 关键词防抖重查（服务端搜索）：输入停顿 300ms 后回到第 1 页重新拉取
watch(keyword, () => {
  if (searchTimer) clearTimeout(searchTimer)
  searchTimer = setTimeout(() => {
    page.value = 1
    void load()
  }, 300)
})

// 语言页签切换 → 重新分页拉取（列表按当前语言过滤）
watch(
  () => props.lang,
  () => {
    page.value = 1
    void load()
  },
)

// 检测完成后自动刷新（store 在 detect / detectLlm / analyze 成功后递增 historyNonce）
watch(
  () => store.historyNonce,
  () => {
    // 新记录在最新页顶部：跳回第 1 页再拉（关键词搜索中则留在当前搜索口径）
    if (!keyword.value.trim()) page.value = 1
    void load()
  },
)
</script>

<template>
  <el-card shadow="never">
    <template #header>
      <div class="card-header">
        <span class="card-title">检测历史</span>
        <span class="sub">共 {{ total }} 条检测记录</span>
        <span class="spacer"></span>
        <el-input
          v-model="keyword"
          size="small"
          placeholder="搜索全文…"
          clearable
          style="width: 200px"
        />
        <el-button size="small" :loading="loading" @click="load">
          <el-icon v-if="!loading"><Refresh /></el-icon>
          刷新
        </el-button>
        <el-button size="small" type="danger" plain @click="clearAll">清空全部</el-button>
      </div>
    </template>

    <div v-if="loading && !items.length" class="loading">
      <el-icon class="is-loading"><Loading /></el-icon>
      加载中…
    </div>
    <el-alert v-else-if="error" type="error" :closable="false" :title="error" />
    <el-table
      v-else
      :data="items"
      :empty-text="
        keyword.trim()
          ? `未找到匹配「${keyword.trim()}」的检测记录`
          : '暂无检测历史：在上方「文本输入」粘贴文本并点「检测 + 归因」，完成后自动记录在此（可回看 / 载入复用 / 删除）'
      "
      class="history-table"
      @row-click="(row: HistoryListItem) => expand(row.id)"
    >
      <el-table-column type="expand">
        <template #default="{ row }">
          <div v-if="expandedId === row.id && detail" class="detail">
            <div class="detail-text">{{ detail.text }}</div>
            <div class="detail-actions">
              <el-button size="small" type="primary" plain @click.stop="loadToWorkspace(row.id)">
                载入工作区
              </el-button>
              <el-button size="small" type="primary" @click.stop="loadAndRedetect(row.id)">
                载入并重新检测
              </el-button>
            </div>
            <div v-if="detail.version || detail.params_json" class="detail-meta">
              <el-tag v-if="detail.version" size="small" type="info">版本 v{{ detail.version }}</el-tag>
              <el-tag size="small" :type="detail.params_json ? 'info' : 'warning'">
                参数 {{ paramsLabel(detail) }}
              </el-tag>
            </div>
            <div v-if="detailTimings" class="detail-timings">
              <el-tag size="small" type="info">总耗时 {{ detailTimings.total_ms }} ms</el-tag>
              <el-tag size="small">规则 {{ detailTimings.rule_ms }} ms</el-tag>
              <el-tag
                v-if="detailTimings.nlp_ms != null"
                size="small"
                :type="detailTimings.nlp_warmup_ms != null ? 'warning' : 'info'"
              >
                NLP {{ detailTimings.nlp_ms }} ms<template v-if="detailTimings.nlp_warmup_ms != null">（含预热 {{ detailTimings.nlp_warmup_ms }} ms）</template>
              </el-tag>
              <el-tag v-if="detailTimings.decision_ms != null" size="small">决策 {{ detailTimings.decision_ms }} ms</el-tag>
              <el-tag v-if="detailTimings.l3_ms != null" size="small">L3 {{ detailTimings.l3_ms }} ms</el-tag>
              <el-tag v-if="detailTimings.recall_ms != null" size="small">召回 {{ detailTimings.recall_ms }} ms</el-tag>
              <el-tag v-if="detailTimings.llm_ms != null" size="small">LLM {{ detailTimings.llm_ms }} ms</el-tag>
            </div>
            <div v-if="detailLayers.length" class="detail-timings">
              <el-tag v-for="l in detailLayers" :key="l.layer" size="small" effect="plain" type="info">
                {{ histLayerText(l) }}
              </el-tag>
            </div>
            <div v-if="detailErrors.length > 0" class="detail-errors">
              <div v-for="(e, i) in detailErrors" :key="i" class="detail-error">
                <el-tag type="danger" size="small">{{ errorTypeLabel(e.error_type) }}</el-tag>
                <span class="orig">{{ e.original }}</span>
                <template v-if="e.correction">
                  <span class="arrow">→</span>
                  <span class="corr">{{ e.correction }}</span>
                </template>
                <span class="msg">{{ e.message }}</span>
              </div>
            </div>
            <div v-else class="no-errors">无错误</div>
          </div>
          <div v-else class="no-errors">点击行加载详情…</div>
        </template>
      </el-table-column>
      <el-table-column prop="id" label="#" width="70" />
      <el-table-column label="时间" width="170">
        <template #default="{ row }"><span class="time">{{ row.created_at }}</span></template>
      </el-table-column>
      <el-table-column label="语言" width="90">
        <template #default="{ row }">
          <el-tag size="small" type="primary" effect="light">{{ langName(row.lang) }}</el-tag>
        </template>
      </el-table-column>
      <el-table-column label="文本预览">
        <template #default="{ row }"><span class="preview">{{ row.text_preview }}</span></template>
      </el-table-column>
      <el-table-column label="错误数" width="90">
        <template #default="{ row }">
          <span :class="{ 'has-errors': row.error_count > 0 }">{{ row.error_count }}</span>
        </template>
      </el-table-column>
      <el-table-column label="耗时" width="110">
        <template #default="{ row }">
          <span v-if="row.total_ms != null" class="time">{{ row.total_ms }} ms</span>
          <span v-else class="muted">—</span>
        </template>
      </el-table-column>
      <el-table-column label="参数" width="150">
        <template #default="{ row }">
          <el-tag v-if="row.params_json" size="small" effect="plain">{{ paramsLabel(row) }}</el-tag>
          <span v-else class="muted">未记录参数</span>
        </template>
      </el-table-column>
      <el-table-column label="版本" width="100">
        <template #default="{ row }">
          <span v-if="row.version" class="muted">v{{ row.version }}</span>
          <span v-else class="muted">—</span>
        </template>
      </el-table-column>
      <el-table-column label="操作" width="170">
        <template #default="{ row }">
          <el-button type="primary" size="small" plain @click.stop="loadToWorkspace(row.id)">
            载入
          </el-button>
          <el-button
            type="danger"
            size="small"
            plain
            @click.stop="remove(row.id)"
          >
            删除
          </el-button>
        </template>
      </el-table-column>
    </el-table>
    <div v-if="total > 0" class="pager">
      <el-pagination
        :current-page="page"
        :page-size="pageSize"
        :page-sizes="PAGE_SIZES"
        :total="total"
        layout="total, sizes, prev, pager, next, jumper"
        size="small"
        background
        @current-change="reloadPage"
        @size-change="reloadSize"
      />
    </div>
    <el-empty
      v-if="!loading && !error && items.length === 0"
      description="暂无检测记录 — 完成一次「检测」或「检测 + 归因」后，记录会出现在这里"
      :image-size="80"
    />
  </el-card>
</template>

<style scoped>
.time { color: var(--muted); white-space: nowrap; }
.muted { color: var(--muted); }
.preview {
  max-width: 300px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  color: var(--text-2);
}
.has-errors { color: var(--danger); font-weight: 600; }
.detail { padding: 8px 12px; }
.detail-text { margin-bottom: 8px; white-space: pre-wrap; font-size: 0.95em; }
.detail-actions { display: flex; gap: 8px; margin-bottom: 10px; }
.detail-meta { margin-bottom: 8px; }
.detail-timings { display: flex; gap: 6px; flex-wrap: wrap; margin-bottom: 8px; }
.detail-errors { display: flex; flex-direction: column; gap: 6px; }
.detail-error { display: flex; gap: 8px; align-items: center; flex-wrap: wrap; font-size: 13.5px; }
.orig { color: var(--text); }
.corr { color: var(--ok); font-weight: 500; }
.msg { color: var(--muted); }
.no-errors { color: var(--muted); font-size: 0.9em; }
.pager { display: flex; justify-content: flex-end; margin-top: 10px; }
</style>