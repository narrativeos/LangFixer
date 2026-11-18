<script setup lang="ts">
import { ref, computed, watch, onMounted, onBeforeUnmount } from 'vue'
import { useGecStore } from './stores/gec'
import Editor from './components/Editor.vue'
import TableEditor from './components/TableEditor.vue'
import KnowledgePanel from './components/KnowledgePanel.vue'
import LearnedKnowledgePanel from './components/LearnedKnowledgePanel.vue'
import RulesPanel from './components/RulesPanel.vue'
import CustomWords from './components/CustomWords.vue'
import SettingsPanel from './components/SettingsPanel.vue'
import EvolutionCenter from './components/EvolutionCenter.vue'
import LayerContributions from './components/LayerContributions.vue'
import PipelineStatus from './components/PipelineStatus.vue'
import MediaWorkspace from './components/MediaWorkspace.vue'
import DocJobsPanel from './components/DocJobsPanel.vue'
import LearningPanel from './components/LearningPanel.vue'
import OnboardingDialog from './components/OnboardingDialog.vue'
import AccessPanel from './components/AccessPanel.vue'
import TokenGate from './components/TokenGate.vue'
import WorkbenchView from './views/WorkbenchView.vue'
import { viewAllowed, type AppView } from './lib/caps'
import { ALL_VIEWS, CONSOLE_ORDER, VIEW_DEFS, shellOf } from './lib/views'
import { api, isTauri, tauriListen } from './api/client'
import { gateReady, identity, initGate } from './lib/auth'
import { LButton, LPopover, LSection, LSegmented, LSidebar, LSelect, toast, LToaster } from './ui'
import { modeLabel } from './lib/labels'
import type { LSidebarSection } from './ui/types'

/**
 * 双壳应用骨架(v2):
 * - 工作台壳(学习者/编校者):一体式工具栏(深度分段 + 语言 + 服务状态 chip)+
 *   校样画布与右侧实用面板;文档/载体审校与练习复用同一工具栏。
 * - 运营台壳(平台运营者):侧栏分区(规则/知识/进化/质量透明 + 服务与接入)。
 * 门控(lib/caps 能力位 + 身份)统一收拢到 consoleItemVisible()/viewAllowed()。
 * 说明:learning 扩展默认开;载体审校 = 编校扩展;规则/知识/进化 = 运营扩展
 * (web 形态需 token 解锁);各层贡献在网关形态全员可见;接入=所有人(MCP 配置);
 * 设置页在网关形态只对超管出现。桌面端 ⌘1-7 由原生菜单转发 menu:view:*。 */

const store = useGecStore()

/** 配置面可见性:单机形态本机即用户;网关形态仅超管(服务端同口径) */
const canConfigure = computed(() => !identity.value.gateway || identity.value.admin)
/** 门禁页 / 产品界面二选一(探测期间留白,避免闪屏) */
const showGate = computed(() => gateReady.value && identity.value.locked && !identity.value.authenticated)
const showApp = computed(() => gateReady.value && (!identity.value.locked || identity.value.authenticated))
const inConsole = computed(() => shellOf(activeView.value) === 'console')

/** 运营台侧栏条目与可达性的单一判定(壳/侧栏/深链/菜单共用) */
function consoleItemVisible(v: AppView): boolean {
  if (v === 'rules' || v === 'knowledge' || v === 'evolution') return store.caps.ops
  if (v === 'layers') return identity.value.gateway || store.caps.ops
  if (v === 'access') return !isTauri
  if (v === 'settings') return canConfigure.value
  return true
}
const canConsole = computed(() => CONSOLE_ORDER.some(consoleItemVisible))
const consoleSections = computed<LSidebarSection[]>(() => {
  const ops = CONSOLE_ORDER.filter(
    (v) => consoleItemVisible(v) && v !== 'access' && v !== 'settings',
  )
  const svc = CONSOLE_ORDER.filter((v) => consoleItemVisible(v) && (v === 'access' || v === 'settings'))
  const sections: LSidebarSection[] = []
  if (ops.length)
    sections.push({ title: '知识与规则', items: ops.map((v) => ({ id: v, label: VIEW_DEFS[v].label })) })
  if (svc.length)
    sections.push({
      title: '服务与接入',
      items: svc.map((v) => ({ id: v, label: VIEW_DEFS[v].label })),
    })
  return sections
})
function goConsole(v: AppView) {
  if (consoleItemVisible(v)) {
    activeView.value = v
    return
  }
  if (v === 'settings') {
    activeView.value = consoleItemVisible('access') ? 'access' : 'workspace'
    return
  }
  lockedViewHint.value = v
  activeView.value = 'workspace'
}

