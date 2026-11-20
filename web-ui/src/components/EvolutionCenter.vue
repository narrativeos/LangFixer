<script setup lang="ts">
import { ref, computed, onMounted, onBeforeUnmount, watch } from 'vue'
import { errMsg } from '../lib/err'
import { useGecStore } from '../stores/gec'

import {
  LButton,
  LCheckbox,
  LDot,
  LEmpty,
  LProgress,
  LSection,
  LSegmented,
  LSelect,
  LSwitch,
  LTable,
  LTag,
  toast,
} from '../ui'
import type { LColumn } from '../ui'
import { api } from '../api/client'
import ObservabilityPanel from './ObservabilityPanel.vue'
import type {
  EvalRunRequest,
  EvalJob,
  EvalLayerStats,
  EvalReport,
  EvalReportSummary,
  ReflectionCenterReport,
  ReflectionCenterSuggestion,
  ReflectionEvalLayer,
  ReflectionReportSummary,
  ReflectionStat,
  ReflectionSuggestionRow,
  ContributionLayer,
  DomainPool,
  LlmTrainingData,
  TrainingJobRow,
  TrainLexiconApplyResponse,
  ContributionResponse,
  OptimizationRound,
  RoundEvaluateResponse,
  RoundReport,
  RoundReportAction,
} from '../types'

type ActiveTab = 'eval' | 'reflect' | 'train' | 'contribution' | 'observability'
const activeTab = ref<ActiveTab>('eval')
const TAB_OPTIONS: { value: ActiveTab; label: string }[] = [
  { value: 'eval', label: '测评' },
  { value: 'reflect', label: '反思' },
  { value: 'train', label: '训练' },
  { value: 'contribution', label: '贡献与轮次' },
  { value: 'observability', label: '可观测' },
]
const store = useGecStore()

// tab 级导航（T1-4）：反思「执行建议」成功后 → 自动切到「训练」tab 查看作业结果
watch(
  () => store.navTabNonce,
  (n) => {
    if (n > 0 && store.navTab) activeTab.value = store.navTab
  },
)

// ─────────────────────────── 运行表单 ───────────────────────────

const BENCHMARKS = [
  { id: 'mucgec', name: 'MuCGEC（zh · F0.5）', splits: ['dev'] },
  { id: 'cscd', name: 'CSCD（zh · F1）', splits: ['cscd', 'sig15', 'med', 'law'] },
  { id: 'nacgec', name: 'NaCGEC（zh · F0.5）', splits: ['nacgec'] },
  // CTCDataset 原生（母语者）集：真实流量代理与泛化性；datasets/prepare_ctc.py
  { id: 'cscdns', name: 'CSCD-NS 原生微博（zh · F1）', splits: ['cscdns'] },
  { id: 'nlpcc23', name: 'NLPCC2023 task8（zh · F1）', splits: ['nlpcc23'] },
  { id: 'sig13', name: 'SIGHAN-2013 修订（zh · F1）', splits: ['sig13'] },
  { id: 'sig14', name: 'SIGHAN-2014 修订（zh · F1）', splits: ['sig14'] },
  { id: 'cctc', name: 'CCTC 跨句（zh · F0.5）', splits: ['cctc'] },
  { id: 'midu', name: '蜜度 2022 真实场景（zh · F0.5）', splits: ['midu'] },
  { id: 'ctc21', name: 'CTC2021 校对集（zh · F0.5）', splits: ['ctc21'] },
  { id: 'jfleg', name: 'JFLEG（en · F0.5）', splits: ['dev', 'test'] },
]
const form = ref<EvalRunRequest>({
  benchmark: 'mucgec',
  split: null,
  arm: 'current',
  /** 默认漏斗模式（自然级联）：贴近真实使用路径；能力模式（强制 LLM）用于测层级能力上限 */
  mode: 'funnel',
  ablation: null,
  subset: 200,
  seed: 42,
  workers: 8,
  fresh: false,
})
const ablDecision = ref(false)
const ablL3 = ref(false)
const ablKnowledge = ref(false)
const models = ref<{ id: string; name: string }[]>([])

/** 默认评测参数（「一键默认评测」用：MuCGEC dev / 当前臂 / 漏斗模式 / 200 句） */
const DEFAULT_FORM: EvalRunRequest = {
  benchmark: 'mucgec',
  split: null,
  arm: 'current',
  mode: 'funnel',
  ablation: null,
  subset: 200,
  seed: 42,
  workers: 8,
  fresh: false,
}
/** 高级选项折叠（T2-2）：默认只显示 基准 / 子集 / 句数，降低首次使用门槛 */
const evalAdvanced = ref(false)

function startDefaultRun() {
  form.value = { ...DEFAULT_FORM }
  ablDecision.value = false
  ablL3.value = false
  ablKnowledge.value = false
  void startRun()
}

const currentBm = computed(() => BENCHMARKS.find((b) => b.id === form.value.benchmark) ?? BENCHMARKS[0])
const armOptions = computed(() => [
  { id: 'current', name: '当前设置（不切换）' },
  ...models.value,
])

// ─────────────────────────── 任务轮询 ───────────────────────────

const job = ref<EvalJob | null>(null)
const running = ref(false)
let pollTimer: number | null = null

async function startRun() {
  const ablation: { decision?: boolean; l3?: boolean; knowledge?: boolean } = {}
  if (ablDecision.value) ablation.decision = false
  if (ablL3.value) ablation.l3 = false
  if (ablKnowledge.value) ablation.knowledge = false
  const body: EvalRunRequest = {
    ...form.value,
    split: form.value.split || null,
    ablation: Object.keys(ablation).length ? ablation : null,
  }
  running.value = true
  report.value = null
  try {
    const r = await api.runEval(body)
    job.value = {
      id: r.job_id,
      status: 'pending',
      progress: { done: 0, total: body.subset ?? 0 },
      created_at: new Date().toISOString(),
    }
    poll()
  } catch (e) {
    running.value = false
    toast(`评测发起失败：${errMsg(e)}`, 'danger')
  }
}

function poll() {
  if (!job.value) return
  api
    .evalJob(job.value.id)
    .then((j) => {
      job.value = j
      if (j.status === 'done') {
        running.value = false
        if (j.report_id) void loadReport(j.report_id)
        void loadReports()
      } else if (j.status === 'error') {
        running.value = false
        toast(j.error || '评测失败', 'danger')
      } else {
        pollTimer = window.setTimeout(poll, 3000)
      }
    })
    .catch((e) => {
      running.value = false
      toast(`进度查询失败：${errMsg(e)}`, 'danger')
    })
}

// ─────────────────────────── 报告 ───────────────────────────

const report = ref<EvalReport | null>(null)
const reports = ref<EvalReportSummary[]>([])
const reportsLoading = ref(false)

async function loadReports() {
  reportsLoading.value = true
  try {
    const r = await api.listEvalReports()
    reports.value = r.reports
  } catch (e) {
    toast(`报告列表加载失败（测评服务未运行？）：${errMsg(e)}`, 'warn')
  } finally {
    reportsLoading.value = false
  }
}

async function loadReport(id: string) {
  try {
    report.value = await api.getEvalReport(id)
  } catch (e) {
    toast(`报告加载失败：${errMsg(e)}`, 'danger')
  }
}

// A/B 对比：选两份报告并排 diff
const compareA = ref('')
const compareB = ref('')
const reportA = ref<EvalReport | null>(null)
const reportB = ref<EvalReport | null>(null)

async function loadCompare() {
  if (!compareA.value || !compareB.value) return
  try {
    reportA.value = await api.getEvalReport(compareA.value)
    reportB.value = await api.getEvalReport(compareB.value)
  } catch (e) {
    toast(`对比加载失败：${errMsg(e)}`, 'danger')
  }
}

const LAYER_ORDER = ['rule', 'decision', 'knowledge', 'feature', 'statistical', 'llm']
const LAYER_NAMES: Record<string, string> = {
  rule: '① 规则',
  decision: '② 决策',
  knowledge: '②.5 知识',
  feature: '③-F L3',
  statistical: '③-S 统计',
  llm: '③ LLM',
}

const layerRows = computed<LayerView[]>(() => {
  if (!report.value) return []
  return LAYER_ORDER.filter((k) => report.value!.layers[k]).map((k) => ({
    name: LAYER_NAMES[k] ?? k,
    ...report.value!.layers[k],
  }))
})

const diffRows = computed(() => {
  if (!reportA.value || !reportB.value) return []
  const a = reportA.value.overall
  const b = reportB.value.overall
  const num = (v: unknown) => (typeof v === 'number' ? v : 0)
  return [
    { label: 'Fβ（严格）', a: num(a.f_beta), b: num(b.f_beta), delta: num(b.f_beta) - num(a.f_beta) },
    { label: 'P（严格）', a: num(a.p_strict), b: num(b.p_strict), delta: num(b.p_strict) - num(a.p_strict) },
    { label: 'R（严格）', a: num(a.r_strict), b: num(b.r_strict), delta: num(b.r_strict) - num(a.r_strict) },
    { label: 'R（重叠）', a: num(a.r_overlap), b: num(b.r_overlap), delta: num(b.r_overlap) - num(a.r_overlap) },
    { label: 'FP（重叠）', a: num(a.fp_overlap), b: num(b.fp_overlap), delta: num(b.fp_overlap) - num(a.fp_overlap) },
    { label: '句级门控 F1', a: num(a.gate?.f1), b: num(b.gate?.f1), delta: num(b.gate?.f1) - num(a.gate?.f1) },
  ]
})

