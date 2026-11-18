<script setup lang="ts">
/**
 * 载体（图片 / PDF / Word）审校工作区（`MULTIMODAL.md` §7、§15）
 *
 * 一条链走完：导入 + 探测 → 选模式（简单 = 零 CLI，只出清单/正误表；高级 = 三家适配器，可回写）
 * → 建 job（副本隔离，永不改原件）→ 预览面（`image_canvas` 才画框；`doc_text` 只按段落定位）
 * → 勾选（采纳 / 放弃 / 计错数）→ 回写（默认 Overlay）→ 导出正误表（JSON / Markdown / HTML / docx）。
 *
 * 两条硬规矩：
 * 1. **不静默**——视觉上传前压缩、网关重试、跨单元丢弃、LLM 降级等告警原样列出；
 * 2. **不画假框**——`unlocated` / 无像素落点的条目进「未能定位」区。
 *
 * 载体串行处理：视觉档走同一个 LLM 网关，并发只会互相排队并触发 503。
 */
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import {
  LButton,
  LCheckbox,
  LEmpty,
  LInput,
  LPopover,
  LProgress,
  LSection,
  LSegmented,
  LSelect,
  LSwitch,
  LTag,
  toast,
} from '../ui'
import { api } from '../api/client'
import { CAT_LABELS, confBand } from '../lib/errorDisplay'
import { layerStatusLabel, modeLabel, plainLayerLabel } from '../lib/labels'
import {
  advancedBlockNote,
  buildReportBody,
  carrierSize,
  compressionText,
  consensusOf,
  detectorBadge,
  extractionText,
  modeCardsFor,
  overlayModel,
  previewIsCanvas,
  probeBlockers,
  rowKey,
  stageProgress,
  type RowEdit,
} from '../lib/media'
import type {
  MediaApplyMode,
  MediaAnnotation,
  MediaJob,
  MediaMode,
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

/**
 * 工作模式（§15.1）：决定「谁来读这份载体」。
 * 可用性只认后端 `probes.modes` 的口径——前端自己判 CLI 在不在，就会与后端 gate 漂开
 * （按钮能点、后端 503）。
 */
const MODES: { value: MediaMode; label: string; hint: string }[] = [
  { value: 'simple', label: '简单', hint: '零外部工具：只出清单与正误表，不回写载体' },
  { value: 'advanced', label: '高级', hint: '交给 PhotoCraft / PdfCraft / WordCraft：可回写载体' },
]

const LANG_OPTIONS = [
  { value: 'zh', label: '中文' },
  { value: 'en', label: 'English' },
  { value: 'ja', label: '日本語' },
]
const LEVEL_OPTIONS = [1, 2, 3].map((l) => ({ value: l, label: modeLabel(l) }))

/** 载体大类 → 正误表里的标号名词（与后端 `carrier_label` 同一口径） */
const CARRIER_NOUNS: Record<string, string> = {
  image: '图片',
  pdf: 'PDF',
  docx: '文档',
  other: '载体',
}

const probes = ref<MediaProbes>({})
const probeError = ref('')
const blockers = computed(() => probeBlockers(probes.value as unknown as Record<string, unknown>))

/** 待处理载体（图片 / PDF / Word；PDF 与 Word 见 §14 P2，现已接入） */
const picked = ref<File[]>([])
const fileInput = ref<HTMLInputElement | null>(null)
const applyMode = ref<MediaApplyMode>('overlay')
/** 工作模式：默认高档（与设置里的 `media.mode` 同默认）；不可用时不静默换档，而是把原因摊开 */
const mode = ref<MediaMode>('advanced')
/** 用户手动选过模式 → 之后不再被探测里的默认值覆盖 */
const modeTouched = ref(false)
const level = ref(3)
const forceLlm = ref(true)
const lang = ref('zh')

/** LSelect 吐 string|number，回到窄类型 */
const levelSel = computed({
  get: () => level.value,
  set: (v: string | number) => {
    level.value = Number(v)
  },
})
function modeChange(v: MediaMode) {
  mode.value = v
  modeTouched.value = true
}
const modeHint = computed(() => MODES.find((m) => m.value === mode.value)?.hint ?? '')
const applyHint = computed(() => APPLY_MODES.find((m) => m.value === applyMode.value)?.hint ?? '')
/** 回写模式分段：不可用时不静默选中（改原文要文字层；简单模式没有回写原语） */
function setApplyMode(v: MediaApplyMode) {
  if (activeJob.value?.mode === 'simple') return
  if (v === 'in_place' && !canInPlace.value) return
  applyMode.value = v
}

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
/** 选了高档但某些载体的高档不可用 → 原因（含退路）；空串 = 都能跑 */
const modeNote = computed(() =>
  advancedBlockNote(probes.value, picked.value.map((f) => f.name), mode.value),
)
/**
 * 这份载体能不能回写（后端口径，不在前端猜）：
 * 简单模式没有回写原语；高级模式要看该载体对应的工具在不在（图片 PhotoCraft / PDF PdfCraft / Word WordCraft）。
 */
const canWriteback = computed(() => writebackBlock.value === '')
/** 可画的 id（列表据此标「未定位」，与画布同一判定 → 不会出现「列表说定位了、图上没框」） */
const drawableIds = computed(() => new Set(overlay.value.boxes.map((b) => b.id)))
/** SVG 用载体像素做 viewBox：线宽 / 字号按比例换算，缩放时不会糊成一团 */
const strokeW = computed(() => (size.value ? Math.max(2, size.value.w / 700) : 2))
const labelSize = computed(() => (size.value ? Math.max(14, size.value.w / 48) : 14))

const writebackBlock = computed(() => {
  const job = activeJob.value
  if (!job) return ''
  if (!['ready', 'applied'].includes(job.stage)) return '还没跑完，暂时没有可回写的判定'
  if (job.mode === 'simple')
    return '简单模式不回写载体：只出清单与正误表。要写回载体，请用「高级」模式重跑这份载体'
  const cards = modeCardsFor(probes.value, job.filename)
  if (cards) return cards.advanced.available ? '' : cards.advanced.reason || '高级模式所需工具不可用'
  // 探测里没这张模式卡（旧后端 / 未知载体）→ 退回图片档的 PhotoCraft 口径，不假装可用
  return photocraftReady.value ? '' : 'PhotoCraft 不可用：图片载体无法回写'
})

/** 阶段点色：failed 是错误态 → danger；就绪 → ok；进行中 → warn */
function stageTone(s: string): 'ok' | 'warn' | 'danger' {
  if (s === 'failed') return 'danger'
  if (['ready', 'applied'].includes(s)) return 'ok'
  return 'warn'
}

/** 检测层贡献：统一走 labels.ts 人话词表（此前本处与 ResultsPanel 已漂移出
 *  「LLM 兜底 / LLM」两套叫法——一个概念一个名字，收敛到 plainLayerLabel） */
function layerText(l: { layer: string; status: string; found: number }): string {
  const base = plainLayerLabel(l.layer)
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
  const noun = CARRIER_NOUNS[job.kind] ?? '载体'
  return `${noun}${i + 1} · ${job.filename}`
}

async function loadProbes() {
  try {
    probes.value = await api.mediaProbes()
    probeError.value = ''
    // 模式默认值跟随后端设置里的 `media.mode`（用户手动选过就不再覆盖）
    const d = probes.value.mode_default
    if (!modeTouched.value && (d === 'simple' || d === 'advanced')) mode.value = d
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
    toast('先选择要审校的载体', 'warn')
    return
  }
  // 高档不可用就摊开原因，不静默换档：换了档，产物形态与正误表口径都会跟着变
  if (modeNote.value) {
    toast(modeNote.value, 'danger')
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
        mode: mode.value,
      })
      upsertJob(created)
      await openJob(created.id)
      const final = await pollUntilDone(created.id)
      if (final.stage === 'failed') {
        toast(`${file.name} 审校失败：${final.error ?? '未知原因'}`, 'danger')
      }
    }
    if (!cancelled) statusText.value = '全部载体处理完毕'
  } catch (e) {
    statusText.value = ''
    toast(errText(e), 'danger')
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
  const job = jobs.value.find((j) => j.id === id)
  // 预览面不是位图画布 → 不取图、不画框（§15.3：文档没有可信像素坐标，画了就是假框）
  if (job && !previewIsCanvas(job)) {
    previewNote.value =
      job.mode === 'simple'
        ? '简单模式：段落级定位 + 正误表，没有位图预览，也不画框（要回写载体请切「高级」）'
        : '这份载体的预览不是位图画布：按段落定位，不画框'
    return
  }
  try {
    const blob = await api.mediaPreviewBlob(id)
    // 后端没有 `result.png` 时回退副本原件：PDF / docx 不是位图，塞进 `<img>` 只会得到破图
    if (blob.type && !blob.type.startsWith('image/')) {
      previewNote.value = `还没有位图预览（后端给回的是 ${blob.type} 原件）：回写后生成 result.png；判定与正误表不受影响`
      return
    }
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
  if (a.region.region === 'unlocated') return '未能定位（把握不足 / 跨多个区域）'
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
      toast(`已回写：${next.artifacts.editable ?? 'result'}（原件未改动）`, 'ok')
    } else {
      toast(`回写未完成：${next.error ?? '见告警'}（结果仍在 sidecar）`, 'warn')
    }
  } catch (e) {
    toast(errText(e), 'danger')
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
    toast(`正误表：候选 ${report.value.candidates} 条 → 导出 ${report.value.exported} 条`, 'ok')
  } catch (e) {
    toast(errText(e), 'danger')
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
    if (saved) toast(`已导出 ${saved.filename}（${saved.size_bytes} 字节）`, 'ok')
  } catch (e) {
    toast(errText(e), 'danger')
  }
}

