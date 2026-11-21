<script setup lang="ts">
import { computed, ref, onBeforeUnmount, onMounted, watch } from 'vue'
import type { ComponentPublicInstance } from 'vue'
import { errMsg } from '../lib/err'
import { errorTypeLabel, modeLabel, plainLayerLabel } from '../lib/labels'

import LButton from '../ui/LButton.vue'
import LInput from '../ui/LInput.vue'
import LSelect from '../ui/LSelect.vue'
import LTag from '../ui/LTag.vue'
import LEmpty from '../ui/LEmpty.vue'
import { toast } from '../ui/toast'
import { api } from '../api/client'
import { useGecStore } from '../stores/gec'
import type { DetectTimings, ErrorItem, HistoryDetail, HistoryListItem, LayerReport } from '../types'
import {
  buildChunkMatrix,
  matrixColumnsFromEvents,
  ORIGIN_LABELS,
  type ChunkOrigin,
  type MatrixCell,
} from '../lib/chunkMatrix'

const props = defineProps<{ lang: string }>()
const store = useGecStore()

const items = ref<HistoryListItem[]>([])
const loading = ref(false)
const error = ref('')
const expandedId = ref<number | null>(null)
const detail = ref<HistoryDetail | null>(null)
const detailErrors = ref<ErrorItem[]>([])
const detailTimings = ref<DetectTimings | null>(null)
const detailLayers = ref<LayerReport[]>([])
/** 审计 F4：历史句表（块矩阵回放数据）；旧记录无句表时为 null（不编造） */
const detailUnits = ref<{ spans: [number, number][]; origins: ChunkOrigin[] } | null>(null)

/** 服务端全文搜索（q 关键词，防抖 300ms 后重查）+ 分页（offset/limit，总数由服务端给） */
const keyword = ref('')
const total = ref(0)
const page = ref(1)
const pageSize = ref(50)
const PAGE_SIZES = [20, 50, 100, 200].map((n) => ({ value: String(n), label: `${n} / 页` }))
const totalPages = computed(() => Math.max(1, Math.ceil(total.value / pageSize.value)))
let searchTimer: ReturnType<typeof setTimeout> | null = null

/** 危险操作走行内二次确认（侧栏形态不开对话框）：先「确认」，5 秒内再点一次才执行 */
const confirmDeleteId = ref<number | null>(null)
let confirmTimer: ReturnType<typeof setTimeout> | null = null
function clearConfirmTimer() {
  if (confirmTimer) {
    clearTimeout(confirmTimer)
    confirmTimer = null
  }
}
function armDelete(id: number) {
  clearConfirmTimer()
  if (confirmDeleteId.value === id) {
    void doDelete(id)
    return
  }
  confirmDeleteId.value = id
  confirmTimer = setTimeout(() => {
    confirmDeleteId.value = null
    confirmTimer = null
  }, 5000)
}

const clearArmed = ref(false)
let clearTimer: ReturnType<typeof setTimeout> | null = null
function armClear() {
  if (clearTimer) clearTimeout(clearTimer)
  if (clearArmed.value) {
    clearArmed.value = false
    clearTimer = null
    void doClear()
    return
  }
  clearArmed.value = true
  clearTimer = setTimeout(() => {
    clearArmed.value = false
    clearTimer = null
  }, 5000)
}

/** 历史详情的层级贡献摘要文案（与 ResultsPanel「检测路径」同词表） */
function histLayerText(l: LayerReport): string {
  // 展示名走统一人话词表（labels.ts plainLayerLabel）
  const base = plainLayerLabel(l.layer)
  if (l.status === 'ran') return l.found > 0 ? `${base} ${l.found} 条` : `${base} 未命中`
  if (l.status === 'decided') return l.found > 0 ? `${base} 定案 ${l.found} 条` : `${base} 定案（无错）`
  if (l.status === 'abstained') return `${base} 未定`
  if (l.status === 'unavailable') return `${base} 不可用`
  return `${base} 跳过`
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
    if (p.force_llm) parts.push('全文 AI 复查')
    else parts.push(modeLabel(p.level ?? 1))
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
function reloadSize(s: string | number) {
  pageSize.value = Number(s)
  page.value = 1
  void load()
}

/** 历史复用：载入工作区（切回检测视图 + 填文本 + 切语言 + 清旧结果） */
async function loadToWorkspace(id: number) {
  try {
    const d = await api.getHistory(id)
    store.loadText(d.text, d.lang)
    store.requestNav('workspace')
    toast('已载入历史文本到工作区', 'ok')
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
    detailUnits.value = null
    confirmDeleteId.value = null
    clearConfirmTimer()
    return
  }
  expandedId.value = id
  confirmDeleteId.value = null
  clearConfirmTimer()
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
    try {
      detailUnits.value = detail.value.units_json
        ? (JSON.parse(detail.value.units_json) as {
            spans: [number, number][]
            origins: ChunkOrigin[]
          })
        : null
    } catch {
      detailUnits.value = null
    }
  } catch (e) {
    error.value = errMsg(e)
  }
}

