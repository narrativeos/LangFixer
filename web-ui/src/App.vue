<script setup lang="ts">
import { ref, watch, onMounted, onBeforeUnmount } from 'vue'
import { useGecStore } from './stores/gec'
import Editor from './components/Editor.vue'
import PreviewPanel from './components/PreviewPanel.vue'
import ResultsPanel from './components/ResultsPanel.vue'
import TableEditor from './components/TableEditor.vue'
import KnowledgePanel from './components/KnowledgePanel.vue'
import LearnedKnowledgePanel from './components/LearnedKnowledgePanel.vue'
import RulesPanel from './components/RulesPanel.vue'
import CustomWords from './components/CustomWords.vue'
import HistoryPanel from './components/HistoryPanel.vue'
import SettingsPanel from './components/SettingsPanel.vue'
import EvolutionCenter from './components/EvolutionCenter.vue'
import PipelineStatus from './components/PipelineStatus.vue'
import MediaWorkspace from './components/MediaWorkspace.vue'
import OnboardingDialog from './components/OnboardingDialog.vue'

const store = useGecStore()
/** 顶层视图：主工作区（检测与归因 + 检测历史）/ 载体审校 / 规则管理 / 知识维护 / 进化中心 / 设置（右上角切换） */
type AppView = 'workspace' | 'media' | 'rules' | 'knowledge' | 'evolution' | 'settings'
const APP_VIEWS: AppView[] = ['workspace', 'media', 'rules', 'knowledge', 'evolution', 'settings']

/** `?view=media` 深链：刷新 / 分享链接都停在同一视图（也便于无头浏览器冒烟测试） */
function viewFromUrl(): AppView | null {
  try {
    const v = new URLSearchParams(window.location.search).get('view')
    return APP_VIEWS.includes(v as AppView) ? (v as AppView) : null
  } catch {
    return null
  }
}

const activeView = ref<AppView>(viewFromUrl() ?? 'workspace')
const rulesSub = ref('tables')
const knowledgeSub = ref('attribution')

// 首次启动引导（T1-2）：只弹一次（localStorage 记录）；header「帮助」可随时重新打开
const showOnboarding = ref(false)

onMounted(() => {
  // 拉取后端真实语言 / 引擎元信息（Web: GET /gec/languages；桌面: invoke('languages')）
  store.loadLanguages()
  // 同步决策模型启用状态（「响应级别」L2 的状态显示用）
  store.loadSettings()
  try {
    showOnboarding.value = !localStorage.getItem('langfixer.onboarded.v1')
  } catch {
    showOnboarding.value = true
  }
})

function finishOnboarding() {
  showOnboarding.value = false
  try {
    localStorage.setItem('langfixer.onboarded.v1', '1')
  } catch {
    /* 隐私模式等写入失败：忽略（下次再弹） */
  }
}

// 切回检测工作区时重新校验检测管线各层启用状态：
// 设置页保存（或他处变更）后，L2 的状态标签 / 管线提示自动与服务端对齐
watch(activeView, (v) => {
  if (v === 'workspace') void store.refreshEngineStatus()
  // 视图同步进地址栏（replaceState：不产生历史堆栈，刷新仍在当前视图）
  try {
    const url = new URL(window.location.href)
    url.searchParams.set('view', v)
    window.history.replaceState(null, '', url.toString())
  } catch {
    /* 非标准地址（如 Tauri 自定义协议）：忽略 */
  }
})

// 反思流程导航：「到规则面板查看 / 积累后重训练」建议 → 切换顶层视图
watch(
  () => store.navNonce,
  (n) => {
    if (n === 0 || !store.navView) return
    activeView.value = store.navView
    if (store.navView === 'rules') rulesSub.value = 'rules'
    store.navView = null
  },
)

// ── 草稿持久化：text / lang / level / sessionId 防抖写 localStorage（防误刷丢稿） ──
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
        /* 隐私模式等写入失败：忽略 */
      }
    }, 500)
  },
)

// ── 全局快捷键：⌘/Ctrl+Enter = 检测 + 归因（仅检测工作区、条件与按钮一致） ──
function onGlobalKeydown(e: KeyboardEvent) {
  if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') {
    if (activeView.value !== 'workspace') return
    if (store.loading || !store.text.trim()) return
    e.preventDefault()
    void store.analyze(store.sessionId || undefined)
  }
}
onMounted(() => window.addEventListener('keydown', onGlobalKeydown))
onBeforeUnmount(() => window.removeEventListener('keydown', onGlobalKeydown))

// ── 主题（日/夜）：main.ts 已初始化 html.dark；此处仅切换与持久化 ──
const isDark = ref(document.documentElement.classList.contains('dark'))
function toggleTheme() {
  isDark.value = !isDark.value
  document.documentElement.classList.toggle('dark', isDark.value)
  try {
    localStorage.setItem('langfixer.theme', isDark.value ? 'dark' : 'light')
  } catch {
    /* 忽略 */
  }
}
</script>

