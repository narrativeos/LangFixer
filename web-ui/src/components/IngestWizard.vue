<script setup lang="ts">
// 知识源导入向导：登记源 → 逐块挖掘（轮询进度）→ 草稿审阅 → 归并预览 → 一键发布
import { reactive, ref, watch, onBeforeUnmount } from 'vue'
import { ElMessage, ElMessageBox } from 'element-plus'
import { errMsg } from '../lib/err'
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
    ElMessage.success(`知识源 ${report.value.source_id} 已发布，归因富化即时生效`)
  } catch (e) {
    error.value = errMsg(e)
  } finally {
    busy.value = false
  }
}

async function discard() {
  try {
    await ElMessageBox.confirm(
      '放弃后草稿全部丢弃（生效知识库不受影响），可重新导入。',
      `放弃导入会话 ${sourceId.value}？`,
      { type: 'warning', confirmButtonText: '放弃会话', cancelButtonText: '返回' },
    )
  } catch {
    return
  }
  stopPolling()
  try {
    await api.discardIngest(sourceId.value)
    ElMessage.info('导入会话已放弃')
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
const actionLabel: Record<string, string> = {
  endorse: '背书既有点',
  create: '新概念 → 造新点',
  covered: '本源已背书',
}
</script>

<template>
  <el-dialog
    :model-value="modelValue"
    title="导入知识源（语法书 → 知识库）"
    width="880px"
    :close-on-click-modal="false"
    @update:model-value="emit('update:modelValue', $event)"
  >
    <el-steps :active="step" finish-status="success" align-center class="steps">
      <el-step title="登记源" />
      <el-step title="逐块挖掘" />
      <el-step title="草稿审阅" />
      <el-step title="一键发布" />
      <el-step title="完成" />
    </el-steps>

    <el-alert v-if="error" type="error" :closable="false" :title="error" class="w-alert" />
    <el-alert
      v-if="step === 0 && !llmEnabled"
      type="warning"
      :closable="false"
      show-icon
      title="LLM 未配置：知识源挖掘由内置 agent 执行，请先在「设置」中配置 LLM"
      class="w-alert"
    />

    <!-- ① 登记源 -->
    <div v-if="step === 0" class="w-body">
      <el-form label-width="90px" @submit.prevent>
        <el-form-item label="书名" required>
          <el-input v-model="form.title" placeholder="如：英语语法指南（第三版）" />
        </el-form-item>
        <el-form-item label="源 ID" required>
          <el-input v-model="form.source_id" placeholder="如：src-mybook-v3（小写字母/数字/-/_）" />
        </el-form-item>
        <el-form-item label="作者 / 版次">
          <el-input v-model="form.author" placeholder="作者" style="width: 220px" />
          <el-input v-model="form.edition" placeholder="版次" style="width: 180px; margin-left: 8px" />
        </el-form-item>
        <el-form-item label="语言 / 许可">
          <el-select v-model="form.lang" style="width: 90px">
            <el-option label="en" value="en" />
            <el-option label="zh" value="zh" />
            <el-option label="ja" value="ja" />
          </el-select>
          <el-select v-model="form.license_scope" style="width: 180px; margin-left: 8px">
            <el-option value="internal_test" label="internal_test（仅内部测试）" />
            <el-option value="licensed" label="licensed（授权）" />
            <el-option value="public" label="public（公开）" />
          </el-select>
        </el-form-item>
        <el-form-item label="正文">
          <div class="text-col">
            <input type="file" accept=".epub,.txt,.md,.markdown,.docx,.pdf,.text" @change="onFile" />
            <el-input
              v-model="form.text"
              type="textarea"
              :rows="8"
              placeholder="上传 .epub/.docx/.pdf/.txt/.md 书本文件（自动解析章节），或粘贴纯文本正文（分块后逐块交给内置 agent 挖掘）"
            />
          </div>
        </el-form-item>
      </el-form>
      <div v-if="preview" class="doc-preview">
        <p class="w-title">
          解析预览：{{ preview.title }}
          <el-tag size="small">{{ preview.format }}</el-tag>
          <el-tag size="small" type="info">{{ preview.chapters.length }} 章 · {{ preview.char_count }} 字</el-tag>
          <el-button size="small" link type="danger" @click="clearFile">移除文件</el-button>
        </p>
        <el-alert
          v-for="(w, i) in preview.warnings"
          :key="i"
          type="warning"
          :closable="false"
          :title="w"
          class="w-alert"
        />
        <p class="sub">
          勾选参与导入的章节（默认全选；试跑可只勾前几章控制成本）
          <el-button size="small" link type="primary" @click="toggleAllChapters(true)">全选</el-button>
          <el-button size="small" link type="primary" @click="toggleAllChapters(false)">清空</el-button>
        </p>
        <el-checkbox-group v-model="selectedChapters" class="chapter-grid">
          <el-checkbox v-for="c in preview.chapters" :key="c.index" :value="c.index">
            #{{ c.index }} {{ c.heading || '（无标题）' }} · {{ c.chars }} 字
          </el-checkbox>
        </el-checkbox-group>
      </div>
      <el-alert v-else-if="previewing" type="info" :closable="false" title="正在解析文件…" class="w-alert" />
      <div class="w-actions">
        <el-button @click="close">取消</el-button>
        <el-button type="primary" :loading="busy" :disabled="!llmEnabled" @click="startIngest">
          开始导入（分块 + 逐块挖掘）
        </el-button>
      </div>
    </div>

    <!-- ② 逐块挖掘 -->
    <div v-else-if="step === 1" class="w-body">
      <template v-if="session">
        <p class="w-title">
          {{ session.source.title }}（{{ session.source.lang }}）·
          <el-tag size="small" :type="session.status === 'failed' ? 'danger' : session.status === 'mined' ? 'success' : 'info'">
            {{ statusLabel[session.status] || session.status }}
          </el-tag>
        </p>
        <el-progress
          :percentage="session.chunks_total ? Math.round((session.chunks_done / session.chunks_total) * 100) : 0"
          :status="session.status === 'failed' ? 'exception' : session.status === 'mined' ? 'success' : undefined"
        />
        <p class="sub">
          已完成 {{ session.chunks_done }}/{{ session.chunks_total }} 块
          <template v-if="session.chunks_failed">（失败 {{ session.chunks_failed }} 块）</template>
          · 已入区草稿 {{ session.drafts.length }} 条
        </p>
        <el-alert
          v-for="(e, i) in session.errors"
          :key="i"
          type="warning"
          :closable="false"
          :title="e"
          class="w-alert"
        />
      </template>
      <div class="w-actions">
        <el-button @click="discard">放弃会话</el-button>
        <el-button
          type="primary"
          :disabled="!session || session.status === 'running' || session.drafts.length === 0"
          @click="toDrafts"
        >
          审阅草稿
        </el-button>
      </div>
    </div>

    <!-- ③ 草稿审阅 -->
    <div v-else-if="step === 2" class="w-body">
      <p class="sub">
        共 {{ drafts.length }} 条草稿；内置 agent 已对齐既有点——同一知识点跨书按 1:N 背书，
        只有新概念造新点（点 ID 不随书变）。
      </p>
      <el-table :data="drafts" size="small" max-height="380">
        <el-table-column prop="seq" label="#" width="50" />
        <el-table-column label="归并预判" width="200">
          <template #default="{ row }">
            <el-tag
              size="small"
              :type="row.action === 'create' ? 'warning' : row.action === 'covered' ? 'info' : 'success'"
            >
              {{ actionLabel[row.action] }} → {{ row.point_id }}
            </el-tag>
          </template>
        </el-table-column>
        <el-table-column label="错误类型" width="110">
          <template #default="{ row }">{{ row.draft.error_type }}</template>
        </el-table-column>
        <el-table-column label="标题" min-width="200">
          <template #default="{ row }">{{ row.draft.title }}</template>
        </el-table-column>
        <el-table-column label="CEFR" width="70">
          <template #default="{ row }">{{ row.draft.cefr || '—' }}</template>
        </el-table-column>
        <el-table-column label="出处" width="150">
          <template #default="{ row }">{{ row.draft.location || '—' }}</template>
        </el-table-column>
      </el-table>
      <div class="rules-block">
        <el-divider />
        <p class="w-title">
          归因规则派生（可选）
          <el-tag v-if="session?.rules_status === 'mined'" size="small" type="success">已派生 {{ session.rule_drafts.length }} 条</el-tag>
          <el-tag v-else-if="session?.rules_status === 'running'" size="small" type="info">派生中 {{ session.rules_done }}/{{ session.rules_total }} 批</el-tag>
          <el-tag v-else-if="session?.rules_status === 'failed'" size="small" type="danger">派生中断</el-tag>
        </p>
        <p class="sub">
          内置 agent 按错误类型从知识点派生决策表候选行（新手层/老手层归因 + 讲解文案）；
          发布时随知识点一并写入决策表（同 ID 内置/他源行自动跳过）。
        </p>
        <el-button
          :disabled="!session || session.rules_status === 'running'"
          :loading="session?.rules_status === 'running'"
          @click="deriveRules"
        >
          {{ session?.rule_drafts?.length ? '重新派生规则' : '派生归因规则' }}
        </el-button>
        <el-alert
          v-for="(e, i) in session?.rules_errors ?? []"
          :key="i"
          type="warning"
          :closable="false"
          :title="e"
          class="w-alert"
        />
        <el-table v-if="session?.rule_drafts?.length" :data="session.rule_drafts" size="small" max-height="220">
          <el-table-column label="规则行 ID" width="190">
            <template #default="{ row }">
              {{ row.draft.error_type }}_{{ row.draft.novice === true ? 'novice' : row.draft.novice === false ? 'hand' : 'any' }}
            </template>
          </el-table-column>
          <el-table-column label="归因" width="150">
            <template #default="{ row }">{{ row.draft.attribution }}</template>
          </el-table-column>
          <el-table-column label="CEFR" width="70">
            <template #default="{ row }">{{ row.draft.cefr || '—' }}</template>
          </el-table-column>
          <el-table-column label="讲解" min-width="240">
            <template #default="{ row }">{{ row.draft.remedial }}</template>
          </el-table-column>
        </el-table>
      </div>
      <div class="w-actions">
        <el-button @click="step = 1">返回</el-button>
        <el-button type="primary" @click="toPublish">下一步（归并预览）</el-button>
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
        <el-table v-if="plan.new_points.length" :data="plan.new_points" size="small" max-height="220">
          <el-table-column prop="point_id" label="新点 ID" width="140" />
          <el-table-column prop="error_type" label="错误类型" width="120" />
          <el-table-column prop="title" label="标题" min-width="220" />
          <el-table-column prop="cefr" label="CEFR" width="70" />
        </el-table>
        <p v-if="plan.endorsements.length" class="sub">
          背书既有点：{{ plan.endorsements.map((e) => e.point_id).join('、') }}
        </p>
        <template v-if="rulesPlan">
          <el-divider />
          <p class="w-title">
            决策表规则：新增 <b>{{ rulesPlan.new }}</b> ·
            本源替换 <b>{{ rulesPlan.replaced }}</b> ·
            冲突跳过 <b>{{ rulesPlan.conflicts }}</b>
          </p>
          <el-table :data="rulesPlan.rows" size="small" max-height="200">
            <el-table-column prop="rule_id" label="规则行 ID" width="200" />
            <el-table-column label="动作" width="110">
              <template #default="{ row }">
                <el-tag size="small" :type="row.action === 'conflict' ? 'info' : row.action === 'replace' ? 'warning' : 'success'">
                  {{ row.action === 'new' ? '新增行' : row.action === 'replace' ? '替换本源' : '冲突跳过' }}
                </el-tag>
              </template>
            </el-table-column>
            <el-table-column label="内容" min-width="240">
              <template #default="{ row }">
                <template v-if="row.action !== 'conflict'">
                  {{ String(row.row.attribution ?? '') }} · {{ String(row.row.remedial ?? '') }}
                </template>
                <template v-else>{{ String(row.row.note ?? '') }}</template>
              </template>
            </el-table-column>
          </el-table>
        </template>
        <el-form label-width="120px" class="token-form">
          <el-form-item label="更新 token">
            <el-input
              v-model="token"
              type="password"
              show-password
              placeholder="服务器设置 GEC_TABLES_UPDATE_TOKEN 时必填，否则留空"
            />
          </el-form-item>
        </el-form>
      </template>
      <div class="w-actions">
        <el-button @click="step = 2">返回</el-button>
        <el-button @click="discard">放弃会话</el-button>
        <el-button type="primary" :loading="busy" @click="publish">一键发布（知识库 + 决策表热更新）</el-button>
      </div>
    </div>

    <!-- ⑤ 完成 -->
    <div v-else class="w-body">
      <el-result icon="success" title="发布成功">
        <template #sub-title>
          <p v-if="report">
            知识源 {{ report.source_id }}：新点 {{ report.new_points }} ·
            追加背书 {{ report.endorsed }} · 已覆盖 {{ report.already_covered }} ·
            {{ report.lang }} 知识点共 {{ report.total_points }} 个
            <template v-if="report.rules_new || report.rules_replaced">
              · 决策表规则新增 {{ report.rules_new }} / 替换 {{ report.rules_replaced }} / 冲突跳过 {{ report.rules_conflicts }}
            </template>
          </p>
          <p class="sub">归因富化即时生效：新检出错误的讲解将引用新知识源（知识点列表可按源过滤）</p>
        </template>
      </el-result>
      <div class="w-actions">
        <el-button type="primary" @click="close">完成</el-button>
      </div>
    </div>
  </el-dialog>
</template>

<style scoped>
.steps {
  margin-bottom: 18px;
}
.w-body {
  min-height: 260px;
}
.w-alert {
  margin-bottom: 10px;
}
.w-title {
  margin: 4px 0 10px;
  font-weight: 600;
}
.sub {
  color: var(--el-text-color-secondary);
  font-size: 13px;
  margin: 8px 0;
}
.text-col {
  display: flex;
  flex-direction: column;
  gap: 8px;
  width: 100%;
}
.w-actions {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
  margin-top: 16px;
}
.token-form {
  margin-top: 14px;
}
.doc-preview {
  margin-top: 6px;
}
.chapter-grid {
  display: flex;
  flex-direction: column;
  gap: 2px;
  max-height: 220px;
  overflow-y: auto;
  border: 1px solid var(--el-border-color-lighter);
  border-radius: 4px;
  padding: 8px 12px;
}
.rules-block {
  margin-top: 8px;
}
</style>