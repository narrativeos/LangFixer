<script setup lang="ts">
import { computed, watch } from 'vue'
import { randomSessionId } from '../lib/format'
import { computePipelineLayers } from '../lib/pipeline'
import { guideFor } from '../lib/guide'
import { isTauri } from '../api/client'
import { useGecStore } from '../stores/gec'
import { LButton, LInput, LPopover, LTag } from '../ui'

const store = useGecStore()

// 文本变化时重置检测完成标记，使空状态回到"尚未检测"
let prevText = store.text
watch(
  () => store.text,
  (newText) => {
    if (newText !== prevText) {
      store.detectCompleted = false
      prevText = newText
    }
  },
)

/** 正文管线提示：预处理行跟随工具栏选中的语言 */
const activeLang = computed(
  () => store.displayLangs.find((l) => l.lang === store.lang) ?? store.displayLangs[0],
)

/** 示例文本：一键「填入 + 检测」。各覆盖一类典型错误，短句、无需上下文即可看懂 */
const samples = [
  { label: '错别字', text: '他从小就立志要干一番大报复。' },
  { label: '多字少字', text: '我昨天在图书管看了一下午书。' },
  { label: '语序颠倒', text: '他把作业没有按时交给老师。' },
]
function useSample(s: { text: string }) {
  // 可反悔：覆盖非空原文前存快照，⌥⌘Z（或菜单「撤销全部采纳」）可找回
  if (store.text.trim() && store.text !== s.text) store.applySnapshot = store.text
  store.text = s.text
  store.detect()
}

/** 检测管线四层（详情 popover 内展示，逻辑见 lib/pipeline.ts，有单测） */
const pipelineLayers = computed(() =>
  computePipelineLayers({
    level: store.level,
    decisionEnabled: store.decisionEnabled,
    llmEnabled: store.llmEnabled,
    embeddingConfigured: store.embeddingConfigured,
    l3Enabled: store.l3Enabled,
  }),
)
const layerTagTone: Record<string, 'neutral' | 'ok' | 'warn'> = {
  success: 'ok',
  warning: 'warn',
  info: 'neutral',
}

/** 主界面「检测方式」一句话摘要（人话；细节见「详情」） */
const plainPipelineText = computed(() => {
  if (!store.llmEnabled && store.level > 1) return '本地检查（AI 复查未配置）'
  if (store.level === 1) return '本地检查，即时完成'
  if (store.level === 2)
    return store.l3Enabled || store.decisionEnabled
      ? '本地快速定案 + AI 复查拿不准的句子'
      : '本地检查 + AI 复查'
  return store.l3Enabled || store.decisionEnabled
    ? '本地快速定案 + AI 全文复查'
    : 'AI 全文复查'
})
// 标准检查门控语义（P0.5 重绑）：能力缺口要说清结果与出路；深度选择在工具栏，此处只补缺口说明
const l2Warn = computed(() => {
  if (store.level !== 2) return ''
  if (store.l3Enabled || store.decisionEnabled) {
    return store.llmEnabled
      ? ''
      : 'AI 复查还没配置：拿不准的句子没法复核，这次相当于快速检查。可以在设置里配置。'
  }
  return store.llmEnabled
    ? 'AI 复查这次不会触发，相当于快速检查。可以在设置里开启快速定案。'
    : 'AI 复查与快速定案都还没配置，这次相当于快速检查。可以在设置里配置。'
})
</script>

