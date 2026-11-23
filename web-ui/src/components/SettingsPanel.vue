<script setup lang="ts">
import { computed, ref, watch, onMounted, nextTick } from 'vue'
import type { Ref } from 'vue'
import { errMsg } from '../lib/err'
import { identity } from '../lib/auth'
import { markOpsUserChoice } from '../lib/caps'
import { modeLabel } from '../lib/labels'
import {
  LButton,
  LCheckbox,
  LDot,
  LEmpty,
  LInput,
  LSection,
  LSegmented,
  LSelect,
  LSwitch,
  LTable,
  LTag,
} from '../ui'
import { toast } from '../ui/toast'
import { api, isTauri } from '../api/client'
import { useGecStore } from '../stores/gec'
import type { LColumn } from '../ui'
import type { AppSettings, DepStatus, L3FlywheelStatus, ModelView, SettingsTestResult, SettingsTestTarget } from '../types'

const store = useGecStore()

// ── 锚点直达（PipelineStatus 点击 → 本卡片滚动定位 + 短暂高亮） ──
watch(
  () => [store.navNonce, store.settingsAnchor],
  ([n, anchor]) => {
    if (!n || !anchor) return
    const key = anchor
    store.settingsAnchor = ''
    void nextTick(() => {
      const el = document.getElementById(`set-${key}`)
      if (!el) return
      el.scrollIntoView({ behavior: 'smooth', block: 'start' })
      el.classList.add('anchor-flash')
      setTimeout(() => el.classList.remove('anchor-flash'), 1600)
    })
  },
)

const loading = ref(false)
const saving = ref(false)
const error = ref('')
const savedMsg = ref('')

// ── 工作场景与角色（能力位门控，见 lib/caps.ts） ──
// 核心工作区共享；学习 / 编校 / 运营三个扩展按需开关。
// ops 在 web 形态解锁需 token 验证（POST /gec/ops/unlock，服务端防线是 token 鉴权端点）；
// 桌面端本地单用户直接解锁。
const showOpsToken = ref(false)
const opsToken = ref('')
const opsUnlocking = ref(false)
const opsUnlockError = ref('')
const showOpsKey = ref(false)

function onCapToggle(key: 'learning' | 'proofreading' | 'ops', value: boolean) {
  if (key !== 'ops') {
    store.setCap(key, value)
    return
  }
  // 用户拨过开关即记录意图，后续不再自动置位
  markOpsUserChoice()
  if (!value) {
    store.setCap('ops', false)
    showOpsToken.value = false
    return
  }
  // 桌面端本地单用户直接置位；web 形态超管写端点已受服务端鉴权，同样免 token 过场
  if (isTauri || identity.value.admin) {
    store.setCap('ops', true)
    return
  }
  // web 形态非超管：先展示内联 token 验证表单，验证通过才真正开启
  showOpsToken.value = true
}

async function unlockOps() {
  opsUnlocking.value = true
  opsUnlockError.value = ''
  try {
    await api.opsUnlock(opsToken.value || undefined)
    store.setCap('ops', true)
    showOpsToken.value = false
    opsToken.value = ''
    toast('已解锁平台运营功能', 'ok')
  } catch (e) {
    opsUnlockError.value = errMsg(e)
  } finally {
    opsUnlocking.value = false
  }
}

// ── 我的档案（学习者画像：母语 + 自评 CEFR → 注入 LLM 讲解 prompt / 缺省档位） ──
const CEFR_LEVELS = ['A1', 'A2', 'B1', 'B2', 'C1', 'C2']
const NATIVE_OPTIONS = ['中文', 'English', '日本語', '한국어', 'français', 'español', 'Deutsch']
const profileNative = ref('')
const profileCefr = ref('')
const profileSaving = ref(false)
const profileError = ref('')

const cefrOptions = [
  { value: '', label: '未评估（讲解默认从最基础的档位开始）' },
  ...CEFR_LEVELS.map((c) => ({ value: c, label: c })),
]

/** 画像挂在会话上：无会话 ID 时本地生成（PUT profile / get_session 两端都会自动建档） */
function ensureSessionId(): string {
  return store.ensureSessionId()
}

async function loadProfile() {
  if (!store.sessionId) return
  try {
    const rec = await api.getSession(store.sessionId, store.lang)
    profileNative.value = rec.meta.native_language ?? ''
    profileCefr.value = rec.meta.cefr ?? ''
  } catch {
    // REST 形态会话不存在（404）：按空档案处理，保存时自动建档
  }
}
void loadProfile()

// ── 删除当前会话（C4）：级联移除会话与错题；复习进度（全局记忆）保留 ──
// 弹窗配额只留给 发布 / 删资源包 / 回滚：这里用行内两步确认。
const sessionDeleting = ref(false)
const pendingDeleteCount = ref<number | null>(null)

async function askDeleteCurrentSession() {
  const id = store.sessionId
  if (!id) return
  sessionDeleting.value = true
  try {
    const mistakes = await api.listSessionMistakes(id)
    pendingDeleteCount.value = mistakes.length
  } catch (e) {
    profileError.value = errMsg(e)
  } finally {
    sessionDeleting.value = false
  }
}

async function confirmDeleteCurrentSession() {
  const id = store.sessionId
  if (!id) return
  pendingDeleteCount.value = null
  sessionDeleting.value = true
  try {
    await api.deleteSession(id)
    store.sessionId = ''
    profileNative.value = ''
    profileCefr.value = ''
    toast('当前会话已删除；复习进度已保留', 'ok')
  } catch (e) {
    profileError.value = errMsg(e)
  } finally {
    sessionDeleting.value = false
  }
}

async function saveProfile() {
  profileSaving.value = true
  profileError.value = ''
  try {
    const id = ensureSessionId()
    const rec = await api.updateSessionProfile(id, {
      lang: store.lang,
      native_language: profileNative.value || undefined,
      cefr: profileCefr.value || undefined,
    })
    profileNative.value = rec.meta.native_language ?? profileNative.value
    profileCefr.value = rec.meta.cefr ?? profileCefr.value
    toast('档案已保存：AI 讲解会自动贴合你的水平', 'ok')
  } catch (e) {
    profileError.value = errMsg(e)
  } finally {
    profileSaving.value = false
  }
}

const hanlpEndpoint = ref('')
/** 离线模式（GEC_OFFLINE）：检测器仅用本地规则，跳过远端 NLP 服务 */
const offlineMode = ref(false)
/** 生产库每日备份：开关位置（设置愿望）与 launchd 实存态分开维护，面板不说谎 */
const dbBackupSetting = ref(false)
const dbBackupInstalled = ref(false)
const dbBackupDest = ref('')
const llmBaseUrl = ref('')
const llmApiKey = ref('')
const showLlmKey = ref(false)
const llmModel = ref('')
const llmTimeout = ref(30)
/** LLM 提示词模板目录（LLM_PROMPTS_DIR） */
const llmPromptsDir = ref('')
/** 当前选中的纠错模型预设 id（`prompts/models.json` 注册表；缺省 `generic`） */
const llmModelId = ref('generic')
/** ③ 自动双判开关（影子臂并行判 + 判定落 dual_judge_log） */
const llmDualJudge = ref(false)
/** ③ 双判影子臂独立配置（与主臂完全解耦；端点/模型两空 = 沿用注册表预设 arm3） */
const llmDualBaseUrl = ref('')
const llmDualModelName = ref('')
const llmDualApiKey = ref('')
const showDualKey = ref(false)
/** 可用纠错模型预设列表（「纠错模型」选择器；仅含 enabled 条目） */
const models = ref<ModelView[]>([])
/** 决策表目录（GEC_TABLES_DIR） */
const tablesDir = ref('')
/** 规则词表目录（GEC_LEXICONS_DIR） */
const lexiconsDir = ref('')
/** 决策模型（vegaml）层：规则引擎之后、LLM 之前的中间检测层 */
const decisionEndpoint = ref('')
const decisionModel = ref('')
const decisionTimeout = ref(10)
const decisionThreshold = ref(0.5)
const decisionEnabled = ref(false)
/** 配置生效来源（审计 F7）：settings_file / env / default */
const decisionSource = ref('')
const decisionSourceText = computed(
  () =>
    ({
      settings_file: '设置文件 langfixer-settings.json',
      env: '环境变量 GEC_DEC_*',
      default: '内置默认',
    })[decisionSource.value] ?? '未知（服务端未上报）',
)
/** L3 特征模型层（三期）：level 3 先跑特征模型，LLM 收缩到纯长尾 */
const l3Endpoint = ref('')
const l3Model = ref('')
const l3Timeout = ref(10)
const l3Threshold = ref(0.5)
const l3Enabled = ref(true)
/** 嵌入引擎（向量召回）配置 */
const embeddingEndpoint = ref('')
const embeddingModel = ref('')
const embeddingApiKey = ref('')
const showEmbKey = ref(false)
/** 载体（图片 / PDF / Word）审校设置（两端同一份定义：`media-flow`） */
const mediaEnabled = ref(false)
const mediaUseStructure = ref(true)
const mediaUseVision = ref(false)
const mediaUseOcr = ref(false)
const mediaLevel = ref(3)
const mediaForceLlm = ref(true)
/** 默认工作模式：simple = 零 CLI（只出清单/正误表）/ advanced = 三家适配器（可回写） */
const mediaMode = ref('advanced')
const mediaApplyMode = ref('overlay')
const mediaVisionModel = ref('')
const mediaWorkdir = ref('')
/** LLM 请求模式（固定默认，只读展示） */
const llmModes = ref<AppSettings['llm_modes'] | null>(null)
/** 当前 LLM 是否已配置（api_key 非空） */
const llmEnabled = ref(false)

