<script setup lang="ts">
import { onMounted, watch } from 'vue'
import { errorTypeLabel } from '../lib/labels'
import { useGecStore } from '../stores/gec'
import { LButton, LEmpty, LSelect } from '../ui'
import { toast } from '../ui/toast'

const store = useGecStore()

/** 讲解页签的「忽略」（R02→R03）：讲解是阅读面，出口只有换一句与忽略；
 *  忽略作用于当前讲解对象（analyzed 下标），不发反馈、回错误归因页签看折叠 chip */
function ignoreExplain() {
  const idx = store.explainIndex
  if (!store.analyzed[idx] || store.ignored[idx]) return
  store.ignoreError(idx)
  store.requestShowAttribution()
  toast(
    '已忽略 1 条：不计入待处理，也不会作为负样本进训练飞轮',
    'info',
    { label: '撤销', onClick: () => store.restoreError(idx) },
    6000,
  )
}

const langChoices = [
  { value: '', label: '跟随检测语言' },
  { value: '中文', label: '中文' },
  { value: 'English', label: 'English' },
  { value: '日本語', label: '日本語' },
  { value: '한국어', label: '한국어' },
  { value: 'Español', label: 'Español' },
  { value: 'Français', label: 'Français' },
  { value: 'Deutsch', label: 'Deutsch' },
  { value: 'Русский', label: 'Русский' },
]

/** 逐错误讲解联动：归因卡片「讲解」按钮递增 explainNonce。
 *  - 本组件已挂载（AI 讲解曾打开过）：watch 直接触发生成；
 *  - 未挂载（曾未打开）：ResultsPanel 先切页签 → 本组件挂载，onMounted 补触发。 */
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
  <LEmpty
    v-if="store.analyzed.length === 0"
    text="暂无讲解"
    :hint="
      store.detectCompleted
        ? '检测完成，未发现需要讲解的错误。'
        : store.llmEnabled
          ? '先在左侧完成「检测 + 归因」，再生成贴合你语言水平的个性化讲解。'
          : 'AI 复查还没配置：到「设置」配置好 AI 复查后，先在左侧完成「检测 + 归因」。'
    "
  />
  <div v-else class="llm-panel">
    <p v-if="store.llm.fallback" class="llm-warn" role="status">
      LLM 讲解服务未配置（服务端未设置 LLM_API_KEY），已回退到决策表静态补救建议
    </p>
    <template v-else>
      <div class="llm-toolbar">
        <span class="hint">讲解语言</span>
        <LSelect
          :model-value="store.targetLanguage"
          :options="langChoices"
          class="lang-select"
          @update:model-value="store.targetLanguage = String($event)"
        />
        <span v-if="store.analyzed[store.explainIndex]" class="explain-target">
          讲解对象：{{ store.analyzed[store.explainIndex].error.original }}
          <template v-if="store.analyzed[store.explainIndex].error.correction">
            → {{ store.analyzed[store.explainIndex].error.correction }}
          </template>
          （{{ errorTypeLabel(store.analyzed[store.explainIndex].error.error_type) }}）
        </span>
        <LButton
          variant="primary"
          :loading="store.llm.loading"
          @click="store.explainStream(store.explainIndex)"
        >
          {{ store.llm.loading ? '生成中…' : '生成个性化讲解' }}
        </LButton>
        <!-- 忽略当前讲解对象；已反馈（采纳/放弃落定）的条目不出忽略钮 -->
        <LButton
          v-if="store.analyzed[store.explainIndex] && !store.feedbackDoneByIndex[store.explainIndex]"
          variant="ghost"
          size="sm"
          @click="ignoreExplain"
          >忽略</LButton
        >
        <span v-if="store.llm.meta" class="trace">
          模型 {{ store.llm.meta.model }} · 模板 {{ store.llm.meta.template }}
          (v{{ store.llm.meta.template_version }})
        </span>
      </div>
      <div v-if="store.llm.loading && !store.llm.explanation" class="llm-box generating">
        <span class="llm-spin" aria-hidden="true"></span>
        <span>正在生成个性化讲解…</span>
      </div>
      <div v-if="store.llm.explanation" class="llm-box">
        <div class="llm-box-title">个性化讲解</div>
        <p class="llm-text">{{ store.llm.explanation }}</p>
      </div>
      <div v-if="store.llm.exercises" class="llm-box">
        <div class="llm-box-title">个性化练习</div>
        <p class="llm-text">{{ store.llm.exercises }}</p>
      </div>
      <p v-if="!store.llm.explanation && !store.llm.loading" class="hint">
        点击「生成个性化讲解」，会按你的语言水平自动生成讲解与练习；也可回到「错误归因」，点某条错误的「讲解」按钮定向生成。
      </p>
    </template>
  </div>
</template>

<style scoped>
.llm-panel {
  display: flex;
  flex-direction: column;
  gap: var(--space-3);
}
.llm-warn {
  margin: 0;
  padding: var(--space-2) var(--space-3);
  background: var(--warn-soft);
  border: 1px solid var(--warn-line);
  border-radius: var(--radius-md);
  color: var(--warn-ink);
  font-size: var(--fs-sm);
  line-height: var(--lh-body);
}
.llm-toolbar {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  flex-wrap: wrap;
}
.lang-select {
  min-width: 150px;
}
.hint {
  font-size: var(--fs-sm);
  color: var(--muted);
  line-height: var(--lh-body);
}
.explain-target {
  font-size: var(--fs-sm);
  color: var(--muted);
}
.trace {
  font-size: var(--fs-sm);
  color: var(--muted);
  font-family: var(--font-mono);
}
.llm-box {
  border: 1px solid var(--border);
  border-radius: var(--radius-md);
  background: var(--surface);
}
.llm-box.generating {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  padding: var(--space-3);
  color: var(--muted);
  font-size: var(--fs-sm);
}
.llm-spin {
  width: 12px;
  height: 12px;
  flex: none;
  border: 2px solid color-mix(in srgb, var(--muted) 30%, transparent);
  border-top-color: var(--muted);
  border-radius: 50%;
  animation: l-spin 0.7s linear infinite;
}
.llm-box-title {
  padding: var(--space-2) var(--space-3);
  border-bottom: 1px solid var(--border);
  font-weight: 600;
  font-size: var(--fs-md);
}
.llm-text {
  margin: 0;
  padding: var(--space-3);
  white-space: pre-wrap;
  line-height: var(--lh-body);
}
</style>