/** `?view=` 深链 */
function viewFromUrl(): AppView | null {
  try {
    const v = new URLSearchParams(window.location.search).get('view')
    return ALL_VIEWS.includes(v as AppView) ? (v as AppView) : null
  } catch {
    return null
  }
}

/** 深链与门控校验:未解锁视图不硬跳转,留在工作台并给一次性说明卡 */
const lockedViewHint = ref<AppView | null>(null)
function resolveInitialView(): AppView {
  let v = viewFromUrl() ?? 'workspace'
  if (v === 'settings' && !canConfigure.value) v = 'access'
  if (v === 'learning' && !store.caps.learning) v = 'workspace'
  if (shellOf(v) === 'console' && !consoleItemVisible(v)) {
    if (v === 'settings' || v === 'access') return canConsole.value ? 'access' : 'workspace'
    lockedViewHint.value = v
    return 'workspace'
  }
  if (!viewAllowed(v, store.caps)) {
    lockedViewHint.value = v
    return 'workspace'
  }
  return v
}

const activeView = ref<AppView>(resolveInitialView())

/** 锁定视图说明卡:一键解锁对应场景并进入 */
function unlockScenario() {
  const hint = lockedViewHint.value
  if (!hint) return
  store.setCap(hint === 'media' ? 'proofreading' : hint === 'learning' ? 'learning' : 'ops', true)
  activeView.value = hint
  lockedViewHint.value = null
}
function adjustScenario() {
  const target = consoleItemVisible('settings') ? 'settings' : 'access'
  lockedViewHint.value = null
  goConsole(target)
}

// ── 工具栏:检测深度分段(HIG §3.3)+ 语言 + 服务状态 chip ──
const depthOptions = [1, 2, 3].map((v) => ({ value: v, label: modeLabel(v) }))
const depthNote = computed(() => {
  if (store.level >= 3)
    return store.llmEnabled ? '本地检查定案高置信,长尾交给 AI 复查' : 'AI 复查未启用,等同标准检查'
  if (store.level <= 1) return '本地规则层,离线可用'
  return store.l3Enabled ? '规则层 → 快速定案,把握不足的进入 AI 复查' : '本地检查为主;AI 复查未启用'
})
const langOptions = computed(() =>
  store.displayLangs.map((l: { lang: string; name: string }) => ({ value: l.lang, label: l.name })),
)
function onLang(v: string | number) {
  store.lang = String(v)
  store.clearResults()
}
const psTotal = 4
const psOk = computed(
  () => 1 + (store.embeddingConfigured ? 1 : 0) + (store.l3Enabled ? 1 : 0) + (store.llmEnabled ? 1 : 0),
)

/** 验证通过后才拉数据:网关形态未登录打业务端点会 401 */
function boot() {
  store.loadLanguages()
  store.loadSettings()
  if (store.caps.learning && (!identity.value.gateway || identity.value.admin))
    void store.refreshReviewDue()
  try {
    showOnboarding.value = !localStorage.getItem('langfixer.onboarded.v1')
  } catch {
    showOnboarding.value = true
  }
}

onMounted(async () => {
  await initGate()
  if (showApp.value) boot()
})

// 首次启动引导:只弹一次;工具栏「帮助」可随时重开
const showOnboarding = ref(false)
function finishOnboarding() {
  showOnboarding.value = false
  try {
    localStorage.setItem('langfixer.onboarded.v1', '1')
  } catch {
    /* 忽略 */
  }
}

