<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { api } from '../api/client'
import { errMsg } from '../lib/err'
import { useGecStore } from '../stores/gec'
import type { EvalLayerStats, EvalReport } from '../types'
import { LButton, LCheckbox, LEmpty, LProgress, LSection, LSelect, LTable, LTag } from '../ui'
import type { LColumn } from '../ui'

/**
 * 各层贡献度报告（质量透明面）：直读评测报告档案（GET /gec/eval/layers），
 * 不依赖 Python 测评服务——生产与评审通道都能展示「质量来自哪一层」。
 * 与进化中心「测评」页的区别：那里是运营侧发起评测 + 看报告全文；
 * 这里只做只读陈列（报告选择 + 两报告对比），评审密钥可达。
 */

const store = useGecStore()

const reports = ref<EvalReport[]>([])
const dir = ref('')
const skipped = ref(0)
const loading = ref(false)
const error = ref('')
const selectedId = ref('')
const compareId = ref('')

/** 层展示顺序与管线口径一致（①规则 → ②决策 → ②.5知识 → ③-F特征 → ③.5统计 → ③ LLM） */
const LAYERS: { key: string; label: string }[] = [
  { key: 'rule', label: '① 规则引擎' },
  { key: 'decision', label: '② 决策模型' },
  { key: 'knowledge', label: '②.5 知识召回' },
  { key: 'feature', label: '③-F 快速定案（特征模型）' },
  { key: 'statistical', label: '③.5 统计模型' },
  { key: 'llm', label: '③ LLM 兜底' },
]

/** 层 → 消融字段键（消融报告里该层当时被临时关闭；规则/统计无消融开关） */
const LAYER_ABLATION_KEY: Record<string, 'decision' | 'knowledge' | 'l3'> = {
  decision: 'decision',
  knowledge: 'knowledge',
  feature: 'l3',
}

function pct(x: number | null | undefined): string {
  return x == null ? '—' : `${(x * 100).toFixed(1)}%`
}
function pp(x: number): string {
  const v = x * 100
  return `${v > 0 ? '+' : ''}${v.toFixed(1)}pp`
}
function fmtTs(ts: string): string {
  return ts.replace('T', ' ').slice(0, 16)
}

const selected = computed<EvalReport | null>(
  () => reports.value.find((r) => r.id === selectedId.value) ?? reports.value[0] ?? null,
)
const compare = computed<EvalReport | null>(
  () => (compareId.value ? reports.value.find((r) => r.id === compareId.value) ?? null : null),
)

function reportLabel(r: EvalReport): string {
  const abl =
    r.mode === 'ablation' && r.ablation
      ? '·消融' +
        Object.entries(r.ablation)
          .filter(([, v]) => v === false)
          .map(([k]) => k)
          .join('+')
      : ''
  return `${fmtTs(r.ts)} · ${r.benchmark}${r.split ? '·' + r.split : ''} · ${r.arm}/${r.mode}${abl}`
}

interface LayerRow {
  key: string
  label: string
  stats: EvalLayerStats
  state: { text: string; tone: 'ok' | 'neutral' | 'warn' }
  share: number
  dShare: number | null
  dF: number | null
}

type EvalFunnelStep = EvalReport['funnel']['cumulative'][number]

const asStr = (v: string | number): string => String(v)

const reportOptions = computed(() => reports.value.map((r) => ({ value: r.id, label: reportLabel(r) })))
const compareOptions = computed(() => [
  { value: '', label: '（不对比）' },
  ...reports.value.filter((r) => r.id !== selected.value?.id).map((r) => ({ value: r.id, label: reportLabel(r) })),
])

const layerCols = computed<LColumn<LayerRow>[]>(() => {
  const cols: LColumn<LayerRow>[] = [
    { key: 'label', label: '层', width: '150px' },
    { key: 'state', label: '状态', width: '130px' },
    { key: 'nPred', label: '预测', width: '70px', align: 'right' },
    { key: 'pStrict', label: 'P（严格）', width: '90px', align: 'right' },
    { key: 'rStrict', label: 'R（严格）', width: '90px', align: 'right' },
    { key: 'fBeta', label: 'Fβ', width: '80px', align: 'right' },
    { key: 'gold', label: '自定案金标', width: '100px', align: 'right' },
    { key: 'share', label: '召回份额', width: '170px' },
  ]
  if (compare.value) {
    cols.push({ key: 'dShare', label: 'Δ份额', width: '90px', align: 'right' })
    cols.push({ key: 'dF', label: 'ΔFβ', width: '90px', align: 'right' })
  }
  return cols
})

