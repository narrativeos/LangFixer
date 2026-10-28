<script setup lang="ts">
/**
 * 载体（图片）审校工作区（`MULTIMODAL.md` §7）
 *
 * 一条链走完：导入 + 探测 → 建 job（副本隔离，永不改原件）→ 预览画布
 * （`result.png` + SVG 画框，点框 ↔ 点行联动）→ 勾选（采纳 / 放弃 / 计错数）
 * → 回写（默认 Overlay）→ 导出正误表（JSON / Markdown / HTML / docx）。
 *
 * 两条硬规矩：
 * 1. **不静默**——视觉上传前压缩、网关重试、跨单元丢弃、LLM 降级等告警原样列出；
 * 2. **不画假框**——`unlocated` / 无像素落点的条目进「未能定位」区。
 *
 * 载体串行处理：视觉档走同一个 LLM 网关，并发只会互相排队并触发 503。
 */
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { ElMessage } from 'element-plus'
import { api } from '../api/client'
import { CAT_LABELS, CAT_TYPES, confBand } from '../lib/errorDisplay'
import { layerStatusLabel } from '../lib/labels'
import {
  buildReportBody,
  carrierSize,
  compressionText,
  consensusOf,
  detectorBadge,
  extractionText,
  overlayModel,
  probeBlockers,
  rowKey,
  stageProgress,
  type RowEdit,
} from '../lib/media'
import type {
  MediaApplyMode,
  MediaAnnotation,
  MediaJob,
  MediaProbes,
  MediaReportResult,
} from '../types'

/** 回写模式（决策 ①：默认 Overlay = 不改原文；`in_place` 只在含文字层时可选） */
const APPLY_MODES: { value: MediaApplyMode; label: string; hint: string }[] = [
  { value: 'overlay', label: '叠加标注', hint: '默认：正文零改动，写 mark-* 标注层' },
  { value: 'comment', label: '批注', hint: 'PDF / Word 批注语义；图片按标注层处理' },
  { value: 'in_place', label: '改原文', hint: '仅含文字层的载体可用（扁平位图不可用）' },
  { value: 'list_only', label: '只出清单', hint: '不碰载体，只出 sidecar 与正误表' },
]

const probes = ref<MediaProbes>({})
const probeError = ref('')
const blockers = computed(() => probeBlockers(probes.value as unknown as Record<string, unknown>))

/** 待处理文件（P0 只有图片适配器；PDF / Word 见 §14 P2） */
const picked = ref<File[]>([])
const fileInput = ref<HTMLInputElement | null>(null)
const applyMode = ref<MediaApplyMode>('overlay')
const level = ref(3)
const forceLlm = ref(true)
const lang = ref('zh')

const jobs = ref<MediaJob[]>([])
const activeId = ref('')
const busy = ref(false)
const statusText = ref('')
let cancelled = false

const previewUrl = ref('')
const previewNote = ref('')
const zoom = ref(1)
const selectedId = ref<number | null>(null)

/** 勾选与人工改写：键 = `${job_id}#${annotation_id}`（id 只在单个 job 内唯一） */
const checked = ref<Record<string, boolean>>({})
const edits = ref<Record<string, RowEdit>>({})
const report = ref<MediaReportResult | null>(null)
const reportBusy = ref(false)

const activeJob = computed(() => jobs.value.find((j) => j.id === activeId.value) ?? null)
const size = computed(() => carrierSize(activeJob.value?.carrier ?? undefined))
const overlay = computed(() => overlayModel(activeJob.value?.annotations ?? [], size.value))
const progress = computed(() => stageProgress(activeJob.value?.stage ?? 'queued'))
const canInPlace = computed(() => activeJob.value?.carrier?.editable === true)
const photocraftReady = computed(() => probes.value.photocraft?.ready === true)
/** 视觉上传压缩口径（探测返回值给的就显示，缺字段不编数字） */
const compression = computed(() => compressionText(probes.value.vision))
/** 可画的 id（列表据此标「未定位」，与画布同一判定 → 不会出现「列表说定位了、图上没框」） */
const drawableIds = computed(() => new Set(overlay.value.boxes.map((b) => b.id)))
/** SVG 用载体像素做 viewBox：线宽 / 字号按比例换算，缩放时不会糊成一团 */
const strokeW = computed(() => (size.value ? Math.max(2, size.value.w / 700) : 2))
const labelSize = computed(() => (size.value ? Math.max(14, size.value.w / 48) : 14))

