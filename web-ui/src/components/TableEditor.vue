<script setup lang="ts">
/**
 * 决策表编辑器
 *
 * - 列表：真实元数据（规则数 + 加载/更新时间）
 * - 矩阵视图：结构化编辑（分组 / 搜索 / 增删改 / 排序 / 变更高亮）
 * - JSON 视图：高级模式（原始 JDM 文本）
 * - 保存前 dry-run 校验（POST validate）+ 变更确认；保存即热更新并自动存版本
 * - 版本历史：一键回滚（POST rollback）
 */
import { ref, computed } from 'vue'
import { errMsg } from '../lib/err'

import { ElMessage, ElMessageBox } from 'element-plus'
import { api } from '../api/client'
import type {
  AttributionResult,
  DecisionTableMeta,
  DecisionTableView,
  ValidateResponse,
} from '../types'
import { WEINER_LABELS } from '../types'
import {
  buildTryContext,
  diffTables,
  parseTable,
  serializeTable,
  validateModel,
  type ModelIssue,
  type RowModel,
  type TableModel,
  type TryContext,
} from '../lib/jdm'
import { formatWhen, highlightJson } from '../lib/format'
import MatrixView from './table/MatrixView.vue'
import RowEditor from './table/RowEditor.vue'
import VersionHistory from './table/VersionHistory.vue'

// ─── 列表 ───
const tables = ref<DecisionTableMeta[]>([])
const loadingList = ref(false)

async function loadTables() {
  loadingList.value = true
  try {
    tables.value = await api.listTables()
  } catch (e) {
    ElMessage.error(`加载决策表列表失败：${errMsg(e)}`)
  } finally {
    loadingList.value = false
  }
}

// ─── 当前表 ───
const activeLang = ref('')
const base = ref<TableModel | null>(null)
const model = ref<TableModel | null>(null)
const view = ref<'matrix' | 'json'>('matrix')
const jsonDraft = ref('')
const status = ref('')
const parseError = ref('')
const saving = ref(false)
const validating = ref(false)
const validateRes = ref<ValidateResponse | null>(null)

const search = ref('')
const groupMode = ref<'grammarPoint' | 'errorType' | 'none'>('grammarPoint')

const editorRow = ref<RowModel | null>(null)
const editorIsNew = ref(false)
const versionsVisible = ref(false)

const diff = computed(() => (base.value && model.value ? diffTables(base.value, model.value) : null))

// ─── 未保存变更保护 ───

async function confirmDiscard(): Promise<boolean> {
  if (!diff.value?.hasChanges) return true
  try {
    await ElMessageBox.confirm(
      '当前表有未保存变更，继续操作将丢失这些变更，确认继续？',
      '未保存变更',
      { type: 'warning', confirmButtonText: '放弃变更', cancelButtonText: '取消' },
    )
    return true
  } catch {
    return false
  }
}

async function openTable(lang: string) {
  if (activeLang.value === lang) return
  if (!(await confirmDiscard())) return
  activeLang.value = lang
  status.value = ''
  parseError.value = ''
  validateRes.value = null
  try {
    const viewRes: DecisionTableView = await api.getTable(lang)
    const parsed = parseTable(viewRes.raw, lang)
    base.value = parsed
    model.value = JSON.parse(JSON.stringify(parsed))
    jsonDraft.value = viewRes.raw
  } catch (e) {
    parseError.value = `解析失败：${errMsg(e)}`
    base.value = null
    model.value = null
  }
}

// ─── 行操作 ───

function addRow() {
  if (!model.value) return
  const row: RowModel = {
    key: `new_${Date.now()}`,
    conds: Object.fromEntries(model.value.inputs.map((i) => [i.id, null])),
    outputs: Object.fromEntries(model.value.outputs.map((o) => [o.id, ''])),
  }
  editorIsNew.value = true
  editorRow.value = row
}

function editRow(row: RowModel) {
  editorIsNew.value = false
  editorRow.value = row
}

function saveRow(row: RowModel) {
  if (!model.value) return
  const idx = model.value.rows.findIndex((r) => r.key === row.key)
  if (idx >= 0) {
    model.value.rows[idx] = row
  } else {
    model.value.rows.push(row)
  }
  editorRow.value = null
}

