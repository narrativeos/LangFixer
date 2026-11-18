<script setup lang="ts">
/**
 * 规则行编辑器（行内展开面板）
 *
 * - 条件按列类型渲染：布尔（新手/老手/任意）、数字（操作符 + 数值）、
 *   字符串（操作符 + 下拉，选项来自该列现有取值；「自定义」可输入新值）
 * - 输出逐格编辑（matchedRule 必填且表内唯一；remedial / examples 用多行）
 * - 「试算」：按本行条件构造上下文，对**当前内存表**求值，
 *   展示命中规则与归因结果（改行 → 试算 → 确认的闭环）
 */
import { ref, watch, computed } from 'vue'
import { errMsg } from '../../lib/err'

import { api } from '../../api/client'
import { ATTRIBUTION_LABELS, type AttributionResult } from '../../types'
import {
  buildTryContext,
  COND_OPS,
  columnLabel,
  distinctValues,
  inferInputType,
  type CondCell,
  type CondOp,
  type RowModel,
  type TableModel,
  type TryContext,
} from '../../lib/jdm'
import { LButton, LCheckbox, LInput, LSelect, LTag, toast } from '../../ui'

const props = defineProps<{
  model: TableModel
  /** null = 关闭 */
  row: RowModel | null
  isNew: boolean
}>()

const emit = defineEmits<{
  close: []
  save: [row: RowModel]
}>()

const draft = ref<RowModel | null>(null)
const condOp = ref<Record<string, CondOp | ''>>({})
const condAny = ref<Record<string, boolean>>({})
/** 字符串列是否处于「自定义值」输入态（替代原下拉 allow-create） */
const condCustom = ref<Record<string, boolean>>({})

watch(
  () => props.row,
  (r) => {
    if (!r) {
      draft.value = null
      return
    }
    draft.value = JSON.parse(JSON.stringify(r))
    const ops: Record<string, CondOp | ''> = {}
    const any: Record<string, boolean> = {}
    for (const [id, c] of Object.entries(r.conds)) {
      ops[id] = c ? c.op : ''
      any[id] = !c
    }
    condOp.value = ops
    condAny.value = any
    condCustom.value = {}
  },
  { immediate: true, deep: true },
)

/** 条件列定义（含类型与下拉选项） */
const condCols = computed(() =>
  props.model.inputs.map((input) => ({
    input,
    type: inferInputType(input.id, props.model.rows),
    options: distinctValues(props.model.rows, input.id),
  })),
)

const outCols = computed(() =>
  props.model.outputs
    .filter((o) => o.id !== 'matchedRule')
    .map((o) => ({ output: o, multiline: o.id === 'remedial' || o.id === 'examples' })),
)

const boolOptions = [
  { value: 'true', label: '新手 (true)' },
  { value: 'false', label: '老手 (false)' },
]
const opOptions = COND_OPS.map((op) => ({ value: op, label: op }))

/** 字符串列下拉：现有取值 + 当前自定义值（若不在列表中） */
function textOptions(col: { options: string[]; input: { id: string } }) {
  const v = condValue(col.input.id)
  const opts = col.options.map((o) => ({ value: o, label: o }))
  if (v && !col.options.includes(v)) opts.push({ value: v, label: `${v}（自定义）` })
  return opts
}

function condValue(inputId: string): string {
  return draft.value?.conds[inputId]?.value ?? ''
}
function setCondValue(inputId: string, v: string) {
  if (!draft.value) return
  const c = draft.value.conds[inputId]
  if (c) c.value = v
}
function setOut(outputId: string, v: string) {
  if (!draft.value) return
  draft.value.outputs[outputId] = v
}
function setAny(inputId: string, any: boolean) {
  if (!draft.value) return
  if (any) {
    draft.value.conds[inputId] = null
  } else if (!draft.value.conds[inputId]) {
    draft.value.conds[inputId] = { op: condOp.value[inputId] || '==', value: condValue(inputId) }
  }
}

