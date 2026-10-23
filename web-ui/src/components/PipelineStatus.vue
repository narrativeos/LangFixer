<script setup lang="ts">
/**
 * 系统状态总览（T1-1 引导增强）
 *
 * 检测页顶部的横向状态条：一眼看清检测管线各层「可用 / 未启用 / 未配置」，
 * 每项可点击直达「设置」对应卡片；右侧「测试全部」一键探测全部外部服务。
 *
 * 与 Editor 卡片内的「检测管线」提示互补：
 * - 本条 = 系统能力总览（与响应级别无关）；
 * - Editor 行 = 当前所选级别下「本次将走哪些层」。
 */
import { computed, ref } from 'vue'
import { ElMessage } from 'element-plus'
import { errMsg } from '../lib/err'
import { api } from '../api/client'
import { useGecStore } from '../stores/gec'
import type { SettingsTestResult } from '../types'

const store = useGecStore()

interface StatusItem {
  key: string
  label: string
  /** ok = 可用 / off = 未启用（功能存在但开关关闭）/ missing = 未配置（缺地址 / Key） */
  state: 'ok' | 'off' | 'missing'
  hint: string
  /** 点击跳设置页后滚动定位的卡片锚点（null = 仅跳设置页） */
  anchor: string | null
}

const items = computed<StatusItem[]>(() => [
  {
    key: 'rule',
    label: '① 本地规则',
    state: 'ok',
    hint: 'ZEN 决策表 + 本地词表，进程内运行，始终可用',
    anchor: 'dirs',
  },
  {
    key: 'decision',
    label: '② 决策模型',
    state: store.decisionEnabled ? 'ok' : 'off',
    hint: store.decisionEnabled
      ? 'vegaml 决策层已启用'
      : '未启用（P0.5 起默认关闭，opt-in）：设置 → 决策模型 打开「启用决策层」',
    anchor: 'decision',
  },
  {
    key: 'embedding',
    label: '②.5 知识召回',
    state: store.embeddingConfigured ? 'ok' : 'missing',
    hint: store.embeddingConfigured
      ? '嵌入引擎已配置，知识向量召回生效'
      : '未配置嵌入引擎：知识向量召回静默跳过（不影响其他层）；设置 → 嵌入引擎',
    anchor: 'embedding',
  },
  {
    key: 'l3',
    label: '③-F 特征模型',
    state: store.l3Enabled ? 'ok' : 'off',
    hint: store.l3Enabled
      ? 'feature-model 服务已启用：高置信定案，LLM 收缩到长尾'
      : '未启用：L3 回退到 LLM 兜底；设置 → L3 特征模型',
    anchor: 'l3',
  },
  {
    key: 'llm',
    label: '③ LLM 兜底',
    state: store.llmEnabled ? 'ok' : 'missing',
    hint: store.llmEnabled
      ? 'LLM 已配置'
      : '未配置 LLM：L2/L3 兜底与「LLM 个性化讲解」不可用；设置 → LLM 参数',
    anchor: 'llm',
  },
])

const okCount = computed(() => items.value.filter((i) => i.state === 'ok').length)

function goSettings(anchor: string | null) {
  store.requestNav('settings', anchor ?? undefined)
}

const testing = ref(false)
const testResult = ref<SettingsTestResult | null>(null)

async function testAll() {
  testing.value = true
  testResult.value = null
  try {
    const r = await api.testSetting('all')
    testResult.value = r
    ElMessage[r.ok ? 'success' : 'warning'](r.message)
  } catch (e) {
    ElMessage.error(`测试失败：${errMsg(e)}`)
  } finally {
    testing.value = false
  }
}
</script>

<template>
  <div class="pipeline-status" role="status" aria-label="检测管线系统状态">
    <span class="ps-label">系统状态</span>
    <div
      v-for="it in items"
      :key="it.key"
      class="ps-item"
      :class="'ps-' + it.state"
      :title="it.hint"
      role="button"
      tabindex="0"
      @click="goSettings(it.anchor)"
      @keydown.enter="goSettings(it.anchor)"
      @keydown.space.prevent="goSettings(it.anchor)"
    >
      <i class="ps-dot"></i>
      <span class="ps-name">{{ it.label }}</span>
      <span class="ps-state">
        {{ it.state === 'ok' ? '可用' : it.state === 'off' ? '未启用' : '未配置' }}
      </span>
    </div>
    <span class="ps-summary">{{ okCount }}/{{ items.length }} 层可用</span>
    <el-button size="small" :loading="testing" @click="testAll">测试全部</el-button>
    <el-tooltip
      v-if="testResult"
      :content="testResult.message"
      placement="bottom"
      :show-after="200"
    >
      <span class="ps-test-tag" :class="testResult.ok ? 'apply-ok' : 'apply-err'">
        {{ testResult.ok ? '✓ 全部通过' : '✗ 部分未通过' }}
        <span class="ps-latency">（{{ testResult.latency_ms }} ms）</span>
      </span>
    </el-tooltip>
  </div>
</template>

<style scoped>
.pipeline-status {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 7px 12px;
  margin-bottom: 12px;
  background: var(--el-fill-color-lighter, #f7f8fa);
  border: 1px solid var(--el-border-color-lighter, #ebeef5);
  border-radius: 8px;
  flex-wrap: wrap;
}
.ps-label {
  font-size: 12px;
  color: var(--el-text-color-secondary);
  margin-right: 2px;
}
.ps-item {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  padding: 3px 9px;
  border-radius: 12px;
  cursor: pointer;
  font-size: 12.5px;
  border: 1px solid transparent;
  transition: border-color 0.15s;
}
.ps-item:hover { border-color: var(--el-border-color); }
.ps-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  display: inline-block;
}
.ps-ok .ps-dot { background: var(--el-color-success, #67c23a); }
.ps-ok { background: rgba(103, 194, 58, 0.08); }
.ps-off .ps-dot { background: var(--el-text-color-placeholder, #c0c4cc); }
.ps-off { color: var(--el-text-color-secondary); }
.ps-missing .ps-dot { background: var(--el-color-warning, #e6a23c); }
.ps-missing { background: rgba(230, 162, 60, 0.08); }
.ps-state { font-size: 11.5px; color: var(--el-text-color-secondary); }
.ps-summary {
  font-size: 12px;
  color: var(--el-text-color-regular);
  margin-left: auto;
}
.ps-test-tag { font-size: 12px; white-space: nowrap; }
.ps-latency { color: var(--el-text-color-secondary); }
.apply-ok { color: var(--el-color-success, #67c23a); }
.apply-err { color: var(--el-color-danger, #f56c6c); }
</style>