// ─────────────────────────── 反思中心（P1） ───────────────────────────

const reflectLang = ref('zh')
const reflectIncludeEval = ref(true)
const reflectRunning = ref(false)
const reflectReport = ref<ReflectionCenterReport | null>(null)
const reflectReports = ref<ReflectionReportSummary[]>([])
const reflectSuggestions = ref<ReflectionSuggestionRow[]>([])
const sugStatusFilter = ref('')

const SUG_KIND_NAMES: Record<string, string> = {
  lexicon_condition: '词表条件化',
  rule_tune: '规则调参',
  l3_sample: 'L3 样本',
  llm_arm: 'LLM 臂',
  lexicon_add: '改错对补充',
}
const SUG_STATUS_NAMES: Record<string, string> = {
  pending: '待处理',
  applied: '已执行',
  consumed: '已消费',
  dismissed: '已忽略',
}

async function runReflect() {
  reflectRunning.value = true
  reflectReport.value = null
  try {
    const r = await api.runReflect({
      lang: reflectLang.value,
      include_eval: reflectIncludeEval.value,
    })
    reflectReport.value = r.report
    void loadReflectSuggestions()
    void loadReflectReports()
  } catch (e) {
    toast(`反思报告生成失败：${errMsg(e)}`, 'danger')
  } finally {
    reflectRunning.value = false
  }
}

async function loadReflectReports() {
  try {
    const r = await api.listReflectReports()
    reflectReports.value = r.reports
  } catch (e) {
    toast(`反思报告列表加载失败：${errMsg(e)}`, 'warn')
  }
}

async function loadReflectReport(id: number) {
  try {
    const r = await api.getReflectReport(id)
    reflectReport.value = r.report
    void loadReflectSuggestions()
  } catch (e) {
    toast(`报告加载失败：${errMsg(e)}`, 'danger')
  }
}

async function loadReflectSuggestions() {
  try {
    const r = await api.listReflectSuggestions(sugStatusFilter.value || undefined, undefined)
    reflectSuggestions.value = r.suggestions
  } catch (e) {
    toast(`建议队列加载失败：${errMsg(e)}`, 'warn')
  }
}

async function setSuggestionStatus(id: number, status: string) {
  try {
    await api.updateReflectSuggestion(id, status)
    toast(`建议 #${id} 已标记为「${SUG_STATUS_NAMES[status] ?? status}」`, 'ok')
    void loadReflectSuggestions()
  } catch (e) {
    toast(`状态更新失败：${errMsg(e)}`, 'danger')
  }
}

// 改错对补充（lexicon_add，知识源发布反哺）：采纳 → 写入学习知识（向量召回层）
async function adoptLexiconAdd(row: ReflectionSuggestionRow) {
  const p = row.payload as {
    lang?: string
    wrong?: string
    right?: string
    error_type?: string
  }
  if (!p?.wrong || !p?.right) {
    toast('建议载荷缺少 wrong / right，无法采纳', 'warn')
    return
  }
  runningSuggestion.value = row.id
  try {
    await api.addLearnedKnowledge({
      lang: p.lang ?? row.lang,
      text: p.wrong,
      error_type: p.error_type ?? '',
      correction: p.right,
      source: 'knowledge_ingest',
    })
    await api.updateReflectSuggestion(row.id, 'applied')
    toast(`已采纳：「${p.wrong} → ${p.right}」进入学习知识召回库`, 'ok')
    void loadReflectSuggestions()
  } catch (e) {
    toast(`采纳失败：${errMsg(e)}`, 'danger')
  } finally {
    runningSuggestion.value = null
  }
}

// ─────────────────────────── 训练中心（P2） ───────────────────────────

const trainLang = ref('zh')
const poolCleanSentences = ref('')
const poolResult = ref<DomainPool | null>(null)
const poolBusy = ref(false)
const llmDataResult = ref<LlmTrainingData | null>(null)
const llmBusy = ref(false)
const l3RetrainResult = ref<Record<string, unknown> | null>(null)
const l3RetrainBusy = ref(false)
const lexRuleId = ref('')
const lexWords = ref('')
const lexResult = ref<TrainLexiconApplyResponse | null>(null)
const lexBusy = ref(false)
const trainJobs = ref<TrainingJobRow[]>([])
const trainJobsLoading = ref(false)
const jobStatusFilter = ref('')
const runningSuggestion = ref<number | null>(null)

const JOB_KIND_NAMES: Record<string, string> = {
  domain_pool: '领域池',
  l3_retrain: 'L3 重训练',
  lexicon_condition: '词表条件化',
  llm_data: 'LLM 训练数据',
}
const JOB_STATUS_NAMES: Record<string, string> = {
  proposed: '已提案',
  confirmed: '已确认',
  running: '执行中',
  completed: '已完成',
  failed: '失败',
}

async function loadTrainJobs() {
  trainJobsLoading.value = true
  try {
    const r = await api.listTrainingJobs(undefined, jobStatusFilter.value || undefined)
    trainJobs.value = r.jobs
  } catch (e) {
    toast(`作业列表加载失败：${errMsg(e)}`, 'warn')
  } finally {
    trainJobsLoading.value = false
  }
}

async function buildPool() {
  poolBusy.value = true
  try {
    const clean = poolCleanSentences.value
      .split('\n')
      .map((s) => s.trim())
      .filter(Boolean)
    const r = await api.trainBuildPool({ lang: trainLang.value, clean_sentences: clean })
    poolResult.value = r.pool
    toast(`领域池已构建：${r.pool.total} 条（error ${r.pool.error_count} / clean ${r.pool.clean_count}）`, 'ok')
    void loadTrainJobs()
  } catch (e) {
    toast(`领域池构建失败：${errMsg(e)}`, 'danger')
  } finally {
    poolBusy.value = false
  }
}

async function buildLlmData() {
  llmBusy.value = true
  try {
    const r = await api.trainBuildLlmData({ lang: trainLang.value })
    llmDataResult.value = r.data
    toast(`LLM 训练数据：${r.data.samples.length} 条 SFT 样本`, 'ok')
    void loadTrainJobs()
  } catch (e) {
    toast(`LLM 训练数据构建失败：${errMsg(e)}`, 'danger')
  } finally {
    llmBusy.value = false
  }
}

async function runL3Retrain() {
  l3RetrainBusy.value = true
  try {
    const r = await api.trainRunL3Retrain({ lang: trainLang.value })
    l3RetrainResult.value = r.result
    const swapped = r.result['swapped'] === true
    toast(swapped ? 'L3 重训练完成：模型已热切换' : 'L3 重训练完成（保留旧模型）', 'ok')
    void loadTrainJobs()
  } catch (e) {
    toast(`L3 重训练失败（feature-model 服务可达？）：${errMsg(e)}`, 'danger')
    void loadTrainJobs()
  } finally {
    l3RetrainBusy.value = false
  }
}

async function applyLexicon() {
  lexBusy.value = true
  try {
    const words = lexWords.value
      .split('\n')
      .map((s) => s.trim())
      .filter(Boolean)
    const r = await api.trainApplyLexicon({
      lang: trainLang.value,
      rule_id: lexRuleId.value || undefined,
      words: words.length ? words : undefined,
    })
    lexResult.value = r
    toast(`条件化入库：${r.applied.length} 条新增（${r.skipped} 条已存在）`, 'ok')
    void loadTrainJobs()
  } catch (e) {
    toast(`条件化入库失败：${errMsg(e)}`, 'danger')
  } finally {
    lexBusy.value = false
  }
}

/** 训练作业结果摘要（T1-4）：与「训练」tab 作业表「结果」列同一语义，
 *  用于「执行建议」成功后的即时反馈（用户不必等切 tab 才知道结果） */
function jobResultSummary(result: Record<string, unknown>): string {
  if (result.error != null) return String(result.error)
  if (result.n_samples != null) return `${result.n_samples} 条样本`
  if (Array.isArray(result.applied))
    return `${result.applied.length} 条入库（${result.skipped ?? 0} 已存在）`
  if (result.swapped != null)
    return `热切换=${result.swapped}（F1 ${result.before_f1 ?? '—'} → ${result.after_f1 ?? '—'}）`
  if (result.total != null)
    return `池 ${result.total} 条（error ${result.error_count} / clean ${result.clean_count}）`
  return ''
}

/** 建议 → 训练作业（P1 → P2 消费入口；rule_tune 为人工操作，不可执行）
 *  执行成功后自动切到「训练」tab：作业行的完整结果在那里可查 */
async function runSuggestion(id: number) {
  runningSuggestion.value = id
  try {
    const r = await api.trainRunSuggestion(id)
    const summary = jobResultSummary(r.result)
    toast(
      `建议 #${id} 已执行：${JOB_KIND_NAMES[r.job_kind] ?? r.job_kind} 作业 #${r.job_id}` +
        (summary ? `（${summary}）` : ''),
      'ok',
    )
    store.requestNavTab('train')
    void loadReflectSuggestions()
    void loadTrainJobs()
  } catch (e) {
    toast(`建议执行失败（作业已记录为 failed，可重试）：${errMsg(e)}`, 'danger')
    void loadReflectSuggestions()
    void loadTrainJobs()
  } finally {
    runningSuggestion.value = null
  }
}