// ── 控件选项（值与提交契约保持原样；label 用词出自 lib/labels） ──
const mediaModeOptions: { value: string; label: string }[] = [
  { value: 'simple', label: '简单（零 CLI）' },
  { value: 'advanced', label: '高级（三家适配器）' },
]
const mediaApplyModeOptions: { value: string; label: string }[] = [
  { value: 'overlay', label: 'Overlay 叠加标注（默认）' },
  { value: 'comment', label: '批注' },
  { value: 'in_place', label: '改原文（仅带文字层的载体）' },
  { value: 'list_only', label: '只出清单' },
]
const mediaLevelOptions: { value: number; label: string }[] = [
  { value: 1, label: modeLabel(1) },
  { value: 2, label: modeLabel(2) },
  { value: 3, label: modeLabel(3) },
]
const flywheelLangOptions: { value: string; label: string }[] = [
  { value: 'zh', label: '中文' },
  { value: 'en', label: '英文' },
]
const modelOptions = computed(() => models.value.map((m) => ({ value: m.id, label: m.name })))

/** 数字输入的原生控件值永远是 string：包一层 computed 保持底层 number ref 与保存逻辑不变 */
function numRef(r: Ref<number>) {
  return computed({
    get: () => String(r.value),
    set: (v: string) => {
      const n = Number(v)
      if (v !== '' && Number.isFinite(n)) r.value = n
    },
  })
}
const llmTimeoutNum = numRef(llmTimeout)
const l3TimeoutNum = numRef(l3Timeout)
const l3ThresholdNum = numRef(l3Threshold)
const decisionTimeoutNum = numRef(decisionTimeout)
const decisionThresholdNum = numRef(decisionThreshold)

async function load() {
  loading.value = true
  error.value = ''
  savedMsg.value = ''
  try {
    // 表单回填期间抑制「启用决策层」开关的自动保存
    suppressAutoSave = true
    const s = await api.getSettings()
    savedSnapshot.value = s
    hanlpEndpoint.value = s.hanlp_endpoint
    offlineMode.value = s.offline_mode
    llmBaseUrl.value = s.llm.base_url
    llmApiKey.value = s.llm.api_key
    llmModel.value = s.llm.model
    llmTimeout.value = s.llm.timeout_secs
    llmPromptsDir.value = s.llm.prompts_dir
    llmModelId.value = s.llm_model_id
    llmDualJudge.value = s.llm_dual_judge ?? false
    llmDualBaseUrl.value = s.llm_dual_base_url ?? ''
    llmDualModelName.value = s.llm_dual_model ?? ''
    llmDualApiKey.value = s.llm_dual_api_key ?? ''
    models.value = s.models ?? []
    tablesDir.value = s.tables_dir
    lexiconsDir.value = s.lexicons_dir
    llmEnabled.value = s.llm.enabled
    llmModes.value = s.llm_modes
    if (s.decision) {
      decisionEndpoint.value = s.decision.endpoint
      decisionModel.value = s.decision.model
      decisionTimeout.value = s.decision.timeout_secs
      decisionThreshold.value = s.decision.abstain_threshold
      decisionEnabled.value = s.decision.enabled
      decisionSource.value = s.decision.config_source ?? ''
    }
    if (s.l3) {
      l3Endpoint.value = s.l3.endpoint
      l3Model.value = s.l3.model
      l3Timeout.value = s.l3.timeout_secs
      l3Threshold.value = s.l3.abstain_threshold
      l3Enabled.value = s.l3.enabled
    }
    if (s.embedding) {
      embeddingEndpoint.value = s.embedding.endpoint
      embeddingModel.value = s.embedding.model
      embeddingApiKey.value = s.embedding.api_key
    }
    dbBackupSetting.value = s.db_backup?.setting ?? false
    dbBackupInstalled.value = s.db_backup?.installed ?? false
    dbBackupDest.value = s.db_backup?.dest ?? ''
    if (s.media) {
      mediaEnabled.value = s.media.enabled
      mediaUseStructure.value = s.media.use_structure_layer
      mediaUseVision.value = s.media.use_vision
      mediaUseOcr.value = s.media.use_ocr
      mediaLevel.value = s.media.level
      mediaForceLlm.value = s.media.force_llm
      mediaMode.value = s.media.mode
      mediaApplyMode.value = s.media.apply_mode
      mediaVisionModel.value = s.media.vision_model
      mediaWorkdir.value = s.media.workdir
    }
    // 以服务端真实状态为准：同步全局 store（检测页 L2 状态）
    store.decisionEnabled = !!s.decision?.enabled
    await nextTick()
    suppressAutoSave = false
  } catch (e) {
    suppressAutoSave = false
    error.value = errMsg(e)
  } finally {
    loading.value = false
  }
}

/** 最近一次已保存的设置快照（load / 保存成功后更新；用于判断表单是否有未保存修改） */
const savedSnapshot = ref<AppSettings | null>(null)

/** 表单是否有未保存修改（与最近保存快照逐字段比较） */
function formIsDirty(): boolean {
  const s = savedSnapshot.value
  if (!s) return false
  return (
    hanlpEndpoint.value.trim() !== s.hanlp_endpoint ||
    offlineMode.value !== s.offline_mode ||
    dbBackupSetting.value !== (s.db_backup?.setting ?? false) ||
    llmBaseUrl.value.trim() !== s.llm.base_url ||
    llmApiKey.value !== s.llm.api_key ||
    llmModel.value.trim() !== s.llm.model ||
    llmTimeout.value !== s.llm.timeout_secs ||
    llmPromptsDir.value.trim() !== s.llm.prompts_dir ||
    llmModelId.value !== s.llm_model_id ||
    llmDualJudge.value !== s.llm_dual_judge ||
    llmDualBaseUrl.value.trim() !== (s.llm_dual_base_url ?? '') ||
    llmDualModelName.value.trim() !== (s.llm_dual_model ?? '') ||
    llmDualApiKey.value !== (s.llm_dual_api_key ?? '') ||
    tablesDir.value.trim() !== s.tables_dir ||
    lexiconsDir.value.trim() !== s.lexicons_dir ||
    decisionEndpoint.value.trim() !== s.decision.endpoint ||
    decisionModel.value.trim() !== s.decision.model ||
    decisionTimeout.value !== s.decision.timeout_secs ||
    decisionThreshold.value !== s.decision.abstain_threshold ||
    decisionEnabled.value !== s.decision.enabled ||
    (s.l3 !== undefined &&
      (l3Endpoint.value.trim() !== s.l3.endpoint ||
        l3Model.value.trim() !== s.l3.model ||
        l3Timeout.value !== s.l3.timeout_secs ||
        l3Threshold.value !== s.l3.abstain_threshold ||
        l3Enabled.value !== s.l3.enabled)) ||
    embeddingEndpoint.value.trim() !== (s.embedding?.endpoint ?? '') ||
    embeddingApiKey.value !== (s.embedding?.api_key ?? '') ||
    embeddingModel.value.trim() !== (s.embedding?.model ?? '') ||
    (s.media !== undefined &&
      (mediaEnabled.value !== s.media.enabled ||
        mediaUseStructure.value !== s.media.use_structure_layer ||
        mediaUseVision.value !== s.media.use_vision ||
        mediaUseOcr.value !== s.media.use_ocr ||
        mediaLevel.value !== s.media.level ||
        mediaForceLlm.value !== s.media.force_llm ||
        mediaMode.value !== s.media.mode ||
        mediaApplyMode.value !== s.media.apply_mode ||
        mediaVisionModel.value.trim() !== s.media.vision_model ||
        mediaWorkdir.value.trim() !== s.media.workdir))
  )
}

/** 保存（部分更新：HanLP 地址与 LLM 参数均全量提交当前表单值）；返回是否保存成功 */
async function save(): Promise<boolean> {
  saving.value = true
  error.value = ''
  savedMsg.value = ''
  try {
    // 备份开关只在改动时提交：注册任务带 RunAtLoad，每次保存都重注册会白白多跑一次备份
    const dbChanged = dbBackupSetting.value !== (savedSnapshot.value?.db_backup?.setting ?? false)
    const s = await api.updateSettings({
      hanlp_endpoint: hanlpEndpoint.value.trim(),
      llm_model_id: llmModelId.value,
      llm_dual_judge: llmDualJudge.value,
      llm_dual_base_url: llmDualBaseUrl.value.trim(),
      llm_dual_model: llmDualModelName.value.trim(),
      llm_dual_api_key: llmDualApiKey.value,
      offline_mode: offlineMode.value,
      ...(dbChanged ? { db_backup_enabled: dbBackupSetting.value } : {}),
      llm: {
        base_url: llmBaseUrl.value.trim(),
        api_key: llmApiKey.value,
        model: llmModel.value.trim(),
        timeout_secs: llmTimeout.value,
        prompts_dir: llmPromptsDir.value.trim(),
      },
      tables_dir: tablesDir.value.trim(),
      lexicons_dir: lexiconsDir.value.trim(),
      decision: {
        endpoint: decisionEndpoint.value.trim(),
        model: decisionModel.value.trim(),
        timeout_secs: decisionTimeout.value,
        abstain_threshold: decisionThreshold.value,
        enabled: decisionEnabled.value,
      },
      l3: {
        endpoint: l3Endpoint.value.trim(),
        model: l3Model.value.trim(),
        timeout_secs: l3Timeout.value,
        abstain_threshold: l3Threshold.value,
        enabled: l3Enabled.value,
      },
      embedding: {
        endpoint: embeddingEndpoint.value.trim(),
        api_key: embeddingApiKey.value,
        model: embeddingModel.value.trim(),
      },
      media: {
        enabled: mediaEnabled.value,
        mode: mediaMode.value,
        apply_mode: mediaApplyMode.value,
        workdir: mediaWorkdir.value.trim(),
        use_structure_layer: mediaUseStructure.value,
        use_vision: mediaUseVision.value,
        use_ocr: mediaUseOcr.value,
        vision_model: mediaVisionModel.value.trim(),
        level: mediaLevel.value,
        force_llm: mediaForceLlm.value,
      },
    })
    savedSnapshot.value = s
    llmEnabled.value = s.llm.enabled
    dbBackupSetting.value = s.db_backup?.setting ?? dbBackupSetting.value
    dbBackupInstalled.value = s.db_backup?.installed ?? dbBackupInstalled.value
    dbBackupDest.value = s.db_backup?.dest ?? dbBackupDest.value
    if (s.decision) decisionEnabled.value = s.decision.enabled
    // 保存成功：以服务端确认的状态同步全局 store，检测页「标准检查」状态自动对齐
    store.decisionEnabled = !!s.decision?.enabled
    savedMsg.value = '已保存并即时生效（已持久化到设置文件，重启后仍生效）'
    return true
  } catch (e) {
    error.value = errMsg(e)
    return false
  } finally {
    saving.value = false
  }
}

