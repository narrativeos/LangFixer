<script setup lang="ts">
import { computed, ref, watch, onMounted, nextTick } from 'vue'
import { errMsg } from '../lib/err'

import { ElMessage } from 'element-plus'
import { api, isTauri } from '../api/client'
import { useGecStore } from '../stores/gec'
import type { AppSettings, L3FlywheelStatus, ModelView, SettingsTestResult, SettingsTestTarget } from '../types'

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

const hanlpEndpoint = ref('')
/** 离线模式（GEC_OFFLINE）：检测器仅用本地规则，跳过远端 NLP 服务 */
const offlineMode = ref(false)
const llmBaseUrl = ref('')
const llmApiKey = ref('')
const llmModel = ref('')
const llmTimeout = ref(30)
/** LLM 提示词模板目录（LLM_PROMPTS_DIR） */
const llmPromptsDir = ref('')
/** 当前选中的纠错模型预设 id（`prompts/models.json` 注册表；缺省 `generic`） */
const llmModelId = ref('generic')
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
    llmBaseUrl.value.trim() !== s.llm.base_url ||
    llmApiKey.value !== s.llm.api_key ||
    llmModel.value.trim() !== s.llm.model ||
    llmTimeout.value !== s.llm.timeout_secs ||
    llmPromptsDir.value.trim() !== s.llm.prompts_dir ||
    llmModelId.value !== s.llm_model_id ||
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
    const s = await api.updateSettings({
      hanlp_endpoint: hanlpEndpoint.value.trim(),
      llm_model_id: llmModelId.value,
      offline_mode: offlineMode.value,
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
    if (s.decision) decisionEnabled.value = s.decision.enabled
    // 保存成功：以服务端确认的状态同步全局 store，检测页「响应级别」L2 自动对齐
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
})

