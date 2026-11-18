<script setup lang="ts">
// 知识源导入向导：登记源 → 逐块挖掘（轮询进度）→ 草稿审阅 → 归并预览 → 一键发布
import { computed, reactive, ref, watch, onBeforeUnmount } from 'vue'
import { errMsg } from '../lib/err'
import { errorTypeLabel } from '../lib/labels'
import { LButton, LCheckbox, LDialog, LPopover, LProgress, LSelect, LTable, LTag, toast } from '../ui'
import { api } from '../api/client'
import type {
  ApplyReport,
  IngestDraftItem,
  IngestSessionDetail,
  MergePlan,
  ParsedDocPreview,
  RulePlan,
} from '../types'

const props = defineProps<{ modelValue: boolean; resumeSourceId?: string }>()
const emit = defineEmits<{
  (e: 'update:modelValue', v: boolean): void
  (e: 'published'): void
}>()

const step = ref(0)
const busy = ref(false)
const error = ref('')
const llmEnabled = ref(false)

// 紧凑编号步骤条（无 EP steps；点已完成的步骤可返回）
const stepLabels = ['登记源', '逐块挖掘', '草稿审阅', '一键发布', '完成']
function goStep(i: number) {
  if (i >= 1 && i < step.value && step.value < 4) step.value = i
}

// ---- 步骤①：登记源 ----
const form = reactive({
  source_id: '',
  title: '',
  author: '',
  edition: '',
  lang: 'en',
  license_scope: 'internal_test',
  text: '',
})

const langOptions = [
  { value: 'en', label: 'en' },
  { value: 'zh', label: 'zh' },
  { value: 'ja', label: 'ja' },
]
const licenseOptions = [
  { value: 'internal_test', label: 'internal_test（仅内部测试）' },
  { value: 'licensed', label: 'licensed（授权）' },
  { value: 'public', label: 'public（公开）' },
]

// 书本文件上传（epub/docx/pdf 等二进制；txt/md 仍走正文文本框）
const fileData = ref<{ name: string; data: Uint8Array } | null>(null)
const preview = ref<ParsedDocPreview | null>(null)
const selectedChapters = ref<number[]>([])
const previewing = ref(false)

// ---- 步骤②③④：会话 ----
const sourceId = ref('')
const session = ref<IngestSessionDetail | null>(null)
let pollTimer: number | undefined

// ---- 步骤③：草稿审阅 ----
const drafts = ref<IngestDraftItem[]>([])

// ---- 步骤④：归并预览 ----
const plan = ref<MergePlan | null>(null)
const rulesPlan = ref<RulePlan | null>(null)

// ---- 步骤③：规则派生 ----
const rulesBusy = ref(false)
let rulesTimer: number | undefined

// ---- 步骤⑤：发布 ----
const token = ref('')
const report = ref<ApplyReport | null>(null)

const progress = computed(() =>
  session.value && session.value.chunks_total
    ? session.value.chunks_done / session.value.chunks_total
    : 0,
)

// LTable 列定义（slot 覆盖的列 key 不必对应行字段）
const draftColumns = [
  { key: 'seq', label: '#', width: '50px' },
  { key: 'action', label: '归并预判', width: '200px' },
  { key: 'error_type', label: '错误类型', width: '110px' },
  { key: 'title', label: '标题' },
  { key: 'cefr', label: 'CEFR', width: '70px' },
  { key: 'location', label: '出处', width: '150px' },
]
const ruleDraftColumns = [
  { key: 'rule_id', label: '规则行 ID', width: '190px' },
  { key: 'attribution', label: '归因', width: '150px' },
  { key: 'cefr', label: 'CEFR', width: '70px' },
  { key: 'remedial', label: '讲解' },
]
const newPointColumns = [
  { key: 'point_id', label: '新点 ID', width: '140px' },
  { key: 'error_type', label: '错误类型', width: '120px' },
  { key: 'title', label: '标题' },
  { key: 'cefr', label: 'CEFR', width: '70px' },
]
const rulePlanColumns = [
  { key: 'rule_id', label: '规则行 ID', width: '200px' },
  { key: 'action', label: '动作', width: '110px' },
  { key: 'content', label: '内容' },
]