// ── 审计 F4：历史详情块矩阵回放（与 StageProgress 矩阵同口径：列从层报告派生，
// 单元格由句表区间 × 错误 span 相交计算；旧记录无句表/无列时降级为提示行）──
const detailMatrixCols = computed(() =>
  matrixColumnsFromEvents([], detailLayers.value.length ? detailLayers.value : null),
)
const detailMatrixRows = computed(() => {
  if (!detail.value || !detailUnits.value || !detailMatrixCols.value.length) return []
  return buildChunkMatrix({
    text: detail.value.text,
    units: detailUnits.value,
    errors: detailErrors.value,
    columns: detailMatrixCols.value,
    layers: detailLayers.value,
  })
})
function matrixCellText(c: MatrixCell): string {
  if (c.status === 'hit') return String(c.hits)
  return c.status === 'pass' ? '✓' : c.status === 'skipped' ? '–' : '·'
}

async function doDelete(id: number) {
  clearConfirmTimer()
  confirmDeleteId.value = null
  try {
    await api.removeHistory(id)
    if (expandedId.value === id) {
      expandedId.value = null
      detail.value = null
    }
    toast('已删除', 'ok')
    await load()
  } catch (e) {
    error.value = errMsg(e)
  }
}

async function doClear() {
  try {
    const res = await api.clearHistory()
    toast(`已清空 ${res.deleted} 条记录`, 'ok')
    expandedId.value = null
    detail.value = null
    await load()
  } catch (e) {
    error.value = errMsg(e)
  }
}

// ── 桌面规范（macOS HIG）：⌘F 聚焦搜索 + 行右键菜单（原生弹出，Web 形态 no-op） ──
const searchRef = ref<ComponentPublicInstance | null>(null)
const onFocusSearch = () => {
  const el = searchRef.value?.$el as HTMLElement | null
  if (el instanceof HTMLElement) el.focus()
}

async function onRowContextMenu(row: HistoryListItem, ev: MouseEvent) {
  ev.preventDefault()
  const action = await api.showContextMenu('history')
  if (action === 'ctx:history:load') void loadToWorkspace(row.id)
  else if (action === 'ctx:history:delete') {
    // 侧栏不弹对话框：展开该行并进入行内二次确认
    if (expandedId.value !== row.id) void expand(row.id)
    armDelete(row.id)
    toast('该行点「确认删除」完成删除', 'warn')
  }
}

