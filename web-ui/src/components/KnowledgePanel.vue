<script setup lang="ts">
import { computed, ref, onMounted, watch } from 'vue'
import { errMsg } from '../lib/err'
import { errorTypeLabel } from '../lib/labels'
import { LButton, LCheckbox, LDialog, LEmpty, LInput, LSection, LSelect, LTable, LTag, toast } from '../ui'
import { api } from '../api/client'
import IngestWizard from './IngestWizard.vue'
import type {
  IngestSessionSummary,
  KnowledgePoint,
  KnowledgeSource,
  PackSummary,
  ResourcePack,
} from '../types'

const sources = ref<KnowledgeSource[]>([])
const points = ref<KnowledgePoint[]>([])
const lang = ref('zh')
const sourceFilter = ref('')
const loading = ref(false)
const error = ref('')

// 知识源导入（一键导入向导 + 进行中的会话）
const sessions = ref<IngestSessionSummary[]>([])
const wizardOpen = ref(false)
const resumeSourceId = ref('')

async function load() {
  loading.value = true
  error.value = ''
  try {
    sources.value = await api.listKnowledgeSources()
    await reloadPoints()
  } catch (e) {
    error.value = errMsg(e)
  } finally {
    loading.value = false
  }
  try {
    sessions.value = await api.listIngestSessions()
  } catch {
    sessions.value = []
  }
}

function openWizard(resumeSource?: string) {
  resumeSourceId.value = resumeSource ?? ''
  wizardOpen.value = true
}

async function discardSession(s: IngestSessionSummary) {
  try {
    await api.discardIngest(s.source_id)
    sessions.value = await api.listIngestSessions()
    toast(`已放弃导入会话 ${s.source_id}`, 'info')
  } catch (e) {
    error.value = errMsg(e)
  }
}

const statusLabel: Record<string, string> = {
  running: '挖掘中',
  mined: '待审阅发布',
  failed: '已中断',
}
const statusTone: Record<string, 'ok' | 'warn' | 'danger' | 'neutral'> = {
  running: 'neutral',
  mined: 'ok',
  failed: 'danger',
}

async function reloadPoints() {
  points.value = await api.listKnowledgePoints(lang.value, sourceFilter.value || undefined)
}

function sourceLabel(s: { source_id: string; title?: string; location?: string }): string {
  const t = s.title || s.source_id
  return s.location ? `${t} · ${s.location}` : t
}

const langOptions = [
  { value: 'zh', label: 'zh' },
  { value: 'en', label: 'en' },
  { value: 'ja', label: 'ja' },
]
const sourceOptions = computed(() => [
  { value: '', label: '全部知识源' },
  ...sources.value.map((s) => ({ value: s.source_id, label: s.title })),
])

// LTable 列定义（slot 覆盖的列 key 不必对应行字段）
const sourceColumns = [
  { key: 'source_id', label: 'ID', width: '170px', sortable: true },
  { key: 'title', label: '标题', sortable: true },
  { key: 'author', label: '作者', width: '110px' },
  { key: 'edition', label: '版次', width: '90px' },
  { key: 'lang', label: '语言', width: '70px' },
  { key: 'license_scope', label: '许可范围', width: '120px' },
  { key: 'point_count', label: '关联知识点', width: '100px', align: 'right' as const },
  { key: 'pack', label: '资源包', width: '150px' },
]
const sessionColumns = [
  { key: 'source_id', label: '源 ID', width: '180px', sortable: true },
  { key: 'title', label: '书名' },
  { key: 'lang', label: '语言', width: '70px' },
  { key: 'status', label: '状态', width: '110px' },
  { key: 'chunks', label: '挖掘进度', width: '120px' },
  { key: 'staged_points', label: '草稿', width: '80px', align: 'right' as const },
  { key: 'actions', label: '操作', width: '150px' },
]
const pointColumns = [
  { key: 'point_id', label: '点 ID', width: '130px', sortable: true },
  { key: 'error_type', label: '错误类型', width: '120px', sortable: true },
  { key: 'section', label: '小节', width: '80px' },
  { key: 'title', label: '标题' },
  { key: 'cefr', label: 'CEFR', width: '70px' },
  { key: 'remedial', label: '补救内容' },
  { key: 'sources', label: '知识源（1:N）' },
]