// ── 桌面规范（macOS HIG）：菜单导入/导出 + Finder 拖放 + 任务右键菜单 ──
async function createJobsFromPaths(paths: string[]) {
  if (busy.value) return
  // 与「开始审校」同一口径：高档不可用就摊开原因，不静默换档
  if (modeNote.value) {
    toast(modeNote.value, 'danger')
    return
  }
  busy.value = true
  report.value = null
  try {
    for (const p of paths) {
      const name = p.split(/[\\/]/).pop() ?? p
      statusText.value = `导入 ${name}…`
      const created = await api.mediaCreateJobFromPath({
        path: p,
        apply_mode: applyMode.value,
        lang: lang.value,
        level: level.value,
        force_llm: forceLlm.value,
        mode: mode.value,
      })
      upsertJob(created)
      await openJob(created.id)
      const final = await pollUntilDone(created.id)
      if (final.stage === 'failed') {
        toast(`${name} 审校失败：${final.error ?? '未知原因'}`, 'danger')
      }
    }
    statusText.value = '拖放导入完成'
  } catch (e) {
    statusText.value = ''
    toast(errText(e), 'danger')
  } finally {
    busy.value = false
  }
}

async function onJobContext(j: MediaJob) {
  const action = await api.showContextMenu('media-job')
  if (action === 'ctx:job:open') void openJob(j.id)
  else if (action === 'ctx:job:export') {
    if (activeId.value !== j.id) await openJob(j.id)
    void exportReport()
  }
}

