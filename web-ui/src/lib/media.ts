/**
 * 载体（图片）审校的纯逻辑：落点换算 / 定位分组 / 阶段进度 / 勾选载荷
 *
 * 从 MediaWorkspace.vue 拆出，全部无副作用可单测（`media.test.ts` 用后端
 * `annotations.json` 的真实形状断言，防止 serde 改名把画框悄悄变成空白）。
 *
 * 硬约束（`MULTIMODAL.md` §7）：**定位失败绝不画假框**。坐标空间立不起来
 * （载体尺寸未知）或面积为零 / NaN → 返回 null，由 UI 归入「未能定位」。
 */
import type {
  ErrorItem,
  MediaAnnotation,
  MediaCarrierInfo,
  MediaMode,
  MediaModeCards,
  MediaPreviewKind,
  MediaProbes,
  MediaRegion,
  MediaReportBody,
  MediaRowOverride,
  MediaStage,
} from '../types'
import { SOURCE_LABELS } from './errorDisplay'

/** 载体像素尺寸（画框的坐标空间） */
export interface CarrierSize {
  w: number
  h: number
}

/** 画布框：与载体同一像素坐标系（SVG 用 viewBox，缩放交给它） */
export interface CanvasBox {
  x: number
  y: number
  w: number
  h: number
}

/** 可画的框（带标注号与样式色） */
export interface OverlayBox {
  /** `Annotation.id`（**job 内**唯一，跨 job 会重号 → 与 job_id 组合才唯一，见 rowKey） */
  id: number
  /** 载体顺序号（1..N，画布上的编号与列表同一套） */
  no: number
  box: CanvasBox
  color: string
}

export interface OverlayModel {
  boxes: OverlayBox[]
  /** 画不出框的标注（`unlocated` / 无 bounds / 尺寸未知 / 零面积）→ 只进列表 */
  undrawable: MediaAnnotation[]
}

export function carrierSize(carrier?: MediaCarrierInfo): CarrierSize | null {
  const w = carrier?.width ?? 0
  const h = carrier?.height ?? 0
  if (!(w > 0) || !(h > 0)) return null
  return { w, h }
}

/**
 * 落点 → 画布框。`image_box` / `layer_ref.bounds` 是**载体像素**，
 * `page_box` 是页内归一化 0..1（后端刻意不带 DPI 假设）。
 */
export function regionBox(region: MediaRegion, size: CarrierSize | null): CanvasBox | null {
  switch (region.region) {
    case 'image_box':
      return clampBox({ x: region.x, y: region.y, w: region.w, h: region.h }, size)
    case 'layer_ref': {
      if (!region.bounds) return null
      const [x, y, w, h] = region.bounds
      return clampBox({ x, y, w, h }, size)
    }
    case 'page_box': {
      if (!size) return null
      return clampBox(
        { x: region.x * size.w, y: region.y * size.h, w: region.w * size.w, h: region.h * size.h },
        size,
      )
    }
    default:
      // unlocated / run_ref：没有位图落点，不画（§7「绝不画假框」）
      return null
  }
}

/** 裁进载体边界 + 拒绝零面积 / NaN（四舍五入超界很常见，不能因此丢掉整框） */
function clampBox(b: CanvasBox, size: CarrierSize | null): CanvasBox | null {
  if (!size) return null
  const { w: cw, h: ch } = size
  const x = Math.min(Math.max(b.x, 0), cw)
  const y = Math.min(Math.max(b.y, 0), ch)
  const w = Math.min(b.w, cw - x)
  const h = Math.min(b.h, ch - y)
  if (!Number.isFinite(w) || !Number.isFinite(h) || w <= 0 || h <= 0) return null
  return { x, y, w, h }
}

/** 标注 → 画布框 + 画不出来的那批（保持后端给的顺序 = 载体顺序） */
export function overlayModel(
  annotations: MediaAnnotation[],
  size: CarrierSize | null,
): OverlayModel {
  const boxes: OverlayBox[] = []
  const undrawable: MediaAnnotation[] = []
  annotations.forEach((a, i) => {
    const box = regionBox(a.region, size)
    if (box) boxes.push({ id: a.id, no: i + 1, box, color: a.style?.color || '#e03131' })
    else undrawable.push(a)
  })
  return { boxes, undrawable }
}

/** 阶段推进（与 `media_jobs` 状态机同序；`applying/applied` 是回写二段） */
export const STAGE_STEPS: { stage: MediaStage; label: string }[] = [
  { stage: 'copying', label: '复制副本' },
  { stage: 'inspecting', label: '探测载体' },
  { stage: 'extracting', label: '抽取文本' },
  { stage: 'detecting', label: '检测差错' },
  { stage: 'remapping', label: '回映射落点' },
  { stage: 'ready', label: '就绪' },
]