const actionTone: Record<string, 'ok' | 'warn' | 'danger' | 'neutral'> = {
  endorse: 'ok',
  create: 'warn',
  covered: 'neutral',
}
const ruleActionTone: Record<string, 'ok' | 'warn' | 'danger' | 'neutral'> = {
  new: 'ok',
  replace: 'warn',
  conflict: 'neutral',
}

function stopPolling() {
  if (pollTimer !== undefined) {
    window.clearInterval(pollTimer)
    pollTimer = undefined
  }
  if (rulesTimer !== undefined) {
    window.clearInterval(rulesTimer)
    rulesTimer = undefined
  }
}
onBeforeUnmount(stopPolling)

async function reset() {
  stopPolling()
  step.value = 0
  busy.value = false
  error.value = ''
  session.value = null
  drafts.value = []
  plan.value = null
  report.value = null
  token.value = ''
  sourceId.value = ''
  fileData.value = null
  preview.value = null
  selectedChapters.value = []
  rulesPlan.value = null
  Object.assign(form, {
    source_id: '',
    title: '',
    author: '',
    edition: '',
    lang: 'en',
    license_scope: 'internal_test',
    text: '',
  })
  try {
    const s = await api.getSettings()
    llmEnabled.value = s.llm.enabled
  } catch {
    llmEnabled.value = false
  }
}

// 打开时：新导入或恢复既有会话
watch(
  () => props.modelValue,
  async (open) => {
    if (!open) return
    await reset()
    if (props.resumeSourceId) {
      sourceId.value = props.resumeSourceId
      try {
        await refreshSession()
        step.value = 1
      } catch (e) {
        error.value = errMsg(e)
      }
    }
  },
)

function onFile(ev: Event) {
  const file = (ev.target as HTMLInputElement).files?.[0]
  if (!file) return
  if (/\.(epub|docx|pdf)$/i.test(file.name)) {
    // 二进制书本文件：读字节 → 解析预览（标题/作者/章节/告警）
    const reader = new FileReader()
    reader.onload = async () => {
      const bytes = new Uint8Array(reader.result as ArrayBuffer)
      fileData.value = { name: file.name, data: bytes }
      previewing.value = true
      error.value = ''
      try {
        preview.value = await api.parsePreview(file.name, bytes)
        selectedChapters.value = preview.value.chapters.map((c) => c.index)
        if (!form.title) form.title = preview.value.title
        if (!form.author) form.author = preview.value.author
        if (!form.source_id) form.source_id = preview.value.suggested_source_id
      } catch (e) {
        fileData.value = null
        preview.value = null
        error.value = errMsg(e)
      } finally {
        previewing.value = false
      }
    }
    reader.readAsArrayBuffer(file)
    return
  }
  const reader = new FileReader()
  reader.onload = () => {
    form.text = String(reader.result ?? '')
    if (!form.title) form.title = file.name.replace(/\.(txt|md|text)$/i, '')
  }
  reader.readAsText(file)
}

function clearFile() {
  fileData.value = null
  preview.value = null
  selectedChapters.value = []
}

function toggleAllChapters(all: boolean) {
  selectedChapters.value = all ? (preview.value?.chapters ?? []).map((c) => c.index) : []
}

function toggleChapter(idx: number) {
  const i = selectedChapters.value.indexOf(idx)
  if (i >= 0) selectedChapters.value.splice(i, 1)
  else selectedChapters.value.push(idx)
}

async function startIngest() {
  error.value = ''
  if (!form.title.trim() || !form.source_id.trim()) {
    error.value = 'source_id 与书名必填'
    return
  }
  if (!fileData.value && form.text.trim().length < 100) {
    error.value = '正文过短（< 100 字符）：请上传书本文件或直接粘贴'
    return
  }
  busy.value = true
  try {
    const r = fileData.value
      ? await api.startIngestFile({
          filename: fileData.value.name,
          data: fileData.value.data,
          source_id: form.source_id.trim(),
          title: form.title.trim(),
          author: form.author.trim(),
          edition: form.edition.trim(),
          lang: form.lang,
          license_scope: form.license_scope,
          chapters: selectedChapters.value,
        })
      : await api.startIngest({
          source_id: form.source_id.trim(),
          title: form.title.trim(),
          author: form.author.trim(),
          edition: form.edition.trim(),
          lang: form.lang,
          license_scope: form.license_scope,
          text: form.text,
        })
    sourceId.value = r.source_id
    step.value = 1
    await refreshSession()
    startPolling()
  } catch (e) {
    error.value = errMsg(e)
  } finally {
    busy.value = false
  }
}