const onMenuImport = () => pickFiles()
const onMenuExport = () => void exportReport()
const onMediaDrop = (ev: Event) => void createJobsFromPaths((ev as CustomEvent<string[]>).detail ?? [])

onMounted(() => {
  void loadProbes()
  void loadJobs()
  window.addEventListener('langfixer:media-import', onMenuImport)
  window.addEventListener('langfixer:media-export', onMenuExport)
  window.addEventListener('langfixer:media-drop', onMediaDrop)
})
onBeforeUnmount(() => {
  cancelled = true
  if (previewUrl.value) URL.revokeObjectURL(previewUrl.value)
  window.removeEventListener('langfixer:media-import', onMenuImport)
  window.removeEventListener('langfixer:media-export', onMenuExport)
  window.removeEventListener('langfixer:media-drop', onMediaDrop)
})
</script>

<template>
  <LSection class="media-card" title="载体审校（图片 / PDF / Word）" desc="副本隔离 · 默认叠加标注，不改原件">
    <template #actions>
      <LButton size="sm" variant="ghost" @click="loadProbes">重新探测</LButton>
    </template>

    <div v-if="probeError" class="l-banner banner-danger" role="alert">探测失败：{{ probeError }}</div>
    <div v-else-if="blockers.length" class="l-banner banner-warn" role="status">
      载体审校能力不全（结果会带告警，不会静默降级）
      <ul class="blocker-list">
        <li v-for="b in blockers" :key="b">{{ b }}</li>
      </ul>
    </div>

    <div class="probe-row">
      <LTag :tone="photocraftReady ? 'ok' : 'neutral'">
        PhotoCraft {{ photocraftReady ? probes.photocraft?.version ?? '就绪' : '不可用（只出清单）' }}
      </LTag>
      <LTag :tone="probes.pdfcraft?.ready ? 'ok' : 'neutral'">
        PdfCraft {{ probes.pdfcraft?.ready ? probes.pdfcraft?.version ?? '就绪' : '不可用（PDF 走简单模式）' }}
      </LTag>
      <LTag :tone="probes.wordcraft?.ready ? 'ok' : 'neutral'">
        WordCraft {{ probes.wordcraft?.ready ? probes.wordcraft?.version ?? '就绪' : '不可用（Word 走简单模式）' }}
      </LTag>
      <LTag :tone="probes.vision?.ready ? 'ok' : 'neutral'">
        视觉档 {{ probes.vision?.ready ? probes.vision?.model : '未就绪' }}
        <template v-if="compression">· {{ compression }}</template>
      </LTag>
    </div>

    <div class="import-row">
      <input
        ref="fileInput"
        type="file"
        accept="image/*,.pdf,.docx"
        multiple
        class="hidden-file"
        @change="onPick"
      />
      <LButton @click="pickFiles">选择载体</LButton>
      <span class="file-names">
        {{ picked.map((f) => f.name).join('、') || '未选择（图片 / PDF / docx，逐个串行处理）' }}
      </span>
      <span class="spacer"></span>
      <label class="param">模式
        <LSegmented :model-value="mode" :options="MODES" @update:model-value="modeChange" />
        <LPopover align="end">
          <template #trigger="{ toggle }">
            <LButton size="sm" variant="ghost" aria-label="模式说明" @click="toggle">?</LButton>
          </template>
          {{ modeHint }}
        </LPopover>
      </label>
      <label class="param">语言
        <LSelect v-model="lang" :options="LANG_OPTIONS" class="lang-select" />
      </label>
      <label class="param">检查模式
        <LSelect v-model="levelSel" :options="LEVEL_OPTIONS" class="level-select" />
      </label>
      <label class="param switch-param">
        <LSwitch v-model="forceLlm" aria-label="强制 LLM 兜底" />
        <span>强制 LLM 兜底</span>
      </label>
      <LButton variant="primary" :loading="busy" @click="runAll">开始审校</LButton>
    </div>

    <!-- 高档不可用就把原因摊开（含退路），不静默换档 -->
    <div v-if="modeNote" class="l-banner banner-warn mode-note" role="status">{{ modeNote }}</div>
    <p v-else-if="mode === 'simple'" class="mode-note">
      简单模式不依赖任何外部工具：只出段落级定位与正误表，载体回写不可用（要回写请切「高级」）。
    </p>

    <div v-if="jobs.length" class="job-tabs">
      <LButton
        v-for="j in jobs"
        :key="j.id"
        size="sm"
        :class="{ 'tab-active': j.id === activeId }"
        @click="openJob(j.id)"
        @contextmenu.prevent="onJobContext(j)"
      >
        {{ labelOf(j) }}
        <LTag :tone="stageTone(j.stage)">{{ stageProgress(j.stage).label }}</LTag>
      </LButton>
    </div>

    <template v-if="activeJob">
      <div v-if="progress.running || progress.failed" :class="{ 'progress-fail': progress.failed }">
        <LProgress :value="progress.percent / 100" />
      </div>
      <div class="job-meta">
        <LTag>{{ statusText || progress.label }}</LTag>
        <LTag>
          {{ activeJob.mode === 'simple' ? '简单模式 · 零 CLI' : '高级模式' }}
          <template v-if="activeJob.tool">· {{ activeJob.tool }}</template>
        </LTag>
        <LTag>
          预览面 {{ activeJob.preview_kind === 'image_canvas' ? '位图画布（可画框）' : '文档文本（不画框）' }}
        </LTag>
        <LTag>
          载体 {{ size ? `${size.w}×${size.h}` : '尺寸未知' }}
          {{ activeJob.carrier?.editable ? '· 含文字层' : '· 无文字层' }}
        </LTag>
        <LTag>
          抽取 {{ extractionText(activeJob.extraction, activeJob.extraction_model) }}
        </LTag>
        <LTag>文本单元 {{ activeJob.units.length }}</LTag>
        <LTag>
          标注 {{ activeJob.annotations.length }}（画框 {{ overlay.boxes.length }} · 未定位
          {{ overlay.undrawable.length }}）
        </LTag>
        <LTag v-for="l in activeJob.layers" :key="l.layer">{{ layerText(l) }}</LTag>
      </div>

      <div v-if="activeJob.error" class="l-banner banner-danger" role="alert">{{ activeJob.error }}</div>
      <div v-if="activeJob.warnings.length" class="warn-box">
        <div v-for="(w, i) in activeJob.warnings" :key="i" class="warn-item">⚠ {{ w }}</div>
      </div>

      <div class="workspace">
        <section class="canvas-wrap">
          <div class="canvas-tools">
            <LButton size="sm" variant="ghost" aria-label="缩小" @click="zoom = Math.max(0.2, zoom / 1.25)">−</LButton>
            <LButton size="sm" variant="ghost" @click="zoom = 1">{{ Math.round(zoom * 100) }}%</LButton>
            <LButton size="sm" variant="ghost" aria-label="放大" @click="zoom = Math.min(6, zoom * 1.25)">＋</LButton>
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
            <LButton size="sm" variant="ghost" @click="setAllChecked(true)">全选</LButton>
            <LButton size="sm" variant="ghost" @click="setAllChecked(false)">全不选</LButton>
          </div>
          <div class="ann-list">
            <div
              v-for="(a, i) in activeJob.annotations"
              :key="a.id"
              class="ann-row"
              :class="{ sel: a.id === selectedId, off: !isChecked(a) }"
              @click="select(a.id)"
            >
              <LCheckbox
                :model-value="isChecked(a)"
                @update:model-value="(v: boolean) => setChecked(a, v)"
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
                  <LTag>{{ CAT_LABELS[a.error.category ?? 'other'] ?? a.error.category ?? '其他' }}</LTag>
                  <LTag>{{ detectorBadge(a.error).text }}</LTag>
                  <LTag v-if="consensusOf(a.error).length > 1">共识 ×{{ consensusOf(a.error).length }}</LTag>
                  <LTag>{{ confBand(a.error).label }} {{ a.error.confidence.toFixed(2) }}</LTag>
                  <LTag v-if="unitConfidence(a) !== null">抽取 conf {{ unitConfidence(a)?.toFixed(2) }}</LTag>
                  <code v-if="a.error.rule_id" class="rule-id">{{ a.error.rule_id }}</code>
                  <LTag v-if="!drawableIds.has(a.id)" tone="warn">{{ locateReason(a) }}</LTag>
                  <LTag v-if="a.locate_warning" tone="warn">{{ a.locate_warning }}</LTag>
                </div>
                <div class="ann-edit" @click.stop>
                  <label class="param">计错数
                    <LSelect
                      :model-value="editOf(a).counted ?? '计'"
                      :options="[{ value: '计', label: '计' }, { value: '不计', label: '不计' }]"
                      class="counted-select"
                      @update:model-value="(v: string | number) => { editOf(a).counted = String(v) }"
                    />
                  </label>
                  <LInput
                    :model-value="editOf(a).suggestion ?? ''"
                    placeholder="改写建议（留空 = 用机器建议）"
                    class="suggest-input"
                    @update:model-value="(v: string) => { editOf(a).suggestion = v }"
                  />
                </div>
              </div>
            </div>
            <LEmpty
              v-if="!activeJob.annotations.length"
              text="本载体没有检出条目"
              :hint="activeJob.extraction === 'none' ? '文本抽取为空：见上方告警，可换模式或换档重跑这份载体' : undefined"
            />
          </div>
        </section>
      </div>

      <div class="action-row">
        <span class="param-label">回写模式</span>
        <LSegmented :model-value="applyMode" :options="APPLY_MODES" @update:model-value="setApplyMode" />
        <LPopover align="end">
          <template #trigger="{ toggle }">
            <LButton size="sm" variant="ghost" aria-label="回写模式说明" @click="toggle">?</LButton>
          </template>
          {{ applyHint }}
        </LPopover>
        <span class="spacer"></span>
        <LButton
          size="sm"
          :disabled="!canWriteback"
          :loading="busy"
          :title="writebackBlock || undefined"
          @click="applyWrite"
        >
          应用并回写
        </LButton>
        <LButton
          size="sm"
          variant="primary"
          :loading="reportBusy"
          :disabled="!jobs.some((j) => ['ready', 'applied'].includes(j.stage))"
          @click="exportReport"
        >
          导出正误表
        </LButton>
      </div>
      <p v-if="writebackBlock" class="hint">{{ writebackBlock }}</p>
      <p v-else-if="applyMode === 'in_place' && !canInPlace" class="hint">
        本载体无文字层（扁平位图）→「改原文」不可用，只能用叠加标注
      </p>

      <div v-if="report" class="report-box">
        <div class="report-head">
          正误表 {{ report.report_id }}：候选 {{ report.candidates }} 条 → 导出
          {{ report.exported }} 条（剔除 {{ report.dropped_ids.length }} 条）
        </div>
        <div class="report-links">
          <LButton size="sm" variant="ghost" @click="saveReport('docx')">Word（docx）</LButton>
          <LButton size="sm" variant="ghost" @click="saveReport('html')">HTML</LButton>
          <LButton size="sm" variant="ghost" @click="saveReport('md')">Markdown</LButton>
          <LButton size="sm" variant="ghost" @click="saveReport('json')">JSON</LButton>
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

    <LEmpty
      v-else
      text="选择载体（图片 / PDF / docx）后点「开始审校」"
      hint="探测载体 → 取文本（无文字层走视觉档）→ 检测 → 回映射落点。「简单」模式零外部工具，只出清单与正误表；「高级」模式交给 PhotoCraft / PdfCraft / WordCraft，可回写载体；原件不会被修改，所有产物在 job 独立目录里。"
    />
  </LSection>