export function stageProgress(stage: MediaStage): {
  percent: number
  label: string
  running: boolean
  failed: boolean
} {
  if (stage === 'failed') return { percent: 100, label: '失败', running: false, failed: true }
  if (stage === 'queued') return { percent: 2, label: '排队中', running: true, failed: false }
  if (stage === 'applying') return { percent: 96, label: '回写载体', running: true, failed: false }
  if (stage === 'applied') return { percent: 100, label: '已回写', running: false, failed: false }
  const i = STAGE_STEPS.findIndex((s) => s.stage === stage)
  if (i < 0) return { percent: 0, label: stage, running: true, failed: false }
  return {
    percent: Math.round(((i + 1) / STAGE_STEPS.length) * 100),
    label: STAGE_STEPS[i].label,
    running: stage !== 'ready',
    failed: false,
  }
}

/** 抽取档显示词（与 job.extraction / unit.source 对齐） */
export const EXTRACTION_LABELS: Record<string, string> = {
  structure: '文字层',
  vision: '视觉档',
  ocr: 'OCR',
  none: '未取到文本',
}

export function extractionText(extraction: string, model?: string | null): string {
  const base = EXTRACTION_LABELS[extraction] ?? extraction
  return model ? `${base} · ${model}` : base
}

/** 来源层徽章：视觉档误读多，来源必须一眼可辨；多层共识转成醒目色 */
export function detectorBadge(e: ErrorItem): {
  text: string
  type: 'success' | 'warning' | 'info' | 'primary'
} {
  const src = e.source ?? e.error_type
  const label = SOURCE_LABELS[src] ?? src
  const consensus = (e.also_found_by?.length ?? 0) > 0
  if (src === 'llm' || src === 'feature') return { text: label, type: consensus ? 'success' : 'warning' }
  if (src === 'rule') return { text: label, type: consensus ? 'success' : 'info' }
  return { text: label, type: 'info' }
}

/** 共识层列表：`source` + `also_found_by`（≥2 层命中 = 更强信号） */
export function consensusOf(e: ErrorItem): string[] {
  return [e.source, ...(e.also_found_by ?? [])].filter(Boolean) as string[]
}

/** 勾选 / 改写的行键：`annotation_id` 只在**单个 job 内**唯一，跨载体必重号 */
export function rowKey(jobId: string, annotationId: number): string {
  return `${jobId}#${annotationId}`
}

export function parseRowKey(key: string): { job_id: string; annotation_id: number } | null {
  const at = key.lastIndexOf('#')
  if (at <= 0) return null
  const id = Number(key.slice(at + 1))
  if (!Number.isInteger(id) || id < 0) return null
  return { job_id: key.slice(0, at), annotation_id: id }
}

/** 人工改写列（导出前覆盖机器判定；改原文会让后端清掉 gold_match，见 report.rs） */
export interface RowEdit {
  original?: string
  suggestion?: string
  counted?: string
  note?: string
}

function editIsEmpty(e: RowEdit): boolean {
  return (
    (e.original === undefined || e.original === '') &&
    (e.suggestion === undefined || e.suggestion === '') &&
    (e.counted === undefined || e.counted === '') &&
    (e.note === undefined || e.note === '')
  )
}

/**
 * 构造 `POST /gec/media/report` 载荷。
 *
 * **不用 `rejected_ids`**：它是裸 `annotation_id` 列表，而每个 job 的标注都从 1 编号，
 * 跨载体必重号——取消勾一张图上的一条，可能顺手删掉另一张图的同 id 行。
 * 这里走 job 作用域的 `overrides[].checked=false`：`apply_selection` 先应用改写
 * 再 `retain(checked)`，语义与 UI 勾选一致，且按 `job_id` 精确匹配。
 */
