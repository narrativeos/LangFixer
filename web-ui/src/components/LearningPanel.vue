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
import { ElMessage } from 'element-plus'
import { useGecStore } from '../stores/gec'
import { api } from '../api/client'
import { errorTypeLabel } from '../lib/labels'
import type { Mistake, ReviewRating, SessionAnalysis } from '../types'

const store = useGecStore()

const mistakes = ref<Mistake[]>([])
const queue = ref<Mistake[]>([])
/** 统一错误画像（与审校侧习惯性错误分析同一投影，GET analysis） */
const profile = ref<SessionAnalysis | null>(null)
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
  type: 'danger' | 'warning' | 'primary' | 'success'
}> = [
  { key: 'again', label: '忘了', type: 'danger' },
  { key: 'hard', label: '有点难', type: 'warning' },
  { key: 'good', label: '记住了', type: 'primary' },
  { key: 'easy', label: '很轻松', type: 'success' },
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

function masteryTag(m: Mistake): { label: string; type: 'info' | 'warning' | 'success' } {
  switch (m.mastery_status) {
    case 'mastered':
      return { label: '已掌握', type: 'success' }
    case 'reviewing':
      return { label: m.next_review_at ? `复习中 · ${m.next_review_at.slice(0, 10)}` : '复习中', type: 'warning' }
    default:
      return { label: '待复习', type: 'info' }
  }
}

async function load() {
  loading.value = true
  errorMsg.value = ''
  try {
    const [ms, q, p] = await Promise.all([
      api.listGlobalMistakes(),
      api.listGlobalReviewQueue(),
      api.getGlobalAnalysis().catch(() => null),
    ])
    mistakes.value = ms
    queue.value = q
    profile.value = p
    revealed.value = false
  } catch (e) {
    errorMsg.value = e instanceof Error ? e.message : String(e)
  } finally {
    loading.value = false
    loaded.value = true
  }
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
    // 列表掌握度同步刷新（非阻断）
    try {
      mistakes.value = await api.listGlobalMistakes()
    } catch {
      /* 列表刷新失败不影响复习流 */
    }
  } catch (e) {
    ElMessage.error(e instanceof Error ? e.message : String(e))
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
    const [ms, q] = await Promise.all([api.listGlobalMistakes(), api.listGlobalReviewQueue()])
    mistakes.value = ms
    queue.value = q
    ElMessage.success('已撤销：该错题重回到复习队列')
  } catch (e) {
    ElMessage.error(e instanceof Error ? e.message : String(e))
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

function goWorkspace() {
  store.requestNav('workspace')
}
function goProfile() {
  store.requestNav('settings', 'profile')
}

onMounted(load)
</script>

<template>
  <el-card shadow="never" class="mistake-card">
    <template #header>
      <div class="card-head">
        <span class="card-title">错题本</span>
        <el-button size="small" text :loading="loading" @click="load">刷新</el-button>
      </div>
    </template>

    <p class="intro">
      你在「检测」中<b>确认并采纳</b>的纠错会自动沉淀到这里，<b>跨会话</b>按错误类型聚合——
      换文稿、删会话都不丢；复习进度（FSRS 记忆状态）同样跨会话共享。
    </p>

    <el-alert v-if="errorMsg" :title="`错题本加载失败：${errorMsg}`" type="warning" :closable="false" class="err" />

    <!-- 间隔复习：到期错题的练习卡（挖空回忆 → 揭晓 → FSRS 四键自评） -->
    <div v-if="current" class="review">
      <div class="review-head">
        <span class="review-title">今日复习</span>
        <el-tag size="small" round>{{ queue.length }} 题待练</el-tag>
        <span class="pace">今日已复习 {{ doneToday }}/{{ DAILY_GOAL }}</span>
      </div>
      <div class="review-card">
        <div class="review-type">{{ errorTypeLabel(current.error_type) }}</div>
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
            <el-button size="small" @click="revealed = true">显示答案</el-button>
          </template>
          <template v-else>
            <el-button
              v-for="r in RATINGS"
              :key="r.key"
              size="small"
              :type="r.type"
              :plain="r.key === 'again' || r.key === 'hard'"
              :loading="submitting"
              @click="submitReview(r.key)"
            >
              {{ r.label }}
            </el-button>
          </template>
        </div>
      </div>
    </div>
    <div v-else-if="mistakes.length && doneToday > 0" class="review done">
      🎉 今日已复习 {{ doneToday }} 题{{ doneToday >= DAILY_GOAL ? '，目标达成，明天见' : '——错题到期会自动回到队列' }}
    </div>
    <div v-if="undoTarget" class="undo-bar">
      已复习「{{ undoTarget.original }}」
      <el-link type="primary" :underline="false" @click="undoReview">撤销（5 秒内）</el-link>
    </div>

    <div v-if="groups.length" class="summary">
      共 {{ mistakes.length }} 条错题 · 待复习 {{ queue.length }} · 已掌握 {{ masteredCount }}
      <span v-if="profile?.basis === 'confirmed'" class="basis">
        · 画像基于确认错误（误报与未采纳建议不计入）
      </span>
    </div>

    <!-- 错题本浏览控制：页签过滤 + 关键词搜索 -->
    <div v-if="mistakes.length" class="notebook-controls">
      <el-radio-group v-model="notebookTab" size="small">
        <el-radio-button value="due">待复习 {{ queue.length }}</el-radio-button>
        <el-radio-button value="all">全部 {{ mistakes.length }}</el-radio-button>
        <el-radio-button value="mastered">已掌握 {{ masteredCount }}</el-radio-button>
      </el-radio-group>
      <el-input
        v-model="search"
        size="small"
        placeholder="搜索错题内容"
        clearable
        class="search-input"
      />
    </div>

    <template v-if="loading && !groups.length">
      <el-skeleton :rows="3" animated />
    </template>
    <template v-else-if="!groups.length">
      <el-empty :description="emptyText">
        <el-button v-if="!mistakes.length" type="primary" plain @click="goWorkspace">去检测</el-button>
      </el-empty>
      <p v-if="!mistakes.length" class="tip">
        提示：完善学习者档案（母语 + CEFR 自评）后，LLM 讲解会按你的水平自动个性化。
        <el-link type="primary" :underline="false" @click="goProfile">完善我的档案</el-link>
      </p>
    </template>
    <template v-else>
      <div v-for="g in groups" :key="g.type" class="group">
        <div class="group-head" @click="toggleGroup(g.type)">
          <span class="group-caret">{{ collapsed.has(g.type) ? '▸' : '▾' }}</span>
          <span class="group-label">{{ g.label }}</span>
          <el-tag v-if="g.habitual" size="small" type="danger" effect="plain">习惯性</el-tag>
          <el-tag size="small" type="info" round>{{ g.total }} 次</el-tag>
        </div>
        <template v-if="!collapsed.has(g.type)">
          <div v-for="m in g.items" :key="m.id" class="mistake">
            <div class="sentence">{{ m.sentence || m.original }}</div>
            <div class="fix">
              <span class="orig">{{ m.original }}</span>
              <span v-if="m.correction" class="arrow">→</span>
              <span v-if="m.correction" class="corr">{{ m.correction }}</span>
              <el-tag v-if="m.count > 1" size="small" type="warning" effect="plain">×{{ m.count }}</el-tag>
              <el-tag v-if="m.sessions_seen && m.sessions_seen > 1" size="small" type="info" effect="plain">来自 {{ m.sessions_seen }} 个会话</el-tag>
              <el-tag size="small" :type="masteryTag(m).type" effect="plain">{{ masteryTag(m).label }}</el-tag>
              <span class="time">{{ m.created_at.slice(0, 10) }}</span>
            </div>
          </div>
        </template>
      </div>
    </template>
  </el-card>
</template>

<style scoped>
.card-head { display: flex; align-items: center; justify-content: space-between; }
.card-title { font-weight: 600; }
.intro { margin: 0 0 12px; line-height: 1.7; color: var(--el-text-color-regular); }
.err { margin-bottom: 12px; }
.summary { margin: 0 0 12px; color: var(--el-text-color-secondary); font-size: 13px; }
.review { margin-bottom: 16px; }
.review-head { display: flex; align-items: center; gap: 8px; margin-bottom: 6px; }
.review-title { font-weight: 600; color: var(--el-color-primary); }
.review-card { border: 1px solid var(--el-color-primary-light-5); border-radius: 8px; padding: 12px 14px; background: var(--el-color-primary-light-9); }
.review-type { font-size: 12px; color: var(--el-text-color-secondary); margin-bottom: 6px; }
.review-actions { margin-top: 10px; display: flex; gap: 8px; flex-wrap: wrap; }
.review-context { margin-top: 6px; color: var(--el-text-color-secondary); font-size: 13px; }
.tip { margin: 8px 0 0; color: var(--el-text-color-secondary); font-size: 13px; }
.pace { color: var(--el-text-color-secondary); font-size: 12px; }
.review.done { padding: 10px 14px; border: 1px dashed var(--el-color-success-light-5); border-radius: 8px; color: var(--el-color-success); font-size: 14px; }
.undo-bar { margin: 0 0 14px; display: flex; align-items: center; gap: 8px; font-size: 13px; color: var(--el-text-color-secondary); }
.notebook-controls { display: flex; align-items: center; gap: 10px; margin-bottom: 12px; flex-wrap: wrap; }
.search-input { width: 200px; }
.group { margin-bottom: 16px; }
.group-head { display: flex; align-items: center; gap: 8px; margin-bottom: 6px; cursor: pointer; user-select: none; }
.group-caret { color: var(--el-text-color-secondary); font-size: 12px; width: 14px; }
.group-label { font-weight: 600; }
.mistake { padding: 8px 10px; border: 1px solid var(--el-border-color-lighter); border-radius: 6px; margin-bottom: 6px; }
.sentence { line-height: 1.6; margin-bottom: 4px; }
.fix { display: flex; align-items: center; gap: 8px; font-size: 13px; }
.orig { color: var(--el-color-danger); text-decoration: line-through; }
.corr { color: var(--el-color-success); font-weight: 600; }
.arrow { color: var(--el-text-color-secondary); }
.time { margin-left: auto; color: var(--el-text-color-secondary); font-size: 12px; }
</style>