watch(activeView, (v) => {
  // 运营台条目按门控回退:深链/菜单任何入口都走同一判定
  if (shellOf(v) === 'console' && !consoleItemVisible(v)) {
    if (v === 'settings' || v === 'access') {
      activeView.value = consoleItemVisible('access') ? 'access' : 'workspace'
    } else {
      lockedViewHint.value = v
      activeView.value = 'workspace'
    }
    return
  }
  if (v === 'workspace') void store.refreshEngineStatus()
  if (v === 'learning' && (!identity.value.gateway || identity.value.admin)) void store.refreshReviewDue()
  try {
    const url = new URL(window.location.href)
    url.searchParams.set('view', v)
    window.history.replaceState(null, '', url.toString())
  } catch {
    /* 非标准地址:忽略 */
  }
})

// 反思流程导航:建议 → 顶层视图(未解锁则忽略)
watch(
  () => store.navNonce,
  (n) => {
    if (n === 0 || !store.navView) return
    const target = store.navView
    store.navView = null
    if (shellOf(target) === 'console') goConsole(target)
    else if (viewAllowed(target, store.caps)) activeView.value = target
  },
)

// ── 草稿持久化:防抖写 localStorage(防误刷丢稿) ──
let draftTimer: ReturnType<typeof setTimeout> | null = null
watch(
  () => [store.text, store.lang, store.level, store.sessionId],
  () => {
    if (draftTimer) clearTimeout(draftTimer)
    draftTimer = setTimeout(() => {
      try {
        localStorage.setItem(
          'langfixer.draft.v1',
          JSON.stringify({ text: store.text, lang: store.lang, level: store.level, sessionId: store.sessionId }),
        )
      } catch {
        /* 忽略 */
      }
    }, 500)
  },
)

// ── 全局快捷键:⌘Enter 检测;⌘F 历史搜索;非编辑区 ⌘Z 撤销「应用修正」 ──
function undoApplyFeedback() {
  if (store.undoApply()) toast('已撤销「应用修正」,恢复应用前文本', 'ok')
  else toast('没有可撤销的「应用修正」操作', 'info')
}
function onGlobalKeydown(e: KeyboardEvent) {
  if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') {
    if (activeView.value !== 'workspace') return
    if (store.loading || !store.text.trim()) return
    e.preventDefault()
    void store.analyze(store.sessionId || undefined)
    return
  }
  if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'f') {
    if (shellOf(activeView.value) !== 'workbench') return
    e.preventDefault()
    window.dispatchEvent(new CustomEvent('langfixer:focus-history-search'))
    return
  }
  if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'z' && !e.shiftKey) {
    const t = e.target as HTMLElement | null
    if (t && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.isContentEditable)) return
    if (store.applySnapshot === null) return
    e.preventDefault()
    undoApplyFeedback()
  }
}

// ── 桌面端(Tauri):原生菜单命令 / 文件拖放路由 ──
let unlistenTauri: Array<() => void> = []
function onMenuAction(payload: unknown) {
  const id = String(payload ?? '')
  if (id.startsWith('menu:view:')) {
    const v = id.slice('menu:view:'.length) as AppView
    if (!ALL_VIEWS.includes(v)) return
    if (shellOf(v) === 'console') goConsole(v)
    else if (viewAllowed(v, store.caps)) activeView.value = v
    return
  }
  switch (id) {
    case 'menu:settings':
      goConsole('settings')
      break
    case 'menu:import':
      if (!viewAllowed('media', store.caps)) {
        lockedViewHint.value = 'media'
        return
      }
      activeView.value = 'media'
      window.dispatchEvent(new CustomEvent('langfixer:media-import'))
      break
    case 'menu:export':
      window.dispatchEvent(new CustomEvent('langfixer:media-export'))
      break
    case 'menu:undo-apply':
      undoApplyFeedback()
      break
    case 'menu:toggle-theme':
      toggleTheme()
      break
    case 'menu:onboarding':
      showOnboarding.value = true
      break
    case 'menu:check-update':
      void api.checkUpdateNow().catch((e) => toast(String((e as Error)?.message ?? e), 'danger'))
      break
  }
}

