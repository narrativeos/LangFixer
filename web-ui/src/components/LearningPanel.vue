<script setup lang="ts">
/**
 * 练习与复习（学习扩展，Phase 2 + 跨会话闭合）
 *
 * 闭环：检测中确认并采纳的错误 → **全局错题本**（跨会话按类型聚合，带句级上下文
 * 与出现会话数）→ 间隔复习（挖空回忆 → 揭晓 → FSRS 四键自评：忘了/有点难/记住了/很轻松，
 * 5 秒可撤销）→ 掌握度（FSRS 算法调度：按记忆状态个性化递增间隔，到期 ≥30 天 = 已掌握）。
 * 记忆状态挂在错题键上：换会话、删会话都不丢学习进度。
 */
import { computed, onMounted, ref } from 'vue'
import {
  LButton,
  LEmpty,
  LInput,
  LProgress,
  LSection,
  LSegmented,
  LTag,
  toast,
} from '../ui'
import { useGecStore } from '../stores/gec'
import { api } from '../api/client'
import { errorTypeLabel } from '../lib/labels'
import type { LearningStats, Mistake, ReviewRating, SessionAnalysis } from '../types'

const store = useGecStore()

const mistakes = ref<Mistake[]>([])
const queue = ref<Mistake[]>([])
/** 统一错误画像（与审校侧习惯性错误分析同一投影，GET analysis） */
const profile = ref<SessionAnalysis | null>(null)
/** 学习统计（D1）：近 30 天采纳/复习事件流水聚合（GET stats） */
const stats = ref<LearningStats | null>(null)
/** 学习者 CEFR 自评（会话档案）：复习提示文案按水平适配（D3）；未填 = 通用文案 */
const learnerCefr = ref('')
const loading = ref(false)
const loaded = ref(false)
const errorMsg = ref('')

/** 当前复习卡：队列首条 + 是否已揭晓答案 + 提交中 */
const current = computed(() => queue.value[0] ?? null)
const revealed = ref(false)
const submitting = ref(false)

/** FSRS 四键评分（Anki 同款）：评分粒度直接反映到下次间隔 */
const RATINGS: Array<{
  key: ReviewRating
  label: string
  variant: 'default' | 'primary' | 'danger'
}> = [
  { key: 'again', label: '忘了', variant: 'danger' },
  { key: 'hard', label: '有点难', variant: 'default' },
  { key: 'good', label: '记住了', variant: 'primary' },
  { key: 'easy', label: '很轻松', variant: 'default' },
]

/** 今日复习节奏（本地计数，跨日自动重置）：进度提示 + 完成态 */
const DAILY_GOAL = 10
const doneKey = `langfixer.reviewDone.${new Date().toISOString().slice(0, 10)}`
const doneToday = ref(0)
try {
  doneToday.value = Number(localStorage.getItem(doneKey) ?? '0') || 0
} catch {
  /* localStorage 不可用（隐私模式等）：当日计数仅存内存 */
}
function bumpDone(delta: number) {
  doneToday.value = Math.max(0, doneToday.value + delta)
  try {
    localStorage.setItem(doneKey, String(doneToday.value))
  } catch {
    /* 忽略持久化失败 */
  }
}

/** 撤销窗口：提交评分后 5 秒内可撤销（后端存有复习前记忆状态快照） */
const undoTarget = ref<Mistake | null>(null)
let undoTimer: ReturnType<typeof setTimeout> | undefined

/** 错题本浏览控制：待复习/全部/已掌握 + 关键词搜索 + 类型折叠 */
const notebookTab = ref<'due' | 'all' | 'mastered'>('all')
const notebookTabOptions = computed(() => [
  { value: 'due' as const, label: `待复习 ${queue.value.length}` },
  { value: 'all' as const, label: `全部 ${mistakes.value.length}` },
  { value: 'mastered' as const, label: `已掌握 ${masteredCount.value}` },
])
const search = ref('')
const collapsed = ref<Set<string>>(new Set())
function toggleGroup(type: string) {
  const next = new Set(collapsed.value)
  if (next.has(type)) next.delete(type)
  else next.add(type)
  collapsed.value = next
}