</template>

<style scoped>
.media-card {
  height: 100%;
}
.hidden-file {
  display: none;
}
.spacer {
  flex: 1;
}
.banner-warn {
  flex-wrap: wrap;
  color: var(--warn-ink);
  background: var(--warn-soft);
  border-color: var(--warn-line);
}
.banner-danger {
  flex-wrap: wrap;
  color: var(--danger-ink);
  background: var(--danger-soft);
  border-color: var(--danger-line);
}
.blocker-list {
  width: 100%;
  margin: var(--space-1) 0 0 var(--space-5);
  padding: 0;
  font-size: var(--fs-sm);
}
.probe-row,
.job-meta,
.ann-badges {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-2);
  align-items: center;
}
.probe-row {
  margin-bottom: var(--space-3);
}
.import-row {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-2);
  align-items: center;
  margin-bottom: var(--space-3);
}
/* 模式说明 / 不可用原因：紧贴操作条，用户点「开始审校」之前就看得见 */
.mode-note {
  margin: 0 0 var(--space-3);
  font-size: var(--fs-sm);
  line-height: var(--lh-ui);
  color: var(--muted);
}
.file-names {
  font-size: var(--fs-sm);
  color: var(--muted);
  max-width: 40%;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.param {
  display: inline-flex;
  align-items: center;
  gap: var(--space-2);
  font-size: var(--fs-sm);
}
.switch-param {
  gap: var(--space-2);
}
/* 选择器宽度交给内容自适应（token 尺度里没有这些魔法数） */
.job-tabs {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-2);
  margin-bottom: var(--space-3);
}
.tab-active {
  border-color: var(--accent);
  color: var(--accent);
}
.job-meta {
  margin: var(--space-2) 0;
}
.progress-fail :deep(.l-progress > i) {
  background: var(--danger);
}
.warn-box {
  border: 1px solid var(--warn-line);
  border-radius: var(--radius-ctl);
  padding: var(--space-2) var(--space-3);
  margin-bottom: var(--space-3);
  background: var(--warn-soft);
}
.warn-item {
  font-size: var(--fs-sm);
  line-height: var(--lh-ui);
}
.workspace {
  display: grid;
  grid-template-columns: minmax(0, 1.15fr) minmax(320px, 1fr);
  gap: var(--space-3);
}
.canvas-wrap {
  min-width: 0;
}
.canvas-tools {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  margin-bottom: var(--space-2);
}
.hint {
  font-size: var(--fs-sm);
  color: var(--muted);
}
.canvas-scroll {
  max-height: 62vh;
  overflow: auto;
  border: 1px solid var(--border-strong);
  border-radius: var(--radius-ctl);
  background: var(--surface-2);
}
.canvas-inner {
  position: relative;
  width: 100%;
  transform-origin: 0 0;
}
.carrier-img {
  display: block;
  width: 100%;
  height: auto;
}
/* SVG 铺满 img 的显示框，viewBox = 载体像素 → 框坐标与 sidecar 逐字一致，缩放交给它 */
.overlay {
  position: absolute;
  inset: 0;
  width: 100%;
  height: 100%;
}
.overlay g {
  cursor: pointer;
}
.box-label {
  font-weight: 700;
  paint-order: stroke;
  stroke: #fff;
  stroke-width: 3px;
}
.preview-note {
  padding: var(--space-3);
  font-size: var(--fs-sm);
  color: var(--muted);
}
.list-wrap {
  min-width: 0;
}
.list-tools {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  margin-bottom: var(--space-2);
}
.ann-list {
  max-height: 62vh;
  overflow: auto;
  display: flex;
  flex-direction: column;
  gap: var(--space-2);
}
.ann-row {
  display: flex;
  gap: var(--space-2);
  padding: var(--space-2);
  border: 1px solid var(--border);
  border-radius: var(--radius-ctl);
  cursor: pointer;
}
.ann-row.sel {
  border-color: var(--accent);
  background: var(--primary-soft);
}
.ann-row.off {
  opacity: 0.5;
}
.ann-no {
  font-size: var(--fs-sm);
  color: var(--muted);
  min-width: 18px;
}
.ann-body {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: var(--space-1);
}
.ann-text {
  font-size: var(--fs-ui);
}
.ann-text code {
  background: var(--surface-2);
  padding: 1px var(--space-1);
  border-radius: var(--radius-ctl);
}
.ann-text code.fix {
  background: var(--ok-soft);
}
.arrow {
  margin: 0 var(--space-1);
  color: var(--muted);
}
.ann-msg {
  font-size: var(--fs-sm);
  color: var(--muted);
}
.rule-id {
  font-size: var(--fs-xs);
  color: var(--muted);
}
.ann-edit {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-2);
  align-items: center;
}
.suggest-input {
  flex: 1;
  min-width: 16ch;
  max-width: 32ch;
}
.action-row {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-2);
  align-items: center;
  margin-top: var(--space-3);
}
.param-label {
  font-size: var(--fs-sm);
  color: var(--muted);
}
.report-box {
  margin-top: var(--space-3);
  border: 1px solid var(--border);
  border-radius: var(--radius-ctl);
  padding: var(--space-2) var(--space-3);
}
.report-head {
  font-size: var(--fs-ui);
  margin-bottom: var(--space-2);
}
.report-links {
  display: flex;
  gap: var(--space-3);
  margin-bottom: var(--space-2);
}
.artifacts {
  margin-top: var(--space-3);
  font-size: var(--fs-sm);
  color: var(--muted);
}
.artifacts ul {
  margin: var(--space-1) 0 0 var(--space-5);
  padding: 0;
}
</style>
