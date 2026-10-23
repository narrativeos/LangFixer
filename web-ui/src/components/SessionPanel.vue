<script setup lang="ts">
import { ref, computed } from 'vue'
import { errMsg } from '../lib/err'

import { api } from '../api/client'
import type { SessionAnalysis, HabitualErrorType } from '../types'

const sessionId = ref('')
const analysis = ref<SessionAnalysis | null>(null)
const loading = ref(false)
const error = ref('')

const dominantWithExamples = computed<HabitualErrorType[]>(() =>
  analysis.value?.habitual.filter((h) => h.is_dominant && h.examples.length) ?? [],
)

async function loadAnalysis() {
  if (!sessionId.value) {
    error.value = '请输入会话 ID'
    return
  }
  loading.value = true
  error.value = ''
  try {
    analysis.value = await api.getSessionAnalysis(sessionId.value)
  } catch (e) {
    error.value = errMsg(e)
  } finally {
    loading.value = false
  }
}

function ratioLabel(r: number): string {
  return `${(r * 100).toFixed(1)}%`
}
</script>

<template>
  <el-card shadow="never">
    <template #header>
      <div class="card-header">
        <span class="card-title">会话分析</span>
        <span class="sub">按会话 ID 查看习惯性错误分布</span>
      </div>
    </template>

    <div class="session-input">
      <el-input
        v-model="sessionId"
        placeholder="输入会话 ID（如 s_xxx）"
        style="width: 280px"
        @keyup.enter="loadAnalysis"
      />
      <el-button type="primary" :loading="loading" @click="loadAnalysis">
        {{ loading ? '分析中…' : '分析' }}
      </el-button>
    </div>

    <el-alert v-if="error" type="error" :closable="false" :title="error" />

    <div v-if="analysis" class="analysis-result">
      <el-descriptions :column="2" size="small" border class="summary-desc">
        <el-descriptions-item label="总错误数">{{ analysis.total_errors }}</el-descriptions-item>
        <el-descriptions-item label="错误类型数">
          {{ Object.keys(analysis.type_distribution).length }}
        </el-descriptions-item>
      </el-descriptions>

      <!-- 错误类型分布 -->
      <div v-if="analysis.habitual.length" class="section">
        <h4>错误类型分布</h4>
        <el-table :data="analysis.habitual" size="small">
          <el-table-column prop="error_type" label="类型" width="160" />
          <el-table-column prop="count" label="次数" width="80" />
          <el-table-column label="占比" width="100">
            <template #default="{ row }">{{ ratioLabel(row.ratio) }}</template>
          </el-table-column>
          <el-table-column label="主导" width="80">
            <template #default="{ row }">
              <el-tag v-if="row.is_dominant" type="warning" size="small">是</el-tag>
              <el-tag v-else type="info" effect="plain" size="small">否</el-tag>
            </template>
          </el-table-column>
        </el-table>
      </div>

      <!-- 习惯性错误示例 -->
      <div v-if="dominantWithExamples.length" class="section">
        <h4>习惯性错误示例</h4>
        <div v-for="h in dominantWithExamples" :key="h.error_type">
          <p class="type-label">{{ h.error_type }}（{{ ratioLabel(h.ratio) }}）</p>
          <ul>
            <li v-for="ex in h.examples" :key="ex">{{ ex }}</li>
          </ul>
        </div>
      </div>

      <!-- 高频修正对 -->
      <div v-if="analysis.top_corrections.length" class="section">
        <h4>高频修正对（≥2 次）</h4>
        <el-table :data="analysis.top_corrections" size="small">
          <el-table-column label="错误" width="200">
            <template #default="{ row }"><span class="from">{{ row.from }}</span></template>
          </el-table-column>
          <el-table-column label="修正" width="200">
            <template #default="{ row }"><span class="to">{{ row.to }}</span></template>
          </el-table-column>
          <el-table-column prop="count" label="次数" width="80" />
        </el-table>
      </div>
    </div>
  </el-card>
</template>

<style scoped>
.session-input {
  display: flex;
  gap: 8px;
  margin-bottom: 16px;
}
.summary-desc { margin-bottom: 16px; }
.section {
  margin-bottom: 20px;
}
.section h4 {
  margin: 0 0 8px;
  font-size: 14px;
  color: var(--text-2);
}
.type-label {
  font-weight: 600;
  margin: 8px 0 4px;
}
.type-label ul {
  margin: 0;
  padding-left: 20px;
}
.from {
  color: var(--danger);
  text-decoration: line-through;
}
.to {
  color: var(--ok);
  font-weight: 500;
}
</style>