onMounted(() => {
  void load()
  window.addEventListener('langfixer:focus-history-search', onFocusSearch)
})
onBeforeUnmount(() => {
  window.removeEventListener('langfixer:focus-history-search', onFocusSearch)
  if (searchTimer) clearTimeout(searchTimer)
  clearConfirmTimer()
  if (clearTimer) clearTimeout(clearTimer)
})

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
  <div class="history-pane">
    <div class="head">
      <span class="title">检测历史</span>
      <span class="sub">共 {{ total }} 条</span>
    </div>

    <div class="search-row">
      <LInput ref="searchRef" v-model="keyword" class="search-input" placeholder="搜索全文…" />
      <LButton size="sm" :loading="loading" title="重新拉取当前页" @click="load">刷新</LButton>
    </div>

    <div v-if="loading && !items.length" class="loading">加载中…</div>

    <LEmpty
      v-else-if="error"
      :text="error"
      hint="检测历史暂时读不出来，重试或切换语言页签再试"
    >
      <template #action>
        <LButton size="sm" :loading="loading" @click="load">重试</LButton>
      </template>
    </LEmpty>

    <LEmpty
      v-else-if="!items.length"
      :text="
        keyword.trim()
          ? `未找到匹配「${keyword.trim()}」的检测记录`
          : '暂无检测历史'
      "
      hint="在「文本输入」粘贴文本并点「检测 + 归因」，完成后自动记录在此（可回看 / 载入复用 / 删除）"
    >
      <template v-if="keyword.trim()" #action>
        <LButton size="sm" @click="keyword = ''">清除搜索</LButton>
      </template>
    </LEmpty>

    <template v-else>
      <ul class="rows">
        <li
          v-for="row in items"
          :key="row.id"
          class="row"
          :class="{ 'row--open': expandedId === row.id }"
          @click="expand(row.id)"
          @contextmenu="onRowContextMenu(row, $event)"
        >
          <div class="row-main">
            <span class="row-line1">
              <span class="time">{{ row.created_at }}</span>
              <span class="spacer"></span>
              <LTag :tone="row.error_count > 0 ? 'danger' : 'neutral'">
                {{ row.error_count }} 处
              </LTag>
            </span>
            <span class="preview">{{ row.text_preview }}</span>
          </div>

          <div v-if="expandedId === row.id" class="detail" @click.stop>
            <template v-if="detail">
              <div class="detail-text">{{ detail.text }}</div>
              <div class="detail-actions">
                <LButton size="sm" @click="loadToWorkspace(row.id)">载入工作区</LButton>
                <LButton size="sm" variant="primary" @click="loadAndRedetect(row.id)">
                  载入并重新检测
                </LButton>
              </div>
              <div v-if="detail.version || detail.params_json" class="detail-tags">
                <LTag v-if="detail.version" tone="neutral">版本 v{{ detail.version }}</LTag>
                <LTag :tone="detail.params_json ? 'neutral' : 'warn'">
                  参数 {{ paramsLabel(detail) }}
                </LTag>
              </div>
              <div v-if="detailTimings" class="detail-tags">
                <LTag tone="neutral">总耗时 {{ detailTimings.total_ms }} ms</LTag>
                <LTag tone="neutral">规则 {{ detailTimings.rule_ms }} ms</LTag>
                <LTag
                  v-if="detailTimings.nlp_ms != null"
                  :tone="detailTimings.nlp_warmup_ms != null ? 'warn' : 'neutral'"
                >
                  文本处理 {{ detailTimings.nlp_ms }} ms<template v-if="detailTimings.nlp_warmup_ms != null">（含首次准备 {{ detailTimings.nlp_warmup_ms }} ms）</template>
                </LTag>
                <LTag v-if="detailTimings.decision_ms != null" tone="neutral">
                  决策模型 {{ detailTimings.decision_ms }} ms
                </LTag>
                <LTag v-if="detailTimings.l3_ms != null" tone="neutral">
                  快速定案 {{ detailTimings.l3_ms }} ms
                </LTag>
                <LTag v-if="detailTimings.recall_ms != null" tone="neutral">
                  知识查询 {{ detailTimings.recall_ms }} ms
                </LTag>
                <LTag v-if="detailTimings.llm_ms != null" tone="neutral">
                  AI 复查 {{ detailTimings.llm_ms }} ms
                </LTag>
              </div>
              <div v-if="detailLayers.length" class="detail-tags">
                <LTag v-for="l in detailLayers" :key="l.layer" tone="neutral">
                  {{ histLayerText(l) }}
                </LTag>
              </div>
              <div v-if="detailErrors.length > 0" class="detail-errors">
                <div v-for="(e, i) in detailErrors" :key="i" class="detail-error">
                  <LTag tone="danger">{{ errorTypeLabel(e.error_type) }}</LTag>
                  <span class="orig">{{ e.original }}</span>
                  <template v-if="e.correction">
                    <span class="arrow">→</span>
                    <span class="corr">{{ e.correction }}</span>
                  </template>
                  <span class="msg">{{ e.message }}</span>
                </div>
              </div>
              <div v-else class="no-errors">无错误</div>
              <table v-if="detailMatrixRows.length" class="cm-table">
                <thead>
                  <tr>
                    <th class="cm-idx">#</th>
                    <th class="cm-prev">块预览</th>
                    <th class="cm-chars">字</th>
                    <th class="cm-origin">切分依据</th>
                    <th v-for="c in detailMatrixCols" :key="c.key">{{ c.no }} {{ c.label }}</th>
                  </tr>
                </thead>
                <tbody>
                  <tr v-for="(r, i) in detailMatrixRows" :key="r.span[0]">
                    <td class="cm-idx">{{ i + 1 }}</td>
                    <td class="cm-prev" :title="r.text">{{ r.preview }}</td>
                    <td class="cm-chars">{{ r.chars }}</td>
                    <td class="cm-origin">{{ ORIGIN_LABELS[r.origin] }}</td>
                    <td
                      v-for="c in detailMatrixCols"
                      :key="c.key"
                      class="cm-cell"
                      :class="'cm-' + r.cells[c.key].status"
                    >
                      {{ matrixCellText(r.cells[c.key]) }}
                    </td>
                  </tr>
                </tbody>
              </table>
              <div v-else-if="detail && !detailUnits" class="cm-empty">
                句表：该记录由旧版检测产生，无块数据（「载入并重新检测」后可见）
              </div>
              <div class="detail-actions">
                <LButton
                  size="sm"
                  :variant="confirmDeleteId === row.id ? 'danger' : 'default'"
                  @click="armDelete(row.id)"
                >
                  {{ confirmDeleteId === row.id ? '确认删除' : '删除' }}
                </LButton>
              </div>
            </template>
            <div v-else class="no-errors">详情加载中…</div>
          </div>
        </li>
      </ul>

      <div class="pager">
        <span v-if="total > 0" class="pager-text">第 {{ page }} / {{ totalPages }} 页</span>
        <span class="spacer"></span>
        <LButton size="sm" :disabled="page <= 1" @click="reloadPage(page - 1)">上一页</LButton>
        <LButton size="sm" :disabled="page >= totalPages" @click="reloadPage(page + 1)">下一页</LButton>
        <LSelect
          :model-value="String(pageSize)"
          :options="PAGE_SIZES"
          aria-label="每页条数"
          class="size-select"
          @update:model-value="reloadSize"
        />
      </div>
      <div class="foot">
        <LButton
          size="sm"
          :variant="clearArmed ? 'danger' : 'default'"
          :disabled="!total"
          @click="armClear"
        >
          {{ clearArmed ? '确认清空全部' : '清空全部' }}
        </LButton>
      </div>
    </template>
  </div>
