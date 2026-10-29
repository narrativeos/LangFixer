<script setup lang="ts">
import { computed, watch } from 'vue'
import { randomSessionId } from '../lib/format'
import { computePipelineLayers } from '../lib/pipeline'
import { guideFor } from '../lib/guide'
import { isTauri } from '../api/client'
import { useGecStore } from '../stores/gec'

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

/** 正文管线提示：预处理行跟随卡片标题行选中的语言 */
const activeLang = computed(
  () => store.displayLangs.find((l) => l.lang === store.lang) ?? store.displayLangs[0],
)

/** 检测管线四层（① 本地 ZEN 规则 → ② 决策模型 → ②.5 知识召回 → ③ LLM 兜底）：
 *  ①②②.5 状态来自服务端设置；③ 按「检查模式」（store.level，内部 level 1/2/3）动态显示
 *  （逻辑见 lib/pipeline.ts，有单测） */
const pipelineLayers = computed(() =>
  computePipelineLayers({
    level: store.level,
    decisionEnabled: store.decisionEnabled,
    llmEnabled: store.llmEnabled,
    embeddingConfigured: store.embeddingConfigured,
    l3Enabled: store.l3Enabled,
  }),
)

// 检查模式用户语言命名（Phase 0 术语翻译）：内部级联细节（规则 / 决策模型 /
// L3 特征模型 / LLM）只出现在「检测管线」状态条、选项右侧状态标签与设置页，
// 选项文案本身不再暴露 L1/L2/L3 内部编号。
const modeLabels: Record<number, string> = {
  1: '快速检查（即时，本地规则）',
  2: '标准检查（推荐）',
  3: '深度扫描（较慢，召回更高）',
}
const modeHint = computed(() =>
  store.level === 1
    ? '只跑本地规则引擎，即时完成，离线完全可用。'
    : store.level === 2
      ? '本地模型先把高置信结果定案，仅不确定的句子交给 AI 复查服务（LLM）复核；需已配置 LLM。'
      : '对全文跑一遍 AI 复查（LLM），漏报召回最高，但较慢；本地模型可先把高置信结果定案以节省时间。',
)
// 标准检查门控语义（P0.5 重绑）：L3 特征模型启用时门控是特征模型（弃权 → LLM 复查）；
// L3 未启用才回退旧语义（决策模型弃权 → LLM 复查）。警告文案随之动态化。
const l2Warn = computed(() => {
  if (store.level !== 2) return ''
  if (store.l3Enabled) {
    return store.llmEnabled
      ? ''
      : 'AI 复查服务（LLM）未配置：灰区句子无法复核，本次等效快速检查。可在「设置 → LLM 参数」配置。'
  }
  return store.decisionEnabled
    ? ''
    : '快速定案的本地模型均未启用：AI 复查兜底不会触发，本次等效快速检查。可在「设置 → L3 特征模型」或「设置 → 决策模型」开启后即时生效。'
})
</script>

