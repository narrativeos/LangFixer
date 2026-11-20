<script setup lang="ts">
/**
 * 决策表编辑器
 *
 * - 列表：真实元数据（规则数 + 加载/更新时间）
 * - 矩阵视图：结构化编辑（分组 / 搜索 / 增删改 / 排序 / 变更高亮）
 * - JSON 视图：高级模式（原始 JDM 文本）
 * - 保存前 dry-run 校验（POST validate）+ 保存确认对话框；保存即热更新并自动存版本
 * - 版本历史：一键回滚（POST rollback）
 */
import { ref, computed } from 'vue'
import { errMsg } from '../lib/err'

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
import {
  LButton,
  LDialog,
  LEmpty,
  LInput,
  LPopover,
  LSection,
  LSegmented,
  LSelect,
  LTable,
  LTag,
  toast,
} from '../ui'
import type { LColumn } from '../ui'

// ─── 列表 ───
const tables = ref<DecisionTableMeta[]>([])
const loadingList = ref(false)

const tableCols: LColumn<DecisionTableMeta>[] = [
  { key: 'lang', label: '语言', width: '90px' },
  { key: 'rule_count', label: '规则数', width: '100px' },
  { key: 'updated_at', label: '加载 / 更新时间' },
  { key: 'actions', label: '操作', width: '88px' },
]

