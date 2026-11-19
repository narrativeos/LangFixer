<script setup lang="ts">
/**
 * 分阶段检测进度（header 与工作台 main 之间）：动态流程图。
 *
 * 拓扑与引擎真实执行一致（gec-core detect_level_traced 的 tokio::join!）：
 * ①规则 / ②决策 / ③-F特征 三路并行 → ②.5知识召回（串行）→ ③-T灰区定向复查
 * → ③LLM复核；L3 特征模型另有两虚线边：部分定案留有灰区句 → 触发 ③-T；
 * 高置信定案 → LLM 跳过。步骤条式的线性顺序无法表达并行组，故弃用。
 *
 * 数据面 = store.progress（与线性版相同的事件收敛逻辑，见 lib/pipeline）；
 * 节点副行 = 贡献度：检出条数（layers[].found）· 共识（also_found_by）· 耗时；
 * 边：上游落定即点亮，上游落定且下游进行中 → 流动动画；
 * 缓存 / 历史命中无阶段事件 → 「瞬时返回」；未收到 End 即中断 → 节点转灰 ✕。
 */
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import { useGecStore } from '../stores/gec'
import type { StageView } from '../lib/pipeline'

const store = useGecStore()

const p = computed(() => store.progress)
const running = computed(() => p.value.active)
const visible = computed(() => p.value.active || p.value.finished)

/** 100ms 心跳仅在检测进行中运行：驱动「已用」重绘 */
const now = ref(Date.now())
let ticker: number | undefined
function setTicker(on: boolean) {
  if (on && ticker === undefined) {
    now.value = Date.now()
    ticker = window.setInterval(() => {
      now.value = Date.now()
    }, 100)
  } else if (!on && ticker !== undefined) {
    clearInterval(ticker)
    ticker = undefined
  }
}
watch(running, setTicker, { immediate: true })

onBeforeUnmount(() => {
  setTicker(false)
})

const isSettled = (s: StageView) =>
  s.status === 'done' ||
  s.status === 'skipped' ||
  s.status === 'unavailable' ||
  s.status === 'canceled'

/** 各层检出条数（LayerReport.found）；随 detect 响应一次性到达 */
const foundMap = computed(() => {
  const m: Record<string, number> = {}
  for (const l of store.layers ?? []) m[l.layer] = l.found ?? 0
  return m
})

/** 该层的共识命中：最终错误中本层参与共识的条数——无论是产出方
 * （source = 本层且 also_found_by 非空）还是被合并后记名方 */
const alsoMap = computed(() => {
  const m: Record<string, number> = {}
  for (const e of store.errors) {
    const also = e.also_found_by ?? []
    if (!also.length) continue
    const layers = new Set(also)
    if (e.source) layers.add(e.source)
    for (const l of layers) m[l] = (m[l] ?? 0) + 1
  }
  return m
})

/** 特征模型高置信定案标记（layers 报告 status=decided） */
const featureDecided = computed(() =>
  (store.layers ?? []).some((l) => l.layer === 'feature' && l.status === 'decided'),
)

/** 该层本次检出条数（LayerReport.found）；③-T 的结果层归因记在 llm 名下
 * （DETECT_PIPELINE §5：layers 不含 targeted），故重定向读取。
 * skipped/canceled 节点显示的是另一条路径的检出数，不挂命中数 */
function foundOf(s: StageView): number | null {
  if (!isSettled(s) || s.status === 'skipped' || s.status === 'canceled') return null
  const n = s.key === 'targeted' ? foundMap.value.llm : foundMap.value[s.key]
  return n != null && n > 0 ? n : null
}

/** 共识命中数（仅落定步骤；③-T 同样经 llm 归因；skipped/canceled 不挂） */
function alsoOf(s: StageView): number {
  if (!isSettled(s) || s.status === 'skipped' || s.status === 'canceled') return 0
  return alsoMap.value[s.key === 'targeted' ? 'llm' : s.key] ?? 0
}