</template>

<style scoped>
.history-pane { display: flex; flex-direction: column; gap: var(--space-2); }
.head { display: flex; align-items: baseline; gap: var(--space-2); }
.title { font-size: var(--fs-md); font-weight: 600; color: var(--text); }
.sub { color: var(--muted); font-size: var(--fs-xs); }
.spacer { flex: 1; }

.search-row { display: flex; gap: var(--space-2); align-items: center; }
.search-input { flex: 1; min-width: 0; }
.size-select { width: auto; }

.loading { color: var(--muted); font-size: var(--fs-sm); padding: var(--space-3) 0; }

.rows { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: var(--space-1); }
.row {
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  background: var(--surface);
  cursor: pointer;
}
.row:hover { border-color: var(--border-strong); }
.row--open { border-color: var(--accent); }
.row-main {
  display: flex; flex-direction: column; gap: 2px;
  padding: var(--space-2); min-width: 0;
}
.row-line1 { display: flex; align-items: center; gap: var(--space-2); min-width: 0; }
.time { color: var(--muted); font-size: var(--fs-xs); font-family: var(--font-mono); white-space: nowrap; }
.preview {
  display: block; min-width: 0;
  overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  color: var(--text-2); font-size: var(--fs-sm); line-height: var(--lh-ui);
}

.detail {
  display: flex; flex-direction: column; gap: var(--space-2);
  padding: var(--space-2); border-top: 1px solid var(--border);
  cursor: default;
}
.detail-text {
  white-space: pre-wrap; font-size: var(--fs-sm); line-height: var(--lh-body);
  max-height: 12lh; overflow: auto;
}
.detail-actions { display: flex; gap: var(--space-2); flex-wrap: wrap; }
.detail-tags { display: flex; gap: var(--space-1); flex-wrap: wrap; }
.detail-errors { display: flex; flex-direction: column; gap: var(--space-1); }
.detail-error { display: flex; gap: var(--space-2); align-items: center; flex-wrap: wrap; font-size: var(--fs-sm); }
.orig { color: var(--text); }
.arrow { color: var(--muted); }
.corr { color: var(--ok); font-weight: 500; }
.msg { color: var(--muted); }
.no-errors { color: var(--muted); font-size: var(--fs-sm); }

/* 审计 F4：历史详情块矩阵回放（与 StageProgress 矩阵同口径；旧记录无句表时为 cm-empty 提示） */
.cm-table { border-collapse: collapse; width: 100%; font-size: 11.5px; margin: var(--space-2) 0; }
.cm-table th, .cm-table td { border-bottom: 1px solid var(--border); border-right: 1px solid var(--border); padding: 3px 8px; text-align: center; white-space: nowrap; }
.cm-table th:last-child, .cm-table td:last-child { border-right: none; }
.cm-table th { background: var(--surface-2); color: var(--text-2); font-weight: 600; }
.cm-prev, td.cm-prev { text-align: left; max-width: 200px; overflow: hidden; text-overflow: ellipsis; }
.cm-idx, td.cm-idx { color: var(--muted); }
.cm-origin { color: var(--muted); font-size: 10.5px; }
.cm-cell { font-weight: 600; }
.cm-cell.cm-hit { color: var(--primary); background: color-mix(in srgb, var(--primary) 10%, transparent); }
.cm-cell.cm-pass { color: var(--ok); }
.cm-cell.cm-skipped, .cm-cell.cm-pending { color: var(--muted); opacity: 0.6; }
.cm-empty { padding: 6px 2px; font-size: 11.5px; color: var(--muted); }

.pager { display: flex; align-items: center; gap: var(--space-2); }
.pager-text { color: var(--muted); font-size: var(--fs-xs); white-space: nowrap; }
.foot { display: flex; justify-content: flex-end; }
</style>
