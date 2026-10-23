<script setup lang="ts">
import { ref, computed, onMounted, onBeforeUnmount, watch } from 'vue'
import { errMsg } from '../lib/err'
import { useGecStore } from '../stores/gec'

import { ElMessage } from 'element-plus'
import { api } from '../api/client'
import type {
  EvalRunRequest,
  EvalJob,
  EvalReport,
  EvalReportSummary,
  ReflectionCenterReport,
  ReflectionReportSummary,
  ReflectionSuggestionRow,
  DomainPool,
  LlmTrainingData,
  TrainingJobRow,
  TrainLexiconApplyResponse,
  ContributionResponse,
  OptimizationRound,
  RoundEvaluateResponse,
  RoundReport,
} from '../types'

const activeTab = ref<'eval' | 'reflect' | 'train' | 'contribution'>('eval')
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
    ElMessage.error(`评测发起失败：${errMsg(e)}`)
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
        ElMessage.error(j.error || '评测失败')
      } else {
        pollTimer = window.setTimeout(poll, 3000)
      }
    })
    .catch((e) => {
      running.value = false
      ElMessage.error(`进度查询失败：${errMsg(e)}`)
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
    ElMessage.warning(`报告列表加载失败（测评服务未运行？）：${errMsg(e)}`)
  } finally {
    reportsLoading.value = false
  }
}