// ── 「启用决策层」开关：切换即自动保存（即时生效 + 持久化），L2 状态随之自动同步 ──
const decisionApplying = ref(false)
const decisionApplyMsg = ref('')
let decisionApplyTimer: ReturnType<typeof setTimeout> | null = null
/** load() 从服务端回填表单值期间为 true，避免误触发自动保存 */
let suppressAutoSave = false
/** 自动保存（含失败回滚）进行中，防止 watch 重入造成循环 */
let autoSaveBusy = false
watch(decisionEnabled, async (v, old) => {
  if (old === undefined || suppressAutoSave || autoSaveBusy) return
  autoSaveBusy = true
  decisionApplying.value = true
  decisionApplyMsg.value = ''
  // 乐观更新全局 store：检测页 L2 状态标签立即跟随开关
  store.decisionEnabled = v
  try {
    const res = await api.updateSettings({ decision: { enabled: v } })
    savedSnapshot.value = res
    const confirmed = res.decision?.enabled
    if (confirmed === undefined || confirmed === v) {
      decisionApplyMsg.value = `决策层已${v ? '启用' : '停用'}（即时生效，已持久化）`
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
    autoSaveBusy = false
    decisionApplying.value = false
    if (decisionApplyTimer) clearTimeout(decisionApplyTimer)
    decisionApplyTimer = setTimeout(() => {
      decisionApplyMsg.value = ''
    }, 8000)
  }
})

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
      ElMessage.success(retrainMsg.value)
    } else {
      retrainMsg.value = `重训练完成但未切换模型：${res.reason ?? '无法评测'}（保留旧模型）`
      ElMessage.warning(retrainMsg.value)
    }
    await loadFlywheelStatus()
  } catch (e) {
    retrainMsg.value = `重训练失败：${errMsg(e)}`
    ElMessage.error(retrainMsg.value)
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
  hanlp: 'NLP 服务',
  llm: 'LLM',
  decision: '决策模型',
  l3: '特征模型',
  embedding: '嵌入引擎',
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
    row('① 本地规则', '进程内', '始终可用', 'success'),
    row(
      'NLP 服务（HanLP）',
      '外部服务',
      offlineMode.value ? '离线模式（仅本地）' : hanlpEndpoint.value.trim() ? '已配置' : '未配置',
      offlineMode.value ? 'info' : hanlpEndpoint.value.trim() ? 'success' : 'warning',
      'hanlp',
    ),
    row(
      '决策模型（vegaml）',
      '外部服务',
      decisionEnabled.value
        ? decisionEndpoint.value.trim()
          ? '已启用'
          : '已启用（未填地址）'
        : '未启用（默认关闭）',
      decisionEnabled.value ? 'success' : 'info',
      'decision',
    ),
    row(
      'L3 特征模型',
      '外部服务',
      l3Enabled.value
        ? l3Endpoint.value.trim()
          ? '已启用'
          : '已启用（未填地址）'
        : '未启用',
      l3Enabled.value ? 'success' : 'info',
      'l3',
    ),
    row('LLM', '外部服务', llmEnabled.value ? '已配置' : '未配置', llmEnabled.value ? 'success' : 'warning', 'llm'),
    row(
      '嵌入引擎',
      '外部服务',
      embeddingEndpoint.value.trim() ? '已配置' : '未配置（召回跳过）',
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
</script>

<template>
  <div class="settings-wrap">
    <!-- 桌面端：进程内运行（默认离线）；下方表单所有项均可修改（即时生效 + 持久化到本地设置文件） -->
    <el-card v-if="isTauri" shadow="never" class="settings-card">
      <template #header>
        <div class="card-header">
          <span class="card-title">桌面端运行方式</span>
        <el-tag type="success" size="small">进程内 · 可编辑</el-tag>
        <span class="sub">检测引擎 / 决策表 / 用户档案均在进程内运行（默认离线）</span>
        </div>
      </template>
      <p class="desc">
        下方表单中的所有项均可直接修改：保存后即时生效（检测器 / LLM 讲解服务 / 决策表热重载），
        并持久化到本地设置文件（重启后仍生效，无需再设置环境变量或重启应用）。
      </p>
    </el-card>

    <!-- 服务总览（T2-3）：配置前先看全景——哪些服务进程内 / 外部、是否已配置、最近测试结论 -->
    <el-card shadow="never" class="settings-card">
      <template #header>
        <div class="card-header">
          <span class="card-title">服务总览</span>
          <span class="sub">配置前先看全景：每行一个服务（类型 / 当前状态 / 最近测试）</span>
        </div>
      </template>
      <el-table :data="serviceOverview" size="small" border>
        <el-table-column label="服务" prop="name" width="150" />
        <el-table-column label="类型" prop="kind" width="90" />
        <el-table-column label="状态" width="170">
          <template #default="{ row }">
            <el-tag size="small" :type="row.stateType">{{ row.state }}</el-tag>
          </template>
        </el-table-column>
        <el-table-column label="最近测试" min-width="220">
          <template #default="{ row }">
            <template v-if="row.test">
              <span :class="row.test.ok ? 'apply-ok' : 'apply-err'">
                {{ row.test.ok ? '✓ 通过' : '✗ 未通过' }}：{{ row.test.message }}
              </span>
              <span class="sub">（{{ row.test.latency_ms }} ms）</span>
            </template>
            <span v-else class="sub">未测试</span>
          </template>
        </el-table-column>
      </el-table>
      <div class="actions" style="margin-top: 10px">
        <el-button :loading="testing.all" @click="runTest('all')">测试全部</el-button>
        <span class="hint">只读探测，不修改配置；逐项结果也可在各服务卡片内单独测试</span>
      </div>
    </el-card>

    <el-card shadow="never" id="set-nlp" class="settings-card">
      <template #header>
        <div class="card-header">
          <span class="card-title">NLP 服务（HanLP）</span>
          <span class="sub">中文 / 英文检测器共用的 NLP 服务（分词 + POS）</span>
        </div>
      </template>
      <el-form label-width="110px">
        <el-form-item label="服务地址">
          <el-input
            v-model="hanlpEndpoint"
            placeholder="http://127.0.0.1:8000"
            style="width: 420px"
          />
        </el-form-item>
        <el-form-item label="离线模式">
          <el-switch v-model="offlineMode" />
          <span class="sub">开启后检测器仅用本地规则词表，不向 NLP 服务发请求</span>
        </el-form-item>
        <el-form-item label="服务测试">
          <el-button :loading="testing.hanlp" @click="runTest('hanlp')">测试 NLP 服务</el-button>
          <span v-if="testResults.hanlp" :class="testResults.hanlp.ok ? 'apply-ok' : 'apply-err'">
            {{ testResults.hanlp.message }}
            <span class="sub">（{{ testResults.hanlp.latency_ms }} ms）</span>
          </span>
        </el-form-item>
      </el-form>
      <div class="hint">
        保存后即时重建中文 / 英文 / 日文检测器（热更新，无需重启服务）；
        服务地址与离线开关由此界面显式控制，无需再设置 HANLP_ENDPOINT / GEC_OFFLINE 环境变量。
      </div>
    </el-card>

    <el-card shadow="never" id="set-decision" class="settings-card">
      <template #header>
        <div class="card-header">
          <span class="card-title">决策模型（vegaml）</span>
          <el-tag :type="decisionEnabled ? 'success' : 'info'" size="small">
            {{ decisionEnabled ? '已启用' : '未启用' }}
          </el-tag>
          <span class="sub">规则引擎之后、LLM 之前的中间检测层（三层级联：规则 → 决策 → LLM）</span>
        </div>
      </template>
      <el-form label-width="110px">
        <el-form-item label="启用决策层">
          <el-switch v-model="decisionEnabled" :loading="decisionApplying" />
          <span class="sub">
            切换即自动保存（即时生效 + 持久化），检测页「响应级别」L2 状态自动同步；
            开启后规则引擎未命中的句子交由 vegaml 决策模型判断
          </span>
        </el-form-item>
        <el-form-item v-if="decisionApplyMsg" label=" ">
          <span :class="decisionApplyMsg.startsWith('保存失败') ? 'apply-err' : 'apply-ok'">
            {{ decisionApplyMsg }}
          </span>
        </el-form-item>
        <el-form-item label="服务地址">
          <el-input
            v-model="decisionEndpoint"
            placeholder="http://127.0.0.1:4804（留空 = 仅环境变量 GEC_DEC_ENDPOINT 可启用）"
            style="width: 420px"
          />
        </el-form-item>
        <el-form-item label="模型">
          <el-input v-model="decisionModel" placeholder="vega-base" style="width: 420px" />
        </el-form-item>
        <el-form-item label="超时（秒）">
          <el-input-number v-model="decisionTimeout" :min="1" :max="120" />
        </el-form-item>
        <el-form-item label="弃权阈值">
          <el-input-number v-model="decisionThreshold" :min="0" :max="1" :step="0.05" :precision="2" />
          <span class="sub">has_error_prob 低于该值视为无错误（弃权）</span>
        </el-form-item>
        <el-form-item label="服务测试">
          <el-button :loading="testing.decision" @click="runTest('decision')">测试决策服务</el-button>
          <span v-if="testResults.decision" :class="testResults.decision.ok ? 'apply-ok' : 'apply-err'">
            {{ testResults.decision.message }}
            <span class="sub">（{{ testResults.decision.latency_ms }} ms）</span>
          </span>
        </el-form-item>
      </el-form>
      <div class="hint">
        决策层对「规则引擎未命中」的句子做 vegaml 判断：检出错误则合成错误项（与规则结果合并去重），
        弃权（prob 低于阈值）则放行；服务不可用时自动回退纯规则，不影响检测。
        保存后即时重建检测器，无需重启。
        <br />
        该开关与检测页「响应级别」对齐：
        <el-tag size="small" :type="decisionEnabled ? 'success' : 'warning'">
          当前{{ decisionEnabled ? '已启用' : '未启用' }}
        </el-tag>
        —— L2（决策模型弃权时 LLM 兜底）仅在决策层启用时生效；未启用时 L2 等效 L1，
        检测页 L2 选项会显示「决策模型未启用」状态。
      </div>
    </el-card>

    <el-card shadow="never" id="set-embedding" class="settings-card">
      <template #header>
        <div class="card-header">
          <span class="card-title">嵌入引擎（向量召回）</span>
          <el-tag :type="embeddingEndpoint ? 'success' : 'info'" size="small">
            {{ embeddingEndpoint ? '已配置' : '未配置' }}
          </el-tag>
          <span class="sub">OpenAI 兼容 /v1/embeddings 接口（知识向量召回 ②.5 层）</span>
        </div>
      </template>
      <el-form label-width="110px">
        <el-form-item label="服务地址">
          <el-input
            v-model="embeddingEndpoint"
            placeholder="http://127.0.0.1:8080/v1（留空 = 向量召回禁用）"
            style="width: 420px"
          />
        </el-form-item>
        <el-form-item label="API Key">
          <el-input
            v-model="embeddingApiKey"
            type="password"
            show-password
            placeholder="Bearer token（留空 = 无认证）"
            style="width: 420px"
          />
        </el-form-item>
        <el-form-item label="模型">
          <el-input v-model="embeddingModel" placeholder="text-embedding-3-small" style="width: 420px" />
        </el-form-item>
        <el-form-item label="服务测试">
          <el-button :loading="testing.embedding" @click="runTest('embedding')">测试嵌入服务</el-button>
          <span v-if="testResults.embedding" :class="testResults.embedding.ok ? 'apply-ok' : 'apply-err'">
            {{ testResults.embedding.message }}
            <span class="sub">（{{ testResults.embedding.latency_ms }} ms）</span>
          </span>
        </el-form-item>
      </el-form>
      <div class="hint">
        配置后，检测流水线 ②.5 层将使用嵌入向量从学习知识库中召回相似错误模式（top-3，余弦相似度 ≥ 0.7）。
        服务不可用时向量召回静默跳过，不影响其他检测层。
      </div>
    </el-card>

    <el-card shadow="never" id="set-media" class="settings-card">
      <template #header>
        <div class="card-header">
          <span class="card-title">载体审校（图片 / PDF / Word）</span>
          <el-tag :type="mediaEnabled ? 'success' : 'info'" size="small">
            {{ mediaEnabled ? '已开启' : '未开启' }}
          </el-tag>
          <span class="sub">副本隔离：只改副本，**永不回写原件**（Web / 桌面同一份设置）</span>
        </div>
      </template>
      <el-form label-width="110px">
        <el-form-item label="总开关">
          <el-switch v-model="mediaEnabled" active-text="开启载体审校" />
        </el-form-item>
        <el-form-item label="工作模式">
          <el-radio-group v-model="mediaMode">
            <el-radio value="simple">简单（零 CLI）</el-radio>
            <el-radio value="advanced">高级（三家适配器）</el-radio>
          </el-radio-group>
        </el-form-item>
        <el-form-item label="">
          <span class="sub">
            简单模式不装任何外部工具：读文本、出清单与正误表照样能用，但载体回写不可用；
            高级模式交给 PhotoCraft / PdfCraft / WordCraft，可回写载体（缺 CLI 时后端会说明原因，不会静默换档）。
          </span>
        </el-form-item>
        <el-form-item label="抽取档位">
          <el-checkbox v-model="mediaUseStructure">A 档 · 结构层（文字层 / docx run，真值）</el-checkbox>
          <el-checkbox v-model="mediaUseVision">B 档 · 多模态视觉</el-checkbox>
          <el-checkbox v-model="mediaUseOcr">C 档 · 本地 OCR</el-checkbox>
        </el-form-item>
        <el-form-item label="视觉模型">
          <el-input
            v-model="mediaVisionModel"
            placeholder="留空 = 复用 LLM 参数里的模型"
            style="width: 420px"
          />
        </el-form-item>
        <el-form-item label="响应级别">
          <el-select v-model="mediaLevel" style="width: 200px">
            <el-option :value="1" label="1 规则" />
            <el-option :value="2" label="2 + 决策模型" />
            <el-option :value="3" label="3 + LLM 兜底" />
          </el-select>
          <el-switch
            v-model="mediaForceLlm"
            active-text="强制 LLM 兜底"
            style="margin-left: 16px"
          />
        </el-form-item>
        <el-form-item label="默认回写">
          <el-select v-model="mediaApplyMode" style="width: 240px">
            <el-option value="overlay" label="Overlay 叠加标注（默认）" />
            <el-option value="comment" label="批注" />
            <el-option value="in_place" label="改原文（仅带文字层的载体）" />
            <el-option value="list_only" label="只出清单" />
          </el-select>
        </el-form-item>
        <el-form-item label="job 目录">
          <el-input
            v-model="mediaWorkdir"
            placeholder="留空 = 当前目录下 media-jobs/（改目录需重启）"
            style="width: 420px"
          />
        </el-form-item>
      </el-form>
      <div class="hint">
        未开启时载体面板不会静默开工：探测卡片会列出缺什么（协作工具 CLI / 视觉档凭据）。
        三家 CLI 路径、视觉上传压缩与超时参数留在设置文件与环境变量里
        （<code>PHOTOCRAFT_CLI</code> / <code>GEC_MEDIA_VISION_MAX_EDGE</code> …）。
      </div>
    </el-card>

    <el-card shadow="never" id="set-llm" class="settings-card">
      <template #header>
        <div class="card-header">
          <span class="card-title">LLM 参数</span>
          <el-tag :type="llmEnabled ? 'success' : 'info'" size="small">
            {{ llmEnabled ? '已配置' : '未配置' }}
          </el-tag>
          <span class="sub">OpenAI 兼容接口（讲解 / 练习生成）</span>
        </div>
      </template>
      <el-form label-width="110px">
        <el-form-item label="纠错模型">
          <el-select v-model="llmModelId" placeholder="选择纠错模型预设" style="width: 420px">
            <el-option v-for="m in models" :key="m.id" :label="m.name" :value="m.id">
              <span>{{ m.name }}</span>
              <el-tag
                size="small"
                :type="m.output_format === 'json' ? 'primary' : 'warning'"
                style="margin-left: 8px"
              >
                {{ m.output_format }}
              </el-tag>
            </el-option>
          </el-select>
          <span class="hint">
            从 `prompts/models.json` 注册表选择纠错模型预设：
            `generic` 用下方 Base URL / 模型（你的 OpenAI 兼容端点，JSON 输出）；
            `cec4` / `cec3` 用预设的本地 vllm 端点与模型名（纯文本输出，自动适配）。保存后即时生效。
          </span>
        </el-form-item>
        <el-form-item label="Base URL">
          <el-input v-model="llmBaseUrl" placeholder="https://api.openai.com/v1" style="width: 420px" />
        </el-form-item>
        <el-form-item label="API Key">
          <el-input
            v-model="llmApiKey"
            type="password"
            show-password
            placeholder="sk-…（留空 = 未配置 LLM，讲解回退静态建议）"
            style="width: 420px"
          />
        </el-form-item>
        <el-form-item label="模型">
          <el-input v-model="llmModel" placeholder="gpt-4o-mini" style="width: 420px" />
        </el-form-item>
        <el-form-item label="超时（秒）">
          <el-input-number v-model="llmTimeout" :min="1" :max="600" />
          <span class="hint">连接 / 单次读超时；流式生成只要数据持续到达就不会中断</span>
        </el-form-item>
        <el-form-item label="提示词目录">
          <el-input v-model="llmPromptsDir" placeholder="prompts" style="width: 420px" />
        </el-form-item>
        <el-form-item label="服务测试">
          <el-button :loading="testing.llm" @click="runTest('llm')">测试 LLM</el-button>
          <span v-if="testResults.llm" :class="testResults.llm.ok ? 'apply-ok' : 'apply-err'">
            {{ testResults.llm.message }}
            <span class="sub">（{{ testResults.llm.latency_ms }} ms）</span>
          </span>
        </el-form-item>
      </el-form>
      <div class="hint">
        LLM 请求模式（固定默认，不可修改）：
        <el-tag v-if="llmModes" size="small" type="primary">前端：{{ llmModes.frontend }}</el-tag>
        <el-tag v-if="llmModes" size="small">外部 API：{{ llmModes.external_api }}</el-tag>
        提示词目录变更时模板库即时重载，无需重启。
      </div>
    </el-card>

    <el-card shadow="never" id="set-l3" class="settings-card">
      <template #header>
        <div class="card-header">
          <span class="card-title">L3 特征模型</span>
          <el-tag :type="l3Enabled ? 'success' : 'info'" size="small">
            {{ l3Enabled ? '已启用' : '未启用' }}
          </el-tag>
          <span class="sub">level 3 主力层：特征模型高置信定案，LLM 收缩到纯长尾</span>
        </div>
      </template>
      <el-form label-width="110px">
        <el-form-item label="启用特征模型">
          <el-switch v-model="l3Enabled" :loading="l3Applying" />
          <span class="sub">
            切换即自动保存（即时生效 + 持久化）；
            开启后 level 3 先跑特征模型（句级特征分类 + 模式化定位 + 机械修正），
            弃权 / 服务不可用时才调用 LLM 兜底
          </span>
        </el-form-item>
        <el-form-item v-if="l3ApplyMsg" label=" ">
          <span :class="l3ApplyMsg.startsWith('保存失败') ? 'apply-err' : 'apply-ok'">
            {{ l3ApplyMsg }}
          </span>
        </el-form-item>
        <el-form-item label="服务地址">
          <el-input
            v-model="l3Endpoint"
            placeholder="http://127.0.0.1:4806（留空 = 仅环境变量 GEC_L3_ENDPOINT 可启用）"
            style="width: 420px"
          />
        </el-form-item>
        <el-form-item label="模型">
          <el-input v-model="l3Model" placeholder="feature-0.1" style="width: 420px" />
        </el-form-item>
        <el-form-item label="超时（秒）">
          <el-input-number v-model="l3Timeout" :min="1" :max="120" />
        </el-form-item>
        <el-form-item label="弃权阈值">
          <el-input-number v-model="l3Threshold" :min="0" :max="1" :step="0.05" :precision="2" />
          <span class="sub">has_error_prob 落在 [0.25, 该值) 灰区视为弃权 → LLM 长尾兜底</span>
        </el-form-item>
        <el-form-item label="服务测试">
          <el-button :loading="testing.l3" @click="runTest('l3')">测试特征服务</el-button>
          <span v-if="testResults.l3" :class="testResults.l3.ok ? 'apply-ok' : 'apply-err'">
            {{ testResults.l3.message }}
            <span class="sub">（{{ testResults.l3.latency_ms }} ms）</span>
          </span>
        </el-form-item>
      </el-form>
      <div class="hint">
        特征模型（feature-model 服务，gold 数据训练）在 LLM 之前运行：
        高置信检出错误 → 直接产出错误项（source = feature，含机械修正建议）；
        明确无错 → 放行；灰区弃权 / 服务宕机 → 回退 LLM 兜底（纯长尾）。
        保存后即时重建检测器，无需重启。
      </div>
    </el-card>

    <!-- L3 训练飞轮（三期闭环）：确认样本积累 → 重训练提醒 → 重训练 → 热切换 -->
    <el-card v-if="l3Enabled" shadow="never" class="settings-card">
      <template #header>
        <div class="card-header">
          <span class="card-title">L3 训练飞轮</span>
          <el-tag v-if="flywheelStatus?.retrain_ready" type="warning" size="small">建议重训练</el-tag>
          <el-tag v-else type="info" size="small">积累中</el-tag>
          <span class="sub">确认采纳的 LLM 检测结果持续汇入训练集，特征模型随使用变强</span>
        </div>
      </template>
      <el-form label-width="110px" v-if="flywheelStatus">
        <el-form-item label="语言">
          <el-select v-model="flywheelLang" style="width: 120px" @change="loadFlywheelStatus">
            <el-option label="中文" value="zh" />
            <el-option label="英文" value="en" />
          </el-select>
        </el-form-item>
        <el-form-item label="确认样本">
          <b>{{ flywheelStatus.samples }}</b> 条
          <span class="sub">（采纳 LLM 错误 = 正样本；LLM 复查确认干净的句子 = 负样本；按文本去重，上限 5000）</span>
        </el-form-item>
        <el-form-item label="当前模型">
          <template v-if="flywheelStatus.service_available">
            {{ flywheelStatus.model || '—' }}
            <span v-if="flywheelStatus.f1 != null" class="sub">（dev F1 {{ flywheelStatus.f1.toFixed(3) }}）</span>
          </template>
          <span v-else class="apply-err">feature-model 服务不可达（仅本地样本统计可用）</span>
        </el-form-item>
        <el-form-item label="上次重训练">
          {{ flywheelStatus.last_retrain_at ?? '从未' }}
        </el-form-item>
        <el-form-item label="重训练">
          <el-button
            type="primary"
            :loading="retraining"
            :disabled="!flywheelStatus.service_available"
            @click="doRetrain"
          >
            重训练并热切换
          </el-button>
          <span class="sub">
            合并确认样本 → 重训练 → dev 集评测；F1 显著退化（&gt; 0.02）时保留旧模型
          </span>
        </el-form-item>
        <el-form-item v-if="retrainMsg" label=" ">
          <span :class="retrainMsg.startsWith('重训练失败') ? 'apply-err' : 'apply-ok'">{{ retrainMsg }}</span>
        </el-form-item>
      </el-form>
      <div v-else class="hint" style="display: flex; align-items: center; gap: 8px">
        <el-icon v-if="flywheelLoading" class="is-loading"><Loading /></el-icon>
        飞轮状态不可用（feature-model 服务未启动或加载失败）
      </div>
      <div class="hint">
        采集来源：① 检测页「采纳」LLM 检出的错误（正样本，自动汇入）；
        ② 检测未产出错误（含 LLM 复查确认干净）时原文句子自动汇入（负样本）。
        提醒条件：上次重训练后新增 ≥ 10 条（从未训练过则累计 ≥ 30）且距上次重训练 ≥ 7 天。
      </div>
    </el-card>

    <el-card shadow="never" id="set-dirs" class="settings-card">
      <template #header>
        <div class="card-header">
          <span class="card-title">数据目录</span>
          <span class="sub">决策表 / 规则词表目录（保存后即时重载）</span>
        </div>
      </template>
      <el-form label-width="110px">
        <el-form-item label="决策表目录">
          <el-input v-model="tablesDir" placeholder="decision-tables" style="width: 420px" />
        </el-form-item>
        <el-form-item label="词表目录">
          <el-input v-model="lexiconsDir" placeholder="lexicons" style="width: 420px" />
        </el-form-item>
        <el-form-item label="目录测试">
          <el-button :loading="testing.tables_dir" @click="runTest('tables_dir')">测试决策表目录</el-button>
          <el-button :loading="testing.lexicons_dir" @click="runTest('lexicons_dir')">测试词表目录</el-button>
        </el-form-item>
        <el-form-item v-if="testResults.tables_dir" label=" ">
          <span :class="testResults.tables_dir.ok ? 'apply-ok' : 'apply-err'">
            决策表：{{ testResults.tables_dir.message }}
            <span class="sub">（{{ testResults.tables_dir.latency_ms }} ms）</span>
          </span>
        </el-form-item>
        <el-form-item v-if="testResults.lexicons_dir" label=" ">
          <span :class="testResults.lexicons_dir.ok ? 'apply-ok' : 'apply-err'">
            词表：{{ testResults.lexicons_dir.message }}
            <span class="sub">（{{ testResults.lexicons_dir.latency_ms }} ms）</span>
          </span>
        </el-form-item>
      </el-form>
      <div class="hint">
        决策表目录扫描 `*_gec_attribution.json`，变更目录后重新加载；
        词表目录（zh/en/ja 共用）存放外置规则词表 `*_gec_lexicon.json`，变更时重建检测器。
      </div>
    </el-card>

    <div class="actions">
      <el-button type="primary" :loading="saving" @click="save">保存设置</el-button>
      <el-button :loading="testing.all" @click="runTest('all')">测试全部</el-button>
      <span v-if="savedMsg" class="saved">{{ savedMsg }}</span>
    </div>

    <!-- 测试全部：逐项结果（并行探测全部 7 项配置） -->
    <el-card v-if="testResults.all" shadow="never" class="settings-card">
      <template #header>
        <div class="card-header">
          <span class="card-title">测试结果</span>
          <el-tag :type="testResults.all.ok ? 'success' : 'danger'" size="small">
            {{ testResults.all.ok ? '全部通过' : '部分未通过' }}
          </el-tag>
          <span class="sub">{{ testResults.all.message }}（总耗时 {{ testResults.all.latency_ms }} ms）</span>
        </div>
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
    </el-card>

    <div v-if="loading" class="loading">
      <el-icon class="is-loading"><Loading /></el-icon>
      加载中…
    </div>
    <el-alert v-else-if="error" type="error" :closable="false" :title="error" />
  </div>
</template>

<style scoped>
.settings-wrap { display: flex; flex-direction: column; gap: 14px; max-width: 760px; }
/* 锚点直达高亮（PipelineStatus 点击 → 对应卡片滚动定位后短暂高亮） */
.settings-card.anchor-flash {
  box-shadow: 0 0 0 3px var(--primary-soft), var(--shadow-sm);
  transition: box-shadow 0.3s ease;
}
.settings-card :deep(.el-card__header) { padding: 10px 16px; }
.desc { font-size: 13px; color: var(--text-2); line-height: 1.7; }
.hint { color: var(--el-text-color-secondary); font-size: 12px; line-height: 1.7; margin-top: 6px; }
.actions { display: flex; align-items: center; gap: 12px; }
.saved { color: var(--el-color-success); font-size: 13px; }
.loading { display: flex; align-items: center; gap: 8px; color: var(--el-text-color-secondary); }
/* 「启用决策层」开关自动保存的结果提示 */
.apply-ok { color: var(--ok); font-size: 13px; }
.apply-err { color: var(--danger); font-size: 13px; }
/* 「测试全部」逐项结果行 */
.test-line { font-size: 13px; line-height: 1.9; }
</style>