export function buildReportBody(opts: {
  jobIds: string[]
  /** rowKey → 是否勾选（缺省 = 勾选） */
  checked: Record<string, boolean>
  /** rowKey → 人工改写 */
  edits?: Record<string, RowEdit>
  title?: string
  docx?: boolean
}): MediaReportBody {
  const overrides: MediaRowOverride[] = []
  const put = (key: string, patch: Partial<MediaRowOverride>) => {
    const k = parseRowKey(key)
    if (!k) return
    const hit = overrides.find((o) => o.annotation_id === k.annotation_id && o.job_id === k.job_id)
    if (hit) Object.assign(hit, patch)
    else overrides.push({ ...patch, annotation_id: k.annotation_id, job_id: k.job_id })
  }
  for (const [key, on] of Object.entries(opts.checked)) {
    if (on === false) put(key, { checked: false })
  }
  for (const [key, edit] of Object.entries(opts.edits ?? {})) {
    if (editIsEmpty(edit)) continue
    put(key, {
      ...(edit.original ? { original: edit.original } : {}),
      ...(edit.suggestion ? { suggestion: edit.suggestion } : {}),
      ...(edit.counted ? { counted: edit.counted } : {}),
      ...(edit.note ? { note: edit.note } : {}),
    })
  }
  return {
    job_ids: opts.jobIds,
    overrides,
    docx: opts.docx ?? true,
    ...(opts.title ? { title: opts.title } : {}),
  }
}

/** 探测结果 → 人话阻塞项（缺什么去哪儿补，不让用户对着空画布猜） */
export function probeBlockers(probes: Record<string, unknown>): string[] {
  const out: string[] = []
  if (probes['enabled'] === false) {
    out.push(
      '多模态审校未启用：在设置文件里加 "media": { "enabled": true, "use_vision": true } 后重启服务（设置文件存在时，环境变量 GEC_MEDIA_ENABLED 会被整体覆盖）',
    )
  }
  const vision = probes['vision'] as { ready?: boolean; degraded_reason?: string } | undefined
  if (!vision) out.push('视觉档状态未知：多模态未启用（设置里开启，或 GEC_MEDIA_ENABLED=1）')
  else if (!vision.ready)
    out.push(vision.degraded_reason || '视觉档未就绪：检查 LLM 配置与 media.use_vision')
  const pc = probes['photocraft'] as { ready?: boolean; degraded_reason?: string } | undefined
  if (pc && !pc.ready)
    out.push(pc.degraded_reason || 'PhotoCraft 不可用：只能出清单与 sidecar，不回写载体')
  return out
}

/** 视觉上传压缩口径（探测返回值 → 一行文案；缺字段就不显示，不编数字） */
export function compressionText(vision?: {
  max_edge?: number
  max_bytes?: number
  jpeg_quality?: number
}): string {
  if (!vision?.max_edge || !vision.max_bytes) return ''
  const mb = vision.max_bytes / 1_000_000
  const q = vision.jpeg_quality ? ` JPEG q${vision.jpeg_quality}` : ''
  return `上传前压缩 ≤${vision.max_edge}px / ≤${(Math.round(mb * 10) / 10).toFixed(1)}MB${q}`
}

/** 文件名 → 载体大类（与后端 `carrier_kind_of` 同一口径，模式卡按它取） */
export function kindOfFilename(name: string): 'image' | 'pdf' | 'docx' | 'other' {
  const ext = name.split('.').pop()?.toLowerCase() ?? ''
  if (ext === 'pdf') return 'pdf'
  if (ext === 'docx') return 'docx'
  if (['png', 'jpg', 'jpeg', 'webp', 'bmp', 'gif', 'tif', 'tiff', 'heic', 'psd'].includes(ext))
    return 'image'
  return 'other'
}

/**
 * 某载体的模式卡（§15.1）：只认后端 `probes.modes` 的口径——
 * 前端自己判「CLI 在不在」就会与后端 gate 漂开（按钮能点、后端 503）。
 * 缺字段 = 未知，按「不可用」处理并说明未知，不猜。
 */
export function modeCardsFor(probes: MediaProbes, filename: string): MediaModeCards | null {
  const kind = kindOfFilename(filename)
  if (kind === 'other') return null
  return probes.modes?.[kind] ?? null
}

/**
 * 选了高档但某个文件的档不可用 → 一句人话（带退路）；可用 → 空串。
 * 多个文件同一原因只说一次，避免同一句话刷屏。
 */
export function advancedBlockNote(
  probes: MediaProbes,
  files: string[],
  mode: MediaMode,
): string {
  if (mode !== 'advanced') return ''
  const reasons: string[] = []
  for (const f of files) {
    const cards = modeCardsFor(probes, f)
    if (!cards) continue
    if (cards.advanced?.available) continue
    const reason = cards.advanced?.reason || `${kindOfFilename(f)} 的高级模式不可用`
    if (!reasons.includes(reason)) reasons.push(reason)
  }
  return reasons.join('；')
}

/** 预览面是否位图画布：`doc_text` 没有可信像素坐标，绝不画框（§15.3） */
export function previewIsCanvas(job?: { preview_kind?: MediaPreviewKind } | null): boolean {
  return job?.preview_kind === 'image_canvas'
}