/** 挖空句：把错误片段替换为下划线（大小写不敏感一次替换）。
 *  回退语义（防泄露答案）：original 不在句中时**不展示原句**——
 *  句面可能直接包含答案，复习有效性优先，返回空串由模板走提示文案 */
const clozeSentence = computed(() => {
  const m = current.value
  if (!m) return ''
  if (m.original && m.sentence.includes(m.original)) return m.sentence.replace(m.original, '＿＿＿')
  return ''
})

const masteredCount = computed(
  () => mistakes.value.filter((m) => m.mastery_status === 'mastered').length,
)
/** 掌握度进度（已掌握 / 全部错题） */
const masteryRatio = computed(() =>
  mistakes.value.length ? masteredCount.value / mistakes.value.length : 0,
)

/** 习惯性（主导）错误类型集合：来自统一画像，与审校侧同一判定口径 */
const dominantTypes = computed(
  () =>
    new Set(
      (profile.value?.habitual ?? []).filter((h) => h.is_dominant).map((h) => h.error_type),
    ),
)

/** 页签过滤 + 关键词搜索 + 按错误类型聚合（保持最近出现顺序） */
const dueIds = computed(() => new Set(queue.value.map((m) => m.id)))
const groups = computed(() => {
  const kw = search.value.trim().toLowerCase()
  const map = new Map<string, Mistake[]>()
  for (const m of mistakes.value) {
    if (notebookTab.value === 'due' && !dueIds.value.has(m.id)) continue
    if (notebookTab.value === 'mastered' && m.mastery_status !== 'mastered') continue
    if (
      kw &&
      ![m.original, m.correction ?? '', m.sentence].some((t) => t.toLowerCase().includes(kw))
    )
      continue
    const arr = map.get(m.error_type)
    if (arr) arr.push(m)
    else map.set(m.error_type, [m])
  }
  return [...map.entries()].map(([type, items]) => ({
    type,
    label: errorTypeLabel(type),
    total: items.reduce((s, m) => s + m.count, 0),
    habitual: dominantTypes.value.has(type),
    items,
  }))
})

function masteryTag(m: Mistake): { label: string; tone: 'ok' | 'neutral' } {
  switch (m.mastery_status) {
    case 'mastered':
      return { label: '已掌握', tone: 'ok' }
    case 'reviewing':
      return {
        label: m.next_review_at ? `复习中 · 下次 ${m.next_review_at.slice(5, 10)}` : '复习中',
        tone: 'neutral',
      }
    default:
      // 「没练过」而非「待复习」：与错题本「待复习」页签（今日到期）区分，一个概念一个名字
      return { label: '没练过', tone: 'neutral' }
  }
}

async function load() {
  loading.value = true
  errorMsg.value = ''
  try {
    const [ms, q, p, st] = await Promise.all([
      api.listGlobalMistakes(),
      api.listGlobalReviewQueue(),
      api.getGlobalAnalysis().catch(() => null),
      api.getLearningStats().catch(() => null),
    ])
    mistakes.value = ms
    queue.value = q
    profile.value = p
    stats.value = st
    revealed.value = false
    // D3：复习提示按学习者 CEFR 适配（会话档案；缺档案不影响主流程）
    try {
      const rec = await api.getSession(store.ensureSessionId())
      learnerCefr.value = (rec.meta.cefr ?? '').toUpperCase()
    } catch {
      /* 档案不可用：通用提示文案 */
    }
  } catch (e) {
    errorMsg.value = e instanceof Error ? e.message : String(e)
  } finally {
    loading.value = false
    loaded.value = true
  }
}