function pct(v?: number): string {
  return v == null ? '—' : `${(v * 100).toFixed(1)}%`
}

onMounted(async () => {
  void loadReflectReports()
  void loadReflectSuggestions()
  void loadReflectReports()
  void loadReflectSuggestions()
  void loadTrainJobs()
  void loadReports()
  try {
    const s = await api.getSettings()
    models.value = (s.models ?? []).map((m) => ({ id: m.id, name: m.name }))
  } catch {
    /* 模型列表不可用时仅「当前设置」臂可用 */
  }
})

onBeforeUnmount(() => {
  if (pollTimer) window.clearTimeout(pollTimer)
})

// ─────────────────────────── 贡献与轮次（P3） ───────────────────────────

/** 已有报告组（按 benchmark/split/arm/mode 去重，新→旧）——候选直接来自测评报告列表 */
const contribGroups = computed(() => {
  const m = new Map<
    string,
    { key: string; benchmark: string; split: string | null; arm: string; mode: string; count: number; ts: string }
  >()
  for (const r of reports.value) {
    const key = [r.benchmark, r.split ?? '', r.arm, r.mode].join('|')
    const g = m.get(key)
    if (g) {
      g.count += 1
      if (r.ts > g.ts) g.ts = r.ts
    } else {
      m.set(key, { key, benchmark: r.benchmark, split: r.split ?? null, arm: r.arm, mode: r.mode, count: 1, ts: r.ts })
    }
  }
  return [...m.values()].sort((a, b) => b.ts.localeCompare(a.ts))
})

/** 选中的报告组（空 = 自动取最新组） */
const contribGroup = ref('')
const contribLoading = ref(false)
const contribution = ref<ContributionResponse | null>(null)

function groupLabel(g: { benchmark: string; split: string | null; arm: string; mode: string; count: number }) {
  return `${g.benchmark} / ${g.split ?? '—'} / ${g.arm} / ${g.mode}（${g.count} 份报告）`
}

/** 一键测量层级贡献：自动补跑缺失报告（全开 → 决策层关 → L3 层关 → 知识层关，逐层单次），跑完自动出表 */
type AblStep = {
  key: 'full' | 'decision' | 'l3' | 'knowledge'
  label: string
  spec: { decision?: boolean; l3?: boolean; knowledge?: boolean } | null
  status: 'pending' | 'running' | 'done' | 'skipped' | 'error'
  prog?: { done: number; total: number }
  note?: string
}
const ablPlan = ref<AblStep[]>([])
const ablRunning = ref(false)

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms))

/** 报告消融字段是否与目标规格一致（full = 无消融；单层关 ≠ 双关） */
function ablationMatches(
  abl: { decision?: boolean; l3?: boolean; knowledge?: boolean } | null | undefined,
  spec: AblStep['spec'],
) {
  if (spec === null) return !abl
  const a = abl ?? {}
  if (spec.decision === false) return a.decision === false && a.l3 !== false && a.knowledge !== false
  if (spec.l3 === false) return a.l3 === false && a.decision !== false && a.knowledge !== false
  if (spec.knowledge === false) return a.knowledge === false && a.decision !== false && a.l3 !== false
  return false
}

async function autoMeasure() {
  if (ablRunning.value) return
  if (reports.value.length === 0) await loadReports()
  // 选定组；无任何报告时退回当前「测评」表单参数（一键 = 连基线评测也代跑）
  const g =
    contribGroups.value.find((x) => x.key === contribGroup.value) ?? {
      key: '',
      benchmark: form.value.benchmark,
      split: form.value.split,
      arm: form.value.arm,
      mode: form.value.mode,
      count: 0,
      ts: '',
    }
  const inGroup = reports.value.filter(
    (r) =>
      r.benchmark === g.benchmark &&
      (r.split ?? null) === g.split &&
      r.arm === g.arm &&
      r.mode === g.mode,
  )
  const specs: { key: AblStep['key']; label: string; spec: AblStep['spec'] }[] = [
    { key: 'full', label: '全开（各层全开）', spec: null },
    { key: 'decision', label: '关闭决策层', spec: { decision: false } },
    { key: 'l3', label: '关闭 L3 层', spec: { l3: false } },
    { key: 'knowledge', label: '关闭知识层', spec: { knowledge: false } },
  ]
  ablPlan.value = specs.map((s) => ({
    ...s,
    status: inGroup.some((r) => ablationMatches(r.ablation, s.spec)) ? 'skipped' : 'pending',
    note: undefined,
  }))
  ablRunning.value = true
  let failed = false
  for (const step of ablPlan.value) {
    if (step.status === 'skipped' || failed) continue
    step.status = 'running'
    try {
      const r = await api.runEval({
        benchmark: g.benchmark,
        split: g.split,
        arm: g.arm,
        mode: g.mode as 'capability' | 'funnel',
        ablation: step.spec,
        subset: form.value.subset,
        seed: form.value.seed,
        workers: form.value.workers,
        fresh: false,
      })
      for (;;) {
        const j = await api.evalJob(r.job_id)
        step.prog = j.progress
        if (j.status === 'done') break
        if (j.status === 'error') throw new Error(j.error || '评测失败')
        await sleep(3000)
      }
      step.status = 'done'
    } catch (e) {
      step.status = 'error'
      step.note = errMsg(e)
      failed = true
      break
    }
  }
  ablRunning.value = false
  if (!failed) {
    await loadReports()
    await loadContributions()
  }
}

async function loadContributions() {
  contribLoading.value = true
  try {
    if (reports.value.length === 0) await loadReports()
    const g = contribGroups.value.find((x) => x.key === contribGroup.value)
    contribution.value = await api.evalContributions({
      benchmark: g?.benchmark ?? null,
      split: g?.split ?? null,
      arm: g?.arm ?? null,
      mode: g?.mode ?? null,
    })
  } catch (e) {
    toast(`贡献测量失败：${errMsg(e)}`, 'danger')
  } finally {
    contribLoading.value = false
  }
}

const roundLang = ref('zh')
const roundEvalResult = ref<RoundEvaluateResponse | null>(null)
const roundEvaluating = ref(false)
const rounds = ref<OptimizationRound[]>([])
const roundsLoading = ref(false)
const actingRoundId = ref<number | null>(null)

async function evaluateRound() {
  roundEvaluating.value = true
  try {
    roundEvalResult.value = await api.roundEvaluate({ lang: roundLang.value })
    if (roundEvalResult.value.action === 'round_proposed') {
      toast(`轮次 #${roundEvalResult.value.round!.id} 已提案（待人工确认）`, 'ok')
    } else {
      toast('无需开轮：' + (roundEvalResult.value.reasons ?? []).join('；'), 'info')
    }
    await loadRounds()
  } catch (e) {
    toast(`自动评估失败：${errMsg(e)}`, 'danger')
  } finally {
    roundEvaluating.value = false
  }
}

async function loadRounds() {
  roundsLoading.value = true
  try {
    const r = await api.roundList(roundLang.value || undefined)
    rounds.value = r.rounds
  } catch (e) {
    toast(`轮次列表加载失败：${errMsg(e)}`, 'danger')
  } finally {
    roundsLoading.value = false
  }
}

async function confirmRound(id: number) {
  actingRoundId.value = id
  try {
    await api.roundConfirm(id)
    toast(`轮次 #${id} 已确认`, 'ok')
    await loadRounds()
  } catch (e) {
    toast(`确认失败：${errMsg(e)}`, 'danger')
  } finally {
    actingRoundId.value = null
  }
}

async function rejectRound(id: number) {
  actingRoundId.value = id
  try {
    await api.roundReject(id)
    toast(`轮次 #${id} 已拒绝`, 'ok')
    await loadRounds()
  } catch (e) {
    toast(`拒绝失败：${errMsg(e)}`, 'danger')
  } finally {
    actingRoundId.value = null
  }
}

async function executeRound(id: number) {
  actingRoundId.value = id
  try {
    const r = await api.roundExecute(id)
    toast(
      `轮次 #${id} ${r.round.status === 'completed' ? '执行完成' : '执行失败'}`,
      r.round.status === 'completed' ? 'ok' : 'danger',
    )
    await loadRounds()
  } catch (e) {
    toast(`执行失败：${errMsg(e)}`, 'danger')
  } finally {
    actingRoundId.value = null
  }
}

// 轮次报告（聚合视图：背景 + 各动作完整结果 + 执行后测评证据）
const roundReport = ref<RoundReport | null>(null)
const roundReportVisible = ref(false)
const roundReportLoading = ref(false)

async function viewRoundReport(id: number) {
  roundReportVisible.value = true
  roundReportLoading.value = true
  try {
    roundReport.value = await api.roundReport(id)
  } catch (e) {
    toast(`报告加载失败：${errMsg(e)}`, 'danger')
    roundReportVisible.value = false
  } finally {
    roundReportLoading.value = false
  }
}

function roundActionsLabel(row: OptimizationRound): string {
  return (row.actions ?? []).map((a) => `${a.kind}→${a.target}`).join('、')
}

type LTone = 'neutral' | 'ok' | 'warn' | 'danger' | 'accent'