const stateWord: Record<StageView['status'], string> = {
  pending: '等待',
  running: '进行中',
  done: '完成',
  unavailable: '不可用',
  skipped: '已跳过',
  canceled: '已中断',
}

function icon(s: StageView): string {
  switch (s.status) {
    case 'done':
      return '✓'
    case 'unavailable':
    case 'canceled':
      return '✕'
    case 'skipped':
      return '–'
    default:
      return ''
  }
}

function subText(s: StageView): string {
  if (s.status === 'running') {
    // 段级预算块进度（长文本 LLM 分块 / stat 滑窗）：已复查块数 / 总块数
    if (s.chunks && s.chunks.total > 1)
      return `段 ${s.chunks.done}/${s.chunks.total}`
    return '进行中…'
  }
  if (s.status === 'pending') return '等待'
  const parts: string[] = []
  if (s.key === 'feature' && s.status === 'done' && featureDecided.value) parts.push('定案')
  const n = foundOf(s)
  if (n != null) parts.push(`${n}条`)
  const also = alsoOf(s)
  if (also) parts.push(`共识 ${also}`)
  if (s.status === 'skipped' || s.status === 'unavailable' || s.status === 'canceled')
    return stateWord[s.status]
  if (s.ms != null) parts.push(`${s.ms}ms`)
  return parts.join(' · ') || stateWord[s.status]
}

function tooltip(s: StageView): string {
  let extra = ''
  const n = foundOf(s)
  const also = alsoOf(s)
  if (n != null) extra = `，本次命中 ${n} 条`
  if (also) extra += `（其中 ${also} 条与其他层共识）`
  return `${s.no} ${s.label}：${stateWord[s.status]}${extra}`
}

/* ---------- 布局：并行组竖排 → 串行链横排 ---------- */

const PARALLEL_KEYS = ['rule', 'decision', 'feature']
const NODE_W = 142
const NODE_H = 42
const LANE_GAP = 16
const TOP = 12
const LEFT = 10
const CGAP = 52

interface FlowNode {
  s: StageView
  x: number
  y: number
  cx: number
  cy: number
}
interface FlowEdge {
  key: string
  d: string
  dashed: boolean
  lit: boolean
  flow: boolean
}