// ── 资源包：打包导出 / 删除（盘点预览确认） / 导入 ──
const cleanSummary = ref<PackSummary | null>(null)
const cleanOpen = ref(false)
const cleanKeepOrphans = ref(false)
const cleanBackup = ref(true)
const cleanToken = ref('')
const cleanBusy = ref(false)
const importOpen = ref(false)
const importFile = ref<File | null>(null)
const importPackPreview = ref<ResourcePack | null>(null)
const importZipInfo = ref<{ name: string; sizeKb: number } | null>(null)
const importToken = ref('')
const importBusy = ref(false)
const fileInput = ref<HTMLInputElement>()

async function exportPack(s: KnowledgeSource) {
  try {
    const r = await api.exportDataset(s.source_id)
    if (!r) {
      toast('导出已取消', 'info')
      return
    }
    const sizeKb = Math.max(1, Math.round(r.size_bytes / 1024))
    const parts = [`${r.filename}（${sizeKb} KB）`]
    if (r.file_path) parts.push(`已保存到 ${r.file_path}`)
    const comps: string[] = []
    if (r.points !== undefined) comps.push(`${r.points} 知识点`)
    if (r.has_table) comps.push('决策表快照')
    if (r.has_aux) comps.push('决策数据')
    if (r.has_staging) comps.push('未发布草稿')
    if (comps.length) parts.push(comps.join(' + '))
    toast(`资源包已导出：${parts.join('，')}`, 'ok', undefined, 6000)
  } catch (e) {
    error.value = errMsg(e)
  }
}

async function openClean(s: KnowledgeSource) {
  try {
    cleanSummary.value = await api.datasetSummary(s.source_id)
    cleanKeepOrphans.value = false
    cleanBackup.value = true
    cleanToken.value = ''
    cleanOpen.value = true
  } catch (e) {
    error.value = errMsg(e)
  }
}

async function confirmClean() {
  if (!cleanSummary.value) return
  cleanBusy.value = true
  const sourceId = cleanSummary.value.source.source_id
  try {
    // ① 删除前自动备份（默认开启）：导出失败则中止删除
    let backupNote = ''
    if (cleanBackup.value) {
      const b = await api.exportDatasetBackup(sourceId)
      const sizeKb = Math.max(1, Math.round(b.size_bytes / 1024))
      backupNote = b.file_path
        ? `，备份 zip 已保存：${b.file_path}`
        : `，备份 zip 已下载：${b.filename}（${sizeKb} KB）`
    }
    // ② 删除资源包
    const r = await api.cleanDataset(sourceId, cleanKeepOrphans.value, cleanToken.value || undefined)
    toast(
      `资源包已删除：知识点 −${r.knowledge.points_removed}，规则行 −${r.rules_removed}，` +
        `决策数据文件 −${r.aux_removed}${backupNote}`,
      'ok',
      undefined,
      8000,
    )
    cleanOpen.value = false
    await load()
  } catch (e) {
    error.value = errMsg(e)
  } finally {
    cleanBusy.value = false
  }
}

function openImport() {
  importFile.value = null
  importPackPreview.value = null
  importZipInfo.value = null
  importToken.value = ''
  error.value = ''
  importOpen.value = true
}

function onPickFile(ev: Event) {
  const file = (ev.target as HTMLInputElement).files?.[0] ?? null
  importFile.value = file
  importPackPreview.value = null
  importZipInfo.value = null
  error.value = ''
  if (!file) return
  if (file.name.toLowerCase().endsWith('.zip')) {
    // zip 压缩包：内容服务端解压校验（校验和不符整体拒收），本地仅展示基本信息
    importZipInfo.value = { name: file.name, sizeKb: Math.max(1, Math.round(file.size / 1024)) }
    return
  }
  file.text().then((text) => {
    try {
      const pack = JSON.parse(text) as ResourcePack
      if (pack.format !== 'langfixer-resource-pack') {
        error.value = `不是 LangFixer 资源包（format=${pack.format ?? '缺失'}）`
        return
      }
      importPackPreview.value = pack
    } catch {
      error.value = '资源包 JSON 解析失败（也可改用导出的 .zip 压缩包）'
    }
  })
}

