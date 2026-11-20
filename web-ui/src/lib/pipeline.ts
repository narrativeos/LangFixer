import type { DetectTimings, StageEvent } from '../types'

/**
 * 检测管线层展示（① 本地 ZEN 规则 → ②.5 知识召回 → ③-F 特征模型 → ③ LLM 兜底）
 *
 * ② 决策模型（vegaml）待发布层（EVOLUTION.md §13 #10；11-13 注：线不放弃，待更合适的决策模型）：默认不显示；
 * 仅当实验开关显式开启（GEC_DEC_ENABLED=1 / 设置文件）时才出现在管线条中。
 *
 * - ①②②.5 状态来自服务端设置（决策模型是否启用 / 嵌入引擎是否配置）；
 * - ③-F 特征模型（三期）：level 2/3 且启用时显示（P0.5：L3 是唯一的 pre-LLM 门控）；
 *   高置信定案，LLM 收缩到纯长尾；
 * - ③ 按「响应级别」动态显示：L1 不启用 / L2 L3 门控弃权（或 L3 未启用时决策模型
 *   弃权）时触发 / L3 特征模型弃权（或特征模型未启用）时触发（无条件）。
 *
 * 纯函数：Editor.vue 的「检测管线」提示行据此渲染 el-tag，便于单测。
 */
export interface PipelineLayer {
  /** 层编号（① / ② / ②.5 / ③-F / ③），用作列表 key */
  no: string
  /** 展示文案（含状态后缀） */
  label: string
  /** el-tag 类型：'' = 主色（条件触发）/ success = 生效 / info = 未启用 / warning = 未配置 */
  type: '' | 'success' | 'info' | 'warning'
}

export interface PipelineState {
  /** 响应级别：1 = 规则（+ 决策模型，若显式启用）；2 = + L3 门控（L3 弃权 → LLM 兜底）；3 = + 强制 LLM */
  level: number
  /** 决策模型（vegaml）层是否启用（P0.5 起默认关闭；opt-in） */
  decisionEnabled: boolean
  /** LLM 是否已配置（api_key 非空） */
  llmEnabled: boolean
  /** 嵌入引擎是否已配置（未配置时 ②.5 知识向量召回静默跳过） */
  embeddingConfigured: boolean
  /** L3 特征模型层是否启用（P0.5：level 2/3 的 pre-LLM 门控；高置信定案，LLM 收缩到纯长尾） */
  l3Enabled: boolean
}

export function computePipelineLayers(s: PipelineState): PipelineLayer[] {
  const layers: PipelineLayer[] = [
    { no: '①', label: '① 本地 ZEN 规则', type: 'success' },
  ]
  // ② 决策模型待发布（11-13 注：线不放弃，待更合适的决策模型）：仅实验开关显式开启时显示（默认路径 = 规则 → L3 → LLM）
  if (s.decisionEnabled) {
    layers.push({ no: '②', label: '② 决策模型 · 已启用', type: 'success' })
  }
  layers.push({
    no: '②.5',
    label: `②.5 知识召回${s.embeddingConfigured ? ' · 已配置' : ' · 未配置'}`,
    type: s.embeddingConfigured ? 'success' : 'info',
  })
  if (s.level === 3) {
    if (s.l3Enabled) {
      // 三期：特征模型高置信定案，LLM 收缩到纯长尾
      layers.push({ no: '③-F', label: '③-F 特征模型 · 已启用', type: 'success' })
      layers.push(
        s.llmEnabled
          ? { no: '③', label: '③ LLM 兜底 · 仅长尾（特征模型弃权时）', type: 'success' }
          : { no: '③', label: '③ LLM 兜底 · 未配置', type: 'warning' },
      )
    } else {
      layers.push(
        s.llmEnabled
          ? { no: '③', label: '③ LLM 兜底 · 无条件', type: 'success' }
          : { no: '③', label: '③ LLM 兜底 · 未配置', type: 'warning' },
      )
    }
  } else if (s.level === 2) {
    // P0.5 重绑：L3 是唯一的 pre-LLM 门控——L3 启用时高置信定案、弃权 → LLM 兜底；
    // L3 未启用时回退旧语义：决策模型弃权 → LLM 兜底
    if (s.l3Enabled) {
      layers.push({ no: '③-F', label: '③-F 特征模型 · 已启用', type: 'success' })
      layers.push(
        s.llmEnabled
          ? { no: '③', label: '③ LLM 兜底 · 仅长尾（L3 弃权时）', type: 'success' }
          : { no: '③', label: '③ LLM 兜底 · 未配置', type: 'warning' },
      )
    } else if (s.decisionEnabled) {
      layers.push(
        s.llmEnabled
          ? { no: '③', label: '③ LLM 兜底 · 弃权时触发', type: '' }
          : { no: '③', label: '③ LLM 兜底 · 未配置', type: 'warning' },
      )
    } else {
      layers.push({
        no: '③',
        label: '③ LLM 兜底 · 未启用（等效 L1）',
        type: 'info',
      })
    }
  } else {
    layers.push({
      no: '③',
      label: '③ LLM 兜底 · 未启用',
      type: 'info',
    })
  }
  return layers
}

