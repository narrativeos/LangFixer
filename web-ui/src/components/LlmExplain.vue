<script setup lang="ts">
import { onMounted, watch } from 'vue'
import { errorTypeLabel } from '../lib/labels'
import { useGecStore } from '../stores/gec'

const store = useGecStore()

/** 逐错误讲解联动：归因卡片「讲解」按钮递增 explainNonce。
 *  - 本组件已挂载（llm tab 曾打开过）：watch 直接触发生成；
 *  - 未挂载（tab 从未打开）：ResultsPanel 先切 tab → 本组件挂载，onMounted 补触发。 */
let handledNonce = 0
function maybeExplain() {
  if (store.explainNonce > 0 && store.explainNonce !== handledNonce) {
    handledNonce = store.explainNonce
    void store.explainStream(store.explainIndex)
  }
}
watch(() => store.explainNonce, maybeExplain)
onMounted(maybeExplain)
</script>

<template>
  <el-empty
    v-if="store.analyzed.length === 0"
    :description="
      store.detectCompleted
        ? '检测完成，未发现需要讲解的错误'
        : store.llmEnabled
          ? '尚未生成讲解 — 先在左侧完成「检测 + 归因」，再按 CEFR 水平生成个性化讲解'
          : 'LLM 未配置：个性化讲解不可用。到「设置 → LLM 参数」配置后，先在左侧完成「检测 + 归因」'
    "
    :image-size="80"
  />
  <div v-else>
    <el-alert
      v-if="store.llm.fallback"
      type="warning"
      :closable="false"
      show-icon
      title="LLM 讲解服务未配置（服务端未设置 LLM_API_KEY），已回退到决策表静态补救建议"
    />
    <template v-else>
      <div class="toolbar">
        <span class="hint">讲解语言</span>
        <el-select v-model="store.targetLanguage" style="width: 150px">
          <el-option label="跟随检测语言" value="" />
          <el-option label="中文" value="中文" />
          <el-option label="English" value="English" />
          <el-option label="日本語" value="日本語" />
          <el-option label="한국어" value="한국어" />
          <el-option label="Español" value="Español" />
          <el-option label="Français" value="Français" />
          <el-option label="Deutsch" value="Deutsch" />
          <el-option label="Русский" value="Русский" />
        </el-select>
        <span v-if="store.analyzed[store.explainIndex]" class="explain-target">
          讲解对象：{{ store.analyzed[store.explainIndex].error.original }}
          <template v-if="store.analyzed[store.explainIndex].error.correction">
            → {{ store.analyzed[store.explainIndex].error.correction }}
          </template>
          （{{ errorTypeLabel(store.analyzed[store.explainIndex].error.error_type) }}）
        </span>
        <el-button type="primary" :loading="store.llm.loading" @click="store.explainStream(store.explainIndex)">
          {{ store.llm.loading ? '生成中…' : '生成个性化讲解' }}
        </el-button>
        <span v-if="store.llm.meta" class="trace">
          模型 {{ store.llm.meta.model }} · 模板 {{ store.llm.meta.template }}
          (v{{ store.llm.meta.template_version }})
        </span>
      </div>
      <el-card
        v-if="store.llm.loading && !store.llm.explanation"
        shadow="never"
        class="llm-box generating"
      >
        <el-icon class="is-loading"><Loading /></el-icon>
        <span>正在生成个性化讲解…</span>
      </el-card>
      <el-card v-if="store.llm.explanation" shadow="never" class="llm-box">
        <template #header><b>个性化讲解</b></template>
        <p class="llm-text" style="white-space: pre-wrap">{{ store.llm.explanation }}</p>
      </el-card>
      <el-card v-if="store.llm.exercises" shadow="never" class="llm-box">
        <template #header><b>个性化练习</b></template>
        <p class="llm-text" style="white-space: pre-wrap">{{ store.llm.exercises }}</p>
      </el-card>
      <div v-if="!store.llm.explanation && !store.llm.loading" class="hint">
        点击「生成个性化讲解」，按 CEFR 水平流式生成讲解与练习；也可回到「错误归因」标签，点某条错误的「讲解」按钮定向生成
      </div>
    </template>
  </div>
</template>

<style scoped>
.explain-target { font-size: 12.5px; color: var(--text-2); }
</style>
