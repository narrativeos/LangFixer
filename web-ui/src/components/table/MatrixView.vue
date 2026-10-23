<script setup lang="ts">
/**
 * 决策表规则列表视图（只读展示 + 行操作入口）
 *
 * - 条件列按输入渲染人读化表达式（`= spelling` / `≥ 0.9` / `任意`），表头用中文列名
 * - 输出列：matchedRule（规则 ID）+ 其余输出（单行截断，悬停看全文）
 * - 智能分组：知识点/错误类型取值过多（组数 > 40 且组均 < 2 行）时自动回退不分组
 * - 列管理：勾选显隐；`#` 列固定左侧、操作列固定右侧
 * - 大表（> 300 行）窗口化渲染：只渲染可视区 ± 缓冲行，上下用占位行撑高
 * - 搜索过滤；相对 base 的变更行高亮（新增 / 修改）
 * - 行内「试算」：按本行条件对当前内存表求值（TableEditor 调 POST try）
 */
import { computed, ref, watch } from 'vue'
import {
  columnLabel,
  diffTables,
  humanizeCell,
  smartGroups,
  type CondCell,
  type RowModel,
  type TableDiff,
  type TableModel,
} from '../../lib/jdm'

const props = defineProps<{
  model: TableModel
  /** 加载时的基线模型（用于变更高亮；null = 无基线） */
  base: TableModel | null
  search: string
  groupMode: 'grammarPoint' | 'errorType' | 'none'
  /** 外部定位请求：设为某行键 → 展开其所在组 */
  focusKey?: string
}>()

const emit = defineEmits<{
  edit: [row: RowModel]
  add: []
  remove: [row: RowModel]
  move: [row: RowModel, dir: -1 | 1]
  try: [row: RowModel]
}>()

const ROW_H = 34
const VIEW_H = 560
const VIRTUAL_THRESHOLD = 300

const expanded = ref<Record<string, boolean>>({})
const colVisible = ref<Record<string, boolean>>({})
const scrollTop = ref(0)

/** 列显隐初始化为全显（切换表时重置） */
function initColVisible() {
  const v: Record<string, boolean> = {}
  for (const c of props.model.inputs) v[c.id] = true
  for (const c of props.model.outputs) v[c.id] = true
  colVisible.value = v
}
initColVisible()
watch(
  () => props.model.lang,
  () => {
    initColVisible()
    expanded.value = {}
    scrollTop.value = 0
  },
)

const diff = computed<TableDiff | null>(() =>
  props.base ? diffTables(props.base, props.model) : null,
)
const changedKeys = computed(() => new Set(diff.value?.changed.map((c) => c.after.key) ?? []))
const addedKeys = computed(() => new Set(diff.value?.added.map((r) => r.key) ?? []))

/** 列顺序：条件列（输入序）→ matchedRule → 其余输出（均按显隐过滤） */
const condCols = computed(() => props.model.inputs.filter((c) => colVisible.value[c.id] !== false))
const outCols = computed(() => {
  const all = props.model.outputs
  const matched = all.find((o) => o.id === 'matchedRule')
  const rest = all.filter((o) => o.id !== 'matchedRule')
  const ordered = matched ? [matched, ...rest] : rest
  return ordered.filter((c) => colVisible.value[c.id] !== false)
})
/** 全部可勾选列（条件 + 输出） */
const allCols = computed(() => [...props.model.inputs, ...props.model.outputs])

/** 智能分组（取值过多自动回退不分组） */
const smart = computed(() => smartGroups(props.model, props.groupMode))

function rowText(r: RowModel): string {
  const parts: string[] = [r.key]
  for (const c of Object.values(r.conds)) if (c) parts.push(c.value)
  for (const v of Object.values(r.outputs)) if (v) parts.push(v)
  return parts.join(' ')
}

/** 过滤后的组（搜索命中行才保留） */
const groups = computed(() => {
  const g = smart.value.groups
  const q = props.search.trim().toLowerCase()
  if (!q) return g
  return g
    .map((grp) => ({
      ...grp,
      rows: grp.rows.filter((r) => rowText(r).toLowerCase().includes(q)),
    }))
    .filter((grp) => grp.rows.length > 0)
})

/** 小表（≤30 行）默认全展开 */
const small = computed(() => props.model.rows.length <= 30)
function isExpanded(name: string): boolean {
  return expanded.value[name] ?? small.value
}
function toggle(name: string) {
  expanded.value[name] = !isExpanded(name)
}
function expandAll(open: boolean) {
  const next: Record<string, boolean> = {}
  for (const g of groups.value) next[g.name] = open
  expanded.value = next
}
/** 外部定位：展开目标行所在组 */
watch(
  () => props.focusKey,
  (k) => {
    if (!k) return
    for (const g of groups.value) {
      if (g.rows.some((r) => r.key === k)) {
        expanded.value[g.name] = true
        break
      }
    }
  },
)

