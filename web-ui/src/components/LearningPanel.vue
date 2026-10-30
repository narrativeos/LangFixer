<script setup lang="ts">
/**
 * 练习与复习（学习扩展，Phase 2）
 *
 * 第一片：错题本——你在「检测」中确认并采纳的错误自动沉淀于此，
 * 按错误类型聚合（同型 + 同原文 + 同修正合并计数），带句级上下文。
 * 后续闭环：间隔复习（同型错改写题）→ 掌握度回顾。
 */
import { computed, onMounted, ref } from 'vue'
import { useGecStore } from '../stores/gec'
import { api } from '../api/client'
import { errorTypeLabel } from '../lib/labels'
import type { Mistake } from '../types'

const store = useGecStore()

const mistakes = ref<Mistake[]>([])
const loading = ref(false)
const loaded = ref(false)
const errorMsg = ref('')

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

async function load() {
  if (!store.sessionId) {
    mistakes.value = []
    loaded.value = true
    return
  }
  loading.value = true
  errorMsg.value = ''
  try {
    mistakes.value = await api.listSessionMistakes(store.sessionId)
  } catch (e) {
    errorMsg.value = e instanceof Error ? e.message : String(e)
  } finally {
    loading.value = false
    loaded.value = true
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