onMounted(() => {
  // Web 端 / 桌面端均加载设置：桌面端走 Tauri 命令（设置文件 > 环境变量 > 内置默认）
  load()
  void refreshDeps()
})

// ── 「启用决策层」开关：切换即自动保存（即时生效 + 持久化），状态条 ② 随之同步 ──
// （zh v2 trackb 门控达标后重新接入产品路径、默认开启，EVOLUTION.md #11.19 补五）
const decisionApplying = ref(false)
const decisionApplyMsg = ref('')
let decisionApplyTimer: ReturnType<typeof setTimeout> | null = null
let decisionAutoSaveBusy = false
watch(decisionEnabled, async (v, old) => {
  if (old === undefined || suppressAutoSave || decisionAutoSaveBusy) return
  decisionAutoSaveBusy = true
  decisionApplying.value = true
  decisionApplyMsg.value = ''
  // 乐观更新全局 store：状态条 ② 立即跟随开关
  store.decisionEnabled = v
  try {
    const res = await api.updateSettings({ decision: { enabled: v } })
    savedSnapshot.value = res
    const confirmed = res.decision?.enabled
    if (confirmed === undefined || confirmed === v) {
      decisionApplyMsg.value = `决策模型层已${v ? '启用' : '停用'}（即时生效，已持久化）`
    } else {
      // 服务端未接受该值：回滚开关 + store
      suppressAutoSave = true
      decisionEnabled.value = confirmed
      store.decisionEnabled = confirmed
      await nextTick()
      suppressAutoSave = false
      decisionApplyMsg.value = '保存失败：服务端未接受该状态'
    }
  } catch (e) {
    // 保存失败：回滚开关 + store，避免界面状态与服务端不一致
    suppressAutoSave = true
    decisionEnabled.value = !v
    store.decisionEnabled = !v
    await nextTick()
    suppressAutoSave = false
    decisionApplyMsg.value = `保存失败：${errMsg(e)}`
  } finally {
    decisionAutoSaveBusy = false
    decisionApplying.value = false
    if (decisionApplyTimer) clearTimeout(decisionApplyTimer)
    decisionApplyTimer = setTimeout(() => {
      decisionApplyMsg.value = ''
    }, 8000)
  }
})

/** load() 从服务端回填表单值期间为 true，避免误触发自动保存（如 L3 / 决策开关） */
let suppressAutoSave = false

// ── 「启用特征模型」开关：切换即自动保存（即时生效 + 持久化），L3 状态随之同步 ──
const l3Applying = ref(false)
const l3ApplyMsg = ref('')
let l3ApplyTimer: ReturnType<typeof setTimeout> | null = null
let l3AutoSaveBusy = false
watch(l3Enabled, async (v, old) => {
  if (old === undefined || suppressAutoSave || l3AutoSaveBusy) return
  l3AutoSaveBusy = true
  l3Applying.value = true
  l3ApplyMsg.value = ''
  try {
    const res = await api.updateSettings({ l3: { enabled: v } })
    savedSnapshot.value = res
    const confirmed = res.l3?.enabled
    if (confirmed === undefined || confirmed === v) {
      l3ApplyMsg.value = `特征模型层已${v ? '启用' : '停用'}（即时生效，已持久化）`
    } else {
      // 服务端未接受该值：回滚开关
      suppressAutoSave = true
      l3Enabled.value = confirmed
      await nextTick()
      suppressAutoSave = false
      l3ApplyMsg.value = '保存失败：服务端未接受该状态'
    }
  } catch (e) {
    suppressAutoSave = true
    l3Enabled.value = !v
    await nextTick()
    suppressAutoSave = false
    l3ApplyMsg.value = `保存失败：${errMsg(e)}`
  } finally {
    l3AutoSaveBusy = false
    l3Applying.value = false
    if (l3ApplyTimer) clearTimeout(l3ApplyTimer)
    l3ApplyTimer = setTimeout(() => {
      l3ApplyMsg.value = ''
    }, 8000)
  }
})

// ── 依赖服务（服务端启动自检 + 监督器主动拉起；GET/POST /gec/deps）──
// 拉不起来的不能静默：逐项展示原因，并提供「拉起服务」/「其他机器地址」两条补救路径
const depsBusy = ref<Record<string, boolean>>({})
const depsMsg = ref<Record<string, string>>({})
const depsAlt = ref<Record<string, string>>({})
const depsRecheck = ref(false)

const DEP_STATE_TEXT: Record<DepStatus['state'], string> = {
  ok: '就绪',
  starting: '拉起中…',
  failed: '不可用',
  skipped: '已跳过（资源不足）',
  off: '未启用 / 未配置',
  pending: '未探测',
}
const DEP_STATE_TONE: Record<DepStatus['state'], 'ok' | 'warn' | 'danger' | 'neutral'> = {
  ok: 'ok',
  starting: 'warn',
  failed: 'danger',
  skipped: 'warn',
  off: 'neutral',
  pending: 'neutral',
}
const DEP_DOT_TONE: Record<DepStatus['state'], 'ok' | 'warn' | 'danger' | 'muted'> = {
  ok: 'ok',
  starting: 'warn',
  failed: 'danger',
  skipped: 'warn',
  off: 'muted',
  pending: 'muted',
}

async function refreshDeps() {
  await store.loadDeps()
}

async function recheckDeps() {
  depsRecheck.value = true
  try {
    await api.recheckDeps()
  } catch (e) {
    toast(`重新探测失败：${errMsg(e)}`, 'warn')
  } finally {
    await refreshDeps()
    depsRecheck.value = false
  }
}

async function startDep(d: DepStatus) {
  depsBusy.value[d.id] = true
  depsMsg.value[d.id] = '正在拉起（侧车模型加载可能需要 1–2 分钟）…'
  try {
    const item = await api.startDep(d.id)
    depsMsg.value[d.id] = item.state === 'ok' ? '已就绪' : item.reason || '拉起未完成'
  } catch (e) {
    depsMsg.value[d.id] = `拉起失败：${errMsg(e)}`
  } finally {
    await refreshDeps()
    depsBusy.value[d.id] = false
  }
}

/** 拉不起来时的另一条路：该服务部署在其他机器 → 保存新地址（设置侧会自动重探） */
async function saveDepEndpoint(d: DepStatus) {
  const v = (depsAlt.value[d.id] || '').trim()
  if (!v) return
  depsBusy.value[d.id] = true
  depsMsg.value[d.id] = '地址已保存，正在探测…'
  try {
    await api.updateSettings(
      d.id === 'decision' ? { decision: { endpoint: v } } : { l3: { endpoint: v } },
    )
    // 服务端保存后会后台自动重探该项，稍候读快照
    await new Promise((r) => setTimeout(r, 1500))
  } catch (e) {
    depsMsg.value[d.id] = `保存失败：${errMsg(e)}`
  } finally {
    await refreshDeps()
    depsBusy.value[d.id] = false
  }
}

// ── L3 训练飞轮（三期闭环）：确认样本积累 → 重训练提醒 → 重训练 → 热切换 ──
const flywheelLang = ref('zh')
const flywheelStatus = ref<L3FlywheelStatus | null>(null)
const flywheelLoading = ref(false)
const retraining = ref(false)
const retrainMsg = ref('')
let retrainTimer: ReturnType<typeof setTimeout> | null = null

/** 加载飞轮状态（feature-model 不可达时降级展示本地样本数） */
async function loadFlywheelStatus() {
  if (!l3Enabled.value) {
    flywheelStatus.value = null
    return
  }
  flywheelLoading.value = true
  try {
    flywheelStatus.value = await api.l3Status(flywheelLang.value)
  } catch {
    flywheelStatus.value = null
  } finally {
    flywheelLoading.value = false
  }
}

/** 触发重训练：合并确认样本 → 重训练 → dev 评测 → F1 未退化则热切换 */
async function doRetrain() {
  retraining.value = true
  retrainMsg.value = ''
  try {
    const res = await api.l3Retrain(flywheelLang.value)
    if (res.swapped) {
      const before = res.before_f1 != null ? res.before_f1.toFixed(3) : '—'
      const after = res.after_f1 != null ? res.after_f1.toFixed(3) : '—'
      retrainMsg.value = `模型已重训练并热切换（dev F1 ${before} → ${after}；合并 ${res.samples?.confirmed ?? 0} 条确认样本）`
      toast(retrainMsg.value, 'ok')
    } else {
      retrainMsg.value = `重训练完成但未切换模型：${res.reason ?? '无法评测'}（保留旧模型）`
      toast(retrainMsg.value, 'warn')
    }
    await loadFlywheelStatus()
  } catch (e) {
    retrainMsg.value = `重训练失败：${errMsg(e)}`
    toast(retrainMsg.value, 'danger')
  } finally {
    retraining.value = false
    if (retrainTimer) clearTimeout(retrainTimer)
    retrainTimer = setTimeout(() => {
      retrainMsg.value = ''
    }, 20000)
  }
}