async function removeRow(row: RowModel) {
  if (!model.value) return
  try {
    await ElMessageBox.confirm(`删除规则 ${row.key}？（保存后生效）`, '确认删除', {
      type: 'warning',
    })
  } catch {
    return
  }
  model.value.rows = model.value.rows.filter((r) => r.key !== row.key)
}

function moveRow(row: RowModel, dir: -1 | 1) {
  if (!model.value) return
  const rows = model.value.rows
  const i = rows.findIndex((r) => r.key === row.key)
  const j = i + dir
  if (i < 0 || j < 0 || j >= rows.length) return
  ;[rows[i], rows[j]] = [rows[j], rows[i]]
}

// ─── 校验 / 保存 ───

const clientIssues = ref<ModelIssue[]>([])
const focusKey = ref('')

function locateRow(key: string) {
  view.value = 'matrix'
  search.value = key
  focusKey.value = key
}

async function validate() {
  if (!model.value) return
  validating.value = true
  validateRes.value = null
  clientIssues.value = validateModel(model.value)
  try {
    validateRes.value = await api.validateTable(activeLang.value, serializeTable(model.value))
  } catch (e) {
    validateRes.value = { ok: false, errors: [`校验请求失败：${errMsg(e)}`] }
  } finally {
    validating.value = false
  }
}

// ─── 行级试算（按本行条件对当前内存表求值） ───

const tryRow = ref<RowModel | null>(null)
const tryVisible = ref(false)
const tryCtx = ref<TryContext | null>(null)
const tryResult = ref<AttributionResult | null>(null)
const tryMatched = ref('')
const trying = ref(false)

function openTry(row: RowModel) {
  tryRow.value = row
  tryCtx.value = buildTryContext(row)
  tryResult.value = null
  tryMatched.value = ''
  tryVisible.value = true
}
async function doTry() {
  if (!tryRow.value || !tryCtx.value || !activeLang.value) return
  trying.value = true
  tryResult.value = null
  try {
    const res = await api.tryTable(activeLang.value, tryCtx.value)
    tryResult.value = res
    tryMatched.value = res.trace.matched_rule
    if (res.trace.matched_rule === tryRow.value.key) {
      ElMessage.success(`试算命中本行（${tryRow.value.key}）`)
    } else {
      ElMessage.warning(`试算命中了其他规则：${res.trace.matched_rule}（本行：${tryRow.value.key}）——检查条件优先级 / hitPolicy`)
    }
  } catch (e) {
    ElMessage.error(`试算失败：${errMsg(e)}`)
  } finally {
    trying.value = false
  }
}

async function save() {
  if (!model.value) return
  const d = diff.value
  if (d && !d.hasChanges) {
    ElMessage.info('没有变更')
    return
  }
  try {
    await ElMessageBox.confirm(
      d
        ? `将保存变更：新增 ${d.added.length} 行 · 修改 ${d.changed.length} 行 · 删除 ${d.removed.length} 行。\n保存后立即热更新生效，并自动存入版本历史。`
        : '保存后立即热更新生效，并自动存入版本历史。',
      '保存确认',
      { type: 'warning', confirmButtonText: '保存', cancelButtonText: '取消' },
    )
  } catch {
    return
  }
  saving.value = true
  status.value = ''
  try {
    const meta = await api.putTable(activeLang.value, serializeTable(model.value))
    status.value = `已保存 ${meta.lang} 决策表（${meta.updated_at}，${meta.rule_count ?? model.value.rows.length} 行）`
    // 基线推进为当前模型（空白规范化差异不影响语义比较）
    base.value = JSON.parse(JSON.stringify(model.value))
    await loadTables()
  } catch (e) {
    status.value = `保存失败：${errMsg(e)}（当前表未变）`
  } finally {
    saving.value = false
  }
}

// ─── JSON 高级视图 ───