// ─── 窗口化渲染 ───

type FlatRow =
  | { kind: 'group'; name: string; rows: RowModel[] }
  | { kind: 'row'; row: RowModel; idx: number }

const flat = computed<FlatRow[]>(() => {
  const out: FlatRow[] = []
  for (const g of groups.value) {
    out.push({ kind: 'group', name: g.name, rows: g.rows })
    if (isExpanded(g.name)) {
      g.rows.forEach((r, i) => out.push({ kind: 'row', row: r, idx: i }))
    }
  }
  return out
})
const virtual = computed(() => flat.value.length > VIRTUAL_THRESHOLD)
const visible = computed(() => {
  const all = flat.value
  if (!virtual.value) return all
  const start = Math.max(0, Math.floor(scrollTop.value / ROW_H) - 5)
  const end = Math.min(all.length, start + Math.ceil(VIEW_H / ROW_H) + 10)
  return all.slice(start, end)
})
const topPad = computed(() =>
  virtual.value ? Math.max(0, Math.floor(scrollTop.value / ROW_H) - 5) * ROW_H : 0,
)
const bottomPad = computed(() =>
  virtual.value
    ? Math.max(0, flat.value.length - (topPad.value / ROW_H + visible.value.length)) * ROW_H
    : 0,
)
function onScroll(e: Event) {
  scrollTop.value = (e.target as HTMLElement).scrollTop
}

function rowClass(r: RowModel): string {
  if (addedKeys.value.has(r.key)) return 'row-added'
  if (changedKeys.value.has(r.key)) return 'row-changed'
  return ''
}

function displayCond(c: CondCell | null | undefined): string {
  return humanizeCell(c)
}
function condTitle(id: string, c: CondCell | null | undefined): string {
  return c ? `${columnLabel(id)}：${c.op} ${c.value}` : `${columnLabel(id)}：任意`
}
function displayOut(id: string, v: string | undefined): string {
  if (v === undefined || v === '') return '—'
  if (id === 'remedial' || id === 'examples' || id === 'source') {
    return v.length > 48 ? v.slice(0, 48) + '…' : v
  }
  return v
}
function fullOut(id: string, v: string | undefined): string {
  return v ?? ''
}
</script>

<template>
  <div class="matrix-wrap">
    <div class="matrix-toolbar">
      <el-button size="small" @click="emit('add')">+ 新增规则行</el-button>
      <el-button size="small" :disabled="groups.length <= 1" @click="expandAll(true)">全部展开</el-button>
      <el-button size="small" :disabled="groups.length <= 1" @click="expandAll(false)">全部折叠</el-button>
      <el-popover trigger="click" :width="210">
        <template #reference>
          <el-button size="small">列 ▾</el-button>
        </template>
        <div class="col-pick">
          <div class="col-pick-title">显示列</div>
          <el-checkbox v-for="c in allCols" :key="c.id" v-model="colVisible[c.id]">
            {{ columnLabel(c.id) }}
          </el-checkbox>
        </div>
      </el-popover>
      <span class="spacer"></span>
      <span class="sub" v-if="smart.ungrouped && smart.reason">{{ smart.reason }}</span>
      <span class="sub" v-if="diff?.hasChanges">
        变更：新增 {{ diff.added.length }} · 修改 {{ diff.changed.length }} · 删除 {{ diff.removed.length }}
      </span>
    </div>

    <div class="matrix-scroll" @scroll="onScroll">
      <table class="matrix">
        <thead>
          <tr>
            <th class="col-idx">#</th>
            <th v-for="c in condCols" :key="c.id" class="col-cond" :title="`${c.id}（${c.field}）`">
              {{ columnLabel(c.id) }}
            </th>
            <th v-for="c in outCols" :key="c.id" class="col-out" :title="`${c.id}（${c.field}）`">
              {{ columnLabel(c.id) }}
            </th>
            <th class="col-ops">操作</th>
          </tr>
        </thead>
        <tbody>
          <tr v-if="topPad > 0" class="spacer-row">
            <td :colspan="condCols.length + outCols.length + 2" :style="{ height: topPad + 'px' }"></td>
          </tr>
          <template v-for="fr in visible" :key="fr.kind === 'group' ? 'g:' + fr.name : 'r:' + fr.row.key">
            <tr v-if="fr.kind === 'group'" class="row-group" @click="toggle(fr.name)">
              <td :colspan="condCols.length + outCols.length + 2">
                <span class="group-arrow">{{ isExpanded(fr.name) ? '▾' : '▸' }}</span>
                <strong>{{ fr.name }}</strong>
                <span class="count" v-if="fr.rows.length > 1">{{ fr.rows.length }} 行</span>
              </td>
            </tr>
            <tr
              v-else
              class="row-rule"
              :class="rowClass(fr.row)"
              @dblclick="emit('edit', fr.row)"
            >
              <td class="col-idx" :title="'双击编辑'">{{ fr.idx + 1 }}</td>
              <td
                v-for="c in condCols"
                :key="c.id"
                class="col-cond"
                :title="condTitle(c.id, fr.row.conds[c.id])"
              >
                {{ displayCond(fr.row.conds[c.id]) }}
              </td>
              <td v-for="c in outCols" :key="c.id" class="col-out" :title="fullOut(c.id, fr.row.outputs[c.id])">
                <span v-if="c.id === 'matchedRule'" class="mono">{{ fr.row.outputs[c.id] || '—' }}</span>
                <span v-else>{{ displayOut(c.id, fr.row.outputs[c.id]) }}</span>
              </td>
              <td class="col-ops">
                <el-button link size="small" @click="emit('try', fr.row)">试算</el-button>
                <el-button link size="small" @click="emit('edit', fr.row)">编辑</el-button>
                <el-button link size="small" @click="emit('move', fr.row, -1)">↑</el-button>
                <el-button link size="small" @click="emit('move', fr.row, 1)">↓</el-button>
                <el-button link size="small" type="danger" @click="emit('remove', fr.row)">删除</el-button>
              </td>
            </tr>
          </template>
          <tr v-if="bottomPad > 0" class="spacer-row">
            <td :colspan="condCols.length + outCols.length + 2" :style="{ height: bottomPad + 'px' }"></td>
          </tr>
          <tr v-if="flat.length === 0">
            <td :colspan="condCols.length + outCols.length + 2" class="empty">无匹配行</td>
          </tr>
        </tbody>
      </table>
    </div>
  </div>