// 特征模型启用时自动加载飞轮状态
watch(l3Enabled, (v) => {
  if (v) void loadFlywheelStatus()
})
onMounted(() => {
  void loadFlywheelStatus()
})

// ── 设置测试：各项配置的连通性 / 可用性测试（探测本身只读；表单有未保存修改时先自动保存） ──

/** 测试目标 → 展示名称（「测试全部」逐项结果用） */
const TEST_LABELS: Record<string, string> = {
  hanlp: '文本处理服务',
  llm: 'AI 复查',
  decision: '快速定案',
  l3: '快速定案',
  embedding: '知识查询',
  tables_dir: '决策表目录',
  lexicons_dir: '词表目录',
  all: '全部项目',
}

const testing = ref<Partial<Record<SettingsTestTarget, boolean>>>({})
const testResults = ref<Partial<Record<SettingsTestTarget, SettingsTestResult>>>({})

// ── 服务总览（T2-3）：每行一个服务（类型 / 当前状态 / 最近测试结论），配置前先看全景 ──

interface OverviewRow {
  name: string
  kind: '进程内' | '外部服务' | '本地目录'
  state: string
  stateType: 'success' | 'info' | 'warning'
  test: SettingsTestResult | null
}

const serviceColumns: LColumn<OverviewRow>[] = [
  { key: 'name', label: '服务', width: '150px' },
  { key: 'kind', label: '类型', width: '90px' },
  { key: 'state', label: '状态', width: '170px' },
  { key: 'test', label: '最近测试' },
]
const stateTone = (t: OverviewRow['stateType']) =>
  t === 'success' ? ('ok' as const) : t === 'warning' ? ('warn' as const) : ('neutral' as const)

const serviceOverview = computed<OverviewRow[]>(() => {
  const t = testResults.value
  const row = (
    name: string,
    kind: OverviewRow['kind'],
    state: string,
    stateType: OverviewRow['stateType'],
    target?: SettingsTestTarget,
  ): OverviewRow => ({ name, kind, state, stateType, test: target ? (t[target] ?? null) : null })
  return [
    row('本地检查', '进程内', '始终可用', 'success'),
    row(
      '文本处理服务（HanLP）',
      '外部服务',
      offlineMode.value ? '离线模式（仅本地）' : hanlpEndpoint.value.trim() ? '已配置' : '未配置',
      offlineMode.value ? 'info' : hanlpEndpoint.value.trim() ? 'success' : 'warning',
      'hanlp',
    ),
    // 决策模型：zh v2 达标后重新列示（默认开启，#11.19 补五）
    row(
      '决策模型',
      '外部服务',
      decisionEnabled.value
        ? decisionEndpoint.value.trim()
          ? '已启用'
          : '已启用（未填地址）'
        : '未启用',
      decisionEnabled.value ? 'success' : 'info',
      'decision',
    ),
    row(
      '快速定案（特征模型）',
      '外部服务',
      l3Enabled.value
        ? l3Endpoint.value.trim()
          ? '已启用'
          : '已启用（未填地址）'
        : '未启用',
      l3Enabled.value ? 'success' : 'info',
      'l3',
    ),
    row('AI 复查（LLM）', '外部服务', llmEnabled.value ? '已配置' : '未配置', llmEnabled.value ? 'success' : 'warning', 'llm'),
    row(
      '知识查询（向量检索）',
      '外部服务',
      embeddingEndpoint.value.trim() ? '已配置' : '未配置（知识查询停用）',
      embeddingEndpoint.value.trim() ? 'success' : 'info',
      'embedding',
    ),
    row(
      '数据目录（表 / 词表）',
      '本地目录',
      tablesDir.value.trim() && lexiconsDir.value.trim() ? '已配置' : '未配置',
      tablesDir.value.trim() && lexiconsDir.value.trim() ? 'success' : 'warning',
      'tables_dir',
    ),
  ]
})

/** 对指定配置项发起一次真实探测（服务可达 + 响应符合预期）
 *
 *  测试探测的是服务端**已保存**的设置；若表单有未保存修改，
 *  先自动保存再测试（所见即所测），避免「表单已填但测试报未配置」的误解。
 */
async function runTest(target: SettingsTestTarget) {
  if (formIsDirty()) {
    const ok = await save()
    if (!ok) {
      testResults.value[target] = {
        target,
        ok: false,
        message: '设置未保存（自动保存失败），未发起测试；请修正错误后重试',
        latency_ms: 0,
      }
      return
    }
  }
  testing.value[target] = true
  try {
    testResults.value[target] = await api.testSetting(target)
  } catch (e) {
    testResults.value[target] = {
      target,
      ok: false,
      message: `测试请求失败：${errMsg(e)}`,
      latency_ms: 0,
    }
  } finally {
    testing.value[target] = false
  }
}

function setMediaLevel(v: string | number) {
  mediaLevel.value = Number(v)
}
function setMediaApplyMode(v: string | number) {
  mediaApplyMode.value = String(v)
}
function setLlmModelId(v: string | number) {
  llmModelId.value = String(v)
}
function setProfileCefr(v: string | number) {
  profileCefr.value = String(v)
}
</script>