/** 忽略无产出层：零预测且零自定案的层默认收起（决策待发布/统计未接入时全是噪声行）；
 *  关掉开关可展开核对。消融报告里被显式关闭的层不受此开关影响（状态列有说明，保留） */
const hideEmpty = ref(true)
const emptyCount = computed(() => rows.value.filter((r) => isEmptyLayer(r)).length)
function isEmptyLayer(r: LayerRow): boolean {
  return r.state.text === '零预测'
}
const visibleRows = computed(() => (hideEmpty.value ? rows.value.filter((r) => !isEmptyLayer(r)) : rows.value))

const funnelCols: LColumn<EvalFunnelStep>[] = [
  { key: 'prefix', label: '层前缀' },
  { key: 'resolved', label: '解决金标数', width: '110px', align: 'right' },
  { key: 'rate', label: '累计解决率', width: '110px', align: 'right' },
]

const rows = computed<LayerRow[]>(() => {
  const sel = selected.value
  if (!sel) return []
  const known = LAYERS.filter((l) => sel.layers[l.key])
  const extra = Object.keys(sel.layers)
    .filter((k) => !LAYERS.some((l) => l.key === k))
    .map((k) => ({ key: k, label: k }))
  const abl = sel.ablation ?? null
  return [...known, ...extra].map(({ key, label }) => {
    const s = sel.layers[key]
    const ablKey = LAYER_ABLATION_KEY[key]
    let state: LayerRow['state']
    // 服务状态不在这里判定（统一读下方实时 chips）；行状态只说这份报告里该层的表现：
    // 消融关掉的层由 ablation 字段说明，其余按数据（当时该层确实零预测就是零预测）
    if (abl && ablKey && abl[ablKey] === false) state = { text: '消融关闭', tone: 'neutral' }
    else if (s.attributed_gold > 0) state = { text: '有自定案', tone: 'ok' }
    else if (s.n_pred > 0) state = { text: '有预测零命中', tone: 'warn' }
    else state = { text: '零预测', tone: 'neutral' }
    const c = compare.value?.layers[key]
    return {
      key,
      label,
      stats: s,
      state,
      share: s.recall_share,
      dShare: c ? s.recall_share - c.recall_share : null,
      dF: c ? s.f_beta - c.f_beta : null,
    }
  })
})

/**
 * 服务状态 chips：读全局实时设置（store，与检测页「标准检查」/PipelineStatus 同口径），
 * 不再读报告内嵌 config 快照——历史快照只反映当时，会让本页与全站状态打架。
 * 「这份报告当时怎么跑的」由「消融」字段 + 各层数据表达；LLM 模型名是报告属性，保留。
 */
const svcChips = computed(() => [
  { k: 'LLM', on: store.llmEnabled, extra: selected.value?.config?.llm_model },
  { k: '特征模型', on: store.l3Enabled, extra: undefined },
  {
    k: '知识召回',
    on: store.embeddingConfigured,
    extra: store.embeddingConfigured
      ? `中文 ${store.knowledgeCounts.zh} · 英文 ${store.knowledgeCounts.en}`
      : undefined,
  },
  { k: '知识短路门', on: store.knowledgeLlmGate, extra: undefined },
  { k: '决策模型', on: store.decisionEnabled, extra: undefined },
])

const funnelCumulative = computed(() => selected.value?.funnel?.cumulative ?? [])

function overallDelta(field: 'f_beta' | 'p_strict' | 'r_strict'): string | null {
  const a = selected.value?.overall[field]
  const b = compare.value?.overall[field]
  return a != null && b != null ? pp(a - b) : null
}

async function load() {
  loading.value = true
  error.value = ''
  try {
    const r = await api.getEvalLayers()
    reports.value = r.reports
    dir.value = r.dir
    skipped.value = r.skipped
    if (!selectedId.value && r.reports[0]) selectedId.value = r.reports[0].id
  } catch (e) {
    error.value = errMsg(e)
  } finally {
    loading.value = false
  }
}

/** 服务状态 chips 的同步口径提示：chips 读 store 实时 flags，拉取失败时不能默默拿占位值冒充实时 */
const svcSyncNote = computed(() =>
  !store.engineStatusKnown
    ? '状态未同步：以下为占位值，可能与服务端不一致'
    : store.engineStatusOutdated
      ? '上次同步失败：以下为上次已知值'
      : '实时全局设置，与检测页一致；该报告当时各层表现见「消融」与层级数据',
)

function retrySync() {
  void store.refreshEngineStatus()
}