function validate(): string {
  if (!draft.value) return ''
  const key = (draft.value.outputs.matchedRule ?? '').trim()
  if (!key) return '规则 ID（matchedRule）必填'
  if (props.model.rows.some((r) => r.key === key && r !== draft.value)) {
    return `规则 ID "${key}" 在表中已存在（必须唯一）`
  }
  draft.value.key = key
  return ''
}

function save() {
  const err = validate()
  if (err) {
    toast(err, 'warn')
    return
  }
  emit('save', draft.value!)
}

// ─── 试算（对当前内存表） ───

const trying = ref(false)
const tryResult = ref<AttributionResult | null>(null)
const tryMatched = ref('')

/** 试算上下文：由本行条件构造（复用 jdm.buildTryContext；errorType 须为 ==） */
function buildTryCtx(): TryContext | null {
  return draft.value ? buildTryContext(draft.value) : null
}

async function tryRow() {
  const ctx = buildTryCtx()
  if (!ctx) {
    toast('试算需要 errorType 条件（= 某类型）；请先在条件中设置', 'warn')
    return
  }
  trying.value = true
  tryResult.value = null
  try {
    const res = await api.tryTable(props.model.lang, ctx)
    tryResult.value = res
    tryMatched.value = res.trace.matched_rule
    const myKey = draft.value?.key ?? ''
    if (res.trace.matched_rule === myKey) {
      toast(`试算命中本行（${myKey}）`, 'ok')
    } else {
      toast(`试算命中了其他规则：${res.trace.matched_rule}（本行：${myKey || '未命名'}）——检查条件优先级 / hitPolicy`, 'warn')
    }
  } catch (e) {
    tryResult.value = null
    toast(`试算失败：${errMsg(e)}`, 'danger')
  } finally {
    trying.value = false
  }
}
</script>

<template>
  <div v-if="draft" class="row-editor">
    <header class="re-head">
      <h3>{{ isNew ? '新增规则行' : `编辑规则 ${props.row?.key ?? ''}` }}</h3>
      <span class="spacer"></span>
      <LButton size="sm" @click="emit('close')">取消</LButton>
      <LButton size="sm" :loading="trying" @click="tryRow()">试算</LButton>
      <LButton size="sm" variant="primary" @click="save()">保存行</LButton>
    </header>

    <section>
      <h4>条件（输入列）</h4>
      <div v-for="col in condCols" :key="col.input.id" class="cond-row">
        <span class="cond-name" :title="col.input.field">{{ columnLabel(col.input.id) }}</span>
        <LCheckbox
          :model-value="!!condAny[col.input.id]"
          label="任意"
          @update:model-value="setAny(col.input.id, $event)"
        />
        <template v-if="!condAny[col.input.id]">
          <LSelect
            v-if="col.type === 'boolean'"
            :model-value="condValue(col.input.id)"
            :options="boolOptions"
            class="w-bool"
            @update:model-value="setCondValue(col.input.id, String($event))"
          />
          <template v-else>
            <LSelect
              :model-value="condOp[col.input.id]"
              :options="opOptions"
              class="w-op"
              @update:model-value="condOp[col.input.id] = String($event) as CondOp"
            />
            <LInput
              v-if="col.type === 'number'"
              :model-value="condValue(col.input.id)"
              class="w-num"
              @update:model-value="setCondValue(col.input.id, $event)"
            />
            <template v-else>
              <LSelect
                v-if="!condCustom[col.input.id]"
                :model-value="condValue(col.input.id)"
                :options="textOptions(col)"
                class="w-text"
                @update:model-value="setCondValue(col.input.id, String($event))"
              />
              <LInput
                v-else
                :model-value="condValue(col.input.id)"
                placeholder="输入自定义值"
                class="w-text"
                @update:model-value="setCondValue(col.input.id, $event)"
                @keyup.enter="condCustom[col.input.id] = false"
              />
              <LButton
                size="sm"
                variant="ghost"
                :title="condCustom[col.input.id] ? '改回下拉选择' : '输入不在列表中的值'"
                @click="condCustom[col.input.id] = !condCustom[col.input.id]"
              >
                {{ condCustom[col.input.id] ? '选择' : '自定义' }}
              </LButton>
            </template>
          </template>
        </template>
      </div>
    </section>

    <section>
      <h4>输出</h4>
      <div class="cond-row">
        <span class="cond-name">规则 ID</span>
        <LInput
          :model-value="draft?.outputs.matchedRule ?? ''"
          placeholder="唯一标识，如 spelling_high_conf_old_hand"
          class="w-key"
          @update:model-value="setOut('matchedRule', $event)"
        />
      </div>
      <div v-for="col in outCols" :key="col.output.id" class="cond-row">
        <span class="cond-name" :title="col.output.field">{{ columnLabel(col.output.id) }}</span>
        <textarea
          v-if="col.multiline"
          class="re-textarea"
          rows="2"
          :value="draft?.outputs[col.output.id] ?? ''"
          @input="setOut(col.output.id, ($event.target as HTMLTextAreaElement).value)"
        ></textarea>
        <LInput
          v-else
          :model-value="draft?.outputs[col.output.id] ?? ''"
          class="w-out"
          @update:model-value="setOut(col.output.id, $event)"
        />
      </div>
    </section>

    <section v-if="tryResult || trying">
      <h4>试算结果（当前内存表）</h4>
      <div v-if="trying" class="sub">正在试算…</div>
      <div v-else-if="tryResult" class="try-result">
        <p>
          命中规则：<span class="mono">{{ tryMatched }}</span>
          <LTag v-if="tryMatched === draft.key" tone="ok">= 本行</LTag>
        </p>
        <p>
          归因：<strong>{{ ATTRIBUTION_LABELS[tryResult.attribution] ?? tryResult.attribution }}</strong>
          <span class="sub">
            （{{ tryResult.weiner.locus }} / {{ tryResult.weiner.stability }} / {{ tryResult.weiner.controllability }}）
          </span>
        </p>
        <ul v-if="tryResult.remedial.length">
          <li v-for="(r, i) in tryResult.remedial" :key="i">{{ r }}</li>
        </ul>
      </div>
    </section>
  </div>