<template>
  <el-form label-position="top">
    <el-form-item>
      <template #label>
        <span>待检测文本</span>
        <span class="char-count">{{ store.text.length }} 字</span>
      </template>
      <el-input
        v-model="store.text"
        type="textarea"
        :rows="8"
        placeholder="在此粘贴或输入待检测文本… 例如：他昨天报复很大。"
      />
    </el-form-item>

    <div class="engine-hint">
      <div class="pipeline-row">
        <span class="hint-label">检测管线</span>
        <el-tag
          v-for="l in pipelineLayers"
          :key="l.no"
          size="small"
          :type="l.type"
          effect="plain"
        >
          {{ l.label }}
        </el-tag>
      </div>
      <div class="preprocess-row">
        <span class="hint-label">预处理</span>
        <span>{{ activeLang.nlp_service }}</span>
      </div>
    </div>

    <div class="actions">
      <el-button
        type="primary"
        :loading="store.isAnalyzing"
        :disabled="!store.text.trim() || (store.loading && !store.isAnalyzing)"
        @click="store.analyze(store.sessionId || undefined)"
      >
        {{ store.isAnalyzing ? '检测中…' : '检测 + 归因' }}
      </el-button>
      <el-button :disabled="store.loading || !store.text.trim()" @click="store.detect()">
        仅检测
      </el-button>
      <el-tooltip
        content="深度重查：检测完成后，再对全文跑一遍 AI 复查（LLM），找回规则漏掉的错误（较慢；需已配置 LLM）"
        placement="top"
      >
        <el-button
          type="warning"
          plain
          :loading="store.isDetectingLlm"
          :disabled="!store.text.trim() || (store.loading && !store.isDetectingLlm)"
          @click="store.detectLlm()"
        >
          深度重查
        </el-button>
      </el-tooltip>
      <el-tooltip
        v-if="store.loading"
        :content="isTauri ? '取消：本次结果将在完成后被丢弃（桌面端无法中断后台执行）' : '取消本次检测请求'"
        placement="top"
      >
        <el-button @click="store.cancelDetect()">取消</el-button>
      </el-tooltip>
    </div>

    <div class="options-row">
      <div class="options-col">
        <el-form-item label="会话 ID（可选）">
          <div class="session-id-row">
            <el-input v-model="store.sessionId" placeholder="如：s_a1b2c3d4" />
            <el-button @click="store.sessionId = randomSessionId()">随机生成</el-button>
          </div>
        </el-form-item>
      </div>
      <div class="options-col">
        <el-form-item label="检查模式">
          <el-select v-model="store.level" style="width: 100%">
            <el-option :value="1" :label="modeLabels[1]">
              <span class="level-opt">{{ modeLabels[1] }}</span>
              <el-tag
                :type="store.decisionEnabled ? 'success' : 'info'"
                size="small"
                class="level-tag"
              >
                {{ store.decisionEnabled ? '含决策模型' : '纯本地规则' }}
              </el-tag>
            </el-option>
            <el-option :value="2" :label="modeLabels[2]">
              <span class="level-opt">{{ modeLabels[2] }}</span>
              <el-tag
                :type="store.llmEnabled ? 'success' : 'warning'"
                size="small"
                class="level-tag"
              >
                {{ store.llmEnabled ? 'AI 复查已配置' : 'AI 复查未配置' }}
              </el-tag>
            </el-option>
            <el-option :value="3" :label="modeLabels[3]">
              <span class="level-opt">{{ modeLabels[3] }}</span>
              <el-tag
                :type="store.l3Enabled ? 'success' : 'info'"
                size="small"
                class="level-tag"
              >
                {{ store.l3Enabled ? '快速定案已启用' : '全量 AI 复查' }}
              </el-tag>
            </el-option>
          </el-select>
          <div class="mode-hint">{{ modeHint }}</div>
          <div v-if="l2Warn" class="level-warn">{{ l2Warn }}</div>
        </el-form-item>
      </div>
    </div>
  </el-form>

  <el-alert
    v-if="store.errorMsg"
    type="error"
    :closable="false"
    show-icon
    :title="store.errorMsg"
    class="err-alert"
  >
    <!-- 修复指引（T1-5）：常见服务类错误附「如何修复」，可一键跳设置；最近一次检测可重试 -->
    <div v-if="guideFor(store.errorMsg)" class="err-guide">
      如何修复：{{ guideFor(store.errorMsg)!.text }}
      <el-button
        v-if="guideFor(store.errorMsg)!.view === 'settings'"
        link
        type="primary"
        size="small"
        @click="store.requestNav('settings')"
      >
        去设置
      </el-button>
    </div>
    <div v-if="store.lastDetectKind" class="err-guide">
      <el-button size="small" :disabled="store.loading" @click="store.retryLast()">重试</el-button>
    </div>
  </el-alert>
</template>

<style scoped>
.err-alert { margin-top: 4px; }
.err-guide { margin-top: 6px; font-size: 12.5px; line-height: 1.7; color: var(--el-text-color-regular); }
.session-id-row { display: flex; gap: 8px; align-items: center; width: 100%; }
.session-id-row :deep(.el-input) { flex: 1; min-width: 0; }
/* 「检查模式」选项：左侧用户语言文案 + 右侧能力状态标签（级联细节只在此与管线状态条出现） */
.level-opt { float: left; }
.level-tag { float: right; margin-top: 2px; }
.mode-hint {
  margin-top: 6px; font-size: 12.5px; line-height: 1.6;
  color: var(--el-text-color-secondary);
}
.level-warn {
  margin-top: 6px; padding: 6px 10px;
  background: var(--warn-soft); border: 1px solid var(--warn-line); border-radius: 6px;
  font-size: 12.5px; color: var(--warn-ink); line-height: 1.6;
}
</style>