function applyJson() {
  try {
    const parsed = parseTable(jsonDraft.value, activeLang.value)
    model.value = parsed
    parseError.value = ''
    ElMessage.success(`已应用 JSON（${parsed.rows.length} 行）`)
  } catch (e) {
    parseError.value = `JSON 解析失败：${errMsg(e)}`
  }
}
function syncJson() {
  if (model.value) jsonDraft.value = serializeTable(model.value)
}
function formatJson() {
  try {
    jsonDraft.value = JSON.stringify(JSON.parse(jsonDraft.value), null, 2)
  } catch (e) {
    parseError.value = `JSON 格式化失败：${errMsg(e)}`
  }
}
function minifyJson() {
  try {
    jsonDraft.value = JSON.stringify(JSON.parse(jsonDraft.value))
  } catch (e) {
    parseError.value = `JSON 压缩失败：${errMsg(e)}`
  }
}
/** JSON 高亮层与编辑层滚动同步 */
function syncJsonScroll(e: Event) {
  const ta = e.target as HTMLElement
  const pre = ta.parentElement?.querySelector<HTMLPreElement>('.json-hl')
  if (pre) {
    pre.scrollTop = ta.scrollTop
    pre.scrollLeft = ta.scrollLeft
  }
}
function onVersionsChanged() {
  // 回滚后重新拉取最新表（若有未保存编辑，先确认丢弃）
  if (activeLang.value) void reopenAfterRollback()
}
async function reopenAfterRollback() {
  if (!(await confirmDiscard())) return
  const lang = activeLang.value
  if (!lang) return
  activeLang.value = '' // 强制重新走 openTable 加载
  openTable(lang)
}

loadTables()
</script>