/** 统计图柱高（按窗口内峰值归一，0 事件不留占位） */
const statsMax = computed(() => {
  const ds = stats.value?.days ?? []
  return Math.max(1, ...ds.map((d) => Math.max(d.adopted, d.reviewed)))
})
function barH(n: number): number {
  return n > 0 ? Math.max(2, Math.round((n / statsMax.value) * 26)) : 0
}
const statsVisible = computed(
  () => !!stats.value && (stats.value.total_adopted > 0 || stats.value.total_reviewed > 0),
)
const RATING_LABELS: Record<string, string> = {
  again: '忘了',
  hard: '有点难',
  good: '记住了',
  easy: '很轻松',
}
/** D3：CEFR 适配的复习提示（A 段给更具体的抓手，C 段只留最小引导） */
const reviewHint = computed(() => {
  const c = learnerCefr.value
  if (c.startsWith('A')) return '提示：先看错误类型，再逐词找——句子里哪个词和它不一致？'
  if (c.startsWith('B')) return '先回忆：句子里错在哪、应改成什么，再揭晓对照。'
  if (c.startsWith('C')) return '直接回忆改错对，再揭晓对照。'
  return ''
})
/** 统计保鲜（非阻断）：复习/撤销后刷新事件流水视图 */
function refreshStats() {
  void api
    .getLearningStats()
    .then((s) => (stats.value = s))
    .catch(() => {})
}

async function submitReview(rating: ReviewRating) {
  const m = current.value
  if (!m) return
  submitting.value = true
  try {
    await api.reviewGlobalMistake(m.id, rating)
    bumpDone(1)
    // 撤销窗口（5s）：后端存有本次复习前的记忆状态快照
    undoTarget.value = m
    if (undoTimer) clearTimeout(undoTimer)
    undoTimer = setTimeout(() => {
      undoTarget.value = null
    }, 5000)
    queue.value = queue.value.filter((x) => x.id !== m.id)
    revealed.value = false
    // 导航徽章保鲜（非阻断）
    void store.refreshReviewDue()
    // 统计流水保鲜（非阻断）
    refreshStats()
    // 列表掌握度同步刷新（非阻断）
    try {
      mistakes.value = await api.listGlobalMistakes()
    } catch {
      /* 列表刷新失败不影响复习流 */
    }
  } catch (e) {
    toast(e instanceof Error ? e.message : String(e), 'danger')
  } finally {
    submitting.value = false
  }
}

/** 撤销最近一次复习：还原复习前记忆状态，卡片重回到期队列 */
async function undoReview() {
  const m = undoTarget.value
  if (!m) return
  undoTarget.value = null
  if (undoTimer) clearTimeout(undoTimer)
  try {
    await api.undoGlobalReview(m.id)
    bumpDone(-1)
    void store.refreshReviewDue()
    refreshStats()
    const [ms, q] = await Promise.all([api.listGlobalMistakes(), api.listGlobalReviewQueue()])
    mistakes.value = ms
    queue.value = q
    toast('已撤销：该错题重回到复习队列', 'ok')
  } catch (e) {
    toast(e instanceof Error ? e.message : String(e), 'danger')
  }
}

/** 空态文案：区分「没有错题」与「过滤后为空」 */
const emptyText = computed(() => {
  if (!loaded.value) return '加载中…'
  if (!mistakes.value.length)
    return '还没有错题——去「检测」处理文稿，采纳的纠错会自动进入错题本'
  if (search.value.trim()) return `没有匹配「${search.value.trim()}」的错题`
  if (notebookTab.value === 'due') return '今天没有到期错题——到期会自动出现在这里'
  if (notebookTab.value === 'mastered') return '还没有已掌握错题——坚持复习'
  return '没有错题'
})
const emptyHint = computed(() => {
  if (!loaded.value) return undefined
  if (!mistakes.value.length) return '在「文本输入」粘贴文稿并点「检测」，确认并采纳的纠错会自动沉淀到这里'
  if (search.value.trim()) return '清空搜索框可查看全部错题'
  if (notebookTab.value === 'due') return '切到「全部」可翻阅所有错题，先复习几条让调度器建立记忆曲线'
  if (notebookTab.value === 'mastered') return '按到期节奏复习，到期 ≥30 天的错题会自动标记为已掌握'
  return undefined
})