onMounted(() => {
  void load()
  // 直接进报告页不经过检测页的刷新钩子：打开时对齐一次实时服务状态
  void store.refreshEngineStatus()
})
</script>

<template>
  <div class="layers-report">
    <LSection title="报告" :desc="`报告来自评测档案 ${dir}（${reports.length} 份${skipped ? `，跳过 ${skipped}` : ''}）`">
      <div class="pick-row">
        <div class="pick-field">
          <span class="k">报告</span>
          <LSelect
            class="pick-select"
            :model-value="selectedId"
            :options="reportOptions"
            @update:model-value="selectedId = asStr($event)"
          />
        </div>
        <div class="pick-field">
          <span class="k">对比</span>
          <LSelect
            class="pick-select"
            :model-value="compareId"
            :options="compareOptions"
            @update:model-value="compareId = asStr($event)"
          />
        </div>
      </div>
    </LSection>

    <LEmpty v-if="error" text="评测档案加载失败" :hint="error">
      <template #action>
        <LButton size="sm" @click="load">重试</LButton>
      </template>
    </LEmpty>
    <LEmpty
      v-else-if="!loading && !selected"
      text="还没有评测报告：档案目录为空"
      hint="评测任务落盘后可见；可在「进化中心 · 测评」发起一次评测"
    />

    <template v-if="selected">
      <!-- 报告基本盘 -->
      <LSection title="基本信息">
        <dl class="meta-grid">
          <div class="meta-item">
            <dt>基准</dt>
            <dd>{{ selected.benchmark }}{{ selected.split ? ' · ' + selected.split : '' }}</dd>
          </div>
          <div class="meta-item">
            <dt>臂 / 模式</dt>
            <dd>{{ selected.arm }} / {{ selected.mode }}</dd>
          </div>
          <div class="meta-item">
            <dt>语言 / β</dt>
            <dd>{{ selected.lang }} / {{ selected.beta }}</dd>
          </div>
          <div class="meta-item">
            <dt>时间</dt>
            <dd>{{ fmtTs(selected.ts) }}</dd>
          </div>
          <div class="meta-item">
            <dt>句数</dt>
            <dd>{{ selected.sentences.ok }}/{{ selected.sentences.total }}（失败 {{ selected.sentences.failed }}）</dd>
          </div>
          <div class="meta-item">
            <dt>金标句</dt>
            <dd>{{ selected.n_gold_sentences }}</dd>
          </div>
          <div class="meta-item">
            <dt>金标编辑</dt>
            <dd>{{ selected.n_gold_edits }}</dd>
          </div>
          <div class="meta-item">
            <dt>消融</dt>
            <dd>
              <span v-if="!selected.ablation">无（当前能力）</span>
              <span v-else>{{ JSON.stringify(selected.ablation) }}</span>
            </dd>
          </div>
        </dl>
        <div class="cfg-row">
          <span class="k">服务状态</span>
          <LTag v-for="c in svcChips" :key="c.k">
            {{ c.k }} {{ c.on ? '开' : '关' }}<template v-if="c.extra"> · {{ c.extra }}</template>
          </LTag>
          <span class="cfg-note">{{ svcSyncNote }}</span>
          <LButton v-if="!store.engineStatusKnown || store.engineStatusOutdated" size="sm" @click="retrySync">
            重试同步
          </LButton>
        </div>
      </LSection>

      <!-- 总体指标 -->
      <LSection title="总体">
        <div class="metric-row">
          <div class="metric">
            <span class="k">Fβ</span>
            <span class="v">{{ selected.overall.f_beta.toFixed(4) }}</span>
            <span v-if="overallDelta('f_beta')" class="d">{{ overallDelta('f_beta') }}</span>
          </div>
          <div class="metric">
            <span class="k">P（严格）</span>
            <span class="v">{{ pct(selected.overall.p_strict) }}</span>
            <span v-if="overallDelta('p_strict')" class="d">{{ overallDelta('p_strict') }}</span>
          </div>
          <div class="metric">
            <span class="k">R（严格）</span>
            <span class="v">{{ pct(selected.overall.r_strict) }}</span>
            <span v-if="overallDelta('r_strict')" class="d">{{ overallDelta('r_strict') }}</span>
          </div>
          <div class="metric">
            <span class="k">R（重叠）</span>
            <span class="v">{{ pct(selected.overall.r_overlap) }}</span>
          </div>
          <div class="metric">
            <span class="k">LLM 触达率</span>
            <span class="v">{{ pct(selected.funnel?.llm_reach) }}</span>
          </div>
          <div v-if="selected.funnel?.llm_calls != null" class="metric">
            <span class="k">LLM 调用</span>
            <span class="v">{{ selected.funnel.llm_calls }} 次（{{ selected.funnel.llm_calls_per_100 }} / 百句）</span>
          </div>
          <div v-if="selected.funnel?.avg_llm_ms != null" class="metric">
            <span class="k">LLM 均时</span>
            <span class="v">{{ selected.funnel.avg_llm_ms }} ms</span>
          </div>
        </div>
      </LSection>

      <!-- 层级贡献 -->
      <LSection title="各层贡献" desc="召回份额 = 该层自定案命中的金标编辑 ÷ 金标编辑全集；对比列是相对所选报告的差">
        <div v-if="emptyCount" class="hide-row">
          <LCheckbox v-model="hideEmpty" :label="`忽略无产出层（当前隐藏 ${emptyCount} 行）`" />
        </div>
        <LTable :columns="layerCols" :rows="visibleRows" row-key="key" compact>
          <template #col-state="{ row }">
            <LTag :tone="row.state.tone">{{ row.state.text }}</LTag>
          </template>
          <template #col-nPred="{ row }">{{ row.stats.n_pred }}</template>
          <template #col-pStrict="{ row }">{{ pct(row.stats.p_strict) }}</template>
          <template #col-rStrict="{ row }">{{ pct(row.stats.r_strict) }}</template>
          <template #col-fBeta="{ row }"><b>{{ row.stats.f_beta.toFixed(4) }}</b></template>
          <template #col-gold="{ row }">{{ row.stats.attributed_gold }}</template>
          <template #col-share="{ row }">
            <div class="share-cell">
              <LProgress :value="row.share" />
              <span class="share-t">{{ pct(row.share) }}</span>
            </div>
          </template>
          <template #col-dShare="{ row }">{{ row.dShare == null ? '—' : pp(row.dShare) }}</template>
          <template #col-dF="{ row }">{{ row.dF == null ? '—' : row.dF.toFixed(4) }}</template>
        </LTable>
      </LSection>

      <!-- 级联漏斗：前缀累计解决率 -->
      <LSection
        v-if="funnelCumulative.length"
        title="级联漏斗"
        :desc="`按层前缀累计解决的金标比例（LLM 触达率 ${pct(selected.funnel?.llm_reach)}）`"
      >
        <LTable :columns="funnelCols" :rows="funnelCumulative" compact>
          <template #col-rate="{ row }">{{ pct(row.rate) }}</template>
        </LTable>
      </LSection>
    </template>
  </div>