async function loadTables() {
  loadingList.value = true
  try {
    tables.value = await api.listTables()
  } catch (e) {
    toast(`加载决策表列表失败：${errMsg(e)}`, 'danger')
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

const viewOptions: { value: 'matrix' | 'json'; label: string }[] = [
  { value: 'matrix', label: '规则列表' },
  { value: 'json', label: 'JSON' },
]
const groupOptions: { value: string; label: string }[] = [
  { value: 'grammarPoint', label: '按知识点分组' },
  { value: 'errorType', label: '按错误类型分组' },
  { value: 'none', label: '不分组' },
]

// ─── 未保存变更保护 ───
// 切换 / 重载不再弹窗确认：直接进行并以 toast 提供「撤销」恢复原内存态

type TableSnap = { lang: string; base: TableModel | null; model: TableModel | null; json: string }

function restoreSnap(s: TableSnap) {
  activeLang.value = s.lang
  base.value = s.base
  model.value = s.model
  jsonDraft.value = s.json
  validateRes.value = null
}

async function openTable(lang: string, force = false) {
  if (!force && activeLang.value === lang) return
  const snap: TableSnap | null = diff.value?.hasChanges
    ? { lang: activeLang.value, base: base.value, model: model.value, json: jsonDraft.value }
    : null
  activeLang.value = lang
  status.value = ''
  parseError.value = ''
  validateRes.value = null
  editorRow.value = null
  tryVisible.value = false
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
  if (snap) {
    toast('已放弃未保存的变更', 'warn', { label: '撤销', onClick: () => restoreSnap(snap) })
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

function removeRow(row: RowModel) {
  if (!model.value) return
  const idx = model.value.rows.indexOf(row)
  if (idx >= 0) model.value.rows.splice(idx, 1)
  toast(`已删除规则 ${row.key}（保存后生效）`, 'info', {
    label: '撤销',
    onClick: () => {
      if (idx >= 0 && model.value) model.value.rows.splice(idx, 0, row)
    },
  })
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

const saveConfirm = ref(false)
const saveDesc = computed(() => {
  const d = diff.value
  return d
    ? `将保存变更：新增 ${d.added.length} 行 · 修改 ${d.changed.length} 行 · 删除 ${d.removed.length} 行。保存后立即热更新生效，并自动存入版本历史。`
    : '保存后立即热更新生效，并自动存入版本历史。'
})

function save() {
  if (!model.value) return
  const d = diff.value
  if (d && !d.hasChanges) {
    toast('没有变更', 'info')
    return
  }
  saveConfirm.value = true
}

async function doSave() {
  if (!model.value) return
  saveConfirm.value = false
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
      toast(`试算命中本行（${tryRow.value.key}）`, 'ok')
    } else {
      toast(`试算命中了其他规则：${res.trace.matched_rule}（本行：${tryRow.value.key}）——检查条件优先级 / hitPolicy`, 'warn')
    }
  } catch (e) {
    toast(`试算失败：${errMsg(e)}`, 'danger')
  } finally {
    trying.value = false
  }
}

// ─── JSON 高级视图 ───

function applyJson() {
  try {
    const parsed = parseTable(jsonDraft.value, activeLang.value)
    model.value = parsed
    parseError.value = ''
    toast(`已应用 JSON（${parsed.rows.length} 行）`, 'ok')
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
  // 回滚后重新拉取最新表（若有未保存编辑，以 toast 撤销位兜底）
  if (activeLang.value) void reopenAfterRollback()
}
async function reopenAfterRollback() {
  const lang = activeLang.value
  if (!lang) return
  openTable(lang, true)
}

loadTables()
</script>

<template>
  <LSection title="决策表列表" desc="结构化编辑 · dry-run 校验 · 版本回滚">
    <template #actions>
      <LButton size="sm" :loading="loadingList" @click="loadTables()">刷新</LButton>
    </template>
    <LTable v-if="tables.length" :columns="tableCols" :rows="tables" row-key="lang" compact>
      <template #col-updated_at="{ row }">
        <span :title="row.updated_at">{{ formatWhen(row.updated_at) }}</span>
      </template>
      <template #col-actions="{ row }">
        <LButton size="sm" @click="openTable(row.lang)">编辑</LButton>
      </template>
    </LTable>
    <LEmpty v-else text="暂无决策表" hint="检查 GEC_TABLES_DIR 配置后点击「刷新」重试">
      <template #action>
        <LButton size="sm" :loading="loadingList" @click="loadTables()">刷新</LButton>
      </template>
    </LEmpty>
  </LSection>

  <LSection
    v-if="activeLang"
    class="editor-section"
    :title="`编辑 ${activeLang} 决策表`"
    :desc="model ? `${model.rows.length} 行` : ''"
  >
    <template #actions>
      <LSegmented v-model="view" :options="viewOptions" />
      <LButton size="sm" :loading="validating" @click="validate()">校验</LButton>
      <LButton size="sm" @click="versionsVisible = true">版本历史</LButton>
      <LButton size="sm" variant="primary" :loading="saving" @click="save()">保存并热更新</LButton>
    </template>

    <div v-if="parseError" class="parse-error">{{ parseError }}</div>

    <div v-if="model" class="editor-toolbar">
      <LInput v-model="search" placeholder="搜索规则…" class="search-input" />
      <LSelect
        :model-value="groupMode"
        :options="groupOptions"
        aria-label="分组方式"
        class="group-select"
        @update:model-value="groupMode = String($event) as typeof groupMode"
      />
      <span class="spacer"></span>
      <LPopover v-if="diff?.hasChanges" align="end">
        <template #trigger="{ toggle }">
          <button type="button" class="diff-ref" @click="toggle()">
            未保存变更：+{{ diff.added.length }} ~{{ diff.changed.length }} −{{ diff.removed.length }} ▾
          </button>
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
      </LPopover>
    </div>

    <div v-if="tryVisible && tryRow" class="try-panel">
      <div class="try-head">
        <strong>试算：{{ tryRow.key }}</strong>
        <span class="spacer"></span>
        <LButton size="sm" variant="ghost" @click="tryVisible = false">关闭</LButton>
        <LButton size="sm" variant="primary" :loading="trying" :disabled="!tryCtx" @click="doTry">试算</LButton>
      </div>
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
            <LTag v-if="tryMatched === tryRow.key" tone="ok">本行</LTag>
            <LTag v-else tone="warn">其他规则</LTag>
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

    <MatrixView
      v-if="view === 'matrix' && model && !editorRow"
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

    <RowEditor
      :model="model ?? ({} as TableModel)"
      :row="editorRow"
      :is-new="editorIsNew"
      @close="editorRow = null"
      @save="saveRow"
    />

    <div v-if="view === 'json' || !model" class="json-view">
      <div class="json-toolbar">
        <LButton size="sm" @click="syncJson">从矩阵生成 JSON</LButton>
        <LButton size="sm" @click="formatJson">格式化</LButton>
        <LButton size="sm" @click="minifyJson">压缩</LButton>
        <span class="spacer"></span>
        <LButton size="sm" variant="primary" @click="applyJson">应用 JSON 到矩阵</LButton>
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
            <LButton v-if="iss.key" size="sm" variant="ghost" @click="locateRow(iss.key)">定位</LButton>
          </li>
        </ul>
      </div>
    </div>

    <p v-if="status" class="status" :class="{ error: status.includes('失败') }">{{ status }}</p>
  </LSection>

  <LDialog v-model:visible="saveConfirm" title="保存确认">
    <p class="save-desc">{{ saveDesc }}</p>
    <template #footer>
      <LButton @click="saveConfirm = false">取消</LButton>
      <LButton variant="primary" :loading="saving" @click="doSave">保存</LButton>
    </template>
  </LDialog>

  <VersionHistory
    v-if="activeLang"
    :lang="activeLang"
    :visible="versionsVisible"
    @close="versionsVisible = false"
    @changed="onVersionsChanged"
  />
</template>

<style scoped>
.editor-section { margin-top: var(--space-4); }
.editor-toolbar {
  display: flex; align-items: center; gap: var(--space-2);
  margin-bottom: var(--space-3);
}
.editor-toolbar .spacer { flex: 1; }
.search-input { width: 320px; }
.group-select { width: 150px; }
.parse-error {
  background: var(--danger-soft); border: 1px solid var(--danger-line);
  border-radius: var(--radius-sm); padding: var(--space-2) var(--space-3);
  margin-bottom: var(--space-3); font-size: var(--fs-ui); color: var(--danger-ink);
}
.sub { color: var(--muted); font-size: var(--fs-sm); }
.try-panel {
  display: flex; flex-direction: column; gap: var(--space-2);
  padding: var(--space-3); margin-bottom: var(--space-3);
  background: var(--surface-2);
  border: 1px solid var(--border); border-radius: var(--radius-sm);
  font-size: var(--fs-ui);
}
.try-head { display: flex; align-items: center; gap: var(--space-2); }
.try-head .spacer { flex: 1; }
.try-ctx { display: flex; gap: var(--space-3); flex-wrap: wrap; color: var(--muted); font-size: var(--fs-sm); }
.try-result ul { margin: var(--space-1) 0 0; padding-left: var(--space-4); }
.json-view { display: flex; flex-direction: column; gap: var(--space-2); }
.json-toolbar { display: flex; gap: var(--space-2); align-items: center; }
.json-toolbar .spacer { flex: 1; }
/* 高亮层 + 透明编辑层叠加（经典 overlay 方案：两层同字体 / 行高 / 内边距） */
.json-edit {
  position: relative; height: 480px;
  border: 1px solid var(--border); border-radius: var(--radius-sm); overflow: hidden;
}
.json-edit .json-hl,
.json-edit .json-src {
  position: absolute; inset: 0; width: 100%; height: 100%;
  margin: 0; padding: var(--space-2) var(--space-3);
  font-family: var(--font-mono);
  font-size: var(--fs-sm); line-height: 1.55;
  white-space: pre; tab-size: 2;
}
.json-edit .json-hl { overflow: hidden; color: var(--text); pointer-events: none; }
.json-edit .json-src {
  overflow: auto; background: var(--surface); color: transparent; caret-color: var(--text);
  border: none; outline: none; resize: none;
}
.json-edit .json-src::selection { background: color-mix(in srgb, var(--accent) 18%, transparent); }
/* 语法着色只用中性层级色，颜色留给语义状态 */
.json-hl .j-key { color: var(--text); }
.json-hl .j-str { color: var(--text-2); }
.json-hl .j-num { color: var(--text-2); }
.json-hl .j-kw { color: var(--muted); }
.diff-ref {
  cursor: pointer; font: inherit; font-size: var(--fs-sm);
  color: var(--warn-ink); background: none; border: 0; padding: 0; white-space: nowrap;
}
.diff-detail ul { margin: var(--space-1) 0 var(--space-3); padding-left: var(--space-4); max-height: 200px; overflow: auto; }
.diff-detail li { font-size: var(--fs-sm); line-height: 1.7; word-break: break-all; }
.diff-sec { font-size: var(--fs-sm); font-weight: 600; color: var(--muted); }
.diff-add { color: var(--ok); }
.diff-mod { color: var(--warn-ink); }
.diff-del { color: var(--danger); }
.mono { font-family: var(--font-mono); font-size: var(--fs-sm); }
.client-issues { margin-top: var(--space-2); }
.validate-box {
  margin-top: var(--space-3); border-radius: var(--radius-sm);
  padding: var(--space-2) var(--space-3); font-size: var(--fs-ui);
}
.validate-box.ok { background: var(--ok-soft); border: 1px solid var(--ok-line); }
.validate-box.err { background: var(--danger-soft); border: 1px solid var(--danger-line); }
.validate-box ul { margin: var(--space-2) 0 0; padding-left: var(--space-4); }
.save-desc { margin: 0; font-size: var(--fs-ui); color: var(--text-2); }
.status { margin-top: var(--space-3); font-size: var(--fs-ui); }
.status.error { color: var(--danger); }
</style>