const layout = computed(() => {
  const stages = p.value.stages
  if (!stages.length) return null
  const parallel = stages.filter((s) => PARALLEL_KEYS.includes(s.key))
  const serial = stages.filter((s) => !PARALLEL_KEYS.includes(s.key))
  const lanes = Math.max(parallel.length, 1)
  const stackH = lanes * (NODE_H + LANE_GAP) - LANE_GAP
  const nodes: FlowNode[] = []
  parallel.forEach((s, i) => {
    const y = TOP + i * (NODE_H + LANE_GAP)
    nodes.push({ s, x: LEFT, y, cx: LEFT + NODE_W / 2, cy: y + NODE_H / 2 })
  })
  const serialY = TOP + (stackH - NODE_H) / 2
  serial.forEach((s, i) => {
    const x = LEFT + NODE_W + CGAP + i * (NODE_W + CGAP)
    nodes.push({ s, x, y: serialY, cx: x + NODE_W / 2, cy: serialY + NODE_H / 2 })
  })
  const w = LEFT + NODE_W + (serial.length ? serial.length * (NODE_W + CGAP) : 0) + LEFT
  const h = TOP + Math.max(stackH, serial.length ? NODE_H : 0) + TOP
  const byKey: Record<string, FlowNode> = {}
  for (const n of nodes) byKey[n.s.key] = n
  const edges: FlowEdge[] = []
  const first = serial[0] ? byKey[serial[0].key] : null
  for (const n of nodes.filter((n) => PARALLEL_KEYS.includes(n.s.key))) {
    if (!first) break
    edges.push({
      key: `p-${n.s.key}`,
      d: `M ${n.x + NODE_W} ${n.cy} C ${n.x + NODE_W + 24} ${n.cy}, ${first.x - 16} ${first.cy}, ${first.x} ${first.cy}`,
      dashed: false,
      lit: isSettled(n.s),
      flow: isSettled(n.s) && first.s.status === 'running',
    })
  }
  for (let i = 0; i + 1 < serial.length; i++) {
    const a = byKey[serial[i].key]
    const b = byKey[serial[i + 1].key]
    edges.push({
      key: `s-${serial[i].key}`,
      d: `M ${a.x + NODE_W} ${a.cy} L ${b.x} ${b.cy}`,
      dashed: false,
      lit: isSettled(a.s),
      flow: isSettled(a.s) && b.s.status === 'running',
    })
  }
  // L3 高置信定案门：特征模型 → LLM（虚线；定案时 LLM 跳过）
  if (byKey['feature'] && byKey['llm']) {
    const f = byKey['feature']
    const l = byKey['llm']
    edges.push({
      key: 'gate',
      d: `M ${f.x + NODE_W} ${f.cy} C ${f.x + NODE_W + 40} ${f.cy}, ${l.cx} ${f.cy + 8}, ${l.cx} ${l.y + NODE_H}`,
      dashed: true,
      lit: isSettled(f.s),
      flow: false,
    })
  }
  // L3 部分定案灰区门：特征模型 → ③-T（虚线；留有灰区句时触发定向复查）
  if (byKey['feature'] && byKey['targeted']) {
    const f = byKey['feature']
    const t = byKey['targeted']
    edges.push({
      key: 'gate-t',
      d: `M ${f.x + NODE_W} ${f.cy} C ${f.x + NODE_W + 40} ${f.cy}, ${t.cx} ${f.cy + 8}, ${t.cx} ${t.y + NODE_H}`,
      dashed: true,
      lit: isSettled(f.s),
      flow: false,
    })
  }
  return { w, h, nodes, edges, byKey }
})

/** 门标签位置：贴在目标节点上方（避免两门标签互相碰撞） */
const gateLabels = computed(() => {
  const L = layout.value
  const f = L?.byKey['feature']
  if (!f || !L) return []
  const out: { x: number; y: number; text: string }[] = []
  for (const [key, text] of [
    ['targeted', '灰区门'],
    ['llm', '定案门'],
  ] as const) {
    const n = L.byKey[key]
    if (n) out.push({ x: n.cx, y: Math.max(9, n.y - 4), text })
  }
  return out
})

const settledCount = computed(() => p.value.stages.filter(isSettled).length)

/** 右侧耗时读数：进行中实时已用；结束后总量（中断按已用时收尾） */
const totalText = computed(() => {
  const q = p.value
  if (q.instant) return ''
  if (!q.active) return `${q.canceled ? '已中断' : '共'} ${q.totalMs ?? 0}ms`
  return `已用 ${Math.max(0, now.value - q.startedAt)}ms`
})

const stateText = computed(() => {
  const q = p.value
  if (q.instant) return '瞬时返回（缓存 / 历史）'
  if (q.canceled) return '检测已中断'
  if (!q.active) return '检测完成'
  if (!q.stages.length) return '检测启动中…'
  return ''
})
</script>