/** Finder 拖入:载体文件 → 载体审校并按路径建 job */
function onDragDrop(payload: unknown) {
  const paths = (payload as { paths?: string[] })?.paths ?? []
  const files = paths.filter((p) => /\.(png|jpe?g|webp|gif|bmp|tiff?|pdf|docx)$/i.test(p))
  if (!files.length) return
  if (!viewAllowed('media', store.caps)) {
    lockedViewHint.value = 'media'
    return
  }
  activeView.value = 'media'
  window.dispatchEvent(new CustomEvent('langfixer:media-drop', { detail: files }))
}

onMounted(() => window.addEventListener('keydown', onGlobalKeydown))
onBeforeUnmount(() => {
  window.removeEventListener('keydown', onGlobalKeydown)
  unlistenTauri.forEach((f) => f())
  unlistenTauri = []
})
if (isTauri) {
  unlistenTauri.push(tauriListen('menu-action', onMenuAction))
  unlistenTauri.push(tauriListen('tauri://drag-drop', onDragDrop))
}

// ── 主题:跟随系统 + localStorage 覆盖;桌面端原生窗口背景对齐 ──
const isDark = ref(document.documentElement.classList.contains('dark'))
function toggleTheme() {
  isDark.value = !isDark.value
  document.documentElement.classList.toggle('dark', isDark.value)
  api.setWindowTheme(isDark.value)
  try {
    localStorage.setItem('langfixer.theme', isDark.value ? 'dark' : 'light')
  } catch {
    /* 忽略 */
  }
}
if (isTauri) api.setWindowTheme(isDark.value)
</script>