function roundStatusTone(status: string): LTone {
  switch (status) {
    case 'proposed':
      return 'warn'
    case 'confirmed':
    case 'running':
      return 'accent'
    case 'completed':
      return 'ok'
    case 'failed':
      return 'danger'
    default:
      return 'neutral'
  }
}

function roundResultSummary(row: OptimizationRound): string {
  const acts = (row.result?.actions ?? []) as { kind: string; status: string; error?: string }[]
  if (!acts.length) return ''
  return acts
    .map((a) => `${a.kind}→${a.status}${a.error ? `（${a.error}）` : ''}`)
    .join('；')
}

// ─────────────────────────── 闭环导览（T2-1） ───────────────────────────
// 新用户面对 4 个 tab 不知道从哪开始。导览条按真实状态动态高亮「建议的下一步」：
// 无测评报告 → 先跑基线；有 pending 建议 → 去反思消费；有未处理作业 → 去训练查看；
// 否则高亮当前 tab。点步骤可跳转（「检测积累」跳回检测工作区）。
const evoSteps: { key: string; label: string; tab: 'eval' | 'reflect' | 'train' | 'contribution' | null }[] = [
  { key: 'detect', label: '检测积累', tab: null },
  { key: 'reflect', label: '反思', tab: 'reflect' },
  { key: 'train', label: '训练', tab: 'train' },
  { key: 'eval', label: '测评', tab: 'eval' },
]
const evoPendingSuggestions = computed(() =>
  reflectSuggestions.value.filter((s) => s.status === 'pending').length,
)
const evoActionableJobs = computed(() =>
  trainJobs.value.filter((j) => j.status === 'proposed' || j.status === 'failed').length,
)
const evoRecommended = computed<{ idx: number; hint: string }>(() => {
  if (reports.value.length === 0)
    return {
      idx: 3,
      hint: '尚无测评报告：先在「测评」跑一次当前状态基线（默认参数即可），后续改进才有量化依据',
    }
  if (evoPendingSuggestions.value > 0)
    return {
      idx: 1,
      hint: `${evoPendingSuggestions.value} 条待处理建议：到「反思」查看并消费（执行 / 忽略）`,
    }
  if (evoActionableJobs.value > 0)
    return {
      idx: 2,
      hint: `${evoActionableJobs.value} 个训练作业待处理（proposed / failed）：到「训练」查看`,
    }
  const cur = evoSteps.findIndex((s) => s.tab === activeTab.value)
  return { idx: cur >= 0 ? cur : 0, hint: '' }
})
function goEvoStep(idx: number) {
  const s = evoSteps[idx]
  if (!s) return
  if (s.tab) activeTab.value = s.tab
  else store.requestNav('workspace')
}

// ─────────────────────── 原语绑定辅助（纯展示层，不改作业逻辑） ───────────────────────

const asStr = (v: string | number): string => String(v)

const BENCHMARK_OPTIONS = BENCHMARKS.map((b) => ({ value: b.id, label: b.name }))
const LANG_OPTIONS = [
  { value: 'zh', label: 'zh' },
  { value: 'en', label: 'en' },
]
const MODE_OPTIONS: { value: 'capability' | 'funnel'; label: string }[] = [
  { value: 'capability', label: '能力（强制 LLM）' },
  { value: 'funnel', label: '漏斗（自然级联）' },
]
const armSelectOptions = computed(() => armOptions.value.map((a) => ({ value: a.id, label: a.name })))
const splitOptions = computed(() => [
  { value: '', label: '全部' },
  ...currentBm.value.splits.map((s) => ({ value: s, label: s })),
])
const compareOptions = computed(() =>
  reports.value.map((r) => ({ value: r.id, label: `${r.arm} · F${r.f_beta?.toFixed(3) ?? '—'}` })),
)
const sugStatusOptions = computed(() => [
  { value: '', label: '全部' },
  ...Object.entries(SUG_STATUS_NAMES).map(([value, label]) => ({ value, label })),
])
const jobStatusOptions = computed(() => [
  { value: '', label: '全部' },
  ...Object.entries(JOB_STATUS_NAMES).map(([value, label]) => ({ value, label })),
])
const contribGroupOptions = computed(() => [
  { value: '', label: '自动（取最新组）' },
  ...contribGroups.value.map((g) => ({ value: g.key, label: groupLabel(g) })),
])

function setSplit(v: string | number) {
  form.value.split = v === '' ? null : String(v)
}
function setSubset(e: Event) {
  const v = (e.target as HTMLInputElement).value
  form.value.subset = v === '' ? null : Number(v)
}
function setPoolText(e: Event) {
  poolCleanSentences.value = (e.target as HTMLTextAreaElement).value
}
function setLexWords(e: Event) {
  lexWords.value = (e.target as HTMLTextAreaElement).value
}
function onSugFilter(v: string | number) {
  sugStatusFilter.value = String(v)
  void loadReflectSuggestions()
}
function onJobFilter(v: string | number) {
  jobStatusFilter.value = String(v)
  void loadTrainJobs()
}

function jobStatusTone(status: string): LTone {
  return status === 'done' ? 'ok' : status === 'error' ? 'danger' : 'warn'
}
function jobStatusLabel(status: string): string {
  return status === 'done' ? '完成' : status === 'error' ? '失败' : status === 'running' ? '运行中' : '排队中'
}
function trainStatusTone(status: string): LTone {
  return status === 'failed' ? 'danger' : status === 'completed' ? 'ok' : 'warn'
}
function sugStatusTone(status: string): LTone {
  return status === 'pending' ? 'warn' : status === 'dismissed' ? 'neutral' : 'ok'
}
function ablStepTone(s: AblStep['status']): LTone {
  return s === 'done' || s === 'skipped' ? 'ok' : s === 'error' ? 'danger' : s === 'running' ? 'warn' : 'neutral'
}
function ablStepLabel(s: AblStep['status']): string {
  return s === 'skipped' ? '已有，跳过' : s === 'running' ? '运行中' : s === 'done' ? '完成' : s === 'error' ? '失败' : '等待'
}
function ablDotTone(s: AblStep['status']): 'ok' | 'warn' | 'danger' | 'muted' {
  return s === 'done' || s === 'skipped' ? 'ok' : s === 'error' ? 'danger' : s === 'running' ? 'warn' : 'muted'
}

/** 训练作业结果摘要（与 jobResultSummary 同语义；行内空值回退破折号） */
function resHint(row: TrainingJobRow): string {
  return row.result ? jobResultSummary(row.result) || '—' : '—'
}

// ─────────────────────── LTable 列定义 ───────────────────────

type LayerView = { name: string } & EvalLayerStats

type EvalFunnelStep = EvalReport['funnel']['cumulative'][number]

const layerCols: LColumn<LayerView>[] = [
  { key: 'name', label: '层', width: '110px' },
  { key: 'n_pred', label: '预测', width: '70px', align: 'right' },
  { key: 'tp_strict', label: 'TP 严格', width: '80px', align: 'right' },
  { key: 'p_strict', label: 'P', width: '70px', align: 'right' },
  { key: 'r_strict', label: 'R', width: '70px', align: 'right' },
  { key: 'f_beta', label: 'Fβ', width: '80px', align: 'right' },
  { key: 'recall_share', label: '召回份额', width: '90px', align: 'right' },
  { key: 'fp_overlap', label: 'FP 重叠', width: '80px', align: 'right' },
  { key: 'gate', label: '门控 F1', width: '80px', align: 'right' },
]

const funnelCumCols: LColumn<EvalFunnelStep>[] = [
  { key: 'prefix', label: '级联前缀' },
  { key: 'resolved', label: '解决 gold 编辑', width: '140px', align: 'right' },
  { key: 'rate', label: '累计解决率', width: '120px', align: 'right' },
]

const reportCols: LColumn<EvalReportSummary>[] = [
  { key: 'id', label: '报告', width: '30%' },
  { key: 'arm', label: '臂', width: '90px' },
  { key: 'mode', label: '模式', width: '100px' },
  { key: 'n_sentences', label: '句数', width: '70px', align: 'right' },
  { key: 'f_beta', label: 'Fβ', width: '80px', align: 'right' },
  { key: 'p_strict', label: 'P', width: '70px', align: 'right' },
  { key: 'r_strict', label: 'R', width: '70px', align: 'right' },
  { key: 'llm_reach', label: 'LLM 触达', width: '90px', align: 'right' },
]

const diffCols: LColumn<{ label: string; a: number; b: number; delta: number }>[] = [
  { key: 'label', label: '指标' },
  { key: 'a', label: 'A', align: 'right' },
  { key: 'b', label: 'B', align: 'right' },
  { key: 'delta', label: 'Δ（B−A）', width: '120px', align: 'right' },
]

const statCols: LColumn<ReflectionStat>[] = [
  { key: 'source', label: '层', width: '110px' },
  { key: 'rule_id', label: '规则', width: '140px' },
  { key: 'adopted', label: '采纳', width: '80px', align: 'right' },
  { key: 'rejected', label: '放弃', width: '80px', align: 'right' },
  { key: 'last_at', label: '最近', width: '30%' },
]

const evalLayerCols: LColumn<ReflectionEvalLayer>[] = [
  { key: 'name', label: '层', width: '110px' },
  { key: 'f_beta', label: 'Fβ', width: '80px', align: 'right' },
  { key: 'recall_share', label: '召回份额', width: '100px', align: 'right' },
  { key: 'fp_overlap', label: 'FP 重叠', width: '90px', align: 'right' },
  { key: 'gate_f1', label: '门控 F1', width: '90px', align: 'right' },
]