<template>
  <div
    v-if="visible"
    class="flw"
    :class="{ 'flw-running': running, 'flw-finished': !running && !p.canceled, 'flw-canceled': p.canceled }"
    role="status"
    :aria-label="`检测进度：${settledCount}/${p.stages.length}`"
  >
    <svg v-if="layout" class="flw-svg" :viewBox="`0 0 ${layout.w} ${layout.h}`" aria-hidden="true">
      <g>
        <path
          v-for="e in layout.edges"
          :key="e.key"
          :d="e.d"
          fill="none"
          class="flw-edge"
          :class="{ on: e.lit, flow: e.flow, dashed: e.dashed }"
        />
      </g>
      <g v-for="n in layout.nodes" :key="n.s.key" class="flw-node" :class="'st-' + n.s.status" :transform="`translate(${n.x} ${n.y})`">
        <title>{{ tooltip(n.s) }}</title>
        <rect :width="142" :height="42" rx="9" />
        <text class="l1" x="11" y="17">{{ n.s.no }} {{ n.s.label }}</text>
        <text class="l2" x="11" y="33">{{ subText(n.s) }}</text>
        <text v-if="icon(n.s)" class="lchk" x="131" y="18" text-anchor="end">{{ icon(n.s) }}</text>
        <circle v-else-if="n.s.status === 'running'" class="lspin" cx="128" cy="13" r="5.5" />
      </g>
      <text v-for="g in gateLabels" :key="g.text" class="flw-gate-label" :x="g.x" :y="g.y" text-anchor="middle">{{ g.text }}</text>
    </svg>
    <div class="flw-foot">
      <span v-if="stateText" class="flw-note">{{ stateText }}</span>
      <span class="flw-total">{{ totalText }}</span>
    </div>
  </div>
</template>

<style scoped>
.flw {
  display: flex;
  flex-direction: column;
  gap: 2px;
  padding: 7px var(--space-3) 5px;
  border-bottom: 1px solid var(--border);
  background: var(--surface-2);
  color: var(--text-2);
}
.flw-svg {
  width: min(100%, 660px);
  height: auto;
}
.flw-edge {
  stroke: var(--muted);
  stroke-width: 1.6;
  opacity: 0.55;
  fill: none;
}
.flw-edge.on {
  stroke: var(--ok);
  opacity: 1;
}
.flw-edge.dashed {
  stroke-dasharray: 4 4;
}
.flw-edge.flow {
  stroke: var(--primary);
  stroke-dasharray: 5 5;
  opacity: 1;
  animation: flw-dash 0.9s linear infinite;
}
@keyframes flw-dash {
  to {
    stroke-dashoffset: -10;
  }
}
.flw-node rect {
  fill: var(--bg);
  stroke: var(--border);
  stroke-width: 1.5;
}
.flw-node .l1 {
  font-size: 12.5px;
  font-weight: 600;
  fill: var(--text);
}
.flw-node .l2 {
  font-size: 10.5px;
  fill: var(--muted);
}
.flw-node .lchk {
  font-size: 12px;
  font-weight: 700;
}
.flw-node.st-done rect {
  stroke: var(--ok);
}
.flw-node.st-done .lchk {
  fill: var(--ok);
}
.flw-node.st-running rect {
  stroke: var(--primary);
  animation: flw-pulse 1.4s ease-in-out infinite alternate;
}
.flw-node.st-running .lchk,
.flw-node.st-running .l2 {
  fill: var(--primary);
}
@keyframes flw-pulse {
  from {
    stroke-opacity: 1;
  }
  to {
    stroke-opacity: 0.45;
  }
}
.flw-node .lspin {
  fill: none;
  stroke: var(--primary);
  stroke-width: 2;
  stroke-dasharray: 10 25;
  stroke-linecap: round;
  transform-box: fill-box;
  transform-origin: center;
  animation: flw-rot 0.9s linear infinite;
}
@keyframes flw-rot {
  to {
    transform: rotate(360deg);
  }
}
.flw-node.st-pending {
  opacity: 0.55;
}
.flw-node.st-unavailable rect,
.flw-node.st-canceled rect {
  stroke: var(--danger, #c0392b);
}
.flw-node.st-unavailable .lchk,
.flw-node.st-canceled .lchk {
  fill: var(--danger, #c0392b);
}
.flw-node.st-canceled {
  opacity: 0.6;
}
.flw-node.st-skipped rect {
  stroke-dasharray: 3 3;
}
.flw-node.st-skipped .lchk {
  fill: var(--muted);
}
.flw-gate-label {
  font-size: 9.5px;
  fill: var(--muted);
}
.flw-foot {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: var(--space-3);
  font-size: var(--fs-sm);
  color: var(--text-2);
}
.flw-total {
  color: var(--muted);
}
</style>