<template>
  <header class="app-header">
    <div class="brand-mark">
      <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round">
        <path d="M5 12.5l4.5 4.5L19 7.5" />
      </svg>
    </div>
    <div class="brand-text">
      <h1>LangFixer</h1>
      <p class="tagline">多语言语法错误检测 × ZEN 决策表教育归因</p>
    </div>
    <div class="spacer"></div>
    <nav class="header-nav" aria-label="主导航">
      <el-button
        :class="{ 'nav-active': activeView === 'workspace' }"
        :aria-current="activeView === 'workspace' ? 'page' : undefined"
        @click="activeView = 'workspace'"
      >
        检测
      </el-button>
      <el-button
        :class="{ 'nav-active': activeView === 'media' }"
        :aria-current="activeView === 'media' ? 'page' : undefined"
        @click="activeView = 'media'"
      >
        载体审校
      </el-button>
      <el-button
        :class="{ 'nav-active': activeView === 'rules' }"
        :aria-current="activeView === 'rules' ? 'page' : undefined"
        @click="activeView = 'rules'"
      >
        规则管理
      </el-button>
      <el-button
        :class="{ 'nav-active': activeView === 'knowledge' }"
        :aria-current="activeView === 'knowledge' ? 'page' : undefined"
        @click="activeView = 'knowledge'"
      >
        知识维护
      </el-button>
      <el-button
        :class="{ 'nav-active': activeView === 'evolution' }"
        :aria-current="activeView === 'evolution' ? 'page' : undefined"
        @click="activeView = 'evolution'"
      >
        进化中心
      </el-button>
      <el-button
        :class="{ 'nav-active': activeView === 'settings' }"
        :aria-current="activeView === 'settings' ? 'page' : undefined"
        @click="activeView = 'settings'"
      >
        设置
      </el-button>
      <el-button text :title="isDark ? '切换到浅色主题' : '切换到深色主题'" @click="toggleTheme">
        <el-icon><Moon v-if="!isDark" /><Sunny v-else /></el-icon>
      </el-button>
      <el-button text title="查看使用引导" @click="showOnboarding = true">帮助</el-button>
    </nav>
  </header>

  <main class="main-area">
    <!-- 主工作区：检测与归因（三列）+ 检测历史，直接展示 -->
    <template v-if="activeView === 'workspace'">
      <!-- 系统状态总览（T1-1）：管线各层可用 / 未启用 / 未配置；点项跳设置，可一键测试全部 -->
      <PipelineStatus />
      <section class="detect-layout">
        <el-card shadow="never" class="input-card">
          <template #header>
            <div class="card-header">
              <span class="card-title">文本输入与参数</span>
              <span class="spacer"></span>
              <!-- 语言切换 = 立即清空旧结果：跨语言结果无意义，避免右列残留上一语言的结果 -->
              <el-radio-group v-model="store.lang" @change="store.clearResults()">
                <el-radio-button v-for="l in store.displayLangs" :key="l.lang" :value="l.lang">
                  {{ l.name }}
                </el-radio-button>
              </el-radio-group>
            </div>
          </template>
          <Editor />
        </el-card>
        <PreviewPanel />
        <ResultsPanel />
      </section>
      <section class="history-section">
        <HistoryPanel :lang="store.lang" />
      </section>
    </template>

    <!-- 载体审校：图片（P0）——探测 → 抽取 → 检测 → 落点画框 → 勾选 → 回写 / 正误表 -->
    <section v-else-if="activeView === 'media'" class="manage-pane">
      <MediaWorkspace />
    </section>

    <!-- 规则管理（右上角入口） -->
    <section v-else-if="activeView === 'rules'" class="manage-pane">
      <el-tabs v-model="rulesSub" class="sub-tabs">
        <el-tab-pane label="决策表管理" name="tables">
          <TableEditor />
        </el-tab-pane>
        <el-tab-pane label="规则管理" name="rules">
          <RulesPanel :lang="store.lang" />
        </el-tab-pane>
        <el-tab-pane label="自定义词" name="words">
          <CustomWords :lang="store.lang" />
        </el-tab-pane>
      </el-tabs>
    </section>

    <!-- 知识维护（右上角入口） -->
    <section v-else-if="activeView === 'knowledge'" class="manage-pane">
      <el-tabs v-model="knowledgeSub" class="sub-tabs">
        <el-tab-pane label="归因知识库" name="attribution">
          <KnowledgePanel />
        </el-tab-pane>
        <el-tab-pane label="学习知识" name="learned">
          <LearnedKnowledgePanel />
        </el-tab-pane>
      </el-tabs>
    </section>

    <!-- 进化中心（右上角入口）：测评（P0）/ 反思（P1）/ 训练（P2）/ 贡献与轮次（P3） -->
    <section v-else-if="activeView === 'evolution'" class="manage-pane">
      <EvolutionCenter />
    </section>

    <!-- 设置（右上角入口） -->
    <section v-else class="manage-pane">
      <SettingsPanel />
    </section>
  </main>

  <!-- 首次启动引导（T1-2）：三步上手，关闭后不再出现 -->
  <OnboardingDialog v-if="showOnboarding" :visible="true" @done="finishOnboarding" />
</template>