const sugCols: LColumn<ReflectionCenterSuggestion>[] = [
  { key: 'kind', label: '类型', width: '120px' },
  { key: 'target', label: '目标', width: '140px' },
  { key: 'summary', label: '建议', width: '40%' },
  { key: 'evidence', label: '证据', width: '32%' },
]

const sugQueueCols: LColumn<ReflectionSuggestionRow>[] = [
  { key: 'kind', label: '类型', width: '120px' },
  { key: 'target', label: '目标', width: '140px' },
  { key: 'summary', label: '建议', width: '36%' },
  { key: 'status', label: '状态', width: '90px' },
  { key: 'ops', label: '操作', width: '260px' },
]

const reflectReportCols: LColumn<ReflectionReportSummary>[] = [
  { key: 'report', label: '报告', width: '50%' },
  { key: 'lang', label: '语言', width: '80px' },
  { key: 'n_suggestions', label: '建议数', width: '80px', align: 'right' },
]

const trainJobCols: LColumn<TrainingJobRow>[] = [
  { key: 'kind', label: '作业', width: '130px' },
  { key: 'lang', label: '语言', width: '70px' },
  { key: 'status', label: '状态', width: '90px' },
  { key: 'source', label: '来源建议', width: '90px' },
  { key: 'result', label: '结果', width: '36%' },
  { key: 'created_at', label: '时间', width: '170px' },
]

const contribCols: LColumn<ContributionLayer>[] = [
  { key: 'layer', label: '层', width: '110px' },
  { key: 'f_beta_full', label: 'Fβ 全开', width: '100px', align: 'right' },
  { key: 'f_beta_ablated', label: 'Fβ 该层关闭', width: '110px', align: 'right' },
  { key: 'delta', label: '边际贡献 Δ', width: '120px', align: 'right' },
  { key: 'tp_self', label: '自决 TP', width: '110px', align: 'right' },
]

const roundCols: LColumn<OptimizationRound>[] = [
  { key: 'id', label: '#', width: '50px', align: 'right' },
  { key: 'lang', label: '语言', width: '60px' },
  { key: 'status', label: '状态', width: '110px' },
  { key: 'reason', label: '原因 / 动作', width: '34%' },
  { key: 'ops', label: '操作', width: '220px' },
]

const roundActionCols: LColumn<RoundReportAction>[] = [
  { key: 'kind', label: '动作', width: '150px' },
  { key: 'target', label: '目标', width: '140px' },
  { key: 'summary', label: '结果摘要', width: '50%' },
]

function fmtNum(v: number | null | undefined, digits = 3): string {
  return v == null ? '—' : v.toFixed(digits)
}
</script>