async function confirmImport() {
  if (!importFile.value) return
  importBusy.value = true
  try {
    // 原样上传文件字节（zip 压缩包 / 裸 JSON，服务端自适应解析 + 校验和验证）
    const bytes = new Uint8Array(await importFile.value.arrayBuffer())
    const r = await api.importDataset(bytes, importToken.value || undefined)
    toast(
      `资源包已导入：新知识点 +${r.knowledge.points_created}，追加背书 +${r.knowledge.points_endorsed}，` +
        `规则行 +${r.rules_added}（替换 ${r.rules_replaced}）`,
      'ok',
    )
    importOpen.value = false
    await load()
  } catch (e) {
    error.value = errMsg(e)
  } finally {
    importBusy.value = false
  }
}

onMounted(load)
watch([lang, sourceFilter], reloadPoints)
</script>

<template>
  <div class="kb">
    <LSection
      title="知识源注册表"
      desc="知识源只登记元数据（书名/作者/版次/许可范围）；正文内容不随产品内置发布"
    >
      <template #actions>
        <LButton size="sm" @click="openImport()">导入资源包</LButton>
        <LButton size="sm" variant="primary" @click="openWizard()">导入知识源</LButton>
      </template>

      <!-- 资源包导入：行内展开（选文件 .zip/.json → 预览组成 → 确认导入） -->
      <div v-if="importOpen" class="import-panel">
        <input
          ref="fileInput"
          type="file"
          accept=".zip,.json,application/zip,application/json"
          class="pack-file"
          @change="onPickFile"
        />
        <p v-if="importZipInfo" class="pack-title">
          {{ importZipInfo.name }}（压缩包，{{ importZipInfo.sizeKb }} KB）
        </p>
        <p v-if="importZipInfo" class="muted pack-zip-hint">
          压缩包内容将由服务端解压并校验（校验和不符整体拒收），确认后导入
        </p>
        <template v-if="importPackPreview">
          <p class="pack-title">
            {{ importPackPreview.source.title }}（{{ importPackPreview.source.source_id }}）
          </p>
          <ul class="pack-list">
            <li>知识点 {{ importPackPreview.components.knowledge_points.length }} 个（幂等归并：既有点只追加背书）</li>
            <li v-for="dt in importPackPreview.components.decision_tables ?? []" :key="dt.lang">
              决策表快照（{{ dt.lang }}，按源合并规则行）
            </li>
            <li v-if="importPackPreview.components.aux_points">含决策数据（书源提取数据文件）</li>
            <li v-if="importPackPreview.components.staging">含未发布导入草稿（同 ID 会话存在时跳过）</li>
          </ul>
        </template>
        <LInput
          v-if="importPackPreview || importZipInfo"
          v-model="importToken"
          class="pack-token"
          placeholder="更新 token（服务器设置 GEC_TABLES_UPDATE_TOKEN 时必填）"
        />
        <div v-if="error" class="err-line">{{ error }}</div>
        <div class="panel-actions">
          <LButton size="sm" @click="importOpen = false">取消</LButton>
          <LButton
            size="sm"
            variant="primary"
            :disabled="!importPackPreview && !importZipInfo"
            :loading="importBusy"
            @click="confirmImport"
          >
            确认导入
          </LButton>
        </div>
      </div>

      <div
        v-if="sources.length > 0 && sources.some((s) => s.license_scope === 'internal_test')"
        class="l-banner kb-warn"
      >
        注意：存在「仅内部测试」许可范围的知识源，其内容不得对外发布
      </div>

      <div v-if="sources.length > 0" class="table-scroll">
        <LTable :columns="sourceColumns" :rows="sources" row-key="source_id" compact>
          <template #col-author="{ row }">{{ row.author || '—' }}</template>
          <template #col-edition="{ row }">{{ row.edition || '—' }}</template>
          <template #col-license_scope="{ row }">
            <LTag>{{ row.license_scope || 'internal_test' }}</LTag>
          </template>
          <template #col-pack="{ row }">
            <div class="row-actions">
              <LButton size="sm" @click="exportPack(row)">导出</LButton>
              <LButton size="sm" variant="danger" title="删除该知识源的资源包" @click="openClean(row)">删除</LButton>
            </div>
          </template>
        </LTable>
      </div>
      <LEmpty
        v-else
        text="未加载知识源"
        hint="部署环境未提供知识库目录（GEC_KNOWLEDGE_DIR）时，归因结果仅含表级信息"
      >
        <template #action>
          <LButton size="sm" @click="load">重新加载</LButton>
        </template>
      </LEmpty>
    </LSection>

    <LSection
      v-if="sessions.length > 0"
      title="进行中的导入会话"
      desc="正文挖掘在后台进行；草稿发布前不影响归因"
    >
      <LTable :columns="sessionColumns" :rows="sessions" row-key="source_id" compact>
        <template #col-status="{ row }">
          <LTag :tone="statusTone[row.status] ?? 'neutral'">{{ statusLabel[row.status] || row.status }}</LTag>
        </template>
        <template #col-chunks="{ row }">{{ row.chunks_done }}/{{ row.chunks_total }}</template>
        <template #col-actions="{ row }">
          <div class="row-actions">
            <LButton size="sm" @click="openWizard(row.source_id)">查看</LButton>
            <LButton size="sm" variant="danger" @click="discardSession(row)">放弃</LButton>
          </div>
        </template>
      </LTable>
    </LSection>

    <LSection title="知识点">
      <template #actions>
        <LSelect
          class="sel-lang"
          :model-value="lang"
          :options="langOptions"
          aria-label="知识点语言"
          @update:model-value="lang = String($event)"
        />
        <LSelect
          class="sel-source"
          :model-value="sourceFilter"
          :options="sourceOptions"
          aria-label="按知识源过滤"
          @update:model-value="sourceFilter = String($event)"
        />
      </template>
      <div v-if="loading" class="loading">加载中…</div>
      <div v-else-if="error && !importOpen" class="err-line">
        <span>{{ error }}</span>
        <LButton size="sm" @click="load">重试</LButton>
      </div>
      <div v-else-if="points.length > 0" class="table-scroll table-tall">
        <LTable :columns="pointColumns" :rows="points" row-key="point_id" compact>
          <template #col-error_type="{ row }">{{ errorTypeLabel(row.error_type) }}</template>
          <template #col-section="{ row }">{{ row.section || '—' }}</template>
          <template #col-cefr="{ row }">{{ row.cefr || '—' }}</template>
          <template #col-sources="{ row }">
            <div v-for="(s, i) in row.sources ?? []" :key="i" class="kb-src">
              <LTag v-if="i === 0 && (row.sources ?? []).length > 1" class="src-badge">
                多源背书 ×{{ (row.sources ?? []).length }}
              </LTag>
              {{ sourceLabel(s) }}
            </div>
            <span v-if="!(row.sources ?? []).length" class="muted">—</span>
          </template>
        </LTable>
      </div>
      <LEmpty
        v-else
        text="该语言暂无知识点（或所选知识源下无关联知识点）"
        hint="可切换语言或知识源重新查询；也可导入知识源扩充知识点"
      >
        <template #action>
          <LButton size="sm" variant="primary" @click="openWizard()">导入知识源</LButton>
        </template>
      </LEmpty>
    </LSection>

    <!-- 资源包删除：盘点预览 + 确认（不可逆，展示将被删除的组成；对话框配额内） -->
    <LDialog
      v-model:visible="cleanOpen"
      title="删除资源包（不可逆，默认先导出备份）"
      width="min(520px, calc(100vw - 48px))"
    >
      <template v-if="cleanSummary">
        <p class="pack-title">
          {{ cleanSummary.source.title }}（{{ cleanSummary.source.source_id }}）
        </p>
        <ul class="pack-list">
          <li>
            知识点：共 {{ cleanSummary.total_points }} 个
            <template v-if="cleanSummary.langs.length">
              （仅本源背书 {{ cleanSummary.langs.reduce((n, l) => n + l.exclusive_points, 0) }}
              个将被删除；多源共享
              {{ cleanSummary.langs.reduce((n, l) => n + l.shared_points, 0) }} 个只剥离引用）
            </template>
          </li>
          <li v-for="r in cleanSummary.rules" :key="r.lang">
            {{ r.lang }} 归因规则：摘除 {{ r.tagged_rules }} 行（保留通用行
            {{ r.total_rules - r.tagged_rules }} 行）
          </li>
          <li v-if="cleanSummary.aux_points_present || cleanSummary.aux_index_present">
            决策数据文件（{{ cleanSummary.aux_points_present ? 'points ' : ''
            }}{{ cleanSummary.aux_index_present ? 'point_index' : '' }}）摘除该源条目
            （其他源条目保留；摘空后删除文件）
          </li>
          <li v-if="cleanSummary.staging">
            未发布导入草稿（{{ cleanSummary.staging.staged_points }} 条）将被删除
          </li>
        </ul>
        <div class="pack-checks">
          <LCheckbox
            v-model="cleanBackup"
            label="删除前先导出 zip 备份（推荐；桌面端存到 resource-packs/ 目录，网页端直接下载）"
          />
          <LCheckbox
            v-model="cleanKeepOrphans"
            label="保留失去全部背书的孤儿知识点（默认删除）"
          />
        </div>
        <LInput
          v-model="cleanToken"
          class="pack-token"
          placeholder="更新 token（服务器设置 GEC_TABLES_UPDATE_TOKEN 时必填）"
        />
      </template>
      <template #footer>
        <LButton size="sm" @click="cleanOpen = false">取消</LButton>
        <LButton size="sm" variant="danger" :loading="cleanBusy" @click="confirmClean">
          确认删除资源包
        </LButton>
      </template>
    </LDialog>

    <IngestWizard
      v-model="wizardOpen"
      :resume-source-id="resumeSourceId"
      @published="load"
    />
  </div>