/**
 * 「本次管线」摘要（T1-3）：检测完成后，从各阶段计时推断哪些层实际跑了。
 *
 * 与 {@link computePipelineLayers}（检测前：按设置/级别推演「将走哪些层」）互补：
 * 本函数回答「这次实际跑了什么、各花了多久」。
 * 计时字段为 null / 缺失 = 该层本次未运行（未启用 / 未触发 / 服务不可用）。
 */
export interface RunLayerSummary {
  /** 层编号（① / ② / ②.5 / ③-F / ③ / Σ），用作列表 key */
  no: string
  /** 展示文案（含耗时） */
  label: string
  /** 该层本次是否实际运行 */
  ran: boolean
}

export function summarizeRun(t: {
  rule_ms: number
  nlp_ms?: number | null
  nlp_warmup_ms?: number | null
  decision_ms?: number | null
  l3_ms?: number | null
  recall_ms?: number | null
  llm_ms?: number | null
  total_ms: number
}): RunLayerSummary[] {
  const out: RunLayerSummary[] = []
  let ruleLabel = `① 规则 ${t.rule_ms}ms`
  if (t.nlp_ms != null) {
    ruleLabel += `（NLP ${t.nlp_ms}ms${t.nlp_warmup_ms != null ? ` 含预热 ${t.nlp_warmup_ms}ms` : ''}）`
  }
  out.push({ no: '①', label: ruleLabel, ran: true })
  out.push({
    no: '②',
    label: t.decision_ms != null ? `② 决策模型 ${t.decision_ms}ms` : '② 决策模型 · 未运行',
    ran: t.decision_ms != null,
  })
  out.push({
    no: '②.5',
    label: t.recall_ms != null ? `②.5 知识召回 ${t.recall_ms}ms` : '②.5 知识召回 · 未运行',
    ran: t.recall_ms != null,
  })
  out.push({
    no: '③-F',
    label: t.l3_ms != null ? `③-F 特征模型 ${t.l3_ms}ms` : '③-F 特征模型 · 未运行',
    ran: t.l3_ms != null,
  })
  out.push({
    no: '③',
    label: t.llm_ms != null ? `③ LLM 兜底 ${t.llm_ms}ms` : '③ LLM 兜底 · 未运行',
    ran: t.llm_ms != null,
  })
  out.push({ no: 'Σ', label: `总耗时 ${t.total_ms}ms`, ran: true })
  return out
}

/**
 * 分阶段进度条（header 下方 StageProgress 组件）：把后端阶段事件序列映射为进度视图。
 *
 * 事件来源：Web 端轮询 /gec/detect/progress 拿全量快照；桌面端经 detect:stage 事件累加。
 * 每次全量重算（纯函数）：Start 声明计划骨架；并行组（规则/决策/特征，引擎 join!）
 * 开局即运行；知识召回在并行组全部结束后运行；③-T 灰区句定向复查在知识召回后
 * （L3 部分定案留有灰区句时触发，否则记 skipped）；LLM 复核最后；End 收尾。
 * 阶段标识与 gec-core `sources` 层名对齐（rule / decision / feature / knowledge /
 * targeted / llm；③-T 结果层归因仍记 llm，见 DETECT_PIPELINE §5）。
 */
export interface StageView {
  key: string
  /** 层编号（① / ② / ②.5 / ③-F / ③），与「本次管线」摘要一致 */
  no: string
  label: string
  status: 'pending' | 'running' | 'done' | 'unavailable' | 'skipped' | 'canceled'
  ms: number | null
  /** 段级预算块进度（chunk_begin/end 事件聚合；无段级事件的层 = null） */
  chunks: { done: number; total: number } | null
}

export interface StageProgressView {
  stages: StageView[]
  finished: boolean
  totalMs: number | null
}

export const STAGE_META: Record<string, { no: string; label: string }> = {
  rule: { no: '①', label: '规则检测' },
  decision: { no: '②', label: '决策模型' },
  feature: { no: '③-F', label: '特征模型' },
  knowledge: { no: '②.5', label: '知识召回' },
  targeted: { no: '③-T', label: '灰区定向复查' },
  llm: { no: '③', label: 'LLM 复核' },
}

/** 渲染顺序（= 引擎内实际推进顺序）；并行组开局即 running */
export const STAGE_ORDER = ['rule', 'decision', 'feature', 'knowledge', 'targeted', 'llm']
const PARALLEL_STAGES = new Set(['rule', 'decision', 'feature'])

