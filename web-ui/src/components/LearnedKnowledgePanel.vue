<script setup lang="ts">
import { ref, onMounted, watch } from 'vue'
import { errMsg } from '../lib/err'
import { errorTypeLabel } from '../lib/labels'
import { LButton, LEmpty, LInput, LSection, LSelect, LTable, LTag } from '../ui'
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

const langOptions = [
  { value: 'zh', label: 'zh' },
  { value: 'en', label: 'en' },
  { value: 'ja', label: 'ja' },
]

const columns = [
  { key: 'text', label: '原文', sortable: true },
  { key: 'error_type', label: '错误类型', width: '140px' },
  { key: 'correction', label: '修正' },
  { key: 'source', label: '来源', width: '120px' },
  { key: 'created_at', label: '创建时间', width: '160px', sortable: true },
  { key: 'actions', label: '', width: '72px', align: 'right' as const },
]

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
    <LSection
      title="学习知识（向量召回）"
      desc="检测流水线 ②.5 层：决策模型弃权后、LLM 兜底前，用嵌入向量召回相似知识条目"
    >
      <template #actions>
        <LSelect
          class="sel-lang"
          :model-value="lang"
          :options="langOptions"
          aria-label="学习知识语言"
          @update:model-value="lang = String($event)"
        />
        <LButton size="sm" variant="primary" @click="showForm = !showForm">
          {{ showForm ? '收起' : '添加条目' }}
        </LButton>
      </template>

      <!-- 添加表单 -->
      <div v-if="showForm" class="lk-form">
        <div class="field">
          <span class="field-label">原文片段</span>
          <LInput v-model="formText" placeholder="包含错误的原文句子或片段" />
        </div>
        <div class="field">
          <span class="field-label">错误类型</span>
          <LInput v-model="formErrorType" placeholder="如：article_misuse / tense_error / preposition" />
        </div>
        <div class="field">
          <span class="field-label">修正</span>
          <LInput v-model="formCorrection" placeholder="正确的写法或解释" />
        </div>
        <div class="field">
          <span class="field-label">来源</span>
          <LInput v-model="formSource" class="fld-source" placeholder="manual / llm_feedback / import" />
        </div>
        <div class="field">
          <span class="field-label"></span>
          <LButton variant="primary" :loading="saving" @click="submit">保存</LButton>
        </div>
      </div>

      <div v-if="loading" class="loading">加载中…</div>
      <div v-else-if="error" class="err-line">
        <span>{{ error }}</span>
        <LButton size="sm" @click="load">重试</LButton>
      </div>
      <div v-else-if="entries.length > 0" class="table-scroll">
        <LTable :columns="columns" :rows="entries" row-key="id" compact>
          <template #col-error_type="{ row }">{{ errorTypeLabel(row.error_type) }}</template>
          <template #col-source="{ row }">
            <LTag>{{ row.source }}</LTag>
          </template>
          <template #col-actions="{ row }">
            <LButton size="sm" variant="danger" title="删除该条学习知识" @click="remove(row.id)">删除</LButton>
          </template>
        </LTable>
      </div>
      <LEmpty
        v-else
        text="暂无学习知识条目"
        hint="添加条目后将在检测流水线中参与向量召回（决策模型弃权后、LLM 兜底前）"
      >
        <template #action>
          <LButton size="sm" variant="primary" @click="showForm = true">添加条目</LButton>
        </template>
      </LEmpty>
    </LSection>
  </div>
</template>

<style scoped>
.lk { display: flex; flex-direction: column; }
.lk-form { margin-bottom: var(--space-3); }
.field {
  display: grid;
  grid-template-columns: calc(var(--space-4) * 7) minmax(0, calc(var(--space-7) * 9));
  align-items: center;
  gap: var(--space-2) var(--space-3);
  margin-bottom: var(--space-2);
  font-size: var(--fs-ui);
}
.field-label { color: var(--text-2); }
.fld-source { width: calc(var(--space-7) * 4); }
.sel-lang { width: calc(var(--space-7) * 2); }
.loading { padding: var(--space-3) 0; font-size: var(--fs-sm); color: var(--muted); }
.err-line {
  display: flex;
  align-items: center;
  gap: var(--space-3);
  padding: var(--space-1) 0;
  font-size: var(--fs-ui);
  color: var(--danger);
}
.table-scroll { max-height: calc(var(--space-7) * 10); overflow-y: auto; }
</style>