</template>

<style scoped>
.kb { display: flex; flex-direction: column; }
.kb-warn { margin: 0 0 var(--space-3); }
.kb-src { line-height: var(--lh-ui); }
.src-badge { margin-right: var(--space-1); }
.muted { color: var(--muted); }
.pack-title { margin: 0 0 var(--space-2); font-weight: 600; }
.pack-list { margin: 0 0 var(--space-3); padding-left: var(--space-5); line-height: var(--lh-body); }
.pack-checks { display: flex; flex-direction: column; gap: var(--space-2); margin: var(--space-2) 0; }
.pack-token { width: 100%; margin-top: var(--space-2); }
.pack-file { margin-bottom: var(--space-2); font-size: var(--fs-sm); color: var(--text-2); }
.pack-zip-hint { margin: 0 0 var(--space-2); font-size: var(--fs-sm); }
.import-panel {
  margin-bottom: var(--space-3);
  padding: var(--space-3);
  background: var(--surface-2);
  border: 1px solid var(--border-strong);
  border-radius: var(--radius-ctl);
}
.panel-actions { display: flex; justify-content: flex-end; gap: var(--space-2); margin-top: var(--space-3); }
.row-actions { display: flex; gap: var(--space-1); }
.loading { padding: var(--space-3) 0; font-size: var(--fs-sm); color: var(--muted); }
.err-line {
  display: flex;
  align-items: center;
  gap: var(--space-3);
  padding: var(--space-1) 0;
  font-size: var(--fs-ui);
  color: var(--danger);
}
.table-scroll { overflow-y: auto; }
.table-tall { max-height: calc(var(--space-7) * 12); }
.sel-lang { width: calc(var(--space-7) * 2); }
.sel-source { max-width: calc(var(--space-7) * 5); }
</style>