/** 检测层贡献（与文本检测的 ResultsPanel 同一词表，避免「图上与列表两套叫法」） */
const LAYER_LABELS: Record<string, string> = {
  rule: '规则',
  decision: '决策模型',
  knowledge: '知识召回',
  feature: '特征模型',
  llm: 'LLM 兜底',
  statistical: '统计模型',
}

function layerText(l: { layer: string; status: string; found: number }): string {
  const base = LAYER_LABELS[l.layer] ?? l.layer
  if (l.status === 'ran') return l.found > 0 ? `${base} ${l.found} 条` : `${base} 未命中`
  return `${base} ${layerStatusLabel(l.status)}`
}

function errText(e: unknown): string {
  if (e instanceof Error) return e.message
  const data = (e as { response?: { data?: { error?: string } } })?.response?.data
  return data?.error ?? String(e)
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

function labelOf(job: MediaJob): string {
  const i = jobs.value.findIndex((j) => j.id === job.id)
  return `图片${i + 1} · ${job.filename}`
}

async function loadProbes() {
  try {
    probes.value = await api.mediaProbes()
    probeError.value = ''
  } catch (e) {
    probeError.value = errText(e)
  }
}

/** 接上后端已有 job（内存表在服务重启前一直在）：刷新页面不必重跑载体；
 *  未终态的 job 顺带续上轮询。后端未开启多模态时静默为空列表（不弹错）。
 *
 * 排序必须按创建时间**升序**：后端 `list()` 是倒序，而正误表里的「图片1 / 图片2」
 * 按提交顺序（升序）编号——两边不一致会让人对着表找不到图。
 * 默认打开有结果的那个载体（空载体只当第二、第三个 tab）。 */
async function loadJobs() {
  if (jobs.value.length) return
  try {
    const list = (await api.mediaJobs()).sort((a, b) => a.created_at.localeCompare(b.created_at))
    if (!list.length) return
    jobs.value = list
    const done = (j: MediaJob) => ['ready', 'applied'].includes(j.stage)
    const first = list.find((j) => done(j) && j.annotations.length > 0) ?? list.find(done) ?? list[0]
    if (first) await openJob(first.id)
    for (const j of list) {
      if (!done(j) && j.stage !== 'failed') void pollUntilDone(j.id)
    }
  } catch {
    /* 未启用 / 不可达：面板保持空态，探测卡片已说明原因 */
  }
}

function onPick(ev: Event) {
  const list = (ev.target as HTMLInputElement).files
  picked.value = list ? Array.from(list) : []
}

function pickFiles() {
  fileInput.value?.click()
}

/** 建 job → 轮询到终态（逐个载体串行：视觉档共用一个网关） */
async function runAll() {
  if (!picked.value.length) {
    ElMessage.warning('先选择要审校的图片')
    return
  }
  busy.value = true
  cancelled = false
  report.value = null
  try {
    for (const file of picked.value) {
      if (cancelled) break
      statusText.value = `上传 ${file.name}…`
      const data = new Uint8Array(await file.arrayBuffer())
      const created = await api.mediaCreateJob({
        filename: file.name,
        data,
        apply_mode: applyMode.value,
        lang: lang.value,
        level: level.value,
        force_llm: forceLlm.value,
      })
      upsertJob(created)
      await openJob(created.id)
      const final = await pollUntilDone(created.id)
      if (final.stage === 'failed') {
        ElMessage.error(`${file.name} 审校失败：${final.error ?? '未知原因'}`)
      }
    }
    if (!cancelled) statusText.value = '全部载体处理完毕'
  } catch (e) {
    statusText.value = ''
    ElMessage.error(errText(e))
  } finally {
    busy.value = false
  }
}

function upsertJob(job: MediaJob) {
  const i = jobs.value.findIndex((j) => j.id === job.id)
  if (i >= 0) jobs.value[i] = job
  else jobs.value.push(job)
}

/** 轮询到终态（ready / applied / failed）；2s 一跳，取消标志让切页不再打后端 */
async function pollUntilDone(id: string): Promise<MediaJob> {
  for (;;) {
    const job = await api.mediaJob(id)
    upsertJob(job)
    if (['ready', 'applied', 'failed'].includes(job.stage) || cancelled) return job
    statusText.value = `${job.filename}：${stageProgress(job.stage).label}…`
    await sleep(2000)
  }
}

/** 打开 / 切换 job：重置选中与预览 */
async function openJob(id: string) {
  activeId.value = id
  selectedId.value = null
  report.value = null
  await loadPreview(id)
}

async function loadPreview(id: string) {
  if (previewUrl.value) {
    URL.revokeObjectURL(previewUrl.value)
    previewUrl.value = ''
  }
  try {
    const blob = await api.mediaPreviewBlob(id)
    previewUrl.value = URL.createObjectURL(blob)
    previewNote.value = ''
  } catch (e) {
    previewNote.value = `预览不可用：${errText(e)}`
  }
}

function select(id: number | null) {
  selectedId.value = id
}

function keyOf(a: MediaAnnotation): string {
  return rowKey(activeJob.value?.id ?? '', a.id)
}

function isChecked(a: MediaAnnotation): boolean {
  return checked.value[keyOf(a)] !== false
}

function setChecked(a: MediaAnnotation, on: boolean) {
  checked.value[keyOf(a)] = on
}

function setAllChecked(on: boolean) {
  for (const a of activeJob.value?.annotations ?? []) checked.value[keyOf(a)] = on
}

function editOf(a: MediaAnnotation): RowEdit {
  const k = keyOf(a)
  if (!edits.value[k]) edits.value[k] = {}
  return edits.value[k]
}

/** 该条原文来自哪个文本单元、抽取置信度多少（视觉档必须露这个，否则误读无从判断） */
function unitConfidence(a: MediaAnnotation): number | null {
  const u = activeJob.value?.units.find((x) => x.id === a.unit_id)
  return u ? u.confidence : null
}

function locateReason(a: MediaAnnotation): string {
  if (a.region.region === 'unlocated') return '未能定位（置信度不足 / 跨单元）'
  if (a.region.region === 'run_ref') return '文档对象引用（位图上无落点）'
  return '无像素落点（不画框）'
}

async function applyWrite() {
  const job = activeJob.value
  if (!job) return
  busy.value = true
  try {
    const next = await api.mediaApply(job.id, applyMode.value)
    upsertJob(next)
    await loadPreview(job.id)
    if (next.stage === 'applied') {
      ElMessage.success(`已回写：${next.artifacts.editable ?? 'result'}（原件未改动）`)
    } else {
      ElMessage.warning(`回写未完成：${next.error ?? '见告警'}（结果仍在 sidecar）`)
    }
  } catch (e) {
    ElMessage.error(errText(e))
  } finally {
    busy.value = false
  }
}

/** 导出正误表：勾选状态按 job 作用域下发（`annotation_id` 跨 job 会重号） */
async function exportReport() {
  if (!jobs.value.length) return
  reportBusy.value = true
  try {
    const map: Record<string, boolean> = {}
    for (const j of jobs.value) {
      for (const a of j.annotations) {
        const k = rowKey(j.id, a.id)
        map[k] = checked.value[k] !== false
      }
    }
    const body = buildReportBody({
      jobIds: jobs.value.filter((j) => ['ready', 'applied'].includes(j.stage)).map((j) => j.id),
      checked: map,
      edits: edits.value,
    })
    report.value = await api.mediaReport(body)
    ElMessage.success(
      `正误表：候选 ${report.value.candidates} 条 → 导出 ${report.value.exported} 条`,
    )
  } catch (e) {
    ElMessage.error(errText(e))
  } finally {
    reportBusy.value = false
  }
}

/** 导出产物落盘：Web 端浏览器下载；桌面端弹原生「另存为」后写盘
 *  （两端同一入口，`api.downloadMediaReport` 内部分支） */
async function saveReport(format: 'json' | 'md' | 'html' | 'docx') {
  if (!report.value) return
  try {
    const saved = await api.downloadMediaReport(
      report.value.report_id,
      format,
      `正误表-${report.value.report_id}.${format === 'md' ? 'md' : format}`,
    )
    if (saved) ElMessage.success(`已导出 ${saved.filename}（${saved.size_bytes} 字节）`)
  } catch (e) {
    ElMessage.error(errText(e))
  }
}

onMounted(() => {
  void loadProbes()
  void loadJobs()
})
onBeforeUnmount(() => {
  cancelled = true
  if (previewUrl.value) URL.revokeObjectURL(previewUrl.value)
})
</script>

<template>
  <el-card shadow="never" class="media-card">
    <template #header>
      <div class="card-header">
        <span class="card-title">载体审校（图片）</span>
        <span class="media-sub">副本隔离 · 默认叠加标注，不改原件</span>
        <span class="spacer"></span>
        <el-button size="small" text @click="loadProbes">重新探测</el-button>
      </div>
    </template>

    <el-alert v-if="probeError" type="error" :closable="false" show-icon :title="`探测失败：${probeError}`" />
    <el-alert
      v-else-if="blockers.length"
      type="warning"
      :closable="false"
      show-icon
      title="载体审校能力不全（结果会带告警，不会静默降级）"
    >
      <ul class="blocker-list">
        <li v-for="b in blockers" :key="b">{{ b }}</li>
      </ul>
    </el-alert>

    <div class="probe-row">
      <el-tag size="small" :type="photocraftReady ? 'success' : 'info'" effect="plain">
        PhotoCraft {{ photocraftReady ? probes.photocraft?.version ?? '就绪' : '不可用（只出清单）' }}
      </el-tag>
      <el-tag size="small" :type="probes.vision?.ready ? 'success' : 'info'" effect="plain">
        视觉档 {{ probes.vision?.ready ? probes.vision?.model : '未就绪' }}
        <template v-if="compression">· {{ compression }}</template>
      </el-tag>
      <el-tag size="small" effect="plain">
        WordCraft 载体 {{ probes.wordcraft?.ready ? '就绪' : '未实现（P2；图片模式与 docx 导出不受影响）' }}
      </el-tag>
    </div>

    <div class="import-row">
      <input ref="fileInput" type="file" accept="image/*" multiple class="hidden-file" @change="onPick" />
      <el-button @click="pickFiles">选择图片</el-button>
      <span class="file-names">
        {{ picked.map((f) => f.name).join('、') || '未选择（png / jpg / webp / psd…，逐个串行处理）' }}
      </span>
      <span class="spacer"></span>
      <label class="param">语言
        <el-select v-model="lang" size="small" style="width: 92px">
          <el-option label="中文" value="zh" />
          <el-option label="English" value="en" />
          <el-option label="日本語" value="ja" />
        </el-select>
      </label>
      <label class="param">响应级别
        <el-select v-model="level" size="small" style="width: 118px">
          <el-option :value="1" label="1 规则" />
          <el-option :value="2" label="2 + 决策模型" />
          <el-option :value="3" label="3 + LLM 兜底" />
        </el-select>
      </label>
      <el-switch v-model="forceLlm" size="small" active-text="强制 LLM 兜底" />
      <el-button type="primary" :loading="busy" @click="runAll">开始审校</el-button>
    </div>

    <div v-if="jobs.length" class="job-tabs">
      <el-button
        v-for="j in jobs"
        :key="j.id"
        size="small"
        :class="{ 'tab-active': j.id === activeId }"
        @click="openJob(j.id)"
      >
        {{ labelOf(j) }}
        <el-tag
          size="small"
          round
          :type="j.stage === 'failed' ? 'danger' : j.stage === 'ready' || j.stage === 'applied' ? 'success' : 'warning'"
        >
          {{ stageProgress(j.stage).label }}
        </el-tag>
      </el-button>
    </div>

    <template v-if="activeJob">
      <el-progress
        v-if="progress.running || progress.failed"
        :percentage="progress.percent"
        :status="progress.failed ? 'exception' : undefined"
        :stroke-width="6"
      />
      <div class="job-meta">
        <el-tag size="small" effect="plain">{{ statusText || progress.label }}</el-tag>
        <el-tag size="small" effect="plain">
          载体 {{ size ? `${size.w}×${size.h}` : '尺寸未知' }}
          {{ activeJob.carrier?.editable ? '· 含文字层' : '· 无文字层' }}
        </el-tag>
        <el-tag size="small" effect="plain">
          抽取 {{ extractionText(activeJob.extraction, activeJob.extraction_model) }}
        </el-tag>
        <el-tag size="small" effect="plain">文本单元 {{ activeJob.units.length }}</el-tag>
        <el-tag size="small" effect="plain">
          标注 {{ activeJob.annotations.length }}（画框 {{ overlay.boxes.length }} · 未定位
          {{ overlay.undrawable.length }}）
        </el-tag>
        <el-tag v-for="l in activeJob.layers" :key="l.layer" size="small" effect="plain">
          {{ layerText(l) }}
        </el-tag>
      </div>

      <el-alert v-if="activeJob.error" type="error" :closable="false" show-icon :title="activeJob.error" />
      <div v-if="activeJob.warnings.length" class="warn-box">
        <div v-for="(w, i) in activeJob.warnings" :key="i" class="warn-item">⚠ {{ w }}</div>
      </div>

      <div class="workspace">
        <section class="canvas-wrap">
          <div class="canvas-tools">
            <el-button size="small" text @click="zoom = Math.max(0.2, zoom / 1.25)">−</el-button>
            <el-button size="small" text @click="zoom = 1">{{ Math.round(zoom * 100) }}%</el-button>
            <el-button size="small" text @click="zoom = Math.min(6, zoom * 1.25)">＋</el-button>
            <span class="hint">点框 = 选中右侧那条；框上数字 = 列表序号</span>
          </div>
          <div class="canvas-scroll">
            <div class="canvas-inner" :style="{ transform: `scale(${zoom})` }">
              <img v-if="previewUrl" :src="previewUrl" class="carrier-img" alt="载体预览" />
              <svg
                v-if="previewUrl && size"
                class="overlay"
                :viewBox="`0 0 ${size.w} ${size.h}`"
                preserveAspectRatio="none"
              >
                <g v-for="b in overlay.boxes" :key="b.id" @click.stop="select(b.id)">
                  <rect
                    :x="b.box.x"
                    :y="b.box.y"
                    :width="b.box.w"
                    :height="b.box.h"
                    :stroke="b.color"
                    :fill="b.color"
                    fill-opacity="0.12"
                    :stroke-width="b.id === selectedId ? strokeW * 2 : strokeW"
                  />
                  <text
                    :x="b.box.x"
                    :y="Math.max(b.box.y - strokeW * 2, labelSize)"
                    :font-size="labelSize"
                    :fill="b.color"
                    class="box-label"
                  >
                    {{ b.no }}
                  </text>
                </g>
              </svg>
            </div>
            <p v-if="previewNote" class="preview-note">{{ previewNote }}</p>
            <p v-else-if="!previewUrl" class="preview-note">预览加载中…</p>
            <p v-else-if="!size" class="preview-note">
              载体尺寸未知 → 不画框（坐标空间立不起来），条目见右侧列表
            </p>
          </div>
        </section>

        <section class="list-wrap">
          <div class="list-tools">
            <span class="hint">勾选 = 进正误表；取消 = 放弃该条</span>
            <span class="spacer"></span>
            <el-button size="small" text @click="setAllChecked(true)">全选</el-button>
            <el-button size="small" text @click="setAllChecked(false)">全不选</el-button>
          </div>
          <div class="ann-list">
            <div
              v-for="(a, i) in activeJob.annotations"
              :key="a.id"
              class="ann-row"
              :class="{ sel: a.id === selectedId, off: !isChecked(a) }"
              @click="select(a.id)"
            >
              <el-checkbox
                :model-value="isChecked(a)"
                @change="(v: boolean) => setChecked(a, v)"
                @click.stop
              />
              <span class="ann-no">{{ i + 1 }}</span>
              <div class="ann-body">
                <div class="ann-text">
                  <code>{{ a.error.original }}</code>
                  <span class="arrow">→</span>
                  <code class="fix">{{ a.error.correction || '（无建议）' }}</code>
                </div>
                <div class="ann-msg">{{ a.error.message }}</div>
                <div class="ann-badges">
                  <el-tag size="small" :type="CAT_TYPES[a.error.category ?? 'other'] ?? 'info'">
                    {{ CAT_LABELS[a.error.category ?? 'other'] ?? a.error.category ?? '其他' }}
                  </el-tag>
                  <el-tag size="small" :type="detectorBadge(a.error).type">
                    {{ detectorBadge(a.error).text }}
                  </el-tag>
                  <el-tag v-if="consensusOf(a.error).length > 1" size="small" type="success">
                    共识 ×{{ consensusOf(a.error).length }}
                  </el-tag>
                  <el-tag size="small" :type="confBand(a.error).type">
                    {{ confBand(a.error).label }} {{ a.error.confidence.toFixed(2) }}
                  </el-tag>
                  <el-tag v-if="unitConfidence(a) !== null" size="small" effect="plain">
                    抽取 conf {{ unitConfidence(a)?.toFixed(2) }}
                  </el-tag>
                  <code v-if="a.error.rule_id" class="rule-id">{{ a.error.rule_id }}</code>
                  <el-tag v-if="!drawableIds.has(a.id)" size="small" type="warning">
                    {{ locateReason(a) }}
                  </el-tag>
                  <el-tag v-if="a.locate_warning" size="small" type="warning">
                    {{ a.locate_warning }}
                  </el-tag>
                </div>
                <div class="ann-edit" @click.stop>
                  <label class="param">计错数
                    <el-select
                      v-model="editOf(a).counted"
                      size="small"
                      style="width: 88px"
                      placeholder="计"
                    >
                      <el-option label="计" value="计" />
                      <el-option label="不计" value="不计" />
                    </el-select>
                  </label>
                  <el-input
                    v-model="editOf(a).suggestion"
                    size="small"
                    placeholder="改写建议（留空 = 用机器建议）"
                    style="max-width: 260px"
                  />
                </div>
              </div>
            </div>
            <p v-if="!activeJob.annotations.length" class="empty-hint">
              本载体没有检出条目{{ activeJob.extraction === 'none' ? '（文本抽取为空：见上方告警）' : '' }}
            </p>
          </div>
        </section>
      </div>

      <div class="action-row">
        <span class="param-label">回写模式</span>
        <el-radio-group v-model="applyMode" size="small">
          <el-radio
            v-for="m in APPLY_MODES"
            :key="m.value"
            :value="m.value"
            :disabled="m.value === 'in_place' && !canInPlace"
            :title="m.hint"
          >
            {{ m.label }}
          </el-radio>
        </el-radio-group>
        <span class="spacer"></span>
        <el-button
          size="small"
          :disabled="!['ready', 'applied'].includes(activeJob.stage) || !photocraftReady"
          :loading="busy"
          @click="applyWrite"
        >
          应用并回写
        </el-button>
        <el-button
          size="small"
          type="primary"
          :loading="reportBusy"
          :disabled="!jobs.some((j) => ['ready', 'applied'].includes(j.stage))"
          @click="exportReport"
        >
          导出正误表
        </el-button>
      </div>
      <p v-if="applyMode === 'in_place' && !canInPlace" class="hint">
        本载体无文字层（扁平位图）→「改原文」不可用，只能用叠加标注
      </p>

      <div v-if="report" class="report-box">
        <div class="report-head">
          正误表 {{ report.report_id }}：候选 {{ report.candidates }} 条 → 导出
          {{ report.exported }} 条（剔除 {{ report.dropped_ids.length }} 条）
        </div>
        <div class="report-links">
          <el-button size="small" text @click="saveReport('docx')">Word（docx）</el-button>
          <el-button size="small" text @click="saveReport('html')">HTML</el-button>
          <el-button size="small" text @click="saveReport('md')">Markdown</el-button>
          <el-button size="small" text @click="saveReport('json')">JSON</el-button>
        </div>
        <div v-for="(w, i) in report.warnings" :key="i" class="warn-item">⚠ {{ w }}</div>
      </div>

      <details class="artifacts">
        <summary>产物路径（原件未改动）</summary>
        <ul>
          <li>副本：{{ activeJob.artifacts.source }}</li>
          <li>sidecar：{{ activeJob.artifacts.annotations_json }}</li>
          <li v-if="activeJob.artifacts.editable">可继续编辑：{{ activeJob.artifacts.editable }}</li>
          <li v-if="activeJob.artifacts.preview">预览：{{ activeJob.artifacts.preview }}</li>
        </ul>
      </details>
    </template>

    <p v-else class="empty-hint">
      选择图片后点「开始审校」：探测载体 → 取文本（无文字层走视觉档）→ 检测 → 回映射落点。
      原件不会被修改，所有产物在 job 独立目录里。
    </p>
  </el-card>
</template>

<style scoped>
.media-card { height: 100%; }
.media-sub { font-size: 12px; color: var(--el-text-color-secondary); margin-left: 8px; }
.spacer { flex: 1; }
.hidden-file { display: none; }
.blocker-list { margin: 4px 0 0 18px; padding: 0; font-size: 12px; }
.probe-row,
.job-meta,
.ann-badges { display: flex; flex-wrap: wrap; gap: 6px; align-items: center; }
.probe-row { margin-bottom: 10px; }
.import-row { display: flex; flex-wrap: wrap; gap: 8px; align-items: center; margin-bottom: 10px; }
.file-names {
  font-size: 12px;
  color: var(--el-text-color-secondary);
  max-width: 40%;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.param { display: inline-flex; align-items: center; gap: 6px; font-size: 12px; }
.param-label { font-size: 12px; color: var(--el-text-color-secondary); }
.job-tabs { display: flex; flex-wrap: wrap; gap: 6px; margin-bottom: 10px; }
.tab-active { border-color: var(--el-color-primary); color: var(--el-color-primary); }
.job-meta { margin: 8px 0; }
.warn-box {
  border: 1px solid var(--el-color-warning-light-5);
  border-radius: 6px;
  padding: 6px 10px;
  margin-bottom: 10px;
  background: var(--el-color-warning-light-9);
}
.warn-item { font-size: 12px; line-height: 1.6; }
.workspace { display: grid; grid-template-columns: minmax(0, 1.15fr) minmax(320px, 1fr); gap: 12px; }
.canvas-wrap { min-width: 0; }
.canvas-tools { display: flex; align-items: center; gap: 6px; margin-bottom: 6px; }
.hint { font-size: 12px; color: var(--el-text-color-secondary); }
.canvas-scroll {
  max-height: 62vh;
  overflow: auto;
  border: 1px solid var(--el-border-color);
  border-radius: 6px;
  background: var(--el-fill-color-lighter);
}
.canvas-inner { position: relative; width: 100%; transform-origin: 0 0; }
.carrier-img { display: block; width: 100%; height: auto; }
/* SVG 铺满 img 的显示框，viewBox = 载体像素 → 框坐标与 sidecar 逐字一致，缩放交给它 */
.overlay { position: absolute; inset: 0; width: 100%; height: 100%; }
.overlay g { cursor: pointer; }
.box-label { font-weight: 700; paint-order: stroke; stroke: #fff; stroke-width: 3px; }
.preview-note { padding: 10px; font-size: 12px; color: var(--el-text-color-secondary); }
.list-wrap { min-width: 0; }
.list-tools { display: flex; align-items: center; gap: 6px; margin-bottom: 6px; }
.ann-list { max-height: 62vh; overflow: auto; display: flex; flex-direction: column; gap: 6px; }
.ann-row {
  display: flex;
  gap: 8px;
  padding: 8px;
  border: 1px solid var(--el-border-color-lighter);
  border-radius: 6px;
  cursor: pointer;
}
.ann-row.sel { border-color: var(--el-color-primary); background: var(--el-color-primary-light-9); }
.ann-row.off { opacity: 0.5; }
.ann-no { font-size: 12px; color: var(--el-text-color-secondary); min-width: 18px; }
.ann-body { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 4px; }
.ann-text { font-size: 13px; }
.ann-text code { background: var(--el-fill-color); padding: 1px 4px; border-radius: 4px; }
.ann-text code.fix { background: var(--el-color-success-light-9); }
.arrow { margin: 0 4px; color: var(--el-text-color-secondary); }
.ann-msg { font-size: 12px; color: var(--el-text-color-secondary); }
.rule-id { font-size: 11px; color: var(--el-text-color-secondary); }
.ann-edit { display: flex; flex-wrap: wrap; gap: 8px; align-items: center; }
.action-row { display: flex; flex-wrap: wrap; gap: 8px; align-items: center; margin-top: 12px; }
.report-box {
  margin-top: 10px;
  border: 1px solid var(--el-border-color-lighter);
  border-radius: 6px;
  padding: 8px 10px;
}
.report-head { font-size: 13px; margin-bottom: 6px; }
.report-links { display: flex; gap: 10px; margin-bottom: 6px; }
.report-links a { color: var(--el-color-primary); font-size: 13px; }
.artifacts { margin-top: 10px; font-size: 12px; color: var(--el-text-color-secondary); }
.artifacts ul { margin: 4px 0 0 18px; padding: 0; }
.empty-hint { font-size: 13px; color: var(--el-text-color-secondary); }
</style>