</template>
<style scoped>
.matrix-wrap { border: 1px solid var(--border); border-radius: 8px; overflow: hidden; }
.matrix-toolbar {
  display: flex; align-items: center; gap: 8px; flex-wrap: wrap;
  padding: 8px 10px; background: var(--surface-2); border-bottom: 1px solid var(--border);
}
.matrix-toolbar .spacer { flex: 1; }
.matrix-scroll { max-height: 560px; overflow: auto; }
.matrix { width: 100%; border-collapse: collapse; font-size: 12.5px; table-layout: fixed; }
.matrix th {
  position: sticky; top: 0; z-index: 1;
  background: var(--surface-2); text-align: left;
  height: 34px; padding: 0 8px; border-bottom: 1px solid var(--border);
  white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
}
.matrix td {
  height: 34px; padding: 0 8px; border-bottom: 1px solid var(--border);
  vertical-align: middle; white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
  background: var(--surface);
}
.col-idx { width: 44px; color: var(--muted); text-align: center; }
.matrix th.col-idx, .matrix td.col-idx { position: sticky; left: 0; z-index: 2; }
.matrix th.col-idx { z-index: 3; background: var(--surface-2); }
.matrix td.col-idx { background: var(--surface); }
.col-cond { width: 130px; }
.col-out { width: 150px; }
.col-ops { width: 208px; white-space: nowrap; }
.matrix td.col-ops {
  position: sticky; right: 0; z-index: 2; background: var(--surface);
  box-shadow: -8px 0 8px -8px rgba(0, 0, 0, 0.08);
}
.matrix th.col-ops { position: sticky; right: 0; z-index: 3; background: var(--surface-2); }
.mono { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12px; }
.row-group td { background: var(--surface-2); cursor: pointer; user-select: none; }
.row-group:hover td { background: var(--surface-3, var(--surface-2)); }
.group-arrow { display: inline-block; width: 16px; color: var(--muted); }
.row-group .count { color: var(--muted); font-size: 12px; margin-left: 8px; }
.row-rule:hover td { background: var(--surface-2); }
.row-added td { background: rgba(82, 196, 124, 0.10); }
.row-changed td { background: rgba(232, 178, 60, 0.10); }
.spacer-row td { padding: 0; border-bottom: none; background: transparent; }
.empty { text-align: center; color: var(--muted); padding: 24px; height: auto; }
.col-pick { display: flex; flex-direction: column; gap: 2px; max-height: 320px; overflow: auto; }
.col-pick-title { font-size: 12px; color: var(--muted); margin-bottom: 4px; }
.col-pick .el-checkbox { height: 26px; }
</style>