const isSettled = (s: StageView) =>
  s.status === 'done' || s.status === 'unavailable' || s.status === 'skipped'

export function applyStageEvents(events: StageEvent[]): StageProgressView {
  let plan: string[] | null = null
  let finished = false
  let totalMs: number | null = null
  const settled = new Map<string, { status: StageView['status']; ms: number | null }>()
  // 段级事件聚合：同层块并发交错，begin 定 total，end 计 done
  const chunked = new Map<string, { done: number; total: number }>()
  for (const ev of events) {
    if (ev.kind === 'start') plan = ev.stages
    else if (ev.kind === 'stage') {
      settled.set(ev.stage, {
        status: ev.status === 'skipped' ? 'skipped' : ev.status === 'unavailable' ? 'unavailable' : 'done',
        ms: ev.ms,
      })
    } else if (ev.kind === 'chunk_begin') {
      const c = chunked.get(ev.stage) ?? { done: 0, total: 0 }
      c.total = ev.total
      chunked.set(ev.stage, c)
    } else if (ev.kind === 'chunk_end') {
      const c = chunked.get(ev.stage) ?? { done: 0, total: ev.total }
      c.done += 1
      chunked.set(ev.stage, c)
    } else if (ev.kind === 'units') {
      // 块矩阵数据面：阶段视图不消费，由 chunkMatrix 投影（勿落入兼底分支误判为 end）
    } else {
      finished = true
      totalMs = ev.total_ms
    }
  }
  // 未收到 Start（缓存/历史命中、旧服务端）：无骨架可渲染
  if (!plan) return { stages: [], finished, totalMs }
  const ordered = [
    ...STAGE_ORDER.filter((k) => plan!.includes(k)),
    ...plan!.filter((k) => !STAGE_ORDER.includes(k)),
  ]
  const stages: StageView[] = ordered.map((key) => {
    const meta = STAGE_META[key] ?? { no: '', label: key }
    const done = settled.get(key)
    let status: StageView['status']
    let ms: number | null = null
    if (done) {
      status = done.status
      ms = done.ms
    } else if (PARALLEL_STAGES.has(key)) status = 'running'
    else status = 'pending'
    const chunks = chunked.get(key) ?? null
    return { key, no: meta.no, label: meta.label, status, ms, chunks }
  })
  // 串行推进位：并行组全部落定 → 召回转 running；召回及其前序全部落定 → ③-T 转 running；
  // 再全落定（或不在计划）→ LLM 转 running
  const seq = stages.filter((s) => !PARALLEL_STAGES.has(s.key))
  const parallelSettled = stages.filter((s) => PARALLEL_STAGES.has(s.key)).every(isSettled)
  const knowledgeIdx = seq.findIndex((s) => s.key === 'knowledge')
  if (knowledgeIdx >= 0 && seq[knowledgeIdx].status === 'pending' && parallelSettled)
    seq[knowledgeIdx].status = 'running'
  const targeted = stages.find((s) => s.key === 'targeted')
  if (
    targeted &&
    targeted.status === 'pending' &&
    stages.filter((s) => s.key !== 'llm' && s.key !== 'targeted').every(isSettled)
  )
    targeted.status = 'running'
  const llm = stages.find((s) => s.key === 'llm')
  if (llm && llm.status === 'pending' && stages.filter((s) => s.key !== 'llm').every(isSettled))
    llm.status = 'running'
  return { stages, finished, totalMs }
}

/**
 * 数据流视图（StageProgress 的视图切换）：同一份阶段事件流投影到
 * DETECT_PIPELINE §2 的管线节点——S0 规范化与 S1 索引合并（亚毫秒、边界不可观测），
 * S4 合并 / S5 归因 / S6 组装 / S7 提交合并为终节点（同在提交瞬间完成，无独立事件）。
 * 不新增任何事件类型：纯派生，层视图与数据流视图是同一事件流的两个投影，随时可切换。
 */
export interface FlowStep {
  key: string
  /** 管线编号（S0·S1 / S2 / S3 / ②.5 / ③-T / ③ / S4–S7） */
  no: string
  label: string
  status: StageView['status']
  /** 副行：数据面推导事实（分块数 / 层数 / 回退耗时）；无则 '' */
  note: string
}

export interface FlowOptions {
  /** 检测进行中 */
  active?: boolean
  /** 响应已完成（事件里可能没有 End） */
  finished?: boolean
  /** 缓存 / 历史命中：无事件瞬时返回 */
  instant?: boolean
  /** 已取消 / 出错且未收到 End */
  canceled?: boolean
  /** 无事件时的兜底：计时合成完成视图（见 stageViewsFromTimings） */
  stages?: StageView[]
}