</template>

<style scoped>
.row-editor {
  display: flex; flex-direction: column; gap: var(--space-4);
  padding: var(--space-3); margin-bottom: var(--space-3);
  background: var(--surface-2);
  border: 1px solid var(--border); border-radius: var(--radius-sm);
}
.re-head { display: flex; align-items: center; gap: var(--space-2); }
.re-head h3 { margin: 0; font-size: var(--fs-lg); font-weight: 600; }
.re-head .spacer { flex: 1; }
.row-editor section h4 { margin: 0 0 var(--space-2); font-size: var(--fs-ui); color: var(--muted); }
.cond-row { display: flex; align-items: center; gap: var(--space-2); margin-bottom: var(--space-2); }
.cond-name { min-width: 110px; font-size: var(--fs-ui); }
.sub { color: var(--muted); font-size: var(--fs-sm); }
.w-bool { width: 140px; }
.w-op { width: 84px; }
.w-num { width: 120px; }
.w-text { min-width: 180px; }
.w-key { min-width: 260px; }
.w-out { flex: 1; min-width: 0; }
.re-textarea {
  flex: 1; min-width: 0;
  padding: var(--space-1) var(--space-2);
  font: inherit; font-size: var(--fs-ui); color: var(--text);
  background: var(--surface);
  border: 1px solid var(--border-strong); border-radius: var(--radius-ctl);
  resize: vertical;
}
.re-textarea:focus { border-color: var(--accent); outline: none; box-shadow: var(--focus-ring); }
.try-result { font-size: var(--fs-ui); }
.try-result p { margin: var(--space-1) 0; }
.try-result ul { margin: var(--space-1) 0 0; padding-left: var(--space-4); }
.mono { font-family: var(--font-mono); font-size: var(--fs-sm); }
</style>