function goWorkspace() {
  store.requestNav('workspace')
}
function goProfile() {
  store.requestNav('settings', 'profile')
}

onMounted(load)
</script>

<template>
  <LSection class="mistake-card" title="错题本 · 跨文稿">
    <template #actions>
      <LButton size="sm" variant="ghost" :loading="loading" @click="load">刷新</LButton>
    </template>

    <p class="intro">
      你在「检测」中<b>确认并采纳</b>的纠错会自动沉淀到这里，<b>跨会话</b>按错误类型聚合——
      换文稿、删会话都不丢；复习进度（记忆状态）同样跨会话共享。
    </p>

    <div v-if="errorMsg" class="l-banner banner-error" role="alert">
      错题本加载失败：{{ errorMsg }}
      <span class="l-spacer"></span>
      <LButton size="sm" @click="load">重试</LButton>
    </div>

    <!-- 间隔复习：到期错题的练习卡（挖空回忆 → 揭晓 → FSRS 四键自评） -->
    <div v-if="current" class="review">
      <div class="review-head">
        <span class="review-title">今日复习</span>
        <LTag>{{ queue.length }} 题待练</LTag>
        <span class="pace">今日已复习 {{ doneToday }}/{{ DAILY_GOAL }}</span>
        <LProgress class="pace-bar" :value="Math.min(1, doneToday / DAILY_GOAL)" />
      </div>
      <div class="review-card">
        <div class="review-type">{{ errorTypeLabel(current.error_type) }}</div>
        <div v-if="reviewHint" class="review-hint">{{ reviewHint }}</div>
        <div class="sentence">
          {{ clozeSentence || '（该错题没有可挖空的上下文——请先回忆，再看下方改错）' }}
        </div>
        <div v-if="revealed" class="fix">
          <span class="orig">{{ current.original }}</span>
          <span v-if="current.correction" class="arrow">→</span>
          <span v-if="current.correction" class="corr">{{ current.correction }}</span>
        </div>
        <div v-if="revealed && !clozeSentence && current.sentence" class="review-context">
          原句：{{ current.sentence }}
        </div>
        <div class="review-actions">
          <template v-if="!revealed">
            <LButton size="sm" @click="revealed = true">显示答案</LButton>
          </template>
          <template v-else>
            <LButton
              v-for="r in RATINGS"
              :key="r.key"
              size="sm"
              :variant="r.variant"
              :loading="submitting"
              @click="submitReview(r.key)"
            >
              {{ r.label }}
            </LButton>
          </template>
        </div>
      </div>
    </div>
    <div v-else-if="mistakes.length && doneToday > 0" class="review done">
      🎉 今日已复习 {{ doneToday }} 题{{ doneToday >= DAILY_GOAL ? '，目标达成，明天见' : '——错题到期会自动回到队列' }}
    </div>
    <div v-if="undoTarget" class="undo-bar">
      已复习「{{ undoTarget.original }}」
      <LButton size="sm" variant="ghost" @click="undoReview">撤销（5 秒内）</LButton>
    </div>

    <div v-if="groups.length" class="summary">
      共 {{ mistakes.length }} 条错题 · 待复习 {{ queue.length }} · 已掌握 {{ masteredCount }}
      <span v-if="profile?.basis === 'confirmed'" class="basis">
        · 只统计你确认过的错误
      </span>
      <div class="mastery-row">
        <span class="pace">掌握度 {{ masteredCount }}/{{ mistakes.length }}</span>
        <LProgress class="mastery-bar" :value="masteryRatio" />
      </div>
    </div>

    <!-- 错题本浏览控制：页签过滤 + 关键词搜索 -->
    <div v-if="mistakes.length" class="notebook-controls">
      <LSegmented v-model="notebookTab" :options="notebookTabOptions" />
      <LInput v-model="search" placeholder="搜索错题内容" class="search-input" />
    </div>

    <p v-if="loading && !groups.length" class="muted-line">加载中…</p>
    <template v-else-if="!groups.length">
      <LEmpty :text="emptyText" :hint="emptyHint">
        <template v-if="!mistakes.length" #action>
          <LButton variant="primary" @click="goWorkspace">去检测</LButton>
        </template>
      </LEmpty>
      <p v-if="!mistakes.length" class="tip">
        提示：完善学习者档案（母语 + CEFR 自评）后，LLM 讲解会按你的水平自动个性化。
        <LButton size="sm" variant="ghost" @click="goProfile">完善我的档案</LButton>
      </p>
    </template>
    <template v-else>
      <div v-for="g in groups" :key="g.type" class="group">
        <div class="group-head" @click="toggleGroup(g.type)">
          <span class="group-caret">{{ collapsed.has(g.type) ? '▸' : '▾' }}</span>
          <span class="group-label">{{ g.label }}</span>
          <LTag
            v-if="g.habitual"
            tone="neutral"
            title="在你确认过的错误里，这类占比最高（跨文稿统计）"
          >习惯性</LTag>
          <LTag>{{ g.total }} 次</LTag>
        </div>
        <template v-if="!collapsed.has(g.type)">
          <div v-for="m in g.items" :key="m.id" class="mistake">
            <div class="sentence">{{ m.sentence || m.original }}</div>
            <div class="fix">
              <span class="orig">{{ m.original }}</span>
              <span v-if="m.correction" class="arrow">→</span>
              <span v-if="m.correction" class="corr">{{ m.correction }}</span>
              <LTag v-if="m.count > 1">×{{ m.count }}</LTag>
              <LTag v-if="m.sessions_seen && m.sessions_seen > 1">来自 {{ m.sessions_seen }} 个会话</LTag>
              <LTag :tone="masteryTag(m).tone">{{ masteryTag(m).label }}</LTag>
              <span class="time">{{ m.created_at.slice(0, 10) }}</span>
            </div>
          </div>
        </template>
      </div>
    </template>

    <!-- 学习统计（D1）：近 30 天逐日采纳/复习事件 + 窗口内评分分布（事件流水不随删会话清除） -->
    <div v-if="statsVisible && stats" class="stats">
      <div class="stats-head">
        <span class="stats-title">学习统计 · 近 30 天</span>
        <span class="pace">累计采纳 {{ stats.total_adopted }} · 复习 {{ stats.total_reviewed }}</span>
      </div>
      <div class="stats-bars">
        <div
          v-for="d in stats.days"
          :key="d.date"
          class="bar-col"
          :title="`${d.date} · 采纳 ${d.adopted} · 复习 ${d.reviewed}`"
        >
          <div class="bar-review" :style="{ height: barH(d.reviewed) + 'px' }"></div>
          <div class="bar-adopt" :style="{ height: barH(d.adopted) + 'px' }"></div>
        </div>
      </div>
      <div class="stats-legend">
        <span><i class="dot dot-adopt"></i>采纳</span>
        <span><i class="dot dot-review"></i>复习</span>
        <span v-for="rc in stats.rating_counts" :key="rc.rating" class="rating-chip">
          {{ RATING_LABELS[rc.rating] ?? rc.rating }} {{ rc.count }}
        </span>
      </div>
    </div>
  </LSection>