<template>
  <div class="settings-wrap">
    <!-- 工作场景与角色（能力位）：核心工作区共享，三个扩展按需开启 -->
    <LSection id="set-caps" class="settings-card" title="工作场景与角色" desc="检测工作区对所有人共享；以下扩展按需开启">
      <div class="cap-row">
        <div>
          <b>语言学习</b>
          <div class="sub">「练习与复习」入口、学习者档案（按母语与语言水平个性化讲解）</div>
        </div>
        <LSwitch :model-value="store.caps.learning" aria-label="语言学习" @update:model-value="(v: boolean) => onCapToggle('learning', v)" />
      </div>
      <div class="cap-row">
        <div>
          <b>编校工作</b>
          <div class="sub">载体审校工作区：图片文字提取 → 检测 → 落点画框 → 回写 / 正误表导出</div>
        </div>
        <LSwitch :model-value="store.caps.proofreading" aria-label="编校工作" @update:model-value="(v: boolean) => onCapToggle('proofreading', v)" />
      </div>
      <div class="cap-row">
        <div>
          <b>平台运营</b>
          <div class="sub">规则管理、知识维护、进化中心，以及检测能力的高级设置（面向平台维护者）</div>
        </div>
        <LSwitch :model-value="store.caps.ops" aria-label="平台运营" @update:model-value="(v: boolean) => onCapToggle('ops', v)" />
      </div>
      <div v-if="showOpsToken" class="ops-unlock">
        <input
          v-model="opsToken"
          class="l-input"
          :type="showOpsKey ? 'text' : 'password'"
          placeholder="运营 token（服务端 GEC_TABLES_UPDATE_TOKEN）"
        />
        <LButton variant="ghost" size="sm" @click="showOpsKey = !showOpsKey">{{ showOpsKey ? '隐藏' : '显示' }}</LButton>
        <LButton variant="primary" :loading="opsUnlocking" @click="unlockOps">验证并解锁</LButton>
        <span v-if="opsUnlockError" class="apply-err">{{ opsUnlockError }}</span>
      </div>
      <!-- 解锁后的账号管理串联：运营 token 与超管密钥是两套凭证，试用账号不在本卡管 -->
      <div v-if="store.caps.ops && !isTauri && identity.gateway" class="ops-account">
        <span class="sub">
          {{
            identity.admin
              ? '试用账号管理在「接入」页的密钥分发：每行「建账号」生成登录用户名与一次性临时口令（首登催改），“重置口令”作废旧口令与在途会话。本卡解锁的运营 token 只管规则 / 知识 / 进化等写端点——账号写端点归超管密钥。'
              : '本卡解锁的运营 token 只管规则 / 知识 / 进化等写端点；试用账号（用户名 + 口令）需用超管密钥登录后，在「接入」页密钥分发每行的「账号」列创建或重置。'
          }}
        </span>
        <LButton size="sm" variant="ghost" @click="store.requestNav('access')">
          去接入页
        </LButton>
      </div>
      <p class="desc">
        能力位是本机偏好（仅保存在此设备）。超管身份下平台运营自动开启（写端点本身受服务端鉴权保护）；
        其余情形 web 形态解锁需 token 验证，未解锁时相关写端点仍受服务端 token 保护。
      </p>
    </LSection>

    <!-- 我的档案（学习者画像）：母语 + 自评 CEFR → LLM 讲解自动个性化 -->
    <LSection id="set-profile" class="settings-card" title="我的档案" desc="讲解时会自动考虑你的母语和语言水平：深度与示例自动贴合">
      <div class="field">
        <span class="field__label">母语</span>
        <div class="field__ctl">
          <input
            v-model="profileNative"
            class="l-input w-280"
            list="set-native-options"
            placeholder="如：中文（留空 = 不注入）"
          />
          <datalist id="set-native-options">
            <option v-for="n in NATIVE_OPTIONS" :key="n" :value="n" />
          </datalist>
        </div>
      </div>
      <div class="field">
        <span class="field__label">语言水平</span>
        <div class="field__ctl">
          <LSelect
            class="w-280"
            aria-label="语言水平"
            :model-value="profileCefr"
            :options="cefrOptions"
            @update:model-value="setProfileCefr"
          />
        </div>
      </div>
      <div class="actions">
        <LButton variant="primary" :loading="profileSaving" @click="saveProfile">保存档案</LButton>
        <!-- 删除会话：行内两步确认（弹窗配额只留给 发布 / 删资源包 / 回滚） -->
        <template v-if="pendingDeleteCount === null">
          <LButton
            variant="danger"
            :loading="sessionDeleting"
            :disabled="!store.sessionId"
            @click="askDeleteCurrentSession"
          >
            删除当前会话
          </LButton>
        </template>
        <span v-else class="confirm-inline">
          将移除该会话的 {{ pendingDeleteCount }} 条错题；你的复习进度会保留。
          <LButton variant="danger" size="sm" @click="confirmDeleteCurrentSession">删除会话</LButton>
          <LButton size="sm" @click="pendingDeleteCount = null">取消</LButton>
        </span>
        <span v-if="profileError" class="apply-err">{{ profileError }}</span>
        <span v-else class="hint">
          档案挂在当前会话（{{ store.sessionId || '保存时自动生成' }}）；
          讲解未指定档位时，自动采用档案里的语言水平
        </span>
      </div>
    </LSection>

    <!-- 桌面端：进程内运行（默认离线）；下方表单所有项均可修改（即时生效 + 持久化到本地设置文件） -->
    <LSection v-if="isTauri" class="settings-card" title="桌面端运行方式" desc="检测引擎 / 决策表 / 用户档案均在进程内运行（默认离线）">
      <template #actions>
        <LTag tone="ok">进程内 · 可编辑</LTag>
      </template>
      <p class="desc">
        下方表单中的所有项均可直接修改：保存后即时生效（检测器 / LLM 讲解服务 / 决策表热重载），
        并持久化到本地设置文件（重启后仍生效，无需再设置环境变量或重启应用）。
      </p>
    </LSection>

    <!-- 服务总览（T2-3）：配置前先看全景——哪些服务进程内 / 外部、是否已配置、最近测试结论 -->
    <LSection class="settings-card" title="服务总览" desc="配置前先看全景：每行一个服务（类型 / 当前状态 / 最近测试）">
      <LTable :columns="serviceColumns" :rows="serviceOverview" compact>
        <template #col-state="{ row }">
          <LTag :tone="stateTone(row.stateType)">{{ row.state }}</LTag>
        </template>
        <template #col-test="{ row }">
          <template v-if="row.test">
            <span :class="row.test.ok ? 'apply-ok' : 'apply-err'">
              {{ row.test.ok ? '✓ 通过' : '✗ 未通过' }}：{{ row.test.message }}
            </span>
            <span class="sub">（{{ row.test.latency_ms }} ms）</span>
          </template>
          <span v-else class="sub">未测试</span>
        </template>
      </LTable>
      <div class="actions actions--mt">
        <LButton :loading="testing.all" @click="runTest('all')">测试全部</LButton>
        <span class="hint">只读探测，不修改配置；逐项结果也可在各服务卡片内单独测试</span>
      </div>
    </LSection>

    <!-- 依赖服务（启动自检 + 监督器）：拉不起来的不能静默——逐项展示原因，
         补救两条路：本机可拉起的「拉起服务」；在其他机器部署的填新地址重测 -->
    <LSection v-if="store.caps.ops && store.deps.length" id="set-deps" class="settings-card"
      title="依赖服务" desc="服务启动后按资源自动拉起；未能就绪的服务逐项说明原因，等待处理（不会静默缺位）">
      <template #actions>
        <LButton variant="ghost" size="sm" :loading="depsRecheck" @click="recheckDeps">全部重新探测</LButton>
      </template>
      <div v-for="d in store.deps" :key="d.id" class="dep-row">
        <div class="dep-row__main">
          <LDot :tone="DEP_DOT_TONE[d.state]" />
          <span class="dep-row__label">{{ d.label }}</span>
          <LTag :tone="DEP_STATE_TONE[d.state]">{{ DEP_STATE_TEXT[d.state] }}</LTag>
          <LTag v-if="d.auto" tone="accent">自动拉起</LTag>
          <span class="sub">{{ d.endpoint || '未配置' }}</span>
        </div>
        <!-- 决策模型 ok 态也展示探测结果（含门控判据，审计 F7）；其余项 ok 态保持安静 -->
        <p v-if="(d.reason || d.hint) && (d.state !== 'ok' || d.id === 'decision')" class="hint">{{ d.reason || d.hint }}</p>
        <p v-if="depsMsg[d.id]" class="sub">{{ depsMsg[d.id] }}</p>
        <div v-if="d.managed && (d.state === 'failed' || d.state === 'skipped')" class="actions actions--mt">
          <LButton size="sm" :disabled="depsBusy[d.id]" @click="startDep(d)">拉起服务</LButton>
          <span class="hint">本机资源不足或上次拉起失败；若这台机器实在放不下，可部署到其他机器后填地址</span>
        </div>
        <div v-if="(d.id === 'decision' || d.id === 'l3') && (d.state === 'failed' || d.state === 'skipped')" class="dep-alt">
          <LInput v-model="depsAlt[d.id]" :placeholder="`该服务部署在其他机器？填地址（如 http://192.168.1.x:${d.id === 'decision' ? '4804' : '4806'}）`" />
          <LButton size="sm" :disabled="!((depsAlt[d.id] || '').trim()) || depsBusy[d.id]" @click="saveDepEndpoint(d)">保存其他地址并重测</LButton>
        </div>
      </div>
      <div class="hint">
        状态来自服务端监督器快照；拉起失败或资源不足时会保留原因供排查（日志见服务端 log/supervisor/）。
        如果某个服务你计划部署在其他机器上，请填写对应地址——不要让功能静默缺位。
      </div>
    </LSection>

    <LSection id="set-nlp" class="settings-card" title="文本处理服务（HanLP）" desc="中文 / 英文检测共用的分词与词性标注服务">
      <div class="field">
        <span class="field__label">服务地址</span>
        <div class="field__ctl">
          <LInput class="w-420" v-model="hanlpEndpoint" placeholder="http://127.0.0.1:8000" />
        </div>
      </div>
      <div class="field">
        <span class="field__label">离线模式</span>
        <div class="field__ctl">
          <LSwitch v-model="offlineMode" aria-label="离线模式" />
          <span class="sub">开启后检测只用本地规则词表，不调用该服务（离线可用）</span>
        </div>
      </div>
      <div class="field">
        <span class="field__label">服务测试</span>
        <div class="field__ctl">
          <LButton :loading="testing.hanlp" @click="runTest('hanlp')">测试服务</LButton>
          <span v-if="testResults.hanlp" :class="testResults.hanlp.ok ? 'apply-ok' : 'apply-err'">
            {{ testResults.hanlp.message }}
            <span class="sub">（{{ testResults.hanlp.latency_ms }} ms）</span>
          </span>
        </div>
      </div>
      <div class="hint">
        保存后即时重建中文 / 英文 / 日文检测器（热更新，无需重启服务）；
        服务地址与离线开关由此界面显式控制，无需再设置 HANLP_ENDPOINT / GEC_OFFLINE 环境变量。
      </div>
    </LSection>

    <LSection
      v-if="store.caps.ops"
      id="set-decision"
      class="settings-card"
      title="决策模型"
      desc="规则之后、AI 复查之前的句级决策层（zh v2 双头门控）：规则未命中的句子逐句交它判定"
    >
      <template #actions>
        <LTag :tone="decisionEnabled ? 'ok' : 'neutral'">{{ decisionEnabled ? '已启用' : '未启用' }}</LTag>
      </template>
      <div class="field">
        <span class="field__label">启用决策层</span>
        <div class="field__ctl">
          <LSwitch v-model="decisionEnabled" :disabled="decisionApplying" aria-label="启用决策层" />
          <span class="sub">切换即自动保存（即时生效 + 持久化），检测状态条 ② 随之同步；默认开启</span>
        </div>
      </div>
      <div v-if="decisionApplyMsg" class="field">
        <span class="field__label"></span>
        <div class="field__ctl">
          <span :class="decisionApplyMsg.startsWith('保存失败') ? 'apply-err' : 'apply-ok'">{{ decisionApplyMsg }}</span>
        </div>
      </div>
      <div class="field">
        <span class="field__label">服务地址</span>
        <div class="field__ctl">
          <LInput class="w-420" v-model="decisionEndpoint" placeholder="http://127.0.0.1:4804" />
        </div>
      </div>
      <div class="field">
        <span class="field__label">模型</span>
        <div class="field__ctl">
          <LInput class="w-420" v-model="decisionModel" placeholder="vegaml-0.8b" />
        </div>
      </div>
      <div class="field">
        <span class="field__label">超时（秒）</span>
        <div class="field__ctl">
          <input v-model="decisionTimeoutNum" class="l-input w-120" type="number" min="1" max="120" />
        </div>
      </div>
      <div class="field">
        <span class="field__label">弃权阈值</span>
        <div class="field__ctl">
          <input v-model="decisionThresholdNum" class="l-input w-120" type="number" min="0" max="1" step="0.05" />
          <span class="sub">句级把握落在灰区 [0.25, 阈值) 的句子弃权交 AI 复查，宁可漏报不误报</span>
        </div>
      </div>
      <div class="field">
        <span class="field__label">生效来源</span>
        <div class="field__ctl">
          <span class="sub">当前配置来自{{ decisionSourceText }}（优先级：设置文件 > 环境变量 GEC_DEC_* > 内置默认）</span>
        </div>
      </div>
      <div class="field">
        <span class="field__label">服务测试</span>
        <div class="field__ctl">
          <LButton :loading="testing.decision" @click="runTest('decision')">测试决策服务</LButton>
          <span v-if="testResults.decision" :class="testResults.decision.ok ? 'apply-ok' : 'apply-err'">
            {{ testResults.decision.message }}
            <span class="sub">（{{ testResults.decision.latency_ms }} ms）</span>
          </span>
        </div>
      </div>
      <div class="hint">
        中文后端为双头门控（zh v2，EVOLUTION #11.19）：句头判有错 ∧ 字头定位同步过阈时，
        错误定位到具体缺失字（块矩阵「② 决策模型」列按字级 span 精确命中）；
        句头仅落在弃权线上方的灰区句子给句级提示；更低则弃权交 AI 复查。
        门控阈值随模型 meta.json（默认句 ≥0.95 ∧ 字 ≥0.30），实际判据见上方
        「测试决策服务」结果；提示「服务进程为旧版」时重启决策模型服务即可显示实际门控。
      </div>
    </LSection>

    <LSection id="set-embedding" class="settings-card" title="知识查询（向量检索）" desc="OpenAI 兼容接口，用于易错词对与错例的智能检索">
      <template #actions>
        <LTag :tone="embeddingEndpoint ? 'ok' : 'neutral'">{{ embeddingEndpoint ? '已配置' : '未配置' }}</LTag>
      </template>
      <div class="field">
        <span class="field__label">服务地址</span>
        <div class="field__ctl">
          <LInput
            class="w-420"
            v-model="embeddingEndpoint"
            placeholder="http://127.0.0.1:8080/v1（留空 = 停用知识查询）"
          />
        </div>
      </div>
      <div class="field">
        <span class="field__label">API Key</span>
        <div class="field__ctl">
          <input
            v-model="embeddingApiKey"
            class="l-input w-420"
            :type="showEmbKey ? 'text' : 'password'"
            placeholder="Bearer token（留空 = 无认证）"
          />
          <LButton variant="ghost" size="sm" @click="showEmbKey = !showEmbKey">{{ showEmbKey ? '隐藏' : '显示' }}</LButton>
        </div>
      </div>
      <div class="field">
        <span class="field__label">模型</span>
        <div class="field__ctl">
          <LInput class="w-420" v-model="embeddingModel" placeholder="text-embedding-3-small" />
        </div>
      </div>
      <div class="field">
        <span class="field__label">服务测试</span>
        <div class="field__ctl">
          <LButton :loading="testing.embedding" @click="runTest('embedding')">测试嵌入服务</LButton>
          <span v-if="testResults.embedding" :class="testResults.embedding.ok ? 'apply-ok' : 'apply-err'">
            {{ testResults.embedding.message }}
            <span class="sub">（{{ testResults.embedding.latency_ms }} ms）</span>
          </span>
        </div>
      </div>
      <div class="hint">
        配置后，检测流水线 ②.5 层将使用嵌入向量从学习知识库中召回相似错误模式（top-3，余弦相似度 ≥ 0.7）。
        服务不可用时向量召回静默跳过，不影响其他检测层。
      </div>
    </LSection>

    <!-- 编校场景专属：载体审校服务配置（OCR 等） -->
    <LSection v-if="store.caps.proofreading" id="set-media" class="settings-card" title="载体审校（图片 / PDF / Word）" desc="副本隔离：只改副本，**永不回写原件**（Web / 桌面同一份设置）">
      <template #actions>
        <LTag :tone="mediaEnabled ? 'ok' : 'neutral'">{{ mediaEnabled ? '已开启' : '未开启' }}</LTag>
      </template>
      <div class="field">
        <span class="field__label">总开关</span>
        <div class="field__ctl">
          <LSwitch v-model="mediaEnabled" aria-label="开启载体审校" />
          <span>开启载体审校</span>
        </div>
      </div>
      <div class="field">
        <span class="field__label">工作模式</span>
        <div class="field__ctl">
          <LSegmented v-model="mediaMode" :options="mediaModeOptions" />
        </div>
      </div>
      <div class="field">
        <span class="field__label"></span>
        <div class="field__ctl">
          <span class="sub">
            简单模式不装任何外部工具：读文本、出清单与正误表照样能用，但载体回写不可用；
            高级模式交给 PhotoCraft / PdfCraft / WordCraft，可回写载体（缺 CLI 时后端会说明原因，不会静默换档）。
          </span>
        </div>
      </div>
      <div class="field">
        <span class="field__label">抽取档位</span>
        <div class="field__ctl field__ctl--checks">
          <LCheckbox v-model="mediaUseStructure" label="A 档 · 结构层（文字层 / docx run，真值）" />
          <LCheckbox v-model="mediaUseVision" label="B 档 · 多模态视觉" />
          <LCheckbox v-model="mediaUseOcr" label="C 档 · 本地 OCR" />
        </div>
      </div>
      <div class="field">
        <span class="field__label">视觉模型</span>
        <div class="field__ctl">
          <LInput class="w-420" v-model="mediaVisionModel" placeholder="留空 = 复用 LLM 参数里的模型" />
        </div>
      </div>
      <div class="field">
        <span class="field__label">检查模式（默认）</span>
        <div class="field__ctl">
          <LSelect
            class="w-200"
            aria-label="检查模式（默认）"
            :model-value="mediaLevel"
            :options="mediaLevelOptions"
            @update:model-value="setMediaLevel"
          />
          <LSwitch v-model="mediaForceLlm" aria-label="强制 LLM 兜底" />
          <span>强制 LLM 兜底</span>
        </div>
      </div>
      <div class="field">
        <span class="field__label">默认回写</span>
        <div class="field__ctl">
          <LSelect
            class="w-240"
            aria-label="默认回写"
            :model-value="mediaApplyMode"
            :options="mediaApplyModeOptions"
            @update:model-value="setMediaApplyMode"
          />
        </div>
      </div>
      <div class="field">
        <span class="field__label">job 目录</span>
        <div class="field__ctl">
          <LInput
            class="w-420"
            v-model="mediaWorkdir"
            placeholder="留空 = 当前目录下 media-jobs/（改目录需重启）"
          />
        </div>
      </div>
      <div class="hint">
        未开启时载体面板不会静默开工：探测卡片会列出缺什么（协作工具 CLI / 视觉档凭据）。
        三家 CLI 路径、视觉上传压缩与超时参数留在设置文件与环境变量里
        （<code>PHOTOCRAFT_CLI</code> / <code>GEC_MEDIA_VISION_MAX_EDGE</code> …）。
      </div>
    </LSection>

    <LSection id="set-llm" class="settings-card" title="AI 复查（LLM）" desc="OpenAI 兼容接口，用于智能复核与讲解 / 练习生成">
      <template #actions>
        <LTag :tone="llmEnabled ? 'ok' : 'neutral'">{{ llmEnabled ? '已配置' : '未配置' }}</LTag>
      </template>
      <div class="banner">
        启用后，待检测与讲解的文本全文会发送到下方配置的模型端点——请使用可信服务，勿粘贴机密文本。
      </div>
      <div class="arm-group">
        <div class="arm-group__title">主判臂<span class="hint">③ 检测 / 讲解 / 练习生成的模型配置</span></div>
        <div class="field">
          <span class="field__label">纠错模型</span>
          <div class="field__ctl field__ctl--wrap">
            <LSelect
              class="w-420"
              aria-label="纠错模型"
              :model-value="llmModelId"
              :options="modelOptions"
              @update:model-value="setLlmModelId"
            />
            <span class="hint hint--block">
              从 `prompts/models.json` 注册表选择纠错模型预设：
              `generic` 用本组 Base URL / 模型（你的 OpenAI 兼容端点，JSON 输出）；
              `cec4` / `cec3` 用预设的本地 vllm 端点与模型名（纯文本输出，自动适配）。保存后即时生效。
            </span>
          </div>
        </div>
        <div class="field">
          <span class="field__label">Base URL</span>
          <div class="field__ctl">
            <LInput class="w-420" v-model="llmBaseUrl" placeholder="https://api.openai.com/v1" />
          </div>
        </div>
        <div class="field">
          <span class="field__label">API Key</span>
          <div class="field__ctl">
            <input
              v-model="llmApiKey"
              class="l-input w-420"
              :type="showLlmKey ? 'text' : 'password'"
              placeholder="sk-…（留空 = 未配置 LLM，讲解回退静态建议）"
            />
            <LButton variant="ghost" size="sm" @click="showLlmKey = !showLlmKey">{{ showLlmKey ? '隐藏' : '显示' }}</LButton>
          </div>
        </div>
        <div class="field">
          <span class="field__label">模型</span>
          <div class="field__ctl">
            <LInput class="w-420" v-model="llmModel" placeholder="gpt-4o-mini" />
          </div>
        </div>
        <div class="field">
          <span class="field__label">超时（秒）</span>
          <div class="field__ctl">
            <input v-model="llmTimeoutNum" class="l-input w-120" type="number" min="1" max="600" />
            <span class="hint">连接 / 单次读超时；影子臂同样使用该超时</span>
          </div>
        </div>
        <div class="field">
          <span class="field__label">提示词目录</span>
          <div class="field__ctl">
            <LInput class="w-420" v-model="llmPromptsDir" placeholder="prompts" />
          </div>
        </div>
        <div class="field">
          <span class="field__label">服务测试</span>
          <div class="field__ctl">
            <LButton :loading="testing.llm" @click="runTest('llm')">测试主判臂</LButton>
            <span v-if="testResults.llm" :class="testResults.llm.ok ? 'apply-ok' : 'apply-err'">
              {{ testResults.llm.message }}
              <span class="sub">（{{ testResults.llm.latency_ms }} ms）</span>
            </span>
          </div>
        </div>
      </div>
      <div class="field">
        <span class="field__label">自动双判</span>
        <div class="field__ctl">
          <LSwitch v-model="llmDualJudge" aria-label="自动双判" />
          <span class="sub">开启后每次 ③ LLM 检测并行多跑一路下方影子臂，双臂判定落日志（臂间对照数据），影子臂失败不影响结果。上下两组配置完全独立、填写顺序无关：各自填自己的端点 / 模型 / 密钥即可。</span>
        </div>
      </div>
      <div class="arm-group" :class="{ 'arm-group--off': !llmDualJudge }">
        <div class="arm-group__title">
          影子臂（并行对照）
          <LTag v-if="!llmDualJudge" tone="neutral">未启用</LTag>
        </div>
        <div class="field">
          <span class="field__label">影子端点 Base URL</span>
          <div class="field__ctl">
            <LInput class="w-420" v-model="llmDualBaseUrl" placeholder="https://llm.jiulu.ltd/v1（留空 = 沿用注册表预设 arm3）" />
          </div>
        </div>
        <div class="field">
          <span class="field__label">影子模型</span>
          <div class="field__ctl">
            <LInput class="w-420" v-model="llmDualModelName" placeholder="arm3-l2kto-v7" />
            <span class="hint hint--block">影子端点与影子模型需成对填写：两者都填 = 影子臂走这套独立配置；两者都留空 = 沿用注册表预设 `arm3`；只填一个会被拒绝保存。影子臂与主臂同端点同模型时双判自动跳过（同臂自对没有对照价值）；超时复用上方设置。</span>
          </div>
        </div>
        <div class="field">
          <span class="field__label">影子 API Key</span>
          <div class="field__ctl">
            <input
              v-model="llmDualApiKey"
              class="l-input w-420"
              :type="showDualKey ? 'text' : 'password'"
              placeholder="sk-…（留空 = 复用主判臂密钥；跨网关时填影子侧自己的密钥）"
            />
            <LButton variant="ghost" size="sm" @click="showDualKey = !showDualKey">{{ showDualKey ? '隐藏' : '显示' }}</LButton>
          </div>
        </div>
        <div class="field">
          <span class="field__label">服务测试</span>
          <div class="field__ctl">
            <LButton :loading="testing.llm_dual" @click="runTest('llm_dual')">测试影子臂</LButton>
            <span
              v-if="testResults.llm_dual"
              :class="testResults.llm_dual.skipped ? 'sub' : testResults.llm_dual.ok ? 'apply-ok' : 'apply-err'"
            >
              {{ testResults.llm_dual.message }}
              <span v-if="!testResults.llm_dual.skipped" class="sub">（{{ testResults.llm_dual.latency_ms }} ms）</span>
            </span>
          </div>
        </div>
      </div>
      <div class="hint">
        LLM 请求模式（固定默认，不可修改）：
        <LTag tone="accent">前端：{{ llmModes?.frontend }}</LTag>
        <LTag tone="neutral">外部 API：{{ llmModes?.external_api }}</LTag>
        提示词目录变更时模板库即时重载，无需重启。
      </div>
    </LSection>

    <!-- 高级（运营）：L3 特征模型层 -->
    <LSection v-if="store.caps.ops" id="set-l3" class="settings-card" title="快速定案（特征模型）" desc="深度扫描的主力：本地先确认有把握的，AI 只复核拿不准的少数">
      <template #actions>
        <LTag :tone="l3Enabled ? 'ok' : 'neutral'">{{ l3Enabled ? '已启用' : '未启用' }}</LTag>
      </template>
      <div class="field">
        <span class="field__label">启用特征模型</span>
        <div class="field__ctl">
          <LSwitch v-model="l3Enabled" :disabled="l3Applying" aria-label="启用特征模型" />
          <span class="sub">
            切换即自动保存（即时生效 + 持久化）；
            开启后 level 3 先跑特征模型（句级特征分类 + 模式化定位 + 机械修正），
            弃权 / 服务不可用时才调用 LLM 兜底
          </span>
        </div>
      </div>
      <div v-if="l3ApplyMsg" class="field">
        <span class="field__label"></span>
        <div class="field__ctl">
          <span :class="l3ApplyMsg.startsWith('保存失败') ? 'apply-err' : 'apply-ok'">
            {{ l3ApplyMsg }}
          </span>
        </div>
      </div>
      <div class="field">
        <span class="field__label">服务地址</span>
        <div class="field__ctl">
          <LInput
            class="w-420"
            v-model="l3Endpoint"
            placeholder="http://127.0.0.1:4806（留空 = 仅环境变量 GEC_L3_ENDPOINT 可启用）"
          />
        </div>
      </div>
      <div class="field">
        <span class="field__label">模型</span>
        <div class="field__ctl">
          <LInput class="w-420" v-model="l3Model" placeholder="feature-0.1" />
        </div>
      </div>
      <div class="field">
        <span class="field__label">超时（秒）</span>
        <div class="field__ctl">
          <input v-model="l3TimeoutNum" class="l-input w-120" type="number" min="1" max="120" />
        </div>
      </div>
      <div class="field">
        <span class="field__label">弃权阈值</span>
        <div class="field__ctl">
          <input v-model="l3ThresholdNum" class="l-input w-120" type="number" min="0" max="1" step="0.05" />
          <span class="sub">把握程度落在某个区间的句子会交给 AI 复核；该值越高，交给 AI 复核的句子越多</span>
        </div>
      </div>
      <div class="field">
        <span class="field__label">服务测试</span>
        <div class="field__ctl">
          <LButton :loading="testing.l3" @click="runTest('l3')">测试特征服务</LButton>
          <span v-if="testResults.l3" :class="testResults.l3.ok ? 'apply-ok' : 'apply-err'">
            {{ testResults.l3.message }}
            <span class="sub">（{{ testResults.l3.latency_ms }} ms）</span>
          </span>
        </div>
      </div>
      <div class="hint">
        特征模型（feature-model 服务，gold 数据训练）在 LLM 之前运行：
        高置信检出错误 → 直接产出错误项（source = feature，含机械修正建议）；
        明确无错 → 放行；灰区弃权 / 服务宕机 → 回退 LLM 兜底（纯长尾）。
        保存后即时重建检测器，无需重启。
      </div>
    </LSection>

    <!-- L3 训练飞轮（三期闭环）：确认样本积累 → 重训练提醒 → 重训练 → 热切换 -->
    <LSection v-if="l3Enabled && store.caps.ops" class="settings-card" title="快速定案 · 训练飞轮" desc="你确认采纳的纠错会持续汇入训练集，模型随使用变强">
      <template #actions>
        <LTag v-if="flywheelStatus?.retrain_ready" tone="warn">建议重训练</LTag>
        <LTag v-else tone="neutral">积累中</LTag>
      </template>
      <template v-if="flywheelStatus">
        <div class="field">
          <span class="field__label">语言</span>
          <div class="field__ctl">
            <LSegmented v-model="flywheelLang" :options="flywheelLangOptions" @update:model-value="loadFlywheelStatus()" />
          </div>
        </div>
        <div class="field">
          <span class="field__label">确认样本</span>
          <div class="field__ctl">
            <b>{{ flywheelStatus.samples }}</b> 条
            <span class="sub">（你采纳的纠错 = 正面样本；AI 复查确认没问题的句子 = 反面样本；按文本去重，上限 5000）</span>
          </div>
        </div>
        <div class="field">
          <span class="field__label">当前模型</span>
          <div class="field__ctl">
            <template v-if="flywheelStatus.service_available">
              {{ flywheelStatus.model || '—' }}
              <span v-if="flywheelStatus.f1 != null" class="sub">（dev F1 {{ flywheelStatus.f1.toFixed(3) }}）</span>
            </template>
            <span v-else class="apply-err">feature-model 服务不可达（仅本地样本统计可用）</span>
          </div>
        </div>
        <div class="field">
          <span class="field__label">上次重训练</span>
          <div class="field__ctl">
            {{ flywheelStatus.last_retrain_at ?? '从未' }}
          </div>
        </div>
        <div class="field">
          <span class="field__label">重训练</span>
          <div class="field__ctl">
            <LButton
              variant="primary"
              :loading="retraining"
              :disabled="!flywheelStatus.service_available"
              @click="doRetrain"
            >
              重训练并热切换
            </LButton>
            <span class="sub">
              合并确认样本 → 重训练 → dev 集评测；F1 显著退化（&gt; 0.02）时保留旧模型
            </span>
          </div>
        </div>
        <div v-if="retrainMsg" class="field">
          <span class="field__label"></span>
          <div class="field__ctl">
            <span :class="retrainMsg.startsWith('重训练失败') ? 'apply-err' : 'apply-ok'">{{ retrainMsg }}</span>
          </div>
        </div>
      </template>
      <div v-else class="hint hint--row">
        <span v-if="flywheelLoading" class="spin" aria-hidden="true"></span>
        飞轮状态不可用（feature-model 服务未启动或加载失败）
      </div>
      <div class="hint">
        采集来源：① 你在检测页「采纳」的 AI 检出的错误（正面样本，自动汇入）；
        ② 检测没发现错误（含 AI 复查确认没问题）时的原句自动汇入（反面样本）。
        提醒条件：上次重训练后新增 ≥ 10 条（从未训练过则累计 ≥ 30）且距上次重训练 ≥ 7 天。
      </div>
    </LSection>

    <!-- 高级（运营）：目录配置 -->
    <LSection v-if="store.caps.ops" id="set-dirs" class="settings-card" title="数据目录" desc="决策表 / 规则词表目录（保存后即时重载）">
      <div class="field">
        <span class="field__label">决策表目录</span>
        <div class="field__ctl">
          <LInput class="w-420" v-model="tablesDir" placeholder="decision-tables" />
        </div>
      </div>
      <div class="field">
        <span class="field__label">词表目录</span>
        <div class="field__ctl">
          <LInput class="w-420" v-model="lexiconsDir" placeholder="lexicons" />
        </div>
      </div>
      <div class="field">
        <span class="field__label">目录测试</span>
        <div class="field__ctl">
          <LButton :loading="testing.tables_dir" @click="runTest('tables_dir')">测试决策表目录</LButton>
          <LButton :loading="testing.lexicons_dir" @click="runTest('lexicons_dir')">测试词表目录</LButton>
        </div>
      </div>
      <div v-if="testResults.tables_dir" class="field">
        <span class="field__label"></span>
        <div class="field__ctl">
          <span :class="testResults.tables_dir.ok ? 'apply-ok' : 'apply-err'">
            决策表：{{ testResults.tables_dir.message }}
            <span class="sub">（{{ testResults.tables_dir.latency_ms }} ms）</span>
          </span>
        </div>
      </div>
      <div v-if="testResults.lexicons_dir" class="field">
        <span class="field__label"></span>
        <div class="field__ctl">
          <span :class="testResults.lexicons_dir.ok ? 'apply-ok' : 'apply-err'">
            词表：{{ testResults.lexicons_dir.message }}
            <span class="sub">（{{ testResults.lexicons_dir.latency_ms }} ms）</span>
          </span>
        </div>
      </div>
      <div class="hint">
        决策表目录扫描 `*_gec_attribution.json`，变更目录后重新加载；
        词表目录（zh/en/ja 共用）存放外置规则词表 `*_gec_lexicon.json`，变更时重建检测器。
      </div>
    </LSection>

    <LSection v-if="store.caps.ops" id="set-backup" class="settings-card" title="生产库每日备份" desc="每天 04:00 用 sqlite3 .backup 对生产库做一致性快照（integrity_check 后保留 14 天，macOS launchd）">
      <template #actions>
        <LTag :tone="dbBackupSetting && dbBackupInstalled ? 'ok' : dbBackupSetting ? 'warn' : 'neutral'">
          {{ dbBackupSetting ? (dbBackupInstalled ? '任务在位' : '任务缺失') : '未开启' }}
        </LTag>
      </template>
      <div class="field">
        <span class="field__label">每日备份</span>
        <div class="field__ctl">
          <LSwitch v-model="dbBackupSetting" aria-label="生产库每日备份" />
          <span class="sub">打开即注册 launchd 任务，关闭即撤销；仅支持 macOS，注册失败会报错且不保存（设置不说谎）</span>
        </div>
      </div>
      <div class="field">
        <span class="field__label">备份目录</span>
        <div class="field__ctl">
          <span class="sub">{{ dbBackupDest || '（注册时按数据库绝对路径确定）' }}</span>
        </div>
      </div>
      <div v-if="dbBackupSetting && !dbBackupInstalled" class="hint">
        开关已开但 launchd 任务实存检测为缺失（可能被人工删除）：点保存会重新注册，或重启服务自动对账。
      </div>
      <div class="hint">
        备份落在数据库同级 backups/（文件权限 600）；开发环境默认关，零副作用。
      </div>
    </LSection>

    <div class="actions">
      <LButton variant="primary" :loading="saving" @click="save">保存设置</LButton>
      <LButton :loading="testing.all" @click="runTest('all')">测试全部</LButton>
      <span v-if="savedMsg" class="saved">{{ savedMsg }}</span>
    </div>

    <!-- 测试全部：逐项结果（并行探测全部 7 项配置） -->
    <LSection v-if="testResults.all" class="settings-card" title="测试结果" :desc="`${testResults.all.message}（总耗时 ${testResults.all.latency_ms} ms）`">
      <template #actions>
        <LTag :tone="testResults.all.ok ? 'ok' : 'danger'">
          {{ testResults.all.ok ? '全部通过' : '部分未通过' }}
        </LTag>
      </template>
      <div
        v-for="r in testResults.all.results ?? []"
        :key="r.target"
        class="test-line"
        :class="r.ok ? 'apply-ok' : 'apply-err'"
      >
        <b>{{ TEST_LABELS[r.target] ?? r.target }}</b>：{{ r.message }}
        <span class="sub">（{{ r.latency_ms }} ms）</span>
      </div>
    </LSection>

    <div v-if="loading" class="loading">
      <span class="spin" aria-hidden="true"></span>
      加载中…
    </div>
    <!-- 错误态带下一步指引：重试 -->
    <LEmpty v-else-if="error" text="设置加载失败" :hint="`${error}；确认服务端已启动后重试。`">
      <template #action>
        <LButton @click="load">重试</LButton>
      </template>
    </LEmpty>
  </div>