<template>
  <div class="evolution-center">
    <!-- 闭环导览（T2-1）：按真实状态高亮建议的下一步；点步骤可跳转 -->
    <div class="evo-guide">
      <div class="evo-steps">
        <template v-for="(s, i) in evoSteps" :key="s.key">
          <span v-if="i > 0" class="evo-step-arrow">→</span>
          <button
            type="button"
            class="evo-step l-focusable"
            :class="{ 'is-done': i < evoRecommended.idx, 'is-current': i === evoRecommended.idx }"
            @click="goEvoStep(i)"
          >
            {{ s.label }}
          </button>
        </template>
      </div>
      <span v-if="evoRecommended.hint" class="evo-guide-hint">
        {{ evoRecommended.hint }}
      </span>
    </div>
    <LSegmented v-model="activeTab" :options="TAB_OPTIONS" class="evo-tabs" />
    <div class="evo-body">
      <!-- ═══════════════ 测评（P0） ═══════════════ -->
      <div v-if="activeTab === 'eval'">
        <div class="eval-grid">
          <LSection title="发起评测" desc="检测 → 反思 → 训练 → 测试 闭环的「测试」环节">
            <div class="field">
              <label class="field-label">基准</label>
              <LSelect
                class="field-select"
                :model-value="form.benchmark"
                :options="BENCHMARK_OPTIONS"
                @update:model-value="form.benchmark = asStr($event)"
              />
            </div>
            <div v-if="currentBm.splits.length > 1" class="field">
              <label class="field-label">子集</label>
              <LSelect
                class="field-select"
                :model-value="form.split ?? ''"
                :options="splitOptions"
                @update:model-value="setSplit"
              />
            </div>
            <div class="field">
              <label class="field-label">句数（空 = 全量）</label>
              <input
                class="l-input field-select"
                type="number"
                min="0"
                max="100000"
                step="100"
                placeholder="全量"
                :value="form.subset ?? ''"
                @input="setSubset"
              />
            </div>
            <div class="eval-actions">
              <LButton variant="primary" :loading="running" @click="startRun">
                {{ running ? '评测中…' : '开始评测' }}
              </LButton>
              <LButton
                :disabled="running"
                title="以默认参数跑一次并覆盖当前表单：MuCGEC dev · 当前设置 · 漏斗模式（自然级联）· 200 句（首次使用推荐）"
                @click="startDefaultRun"
              >
                一键默认评测
              </LButton>
            </div>
            <LButton variant="ghost" size="sm" @click="evalAdvanced = !evalAdvanced">
              {{ evalAdvanced ? '收起高级选项' : '展开高级选项（臂 / 模式 / 消融 / 缓存）' }}
            </LButton>
            <div v-if="evalAdvanced" class="eval-advanced">
              <div class="field">
                <label class="field-label">纠错模型臂</label>
                <LSelect
                  class="field-select"
                  :model-value="form.arm ?? ''"
                  :options="armSelectOptions"
                  @update:model-value="form.arm = asStr($event)"
                />
              </div>
              <div class="field">
                <span class="field-label">模式</span>
                <LSegmented
                  :model-value="form.mode ?? 'funnel'"
                  :options="MODE_OPTIONS"
                  @update:model-value="form.mode = $event"
                />
              </div>
              <div class="field">
                <span class="field-label">消融（临时关闭某层，测边际贡献）</span>
                <div class="field-row">
                  <LCheckbox v-model="ablDecision" label="关闭决策层" />
                  <LCheckbox v-model="ablL3" label="关闭 L3 层" />
                  <LCheckbox v-model="ablKnowledge" label="关闭知识层" />
                </div>
              </div>
              <div class="field-row">
                <LCheckbox
                  :model-value="form.fresh === true"
                  label="忽略缓存重跑（fresh）"
                  @update:model-value="form.fresh = Boolean($event)"
                />
              </div>
            </div>
          </LSection>
          <LSection v-if="job" title="任务进度">
            <template #actions>
              <LTag :tone="jobStatusTone(job.status)">{{ jobStatusLabel(job.status) }}</LTag>
            </template>
            <LProgress
              :value="job.progress.total ? job.progress.done / job.progress.total : 0"
            />
            <p class="job-meta">
              {{ job.progress.done }} / {{ job.progress.total }} 句
              <template v-if="job.config">
                · 臂 {{ job.config.llm_model_id }}
                · decision={{ job.config.decision_enabled }} l3={{ job.config.l3_enabled }}
              </template>
            </p>
            <p v-if="job.error" class="job-err">{{ job.error }}</p>
            <p v-if="job.warnings?.length" class="job-err">{{ job.warnings.join('；') }}</p>
          </LSection>
        </div>

        <LSection
          v-if="report"
          :title="`报告：${report.benchmark}${report.split ? ` / ${report.split}` : ''} · 臂 ${report.arm} · ${report.mode}`"
          :desc="`${report.ts} · ${report.n_sentences} 句 · gold 编辑 ${report.n_gold_edits}`"
        >
          <template #actions>
            <LTag v-if="report.ablation">
              消融 {{ Object.entries(report.ablation).filter(([, v]) => v === false).map(([k]) => k).join('+') }}
            </LTag>
          </template>

          <div class="overall-row">
            <div class="metric"><span class="k">F{{ report.beta }}（严格）</span><span class="v">{{ report.overall.f_beta?.toFixed(3) }}</span></div>
            <div class="metric"><span class="k">P（严格）</span><span class="v">{{ report.overall.p_strict?.toFixed(3) }}</span></div>
            <div class="metric"><span class="k">R（严格）</span><span class="v">{{ report.overall.r_strict?.toFixed(3) }}</span></div>
            <div class="metric"><span class="k">R（重叠）</span><span class="v">{{ report.overall.r_overlap?.toFixed(3) }}</span></div>
            <div class="metric"><span class="k">FP（重叠）</span><span class="v">{{ report.overall.fp_overlap }}</span></div>
            <div class="metric"><span class="k">句级门控 F1</span><span class="v">{{ report.overall.gate.f1?.toFixed(3) }}</span></div>
            <div v-if="report.funnel" class="metric"><span class="k">LLM 触达</span><span class="v">{{ pct(report.funnel.llm_reach) }}</span></div>
            <div v-if="report.funnel?.llm_calls != null" class="metric"><span class="k">LLM 调用</span><span class="v">{{ report.funnel.llm_calls }} 次（{{ report.funnel.llm_calls_per_100 }} / 百句）</span></div>
          </div>

          <h4 class="sec-t">层级归因（级联序；召回份额 = 该层「最先解决」的 gold 编辑占比）</h4>
          <LTable :columns="layerCols" :rows="layerRows" row-key="name" compact>
            <template #col-p_strict="{ row }">{{ fmtNum(row.p_strict) }}</template>
            <template #col-r_strict="{ row }">{{ fmtNum(row.r_strict) }}</template>
            <template #col-f_beta="{ row }"><b>{{ fmtNum(row.f_beta) }}</b></template>
            <template #col-recall_share="{ row }">{{ pct(row.recall_share) }}</template>
            <template #col-gate="{ row }">{{ fmtNum(row.gate.f1) }}</template>
          </LTable>

          <template v-if="report.funnel && report.funnel.cumulative.length">
            <h4 class="sec-t">漏斗（级联前缀累计解决率；LLM 触达率 {{ pct(report.funnel.llm_reach) }}，平均 {{ report.funnel.avg_llm_ms }}ms）</h4>
            <LTable :columns="funnelCumCols" :rows="report.funnel.cumulative" compact>
              <template #col-rate="{ row }">{{ pct(row.rate) }}</template>
            </LTable>
          </template>
        </LSection>
        <LSection title="历史报告" desc="选两份做 A/B diff">
          <p v-if="reportsLoading" class="hint">加载中…</p>
          <LTable v-else :columns="reportCols" :rows="reports" row-key="id" compact>
            <template #col-id="{ row }">
              <LButton variant="ghost" size="sm" @click="loadReport(row.id)">{{ row.id }}</LButton>
            </template>
            <template #col-f_beta="{ row }">{{ row.f_beta?.toFixed(3) ?? '—' }}</template>
            <template #col-p_strict="{ row }">{{ row.p_strict?.toFixed(3) ?? '—' }}</template>
            <template #col-r_strict="{ row }">{{ row.r_strict?.toFixed(3) ?? '—' }}</template>
            <template #col-llm_reach="{ row }">{{ pct(row.llm_reach) }}</template>
          </LTable>
          <div class="compare-row">
            <LSelect
              class="compare-select"
              :model-value="compareA"
              :options="compareOptions"
              @update:model-value="compareA = asStr($event)"
            />
            <LSelect
              class="compare-select"
              :model-value="compareB"
              :options="compareOptions"
              @update:model-value="compareB = asStr($event)"
            />
            <LButton size="sm" :disabled="!compareA || !compareB" @click="loadCompare">对比</LButton>
          </div>
          <template v-if="reportA && reportB">
            <h4 class="sec-t">A/B diff（{{ reportA.arm }} vs {{ reportB.arm }}）</h4>
            <LTable :columns="diffCols" :rows="diffRows" compact>
              <template #col-a="{ row }">{{ row.a.toFixed(3) }}</template>
              <template #col-b="{ row }">{{ row.b.toFixed(3) }}</template>
              <template #col-delta="{ row }">
                <b :class="row.delta >= 0 ? 'delta-up' : 'delta-down'">{{ row.delta >= 0 ? '+' : '' }}{{ row.delta.toFixed(3) }}</b>
              </template>
            </LTable>
          </template>
        </LSection>
      </div>

      <!-- ═══════════════ 反思（P1） ═══════════════ -->
      <div v-if="activeTab === 'reflect'">
        <LSection title="反思报告" desc="反馈聚合（采纳 / 放弃）+ 测评证据 → 发现 + 建议队列">
          <div class="compare-row">
            <LSelect
              class="field-select-narrow"
              :options="LANG_OPTIONS"
              :model-value="reflectLang"
              @update:model-value="reflectLang = asStr($event)"
            />
            <span class="field-inline">
              <LSwitch
                v-model="reflectIncludeEval"
                aria-label="含测评证据"
              />
              <span class="hint">含测评证据</span>
            </span>
            <LButton variant="primary" :loading="reflectRunning" @click="runReflect">
              生成反思报告
            </LButton>
          </div>
          <template v-if="reflectReport">
            <h4 class="sec-t">发现</h4>
            <ul class="findings">
              <li v-for="(f, i) in reflectReport.findings ?? []" :key="i">{{ f }}</li>
            </ul>
            <h4 class="sec-t">反馈统计</h4>
            <LTable :columns="statCols" :rows="reflectReport.stats ?? []" compact />
            <template
              v-if="reflectReport.eval_evidence && reflectReport.eval_evidence.report_id"
            >
              <h4 class="sec-t">
                测评证据（{{ reflectReport.eval_evidence.benchmark }} /
                {{ reflectReport.eval_evidence.arm }} · Fβ
                {{ (reflectReport.eval_evidence.f_beta ?? 0).toFixed(3) }}）
              </h4>
              <LTable :columns="evalLayerCols" :rows="reflectReport.eval_evidence.layers ?? []" compact>
                <template #col-f_beta="{ row }">{{ fmtNum(row.f_beta) }}</template>
                <template #col-recall_share="{ row }">{{ pct(row.recall_share) }}</template>
                <template #col-gate_f1="{ row }">{{ fmtNum(row.gate_f1) }}</template>
              </LTable>
            </template>
            <h4 class="sec-t">建议（{{ reflectReport.suggestions?.length ?? 0 }}）</h4>
            <LTable :columns="sugCols" :rows="reflectReport.suggestions ?? []" compact>
              <template #col-kind="{ row }">{{ SUG_KIND_NAMES[row.kind] ?? row.kind }}</template>
              <template #col-evidence="{ row }">
                <div v-for="(e, i) in row.evidence ?? []" :key="i" class="hint">{{ e }}</div>
              </template>
            </LTable>
          </template>
          <LEmpty
            v-else
            text="还没有反思报告"
            hint="点击「生成反思报告」聚合使用反馈（采纳 / 放弃）与测评证据"
          />
        </LSection>
        <LSection title="建议队列">
          <template #actions>
            <LSelect
              class="field-select-narrow"
              :options="sugStatusOptions"
              :model-value="sugStatusFilter"
              @update:model-value="onSugFilter"
            />
            <span class="hint">P2 训练中心消费入口：消费后标记已消费 / 忽略</span>
          </template>
          <LTable v-if="reflectSuggestions.length" :columns="sugQueueCols" :rows="reflectSuggestions" compact>
            <template #col-kind="{ row }">{{ SUG_KIND_NAMES[row.kind] ?? row.kind }}</template>
            <template #col-status="{ row }">
              <LTag :tone="sugStatusTone(row.status)">{{ SUG_STATUS_NAMES[row.status] ?? row.status }}</LTag>
            </template>
            <template #col-ops="{ row }">
              <span class="ops-row">
                <LButton
                  v-if="row.status === 'pending' && row.kind === 'lexicon_add'"
                  size="sm"
                  variant="primary"
                  :loading="runningSuggestion === row.id"
                  @click="adoptLexiconAdd(row)"
                >
                  采纳入召回库
                </LButton>
                <LButton
                  v-if="row.status === 'pending' && row.kind !== 'rule_tune' && row.kind !== 'lexicon_add'"
                  size="sm"
                  variant="primary"
                  :loading="runningSuggestion === row.id"
                  @click="runSuggestion(row.id)"
                >
                  执行
                </LButton>
                <LButton
                  v-if="row.status === 'pending'"
                  size="sm"
                  @click="setSuggestionStatus(row.id, 'consumed')"
                >
                  标记已消费
                </LButton>
                <LButton
                  v-if="row.status === 'pending'"
                  size="sm"
                  @click="setSuggestionStatus(row.id, 'dismissed')"
                >
                  忽略
                </LButton>
              </span>
            </template>
          </LTable>
          <LEmpty
            v-else
            text="暂无建议"
            hint="先在上方点「生成反思报告」——聚合使用反馈（采纳 / 放弃）与测评证据后自动生成"
          />
        </LSection>

        <LSection title="反思报告历史">
          <LTable :columns="reflectReportCols" :rows="reflectReports" compact>
            <template #col-report="{ row }">
              <LButton variant="ghost" size="sm" @click="loadReflectReport(row.id)">
                #{{ row.id }} · {{ row.created_at }}
              </LButton>
            </template>
          </LTable>
        </LSection>
      </div>

      <!-- ═══════════════ 训练（P2） ═══════════════ -->
      <div v-if="activeTab === 'train'">
        <LSection title="训练作业" desc="每次训练动作一行作业（状态机 proposed → confirmed → running → completed / failed）">
          <template #actions>
            <LSelect
              class="field-select-narrow"
              :options="jobStatusOptions"
              :model-value="jobStatusFilter"
              @update:model-value="onJobFilter"
            />
            <LButton size="sm" :loading="trainJobsLoading" @click="loadTrainJobs">刷新</LButton>
          </template>
          <p v-if="trainJobsLoading" class="hint">加载中…</p>
          <LTable :columns="trainJobCols" :rows="trainJobs" compact>
            <template #col-kind="{ row }">{{ JOB_KIND_NAMES[row.kind] ?? row.kind }}</template>
            <template #col-status="{ row }">
              <LTag :tone="trainStatusTone(row.status)">{{ JOB_STATUS_NAMES[row.status] ?? row.status }}</LTag>
            </template>
            <template #col-source="{ row }">
              {{ row.source_suggestion ? `#${row.source_suggestion}` : '—' }}
            </template>
            <template #col-result="{ row }">
              <span v-if="row.result?.error != null" class="job-err">{{ String(row.result.error) }}</span>
              <span v-else class="hint">{{ resHint(row) }}</span>
            </template>
          </LTable>
        </LSection>
        <div class="eval-grid">
          <LSection title="领域池构建">
            <template #actions>
              <LSelect
                class="field-select-narrow"
                :options="LANG_OPTIONS"
                :model-value="trainLang"
                @update:model-value="trainLang = asStr($event)"
              />
            </template>
            <div class="hint sec-gap">
              模型层反馈（adopt=error / reject=clean）+ 飞轮确认样本 + 增强器干净句（互补单侧分配去重）
            </div>
            <textarea
              class="l-area"
              rows="3"
              placeholder="增强器：干净句（每行一条；用户自有语料——基准集句子禁止入池，防污染）"
              :value="poolCleanSentences"
              @input="setPoolText"
            ></textarea>
            <div class="btn-row">
              <LButton variant="primary" size="sm" :loading="poolBusy" @click="buildPool">
                构建领域池
              </LButton>
            </div>
            <template v-if="poolResult">
              <h4 class="sec-t">
                池：{{ poolResult.total }} 条（error {{ poolResult.error_count }} / clean
                {{ poolResult.clean_count }}）
              </h4>
              <div class="hint">
                <div v-for="[src, n] in poolResult.by_source" :key="src">{{ src }} × {{ n }}</div>
              </div>
            </template>
          </LSection>

          <div class="stack-col">
            <LSection title="L3 重训练（飞轮）">
              <template #actions>
                <LButton variant="primary" size="sm" :loading="l3RetrainBusy" @click="runL3Retrain">
                  触发重训练
                </LButton>
              </template>
              <div class="hint">
                合并确认样本 → 重训练 → dev 评测 → 热切换（F1 显著退化时保留旧模型）
              </div>
              <div v-if="l3RetrainResult" class="hint sec-gap-top">
                热切换：{{ l3RetrainResult.swapped ? '是' : '否' }}
                <template v-if="l3RetrainResult.reason">（{{ l3RetrainResult.reason }}）</template>
                · F1：{{ l3RetrainResult.before_f1 ?? '—' }} →
                {{ l3RetrainResult.after_f1 ?? '—' }}
              </div>
            </LSection>

            <LSection title="词表条件化入库" desc="条件化排除条目（入库前先过测评消融）">
              <LInput
                v-model="lexRuleId"
                placeholder="规则 id（如 ZH_CONFUSABLE）"
                aria-label="规则 id"
              />
              <textarea
                class="l-area sec-gap-top"
                rows="2"
                placeholder="排除文本（每行一条）"
                :value="lexWords"
                @input="setLexWords"
              ></textarea>
              <div class="btn-row sec-gap-top">
                <LButton size="sm" :loading="lexBusy" @click="applyLexicon">入库</LButton>
                <span v-if="lexResult" class="hint">
                  {{ lexResult.applied.length }} 条新增（{{ lexResult.skipped }} 已存在）
                </span>
              </div>
            </LSection>

            <LSection title="LLM 训练数据">
              <template #actions>
                <LButton size="sm" :loading="llmBusy" @click="buildLlmData">
                  构建（SFT JSONL）
                </LButton>
              </template>
              <div class="hint">LLM 层 adopt 反馈（带修正）→ SFT 样本；可导出外部训练</div>
              <template v-if="llmDataResult">
                <div class="hint sec-gap-top">
                  {{ llmDataResult.samples.length }} 条样本（arm:
                  {{ llmDataResult.arm || '—' }}）
                </div>
                <pre v-if="llmDataResult.jsonl" class="hint jsonl-pre">{{ llmDataResult.jsonl }}</pre>
              </template>
            </LSection>
          </div>
        </div>
      </div>

      <!-- ═══════════════ 贡献与轮次（P3） ═══════════════ -->
      <div v-if="activeTab === 'contribution'">
        <LSection title="层级贡献测量（消融 Δ 常态化）">
          <div class="eval-grid">
            <div>
              <p class="hint">
                从测评报告派生：每层边际贡献 = Fβ(全开) − Fβ(该层关闭)。
                点「一键测量层级贡献」会自动补跑缺失的报告（一次只关一层），跑完直接出表。
              </p>
              <div class="field">
                <div class="field-label">报告组</div>
                <LSelect
                  class="field-select field-select-wide"
                  :options="contribGroupOptions"
                  :model-value="contribGroup"
                  :disabled="ablRunning"
                  @update:model-value="contribGroup = asStr($event)"
                />
              </div>
              <div class="btn-row">
                <LButton variant="primary" :loading="ablRunning" @click="autoMeasure">
                  一键测量层级贡献
                </LButton>
                <LButton :disabled="ablRunning" :loading="contribLoading" @click="loadContributions">
                  重新测量
                </LButton>
              </div>
              <ul v-if="ablPlan.length" class="l-timeline sec-gap-top">
                <li v-for="s in ablPlan" :key="s.key">
                  <LDot :tone="ablDotTone(s.status)" />
                  <LTag :tone="ablStepTone(s.status)">{{ ablStepLabel(s.status) }}</LTag>
                  <span>{{ s.label }}</span>
                  <span v-if="s.status === 'running' && s.prog" class="hint">{{ s.prog.done }}/{{ s.prog.total }} 句</span>
                  <span v-if="s.status === 'error'" class="job-err">{{ s.note }}</span>
                </li>
              </ul>
              <p v-if="ablRunning" class="hint sec-gap-top">
                后台顺序执行中；离开本页任务不中断，回来点「一键测量层级贡献」续跑
              </p>
            </div>
            <div v-if="contribution">
              <p class="hint">
                组：{{ contribution.group.benchmark }}
                <template v-if="contribution.group.split"> / {{ contribution.group.split }}</template>
                · 臂 {{ contribution.group.arm }} · {{ contribution.group.mode }}
                · 全开 Fβ <b>{{ contribution.table.f_beta_full?.toFixed(3) ?? '—' }}</b>
                <template v-if="contribution.table.f_beta_core != null">
                  · 核心基线（两层皆关）{{ contribution.table.f_beta_core.toFixed(3) }}</template>
                <LTag
                  v-if="contribution.table.f_beta_full != null && contribution.table.f_beta_full <= 0.05"
                  tone="warn"
                >
                  全开 Fβ 极低（{{ contribution.table.f_beta_full.toFixed(3) }}）：漏斗级联可能在上游丢失了大部分金句，看「测评」页该报告的 gate 召回 / LLM 触达率
                </LTag>
              </p>
              <LTable v-if="contribution.table.layers.length" :columns="contribCols" :rows="contribution.table.layers" compact>
                <template #col-f_beta_full="{ row }">{{ fmtNum(row.f_beta_full) }}</template>
                <template #col-f_beta_ablated="{ row }">{{ fmtNum(row.f_beta_ablated) }}</template>
                <template #col-delta="{ row }">
                  <b :class="row.delta >= 0 ? 'delta-up' : 'delta-down'">
                    {{ row.delta >= 0 ? '+' : '' }}{{ row.delta.toFixed(3) }}
                  </b>
                </template>
                <template #col-tp_self="{ row }">
                  <span>{{ row.tp_self ?? 0 }}</span>
                  <LTag v-if="row.topological" tone="neutral">拓扑</LTag>
                </template>
              </LTable>
              <p
                v-if="contribution.table.layers.some((l) => l.topological)"
                class="hint sec-gap-top"
              >
                ⚠ 拓扑假象：标「拓扑」的层自身零定案（自决 TP = 0），其 Δ 来自「该层作为下游开关」的级联效应
                （关闭它连带改变了下游层的执行），不代表该层自身的解决能力——解读时该 Δ 不应记在该层名下。
              </p>
              <div v-else class="hint sec-gap-top">
                <p class="flat-p">
                  该组只有全开报告（{{ contribution.table.reports }} 份），没有消融报告——贡献表需要「单层关闭」的消融报告才能逐层对比。
                </p>
                <p class="hint sec-gap-top">
                  点上方「一键测量层级贡献」：自动补跑缺失的消融（一次只关一层），跑完直接出表。
                </p>
              </div>
              <p v-if="contribution.table.missing.length && contribution.table.layers.length" class="hint sec-gap-top">
                缺失消融臂：{{ contribution.table.missing.join('、') }}（无对应报告，该层无法测量）
              </p>
            </div>
            <LEmpty v-else text="贡献尚未测量" hint="贡献表从「测评」报告派生（每层边际贡献 = 全开 Fβ − 该层关闭 Fβ）；点下方按钮自动补跑缺失的评测（全开 / 关闭决策层 / 关闭 L3 层），跑完直接出表">
              <template #action>
                <LButton variant="primary" :disabled="ablRunning" @click="autoMeasure">
                  一键测量层级贡献
                </LButton>
              </template>
            </LEmpty>
          </div>
        </LSection>

        <LSection title="优化轮次（自动评估 → 人工确认 → 执行）">
          <template #actions>
            <LButton size="sm" :loading="roundsLoading" @click="loadRounds">刷新</LButton>
          </template>
          <div class="eval-grid">
            <div>
              <div class="field">
                <div class="field-label">语言</div>
                <LSelect
                  class="field-select"
                  :options="LANG_OPTIONS"
                  :model-value="roundLang"
                  @update:model-value="roundLang = asStr($event)"
                />
              </div>
              <LButton variant="primary" :loading="roundEvaluating" @click="evaluateRound">
                自动评估
              </LButton>
              <div v-if="roundEvalResult" class="sec-gap-top">
                <div v-if="roundEvalResult.action === 'round_proposed'" class="l-banner">
                  轮次 #{{ roundEvalResult.round!.id }} 已提案：{{ roundEvalResult.round!.reason }}
                </div>
                <div v-else class="l-banner">
                  <div>无需开轮</div>
                  <div v-for="(r, i) in roundEvalResult.reasons" :key="i" class="hint">{{ r }}</div>
                </div>
              </div>
            </div>
            <div>
              <p v-if="roundsLoading" class="hint">加载中…</p>
              <LTable :columns="roundCols" :rows="rounds" compact>
                <template #col-status="{ row }">
                  <LTag :tone="roundStatusTone(row.status)">{{ row.status }}</LTag>
                </template>
                <template #col-reason="{ row }">
                  <div>{{ row.reason }}</div>
                  <div class="hint">{{ roundActionsLabel(row) }}</div>
                </template>
                <template #col-ops="{ row }">
                  <span class="ops-row">
                    <LButton
                      v-if="row.status === 'proposed'"
                      size="sm"
                      variant="primary"
                      title="确认后进入「已确认」状态：下一步可「执行」（应用优化动作）或「拒绝」；执行完成后点「报告」查看本轮完整结果与执行后测评证据"
                      :loading="actingRoundId === row.id"
                      @click="confirmRound(row.id)"
                    >
                      确认
                    </LButton>
                    <LButton
                      v-if="row.status === 'proposed'"
                      size="sm"
                      :loading="actingRoundId === row.id"
                      @click="rejectRound(row.id)"
                    >
                      拒绝
                    </LButton>
                    <LButton
                      v-if="row.status === 'confirmed'"
                      size="sm"
                      variant="primary"
                      :loading="actingRoundId === row.id"
                      @click="executeRound(row.id)"
                    >
                      执行
                    </LButton>
                    <LButton size="sm" @click="viewRoundReport(row.id)">
                      报告
                    </LButton>
                    <span v-if="row.status === 'completed' || row.status === 'failed'" class="hint">
                      {{ roundResultSummary(row) }}
                    </span>
                  </span>
                </template>
              </LTable>
            </div>
          </div>
          <!-- 轮次报告：内联展开面板（不占用弹窗配额） -->
          <div v-if="roundReportVisible" class="round-report sec-gap-top">
            <div class="round-report-h">
              <h4 class="sec-t">轮次 #{{ roundReport?.round.id ?? '' }} 报告</h4>
              <LButton variant="ghost" size="sm" @click="roundReportVisible = false">收起</LButton>
            </div>
            <p v-if="roundReportLoading" class="hint">加载中…</p>
            <template v-else-if="roundReport">
              <div class="l-banner round-report-banner">
                <LTag :tone="roundStatusTone(roundReport.round.status)">{{ roundReport.summary }}</LTag>
              </div>
              <div class="hint sec-gap-top">
                开轮原因：{{ roundReport.round.reason }}
              </div>
              <LTable :columns="roundActionCols" :rows="roundReport.actions" compact>
                <template #col-summary="{ row }">
                  <div>{{ row.summary }}</div>
                  <div v-if="row.job" class="hint">
                    作业 #{{ row.job.id }}（{{ row.job.status }}）
                    <template v-if="row.job.result">
                      ：{{ JSON.stringify(row.job.result).slice(0, 160) }}
                    </template>
                  </div>
                </template>
              </LTable>
              <div v-if="roundReport.evidence?.length" class="hint sec-gap-top">
                证据：{{ roundReport.evidence.join('；') }}
              </div>
              <div v-if="roundReport.eval_after" class="l-banner sec-gap-top">
                执行后测评：{{ roundReport.eval_after.arm }} Fβ = {{ roundReport.eval_after.f_beta }}（报告 {{ roundReport.eval_after.report_id }}）
              </div>
              <div v-else class="hint sec-gap-top">
                执行后测评证据不可用（测评服务不可达或尚无新报告）
              </div>
            </template>
          </div>
        </LSection>
      </div>

      <!-- ═══════════════ 可观测（§7） ═══════════════ -->
      <div v-if="activeTab === 'observability'">
        <ObservabilityPanel />
      </div>
    </div>
  </div>
