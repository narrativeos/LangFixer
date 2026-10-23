<script setup lang="ts">
import { ref, onMounted, watch } from 'vue'
import { ElMessage } from 'element-plus'
import { errMsg } from '../lib/err'

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
  } catch (e) {
    error.value = errMsg(e)
  }
}

const statusLabel: Record<string, string> = {
  running: '挖掘中',
  mined: '待审阅发布',
  failed: '已中断',
}

async function reloadPoints() {
  points.value = await api.listKnowledgePoints(lang.value, sourceFilter.value || undefined)
}

function sourceLabel(s: { source_id: string; title?: string; location?: string }): string {
  const t = s.title || s.source_id
  return s.location ? `${t} · ${s.location}` : t
}

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
      ElMessage.info('导出已取消')
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
    ElMessage.success({
      message: `资源包已导出：${parts.join('，')}`,
      duration: 6000,
      showClose: true,
    })
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
    ElMessage.success({
      message:
        `资源包已删除：知识点 −${r.knowledge.points_removed}，规则行 −${r.rules_removed}，` +
        `决策数据文件 −${r.aux_removed}${backupNote}`,
      duration: 8000,
      showClose: true,
    })
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
    ElMessage.success(
      `资源包已导入：新知识点 +${r.knowledge.points_created}，追加背书 +${r.knowledge.points_endorsed}，` +
        `规则行 +${r.rules_added}（替换 ${r.rules_replaced}）`,
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
    <el-card shadow="never">
      <template #header>
        <div class="card-header">
          <span class="card-title">知识源注册表</span>
          <span class="sub">
            知识源只登记元数据（书名/作者/版次/许可范围）；正文内容不随产品内置发布
          </span>
          <span class="spacer"></span>
          <el-button size="small" @click="openImport()">导入资源包</el-button>
          <el-button type="primary" size="small" @click="openWizard()">导入知识源</el-button>
        </div>
      </template>
      <el-alert
        v-if="sources.length > 0 && sources.some((s) => s.license_scope === 'internal_test')"
        type="warning"
        :closable="false"
        show-icon
        title="注意：存在「仅内部测试」许可范围的知识源，其内容不得对外发布"
        class="kb-warn"
      />
      <el-table v-if="sources.length > 0" :data="sources">
        <el-table-column prop="source_id" label="ID" width="170" />
        <el-table-column prop="title" label="标题" min-width="240" />
        <el-table-column prop="author" label="作者" width="110">
          <template #default="{ row }">{{ row.author || '—' }}</template>
        </el-table-column>
        <el-table-column prop="edition" label="版次" width="90">
          <template #default="{ row }">{{ row.edition || '—' }}</template>
        </el-table-column>
        <el-table-column prop="lang" label="语言" width="70" />
        <el-table-column label="许可范围" width="120">
          <template #default="{ row }">
            <el-tag size="small" :type="row.license_scope === 'public' ? 'success' : 'warning'">
              {{ row.license_scope || 'internal_test' }}
            </el-tag>
          </template>
        </el-table-column>
        <el-table-column prop="point_count" label="关联知识点" width="100" align="right" />
        <el-table-column label="资源包" width="150">
          <template #default="{ row }">
            <el-button size="small" @click="exportPack(row)">导出</el-button>
            <el-button size="small" type="danger" plain @click="openClean(row)">删除</el-button>
          </template>
        </el-table-column>
      </el-table>
      <el-empty
        v-else
        description="未加载知识源 — 部署环境未提供知识库目录（GEC_KNOWLEDGE_DIR）时归因结果仅含表级信息"
        :image-size="80"
      />
    </el-card>

    <el-card v-if="sessions.length > 0" shadow="never">
      <template #header>
        <div class="card-header">
          <span class="card-title">进行中的导入会话</span>
          <span class="sub">正文挖掘在后台进行；草稿发布前不影响归因</span>
        </div>
      </template>
      <el-table :data="sessions" size="small">
        <el-table-column prop="source_id" label="源 ID" width="180" />
        <el-table-column prop="title" label="书名" min-width="200" />
        <el-table-column prop="lang" label="语言" width="70" />
        <el-table-column label="状态" width="110">
          <template #default="{ row }">
            <el-tag
              size="small"
              :type="row.status === 'failed' ? 'danger' : row.status === 'mined' ? 'success' : 'info'"
            >
              {{ statusLabel[row.status] || row.status }}
            </el-tag>
          </template>
        </el-table-column>
        <el-table-column label="挖掘进度" width="120">
          <template #default="{ row }">{{ row.chunks_done }}/{{ row.chunks_total }}</template>
        </el-table-column>
        <el-table-column prop="staged_points" label="草稿" width="80" align="right" />
        <el-table-column label="操作" width="150">
          <template #default="{ row }">
            <el-button size="small" @click="openWizard(row.source_id)">查看</el-button>
            <el-button size="small" type="danger" plain @click="discardSession(row)">放弃</el-button>
          </template>
        </el-table-column>
      </el-table>
    </el-card>

    <el-card shadow="never">
      <template #header>
        <div class="card-header">
          <span class="card-title">知识点</span>
          <span class="spacer"></span>
          <el-select v-model="lang" size="small" style="width: 90px">
            <el-option label="zh" value="zh" />
            <el-option label="en" value="en" />
            <el-option label="ja" value="ja" />
          </el-select>
          <el-select v-model="sourceFilter" size="small" style="width: 260px" clearable placeholder="全部知识源">
            <el-option value="" label="全部知识源" />
            <el-option v-for="s in sources" :key="s.source_id" :label="s.title" :value="s.source_id" />
          </el-select>
        </div>
      </template>
      <div v-if="loading" class="loading">加载中…</div>
      <el-alert v-else-if="error" type="error" :closable="false" :title="error" />
      <el-table v-else-if="points.length > 0" :data="points" size="small" max-height="560">
        <el-table-column prop="point_id" label="点 ID" width="130" />
        <el-table-column prop="error_type" label="错误类型" width="120" />
        <el-table-column prop="section" label="小节" width="80">
          <template #default="{ row }">{{ row.section || '—' }}</template>
        </el-table-column>
        <el-table-column prop="title" label="标题" min-width="180" />
        <el-table-column prop="cefr" label="CEFR" width="70">
          <template #default="{ row }">{{ row.cefr || '—' }}</template>
        </el-table-column>
        <el-table-column prop="remedial" label="补救内容" min-width="260" show-overflow-tooltip />
        <el-table-column label="知识源（1:N）" min-width="260">
          <template #default="{ row }">
            <div v-for="(s, i) in row.sources ?? []" :key="i" class="kb-src">
              <el-tag
                v-if="i === 0 && (row.sources ?? []).length > 1"
                size="small"
                type="success"
                class="src-badge"
              >
                多源背书 ×{{ (row.sources ?? []).length }}
              </el-tag>
              {{ sourceLabel(s) }}
            </div>
            <span v-if="!(row.sources ?? []).length" class="muted">—</span>
          </template>
        </el-table-column>
      </el-table>
      <el-empty v-else description="该语言暂无知识点（或所选知识源下无关联知识点）" :image-size="80" />
    </el-card>

    <!-- 资源包删除：盘点预览 + 确认（不可逆，展示将被删除的组成） -->
    <el-dialog v-model="cleanOpen" title="删除资源包（不可逆，默认先导出备份）" width="520px">
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
        <el-checkbox v-model="cleanBackup">
          删除前先导出 zip 备份（推荐；桌面端存到 resource-packs/ 目录，网页端直接下载）
        </el-checkbox>
        <br />
        <el-checkbox v-model="cleanKeepOrphans">
          保留失去全部背书的孤儿知识点（默认删除）
        </el-checkbox>
        <el-input
          v-model="cleanToken"
          size="small"
          class="pack-token"
          placeholder="更新 token（服务器设置 GEC_TABLES_UPDATE_TOKEN 时必填）"
        />
      </template>
      <template #footer>
        <el-button @click="cleanOpen = false">取消</el-button>
        <el-button type="danger" :loading="cleanBusy" @click="confirmClean">
          确认删除资源包
        </el-button>
      </template>
    </el-dialog>

    <!-- 资源包导入：选择文件（.zip 压缩包 / .json）→ 预览组成 → 确认导入 -->
    <el-dialog v-model="importOpen" title="导入资源包" width="520px">
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
      <el-input
        v-if="importPackPreview || importZipInfo"
        v-model="importToken"
        size="small"
        class="pack-token"
        placeholder="更新 token（服务器设置 GEC_TABLES_UPDATE_TOKEN 时必填）"
      />
      <template #footer>
        <el-button @click="importOpen = false">取消</el-button>
        <el-button
          type="primary"
          :disabled="!importPackPreview && !importZipInfo"
          :loading="importBusy"
          @click="confirmImport"
        >
          确认导入
        </el-button>
      </template>
    </el-dialog>

    <IngestWizard
      v-model="wizardOpen"
      :resume-source-id="resumeSourceId"
      @published="load"
    />
  </div>
</template>

<style scoped>
.kb { display: flex; flex-direction: column; gap: 14px; }
.kb-warn { margin-bottom: 12px; }
.kb-src { line-height: 1.6; }
.src-badge { margin-right: 6px; }
.muted { color: var(--el-text-color-secondary); }
.pack-title { margin: 0 0 8px; font-weight: 600; }
.pack-list { margin: 0 0 12px; padding-left: 20px; line-height: 1.8; }
.pack-token { margin-top: 10px; }
.pack-file { margin-bottom: 10px; }
.pack-zip-hint { margin: 0 0 10px; font-size: 12px; }
</style>