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
 *  ①②②.5 状态来自服务端设置；③ 按「响应级别」动态显示（逻辑见 lib/pipeline.ts，有单测） */
const pipelineLayers = computed(() =>
  computePipelineLayers({
    level: store.level,
    decisionEnabled: store.decisionEnabled,
    llmEnabled: store.llmEnabled,
    embeddingConfigured: store.embeddingConfigured,
    l3Enabled: store.l3Enabled,
  }),
)

// L2 门控语义（P0.5 重绑）：L3 特征模型启用时门控是特征模型（弃权 → LLM 兜底）；
// L3 未启用才回退旧语义（决策模型弃权 → LLM 兜底）。标签与警告随之动态化，
// 避免「L2 = 决策模型弃权」的旧文案误导（T2-4 术语修正）。
const l2Label = computed(() =>
  store.l3Enabled
    ? 'L2：+ 特征模型弃权时 LLM 兜底'
    : 'L2：+ 决策模型弃权时 LLM 兜底（L3 未启用）',
)
const l2Warn = computed(() => {
  if (store.level !== 2) return ''
  if (store.l3Enabled) {
    return store.llmEnabled
      ? ''
      : 'LLM 未配置：L2 的「特征模型弃权 → LLM 兜底」不可用（特征模型仍会高置信定案）。可在「设置 → LLM 参数」配置。'
  }
  return store.decisionEnabled
    ? ''
    : '特征模型与决策模型均未启用：L2 的「弃权 → LLM 兜底」不生效，本次等效 L1。可在「设置 → L3 特征模型」或「设置 → 决策模型」开启后即时生效。'
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
        content="强制 LLM 兜底检测：规则与决策模型完成后，无条件对全文跑一次 LLM 兜底检查，合并检出规则漏报的错误（force_llm；需已配置 LLM）"
        placement="top"
      >
        <el-button
          type="warning"
          plain
          :loading="store.isDetectingLlm"
          :disabled="!store.text.trim() || (store.loading && !store.isDetectingLlm)"
          @click="store.detectLlm()"
        >
          漏报检测
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
        <el-form-item label="响应级别">
          <el-select v-model="store.level" style="width: 100%">
            <el-option :value="1" label="L1：规则 + 决策模型">
              <span class="level-opt">L1：规则 + 决策模型</span>
              <el-tag
                :type="store.decisionEnabled ? 'success' : 'warning'"
                size="small"
                class="level-tag"
              >
                {{ store.decisionEnabled ? '决策模型已启用' : '决策模型未启用' }}
              </el-tag>
            </el-option>
            <el-option :value="2" :label="l2Label">
              <span class="level-opt">{{ l2Label }}</span>
              <el-tag
                :type="store.l3Enabled ? (store.llmEnabled ? 'success' : 'warning') : store.decisionEnabled ? 'success' : 'warning'"
                size="small"
                class="level-tag"
              >
                {{
                  store.l3Enabled
                    ? store.llmEnabled
                      ? '特征模型门控 · LLM 已配置'
                      : '特征模型门控 · LLM 未配置'
                    : store.decisionEnabled
                      ? '决策模型已启用'
                      : '决策模型未启用'
                }}
              </el-tag>
            </el-option>
            <el-option :value="3" label="L3：+ 特征模型定案，LLM 收长尾">
              <span class="level-opt">L3：+ 特征模型定案，LLM 收长尾</span>
              <el-tag
                :type="store.l3Enabled ? 'success' : 'warning'"
                size="small"
                class="level-tag"
              >
                {{ store.l3Enabled ? '特征模型已启用' : '特征模型未启用' }}
              </el-tag>
            </el-option>
          </el-select>
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
/* 「响应级别」L2 选项：左侧文案 + 右侧决策模型启用状态标签 */
.level-opt { float: left; }
.level-tag { float: right; margin-top: 2px; }
.level-warn {
  margin-top: 6px; padding: 6px 10px;
  background: var(--warn-soft); border: 1px solid var(--warn-line); border-radius: 6px;
  font-size: 12.5px; color: var(--warn-ink); line-height: 1.6;
}
</style>
