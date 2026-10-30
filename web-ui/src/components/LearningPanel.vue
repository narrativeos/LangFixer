<script setup lang="ts">
/**
 * 练习与复习（学习扩展，Phase 2）
 *
 * 闭环：检测中确认并采纳的错误 → 错题本（按类型聚合，带句级上下文）
 * → 间隔复习（句子挖空回忆修正，记住/没记住）→ 掌握度（连对 1/3/7/14/30 天，
 * 6 次连对 = 已掌握；没记住次日再练）。
 */
import { computed, onMounted, ref } from 'vue'
import { ElMessage } from 'element-plus'
import { useGecStore } from '../stores/gec'
import { api } from '../api/client'
import { errorTypeLabel } from '../lib/labels'
import type { Mistake } from '../types'

const store = useGecStore()

const mistakes = ref<Mistake[]>([])
const queue = ref<Mistake[]>([])
const loading = ref(false)
const loaded = ref(false)
const errorMsg = ref('')

/** 当前复习卡：队列首条 + 是否已揭晓答案 + 提交中 */
const current = computed(() => queue.value[0] ?? null)
const revealed = ref(false)
const submitting = ref(false)

/** 挖空句：把错误片段替换为下划线（大小写不敏感一次替换） */
const clozeSentence = computed(() => {
  const m = current.value
  if (!m) return ''
  if (!m.original || !m.sentence.includes(m.original)) return m.sentence
  return m.sentence.replace(m.original, '＿＿＿')
})

const masteredCount = computed(
  () => mistakes.value.filter((m) => m.mastery_status === 'mastered').length,
)

/** 按错误类型聚合（保持最近出现顺序） */
const groups = computed(() => {
  const map = new Map<string, Mistake[]>()
  for (const m of mistakes.value) {
    const arr = map.get(m.error_type)
    if (arr) arr.push(m)
    else map.set(m.error_type, [m])
  }
  return [...map.entries()].map(([type, items]) => ({
    type,
    label: errorTypeLabel(type),
    total: items.reduce((s, m) => s + m.count, 0),
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
  if (!store.sessionId) {
    mistakes.value = []
    queue.value = []
    loaded.value = true
    return
  }
  loading.value = true
  errorMsg.value = ''
  try {
    const [ms, q] = await Promise.all([
      api.listSessionMistakes(store.sessionId),
      api.listReviewQueue(store.sessionId),
    ])
    mistakes.value = ms
    queue.value = q
    revealed.value = false
  } catch (e) {
    errorMsg.value = e instanceof Error ? e.message : String(e)
  } finally {
    loading.value = false
    loaded.value = true
  }
}

async function submitReview(passed: boolean) {
  const m = current.value
  if (!m || !store.sessionId) return
  submitting.value = true
  try {
    await api.reviewMistake(store.sessionId, m.id, passed)
    queue.value = queue.value.filter((x) => x.id !== m.id)
    revealed.value = false
    // 列表掌握度同步刷新（非阻断）
    try {
      mistakes.value = await api.listSessionMistakes(store.sessionId)
    } catch {
      /* 列表刷新失败不影响复习流 */
    }
  } catch (e) {
    ElMessage.error(e instanceof Error ? e.message : String(e))
  } finally {
    submitting.value = false
  }
}

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
      你在「检测」中<b>确认并采纳</b>的纠错会自动沉淀到这里，按错误类型聚合。
      后续将开放同型错的改写练习与周期性复习。
    </p>

    <el-alert v-if="errorMsg" :title="`错题本加载失败：${errorMsg}`" type="warning" :closable="false" class="err" />

    <!-- 间隔复习：到期错题的练习卡（挖空回忆 → 揭晓 → 记住/没记住） -->
    <div v-if="current" class="review">
      <div class="review-head">
        <span class="review-title">今日复习</span>
        <el-tag size="small" round>{{ queue.length }} 题待练</el-tag>
      </div>
      <div class="review-card">
        <div class="review-type">{{ errorTypeLabel(current.error_type) }}</div>
        <div class="sentence">{{ clozeSentence }}</div>
        <div v-if="revealed" class="fix">
          <span class="orig">{{ current.original }}</span>
          <span v-if="current.correction" class="arrow">→</span>
          <span v-if="current.correction" class="corr">{{ current.correction }}</span>
        </div>
        <div class="review-actions">
          <template v-if="!revealed">
            <el-button size="small" @click="revealed = true">显示答案</el-button>
          </template>
          <template v-else>
            <el-button size="small" type="success" :loading="submitting" @click="submitReview(true)">
              记住了
            </el-button>
            <el-button size="small" type="warning" plain :loading="submitting" @click="submitReview(false)">
              没记住
            </el-button>
          </template>
        </div>
      </div>
    </div>

    <div v-if="groups.length" class="summary">
      共 {{ mistakes.length }} 条错题 · 待复习 {{ queue.length }} · 已掌握 {{ masteredCount }}
    </div>

    <template v-if="loading && !groups.length">
      <el-skeleton :rows="3" animated />
    </template>
    <template v-else-if="!groups.length">
      <el-empty
        :description="loaded ? '还没有错题——去「检测」处理文稿，采纳的纠错会自动进入错题本' : '加载中…'"
      >
        <el-button type="primary" plain @click="goWorkspace">去检测</el-button>
      </el-empty>
      <p class="tip">
        提示：完善学习者档案（母语 + CEFR 自评）后，LLM 讲解会按你的水平自动个性化。
        <el-link type="primary" :underline="false" @click="goProfile">完善我的档案</el-link>
      </p>
    </template>
    <template v-else>
      <div v-for="g in groups" :key="g.type" class="group">
        <div class="group-head">
          <span class="group-label">{{ g.label }}</span>
          <el-tag size="small" type="info" round>{{ g.total }} 次</el-tag>
        </div>
        <div v-for="m in g.items" :key="m.id" class="mistake">
          <div class="sentence">{{ m.sentence || m.original }}</div>
          <div class="fix">
            <span class="orig">{{ m.original }}</span>
            <span v-if="m.correction" class="arrow">→</span>
            <span v-if="m.correction" class="corr">{{ m.correction }}</span>
            <el-tag v-if="m.count > 1" size="small" type="warning" effect="plain">×{{ m.count }}</el-tag>
            <el-tag size="small" :type="masteryTag(m).type" effect="plain">{{ masteryTag(m).label }}</el-tag>
            <span class="time">{{ m.created_at.slice(0, 10) }}</span>
          </div>
        </div>
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
.review-actions { margin-top: 10px; display: flex; gap: 8px; }
.tip { margin: 8px 0 0; color: var(--el-text-color-secondary); font-size: 13px; }
.group { margin-bottom: 16px; }
.group-head { display: flex; align-items: center; gap: 8px; margin-bottom: 6px; }
.group-label { font-weight: 600; }
.mistake { padding: 8px 10px; border: 1px solid var(--el-border-color-lighter); border-radius: 6px; margin-bottom: 6px; }
.sentence { line-height: 1.6; margin-bottom: 4px; }
.fix { display: flex; align-items: center; gap: 8px; font-size: 13px; }
.orig { color: var(--el-color-danger); text-decoration: line-through; }
.corr { color: var(--el-color-success); font-weight: 600; }
.arrow { color: var(--el-text-color-secondary); }
.time { margin-left: auto; color: var(--el-text-color-secondary); font-size: 12px; }
</style>