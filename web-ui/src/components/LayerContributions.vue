<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { api } from '../api/client'
import { errMsg } from '../lib/err'
import type { EvalLayerStats, EvalReport } from '../types'

/**
 * 各层贡献度报告（质量透明面）：直读评测报告档案（GET /gec/eval/layers），
 * 不依赖 Python 测评服务——生产与评审通道都能展示「质量来自哪一层」。
 * 与进化中心「测评」页的区别：那里是运营侧发起评测 + 看报告全文；
 * 这里只做只读陈列（报告选择 + 两报告对比），评审密钥可达。
 */

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

/** 层 → 报告 config 开关键（规则/统计无开关恒开） */
const LAYER_CFG_KEY: Record<string, 'decision_enabled' | 'knowledge_enabled' | 'l3_enabled' | 'llm_enabled'> = {
  decision: 'decision_enabled',
  knowledge: 'knowledge_enabled',
  feature: 'l3_enabled',
  llm: 'llm_enabled',
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
  state: { text: string; type: 'success' | 'info' | 'warning' }
  share: number
  dShare: number | null
  dF: number | null
}

const rows = computed<LayerRow[]>(() => {
  const sel = selected.value
  if (!sel) return []
  const known = LAYERS.filter((l) => sel.layers[l.key])
  const extra = Object.keys(sel.layers)
    .filter((k) => !LAYERS.some((l) => l.key === k))
    .map((k) => ({ key: k, label: k }))
  return [...known, ...extra].map(({ key, label }) => {
    const s = sel.layers[key]
    const cfgKey = LAYER_CFG_KEY[key]
    let state: LayerRow['state']
    if (cfgKey && sel.config?.[cfgKey] === false)
      state = { text: key === 'decision' ? '未启用（待发布）' : '未启用', type: 'info' }
    else if (s.attributed_gold > 0) state = { text: '有自定案', type: 'success' }
    else if (s.n_pred > 0) state = { text: '有预测零命中', type: 'warning' }
    else state = { text: '零预测', type: 'info' }
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

/** 报告 config 开关 chips（一眼看清这轮评测哪几层在跑） */
const cfgChips = computed(() => {
  const c = selected.value?.config
  if (!c) return []
  return [
    { k: 'LLM', on: c.llm_enabled !== false, extra: c.llm_model },
    { k: '特征模型', on: c.l3_enabled !== false, extra: undefined },
    { k: '知识召回', on: c.knowledge_enabled !== false, extra: c.embedding_configured === false ? '未配嵌入' : undefined },
    { k: '决策模型', on: c.decision_enabled === true, extra: c.decision_enabled === true ? undefined : '待发布' },
  ]
})

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

onMounted(load)
</script>

<template>
  <div class="layers-report">
    <el-alert v-if="error" type="error" :title="error" show-icon :closable="false" />

    <el-card shadow="never" class="pick-card">
      <div class="pick-row">
        <div class="pick-field">
          <span class="k">报告</span>
          <el-select v-model="selectedId" style="width: 380px" :loading="loading">
            <el-option
              v-for="r in reports"
              :key="r.id"
              :label="reportLabel(r)"
              :value="r.id"
            />
          </el-select>
        </div>
        <div class="pick-field">
          <span class="k">对比</span>
          <el-select v-model="compareId" clearable placeholder="选一份报告做差值" style="width: 380px">
            <el-option
              v-for="r in reports"
              :key="r.id"
              :disabled="r.id === selected?.id"
              :label="reportLabel(r)"
              :value="r.id"
            />
          </el-select>
        </div>
        <span class="hint">报告来自评测档案 {{ dir }}（{{ reports.length }} 份{{ skipped ? `，跳过 ${skipped}` : '' }}）</span>
      </div>
    </el-card>

    <el-empty v-if="!loading && !error && !selected" description="还没有评测报告：档案目录为空（评测任务落盘后可见）" />

    <template v-if="selected">
      <!-- 报告基本盘 -->
      <el-card shadow="never" class="meta-card">
        <el-descriptions :column="4" size="small" border>
          <el-descriptions-item label="基准">{{ selected.benchmark }}{{ selected.split ? ' · ' + selected.split : '' }}</el-descriptions-item>
          <el-descriptions-item label="臂 / 模式">{{ selected.arm }} / {{ selected.mode }}</el-descriptions-item>
          <el-descriptions-item label="语言 / β">{{ selected.lang }} / {{ selected.beta }}</el-descriptions-item>
          <el-descriptions-item label="时间">{{ fmtTs(selected.ts) }}</el-descriptions-item>
          <el-descriptions-item label="句数">{{ selected.sentences.ok }}/{{ selected.sentences.total }}（失败 {{ selected.sentences.failed }}）</el-descriptions-item>
          <el-descriptions-item label="金标句">{{ selected.n_gold_sentences }}</el-descriptions-item>
          <el-descriptions-item label="金标编辑">{{ selected.n_gold_edits }}</el-descriptions-item>
          <el-descriptions-item label="消融">
            <span v-if="!selected.ablation">无（当前能力）</span>
            <span v-else>{{ JSON.stringify(selected.ablation) }}</span>
          </el-descriptions-item>
        </el-descriptions>
        <div class="cfg-row">
          <el-tag
            v-for="c in cfgChips"
            :key="c.k"
            :type="c.on ? 'success' : 'info'"
            size="small"
            :title="c.on ? '该层已启用' : '该层未启用'"
          >
            {{ c.k }} {{ c.on ? '开' : '关' }}<template v-if="c.extra"> · {{ c.extra }}</template>
          </el-tag>
        </div>
      </el-card>

      <!-- 总体指标 -->
      <el-card shadow="never">
        <template #header><span class="card-t">总体</span></template>
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
      </el-card>

      <!-- 层级贡献 -->
      <el-card shadow="never">
        <template #header>
          <div class="card-h">
            <span class="card-t">各层贡献</span>
            <span class="hint">召回份额 = 该层自定案命中的金标编辑 ÷ 金标编辑全集；对比列是相对所选报告的差</span>
          </div>
        </template>
        <el-table :data="rows" size="small" border>
          <el-table-column label="层" prop="label" min-width="150" />
          <el-table-column label="状态" width="130">
            <template #default="{ row }">
              <el-tag :type="row.state.type" size="small">{{ row.state.text }}</el-tag>
            </template>
          </el-table-column>
          <el-table-column label="预测" prop="stats.n_pred" width="70" align="right" />
          <el-table-column label="P（严格）" width="90" align="right">
            <template #default="{ row }">{{ pct(row.stats.p_strict) }}</template>
          </el-table-column>
          <el-table-column label="R（严格）" width="90" align="right">
            <template #default="{ row }">{{ pct(row.stats.r_strict) }}</template>
          </el-table-column>
          <el-table-column label="Fβ" width="80" align="right">
            <template #default="{ row }"><b>{{ row.stats.f_beta.toFixed(4) }}</b></template>
          </el-table-column>
          <el-table-column label="自定案金标" prop="stats.attributed_gold" width="100" align="right" />
          <el-table-column label="召回份额" min-width="150">
            <template #default="{ row }">
              <div class="share-cell">
                <el-progress
                  :percentage="Math.round(row.share * 1000) / 10"
                  :stroke-width="8"
                  :show-text="false"
                />
                <span class="share-t">{{ pct(row.share) }}</span>
              </div>
            </template>
          </el-table-column>
          <el-table-column v-if="compare" label="Δ份额" width="90" align="right">
            <template #default="{ row }">{{ row.dShare == null ? '—' : pp(row.dShare) }}</template>
          </el-table-column>
          <el-table-column v-if="compare" label="ΔFβ" width="90" align="right">
            <template #default="{ row }">{{ row.dF == null ? '—' : row.dF.toFixed(4) }}</template>
          </el-table-column>
        </el-table>
      </el-card>

      <!-- 级联漏斗：前缀累计解决率 -->
      <el-card v-if="funnelCumulative.length" shadow="never">
        <template #header>
          <div class="card-h">
            <span class="card-t">级联漏斗</span>
            <span class="hint">按层前缀累计解决的金标比例（LLM 触达率 {{ pct(selected.funnel?.llm_reach) }}）</span>
          </div>
        </template>
        <el-table :data="funnelCumulative" size="small" border>
          <el-table-column label="层前缀" prop="prefix" min-width="220" />
          <el-table-column label="解决金标数" prop="resolved" width="110" align="right" />
          <el-table-column label="累计解决率" width="110" align="right">
            <template #default="{ row }">{{ pct(row.rate) }}</template>
          </el-table-column>
        </el-table>
      </el-card>
    </template>
  </div>
</template>

<style scoped>
.layers-report {
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.pick-row {
  display: flex;
  align-items: center;
  gap: 16px;
  flex-wrap: wrap;
}
.pick-field {
  display: flex;
  align-items: center;
  gap: 8px;
}
.k {
  color: var(--el-text-color-secondary);
  font-size: 13px;
}
.hint {
  color: var(--el-text-color-secondary);
  font-size: 12px;
}
.card-h {
  display: flex;
  align-items: baseline;
  gap: 12px;
}
.card-t {
  font-weight: 600;
}
.cfg-row {
  margin-top: 10px;
  display: flex;
  gap: 8px;
  flex-wrap: wrap;
}
.metric-row {
  display: flex;
  gap: 28px;
  flex-wrap: wrap;
}
.metric {
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.metric .v {
  font-size: 18px;
  font-weight: 600;
}
.metric .d {
  font-size: 12px;
  color: var(--el-color-primary);
}
.share-cell {
  display: flex;
  align-items: center;
  gap: 8px;
}
.share-cell :deep(.el-progress) {
  flex: 1;
  min-width: 60px;
}
.share-t {
  font-size: 12px;
  min-width: 48px;
  text-align: right;
}
</style>
