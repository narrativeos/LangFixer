<script setup lang="ts">
import { ref, onMounted, watch } from 'vue'
import { errMsg } from '../lib/err'

import { api } from '../api/client'
import type { CustomWord } from '../types'
import { LButton, LEmpty, LInput, LSection, LTable, toast } from '../ui'
import type { LColumn } from '../ui'

const props = defineProps<{ lang: string }>()

const words = ref<CustomWord[]>([])
const loading = ref(false)
const error = ref('')
const newWord = ref('')
const newReason = ref('')
/** 两步删除确认：待确认的词 id */
const pendingDelete = ref<number | null>(null)

const cols: LColumn<CustomWord>[] = [
  { key: 'word', label: '词语', width: '180px' },
  { key: 'reason', label: '原因' },
  { key: 'created_at', label: '添加时间', width: '180px' },
  { key: 'actions', label: '', width: '150px' },
]

async function load() {
  loading.value = true
  error.value = ''
  pendingDelete.value = null
  try {
    const res = await api.listCustomWords(props.lang)
    words.value = res.words
  } catch (e) {
    error.value = errMsg(e)
  } finally {
    loading.value = false
  }
}

async function add() {
  if (!newWord.value.trim()) return
  error.value = ''
  try {
    await api.addCustomWord(props.lang, newWord.value.trim(), newReason.value.trim())
    const added = newWord.value.trim()
    newWord.value = ''
    newReason.value = ''
    await load()
    toast(`已添加自定义词「${added}」`, 'ok')
  } catch (e) {
    error.value = errMsg(e)
  }
}

function askDelete(w: CustomWord) {
  pendingDelete.value = w.id
}

async function remove(w: CustomWord) {
  pendingDelete.value = null
  try {
    await api.removeCustomWord(w.id)
    await load()
    toast(`已删除自定义词「${w.word}」`, 'info')
  } catch (e) {
    error.value = errMsg(e)
  }
}

onMounted(load)
watch(() => props.lang, load)
</script>

<template>
  <LSection title="自定义词（白名单）" desc="添加后，检测时匹配的词条将被自动排除">
    <div class="add-form">
      <LInput v-model="newWord" placeholder="输入词语…" class="w-word" @keyup.enter="add" />
      <LInput v-model="newReason" placeholder="原因（可选）" class="w-reason" @keyup.enter="add" />
      <LButton variant="primary" :disabled="!newWord.trim()" @click="add">添加</LButton>
    </div>

    <div v-if="loading" class="sub">加载中…</div>
    <div v-else-if="error" class="error-banner">
      <span class="error-text">加载失败：{{ error }} — 检查后端服务后重试</span>
      <LButton size="sm" @click="load">重试</LButton>
    </div>
    <LTable v-else-if="words.length > 0" :columns="cols" :rows="words" row-key="id" compact>
      <template #col-reason="{ row }">{{ row.reason || '—' }}</template>
      <template #col-actions="{ row }">
        <span v-if="pendingDelete === row.id" class="confirm-pair">
          <LButton size="sm" variant="danger" @click="remove(row)">确认删除</LButton>
          <LButton size="sm" variant="ghost" @click="pendingDelete = null">取消</LButton>
        </span>
        <LButton v-else size="sm" @click="askDelete(row)">删除</LButton>
      </template>
    </LTable>
    <LEmpty
      v-else
      text="暂无自定义词"
      hint="在上方添加专有名词或领域词汇，避免误报"
    />
  </LSection>
</template>

<style scoped>
.add-form { display: flex; gap: var(--space-2); margin-bottom: var(--space-4); }
.add-form .w-word,
.add-form .w-reason { flex: 1; min-width: 0; }
.sub { color: var(--muted); font-size: var(--fs-sm); }
.error-banner {
  display: flex; align-items: center; justify-content: space-between; gap: var(--space-3);
  padding: var(--space-2) var(--space-3); margin-bottom: var(--space-3);
  font-size: var(--fs-ui);
  background: var(--danger-soft); border: 1px solid var(--danger-line);
  border-radius: var(--radius-sm);
}
.error-text { color: var(--danger-ink); }
.confirm-pair { display: inline-flex; gap: var(--space-1); }
</style>