async function refreshSession() {
  session.value = await api.getIngestSession(sourceId.value)
  return session.value
}

function startPolling() {
  stopPolling()
  pollTimer = window.setInterval(async () => {
    try {
      const s = await refreshSession()
      if (s.status !== 'running') stopPolling()
    } catch {
      stopPolling()
    }
  }, 2000)
}

async function toDrafts() {
  error.value = ''
  busy.value = true
  try {
    drafts.value = await api.listIngestDrafts(sourceId.value)
    plan.value = await api.getIngestPlan(sourceId.value)
    step.value = 2
  } catch (e) {
    error.value = errMsg(e)
  } finally {
    busy.value = false
  }
}

/// 派生归因规则（按 error_type 分批跑 agent；轮询进度）
async function deriveRules() {
  error.value = ''
  rulesBusy.value = true
  try {
    await api.deriveIngestRules(sourceId.value)
    if (rulesTimer !== undefined) window.clearInterval(rulesTimer)
    rulesTimer = window.setInterval(async () => {
      try {
        const s = await refreshSession()
        if (s.rules_status !== 'running') {
          window.clearInterval(rulesTimer)
          rulesTimer = undefined
          if (s.rules_status === 'failed') error.value = '规则派生中断（见会话错误记录）'
        }
      } catch {
        if (rulesTimer !== undefined) window.clearInterval(rulesTimer)
        rulesTimer = undefined
      }
    }, 2000)
  } catch (e) {
    error.value = errMsg(e)
  } finally {
    rulesBusy.value = false
  }
}

async function toPublish() {
  error.value = ''
  plan.value = await api.getIngestPlan(sourceId.value)
  rulesPlan.value = await api.getIngestRulesPlan(sourceId.value)
  step.value = 3
}

async function publish() {
  error.value = ''
  busy.value = true
  try {
    report.value = await api.publishIngest(sourceId.value, token.value || undefined)
    step.value = 4
    emit('published')
    toast(`知识源 ${report.value.source_id} 已发布，归因富化即时生效`, 'ok')
  } catch (e) {
    error.value = errMsg(e)
  } finally {
    busy.value = false
  }
}

// 放弃会话：LPopover 行内确认（对话框配额外）
async function doDiscard() {
  stopPolling()
  try {
    await api.discardIngest(sourceId.value)
    toast('导入会话已放弃', 'info')
    emit('update:modelValue', false)
  } catch (e) {
    error.value = errMsg(e)
  }
}

function close() {
  emit('update:modelValue', false)
}

const statusLabel: Record<string, string> = {
  running: '挖掘中',
  mined: '挖掘完成（待审阅）',
  failed: '已中断（熔断/重启）',
}
const statusTone: Record<string, 'ok' | 'warn' | 'danger' | 'neutral'> = {
  running: 'neutral',
  mined: 'ok',
  failed: 'danger',
}
const actionLabel: Record<string, string> = {
  endorse: '背书既有点',
  create: '新概念 → 造新点',
  covered: '本源已背书',
}
</script>