<template>
  <TokenGate v-if="showGate" @entered="boot" />
  <div v-else-if="showApp" class="l-shell">
    <header class="l-toolbar">
      <span class="l-brand"><span class="l-brand-mark">✓</span><span class="l-brand-name">LangFixer</span></span>
      <span v-if="!inConsole" class="l-spacer" style="flex: 0 0 8px"></span>
      <!-- 工作台簇:深度分段 + 说明 + 语言 + 服务 chip -->
      <template v-if="!inConsole">
        <LSegmented v-model="store.level" :options="depthOptions" :title="depthNote" />
        <span class="l-depth-note">{{ depthNote }}</span>
        <LSelect
          :model-value="store.lang"
          :options="langOptions"
          aria-label="检测语言"
          @update:model-value="onLang"
        />
        <LPopover align="end">
          <template #trigger="{ toggle, open }">
            <button
              class="ps-chip"
              :class="psOk === psTotal ? 'ps-chip-ok' : 'ps-chip-warn'"
              :aria-expanded="open ? 'true' : 'false'"
              :title="`检测服务:${psOk}/${psTotal} 项可用(点击查看明细)`"
              aria-label="检测服务:查看可用情况"
              @click="toggle"
            >
              <i class="ps-chip-dot"></i>
              检测服务 {{ psOk }}/{{ psTotal }}
            </button>
          </template>
          <PipelineStatus flat />
        </LPopover>
      </template>
      <h2 v-else class="l-toolbar-title">运营台</h2>
      <span class="l-spacer"></span>
      <LButton v-if="canConsole" variant="ghost" @click="inConsole ? (activeView = 'workspace') : goConsole('rules')">
        {{ inConsole ? '工作台' : '运营台' }}
      </LButton>
      <LButton
        variant="ghost"
        :title="isDark ? '切换到浅色主题' : '切换到深色主题'"
        aria-label="切换浅色/深色主题"
        @click="toggleTheme"
      >
        {{ isDark ? '☀' : '☾' }}
      </LButton>
      <LButton variant="ghost" title="查看使用引导" aria-label="查看使用引导" @click="showOnboarding = true">
        帮助
      </LButton>
      <LButton
        v-if="identity.gateway"
        variant="ghost"
        :title="identity.admin ? '当前为超级管理员(可改配置、签发密钥)' : `当前密钥:${identity.token_id ?? '会话中'}`"
        @click="goConsole('access')"
      >
        {{ identity.admin ? '超管' : '评审' }}
      </LButton>
    </header>

    <!-- 工作台壳 -->
    <main v-if="!inConsole" class="l-main">
      <div v-if="lockedViewHint" class="l-banner" role="status">
        <template v-if="lockedViewHint === 'media'">
          「载体审校」是编校工作流:图片 / 扫描件文字提取 → 检测 → 落点画框 → 回写 / 正误表导出。
        </template>
        <template v-else-if="lockedViewHint === 'learning'">
          「练习与复习」是学习扩展:错题沉淀 → 间隔复习 → 掌握度回顾。
        </template>
        <template v-else>「规则管理 / 知识维护 / 进化中心」属于平台运营功能,面向平台维护者。</template>
        <span class="l-spacer"></span>
        <LButton
          v-if="lockedViewHint === 'media' || lockedViewHint === 'learning'"
          variant="primary"
          size="sm"
          @click="unlockScenario"
        >
          启用该场景并进入
        </LButton>
        <LButton
          v-else-if="lockedViewHint && !identity.gateway"
          variant="primary"
          size="sm"
          @click="unlockScenario"
        >
          启用运营场景并进入
        </LButton>
        <LButton size="sm" @click="adjustScenario">
          到设置调整场景
        </LButton>
      </div>

      <WorkbenchView v-if="activeView === 'workspace'" />
      <section v-else-if="activeView === 'docjobs'" class="l-fullpane"><DocJobsPanel /></section>
      <section v-else-if="activeView === 'learning'" class="l-fullpane"><LearningPanel /></section>
      <section v-else-if="activeView === 'media'" class="l-fullpane"><MediaWorkspace /></section>
    </main>

    <!-- 运营台壳 -->
    <main v-else class="l-console">
      <LSidebar :model-value="activeView" :sections="consoleSections" @select="(id) => goConsole(id as AppView)" />
      <div class="l-console-content">
        <template v-if="activeView === 'rules'">
          <LSection title="决策表" desc="规则行热更新 · 试算 · 版本回滚"><TableEditor /></LSection>
          <LSection title="规则开关" desc="词表规则启停,持久化不随重启丢失"><RulesPanel :lang="store.lang" /></LSection>
          <LSection title="自定义词"><CustomWords :lang="store.lang" /></LSection>
        </template>
        <template v-else-if="activeView === 'knowledge'">
          <LSection title="归因知识库" desc="知识点与知识源(1:N 背书)· 导入向导 · 资源包"><KnowledgePanel /></LSection>
          <LSection title="学习知识" desc="向量召回层的改错对与采纳资产"><LearnedKnowledgePanel /></LSection>
        </template>
        <EvolutionCenter v-else-if="activeView === 'evolution'" />
        <LayerContributions v-else-if="activeView === 'layers'" />
        <AccessPanel v-else-if="activeView === 'access'" />
        <SettingsPanel v-else-if="activeView === 'settings'" />
      </div>
    </main>

    <LToaster />
  </div>

  <OnboardingDialog v-if="showOnboarding && showApp" :visible="true" @done="finishOnboarding" />
</template>

<style scoped>
.l-brand { display: inline-flex; align-items: center; gap: var(--space-2); }
.l-brand-mark {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 22px;
  height: 22px;
  border-radius: 6px;
  background: var(--accent);
  color: #fff;
  font-size: 13px;
  font-weight: 700;
}
.l-brand-name { font-size: var(--fs-md); font-weight: 600; }
.l-depth-note { font-size: var(--fs-sm); color: var(--muted); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
@media (max-width: 1180px) { .l-depth-note { display: none; } }
.l-fullpane { min-height: 0; }
html.desktop .l-fullpane { height: 100%; overflow-y: auto; overscroll-behavior: contain; }
</style>
