import { describe, expect, it } from 'vitest'
import {
  advancedBlockNote,
  buildReportBody,
  carrierSize,
  compressionText,
  detectorBadge,
  kindOfFilename,
  modeCardsFor,
  overlayModel,
  parseRowKey,
  previewIsCanvas,
  probeBlockers,
  regionBox,
  rowKey,
  stageProgress,
} from './media'
import type {
  ErrorItem,
  MediaAnnotation,
  MediaCarrierInfo,
  MediaProbes,
  MediaRegion,
} from '../types'

/** 与后端 `annotations.json` 同形状（`Region` 是内部标签 `region`，snake_case） */
function ann(id: number, region: MediaRegion, over: Partial<MediaAnnotation> = {}): MediaAnnotation {
  const error: ErrorItem = {
    lang: 'zh',
    error_type: 'grammar',
    confidence: 0.5,
    span: [785, 791],
    original: '避免',
    correction: null,
    message: '句子以动词开头，疑似成分残缺（缺主语）',
    category: 'grammar',
    rule_id: 'ZH_MISSING_SUBJECT',
    source: 'rule',
  }
  return {
    id,
    error,
    unit_id: 14,
    local_span: [27, 33],
    region,
    apply: 'overlay',
    style: { kind: 'box', color: '#e03131' },
    ...over,
  }
}

const CARRIER: MediaCarrierInfo = {
  kind: 'image',
  width: 3072,
  height: 4096,
  pages: null,
  text_units: 0,
  editable: false,
  warnings: ['无文字层：只能走视觉/OCR 档，回写只能 Overlay'],
}

describe('regionBox', () => {
  it('image_box 用载体像素坐标（run13 真实标注）', () => {
    const box = regionBox(
      { region: 'image_box', x: 516.096, y: 1589.248, w: 1996.8, h: 57.344 },
      carrierSize(CARRIER),
    )
    expect(box).toEqual({ x: 516.096, y: 1589.248, w: 1996.8, h: 57.344 })
  })

  it('载体尺寸未知 → 不画（坐标空间立不起来，不能按 0,0 猜）', () => {
    expect(regionBox({ region: 'image_box', x: 10, y: 10, w: 20, h: 20 }, null)).toBeNull()
  })

  it('超出载体边界的框裁边保留，而不是整框丢掉', () => {
    const box = regionBox({ region: 'image_box', x: 3000, y: 4050, w: 200, h: 200 }, carrierSize(CARRIER))
    expect(box).toEqual({ x: 3000, y: 4050, w: 72, h: 46 })
  })

  it('零面积 / NaN 不画', () => {
    const size = carrierSize(CARRIER)
    expect(regionBox({ region: 'image_box', x: 10, y: 10, w: 0, h: 30 }, size)).toBeNull()
    expect(regionBox({ region: 'image_box', x: 10, y: 10, w: Number.NaN, h: 30 }, size)).toBeNull()
  })

  it('unlocated / run_ref 绝不画假框（§7 硬约束）', () => {
    const size = carrierSize(CARRIER)
    expect(regionBox({ region: 'unlocated' }, size)).toBeNull()
    expect(regionBox({ region: 'run_ref', part: 'body', run_range: [0, 4] }, size)).toBeNull()
  })

  it('layer_ref 只有带 bounds 才能画；page_box 按页内归一化换算', () => {
    const size = carrierSize(CARRIER)
    expect(regionBox({ region: 'layer_ref', layer_id: 7, kind: 'Type' }, size)).toBeNull()
    expect(
      regionBox({ region: 'layer_ref', layer_id: 7, kind: 'Type', bounds: [0, 0, 100, 40] }, size),
    ).toEqual({ x: 0, y: 0, w: 100, h: 40 })
    expect(
      regionBox({ region: 'page_box', page: 1, x: 0.25, y: 0.5, w: 0.5, h: 0.25 }, size),
    ).toEqual({ x: 768, y: 2048, w: 1536, h: 1024 })
  })
})

describe('overlayModel', () => {
  it('画不出来的落到 undrawable，编号按载体顺序连续（框与列表同一套号）', () => {
    const { boxes, undrawable } = overlayModel(
      [
        ann(1, { region: 'image_box', x: 0, y: 0, w: 100, h: 40 }),
        ann(2, { region: 'unlocated' }),
        ann(3, { region: 'image_box', x: 10, y: 10, w: 50, h: 20 }),
      ],
      carrierSize(CARRIER),
    )
    expect(boxes.map((b) => [b.id, b.no])).toEqual([
      [1, 1],
      [3, 3],
    ])
    expect(undrawable.map((a) => a.id)).toEqual([2])
    expect(boxes[0].color).toBe('#e03131')
  })

  it('尺寸缺失时整批都不画（而不是画到 0,0 上）', () => {
    const { boxes, undrawable } = overlayModel(
      [ann(1, { region: 'image_box', x: 0, y: 0, w: 100, h: 40 })],
      null,
    )
    expect(boxes).toHaveLength(0)
    expect(undrawable).toHaveLength(1)
  })
})

describe('stageProgress', () => {
  it('阶段文案与百分比（轮询用）', () => {
    expect(stageProgress('queued')).toMatchObject({ percent: 2, running: true })
    expect(stageProgress('extracting')).toMatchObject({ percent: 50, label: '抽取文本' })
    expect(stageProgress('ready')).toMatchObject({ percent: 100, label: '就绪', running: false })
    expect(stageProgress('applying')).toMatchObject({ percent: 96, label: '回写载体' })
    expect(stageProgress('failed')).toMatchObject({ label: '失败', failed: true, running: false })
  })
})