</template>

<style scoped>
.layers-report {
  display: flex;
  flex-direction: column;
  gap: var(--space-3);
}
.hide-row {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  margin-bottom: var(--space-2);
  font-size: var(--fs-sm);
  color: var(--muted);
}
.pick-row {
  display: flex;
  align-items: center;
  gap: var(--space-4);
  flex-wrap: wrap;
}
.pick-field {
  display: flex;
  align-items: center;
  gap: var(--space-2);
}
.pick-select {
  width: 380px;
  max-width: 100%;
}
.k {
  color: var(--muted);
  font-size: var(--fs-ui);
}
.meta-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
  gap: var(--space-2) var(--space-4);
  margin: 0;
}
.meta-item {
  display: flex;
  gap: var(--space-2);
  font-size: var(--fs-ui);
  min-width: 0;
}
.meta-item dt {
  color: var(--muted);
  flex: none;
}
.meta-item dd {
  margin: 0;
  color: var(--text-2);
  word-break: break-all;
}
.cfg-row {
  margin-top: var(--space-3);
  display: flex;
  align-items: center;
  gap: var(--space-2);
  flex-wrap: wrap;
}
.cfg-note {
  color: var(--muted);
  font-size: var(--fs-sm);
}
.metric-row {
  display: flex;
  gap: var(--space-5);
  flex-wrap: wrap;
}
.metric {
  display: flex;
  flex-direction: column;
  gap: 2px;
  font-size: var(--fs-sm);
  color: var(--muted);
}
.metric .v {
  font-size: var(--fs-xl);
  font-weight: 600;
  color: var(--text);
}
.metric .d {
  font-size: var(--fs-sm);
  color: var(--accent);
}
.share-cell {
  display: flex;
  align-items: center;
  gap: var(--space-2);
}
.share-cell :deep(.l-progress) {
  flex: 1;
  min-width: 60px;
}
.share-t {
  font-size: var(--fs-sm);
  min-width: 48px;
  text-align: right;
}
</style>