</template>

<style scoped>
.intro {
  margin: 0 0 var(--space-3);
  line-height: var(--lh-body);
  color: var(--text-2);
}
.banner-error {
  margin-bottom: var(--space-3);
  color: var(--danger-ink);
  background: var(--danger-soft);
  border-color: var(--danger-line);
}
.muted-line {
  margin: 0;
  color: var(--muted);
  font-size: var(--fs-ui);
}
.summary {
  margin: 0 0 var(--space-3);
  color: var(--muted);
  font-size: var(--fs-ui);
}
.mastery-row {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  margin-top: var(--space-2);
}
.mastery-bar {
  flex: 1;
  max-width: 240px;
}
.review {
  margin-bottom: var(--space-4);
}
.review-head {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  margin-bottom: var(--space-2);
}
.review-title {
  font-weight: 600;
  color: var(--accent);
}
.pace-bar {
  width: 96px;
}
.review-card {
  border: 1px solid color-mix(in srgb, var(--accent) 35%, var(--border));
  border-radius: var(--radius-sm);
  padding: var(--space-3) var(--space-4);
  background: var(--primary-soft);
}
.review-type {
  font-size: var(--fs-sm);
  color: var(--muted);
  margin-bottom: var(--space-2);
}
.review-actions {
  margin-top: var(--space-3);
  display: flex;
  gap: var(--space-2);
  flex-wrap: wrap;
}
.review-context {
  margin-top: var(--space-2);
  color: var(--muted);
  font-size: var(--fs-ui);
}
.tip {
  margin: var(--space-2) 0 0;
  color: var(--muted);
  font-size: var(--fs-ui);
}
.pace {
  color: var(--muted);
  font-size: var(--fs-sm);
}
.review.done {
  padding: var(--space-3) var(--space-4);
  border: 1px dashed color-mix(in srgb, var(--ok) 55%, var(--border));
  border-radius: var(--radius-sm);
  color: var(--ok);
  font-size: var(--fs-md);
}
.undo-bar {
  margin: 0 0 var(--space-4);
  display: flex;
  align-items: center;
  gap: var(--space-2);
  font-size: var(--fs-ui);
  color: var(--muted);
}
.notebook-controls {
  display: flex;
  align-items: center;
  gap: var(--space-3);
  margin-bottom: var(--space-3);
  flex-wrap: wrap;
}
.search-input {
  width: 200px;
}
.group {
  margin-bottom: var(--space-4);
}
.group-head {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  margin-bottom: var(--space-2);
  cursor: pointer;
  user-select: none;
}
.group-caret {
  color: var(--muted);
  font-size: var(--fs-sm);
  width: 14px;
}
.group-label {
  font-weight: 600;
}
.mistake {
  padding: var(--space-2) var(--space-3);
  border: 1px solid var(--border);
  border-radius: var(--radius-ctl);
  margin-bottom: var(--space-2);
}
.sentence {
  line-height: var(--lh-ui);
  margin-bottom: var(--space-1);
}
.fix {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  font-size: var(--fs-ui);
  flex-wrap: wrap;
}
.orig {
  color: var(--danger);
  text-decoration: line-through;
}
.corr {
  color: var(--ok);
  font-weight: 600;
}
.arrow {
  color: var(--muted);
}
.time {
  margin-left: auto;
  color: var(--muted);
  font-size: var(--fs-sm);
}
.review-hint {
  font-size: var(--fs-sm);
  color: var(--muted);
  margin-bottom: var(--space-2);
}
.stats {
  margin-top: var(--space-5);
  border-top: 1px solid var(--border);
  padding-top: var(--space-3);
}
.stats-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: var(--space-2);
}
.stats-title {
  font-weight: 600;
  font-size: var(--fs-ui);
}
.stats-bars {
  display: flex;
  align-items: flex-end;
  gap: 2px;
  height: var(--h-control);
}
.bar-col {
  flex: 1;
  display: flex;
  flex-direction: column;
  justify-content: flex-end;
  gap: 1px;
  min-width: 0;
}
.bar-adopt {
  background: color-mix(in srgb, var(--accent) 45%, transparent);
  border-radius: 2px;
}
.bar-review {
  background: var(--accent);
  border-radius: 2px;
}
.stats-legend {
  display: flex;
  gap: var(--space-3);
  align-items: center;
  margin-top: var(--space-2);
  font-size: var(--fs-sm);
  color: var(--muted);
  flex-wrap: wrap;
}
.dot {
  display: inline-block;
  width: 8px;
  height: 8px;
  border-radius: 2px;
  margin-right: var(--space-1);
}
.dot-adopt {
  background: color-mix(in srgb, var(--accent) 45%, transparent);
}
.dot-review {
  background: var(--accent);
}
.rating-chip {
  padding: 1px var(--space-2);
  border: 1px solid var(--border);
  border-radius: 10px;
}
</style>