<template>
  <LDialog
    :visible="modelValue"
    title="导入知识源（语法书 → 知识库）"
    width="min(880px, calc(100vw - 48px))"
    @update:visible="emit('update:modelValue', $event)"
  >
    <ol class="steps">
      <li v-for="(label, i) in stepLabels" :key="label" class="step">
        <button
          type="button"
          class="step-btn"
          :class="{ 'is-done': i < step, 'is-active': i === step }"
          :disabled="!(i >= 1 && i < step && step < 4)"
          :aria-current="i === step ? 'step' : undefined"
          @click="goStep(i)"
        >
          <span class="step-num">{{ i + 1 }}</span>{{ label }}
        </button>
      </li>
    </ol>

    <div v-if="error" class="w-alert">{{ error }}</div>
    <div v-if="step === 0 && !llmEnabled" class="l-banner w-alert">
      LLM 未配置：知识源挖掘由内置 agent 执行，请先在「设置」中配置 LLM
    </div>

    <!-- ① 登记源 -->
    <div v-if="step === 0" class="w-body">
      <form class="w-form" @submit.prevent>
        <div class="field">
          <span class="field-label" title="必填">书名 *</span>
          <LInput v-model="form.title" placeholder="如：英语语法指南（第三版）" />
        </div>
        <div class="field">
          <span class="field-label" title="必填">源 ID *</span>
          <LInput v-model="form.source_id" placeholder="如：src-mybook-v3（小写字母/数字/-/_）" />
        </div>
        <div class="field">
          <span class="field-label">作者 / 版次</span>
          <span class="field-row">
            <LInput v-model="form.author" placeholder="作者" />
            <LInput v-model="form.edition" placeholder="版次" />
          </span>
        </div>
        <div class="field">
          <span class="field-label">语言 / 许可</span>
          <span class="field-row">
            <LSelect
              class="fld-lang"
              :model-value="form.lang"
              :options="langOptions"
              @update:model-value="form.lang = String($event)"
            />
            <LSelect
              :model-value="form.license_scope"
              :options="licenseOptions"
              @update:model-value="form.license_scope = String($event)"
            />
          </span>
        </div>
        <div class="field field--top">
          <span class="field-label">正文</span>
          <span class="text-col">
            <input type="file" accept=".epub,.txt,.md,.markdown,.docx,.pdf,.text" class="file-input" @change="onFile" />
            <textarea
              v-model="form.text"
              class="l-input l-textarea"
              rows="8"
              placeholder="上传 .epub/.docx/.pdf/.txt/.md 书本文件（自动解析章节），或粘贴纯文本正文（分块后逐块交给内置 agent 挖掘）"
            ></textarea>
          </span>
        </div>
      </form>
      <div v-if="preview" class="doc-preview">
        <p class="w-title">
          解析预览：{{ preview.title }}
          <LTag>{{ preview.format }}</LTag>
          <LTag>{{ preview.chapters.length }} 章 · {{ preview.char_count }} 字</LTag>
          <LButton size="sm" variant="ghost" @click="clearFile">移除文件</LButton>
        </p>
        <div v-for="(w, i) in preview.warnings" :key="i" class="l-banner w-alert">{{ w }}</div>
        <p class="sub">
          勾选参与导入的章节（默认全选；试跑可只勾前几章控制成本）
          <LButton size="sm" variant="ghost" @click="toggleAllChapters(true)">全选</LButton>
          <LButton size="sm" variant="ghost" @click="toggleAllChapters(false)">清空</LButton>
        </p>
        <div class="chapter-grid">
          <LCheckbox
            v-for="c in preview.chapters"
            :key="c.index"
            :model-value="selectedChapters.includes(c.index)"
            :label="`#${c.index} ${c.heading || '（无标题）'} · ${c.chars} 字`"
            @update:model-value="toggleChapter(c.index)"
          />
        </div>
      </div>
      <div v-else-if="previewing" class="l-banner w-alert">正在解析文件…</div>
      <div class="w-actions">
        <LButton @click="close">取消</LButton>
        <LButton variant="primary" :loading="busy" :disabled="!llmEnabled" @click="startIngest">
          开始导入（分块 + 逐块挖掘）
        </LButton>
      </div>
    </div>

    <!-- ② 逐块挖掘 -->
    <div v-else-if="step === 1" class="w-body">
      <template v-if="session">
        <p class="w-title">
          {{ session.source.title }}（{{ session.source.lang }}）·
          <LTag :tone="statusTone[session.status] ?? 'neutral'">
            {{ statusLabel[session.status] || session.status }}
          </LTag>
        </p>
        <LProgress :value="progress" />
        <p class="sub">
          已完成 {{ session.chunks_done }}/{{ session.chunks_total }} 块
          <template v-if="session.chunks_failed">（失败 {{ session.chunks_failed }} 块）</template>
          · 已入区草稿 {{ session.drafts.length }} 条
        </p>
        <div v-for="(e, i) in session.errors" :key="i" class="w-alert">{{ e }}</div>
      </template>
      <div class="w-actions">
        <LPopover align="end">
          <template #trigger="{ toggle }">
            <LButton @click="toggle()">放弃会话</LButton>
          </template>
          <template #default="{ close: closePop }">
            <p class="pop-title">放弃导入会话 {{ sourceId }}？</p>
            <p class="pop-text">放弃后草稿全部丢弃（生效知识库不受影响），可重新导入。</p>
            <div class="pop-actions">
              <LButton size="sm" @click="closePop()">返回</LButton>
              <LButton size="sm" variant="danger" @click="closePop(); doDiscard()">放弃会话</LButton>
            </div>
          </template>
        </LPopover>
        <LButton
          variant="primary"
          :disabled="!session || session.status === 'running' || session.drafts.length === 0"
          @click="toDrafts"
        >
          审阅草稿
        </LButton>
      </div>
    </div>

    <!-- ③ 草稿审阅 -->
    <div v-else-if="step === 2" class="w-body">
      <p class="sub">
        共 {{ drafts.length }} 条草稿；内置 agent 已对齐既有点——同一知识点跨书按 1:N 背书，
        只有新概念造新点（点 ID 不随书变）。
      </p>
      <div class="table-scroll table-mid">
        <LTable :columns="draftColumns" :rows="drafts" row-key="seq" compact>
          <template #col-action="{ row }">
            <LTag :tone="actionTone[row.action] ?? 'neutral'">{{ actionLabel[row.action] }} → {{ row.point_id }}</LTag>
          </template>
          <template #col-error_type="{ row }">{{ errorTypeLabel(row.draft.error_type) }}</template>
          <template #col-title="{ row }">{{ row.draft.title }}</template>
          <template #col-cefr="{ row }">{{ row.draft.cefr || '—' }}</template>
          <template #col-location="{ row }">{{ row.draft.location || '—' }}</template>
        </LTable>
      </div>
      <div class="rules-block">
        <p class="w-title">
          归因规则派生（可选）
          <LTag v-if="session?.rules_status === 'mined'" tone="ok">已派生 {{ session.rule_drafts.length }} 条</LTag>
          <LTag v-else-if="session?.rules_status === 'running'">派生中 {{ session.rules_done }}/{{ session.rules_total }} 批</LTag>
          <LTag v-else-if="session?.rules_status === 'failed'" tone="danger">派生中断</LTag>
        </p>
        <p class="sub">
          内置 agent 按错误类型从知识点派生决策表候选行（新手层/老手层归因 + 讲解文案）；
          发布时随知识点一并写入决策表（同 ID 内置/他源行自动跳过）。
        </p>
        <LButton
          :disabled="!session || session.rules_status === 'running'"
          :loading="session?.rules_status === 'running'"
          @click="deriveRules"
        >
          {{ session?.rule_drafts?.length ? '重新派生规则' : '派生归因规则' }}
        </LButton>
        <div v-for="(e, i) in session?.rules_errors ?? []" :key="i" class="w-alert">{{ e }}</div>
        <div v-if="session?.rule_drafts?.length" class="table-scroll table-short">
          <LTable :columns="ruleDraftColumns" :rows="session.rule_drafts" row-key="seq" compact>
            <template #col-rule_id="{ row }">
              {{ row.draft.error_type }}_{{ row.draft.novice === true ? 'novice' : row.draft.novice === false ? 'hand' : 'any' }}
            </template>
            <template #col-attribution="{ row }">{{ row.draft.attribution }}</template>
            <template #col-cefr="{ row }">{{ row.draft.cefr || '—' }}</template>
            <template #col-remedial="{ row }">{{ row.draft.remedial }}</template>
          </LTable>
        </div>
      </div>
      <div class="w-actions">
        <LButton @click="step = 1">返回</LButton>
        <LButton variant="primary" @click="toPublish">下一步（归并预览）</LButton>
      </div>
    </div>

    <!-- ④ 归并预览 + 发布 -->
    <div v-else-if="step === 3" class="w-body">
      <template v-if="plan">
        <p class="w-title">
          发布预览：新点 <b>{{ plan.new_points.length }}</b> ·
          既有点追加背书 <b>{{ plan.endorsements.length }}</b> ·
          已覆盖跳过 <b>{{ plan.already_covered }}</b>
        </p>
        <div v-if="plan.new_points.length" class="table-scroll table-short">
          <LTable :columns="newPointColumns" :rows="plan.new_points" row-key="point_id" compact>
            <template #col-error_type="{ row }">{{ errorTypeLabel(row.error_type) }}</template>
          </LTable>
        </div>
        <p v-if="plan.endorsements.length" class="sub">
          背书既有点：{{ plan.endorsements.map((e) => e.point_id).join('、') }}
        </p>
        <div v-if="rulesPlan" class="rules-block">
          <p class="w-title">
            决策表规则：新增 <b>{{ rulesPlan.new }}</b> ·
            本源替换 <b>{{ rulesPlan.replaced }}</b> ·
            冲突跳过 <b>{{ rulesPlan.conflicts }}</b>
          </p>
          <div class="table-scroll table-short">
            <LTable :columns="rulePlanColumns" :rows="rulesPlan.rows" row-key="rule_id" compact>
              <template #col-action="{ row }">
                <LTag :tone="ruleActionTone[row.action] ?? 'neutral'">
                  {{ row.action === 'new' ? '新增行' : row.action === 'replace' ? '替换本源' : '冲突跳过' }}
                </LTag>
              </template>
              <template #col-content="{ row }">
                <template v-if="row.action !== 'conflict'">
                  {{ String(row.row.attribution ?? '') }} · {{ String(row.row.remedial ?? '') }}
                </template>
                <template v-else>{{ String(row.row.note ?? '') }}</template>
              </template>
            </LTable>
          </div>
        </div>
        <div class="field token-form">
          <span class="field-label">更新 token</span>
          <input
            v-model="token"
            type="password"
            class="l-input"
            placeholder="服务器设置 GEC_TABLES_UPDATE_TOKEN 时必填，否则留空"
          />
        </div>
      </template>
      <div class="w-actions">
        <LButton @click="step = 2">返回</LButton>
        <LPopover align="end">
          <template #trigger="{ toggle }">
            <LButton @click="toggle()">放弃会话</LButton>
          </template>
          <template #default="{ close: closePop }">
            <p class="pop-title">放弃导入会话 {{ sourceId }}？</p>
            <p class="pop-text">放弃后草稿全部丢弃（生效知识库不受影响），可重新导入。</p>
            <div class="pop-actions">
              <LButton size="sm" @click="closePop()">返回</LButton>
              <LButton size="sm" variant="danger" @click="closePop(); doDiscard()">放弃会话</LButton>
            </div>
          </template>
        </LPopover>
        <LButton variant="primary" :loading="busy" @click="publish">一键发布（知识库 + 决策表热更新）</LButton>
      </div>
    </div>

    <!-- ⑤ 完成 -->
    <div v-else class="w-body">
      <div class="done">
        <div class="done__icon">✓</div>
        <p class="done__title">发布成功</p>
        <p v-if="report" class="done__text">
          知识源 {{ report.source_id }}：新点 {{ report.new_points }} ·
          追加背书 {{ report.endorsed }} · 已覆盖 {{ report.already_covered }} ·
          {{ report.lang }} 知识点共 {{ report.total_points }} 个
          <template v-if="report.rules_new || report.rules_replaced">
            · 决策表规则新增 {{ report.rules_new }} / 替换 {{ report.rules_replaced }} / 冲突跳过 {{ report.rules_conflicts }}
          </template>
        </p>
        <p class="sub">归因富化即时生效：新检出错误的讲解将引用新知识源（知识点列表可按源过滤）</p>
      </div>
      <div class="w-actions">
        <LButton variant="primary" @click="close">完成</LButton>
      </div>
    </div>
  </LDialog>
