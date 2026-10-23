<script setup lang="ts">
import { ref, onMounted, watch } from 'vue'
import { errMsg } from '../lib/err'

import { Delete } from '@element-plus/icons-vue'
import { api } from '../api/client'
import type { LearnedKnowledge } from '../types'

const entries = ref<LearnedKnowledge[]>([])
const lang = ref('zh')
const loading = ref(false)
const error = ref('')
const saving = ref(false)

// 添加表单
const showForm = ref(false)
const formText = ref('')
const formErrorType = ref('')
const formCorrection = ref('')
const formSource = ref('manual')

async function load() {
  loading.value = true
  error.value = ''
  try {
    entries.value = await api.listLearnedKnowledge(lang.value)
  } catch (e) {
    error.value = errMsg(e)
  } finally {
    loading.value = false
  }
}

async function submit() {
  if (!formText.value || !formErrorType.value || !formCorrection.value) return
  saving.value = true
  error.value = ''
  try {
    await api.addLearnedKnowledge({
      lang: lang.value,
      text: formText.value,
      error_type: formErrorType.value,
      correction: formCorrection.value,
      source: formSource.value,
    })
    formText.value = ''
    formErrorType.value = ''
    formCorrection.value = ''
    showForm.value = false
    await load()
  } catch (e) {
    error.value = errMsg(e)
  } finally {
    saving.value = false
  }
}

async function remove(id: number) {
  try {
    await api.removeLearnedKnowledge(id)
    await load()
  } catch (e) {
    error.value = errMsg(e)
  }
}

onMounted(load)
watch(lang, load)
</script>

<template>
  <div class="lk">
    <el-card shadow="never">
      <template #header>
        <div class="card-header">
          <span class="card-title">学习知识（向量召回）</span>
          <span class="sub">
            检测流水线 ②.5 层：决策模型弃权后、LLM 兜底前，用嵌入向量召回相似知识条目
          </span>
          <span class="spacer"></span>
          <el-select v-model="lang" size="small" style="width: 90px">
            <el-option label="zh" value="zh" />
            <el-option label="en" value="en" />
            <el-option label="ja" value="ja" />
          </el-select>
          <el-button size="small" type="primary" @click="showForm = !showForm">
            {{ showForm ? '收起' : '添加条目' }}
          </el-button>
        </div>
      </template>

      <!-- 添加表单 -->
      <el-form v-if="showForm" label-width="100px" size="small" class="lk-form">
        <el-form-item label="原文片段">
          <el-input v-model="formText" placeholder="包含错误的原文句子或片段" />
        </el-form-item>
        <el-form-item label="错误类型">
          <el-input v-model="formErrorType" placeholder="如：article_misuse / tense_error / preposition" />
        </el-form-item>
        <el-form-item label="修正">
          <el-input v-model="formCorrection" placeholder="正确的写法或解释" />
        </el-form-item>
        <el-form-item label="来源">
          <el-input v-model="formSource" placeholder="manual / llm_feedback / import" style="width: 200px" />
        </el-form-item>
        <el-form-item>
          <el-button type="primary" :loading="saving" @click="submit">保存</el-button>
        </el-form-item>
      </el-form>

      <div v-if="loading" class="loading">加载中…</div>
      <el-alert v-else-if="error" type="error" :closable="false" :title="error" />
      <el-table v-else-if="entries.length > 0" :data="entries" size="small" max-height="480">
        <el-table-column prop="text" label="原文" min-width="240" show-overflow-tooltip />
        <el-table-column prop="error_type" label="错误类型" width="140" />
        <el-table-column prop="correction" label="修正" min-width="200" show-overflow-tooltip />
        <el-table-column prop="source" label="来源" width="120" />
        <el-table-column prop="created_at" label="创建时间" width="160" />
        <el-table-column label="" width="60" align="center">
          <template #default="{ row }">
            <el-button size="small" type="danger" :icon="Delete" circle @click="remove(row.id)" />
          </template>
        </el-table-column>
      </el-table>
      <el-empty v-else description="暂无学习知识条目 — 添加后将在检测流水线中参与向量召回" :image-size="80" />
    </el-card>
  </div>
</template>

<style scoped>
.lk { display: flex; flex-direction: column; gap: 14px; }
.lk-form { margin-bottom: 12px; }
</style>