</template>

<style scoped>
.ops-account {
  display: flex;
  gap: var(--space-2);
  align-items: flex-start;
  justify-content: space-between;
  margin-top: var(--space-2);
  padding: var(--space-2) var(--space-3);
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  background: var(--surface-2);
}
.ops-account .sub {
  flex: 1;
  min-width: 0;
}
.settings-wrap { display: flex; flex-direction: column; gap: var(--space-3); max-width: 760px; }
.settings-wrap .l-section { margin-bottom: 0; }
/* 锚点直达高亮（PipelineStatus 点击 → 对应卡片滚动定位后短暂高亮） */
.settings-card.anchor-flash {
  box-shadow: 0 0 0 3px var(--primary-soft), var(--shadow-sm);
  transition: box-shadow 0.3s ease;
}
.field {
  display: grid;
  grid-template-columns: 110px minmax(0, 1fr);
  column-gap: var(--space-3);
  align-items: center;
  padding: var(--space-1) 0;
  font-size: var(--fs-ui);
}
.field__label { color: var(--text-2); }
.field__ctl { display: flex; align-items: center; gap: var(--space-2); min-width: 0; flex-wrap: wrap; }
/* ③ LLM 两臂配置分组（主判 / 影子）：视觉独立，自动双判开关居两者之间 */
.arm-group {
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  padding: var(--space-1) var(--space-3) 0;
  margin: var(--space-2) 0;
  background: var(--surface-2);
}
.arm-group__title {
  display: flex;
  align-items: baseline;
  gap: var(--space-2);
  font-weight: 600;
  color: var(--text);
  padding-top: var(--space-1);
}
.arm-group--off { opacity: 0.55; }
.field__ctl--checks { gap: var(--space-4); }
.field__ctl--wrap { flex-direction: column; align-items: flex-start; }
.w-120 { width: calc(var(--space-7) * 2 + var(--space-2)); }
.w-200 { width: calc(var(--space-7) * 4 + var(--space-2)); }
.w-240 { width: calc(var(--space-7) * 5); }
.w-280 { width: calc(var(--space-7) * 5 + var(--space-7)); }
.w-420 { width: 100%; max-width: calc(var(--space-7) * 8 + var(--space-3)); }
.desc { font-size: var(--fs-ui); color: var(--text-2); line-height: var(--lh-body); }
.hint { color: var(--muted); font-size: var(--fs-sm); line-height: var(--lh-body); margin-top: var(--space-1); }
.hint--block { margin-top: 0; }
.hint--row { display: flex; align-items: center; gap: var(--space-2); }
.sub { color: var(--muted); font-size: var(--fs-sm); }
.actions { display: flex; align-items: center; gap: var(--space-3); flex-wrap: wrap; }
.actions--mt { margin-top: var(--space-2); }
.saved { color: var(--ok); font-size: var(--fs-ui); }
.loading { display: flex; align-items: center; gap: var(--space-2); color: var(--muted); font-size: var(--fs-ui); }
.banner {
  display: flex;
  align-items: flex-start;
  gap: var(--space-3);
  padding: var(--space-2) var(--space-3);
  margin-bottom: var(--space-2);
  font-size: var(--fs-ui);
  line-height: var(--lh-body);
  color: var(--text-2);
  background: var(--surface-2);
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
}
/* 结果行状态色：红色只用于「未通过 / 失败」 */
.apply-ok { color: var(--ok); font-size: var(--fs-ui); }
.apply-err { color: var(--danger); font-size: var(--fs-ui); }
/* 「测试全部」逐项结果行 */
.test-line { font-size: var(--fs-ui); line-height: 1.9; }
/* 「工作场景与角色」能力位行 */
.cap-row {
  display: flex; align-items: center; justify-content: space-between; gap: var(--space-4);
  padding: var(--space-2) 0; border-bottom: 1px dashed var(--border);
}
.cap-row:last-of-type { border-bottom: none; }
.ops-unlock { display: flex; align-items: center; gap: var(--space-2); margin-top: var(--space-2); }
.ops-unlock .l-input { max-width: 320px; flex: 1; min-width: 0; }
.confirm-inline { display: inline-flex; align-items: center; gap: var(--space-2); font-size: var(--fs-sm); color: var(--text-2); }
/* 依赖服务状态行（监督器快照） */
.dep-row { padding: var(--space-2) 0; border-bottom: 1px dashed var(--border); }
.dep-row:last-of-type { border-bottom: none; }
.dep-row__main { display: flex; align-items: center; gap: var(--space-2); flex-wrap: wrap; }
.dep-row__label { font-size: var(--fs-ui); font-weight: 600; }
.dep-alt { display: flex; align-items: center; gap: var(--space-2); margin-top: var(--space-2); }
.dep-alt .l-input { max-width: 340px; flex: 1; min-width: 0; }
.spin {
  width: var(--space-3); height: var(--space-3); flex: none; display: inline-block;
  border: 2px solid color-mix(in srgb, currentColor 30%, transparent);
  border-top-color: currentColor; border-radius: 50%;
  animation: l-spin 0.7s linear infinite;
}
</style>
