<script setup lang="ts">
import { ref, onMounted, watch } from 'vue'
import { errMsg } from '../lib/err'

import { api } from '../api/client'
import type { CustomWord } from '../types'

const props = defineProps<{ lang: string }>()

const words = ref<CustomWord[]>([])
const loading = ref(false)
const error = ref('')
const newWord = ref('')
const newReason = ref('')

async function load() {
  loading.value = true
  error.value = ''
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
    newWord.value = ''
    newReason.value = ''
    await load()
  } catch (e) {
    error.value = errMsg(e)
  }
}

async function remove(id: number) {
  try {
    await api.removeCustomWord(id)
    await load()
  } catch (e) {
    error.value = errMsg(e)
  }
}

onMounted(load)
watch(() => props.lang, load)
</script>

<template>
  <el-card shadow="never">
    <template #header>
      <div class="card-header">
        <span class="card-title">自定义词（白名单）</span>
        <span class="sub">添加后，检测时匹配的词条将被自动排除</span>
      </div>
    </template>

    <div class="add-form">
      <el-input v-model="newWord" placeholder="输入词语…" @keyup.enter="add" style="flex: 1" />
      <el-input v-model="newReason" placeholder="原因（可选）" @keyup.enter="add" style="flex: 1" />
      <el-button type="primary" :disabled="!newWord.trim()" @click="add">添加</el-button>
    </div>

    <div v-if="loading" class="loading">
      <el-icon class="is-loading"><Loading /></el-icon>
      加载中…
    </div>
    <el-alert v-else-if="error" type="error" :closable="false" :title="error" />
    <el-table v-else-if="words.length > 0" :data="words">
      <el-table-column prop="word" label="词语" width="180" />
      <el-table-column prop="reason" label="原因">
        <template #default="{ row }">{{ row.reason || '—' }}</template>
      </el-table-column>
      <el-table-column prop="created_at" label="添加时间" width="180" />
      <el-table-column label="" width="90">
        <template #default="{ row }">
          <el-button type="danger" size="small" plain @click="remove(row.id)">删除</el-button>
        </template>
      </el-table-column>
    </el-table>
    <el-empty
      v-else
      description="暂无自定义词 — 在上方添加专有名词或领域词汇，避免误报"
      :image-size="80"
    />
  </el-card>
</template>

<style scoped>
.add-form { display: flex; gap: 10px; margin-bottom: 14px; }
</style>