const FLOW_META: { key: string; no: string; label: string }[] = [
  { key: 's0', no: 'S0·S1', label: '规范化 · 索引' },
  { key: 's2', no: 'S2', label: '段规划' },
  { key: 's3', no: 'S3', label: '层执行 · 并行组' },
]
const FLOW_TAIL: FlowStep = {
  key: 's47',
  no: 'S4–S7',
  label: '合并 · 归因 · 提交',
  status: 'pending',
  note: '',
}

export function applyFlowEvents(events: StageEvent[], opts: FlowOptions = {}): FlowStep[] {
  if (opts.instant)
    return FLOW_META.map((m) => ({ ...m, status: 'done' as const, note: '' })).concat({
      ...FLOW_TAIL,
      status: 'done',
      note: '缓存/历史回放',
    })
  // 是否已进入管线主体：收到过事件，或有计时合成兜底
  const started = events.length > 0 || Boolean(opts.stages?.length)
  const v = events.length
    ? applyStageEvents(events)
    : { stages: opts.stages ?? [], finished: !opts.active && Boolean(opts.stages?.length), totalMs: null }
  const done = v.finished || opts.finished === true
  const byk = new Map(v.stages.map((s) => [s.key, s]))
  // 中断收尾：未落定一律转 canceled
  const fin = (st: StageView['status']): StageView['status'] =>
    !done && opts.canceled && (st === 'running' || st === 'pending') ? 'canceled' : st
  const out: FlowStep[] = []
  const push = (key: string, no: string, label: string, status: StageView['status'], note = '') =>
    out.push({ key, no, label, status: fin(status), note })
  // S0·S1：检测已发起而事件未达 = 规范化/索引进行中
  push('s0', 'S0·S1', '规范化 · 索引', started ? 'done' : opts.active ? 'running' : 'pending')
  // S2：Start 事件前即完成；副行 = 物理切块次数（§3.1：句级恒 ×1 + 段级每预算层 ×1）
  const chunkStages = new Set<string>()
  for (const ev of events)
    if (ev.kind === 'chunk_begin' || ev.kind === 'chunk_end') chunkStages.add(ev.stage)
  push(
    's2',
    'S2',
    '段规划',
    started ? 'done' : 'pending',
    started ? (chunkStages.size ? `句级 ×1 · 段级 ×${chunkStages.size}` : '句级 ×1') : '',
  )
  // S3 并行组聚合：任一 running → running；全部落定 → done
  const par = v.stages.filter((s) => PARALLEL_STAGES.has(s.key))
  const s3: StageView['status'] = !started
    ? 'pending'
    : par.some((s) => s.status === 'canceled')
      ? 'canceled'
      : par.length && par.every((s) => isSettled(s))
        ? 'done'
        : 'running'
  push(
    's3',
    'S3',
    '层执行 · 并行组',
    s3,
    started ? `${par.filter((s) => s.status === 'done').length}/${par.length} 层` : '',
  )
  for (const key of ['knowledge', 'targeted', 'llm'] as const) {
    const s = byk.get(key)
    if (!s) continue
    const meta =
      key === 'knowledge'
        ? { no: '②.5', label: '知识召回' }
        : key === 'targeted'
          ? { no: '③-T', label: '灰区定向复查' }
          : { no: '③', label: 'LLM 复核' }
    push(key, meta.no, meta.label, s.status, s.status === 'done' && s.ms != null ? `${s.ms}ms` : '')
  }
  push(
    's47',
    FLOW_TAIL.no,
    FLOW_TAIL.label,
    done ? 'done' : opts.canceled ? 'canceled' : 'pending',
  )
  return out
}

/** 把未落定阶段标记为已取消/失败（检测中断或出错时的收尾视图） */
export function markStagesCanceled(stages: StageView[]): StageView[] {
  return stages.map((s) => (s.status === 'running' || s.status === 'pending' ? { ...s, status: 'canceled' } : s))
}

/** 计时兜底合成（桌面端事件丢失 / 旧服务端）：从 DetectTimings 合成完成视图 */
export function stageViewsFromTimings(t: DetectTimings): StageView[] {
  const mk = (key: string, ms: number | null | undefined): StageView | null => {
    if (ms == null) return null
    const meta = STAGE_META[key]
    return { key, no: meta.no, label: meta.label, status: 'done', ms, chunks: null }
  }
  const out: StageView[] = []
  const rule = mk('rule', t.rule_ms)
  if (rule) out.push(rule)
  for (const pair of [
    ['decision', t.decision_ms],
    ['feature', t.l3_ms],
    ['knowledge', t.recall_ms],
    ['llm', t.llm_ms],
  ] as const) {
    const v = mk(pair[0], pair[1])
    if (v) out.push(v)
  }
  return out
}