describe('rowKey / buildReportBody', () => {
  it('rowKey 往返；非法键拒绝', () => {
    expect(parseRowKey(rowKey('6beffb8e', 12))).toEqual({ job_id: '6beffb8e', annotation_id: 12 })
    expect(parseRowKey('no-id')).toBeNull()
    expect(parseRowKey('j#x')).toBeNull()
  })

  it('取消勾选走 job 作用域 override（annotation_id 跨 job 会重号）', () => {
    const body = buildReportBody({
      jobIds: ['jA', 'jB'],
      checked: { [rowKey('jA', 3)]: false, [rowKey('jB', 3)]: false, [rowKey('jA', 4)]: true },
    })
    expect(body.rejected_ids).toBeUndefined()
    expect(body.overrides).toEqual([
      { annotation_id: 3, job_id: 'jA', checked: false },
      { annotation_id: 3, job_id: 'jB', checked: false },
    ])
    expect(body.job_ids).toEqual(['jA', 'jB'])
    expect(body.docx).toBe(true)
  })

  it('人工改写与勾选合并进同一条 override；空改写不产生条目', () => {
    const body = buildReportBody({
      jobIds: ['jA'],
      checked: { [rowKey('jA', 9)]: false },
      edits: {
        [rowKey('jA', 9)]: { counted: '不计', note: '视觉误读' },
        [rowKey('jA', 2)]: { original: '', suggestion: '' },
      },
    })
    expect(body.overrides).toEqual([
      { annotation_id: 9, job_id: 'jA', checked: false, counted: '不计', note: '视觉误读' },
    ])
  })
})

describe('badges / blockers', () => {
  it('多层共识把徽章转成醒目色（rule 单独命中 = 中性）', () => {
    const solo = { ...ann(1, { region: 'unlocated' }).error }
    expect(detectorBadge(solo)).toEqual({ text: '规则', type: 'info' })
    expect(detectorBadge({ ...solo, also_found_by: ['llm'] }).type).toBe('success')
    expect(detectorBadge({ ...solo, source: 'llm' })).toEqual({ text: 'LLM 兜底', type: 'warning' })
  })

  it('探测未就绪要变成人话阻塞项（不能让用户对着空画布猜）', () => {
    expect(probeBlockers({})).toContain(
      '视觉档状态未知：多模态未启用（设置里开启，或 GEC_MEDIA_ENABLED=1）',
    )
    expect(
      probeBlockers({
        vision: { ready: true },
        photocraft: { ready: false, degraded_reason: 'photocraft-cli 未找到' },
      }),
    ).toEqual(['photocraft-cli 未找到'])
  })

  it('总开关关闭必须点名（设置文件存在时环境变量会被整体覆盖——这是最常见的坑）', () => {
    const [first] = probeBlockers({ enabled: false, vision: { ready: false } })
    expect(first).toContain('多模态审校未启用')
    expect(first).toContain('设置文件')
  })

  it('压缩口径按探测值显示；缺字段不编数字', () => {
    expect(compressionText({ max_edge: 2000, max_bytes: 1_500_000, jpeg_quality: 80 })).toBe(
      '上传前压缩 ≤2000px / ≤1.5MB JPEG q80',
    )
    expect(compressionText(undefined)).toBe('')
    expect(compressionText({ max_edge: 2000 })).toBe('')
  })
})

describe('模式卡（§15.1）', () => {
  const PROBES: MediaProbes = {
    mode_default: 'advanced',
    modes: {
      pdf: {
        simple: { available: true },
        advanced: {
          available: false,
          reason: 'pdfcraft-cli 未找到（装到 ~/.local/bin/pdfcraft-cli，或在设置里给绝对路径）',
        },
      },
      docx: {
        simple: { available: true },
        advanced: { available: true },
      },
    },
  }

  it('文件名 → 载体大类与后端同一口径', () => {
    expect(kindOfFilename('Note.PDF')).toBe('pdf')
    expect(kindOfFilename('proof.docx')).toBe('docx')
    expect(kindOfFilename('shot.PNG')).toBe('image')
    expect(kindOfFilename('notes.txt')).toBe('other')
  })

  it('模式卡只认后端 probes.modes，缺字段不猜可用', () => {
    expect(modeCardsFor(PROBES, 'a.pdf')?.advanced.available).toBe(false)
    expect(modeCardsFor(PROBES, 'a.docx')?.advanced.available).toBe(true)
    expect(modeCardsFor(PROBES, 'a.txt')).toBeNull()
    expect(modeCardsFor({}, 'a.pdf')).toBeNull()
  })

  it('选了高档但工具缺失 → 一句话带退路；简单模式不受影响', () => {
    const note = advancedBlockNote(PROBES, ['a.pdf', 'b.docx'], 'advanced')
    expect(note).toContain('pdfcraft-cli 未找到')
    expect(note).not.toContain('docx')
    expect(advancedBlockNote(PROBES, ['a.pdf'], 'simple')).toBe('')
    // 同一原因只说一次，不刷屏
    expect(advancedBlockNote(PROBES, ['a.pdf', 'b.pdf'], 'advanced').split('；')).toHaveLength(1)
  })

  it('预览面决定能不能画框：doc_text 绝不画框（§15.3）', () => {
    expect(previewIsCanvas({ preview_kind: 'image_canvas' })).toBe(true)
    expect(previewIsCanvas({ preview_kind: 'doc_text' })).toBe(false)
    expect(previewIsCanvas(undefined)).toBe(false)
  })
})