<template>
  <el-card shadow="never">
    <template #header>
      <div class="card-header">
        <span class="card-title">决策表列表</span>
        <span class="sub">结构化编辑 · dry-run 校验 · 版本回滚</span>
        <span class="spacer"></span>
        <el-button size="small" :loading="loadingList" @click="loadTables()">刷新</el-button>
      </div>
    </template>
    <el-table :data="tables" empty-text="暂无决策表（检查 GEC_TABLES_DIR 配置）">
      <el-table-column prop="lang" label="语言" width="90" />
      <el-table-column prop="rule_count" label="规则数" width="100" />
      <el-table-column label="加载 / 更新时间" min-width="140">
        <template #default="{ row }">
          <span :title="row.updated_at">{{ formatWhen(row.updated_at) }}</span>
        </template>
      </el-table-column>
      <el-table-column label="操作" width="100">
        <template #default="{ row }">
          <el-button size="small" type="primary" plain @click="openTable(row.lang)">编辑</el-button>
        </template>
      </el-table-column>
    </el-table>
  </el-card>

  <el-card v-if="activeLang" shadow="never" class="editor-card">
    <template #header>
      <div class="card-header">
        <span class="card-title">编辑 {{ activeLang }} 决策表</span>
        <span class="sub" v-if="model">{{ model.rows.length }} 行</span>
        <span class="spacer"></span>
        <el-radio-group v-model="view" size="small">
          <el-radio-button value="matrix">规则列表</el-radio-button>
          <el-radio-button value="json">JSON</el-radio-button>
        </el-radio-group>
        <el-button size="small" :loading="validating" @click="validate()">校验</el-button>
        <el-button size="small" @click="versionsVisible = true">版本历史</el-button>
        <el-button size="small" type="primary" :loading="saving" @click="save()">保存并热更新</el-button>
      </div>
    </template>

    <div v-if="parseError" class="parse-error">{{ parseError }}</div>

    <div v-if="model" class="editor-toolbar">
      <el-input
        v-model="search"
        placeholder="搜索规则…"
        clearable
        style="width: 320px"
      />
      <el-select v-model="groupMode" style="width: 150px">
        <el-option label="按知识点分组" value="grammarPoint" />
        <el-option label="按错误类型分组" value="errorType" />
        <el-option label="不分组" value="none" />
      </el-select>
      <span class="spacer"></span>
      <el-popover v-if="diff?.hasChanges" trigger="click" :width="340">
        <template #reference>
          <span class="diff-ref">
            未保存变更：+{{ diff.added.length }} ~{{ diff.changed.length }} −{{ diff.removed.length }} ▾
          </span>
        </template>
        <div class="diff-detail">
          <div v-if="diff.added.length > 0">
            <div class="diff-sec diff-add">新增 {{ diff.added.length }}</div>
            <ul>
              <li v-for="r in diff.added" :key="r.key" class="mono">{{ r.key }}</li>
            </ul>
          </div>
          <div v-if="diff.changed.length > 0">
            <div class="diff-sec diff-mod">修改 {{ diff.changed.length }}</div>
            <ul>
              <li v-for="c in diff.changed" :key="c.after.key" class="mono">{{ c.after.key }}</li>
            </ul>
          </div>
          <div v-if="diff.removed.length > 0">
            <div class="diff-sec diff-del">删除 {{ diff.removed.length }}</div>
            <ul>
              <li v-for="r in diff.removed" :key="r.key" class="mono">{{ r.key }}</li>
            </ul>
          </div>
        </div>
      </el-popover>
    </div>

    <MatrixView
      v-if="view === 'matrix' && model"
      :model="model"
      :base="base"
      :search="search"
      :group-mode="groupMode"
      :focus-key="focusKey"
      @edit="editRow"
      @add="addRow"
      @remove="removeRow"
      @move="moveRow"
      @try="openTry"
    />

    <div v-else class="json-view">
      <div class="json-toolbar">
        <el-button size="small" @click="syncJson">从矩阵生成 JSON</el-button>
        <el-button size="small" @click="formatJson">格式化</el-button>
        <el-button size="small" @click="minifyJson">压缩</el-button>
        <span class="spacer"></span>
        <el-button size="small" type="primary" @click="applyJson">应用 JSON 到矩阵</el-button>
      </div>
      <div class="json-edit">
        <pre class="json-hl" aria-hidden="true"><code v-html="highlightJson(jsonDraft)"></code><span>&#10;</span></pre>
        <textarea
          v-model="jsonDraft"
          class="json-src"
          spellcheck="false"
          @scroll="syncJsonScroll"
        ></textarea>
      </div>
    </div>

    <div v-if="validateRes || clientIssues.length" class="validate-box" :class="validateRes && !validateRes.ok ? 'err' : 'ok'">
      <strong v-if="validateRes">{{ validateRes.ok ? '✓ 校验通过' : '✗ 校验未通过' }}</strong>
      <ul v-if="validateRes && validateRes.errors.length">
        <li v-for="(e, i) in validateRes.errors" :key="i">{{ e }}</li>
      </ul>
      <div v-if="clientIssues.length" class="client-issues">
        <div class="diff-sec">客户端预检问题（{{ clientIssues.length }}）</div>
        <ul>
          <li v-for="(iss, i) in clientIssues" :key="i">
            {{ iss.message }}
            <el-button v-if="iss.key" link size="small" type="primary" @click="locateRow(iss.key)">定位</el-button>
          </li>
        </ul>
      </div>
    </div>

    <p v-if="status" class="status" :class="{ error: status.includes('失败') }">{{ status }}</p>
  </el-card>

  <el-dialog v-model="tryVisible" :title="`试算：${tryRow?.key ?? ''}`" width="560px">
    <div v-if="tryRow" class="try-body">
      <div v-if="!tryCtx" class="sub">
        本行缺少 errorType == 条件，无法构造试算上下文（请在「编辑」中补全条件）
      </div>
      <template v-else>
        <div class="try-ctx">
          <span class="mono">{{ tryCtx.error_type }}</span>
          <span>· 置信度 {{ tryCtx.confidence }}</span>
          <span>· 历史错误数 {{ tryCtx.session_error_count }}</span>
          <span v-if="tryCtx.grammar_point">· 知识点 {{ tryCtx.grammar_point }}</span>
        </div>
        <div v-if="tryResult" class="try-result">
          <div>
            命中规则：<span class="mono">{{ tryMatched }}</span>
            <el-tag v-if="tryMatched === tryRow.key" type="success" size="small">本行</el-tag>
            <el-tag v-else type="warning" size="small">其他规则</el-tag>
          </div>
          <div>
            归因：{{ tryResult.attribution }}
            <span class="sub">
              （{{ WEINER_LABELS[tryResult.weiner.locus] }} /
              {{ WEINER_LABELS[tryResult.weiner.stability] }} /
              {{ WEINER_LABELS[tryResult.weiner.controllability] }}）
            </span>
          </div>
          <ul v-if="tryResult.remedial.length">
            <li v-for="(r, i) in tryResult.remedial" :key="i">{{ r }}</li>
          </ul>
        </div>
        <div v-else class="sub">尚未试算</div>
      </template>
    </div>
    <template #footer>
      <el-button @click="tryVisible = false">关闭</el-button>
      <el-button type="primary" :loading="trying" :disabled="!tryCtx" @click="doTry">
        试算
      </el-button>
    </template>
  </el-dialog>

  <RowEditor
    :model="model ?? ({} as TableModel)"
    :row="editorRow"
    :is-new="editorIsNew"
    @close="editorRow = null"
    @save="saveRow"
  />
  <VersionHistory
    v-if="activeLang"
    :lang="activeLang"
    :visible="versionsVisible"
    @close="versionsVisible = false"
    @changed="onVersionsChanged"
  />