</template>

<style scoped>
.steps {
  display: flex;
  gap: var(--space-1);
  list-style: none;
  margin: 0 0 var(--space-4);
  padding: 0;
}
.step-btn {
  display: inline-flex;
  align-items: center;
  gap: var(--space-1);
  height: var(--h-control);
  padding: 0 var(--space-2);
  font: inherit;
  font-size: var(--fs-sm);
  color: var(--muted);
  background: transparent;
  border: 0;
  border-radius: var(--radius-ctl);
  cursor: pointer;
}
.step-btn:hover:not(:disabled) { background: var(--surface-2); color: var(--text); }
.step-btn:disabled { cursor: default; }
.step-num {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: var(--h-control-sm);
  height: var(--h-control-sm);
  border: 1px solid var(--border-strong);
  border-radius: 50%;
  font-size: var(--fs-xs);
}
.step.is-active .step-btn { color: var(--text); font-weight: 600; }
.step.is-active .step-num { border-color: var(--accent); color: var(--accent); }
.step.is-done .step-btn { color: var(--text-2); }
.step.is-done .step-num { background: var(--accent); border-color: var(--accent); color: #fff; }
.w-body {
  min-height: calc(var(--space-7) * 5);
}
.w-alert {
  margin: 0 0 var(--space-2);
  font-size: var(--fs-sm);
  color: var(--danger);
}
.l-banner.w-alert { margin: var(--space-1) 0 var(--space-2); color: var(--text-2); }
.w-title {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: var(--space-2);
  margin: var(--space-1) 0 var(--space-3);
  font-weight: 600;
}
.sub {
  color: var(--text-2);
  font-size: var(--fs-sm);
  margin: var(--space-2) 0;
}
.w-form {
  display: flex;
  flex-direction: column;
  gap: var(--space-2);
}
.field {
  display: grid;
  grid-template-columns: calc(var(--space-4) * 7) minmax(0, 1fr);
  align-items: center;
  gap: var(--space-2) var(--space-3);
  font-size: var(--fs-ui);
}
.field--top { align-items: start; }
.field-label { color: var(--text-2); }
.field-row {
  display: flex;
  gap: var(--space-2);
}
.field-row > .l-input,
.field-row > .l-select {
  flex: 1;
  min-width: 0;
  max-width: calc(var(--space-7) * 4.5);
}
.fld-lang { width: calc(var(--space-7) * 2); }
.l-textarea {
  height: auto;
  min-height: calc(var(--space-7) * 5);
  padding: var(--space-2);
  line-height: var(--lh-body);
}
.file-input { font-size: var(--fs-sm); color: var(--text-2); }
.text-col {
  display: flex;
  flex-direction: column;
  gap: var(--space-2);
  width: 100%;
}
.w-actions {
  display: flex;
  justify-content: flex-end;
  gap: var(--space-2);
  margin-top: var(--space-4);
}
.token-form {
  margin-top: var(--space-4);
}
.doc-preview {
  margin-top: var(--space-1);
}
.chapter-grid {
  display: flex;
  flex-direction: column;
  gap: var(--space-1);
  max-height: calc(var(--space-7) * 4.5);
  overflow-y: auto;
  border: 1px solid var(--border);
  border-radius: var(--radius-ctl);
  padding: var(--space-2) var(--space-3);
}
.rules-block {
  margin-top: var(--space-2);
  padding-top: var(--space-3);
  border-top: 1px solid var(--border);
}
.table-scroll { overflow-y: auto; }
.table-mid { max-height: calc(var(--space-7) * 8); }
.table-short { max-height: calc(var(--space-7) * 4.5); }
.done {
  padding: var(--space-4) 0;
  text-align: center;
}
.done__icon {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: var(--space-7);
  height: var(--space-7);
  background: var(--ok-soft);
  color: var(--ok);
  border-radius: 50%;
  font-size: var(--fs-xl);
}
.done__title {
  margin: var(--space-2) 0;
  font-size: var(--fs-lg);
  font-weight: 600;
}
.done__text {
  margin: 0;
  color: var(--text-2);
  font-size: var(--fs-ui);
}
.pop-title {
  margin: 0 0 var(--space-1);
  font-size: var(--fs-ui);
  font-weight: 600;
}
.pop-text {
  margin: 0 0 var(--space-2);
  font-size: var(--fs-sm);
  color: var(--text-2);
}
.pop-actions {
  display: flex;
  justify-content: flex-end;
  gap: var(--space-2);
}
:deep(menu.l-dialog__foot:empty) { display: none; }
</style>
