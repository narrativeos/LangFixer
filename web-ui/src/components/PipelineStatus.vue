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
 *
 * `flat` 形态用于桌面端 header popover（紧凑竖排点行，由 popover 提供容器）。
 */
import { computed, ref } from 'vue'
import { LButton, LDot } from '../ui'
import { toast } from '../ui/toast'
import { errMsg } from '../lib/err'
import { api } from '../api/client'
import { useGecStore } from '../stores/gec'
import { identity } from '../lib/auth'
import { plainLayerLabel } from '../lib/labels'
import type { SettingsTestResult } from '../types'

const store = useGecStore()

/** flat = popover 内嵌形态（桌面端 header chip 点开）；默认 = Web 端整行状态条 */
defineProps<{ flat?: boolean }>()

/**
 * 「测试全部」= 用服务端已配置的地址做一次外部探测，属运营侧动作：网关形态下
 * 评审密钥的可达面是「检测 + 本人历史」（`middleware::auth::review_denied`），
 * 按钮随之收起，免得点出 403 让人以为是产品坏了。状态点本身仍来自 detect 响应，
 * 与身份无关。
 */
const canTest = computed(() => !identity.value.gateway || identity.value.admin)

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
    label: plainLayerLabel('rule'),
    state: 'ok',
    hint: '内置规则与词表，即时运行，始终可用',
    anchor: 'dirs',
  },
  // ② 决策模型待发布层（EVOLUTION.md §13 #10；11-13 注：线不放弃，待更合适的决策模型）：状态条不再列示
  {
    key: 'embedding',
    label: plainLayerLabel('knowledge'),
    state: store.embeddingConfigured ? 'ok' : 'missing',
    hint: store.embeddingConfigured
      ? '易错词对与错例的检索已就绪'
      : '未配置：易错词对的智能检索不会生效（不影响其他检查）；可在设置中配置',
    anchor: 'embedding',
  },
  {
    key: 'l3',
    label: plainLayerLabel('feature'),
    state: store.l3Enabled ? 'ok' : 'off',
    hint: store.l3Enabled
      ? '确定的结果本地先行处理，AI 只复核拿不准的'
      : '未启用：AI 复查范围更大、更慢；可在设置中开启',
    anchor: 'l3',
  },
  {
    key: 'llm',
    label: plainLayerLabel('llm'),
    state: store.llmEnabled ? 'ok' : 'missing',
    hint: store.llmEnabled ? 'AI 复查已就绪' : '未配置：标准 / 深度检查与个性化讲解不可用；可在设置中配置',
    anchor: 'llm',
  },
])

const okCount = computed(() => items.value.filter((i) => i.state === 'ok').length)

const dotTone = (s: StatusItem['state']) => (s === 'ok' ? 'ok' : s === 'off' ? 'muted' : 'warn')
const stateText = (s: StatusItem['state']) => (s === 'ok' ? '可用' : s === 'off' ? '未启用' : '未配置')

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
    toast(r.message, r.ok ? 'ok' : 'warn')
  } catch (e) {
    toast(`测试失败：${errMsg(e)}`, 'danger')
  } finally {
    testing.value = false
  }
}
</script>

<template>
  <div class="pipeline-status" :class="{ 'ps-flat': flat }" role="status" aria-label="检测服务可用情况">
    <span class="ps-label">检测服务</span>
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
      <LDot :tone="dotTone(it.state)" />
      <span class="ps-name">{{ it.label }}</span>
      <span class="ps-state">{{ stateText(it.state) }}</span>
    </div>
    <span class="ps-summary">{{ okCount }}/{{ items.length }} 项可用</span>
    <LButton v-if="canTest" size="sm" :loading="testing" @click="testAll">测试全部</LButton>
    <span
      v-if="testResult"
      class="ps-test-tag"
      :class="testResult.ok ? 'apply-ok' : 'apply-err'"
      :title="testResult.message"
    >
      {{ testResult.ok ? '✓ 全部通过' : '✗ 部分未通过' }}
      <span class="ps-latency">（{{ testResult.latency_ms }} ms）</span>
    </span>
  </div>
</template>

<style scoped>
.pipeline-status {
  display: flex;
  align-items: center;
  gap: var(--space-3);
  padding: var(--space-2) var(--space-3);
  margin-bottom: var(--space-3);
  background: var(--surface-2);
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  flex-wrap: wrap;
  font-size: var(--fs-ui);
}
.ps-label {
  font-size: var(--fs-sm);
  color: var(--muted);
  margin-right: var(--space-1);
}
.ps-item {
  display: inline-flex;
  align-items: center;
  gap: var(--space-1);
  padding: var(--space-1) var(--space-2);
  border-radius: var(--space-3);
  cursor: pointer;
  font-size: var(--fs-sm);
  border: 1px solid transparent;
  transition: border-color 0.15s;
}
.ps-item:hover {
  border-color: var(--border);
}
.ps-off {
  color: var(--text-2);
}
.ps-state {
  font-size: var(--fs-xs);
  color: var(--muted);
}
.ps-summary {
  font-size: var(--fs-sm);
  color: var(--text-2);
  margin-left: auto;
}
.ps-test-tag {
  font-size: var(--fs-sm);
  white-space: nowrap;
}
.ps-latency {
  color: var(--muted);
}
/* 红色只用于「未通过」这一种错误语义 */
.apply-ok {
  color: var(--ok);
}
.apply-err {
  color: var(--danger);
}
/* popover 内嵌形态：紧凑竖排点行（LDot + 标签），宽度按 popover ~300px 收窄 */
.ps-flat {
  display: flex;
  flex-direction: column;
  align-items: stretch;
  gap: var(--space-1);
  background: transparent;
  border: none;
  margin: 0;
  padding: var(--space-1);
  min-width: 260px; /* popover 约 300px，点行需要稳定可读宽度 */
}
.ps-flat .ps-label {
  margin: 0 0 var(--space-1);
}
.ps-flat .ps-item {
  width: 100%;
  justify-content: flex-start;
}
.ps-flat .ps-state {
  margin-left: auto;
}
.ps-flat .ps-summary {
  margin: var(--space-1) 0 0;
}
</style>