</template>

<style scoped>
.editor-card { margin-top: 14px; }
.editor-toolbar {
  display: flex; align-items: center; gap: 10px;
  margin-bottom: 10px;
}
.editor-toolbar .spacer { flex: 1; }
.parse-error {
  background: rgba(220, 80, 80, 0.08); border: 1px solid rgba(220, 80, 80, 0.3);
  border-radius: 8px; padding: 8px 12px; margin-bottom: 10px; font-size: 13px; color: #c05252;
}
.json-view { display: flex; flex-direction: column; gap: 8px; }
.json-toolbar { display: flex; gap: 8px; align-items: center; }
.json-toolbar .spacer { flex: 1; }
/* 高亮层 + 透明编辑层叠加（经典 overlay 方案：两层同字体 / 行高 / 内边距） */
.json-edit { position: relative; height: 480px; border: 1px solid var(--border); border-radius: 8px; overflow: hidden; }
.json-edit .json-hl,
.json-edit .json-src {
  position: absolute; inset: 0; width: 100%; height: 100%;
  margin: 0; padding: 10px 12px;
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 12.5px; line-height: 1.55;
  white-space: pre; tab-size: 2;
}
.json-edit .json-hl { overflow: hidden; color: #333; pointer-events: none; }
.json-edit .json-src {
  overflow: auto; background: var(--surface); color: transparent; caret-color: #333;
  border: none; outline: none; resize: none;
}
.json-edit .json-src::selection { background: rgba(64, 128, 255, 0.18); }
.json-hl .j-key { color: #0550ae; }
.json-hl .j-str { color: #0a7d33; }
.json-hl .j-num { color: #b58900; }
.json-hl .j-kw { color: #c05252; }
.diff-ref { cursor: pointer; font-size: 12.5px; color: #a06a00; white-space: nowrap; }
.diff-detail ul { margin: 4px 0 10px; padding-left: 16px; max-height: 200px; overflow: auto; }
.diff-detail li { font-size: 12px; line-height: 1.7; word-break: break-all; }
.diff-sec { font-size: 12px; font-weight: 600; color: var(--muted); }
.diff-add { color: #2f8a4f; }
.diff-mod { color: #a06a00; }
.diff-del { color: #c05252; }
.client-issues { margin-top: 8px; }
.try-body { display: flex; flex-direction: column; gap: 10px; font-size: 13px; }
.try-ctx { display: flex; gap: 12px; flex-wrap: wrap; color: var(--muted); font-size: 12.5px; }
.try-result ul { margin: 4px 0 0; padding-left: 18px; }
.validate-box {
  margin-top: 12px; border-radius: 8px; padding: 8px 12px; font-size: 13px;
}
.validate-box.ok { background: rgba(82, 196, 124, 0.08); border: 1px solid rgba(82, 196, 124, 0.3); }
.validate-box.err { background: rgba(220, 80, 80, 0.08); border: 1px solid rgba(220, 80, 80, 0.3); }
.validate-box ul { margin: 6px 0 0; padding-left: 18px; }
.status { margin-top: 10px; font-size: 13px; }
.status.error { color: #c05252; }
</style>