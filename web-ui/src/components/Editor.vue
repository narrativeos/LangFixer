<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { randomSessionId } from '../lib/format'
import { computePipelineLayers } from '../lib/pipeline'
import { guideFor } from '../lib/guide'
import { buildSegments } from '../lib/highlight'
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

// ── 编辑态原文同步高亮：textarea 背后的镜像底纹层（backdrop）──────────────
// textarea 本身不能画背景，故用绝对定位镜像层在相同位置铺半透明色块：
// 文字由 textarea 绘制，镜像层文字透明只出底纹。镜像必须与 textarea 像素级
// 对齐（字体/padding/换行逐项从计算样式复制），错位比不高亮更糟；切段逻辑
// 与校样态共用 lib/highlight，两态位置必然一致。
const areaRef = ref<HTMLTextAreaElement | null>(null)
const backdropRef = ref<HTMLDivElement | null>(null)
const innerRef = ref<HTMLDivElement | null>(null)

const segments = computed(() => buildSegments(store.text, store.errors, store.resultsStale))
// textarea 末尾换行在 textarea 内占一行，pre-wrap 镜像会丢弃行尾换行 → 补一个对齐行数
const endsWithNewline = computed(() => store.text.endsWith('\n'))

/** 逐项复制影响文本排版的计算样式（textarea 默认 UA 字体与页面字体不同，必须照抄） */
const MIRROR_PROPS = [
  'fontFamily', 'fontStyle', 'fontVariant', 'fontWeight', 'fontStretch', 'fontSize',
  'lineHeight', 'letterSpacing', 'wordSpacing', 'textTransform', 'textIndent',
  'whiteSpace', 'wordBreak', 'overflowWrap', 'tabSize',
  'paddingTop', 'paddingRight', 'paddingBottom', 'paddingLeft',
  'borderTopWidth', 'borderRightWidth', 'borderBottomWidth', 'borderLeftWidth',
] as const

function syncMirror() {
  const ta = areaRef.value
  const inner = innerRef.value
  if (!ta || !inner) return
  const cs = getComputedStyle(ta)
  for (const p of MIRROR_PROPS) inner.style[p] = cs[p as 'fontSize']
  // 宽＝ textarea 内容宽（clientWidth 已排滚动条），镜像换行才会一致
  inner.style.boxSizing = 'border-box'
  inner.style.width = `${ta.clientWidth}px`
}

function syncScroll() {
  const ta = areaRef.value
  const inner = innerRef.value
  if (!ta || !inner) return
  inner.style.transform = `translate(${-ta.scrollLeft}px, ${-ta.scrollTop}px)`
}

// 列表→原文正向联动：点检测结果卡片 → 镜像底纹闪烁 + 滚动居中 + 选中错误区间
const flashIdx = ref(-1)
let flashTimer: ReturnType<typeof setTimeout> | null = null
watch(
  () => store.focusErrorNonce,
  (n) => {
    if (n === 0) return
    const idx = store.focusErrorIndex
    nextTick(() => {
      // 标签页/画布态刚切换时等一帧，确保镜像内 DOM 就绪
      requestAnimationFrame(() => {
        const ta = areaRef.value
        const bd = backdropRef.value
        // 画布在「校样」态时编辑区被 v-show 隐藏，交由 PreviewPanel 处理
        if (!ta || !bd || !ta.offsetParent) return
        const el = bd.querySelector<HTMLElement>(`[data-idx="${idx}"]`)
        if (!el) return
        ta.scrollTop = Math.max(0, el.offsetTop - Math.round((ta.clientHeight - el.offsetHeight) / 2))
        // 整页滚动布局（非 desktop）下顺带把画布滚进视口
        ta.scrollIntoView({ behavior: 'smooth', block: 'nearest' })
        syncScroll()
        const seg = segments.value.find((s) => s.mark?.index === idx)
        if (seg?.mark) {
          ta.focus({ preventScroll: true })
          ta.setSelectionRange(seg.mark.start, seg.mark.end)
        }
        flashIdx.value = idx
        if (flashTimer) clearTimeout(flashTimer)
        flashTimer = setTimeout(() => {
          if (flashIdx.value === idx) flashIdx.value = -1
        }, 1600)
      })
    })
  },
)

let ro: ResizeObserver | null = null
onMounted(() => {
  syncMirror()
  syncScroll()
  areaRef.value?.addEventListener('scroll', syncScroll)
  // textarea 可拖拽调高 / 滚动条出现都会改变内容宽，镜像尺寸跟着重算
  if (areaRef.value && 'ResizeObserver' in window) {
    ro = new ResizeObserver(() => {
      syncMirror()
      syncScroll()
    })
    ro.observe(areaRef.value)
  }
})
onBeforeUnmount(() => {
  areaRef.value?.removeEventListener('scroll', syncScroll)
  ro?.disconnect()
  if (flashTimer) clearTimeout(flashTimer)
})
// 文本被程序替换（采纳/替换修正全文）时 textarea 不一定发 scroll 事件，主动同步一次
watch(segments, () => nextTick(syncScroll))
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

    <div class="l-editor-mirror">
      <div ref="backdropRef" class="l-editor-backdrop" aria-hidden="true">
        <div ref="innerRef" class="l-editor-mirror-inner">
          <template v-for="(seg, i) in segments" :key="i">
            <span
              v-if="seg.mark"
              :data-idx="seg.mark.index"
              :class="{ 'is-flash': flashIdx === seg.mark.index }"
            >{{ seg.text }}</span>
            <template v-else>{{ seg.text }}</template>
          </template>
          <template v-if="endsWithNewline">{{ '\n' }}</template>
        </div>
      </div>
      <textarea
        ref="areaRef"
        v-model="store.text"
        class="l-canvas-text l-editor-area"
        rows="10"
        placeholder="在此粘贴或输入待检测文本… 例如：他昨天报复很大。"
        aria-label="待检测文本"
      />
    </div>

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
/* 镜像底纹层：绝对定位在 textarea 后面，文字透明只出半透明色块；
   尺寸/字体由脚本从 textarea 计算样式复制，保证逐行对齐 */
.l-editor-mirror {
  position: relative;
}
.l-editor-backdrop {
  position: absolute;
  inset: 0;
  overflow: hidden;
  pointer-events: none;
  z-index: 0;
}
.l-editor-mirror-inner {
  position: relative;
  color: transparent;
}
.l-editor-area {
  position: relative;
  z-index: 1;
}
.l-editor-mirror-inner span[data-idx] {
  background: color-mix(in srgb, var(--danger) 12%, transparent);
  border-radius: 2px;
}
.l-editor-mirror-inner span.is-flash {
  animation: locate-flash 1.6s ease;
}
@keyframes locate-flash {
  0%, 55% { background: color-mix(in srgb, var(--accent) 30%, transparent); }
  100% { background: color-mix(in srgb, var(--danger) 12%, transparent); }
}
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
