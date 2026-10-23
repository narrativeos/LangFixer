<script setup lang="ts">
/**
 * 规则行编辑器（对话框）
 *
 * - 条件按列类型渲染：布尔（新手/老手/任意）、数字（操作符 + 数值）、
 *   字符串（操作符 + 下拉/自由输入，选项来自该列现有取值）
 * - 输出逐格编辑（matchedRule 必填且表内唯一；remedial / examples 用多行）
 * - 「试算」：按本行条件构造上下文，对**当前内存表**求值，
 *   展示命中规则与归因结果（改行 → 试算 → 确认的闭环）
 */
import { ref, watch, computed } from 'vue'
import { errMsg } from '../../lib/err'

import { ElMessage, ElMessageBox } from 'element-plus'
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
  },
  { immediate: true, deep: true },
)

const visible = computed(() => !!props.row)

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
    ElMessage.warning(err)
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
    ElMessage.warning('试算需要 errorType 条件（= 某类型）；请先在条件中设置')
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
      ElMessage.success(`试算命中本行（${myKey}）`)
    } else {
      ElMessage.warning(`试算命中了其他规则：${res.trace.matched_rule}（本行：${myKey || '未命名'}）——检查条件优先级 / hitPolicy`)
    }
  } catch (e) {
    tryResult.value = null
    ElMessage.error(`试算失败：${errMsg(e)}`)
  } finally {
    trying.value = false
  }
}
</script>

<template>
  <el-dialog
    :model-value="visible"
    @update:model-value="(v: boolean) => !v && emit('close')"
    :title="isNew ? '新增规则行' : `编辑规则 ${props.row?.key ?? ''}`"
    width="720px"
    :close-on-click-modal="false"
  >
    <div v-if="draft" class="row-editor">
      <section>
        <h4>条件（输入列）</h4>
        <div v-for="col in condCols" :key="col.input.id" class="cond-row">
          <span class="cond-name" :title="col.input.field">{{ columnLabel(col.input.id) }}</span>
          <el-checkbox
            :model-value="condAny[col.input.id]"
            @change="(v: string | number | boolean) => setAny(col.input.id, v === true)"
          >
            任意
          </el-checkbox>
          <template v-if="!condAny[col.input.id]">
            <el-select
              v-if="col.type === 'boolean'"
              :model-value="condValue(col.input.id)"
              style="width: 140px"
              @change="(v: string) => setCondValue(col.input.id, v)"
            >
              <el-option label="新手 (true)" value="true" />
              <el-option label="老手 (false)" value="false" />
            </el-select>
            <template v-else>
              <el-select v-model="condOp[col.input.id]" style="width: 84px">
                <el-option v-for="op in COND_OPS" :key="op" :label="op" :value="op" />
              </el-select>
              <el-input
                v-if="col.type === 'number'"
                :model-value="condValue(col.input.id)"
                style="width: 120px"
                @input="(v: string) => setCondValue(col.input.id, v)"
              />
              <el-select
                v-else
                :model-value="condValue(col.input.id)"
                filterable
                allow-create
                default-first-option
                style="min-width: 180px"
                @change="(v: string) => setCondValue(col.input.id, v)"
              >
                <el-option v-for="o in col.options" :key="o" :label="o" :value="o" />
              </el-select>
            </template>
          </template>
        </div>
      </section>

      <section>
        <h4>输出</h4>
        <div class="cond-row">
          <span class="cond-name">规则 ID</span>
          <el-input
            :model-value="draft?.outputs.matchedRule ?? ''"
            placeholder="唯一标识，如 spelling_high_conf_old_hand"
            style="min-width: 260px"
            @input="(v: string) => setOut('matchedRule', v)"
          />
        </div>
        <div v-for="col in outCols" :key="col.output.id" class="cond-row">
          <span class="cond-name" :title="col.output.field">{{ columnLabel(col.output.id) }}</span>
          <el-input
            v-if="col.multiline"
            type="textarea"
            :rows="2"
            :model-value="draft?.outputs[col.output.id] ?? ''"
            @input="(v: string) => setOut(col.output.id, v)"
          />
          <el-input
            v-else
            :model-value="draft?.outputs[col.output.id] ?? ''"
            @input="(v: string) => setOut(col.output.id, v)"
          />
        </div>
      </section>

      <section v-if="tryResult || trying">
        <h4>试算结果（当前内存表）</h4>
        <div v-if="trying" class="sub">正在试算…</div>
        <div v-else-if="tryResult" class="try-result">
          <p>
            命中规则：<span class="mono">{{ tryMatched }}</span>
            <span v-if="tryMatched === draft.key" class="tag ok">= 本行</span>
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
    <template #footer>
      <el-button @click="emit('close')">取消</el-button>
      <el-button :loading="trying" @click="tryRow()">试算</el-button>
      <el-button type="primary" @click="save()">保存行</el-button>
    </template>
  </el-dialog>
</template>

<style scoped>
.row-editor { display: flex; flex-direction: column; gap: 14px; max-height: 60vh; overflow: auto; }
.row-editor h4 { margin: 0 0 6px; font-size: 13px; color: var(--muted); }
.cond-row { display: flex; align-items: center; gap: 10px; margin-bottom: 8px; }
.cond-name { min-width: 110px; font-size: 13px; }
.try-result { font-size: 13px; }
.try-result p { margin: 4px 0; }
.try-result ul { margin: 4px 0 0; padding-left: 18px; }
.mono { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12.5px; }
.tag { font-size: 12px; padding: 1px 6px; border-radius: 6px; margin-left: 6px; }
.tag.ok { background: rgba(82, 196, 124, 0.15); color: #3f9e63; }
</style>