<template>
  <div class="l-editor">
    <div class="editor-meta">
      <span>{{ store.text.length }} 字</span>
      <span class="sample-links">
        <span class="hint-label">试一试</span>
        <button
          v-for="s in samples"
          :key="s.label"
          type="button"
          class="l-link"
          :disabled="store.loading"
          @click="useSample(s)"
        >
          {{ s.label }}
        </button>
      </span>
    </div>

    <textarea
      v-model="store.text"
      class="l-canvas-text l-editor-area"
      rows="10"
      placeholder="在此粘贴或输入待检测文本… 例如：他昨天报复很大。"
      aria-label="待检测文本"
    />

    <div class="editor-actions">
      <LButton
        variant="primary"
        :loading="store.isAnalyzing"
        :disabled="!store.text.trim() || (store.loading && !store.isAnalyzing)"
        :title="isTauri ? '快捷键 ⌘Enter' : '快捷键 Ctrl+Enter'"
        @click="store.analyze(store.sessionId || undefined)"
      >
        {{ store.isAnalyzing ? '检测中…' : '检测 + 归因' }}
      </LButton>
      <LButton :disabled="store.loading || !store.text.trim()" @click="store.detect()">仅检测</LButton>
      <LButton
        variant="ghost"
        :loading="store.isDetectingLlm"
        :disabled="!store.text.trim() || (store.loading && !store.isDetectingLlm)"
        title="深度重查：检测完成后，再对全文跑一遍 AI 复查（LLM），找回规则漏掉的错误（较慢；需已配置 LLM）"
        @click="store.detectLlm()"
      >
        深度重查
      </LButton>
      <LButton
        v-if="store.loading"
        variant="ghost"
        :title="isTauri ? '取消：本次结果将在完成后被丢弃（桌面端无法中断后台执行）' : '取消本次检测请求'"
        @click="store.cancelDetect()"
      >
        取消
      </LButton>
      <span class="l-spacer"></span>
      <div class="session-id-row">
        <span class="hint-label">会话</span>
        <LInput v-model="store.sessionId" class="session-input" placeholder="会话 ID（可选）" />
        <LButton variant="ghost" size="sm" @click="store.sessionId = randomSessionId()">随机</LButton>
      </div>
    </div>

    <p v-if="l2Warn" class="editor-warn" role="status">{{ l2Warn }}</p>

    <!-- 引擎信息（信息减负）：主界面只留一句人话「检测方式」；层级标签与预处理
         引擎等内部细节收进「详情」popover（渐进披露，运营/排障可查） -->
    <div class="engine-hint">
      <span class="hint-label">检测方式</span>
      <span>{{ plainPipelineText }}</span>
      <LPopover align="start">
        <template #trigger="{ toggle }">
          <button type="button" class="l-link" aria-label="查看检测管线细节" @click="toggle()">详情</button>
        </template>
        <div class="pipeline-detail">
          <LTag v-for="l in pipelineLayers" :key="l.no" :tone="layerTagTone[l.type] ?? 'neutral'">
            {{ l.label }}
          </LTag>
          <div class="preprocess-row">
            <span class="hint-label">预处理</span>
            <span>{{ activeLang.nlp_service }}</span>
          </div>
        </div>
      </LPopover>
    </div>

    <div v-if="store.errorMsg" class="editor-error" role="alert">
      <div class="editor-error-title">{{ store.errorMsg }}</div>
      <!-- 修复指引（T1-5）：常见服务类错误附「如何修复」，可一键跳设置；最近一次检测可重试 -->
      <div v-if="guideFor(store.errorMsg)" class="editor-error-guide">
        如何修复：{{ guideFor(store.errorMsg)!.text }}
        <LButton
          v-if="guideFor(store.errorMsg)!.view === 'settings'"
          variant="ghost"
          size="sm"
          @click="store.requestNav('settings')"
        >
          去设置
        </LButton>
      </div>
      <LButton
        v-if="store.lastDetectKind"
        size="sm"
        :disabled="store.loading"
        @click="store.retryLast()"
      >
        重试
      </LButton>
    </div>
  </div>
</template>

<style scoped>
.l-editor {
  display: flex;
  flex-direction: column;
  gap: var(--space-3);
  min-height: 0;
}
.editor-meta {
  display: flex;
  align-items: center;
  gap: var(--space-4);
  font-size: var(--fs-sm);
  color: var(--muted);
}
.sample-links {
  display: inline-flex;
  align-items: center;
  gap: var(--space-2);
}
.hint-label {
  color: var(--muted);
  font-size: var(--fs-sm);
}
.l-link {
  font: inherit;
  font-size: var(--fs-sm);
  color: var(--accent);
  background: none;
  border: 0;
  padding: 0;
  cursor: pointer;
}
.l-link:disabled {
  color: var(--muted);
  cursor: default;
}
/* 画布即正文:无边框、留白即容器;行长在 .l-canvas-text 内已限 76ch */
.l-editor-area {
  display: block;
  width: 100%;
  max-width: 76ch;
  min-height: 40vh;
  padding: var(--space-4) var(--space-2);
  border: 0;
  border-radius: var(--radius-md);
  background: transparent;
  color: var(--text);
  resize: vertical;
  outline: none;
}
html.desktop .l-editor-area {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  overscroll-behavior: contain;
}
.l-editor-area::placeholder {
  color: var(--muted);
}
.editor-actions {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  flex-wrap: wrap;
}
.session-id-row {
  display: flex;
  align-items: center;
  gap: var(--space-2);
}
.session-input {
  width: 180px;
}
.editor-warn {
  margin: 0;
  max-width: 76ch;
  padding: var(--space-2) var(--space-3);
  background: var(--warn-soft);
  border: 1px solid var(--warn-line);
  border-radius: var(--radius-md);
  font-size: var(--fs-sm);
  color: var(--warn-ink);
  line-height: var(--lh-body);
}
.engine-hint {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  font-size: var(--fs-sm);
  color: var(--muted);
}
.pipeline-detail {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-2);
  font-size: var(--fs-sm);
}
.pipeline-detail .preprocess-row {
  flex: 1 0 100%;
  display: flex;
  gap: var(--space-2);
  color: var(--muted);
}
.editor-error {
  padding: var(--space-3);
  border: 1px solid color-mix(in srgb, var(--danger) 35%, transparent);
  background: color-mix(in srgb, var(--danger) 6%, transparent);
  border-radius: var(--radius-md);
  font-size: var(--fs-sm);
}
.editor-error-title {
  color: var(--danger);
  font-weight: 600;
  line-height: var(--lh-body);
}
.editor-error-guide {
  margin-top: var(--space-2);
  color: var(--text);
  line-height: var(--lh-body);
}
</style>