</template>

<style scoped>
.evolution-center {
  padding: var(--space-3) var(--space-4);
}
/* 闭环导览条（T2-1） */
.evo-guide {
  display: flex;
  align-items: center;
  gap: var(--space-3);
  margin-bottom: var(--space-3);
}
.evo-steps {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  flex: 1;
  min-width: 320px;
}
.evo-step {
  cursor: pointer;
  border: 1px solid var(--border);
  border-radius: var(--radius-ctl);
  background: var(--surface);
  color: var(--muted);
  font-size: var(--fs-sm);
  line-height: var(--lh-ui);
  padding: var(--space-1) var(--space-2);
}
.evo-step.is-done { color: var(--text-2); }
.evo-step.is-current {
  color: var(--accent);
  border-color: var(--accent);
  font-weight: 600;
}
.evo-step-arrow { color: var(--muted); }
.evo-guide-hint {
  font-size: var(--fs-sm);
  color: var(--warn-ink, var(--warn));
  white-space: nowrap;
}
.evo-tabs { margin-bottom: var(--space-3); }
/* 测评表单：主操作行 + 高级选项折叠区（T2-2） */
.eval-actions { display: flex; gap: var(--space-2); margin: var(--space-1) 0; }
.eval-advanced { margin-top: var(--space-2); }
/* 表单字段布局 */
.field { margin-bottom: var(--space-3); }
.field-label {
  display: block;
  margin-bottom: var(--space-1);
  font-size: var(--fs-sm);
  color: var(--muted);
}
.field-select { width: 100%; }
.field-select-narrow { width: 120px; }
.field-select-wide { max-width: 420px; }
.field-row {
  display: flex;
  align-items: center;
  gap: var(--space-3);
}
.field-inline {
  display: inline-flex;
  align-items: center;
  gap: var(--space-2);
}
.btn-row {
  display: flex;
  align-items: center;
  gap: var(--space-2);
}
.ops-row {
  display: inline-flex;
  align-items: center;
  gap: var(--space-1);
}
.sec-gap { margin-bottom: var(--space-2); }
.sec-gap-top { margin-top: var(--space-2); }
.flat-p { margin: 0; }
.eval-grid {
  display: grid;
  grid-template-columns: 360px 1fr;
  gap: var(--space-3);
  margin-bottom: var(--space-3);
}
.stack-col {
  display: grid;
  gap: var(--space-3);
}
.hint {
  font-size: var(--fs-sm);
  color: var(--muted);
}
.job-meta {
  margin: var(--space-2) 0 0;
  font-size: var(--fs-ui);
}
.job-err {
  color: var(--danger);
  font-size: var(--fs-ui);
  margin: var(--space-1) 0 0;
  word-break: break-all;
}
.overall-row {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-3);
  margin-bottom: var(--space-3);
}
.metric {
  display: flex;
  flex-direction: column;
  gap: 2px;
  padding: var(--space-2) var(--space-4);
  border: 1px solid var(--border);
  border-radius: var(--radius-ctl);
  min-width: 90px;
}
.metric .k {
  font-size: var(--fs-sm);
  color: var(--muted);
}
.metric .v {
  font-size: var(--fs-xl);
  font-weight: 700;
}
.sec-t {
  margin: var(--space-4) 0 var(--space-2);
  font-size: var(--fs-md);
}
.compare-row {
  display: flex;
  align-items: center;
  gap: var(--space-3);
  margin-top: var(--space-3);
}
.compare-select { width: 240px; }
.delta-up { color: var(--ok); }
.delta-down { color: var(--danger); }
.findings {
  margin: 0 0 var(--space-2);
  padding-left: var(--space-5);
  font-size: var(--fs-ui);
  line-height: var(--lh-body);
}
.l-area {
  display: block;
  width: 100%;
  box-sizing: border-box;
  margin-top: var(--space-2);
  padding: var(--space-2);
  font: inherit;
  font-size: var(--fs-ui);
  color: var(--text);
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: var(--radius-ctl);
  resize: vertical;
}
.jsonl-pre {
  max-height: 120px;
  overflow: auto;
  white-space: pre-wrap;
  margin: var(--space-2) 0 0;
  font-family: var(--font-mono);
  font-size: var(--fs-xs);
}
/* 消融计划时间线 */
.l-timeline {
  list-style: none;
  margin: 0;
  padding: 0 0 0 var(--space-4);
  border-left: 2px solid var(--border);
  display: flex;
  flex-direction: column;
  gap: var(--space-2);
}
.l-timeline li {
  display: flex;
  align-items: center;
  gap: var(--space-2);
}
/* 轮次报告内联面板 */
.round-report {
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  background: var(--surface-2);
  padding: var(--space-3);
}
.round-report-h {
  display: flex;
  align-items: center;
  gap: var(--space-2);
}
.round-report-h .l-btn { margin-left: auto; }
.round-report-banner { margin-bottom: var(--space-2); }
</style>