async function loadReport(id: string) {
  try {
    report.value = await api.getEvalReport(id)
  } catch (e) {
    ElMessage.error(`报告加载失败：${errMsg(e)}`)
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
    ElMessage.error(`对比加载失败：${errMsg(e)}`)
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

const layerRows = computed(() => {
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
    ElMessage.error(`反思报告生成失败：${errMsg(e)}`)
  } finally {
    reflectRunning.value = false
  }
}

async function loadReflectReports() {
  try {
    const r = await api.listReflectReports()
    reflectReports.value = r.reports
  } catch (e) {
    ElMessage.warning(`反思报告列表加载失败：${errMsg(e)}`)
  }
}

async function loadReflectReport(id: number) {
  try {
    const r = await api.getReflectReport(id)
    reflectReport.value = r.report
    void loadReflectSuggestions()
  } catch (e) {
    ElMessage.error(`报告加载失败：${errMsg(e)}`)
  }
}

async function loadReflectSuggestions() {
  try {
    const r = await api.listReflectSuggestions(sugStatusFilter.value || undefined, undefined)
    reflectSuggestions.value = r.suggestions
  } catch (e) {
    ElMessage.warning(`建议队列加载失败：${errMsg(e)}`)
  }
}

async function setSuggestionStatus(id: number, status: string) {
  try {
    await api.updateReflectSuggestion(id, status)
    ElMessage.success(`建议 #${id} 已标记为「${SUG_STATUS_NAMES[status] ?? status}」`)
    void loadReflectSuggestions()
  } catch (e) {
    ElMessage.error(`状态更新失败：${errMsg(e)}`)
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
    ElMessage.warning('建议载荷缺少 wrong / right，无法采纳')
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
    ElMessage.success(`已采纳：「${p.wrong} → ${p.right}」进入学习知识召回库`)
    void loadReflectSuggestions()
  } catch (e) {
    ElMessage.error(`采纳失败：${errMsg(e)}`)
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
    ElMessage.warning(`作业列表加载失败：${errMsg(e)}`)
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
    ElMessage.success(`领域池已构建：${r.pool.total} 条（error ${r.pool.error_count} / clean ${r.pool.clean_count}）`)
    void loadTrainJobs()
  } catch (e) {
    ElMessage.error(`领域池构建失败：${errMsg(e)}`)
  } finally {
    poolBusy.value = false
  }
}

async function buildLlmData() {
  llmBusy.value = true
  try {
    const r = await api.trainBuildLlmData({ lang: trainLang.value })
    llmDataResult.value = r.data
    ElMessage.success(`LLM 训练数据：${r.data.samples.length} 条 SFT 样本`)
    void loadTrainJobs()
  } catch (e) {
    ElMessage.error(`LLM 训练数据构建失败：${errMsg(e)}`)
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
    ElMessage.success(swapped ? 'L3 重训练完成：模型已热切换' : 'L3 重训练完成（保留旧模型）')
    void loadTrainJobs()
  } catch (e) {
    ElMessage.error(`L3 重训练失败（feature-model 服务可达？）：${errMsg(e)}`)
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
    ElMessage.success(`条件化入库：${r.applied.length} 条新增（${r.skipped} 条已存在）`)
    void loadTrainJobs()
  } catch (e) {
    ElMessage.error(`条件化入库失败：${errMsg(e)}`)
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
    ElMessage.success(
      `建议 #${id} 已执行：${JOB_KIND_NAMES[r.job_kind] ?? r.job_kind} 作业 #${r.job_id}` +
        (summary ? `（${summary}）` : ''),
    )
    store.requestNavTab('train')
    void loadReflectSuggestions()
    void loadTrainJobs()
  } catch (e) {
    ElMessage.error(`建议执行失败（作业已记录为 failed，可重试）：${errMsg(e)}`)
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
    ElMessage.error(`贡献测量失败：${errMsg(e)}`)
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
      ElMessage.success(`轮次 #${roundEvalResult.value.round!.id} 已提案（待人工确认）`)
    } else {
      ElMessage.info('无需开轮：' + (roundEvalResult.value.reasons ?? []).join('；'))
    }
    await loadRounds()
  } catch (e) {
    ElMessage.error(`自动评估失败：${errMsg(e)}`)
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
    ElMessage.error(`轮次列表加载失败：${errMsg(e)}`)
  } finally {
    roundsLoading.value = false
  }
}

async function confirmRound(id: number) {
  actingRoundId.value = id
  try {
    await api.roundConfirm(id)
    ElMessage.success(`轮次 #${id} 已确认`)
    await loadRounds()
  } catch (e) {
    ElMessage.error(`确认失败：${errMsg(e)}`)
  } finally {
    actingRoundId.value = null
  }
}

async function rejectRound(id: number) {
  actingRoundId.value = id
  try {
    await api.roundReject(id)
    ElMessage.success(`轮次 #${id} 已拒绝`)
    await loadRounds()
  } catch (e) {
    ElMessage.error(`拒绝失败：${errMsg(e)}`)
  } finally {
    actingRoundId.value = null
  }
}

async function executeRound(id: number) {
  actingRoundId.value = id
  try {
    const r = await api.roundExecute(id)
    ElMessage[r.round.status === 'completed' ? 'success' : 'error'](
      `轮次 #${id} ${r.round.status === 'completed' ? '执行完成' : '执行失败'}`,
    )
    await loadRounds()
  } catch (e) {
    ElMessage.error(`执行失败：${errMsg(e)}`)
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
    ElMessage.error(`报告加载失败：${errMsg(e)}`)
    roundReportVisible.value = false
  } finally {
    roundReportLoading.value = false
  }
}

function roundActionsLabel(row: OptimizationRound): string {
  return (row.actions ?? []).map((a) => `${a.kind}→${a.target}`).join('、')
}

function roundStatusType(status: string) {
  switch (status) {
    case 'proposed':
      return 'warning'
    case 'confirmed':
      return 'primary'
    case 'running':
      return 'primary'
    case 'completed':
      return 'success'
    case 'failed':
      return 'danger'
    case 'rejected':
      return 'info'
    default:
      return 'info'
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
</script>

<template>
  <div class="evolution-center">
    <!-- 闭环导览（T2-1）：按真实状态高亮建议的下一步；点步骤可跳转 -->
    <div class="evo-guide">
      <el-steps
        :active="evoRecommended.idx"
        :finish-status="'success'"
        class="evo-steps"
        simple
      >
        <el-step
          v-for="(s, i) in evoSteps"
          :key="s.key"
          :title="s.label"
          class="evo-step"
          @click="goEvoStep(i)"
        />
      </el-steps>
      <span v-if="evoRecommended.hint" class="evo-guide-hint">
        {{ evoRecommended.hint }}
      </span>
    </div>
    <el-tabs v-model="activeTab" class="evo-tabs">
      <!-- ═══════════════ 测评（P0） ═══════════════ -->
      <el-tab-pane label="测评" name="eval">
        <div class="eval-grid">
          <el-card shadow="never" class="run-card">
            <template #header>
              <div class="card-h">
                <span class="card-t">发起评测</span>
                <span class="hint">检测 → 反思 → 训练 → 测试 闭环的「测试」环节</span>
              </div>
            </template>
            <el-form label-position="top">
              <el-form-item label="基准">
                <el-select v-model="form.benchmark" style="width: 100%">
                  <el-option v-for="b in BENCHMARKS" :key="b.id" :value="b.id" :label="b.name" />
                </el-select>
              </el-form-item>
              <el-form-item v-if="currentBm.splits.length > 1" label="子集">
                <el-select v-model="form.split" clearable placeholder="全部" style="width: 100%">
                  <el-option v-for="s in currentBm.splits" :key="s" :value="s" :label="s" />
                </el-select>
              </el-form-item>
              <el-form-item label="句数（空 = 全量）">
                <el-input-number v-model="form.subset" :min="0" :max="100000" :step="100"
                  placeholder="全量" controls-position="right" style="width: 100%" />
              </el-form-item>
              <div class="eval-actions">
                <el-button type="primary" :loading="running" @click="startRun">
                  {{ running ? '评测中…' : '开始评测' }}
                </el-button>
                <el-tooltip
                  content="以默认参数跑一次并覆盖当前表单：MuCGEC dev · 当前设置 · 漏斗模式（自然级联）· 200 句（首次使用推荐）"
                  placement="top"
                  :show-after="300"
                >
                  <el-button :disabled="running" @click="startDefaultRun">一键默认评测</el-button>
                </el-tooltip>
              </div>
              <el-button link type="primary" size="small" @click="evalAdvanced = !evalAdvanced">
                {{ evalAdvanced ? '收起高级选项' : '展开高级选项（臂 / 模式 / 消融 / 缓存）' }}
              </el-button>
              <div v-if="evalAdvanced" class="eval-advanced">
                <el-form-item label="纠错模型臂">
                  <el-select v-model="form.arm" style="width: 100%">
                    <el-option v-for="a in armOptions" :key="a.id" :value="a.id" :label="a.name" />
                  </el-select>
                </el-form-item>
                <el-form-item label="模式">
                  <el-radio-group v-model="form.mode">
                    <el-radio-button value="capability">能力（强制 LLM）</el-radio-button>
                    <el-radio-button value="funnel">漏斗（自然级联）</el-radio-button>
                  </el-radio-group>
                </el-form-item>
                <el-form-item label="消融（临时关闭某层，测边际贡献）">
                  <el-checkbox v-model="ablDecision">关闭决策层</el-checkbox>
                  <el-checkbox v-model="ablL3">关闭 L3 层</el-checkbox>
                  <el-checkbox v-model="ablKnowledge">关闭知识层</el-checkbox>
                </el-form-item>
                <el-form-item>
                  <el-checkbox v-model="form.fresh">忽略缓存重跑（fresh）</el-checkbox>
                </el-form-item>
              </div>
            </el-form>
          </el-card>
          <el-card v-if="job" shadow="never" class="run-card">
            <template #header>
              <div class="card-h">
                <span class="card-t">任务进度</span>
                <el-tag size="small" :type="job.status === 'done' ? 'success' : job.status === 'error' ? 'danger' : 'warning'">
                  {{ job.status }}
                </el-tag>
              </div>
            </template>
            <el-progress
              :percentage="job.progress.total ? Math.round((job.progress.done / job.progress.total) * 100) : 0"
              :status="job.status === 'done' ? 'success' : job.status === 'error' ? 'exception' : undefined" />
            <p class="job-meta">
              {{ job.progress.done }} / {{ job.progress.total }} 句
              <template v-if="job.config">
                · 臂 {{ job.config.llm_model_id }}
                · decision={{ job.config.decision_enabled }} l3={{ job.config.l3_enabled }}
              </template>
            </p>
            <p v-if="job.error" class="job-err">{{ job.error }}</p>
            <p v-if="job.warnings?.length" class="job-err">{{ job.warnings.join('；') }}</p>
          </el-card>
        </div>

        <el-card v-if="report" shadow="never" class="report-card">
          <template #header>
            <div class="card-h">
              <span class="card-t">
                报告：{{ report.benchmark }}{{ report.split ? ` / ${report.split}` : '' }}
                · 臂 {{ report.arm }} · {{ report.mode }}
                <el-tag v-if="report.ablation" size="small" type="warning">
                  消融 {{ Object.entries(report.ablation).filter(([, v]) => v === false).map(([k]) => k).join('+') }}
                </el-tag>
              </span>
              <span class="hint">{{ report.ts }} · {{ report.n_sentences }} 句 · gold 编辑 {{ report.n_gold_edits }}</span>
            </div>
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
          <el-table :data="layerRows" size="small" border>
            <el-table-column label="层" prop="name" width="110" />
            <el-table-column label="预测" prop="n_pred" width="70" align="right" />
            <el-table-column label="TP 严格" prop="tp_strict" width="80" align="right" />
            <el-table-column label="P" width="70" align="right"><template #default="{ row }">{{ row.p_strict?.toFixed(3) }}</template></el-table-column>
            <el-table-column label="R" width="70" align="right"><template #default="{ row }">{{ row.r_strict?.toFixed(3) }}</template></el-table-column>
            <el-table-column label="Fβ" width="80" align="right"><template #default="{ row }"><b>{{ row.f_beta?.toFixed(3) }}</b></template></el-table-column>
            <el-table-column label="召回份额" width="90" align="right"><template #default="{ row }">{{ pct(row.recall_share) }}</template></el-table-column>
            <el-table-column label="FP 重叠" prop="fp_overlap" width="80" align="right" />
            <el-table-column label="门控 F1" width="80" align="right"><template #default="{ row }">{{ row.gate.f1?.toFixed(3) }}</template></el-table-column>
          </el-table>

          <template v-if="report.funnel && report.funnel.cumulative.length">
            <h4 class="sec-t">漏斗（级联前缀累计解决率；LLM 触达率 {{ pct(report.funnel.llm_reach) }}，平均 {{ report.funnel.avg_llm_ms }}ms）</h4>
            <el-table :data="report.funnel.cumulative" size="small" border>
              <el-table-column label="级联前缀" prop="prefix" />
              <el-table-column label="解决 gold 编辑" prop="resolved" align="right" width="140" />
              <el-table-column label="累计解决率" width="120" align="right"><template #default="{ row }">{{ pct(row.rate) }}</template></el-table-column>
            </el-table>
          </template>
        </el-card>
        <el-card shadow="never" class="report-card">
          <template #header>
            <div class="card-h">
              <span class="card-t">历史报告</span>
              <span class="hint">选两份做 A/B diff</span>
            </div>
          </template>
          <el-table :data="reports" size="small" border v-loading="reportsLoading">
            <el-table-column label="报告" min-width="260">
              <template #default="{ row }">
                <el-link type="primary" @click="loadReport(row.id)">{{ row.id }}</el-link>
              </template>
            </el-table-column>
            <el-table-column label="臂" prop="arm" width="90" />
            <el-table-column label="模式" prop="mode" width="100" />
            <el-table-column label="句数" prop="n_sentences" width="70" align="right" />
            <el-table-column label="Fβ" width="80" align="right"><template #default="{ row }">{{ row.f_beta?.toFixed(3) ?? '—' }}</template></el-table-column>
            <el-table-column label="P" width="70" align="right"><template #default="{ row }">{{ row.p_strict?.toFixed(3) ?? '—' }}</template></el-table-column>
            <el-table-column label="R" width="70" align="right"><template #default="{ row }">{{ row.r_strict?.toFixed(3) ?? '—' }}</template></el-table-column>
            <el-table-column label="LLM 触达" width="90" align="right"><template #default="{ row }">{{ pct(row.llm_reach) }}</template></el-table-column>
          </el-table>
          <div class="compare-row">
            <el-select v-model="compareA" placeholder="报告 A" size="small" style="width: 240px">
              <el-option v-for="r in reports" :key="r.id" :value="r.id" :label="`${r.arm} · F${r.f_beta?.toFixed(3) ?? '—'}`" />
            </el-select>
            <el-select v-model="compareB" placeholder="报告 B" size="small" style="width: 240px">
              <el-option v-for="r in reports" :key="r.id" :value="r.id" :label="`${r.arm} · F${r.f_beta?.toFixed(3) ?? '—'}`" />
            </el-select>
            <el-button size="small" :disabled="!compareA || !compareB" @click="loadCompare">对比</el-button>
          </div>
          <template v-if="reportA && reportB">
            <h4 class="sec-t">A/B diff（{{ reportA.arm }} vs {{ reportB.arm }}）</h4>
            <el-table :data="diffRows" size="small" border>
              <el-table-column label="指标" prop="label" />
              <el-table-column :label="`A（${reportA.arm}）`" prop="a" align="right" />
              <el-table-column :label="`B（${reportB.arm}）`" prop="b" align="right" />
              <el-table-column label="Δ（B−A）" align="right" width="120">
                <template #default="{ row }">
                  <b :class="row.delta >= 0 ? 'delta-up' : 'delta-down'">{{ row.delta >= 0 ? '+' : '' }}{{ row.delta.toFixed(3) }}</b>
                </template>
              </el-table-column>
            </el-table>
          </template>
        </el-card>
      </el-tab-pane>

      <!-- ═══════════════ 反思（P1） ═══════════════ -->
      <el-tab-pane label="反思" name="reflect">
        <el-card shadow="never" class="report-card">
          <template #header>
            <div class="card-h">
              <span class="card-t">反思报告</span>
              <span class="hint">反馈聚合（采纳 / 放弃）+ 测评证据 → 发现 + 建议队列</span>
            </div>
          </template>
          <div class="compare-row">
            <el-select v-model="reflectLang" size="small" style="width: 100px">
              <el-option label="zh" value="zh" />
              <el-option label="en" value="en" />
            </el-select>
            <el-switch v-model="reflectIncludeEval" active-text="含测评证据" />
            <el-button type="primary" size="small" :loading="reflectRunning" @click="runReflect">
              生成反思报告
            </el-button>
          </div>
          <template v-if="reflectReport">
            <h4 class="sec-t">发现</h4>
            <ul class="findings">
              <li v-for="(f, i) in reflectReport.findings ?? []" :key="i">{{ f }}</li>
            </ul>
            <h4 class="sec-t">反馈统计</h4>
            <el-table :data="reflectReport.stats ?? []" size="small" border>
              <el-table-column label="层" prop="source" width="110" />
              <el-table-column label="规则" prop="rule_id" width="140" />
              <el-table-column label="采纳" prop="adopted" width="80" align="right" />
              <el-table-column label="放弃" prop="rejected" width="80" align="right" />
              <el-table-column label="最近" prop="last_at" min-width="160" />
            </el-table>
            <template
              v-if="reflectReport.eval_evidence && reflectReport.eval_evidence.report_id"
            >
              <h4 class="sec-t">
                测评证据（{{ reflectReport.eval_evidence.benchmark }} /
                {{ reflectReport.eval_evidence.arm }} · Fβ
                {{ (reflectReport.eval_evidence.f_beta ?? 0).toFixed(3) }}）
              </h4>
              <el-table :data="reflectReport.eval_evidence.layers ?? []" size="small" border>
                <el-table-column label="层" prop="name" width="110" />
                <el-table-column label="Fβ" width="80" align="right">
                  <template #default="{ row }">{{ (row.f_beta ?? 0).toFixed(3) }}</template>
                </el-table-column>
                <el-table-column label="召回份额" width="100" align="right">
                  <template #default="{ row }">{{ pct(row.recall_share) }}</template>
                </el-table-column>
                <el-table-column label="FP 重叠" prop="fp_overlap" width="90" align="right" />
                <el-table-column label="门控 F1" width="90" align="right">
                  <template #default="{ row }">
                    {{ row.gate_f1 == null ? '—' : row.gate_f1.toFixed(3) }}
                  </template>
                </el-table-column>
              </el-table>
            </template>
            <h4 class="sec-t">建议（{{ reflectReport.suggestions?.length ?? 0 }}）</h4>
            <el-table :data="reflectReport.suggestions ?? []" size="small" border>
              <el-table-column label="类型" width="120">
                <template #default="{ row }">{{ SUG_KIND_NAMES[row.kind] ?? row.kind }}</template>
              </el-table-column>
              <el-table-column label="目标" prop="target" width="140" />
              <el-table-column label="建议" prop="summary" min-width="280" />
              <el-table-column label="证据" min-width="220">
                <template #default="{ row }">
                  <div v-for="(e, i) in row.evidence ?? []" :key="i" class="hint">{{ e }}</div>
                </template>
              </el-table-column>
            </el-table>
          </template>
          <el-empty
            v-else
            description="点击「生成反思报告」聚合反馈与测评证据"
            :image-size="60"
          />
        </el-card>
        <el-card shadow="never" class="report-card">
          <template #header>
            <div class="card-h">
              <span class="card-t">建议队列</span>
              <el-select
                v-model="sugStatusFilter"
                size="small"
                style="width: 120px"
                @change="loadReflectSuggestions"
              >
                <el-option label="全部" value="" />
                <el-option
                  v-for="(name, k) in SUG_STATUS_NAMES"
                  :key="k"
                  :label="name"
                  :value="k"
                />
              </el-select>
              <span class="hint">P2 训练中心消费入口：消费后标记已消费 / 忽略</span>
            </div>
          </template>
          <el-table
            :data="reflectSuggestions"
            size="small"
            border
            empty-text="暂无建议：先在上方点「生成反思报告」——聚合使用反馈（采纳 / 放弃）与测评证据后自动生成"
          >
            <el-table-column label="类型" width="120">
              <template #default="{ row }">{{ SUG_KIND_NAMES[row.kind] ?? row.kind }}</template>
            </el-table-column>
            <el-table-column label="目标" prop="target" width="140" />
            <el-table-column label="建议" prop="summary" min-width="260" />
            <el-table-column label="状态" width="90">
              <template #default="{ row }">
                <el-tag
                  size="small"
                  :type="
                    row.status === 'pending'
                      ? 'warning'
                      : row.status === 'dismissed'
                        ? 'info'
                        : 'success'
                  "
                >
                  {{ SUG_STATUS_NAMES[row.status] ?? row.status }}
                </el-tag>
              </template>
            </el-table-column>
            <el-table-column label="操作" width="240">
              <template #default="{ row }">
                <el-button
                  v-if="row.status === 'pending' && row.kind === 'lexicon_add'"
                  size="small"
                  type="primary"
                  :loading="runningSuggestion === row.id"
                  @click="adoptLexiconAdd(row)"
                >
                  采纳入召回库
                </el-button>
                <el-button
                  v-if="row.status === 'pending' && row.kind !== 'rule_tune' && row.kind !== 'lexicon_add'"
                  size="small"
                  type="primary"
                  :loading="runningSuggestion === row.id"
                  @click="runSuggestion(row.id)"
                >
                  执行
                </el-button>
                <el-button
                  v-if="row.status === 'pending'"
                  size="small"
                  @click="setSuggestionStatus(row.id, 'consumed')"
                >
                  标记已消费
                </el-button>
                <el-button
                  v-if="row.status === 'pending'"
                  size="small"
                  type="info"
                  @click="setSuggestionStatus(row.id, 'dismissed')"
                >
                  忽略
                </el-button>
              </template>
            </el-table-column>
          </el-table>
        </el-card>

        <el-card shadow="never">
          <template #header>
            <div class="card-h">
              <span class="card-t">反思报告历史</span>
            </div>
          </template>
          <el-table :data="reflectReports" size="small" border>
            <el-table-column label="报告" min-width="220">
              <template #default="{ row }">
                <el-link type="primary" @click="loadReflectReport(row.id)">
                  #{{ row.id }} · {{ row.created_at }}
                </el-link>
              </template>
            </el-table-column>
            <el-table-column label="语言" prop="lang" width="80" />
            <el-table-column label="建议数" prop="n_suggestions" width="80" align="right" />
          </el-table>
        </el-card>
      </el-tab-pane>

      <!-- ═══════════════ 训练（P2） ═══════════════ -->
      <el-tab-pane label="训练" name="train">
        <el-card shadow="never" class="report-card">
          <template #header>
            <div class="card-h">
              <span class="card-t">训练作业</span>
              <el-select
                v-model="jobStatusFilter"
                size="small"
                style="width: 120px"
                @change="loadTrainJobs"
              >
                <el-option label="全部" value="" />
                <el-option
                  v-for="(name, k) in JOB_STATUS_NAMES"
                  :key="k"
                  :label="name"
                  :value="k"
                />
              </el-select>
              <el-button size="small" @click="loadTrainJobs">刷新</el-button>
              <span class="hint">
                每次训练动作一行作业（状态机 proposed → confirmed → running → completed / failed）
              </span>
            </div>
          </template>
          <el-table :data="trainJobs" size="small" border v-loading="trainJobsLoading">
            <el-table-column label="作业" width="130">
              <template #default="{ row }">{{ JOB_KIND_NAMES[row.kind] ?? row.kind }}</template>
            </el-table-column>
            <el-table-column label="语言" prop="lang" width="70" />
            <el-table-column label="状态" width="90">
              <template #default="{ row }">
                <el-tag
                  size="small"
                  :type="
                    row.status === 'failed'
                      ? 'danger'
                      : row.status === 'completed'
                        ? 'success'
                        : 'warning'
                  "
                >
                  {{ JOB_STATUS_NAMES[row.status] ?? row.status }}
                </el-tag>
              </template>
            </el-table-column>
            <el-table-column label="来源建议" width="90">
              <template #default="{ row }">
                {{ row.source_suggestion ? `#${row.source_suggestion}` : '—' }}
              </template>
            </el-table-column>
            <el-table-column label="结果" min-width="300">
              <template #default="{ row }">
                <span v-if="row.result?.error" class="hint">{{ String(row.result.error) }}</span>
                <span v-else-if="row.result" class="hint">
                  {{
                    row.result.n_samples != null
                      ? `${row.result.n_samples} 条样本`
                      : row.result.applied
                        ? `${row.result.applied.length} 条入库（${row.result.skipped ?? 0} 已存在）`
                        : row.result.swapped != null
                          ? `热切换=${row.result.swapped}（F1 ${row.result.before_f1 ?? '—'} → ${row.result.after_f1 ?? '—'}）`
                          : row.result.total != null
                            ? `池 ${row.result.total} 条（error ${row.result.error_count} / clean ${row.result.clean_count}）`
                            : '—'
                  }}
                </span>
                <span v-else class="hint">—</span>
              </template>
            </el-table-column>
            <el-table-column label="时间" prop="created_at" min-width="170" />
          </el-table>
        </el-card>
        <div class="eval-grid">
          <el-card shadow="never">
            <template #header>
              <div class="card-h">
                <span class="card-t">领域池构建</span>
                <el-select v-model="trainLang" size="small" style="width: 90px">
                  <el-option label="zh" value="zh" />
                  <el-option label="en" value="en" />
                </el-select>
              </div>
            </template>
            <div class="hint" style="margin-bottom: 8px">
              模型层反馈（adopt=error / reject=clean）+ 飞轮确认样本 + 增强器干净句（互补单侧分配去重）
            </div>
            <el-input
              v-model="poolCleanSentences"
              type="textarea"
              :rows="3"
              placeholder="增强器：干净句（每行一条；用户自有语料——基准集句子禁止入池，防污染）"
            />
            <div style="margin-top: 8px">
              <el-button type="primary" size="small" :loading="poolBusy" @click="buildPool">
                构建领域池
              </el-button>
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
          </el-card>

          <div style="display: grid; gap: 12px">
            <el-card shadow="never">
              <template #header>
                <div class="card-h">
                  <span class="card-t">L3 重训练（飞轮）</span>
                  <el-button type="primary" size="small" :loading="l3RetrainBusy" @click="runL3Retrain">
                    触发重训练
                  </el-button>
                </div>
              </template>
              <div class="hint">
                合并确认样本 → 重训练 → dev 评测 → 热切换（F1 显著退化时保留旧模型）
              </div>
              <div v-if="l3RetrainResult" class="hint" style="margin-top: 8px">
                热切换：{{ l3RetrainResult.swapped ? '是' : '否' }}
                <template v-if="l3RetrainResult.reason">（{{ l3RetrainResult.reason }}）</template>
                · F1：{{ l3RetrainResult.before_f1 ?? '—' }} →
                {{ l3RetrainResult.after_f1 ?? '—' }}
              </div>
            </el-card>

            <el-card shadow="never">
              <template #header>
                <div class="card-h">
                  <span class="card-t">词表条件化入库</span>
                </div>
              </template>
              <div class="hint">条件化排除条目（入库前先过测评消融）</div>
              <el-input
                v-model="lexRuleId"
                size="small"
                style="margin-top: 8px"
                placeholder="规则 id（如 ZH_CONFUSABLE）"
              />
              <el-input
                v-model="lexWords"
                type="textarea"
                :rows="2"
                style="margin-top: 8px"
                placeholder="排除文本（每行一条）"
              />
              <div style="margin-top: 8px">
                <el-button size="small" :loading="lexBusy" @click="applyLexicon">入库</el-button>
                <span v-if="lexResult" class="hint" style="margin-left: 8px">
                  {{ lexResult.applied.length }} 条新增（{{ lexResult.skipped }} 已存在）
                </span>
              </div>
            </el-card>

            <el-card shadow="never">
              <template #header>
                <div class="card-h">
                  <span class="card-t">LLM 训练数据</span>
                  <el-button size="small" :loading="llmBusy" @click="buildLlmData">
                    构建（SFT JSONL）
                  </el-button>
                </div>
              </template>
              <div class="hint">LLM 层 adopt 反馈（带修正）→ SFT 样本；可导出外部训练</div>
              <template v-if="llmDataResult">
                <div class="hint" style="margin-top: 8px">
                  {{ llmDataResult.samples.length }} 条样本（arm:
                  {{ llmDataResult.arm || '—' }}）
                </div>
                <pre
                  v-if="llmDataResult.jsonl"
                  class="hint"
                  style="max-height: 120px; overflow: auto; white-space: pre-wrap"
                >{{ llmDataResult.jsonl }}</pre>
              </template>
            </el-card>
          </div>
        </div>
      </el-tab-pane>

      <!-- ═══════════════ 贡献与轮次（P3） ═══════════════ -->
      <el-tab-pane label="贡献与轮次" name="contribution">
        <el-card shadow="never" class="report-card">
          <template #header>
            <div class="card-h">
              <span class="card-t">层级贡献测量（消融 Δ 常态化）</span>
            </div>
          </template>
          <div class="eval-grid">
            <div>
              <p class="hint">
                从测评报告派生：每层边际贡献 = Fβ(全开) − Fβ(该层关闭)。
                点「一键测量层级贡献」会自动补跑缺失的报告（一次只关一层），跑完直接出表。
              </p>
              <el-form label-width="84px" size="small">
                <el-form-item label="报告组">
                  <el-select
                    v-model="contribGroup"
                    style="width: 420px"
                    placeholder="自动（最新组）"
                    :disabled="ablRunning"
                  >
                    <el-option label="自动（取最新组）" value="" />
                    <el-option v-for="g in contribGroups" :key="g.key" :label="groupLabel(g)" :value="g.key" />
                  </el-select>
                </el-form-item>
              </el-form>
              <div style="display: flex; gap: 8px">
                <el-button type="primary" :loading="ablRunning" @click="autoMeasure">
                  一键测量层级贡献
                </el-button>
                <el-button :disabled="ablRunning" :loading="contribLoading" @click="loadContributions">
                  重新测量
                </el-button>
              </div>
              <div v-if="ablPlan.length" class="hint" style="margin-top: 10px">
                <div v-for="s in ablPlan" :key="s.key" style="display: flex; gap: 8px; align-items: center; margin: 2px 0">
                  <el-tag size="small" :type="s.status === 'done' || s.status === 'skipped' ? 'success' : s.status === 'error' ? 'danger' : s.status === 'running' ? 'warning' : 'info'">
                    {{ s.status === 'skipped' ? '已有，跳过' : s.status === 'running' ? '运行中' : s.status === 'done' ? '完成' : s.status === 'error' ? '失败' : '等待' }}
                  </el-tag>
                  <span>{{ s.label }}</span>
                  <span v-if="s.status === 'running' && s.prog">{{ s.prog.done }}/{{ s.prog.total }} 句</span>
                  <span v-if="s.status === 'error'" style="color: #f56c6c">{{ s.note }}</span>
                </div>
                <p v-if="ablRunning" class="hint" style="margin: 6px 0 0">
                  后台顺序执行中；离开本页任务不中断，回来点「一键测量层级贡献」续跑
                </p>
              </div>
            </div>
            <div v-if="contribution">
              <p class="hint">
                组：{{ contribution.group.benchmark }}
                <template v-if="contribution.group.split"> / {{ contribution.group.split }}</template>
                · 臂 {{ contribution.group.arm }} · {{ contribution.group.mode }}
                · 全开 Fβ <b>{{ contribution.table.f_beta_full?.toFixed(3) ?? '—' }}</b>
                <template v-if="contribution.table.f_beta_core != null">
                  · 核心基线（两层皆关）{{ contribution.table.f_beta_core.toFixed(3) }}</template>
                <el-tag
                  v-if="contribution.table.f_beta_full != null && contribution.table.f_beta_full <= 0.05"
                  type="warning"
                  size="small"
                  style="margin-left: 6px"
                  >
                  全开 Fβ 极低（{{ contribution.table.f_beta_full.toFixed(3) }}）：漏斗级联可能在上游丢失了大部分金句，看「测评」页该报告的 gate 召回 / LLM 触达率
                </el-tag>
              </p>
              <el-table v-if="contribution.table.layers.length" :data="contribution.table.layers" size="small" border>
                <el-table-column label="层" prop="layer" width="110" />
                <el-table-column label="Fβ 全开" width="100" align="right">
                  <template #default="{ row }">{{ row.f_beta_full.toFixed(3) }}</template>
                </el-table-column>
                <el-table-column label="Fβ 该层关闭" width="110" align="right">
                  <template #default="{ row }">{{ row.f_beta_ablated.toFixed(3) }}</template>
                </el-table-column>
                <el-table-column label="边际贡献 Δ" width="120" align="right">
                  <template #default="{ row }">
                    <b :style="{ color: row.delta >= 0 ? '#67c23a' : '#f56c6c' }">
                      {{ row.delta >= 0 ? '+' : '' }}{{ row.delta.toFixed(3) }}
                    </b>
                  </template>
                </el-table-column>
                <el-table-column label="自决 TP" width="110" align="right">
                  <template #default="{ row }">
                    <span>{{ row.tp_self ?? 0 }}</span>
                    <el-tag v-if="row.topological" type="warning" size="small" style="margin-left: 4px">
                      拓扑
                    </el-tag>
                  </template>
                </el-table-column>
              </el-table>
              <p
                v-if="contribution.table.layers.some((l) => l.topological)"
                class="hint"
                style="margin: 6px 0 0"
              >
                ⚠ 拓扑假象：标「拓扑」的层自身零定案（自决 TP = 0），其 Δ 来自「该层作为下游开关」的级联效应
                （关闭它连带改变了下游层的执行），不代表该层自身的解决能力——解读时该 Δ 不应记在该层名下。
              </p>
              <div v-else class="hint" style="padding: 12px 0">
                <p style="margin: 0">
                  该组只有全开报告（{{ contribution.table.reports }} 份），没有消融报告——贡献表需要「单层关闭」的消融报告才能逐层对比。
                </p>
                <p class="hint" style="margin: 6px 0 0">
                  点上方「一键测量层级贡献」：自动补跑缺失的消融（一次只关一层），跑完直接出表。
                </p>
              </div>
              <p v-if="contribution.table.missing.length && contribution.table.layers.length" class="hint">
                缺失消融臂：{{ contribution.table.missing.join('、') }}（无对应报告，该层无法测量）
              </p>
            </div>
            <el-empty v-else :image-size="60">
              <p style="margin: 0">贡献尚未测量：贡献表从「测评」报告派生（每层边际贡献 = 全开 Fβ − 该层关闭 Fβ）</p>
              <p class="hint" style="margin: 6px 0 0">
                点「一键测量层级贡献」：自动补跑缺失的评测（全开 / 关闭决策层 / 关闭 L3 层），跑完直接出表
              </p>
            </el-empty>
          </div>
        </el-card>

        <el-card shadow="never">
          <template #header>
            <div class="card-h">
              <span class="card-t">优化轮次（自动评估 → 人工确认 → 执行）</span>
              <el-button size="small" @click="loadRounds">刷新</el-button>
            </div>
          </template>
          <div class="eval-grid">
            <div>
              <el-form label-width="84px" size="small">
                <el-form-item label="语言">
                  <el-select v-model="roundLang" style="width: 120px">
                    <el-option label="zh" value="zh" />
                    <el-option label="en" value="en" />
                  </el-select>
                </el-form-item>
              </el-form>
              <el-button type="primary" :loading="roundEvaluating" @click="evaluateRound">
                自动评估
              </el-button>
              <div v-if="roundEvalResult" style="margin-top: 10px">
                <el-alert
                  v-if="roundEvalResult.action === 'round_proposed'"
                  type="success"
                  :closable="false"
                  show-icon
                  :title="`轮次 #${roundEvalResult.round!.id} 已提案：${roundEvalResult.round!.reason}`"
                />
                <el-alert
                  v-else
                  type="info"
                  :closable="false"
                  show-icon
                  title="无需开轮"
                >
                  <div v-for="(r, i) in roundEvalResult.reasons" :key="i" class="hint">{{ r }}</div>
                </el-alert>
              </div>
            </div>
            <div>
              <el-table :data="rounds" size="small" border v-loading="roundsLoading">
                <el-table-column label="#" prop="id" width="50" />
                <el-table-column label="语言" prop="lang" width="60" />
                <el-table-column label="状态" width="110">
                  <template #default="{ row }">
                    <el-tag :type="roundStatusType(row.status)" size="small">{{ row.status }}</el-tag>
                  </template>
                </el-table-column>
                <el-table-column label="原因 / 动作" min-width="260">
                  <template #default="{ row }">
                    <div>{{ row.reason }}</div>
                    <div class="hint">{{ roundActionsLabel(row) }}</div>
                  </template>
                </el-table-column>
                <el-table-column label="操作" width="220">
                  <template #default="{ row }">
                    <el-tooltip
                      v-if="row.status === 'proposed'"
                      content="确认后进入「已确认」状态：下一步可「执行」（应用优化动作）或「拒绝」；执行完成后点「报告」查看本轮完整结果与执行后测评证据"
                      placement="top"
                      :show-after="300"
                    >
                      <el-button
                        size="small"
                        type="primary"
                        :loading="actingRoundId === row.id"
                        @click="confirmRound(row.id)"
                      >
                        确认
                      </el-button>
                    </el-tooltip>
                    <el-button
                      v-if="row.status === 'proposed'"
                      size="small"
                      :loading="actingRoundId === row.id"
                      @click="rejectRound(row.id)"
                    >
                      拒绝
                    </el-button>
                    <el-button
                      v-if="row.status === 'confirmed'"
                      size="small"
                      type="success"
                      :loading="actingRoundId === row.id"
                      @click="executeRound(row.id)"
                    >
                      执行
                    </el-button>
                    <el-button size="small" @click="viewRoundReport(row.id)">
                      报告
                    </el-button>
                    <span v-if="row.status === 'completed' || row.status === 'failed'" class="hint">
                      {{ roundResultSummary(row) }}
                    </span>
                  </template>
                </el-table-column>
              </el-table>
            </div>
          </div>
      <!-- 轮次报告 dialog -->
      <el-dialog
        v-model="roundReportVisible"
        :title="`轮次 #${roundReport?.round.id ?? ''} 报告`"
        width="720px"
        append-to-body
      >
        <div v-loading="roundReportLoading">
          <template v-if="roundReport">
            <el-alert
              :title="roundReport.summary"
              :type="roundReport.round.status === 'failed' ? 'error' : roundReport.round.status === 'completed' ? 'success' : 'info'"
              :closable="false"
              show-icon
              style="margin-bottom: 12px"
            />
            <div style="margin-bottom: 8px; color: #909399; font-size: 12px">
              开轮原因：{{ roundReport.round.reason }}
            </div>
            <el-table :data="roundReport.actions" size="small" border style="margin-bottom: 12px">
              <el-table-column label="动作" prop="kind" width="150" />
              <el-table-column label="目标" prop="target" width="140" />
              <el-table-column label="结果摘要" min-width="260">
                <template #default="{ row }">
                  <div>{{ row.summary }}</div>
                  <div v-if="row.job" class="hint">
                    作业 #{{ row.job.id }}（{{ row.job.status }}）
                    <template v-if="row.job.result">
                      ：{{ JSON.stringify(row.job.result).slice(0, 160) }}
                    </template>
                  </div>
                </template>
              </el-table-column>
            </el-table>
            <div v-if="roundReport.evidence?.length" style="margin-bottom: 8px; color: #909399; font-size: 12px">
              证据：{{ roundReport.evidence.join('；') }}
            </div>
            <el-alert
              v-if="roundReport.eval_after"
              :title="`执行后测评：${roundReport.eval_after.arm} Fβ = ${roundReport.eval_after.f_beta}（报告 ${roundReport.eval_after.report_id}）`"
              type="success"
              :closable="false"
              show-icon
            />
            <div v-else class="hint" style="margin-top: 8px">
              执行后测评证据不可用（测评服务不可达或尚无新报告）
            </div>
          </template>
        </div>
      </el-dialog>
        </el-card>
      </el-tab-pane>
    </el-tabs>
  </div>
</template>

<style scoped>
/* 闭环导览条（T2-1） */
.evo-guide {
  display: flex;
  align-items: center;
  gap: 14px;
  margin-bottom: 10px;
}
.evo-steps { flex: 1; min-width: 320px; }
.evo-step { cursor: pointer; }
.evo-step :deep(.el-step__title) { font-size: 13px; cursor: pointer; }
.evo-guide-hint {
  font-size: 12.5px;
  color: var(--el-color-warning, #e6a23c);
  white-space: nowrap;
}
/* 测评表单：主操作行 + 高级选项折叠区（T2-2） */
.eval-actions { display: flex; gap: 8px; margin: 4px 0; }
.eval-advanced { margin-top: 6px; padding-left: 2px; }
.evolution-center {
  padding: 12px 16px;
}
.eval-grid {
  display: grid;
  grid-template-columns: 360px 1fr;
  gap: 12px;
  margin-bottom: 12px;
}
.report-card {
  margin-bottom: 12px;
}
.card-h {
  display: flex;
  align-items: center;
  gap: 8px;
}
.card-t {
  font-weight: 600;
}
.hint {
  font-size: 12px;
  color: var(--el-text-color-secondary);
}
.job-meta {
  margin: 8px 0 0;
  font-size: 13px;
}
.job-err {
  color: var(--el-color-danger);
  font-size: 13px;
  margin: 4px 0 0;
  word-break: break-all;
}
.overall-row {
  display: flex;
  flex-wrap: wrap;
  gap: 10px;
  margin-bottom: 12px;
}
.metric {
  display: flex;
  flex-direction: column;
  gap: 2px;
  padding: 8px 14px;
  border: 1px solid var(--el-border-color-lighter);
  border-radius: 6px;
  min-width: 90px;
}
.metric .k {
  font-size: 12px;
  color: var(--el-text-color-secondary);
}
.metric .v {
  font-size: 18px;
  font-weight: 700;
}
.sec-t {
  margin: 14px 0 8px;
  font-size: 14px;
}
.compare-row {
  display: flex;
  align-items: center;
  gap: 10px;
  margin-top: 12px;
}
.delta-up {
  color: var(--el-color-success);
}
.delta-down {
  color: var(--el-color-danger);
}
.findings {
  margin: 0 0 8px;
  padding-left: 20px;
  font-size: 13px;
  line-height: 1.7;
}
.findings {
  margin: 0 0 8px;
  padding-left: 20px;
  font-size: 13px;
  line-height: 1.7;
}
</style>