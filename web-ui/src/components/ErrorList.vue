<script setup lang="ts">
import { computed, nextTick, reactive, ref, watch } from 'vue'
import { LButton, LEmpty, LInput, LSwitch, LTag } from '../ui'
import { toast } from '../ui/toast'
import { useGecStore } from '../stores/gec'
import { useErrorFeedback } from '../composables/useErrorFeedback'
import { api } from '../api/client'
import {
  CAT_LABELS,
  SOURCE_LABELS,
  SOURCE_TIPS,
  confBand,
  consensusText,
  instanceContext,
  isVerdictOnly,
} from '../lib/errorDisplay'
import {
  buildMergedGroups,
  buildSoloGroups,
  summarizeCategories,
  summarizeTypes,
  type MergedGroup,
} from '../lib/errorGroups'
import { errorTypeLabel } from '../lib/labels'
import type { ErrorItem } from '../types'

const store = useGecStore()

// 汇总条分类计数（category 缺省归入 other）
const summary = computed(() => summarizeCategories(store.errors))

// 两级过滤（纯前端展示过滤）：粗分类（category 5 类）→ 细分类型（error_type，
// 即分型架构的 17 类 T 族在界面上的投影）；空 = 不过滤
// 保留原始下标 i 作各反馈状态 key —— 过滤只影响展示，不影响状态映射
const catFiltered = computed(() =>
  store.errors
    .map((e, i) => ({ i, e }))
    .filter(({ e }) => !store.categoryFilter || (e.category || 'other') === store.categoryFilter),
)
/** 细分类型汇总：在当前分类作用域内按 error_type 计数（种类 ≤1 时不渲染过滤行） */
const typeSummary = computed(() => summarizeTypes(catFiltered.value.map(({ e }) => e)))
const visible = computed(() =>
  store.typeFilter
    ? catFiltered.value.filter(({ e }) => e.error_type === store.typeFilter)
    : catFiltered.value,
)
function toggleCategory(k: string) {
  store.categoryFilter = store.categoryFilter === k ? '' : k
}
function toggleType(k: string) {
  store.typeFilter = store.typeFilter === k ? '' : k
}
// 两级过滤级联：换分类后旧类型多半不在新作用域内 → 清类型过滤，避免列表「莫名变空」
watch(
  () => store.categoryFilter,
  () => {
    store.typeFilter = ''
  },
)

// 置信度等级 → 标签色调（把握大=ok / 中等=warn / 其余中性；红色只留给真错误）
function confTone(e: ErrorItem) {
  const t = confBand(e).type
  return t === 'success' ? 'ok' : t === 'warning' ? 'warn' : 'neutral'
}
// ── 句级「待复查」的 LLM 复查（状态存在 store，新检测统一清空）──
function recheckOf(i: number) {
  return store.verdictRecheck[i]
}
/** 还没有送检过的「待复查」句数（「复查全部」按钮的计数；已复查/失败条目不计） */
const pendingVerdictCount = computed(() =>
  store.errors.reduce((n, e, i) => n + (isVerdictOnly(e) && !store.verdictRecheck[i] ? 1 : 0), 0),
)
function verdictHintText(i: number): string {
  const st = recheckOf(i)
  if (st?.state === 'loading') return 'AI 正在复查该句，稍候…'
  if (st?.state === 'clean')
    return 'AI 复查没有在这句里找到错误，可能是决策模型误报。点「按误报处理」会记为误报反馈；确信有错可点「仍判有错」，作为你的裁决入错题本'
  if (st?.state === 'error') return `复查失败：${st.msg ?? '未知错误'}（可重试）`
  return '决策模型认为这句有错，但没找到错在哪。送 AI 复查可定位并给出修改；也可直接「确认有错」或「按误报放弃」，两者都会作为反馈记录'
}

// 反思建议按钮形态：导航/重训练=主操作；停用规则=普通；删除类=破坏性（红）
function suggestionVariant(kind: string) {
  if (kind === 'view_rule' || kind === 'retrain' || kind === 'exclude_word') return 'primary'
  if (kind === 'disable_rule') return 'default'
  return 'danger'
}

// ── 桌面规范（macOS HIG）：错误条目右键菜单（原生弹出；Web 形态 no-op） ──
async function onItemContext(i: number) {
  const action = await api.showContextMenu('error')
  if (!action) return
  if (action === 'ctx:error:locate') store.focusError(i)
  else if (action === 'ctx:error:copy-original') void copyErrorText(store.errors[i]?.original ?? '')
  else if (action === 'ctx:error:copy-correction') void copyErrorText(store.errors[i]?.correction ?? '')
}
async function copyErrorText(t: string) {
  if (!t) {
    toast('该条目没有可复制的文本', 'info')
    return
  }
  try {
    await navigator.clipboard.writeText(t)
    toast('已复制到剪贴板', 'ok')
  } catch {
    toast('复制失败：请手动选择文本复制（浏览器权限限制）', 'warn')
  }
}

// 合并同类开关：开 = 相同实例并卡（×N）；关 = 逐条平铺（原有卡片形态）。默认开，偏好记入 localStorage
const mergeEnabled = ref(localStorage.getItem('gec.merge_enabled') !== '0')
watch(mergeEnabled, (v) => localStorage.setItem('gec.merge_enabled', v ? '1' : '0'))
/** 列表分组：合并开 = 同类分组（error_type + original + correction 相同并卡）；
 *  关 = 每条独立成组（模板对 N=1 组渲染原有逐条卡片）。两级（分类过滤 / 同类合并）正交组合 */
const listGroups = computed<MergedGroup[]>(() =>
  mergeEnabled.value ? buildMergedGroups(visible.value) : buildSoloGroups(visible.value),
)
// 合并组展开状态（组 key → 是否展开；新检测替换列表时重置）
const expanded = reactive<Record<string, boolean>>({})

// 反馈行为层（采纳 / 放弃 / 反思 / 规则 / 知识 / 批量；状态以原始下标为 key，
// 新检测替换 store.errors 时由 composable 内部统一清空）→ composables/useErrorFeedback
const {
  feedbackLoading,
  feedbackAction,
  feedbackDone,
  feedbackMsg,
  feedbackVia,
  ruleEnabled,
  knowledgeKept,
  recallTest,
  recallTesting,
  knowledgeSaving,
  reflection,
  reasonInput,
  showReason,
  suggestionMsg,
  suggestionLoading,
  llmReflectReady,
  llmReflecting,
  llmReflectMsg,
  batch,
  pendingCount,
  ruleOf,
  anomalyOf,
  knowledgeOf,
  adoptOne,
  rejectOne,
  onFeedback,
  batchReject,
  batchAdopt,
  runSuggestion,
  runLlmReflect,
  runRecallTest,
  keepKnowledge,
  discardKnowledge,
  quickToggleRule,
} = useErrorFeedback(visible)

// 预览高亮联动：滚动定位到对应错误卡片并短暂高亮
// 点卡片自身发起的 focusError 不重新滚动/闪烁本页（原位已可见，滚动只会捣乱），
// 由 onCardClick 预置一次性标记，定位 watcher 消费后清除
const listRef = ref<HTMLElement | null>(null)
const flashIdx = ref(-1)
let selfFocusPending = false
let flashTimer: ReturnType<typeof setTimeout> | null = null
watch(
  () => store.focusErrorNonce,
  (n) => {
    if (n === 0) return
    if (selfFocusPending) {
      selfFocusPending = false
      return
    }
    const idx = store.focusErrorIndex
    nextTick(() => {
      // 标签页内容可能刚挂载（从归因/讲解标签切回），再等一帧确保 DOM 就绪
      requestAnimationFrame(() => {
        const el = listRef.value?.querySelector<HTMLElement>(`[data-idx="${idx}"]`)
        if (!el) return
        el.scrollIntoView({ behavior: 'smooth', block: 'center' })
        flashIdx.value = idx
        if (flashTimer) clearTimeout(flashTimer)
        flashTimer = setTimeout(() => {
          if (flashIdx.value === idx) flashIdx.value = -1
        }, 1600)
      })
    })
  },
)

// 新检测替换错误列表 → 同类合并展开状态与细分类型过滤随新列表重置
watch(
  () => store.errors,
  () => {
    for (const k of Object.keys(expanded)) delete expanded[k]
    store.typeFilter = ''
  },
)

// 预览高亮联动：定位目标落在某个合并组内时自动展开该组（保证滚动定位可见）
watch(
  () => store.focusErrorNonce,
  (n) => {
    if (n === 0) return
    const g = listGroups.value.find((x) => x.items.some((it) => it.i === store.focusErrorIndex))
    if (g && g.items.length > 1) expanded[g.key] = true
  },
)

// 卡片→原文联动：点卡片空白/文本区即定位原文（不拦截卡内按钮/输入/链接）；
// 键盘用户用 Enter 触发同一定位。合并组卡片定位到首个实例。
function onCardClick(g: MergedGroup, ev: MouseEvent) {
  if ((ev.target as HTMLElement).closest('button, a, input, select, textarea, label, [role="button"], .instances')) return
  selfFocusPending = true
  store.focusError(g.first.i)
}
function onCardKeydown(g: MergedGroup, ev: KeyboardEvent) {
  const t = ev.target as HTMLElement
  if (t !== ev.currentTarget) return // 卡内控件自己的键盘操作不串扰
  if (ev.key === 'Enter' || ev.key === ' ') {
    ev.preventDefault()
    selfFocusPending = true
    store.focusError(g.first.i)
  }
}
</script>


<template>
  <div v-if="store.decisionAbstained" class="abstain-banner">
    <span class="abstain-icon">⚠</span>
    <span>部分句子本地拿不准，已由 AI 自动复核</span>
  </div>
  <LEmpty
    v-if="store.errors.length === 0"
    :text="
      store.detectCompleted
        ? store.llmConfirmedClean
          ? '检测完成 — AI 复查确认没有错误 ✓'
          : '检测完成 — 未发现错误 ✓'
        : '暂无检测结果'
    "
    :hint="
      store.detectCompleted
        ? store.llmConfirmedClean
          ? ''
          : '仅本次参与的检测能力范围，见下方「检测方式」'
        : '在左侧输入文本，点击「检测 + 归因」或「仅检测」开始'
    "
  />
  <div v-else ref="listRef">
    <div class="summary-bar">
      <span class="total">
        <template v-if="store.categoryFilter || store.typeFilter">
          筛选后 <b>{{ visible.length }}</b> / 共 {{ store.errors.length }} 处问题
        </template>
        <template v-else>
          共发现 <b>{{ store.errors.length }}</b> 处问题
        </template>
      </span>
      <LTag
        tone="neutral"
        class="cat-tag"
        :class="{ active: !store.categoryFilter }"
        role="button"
        tabindex="0"
        :aria-pressed="store.categoryFilter === ''"
        @click="store.categoryFilter = ''"
        @keydown.enter="store.categoryFilter = ''"
        @keydown.space.prevent="store.categoryFilter = ''"
      >
        全部 {{ store.errors.length }}
      </LTag>
      <LTag
        v-for="(n, k) in summary"
        :key="k"
        tone="neutral"
        class="cat-tag"
        :class="{ active: store.categoryFilter === k }"
        role="button"
        tabindex="0"
        :aria-pressed="store.categoryFilter === k"
        @click="toggleCategory(k)"
        @keydown.enter="toggleCategory(k)"
        @keydown.space.prevent="toggleCategory(k)"
      >
        {{ CAT_LABELS[k] ?? k }} {{ n }}
      </LTag>
      <span class="merge-toggle">
        <LSwitch v-model="mergeEnabled" aria-label="合并同类" /><span>合并同类</span>
      </span>
      <LButton
        size="sm"
        variant="primary"
        class="batch-adopt-btn"
        title="批量采纳当前筛选作用域内的全部错误（选了分类/细分类型 = 对应作用域；未选 = 全部）；已反馈与无修正文本的条目自动跳过"
        :disabled="pendingCount === 0 || !!batch.scope"
        @click="batchAdopt(visible, store.categoryFilter ? `「${CAT_LABELS[store.categoryFilter] ?? store.categoryFilter}」分类` : store.typeFilter ? `「${errorTypeLabel(store.typeFilter)}」类型` : '全部')"
      >
        {{ batch.scope ? `采纳中 ${batch.done}/${batch.total}…` : `批量采纳${pendingCount > 0 ? `（${pendingCount}）` : ''}` }}
      </LButton>
      <LButton
        v-if="pendingVerdictCount > 0 || store.recheckAll.running"
        size="sm"
        variant="ghost"
        class="batch-recheck-btn"
        :loading="store.recheckAll.running"
        :disabled="!store.llmEnabled"
        :title="
          store.llmEnabled
            ? '把所有「待复查」句子逐句送 AI 复查（串行执行，较慢）；已复查过的不重复送，首句失败即停，可稍后再点继续'
            : 'AI 复查未配置（见设置）'
        "
        @click="store.recheckAllVerdicts()"
      >
        {{
          store.recheckAll.running
            ? `复查中 ${store.recheckAll.done}/${store.recheckAll.total}…（已收录运行日志 ✓）`
            : `复查全部（${pendingVerdictCount}）`
        }}
      </LButton>
    </div>
    <!-- 细分类型过滤行：横向分型的第二级；按型反馈/路由数据积累后可挂按型收益徽章 -->
    <div v-if="typeSummary.length > 1" class="type-bar">
      <span class="type-bar-label">细分类型</span>
      <LTag
        v-for="t in typeSummary"
        :key="t.key"
        tone="neutral"
        class="cat-tag"
        :class="{ active: store.typeFilter === t.key }"
        role="button"
        tabindex="0"
        :aria-pressed="store.typeFilter === t.key"
        :title="`按细分类型筛选：${errorTypeLabel(t.key)}（点按可取消）`"
        @click="toggleType(t.key)"
        @keydown.enter="toggleType(t.key)"
        @keydown.space.prevent="toggleType(t.key)"
      >
        {{ errorTypeLabel(t.key) }} {{ t.count }}
      </LTag>
    </div>
    <div
      v-for="g in listGroups"
      :key="g.key"
      class="error-item"
      :class="[
        'cat-' + (g.first.e.category || 'other'),
        {
          merged: g.items.length > 1,
          flash: flashIdx === g.first.i,
          'is-current': store.focusErrorIndex === g.first.i,
        },
      ]"
      :data-idx="g.first.i"
      tabindex="0"
      title="点击在原文中定位此错误"
      @click="onCardClick(g, $event)"
      @keydown="onCardKeydown(g, $event)"
      @contextmenu.prevent="onItemContext(g.first.i)"
    >
      <div class="line">
        <template v-if="g.items.length > 1">
          <LTag tone="neutral" class="count-badge">×{{ g.items.length }}</LTag>
          <LButton
            size="sm"
            variant="ghost"
            class="expand-toggle"
            @click="expanded[g.key] = !expanded[g.key]"
          >
            {{ expanded[g.key] ? '收起 ▲' : `展开 ${g.items.length} 处 ▼` }}
          </LButton>
        </template>
        <!-- 标签瘦身：主行 = 错误类型（汉化）+ 分类 + 来源；共识保留（可信信号）；
             置信度只显示等级（数值移入 tooltip，各层口径不同不可直接比较）；issue_type 移入类型 tag tooltip -->
        <LTag
          tone="neutral"
          :title="`错误类型：${g.first.e.error_type}${g.first.e.issue_type ? ` · ITS 问题类型：${g.first.e.issue_type}` : ''}`"
        >{{ errorTypeLabel(g.first.e.error_type) }}</LTag>
        <LTag v-if="g.first.e.category" tone="neutral">
          {{ CAT_LABELS[g.first.e.category] ?? g.first.e.category }}
        </LTag>
        <LTag
          v-if="g.first.e.source"
          tone="neutral"
          :title="SOURCE_TIPS[g.first.e.source] ?? `该错误由 ${SOURCE_LABELS[g.first.e.source] ?? g.first.e.source} 检出`"
        >
          {{ SOURCE_LABELS[g.first.e.source] ?? g.first.e.source }}
        </LTag>
        <LTag
          v-if="g.first.e.also_found_by?.length"
          tone="ok"
          :title="`多个检测能力都发现了它：${consensusText(g.first.e)}——一致确认，更可信`"
        >
          共识 ×{{ (g.first.e.also_found_by?.length ?? 0) + 1 }}
        </LTag>
        <LTag
          :tone="confTone(g.first.e)"
          :title="`把握程度：${confBand(g.first.e).label}（参考值 ${g.first.e.confidence.toFixed(2)}；不同检测能力的参考值口径不同，不能直接比较）`"
        >
          {{ confBand(g.first.e).label }}
        </LTag>
        <LTag
          v-if="isVerdictOnly(g.first.e)"
          :tone="recheckOf(g.first.i)?.state === 'clean' ? 'ok' : 'warn'"
          :title="
            recheckOf(g.first.i)?.state === 'clean'
              ? 'AI 复查未发现具体错误，可能是决策模型误报；复查结论已收录运行日志（recheck_clean 样本）'
              : '决策模型判断该句有错，但未定位具体错误点；待 AI 复查细化'
          "
        >
          {{ recheckOf(g.first.i)?.state === 'clean' ? '已复查·无错' : '待复查' }}
        </LTag>
      </div>
      <div
        class="fix"
        :class="{ 'fix-clean': isVerdictOnly(g.first.e) && recheckOf(g.first.i)?.state === 'clean' }"
      >
        <span class="original">{{ g.first.e.original }}</span>
        <template v-if="g.first.e.correction">
          <span class="arrow">→</span>
          <span class="correction">{{ g.first.e.correction }}</span>
        </template>
      </div>
      <div class="message">{{ g.first.e.message }}</div>
      <!-- verdict 复查入口：单条 pending/loading 的主按钮在反馈行（不重复），
           此处只保留「重送复查」（clean 可重送）与合并组；运行日志的说法收进 tooltip -->
      <div
        v-if="
          isVerdictOnly(g.first.e) &&
          (g.items.length > 1 || recheckOf(g.first.i)?.state === 'clean')
        "
        class="verdict-hint"
      >
        <span>{{ verdictHintText(g.first.i) }}</span>
        <LButton
          size="sm"
          variant="ghost"
          :loading="recheckOf(g.first.i)?.state === 'loading'"
          :disabled="!store.llmEnabled || store.recheckAll.running"
          :title="
            store.recheckAll.running
              ? '批量复查进行中，逐句轮到即送，无需重复点'
              : store.llmEnabled
                ? '送 AI 复查定位并给出修改；复查结论会收录运行日志'
                : 'AI 复查未配置（见设置）'
          "
          @click="store.recheckVerdict(g.first.i)"
        >
          {{
            recheckOf(g.first.i)?.state === 'loading'
              ? '复查中…'
              : recheckOf(g.first.i)?.state === 'error'
                ? '重试复查'
                : recheckOf(g.first.i)?.state === 'clean'
                  ? '重送复查'
                  : 'LLM 复查'
          }}
        </LButton>
      </div>
      <div v-else-if="isVerdictOnly(g.first.e)" class="verdict-hint">
        <span>{{ verdictHintText(g.first.i) }}</span>
      </div>
      <div class="llm-feedback">
        <!-- 同类合并组（N>1）：卡级「采纳 ×N」批量按钮 + 放弃（两段式，作用于组内全部实例）+ 展开的实例清单 -->
        <template v-if="g.items.length > 1">
          <LButton
            size="sm"
            :variant="
              isVerdictOnly(g.first.e) && recheckOf(g.first.i)?.state === 'clean'
                ? 'ghost'
                : 'primary'
            "
            :loading="batch.scope === g.key && batch.done < batch.total"
            :disabled="
              g.items.every((it) => feedbackDone[it.i]) ||
              (batch.scope !== '' && batch.scope !== g.key)"
            @click="batchAdopt(g.items, `「${g.first.e.original} → ${g.first.e.correction ?? '—'}」同类组`)"
          >
            {{
              batch.scope === g.key && batch.done < batch.total
                ? isVerdictOnly(g.first.e)
                  ? `确认中 ${batch.done}/${g.items.length}…`
                  : `采纳中 ${batch.done}/${g.items.length}…`
                : isVerdictOnly(g.first.e)
                  ? `确认有错 ×${g.items.length}`
                  : `采纳 ×${g.items.length}`
            }}
          </LButton>
          <LButton
            size="sm"
            :variant="
              isVerdictOnly(g.first.e) && recheckOf(g.first.i)?.state === 'clean'
                ? 'primary'
                : undefined
            "
            :loading="feedbackLoading === g.first.i && feedbackAction === 'reject'"
            :disabled="
              feedbackDone[g.first.i] ||
              (feedbackLoading === g.first.i && feedbackAction === 'adopt')"
            @click="rejectOne(g.first.i)"
          >
            {{
              isVerdictOnly(g.first.e) && recheckOf(g.first.i)?.state === 'clean'
                ? showReason[g.first.i]
                  ? '✓ 待确认…'
                  : '✓ 按误报处理'
                : isVerdictOnly(g.first.e)
                  ? showReason[g.first.i]
                    ? '✗ 待确认放弃…'
                    : '✗ 按误报放弃'
                  : showReason[g.first.i]
                    ? '✗ 待确认放弃…'
                    : '✗ 放弃'
            }}
          </LButton>
          <div v-if="showReason[g.first.i]" class="reason-row">
            <LInput
              v-model="reasonInput[g.first.i]"
              class="reason-input"
              placeholder="放弃原因（可选，有助反思诊断）"
              @keyup.enter="batchReject(g.items, reasonInput[g.first.i] || undefined)"
            />
            <LButton
              size="sm"
              :loading="g.items.some((it) => feedbackLoading === it.i && feedbackAction === 'reject')"
              @click="batchReject(g.items, reasonInput[g.first.i] || undefined)"
            >
              {{ isVerdictOnly(g.first.e) ? '确认' : '确认放弃' }}
            </LButton>
            <LButton size="sm" variant="ghost" @click="showReason[g.first.i] = false">取消</LButton>
          </div>
          <span v-if="feedbackMsg[g.first.i]" class="feedback-msg">
            {{ feedbackMsg[g.first.i] }}
          </span>
          <div v-if="expanded[g.key]" class="instances">
            <div v-for="it in g.items" :key="it.i" class="instance-item">
              <span class="instance-idx">#{{ it.i }}</span>
              <span class="instance-span">{{ instanceContext(store.text ?? '', it.e) }}</span>
              <LTag :tone="feedbackDone[it.i] ? (feedbackVia[it.i] ? 'ok' : 'neutral') : 'warn'">
                {{ feedbackDone[it.i] ? (feedbackVia[it.i] ? '已采纳' : '已放弃') : '待反馈' }}
              </LTag>
              <span v-if="feedbackMsg[it.i] && !feedbackDone[it.i]" class="instance-msg">
                {{ feedbackMsg[it.i] }}
              </span>
            </div>
          </div>
        </template>
        <!-- 单条错误：原有逐条反馈结构（全部来源层） -->
        <template v-else>
          <!-- 句级 verdict 卡片：采纳/放弃不应用任何修正，只是对「句级判断」的裁决 →
               用效词呈现，复查才是主路径。已复查·无错时主按钮与复查结论一致
               （按误报处理），推翻复查（仍判有错）降为 ghost，消除「无错徽章 ×
               蓝色采纳」的矛盾信号 -->
          <template v-if="isVerdictOnly(g.first.e)">
            <template v-if="recheckOf(g.first.i)?.state === 'clean'">
              <LButton
                size="sm"
                variant="primary"
                :loading="feedbackLoading === g.first.i && feedbackAction === 'reject'"
                :disabled="
                  feedbackDone[g.first.i] ||
                  (feedbackLoading === g.first.i && feedbackAction === 'adopt')"
                title="与复查结论一致：按误报记录反馈（两段式，可附原因）"
                @click="rejectOne(g.first.i)"
              >
                {{ showReason[g.first.i] ? '✓ 待确认…' : '✓ 按误报处理' }}
              </LButton>
              <LButton
                size="sm"
                variant="ghost"
                :loading="feedbackLoading === g.first.i && feedbackAction === 'adopt'"
                :disabled="
                  feedbackDone[g.first.i] ||
                  (feedbackLoading === g.first.i && feedbackAction === 'reject')"
                title="以你的判断推翻复查结论：确认为真实错误并记入错题本（该条无改正，不改文本）"
                @click="adoptOne(g.first.i)"
              >
                仍判有错
              </LButton>
            </template>
            <template v-else>
              <LButton
                size="sm"
                variant="primary"
                :loading="recheckOf(g.first.i)?.state === 'loading'"
                :disabled="!store.llmEnabled || store.recheckAll.running"
                :title="
                  store.recheckAll.running
                    ? '批量复查进行中，逐句轮到即送，无需重复点'
                    : store.llmEnabled
                      ? '送 AI 复查定位并给出修改；复查结论会收录运行日志'
                      : 'AI 复查未配置（见设置）'
                "
                @click="store.recheckVerdict(g.first.i)"
              >
                {{
                  recheckOf(g.first.i)?.state === 'error'
                    ? '重试复查'
                    : recheckOf(g.first.i)?.state === 'loading'
                      ? '复查中…'
                      : '送 AI 复查'
                }}
              </LButton>
              <LButton
                size="sm"
                variant="ghost"
                :loading="feedbackLoading === g.first.i && feedbackAction === 'adopt'"
                :disabled="
                  feedbackDone[g.first.i] ||
                  (feedbackLoading === g.first.i && feedbackAction === 'reject')"
                title="不经复查直接确认该句为真实错误：记反馈并入错题本（该条无改正，不改文本）"
                @click="adoptOne(g.first.i)"
              >
                确认有错
              </LButton>
              <LButton
                size="sm"
                variant="ghost"
                :loading="feedbackLoading === g.first.i && feedbackAction === 'reject'"
                :disabled="
                  feedbackDone[g.first.i] ||
                  (feedbackLoading === g.first.i && feedbackAction === 'adopt')"
                title="不经复查直接按误报处理：记反馈并触发反思"
                @click="rejectOne(g.first.i)"
              >
                {{ showReason[g.first.i] ? '✗ 待确认放弃…' : '✗ 按误报放弃' }}
              </LButton>
            </template>
          </template>
          <template v-else>
            <LButton
              size="sm"
              variant="primary"
              :loading="feedbackLoading === g.first.i && feedbackAction === 'adopt'"
              :disabled="
                feedbackDone[g.first.i] ||
                (feedbackLoading === g.first.i && feedbackAction === 'reject')"
              @click="adoptOne(g.first.i)"
            >
              ✓ {{ g.first.e.error_type === 'anomaly' ? '确认污染' : '采纳' }}
            </LButton>
            <LButton
              size="sm"
              :loading="feedbackLoading === g.first.i && feedbackAction === 'reject'"
              :disabled="
                feedbackDone[g.first.i] ||
                (feedbackLoading === g.first.i && feedbackAction === 'adopt')"
              @click="rejectOne(g.first.i)"
            >
              {{ showReason[g.first.i] ? '✗ 待确认放弃…' : '✗ 放弃' }}
            </LButton>
          </template>
          <!-- 放弃（两段式）：首次点击展开原因输入框，「确认放弃」才执行 -->
          <div v-if="showReason[g.first.i]" class="reason-row">
            <LInput
              v-model="reasonInput[g.first.i]"
              class="reason-input"
              placeholder="放弃原因（可选，有助反思诊断）"
              @keyup.enter="onFeedback(g.first.i, 'reject')"
            />
            <LButton
              size="sm"
              :loading="feedbackLoading === g.first.i && feedbackAction === 'reject'"
              @click="onFeedback(g.first.i, 'reject')"
            >
              {{ isVerdictOnly(g.first.e) ? '确认' : '确认放弃' }}
            </LButton>
            <LButton size="sm" variant="ghost" @click="showReason[g.first.i] = false">取消</LButton>
          </div>
          <span v-if="feedbackMsg[g.first.i] && !reflection[g.first.i]" class="feedback-msg">
            <LTag v-if="feedbackVia[g.first.i] === 'knowledge'" tone="neutral">知识</LTag>
            <LTag v-else-if="feedbackVia[g.first.i] === 'rule'" tone="neutral">规则</LTag>
            <LTag v-else-if="feedbackVia[g.first.i] === 'anomaly'" tone="danger">污染</LTag>
            {{ feedbackMsg[g.first.i] }}
          </span>
          <!-- 采纳 → anomaly 分支：蒸馏结果卡片（过门槛 = 模式已沉淀；未过 = 降级知识候选） -->
          <div v-if="anomalyOf(g.first.i)" class="adopt-anomaly-card">
            <LTag tone="danger">污染</LTag>
            <div v-if="anomalyOf(g.first.i)!.accepted" class="adopt-card-text">
              已蒸馏新模式 <code class="anomaly-pattern">{{ anomalyOf(g.first.i)!.pattern?.pattern }}</code>
              <LTag v-if="anomalyOf(g.first.i)!.pattern?.strong" tone="ok">强信号</LTag>
              <LTag v-else tone="warn">弱信号</LTag>
              <span v-if="anomalyOf(g.first.i)!.gate" class="recall-test-note">（{{ anomalyOf(g.first.i)!.gate?.reason }}）</span>
            </div>
            <div v-else class="adopt-card-text">
              蒸馏未沉淀：{{ anomalyOf(g.first.i)!.reason || anomalyOf(g.first.i)!.gate?.reason }}
            </div>
          </div>
          <!-- 采纳 → 规则分支：规则卡片（默认停用；启用/停用即时生效，无需重建检测器） -->
          <div v-if="ruleOf(g.first.i)" class="adopt-rule-card">
            <LTag tone="neutral">规则</LTag>
            <span class="adopt-card-text">
              已编译为规则 <b>{{ ruleOf(g.first.i)?.id }}</b>：「{{ ruleOf(g.first.i)?.wrong }}」→「{{ ruleOf(g.first.i)?.right
              }}」（默认停用）
            </span>
            <LTag :tone="ruleEnabled[g.first.i] ? 'ok' : 'neutral'">
              {{ ruleEnabled[g.first.i] ? '已启用' : '已停用' }}
            </LTag>
            <LButton size="sm" variant="primary" :disabled="ruleEnabled[g.first.i]" @click="quickToggleRule(g.first.i, true)"
              >启用</LButton
            >
            <LButton size="sm" :disabled="!ruleEnabled[g.first.i]" @click="quickToggleRule(g.first.i, false)"
              >停用</LButton
            >
          </div>
          <!-- 采纳 → 知识分支：知识卡片（自动测试召回效果；用户选择保留/放弃） -->
          <div v-else-if="knowledgeOf(g.first.i)" class="adopt-knowledge-card">
            <LTag tone="neutral">知识</LTag>
            <div class="adopt-card-text">
              建议沉淀为知识：「{{ knowledgeOf(g.first.i)?.text }}」（{{ knowledgeOf(g.first.i)?.error_type }}）→「{{
                knowledgeOf(g.first.i)?.correction
              }}」
            </div>
            <div class="recall-test-row">
              <span v-if="recallTesting === g.first.i" class="recall-test-note">正在测试召回效果…</span>
              <template v-else-if="recallTest[g.first.i]">
                <LTag v-if="recallTest[g.first.i].would_hit === true" tone="ok">召回测试：将命中</LTag>
                <LTag v-else-if="recallTest[g.first.i].would_hit === false" tone="neutral"
                  >召回测试：不会命中</LTag
                >
                <LTag v-else tone="warn">召回测试：无法测试</LTag>
                <span v-if="recallTest[g.first.i].similarity !== null" class="recall-test-note"
                  >相似度 {{ recallTest[g.first.i].similarity!.toFixed(2) }} / 阈值 {{ recallTest[g.first.i].threshold }}</span
                >
                <span class="recall-test-note">{{ recallTest[g.first.i].note }}</span>
              </template>
            </div>
            <div v-if="knowledgeKept[g.first.i]" class="adopt-knowledge-kept">
              <LTag tone="ok">已保留（#{{ knowledgeKept[g.first.i] }}）</LTag>
              <span class="recall-test-note">已存入你的错词库：以后检测相似文本时会自动参考</span>
            </div>
            <div v-else class="adopt-knowledge-actions">
              <LButton size="sm" variant="primary" :loading="knowledgeSaving === g.first.i" @click="keepKnowledge(g.first.i)"
                >保留</LButton
              >
              <LButton size="sm" :disabled="knowledgeSaving === g.first.i" @click="discardKnowledge(g.first.i)">放弃</LButton>
            </div>
          </div>
          <!-- 放弃 → 反思报告卡片（诊断 + 证据 + 可执行建议 + LLM 深度反思） -->
          <div v-if="reflection[g.first.i]" class="reflection-card">
            <div class="reflection-head">
              <LTag tone="danger">反思</LTag>
              <span class="reflection-subject">{{ reflection[g.first.i].subject }}</span>
            </div>
            <div class="reflection-diagnosis">{{ reflection[g.first.i].diagnosis }}</div>
            <ul v-if="reflection[g.first.i].evidence.length" class="reflection-evidence">
              <li v-for="(ev, j) in reflection[g.first.i].evidence" :key="j">{{ ev }}</li>
            </ul>
            <div v-if="reflection[g.first.i].suggestions.length" class="reflection-suggestions">
              <template v-for="(s, j) in reflection[g.first.i].suggestions" :key="j">
                <LButton
                  size="sm"
                  :variant="suggestionVariant(s.kind)"
                  :loading="suggestionLoading === g.first.i + ':' + j"
                  @click="runSuggestion(g.first.i, j)"
                >
                  {{ s.label }}
                </LButton>
                <span v-if="suggestionMsg[g.first.i + ':' + j]" class="recall-test-note">{{ suggestionMsg[g.first.i + ':' + j] }}</span>
              </template>
            </div>
            <div v-if="llmReflectReady[g.first.i]" class="reflection-llm-row">
              <LButton size="sm" variant="ghost" :loading="llmReflecting === g.first.i" @click="runLlmReflect(g.first.i)">
                🤖 深度分析（LLM）
              </LButton>
            </div>
            <div v-if="llmReflectMsg[g.first.i]" class="reflection-llm">{{ llmReflectMsg[g.first.i] }}</div>
          </div>
        </template>
      </div>
      <div v-if="g.first.e.suggestions && g.first.e.suggestions.length > 1" class="suggestions">
        <div v-for="(s, j) in g.first.e.suggestions" :key="j" class="suggestion-item">
          <span class="suggestion-text">{{ s.text }}</span>
          <span class="suggestion-score">{{ (s.score * 100).toFixed(0) }}%</span>
        </div>
      </div>
    </div>
    <LEmpty
      v-if="visible.length === 0"
      :text="
        store.typeFilter
          ? '该细分类型下没有错误 — 点按同名类型标签或上方「全部」恢复显示'
          : '该分类下无错误 — 点击上方「全部」恢复显示'
      "
    />
  </div>
</template>

<style scoped>
/* 卡片整卡可点 → 原文定位；悬停微亮提示可点，当前焦点卡片持续描边 */
.error-item { cursor: pointer; }
.error-item:hover { border-color: var(--border-strong); }
.error-item:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
.error-item.is-current {
  border-color: color-mix(in srgb, var(--accent) 55%, var(--border));
  background: color-mix(in srgb, var(--accent) 5%, var(--surface));
}
/* 预览高亮联动：定位到的错误卡片短暂闪烁提示 */
.error-item.flash { animation: error-flash 1.6s ease; }
@keyframes error-flash {
  0%, 55% { box-shadow: 0 0 0 3px var(--primary-soft); background: var(--primary-soft); }
  100% { box-shadow: 0 0 0 0 transparent; }
}
/* 分类过滤：汇总条标签可点击切换过滤；选中态加内描边高亮 */
.cat-tag { cursor: pointer; }
.cat-tag.active {
  font-weight: 600;
  box-shadow: inset 0 0 0 1.5px currentColor;
}
.abstain-banner {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  padding: var(--space-2) var(--space-3);
  margin-bottom: var(--space-3);
  background: var(--warn-soft);
  border: 1px solid var(--warn-line);
  border-radius: var(--radius-ctl);
  font-size: var(--fs-ui);
  color: var(--warn-ink);
}
.abstain-icon {
  font-size: var(--fs-lg);
}
.adopt-rule-card,
.adopt-knowledge-card,
.adopt-anomaly-card {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-2);
  margin-top: var(--space-2);
  padding: var(--space-2) var(--space-3);
  border-radius: var(--radius-ctl);
  font-size: var(--fs-ui);
}
.adopt-rule-card {
  background: var(--ok-soft);
  border: 1px solid var(--ok-line);
}
.adopt-knowledge-card {
  flex-direction: column;
  align-items: stretch;
  background: var(--warn-soft);
  border: 1px solid var(--warn-line);
}
.adopt-anomaly-card {
  flex-direction: column;
  align-items: stretch;
  background: var(--danger-soft);
  border: 1px solid var(--danger-line);
}
.anomaly-pattern {
  background: var(--danger-soft);
  padding: var(--space-1) var(--space-2);
  border-radius: var(--radius-ctl);
  font-family: var(--font-mono);
  font-size: var(--fs-sm);
  word-break: break-all;
}
.adopt-card-text {
  flex: 1;
  min-width: 16ch;
  color: var(--text);
}
.recall-test-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-2);
}
.recall-test-note {
  font-size: var(--fs-sm);
  color: var(--muted);
}
.verdict-hint {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-2);
  margin-top: 2px;
  font-size: var(--fs-sm);
  color: var(--muted);
}

/* 复查·无错：该句大概率无误，撤销全局 .original 的「划线待替换」视觉暗示 */
.fix-clean .original {
  color: var(--muted);
  text-decoration: none;
}
.adopt-knowledge-kept {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-2);
}
.adopt-knowledge-actions {
  display: flex;
  gap: var(--space-2);
}
/* 反思流程：放弃原因输入行 + 反思报告卡片 */
.reason-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-2);
  margin-top: var(--space-2);
}
.reason-input {
  flex: 1;
  min-width: 12ch;
}
.reflection-card {
  display: flex;
  flex-direction: column;
  align-items: stretch;
  gap: var(--space-2);
  margin-top: var(--space-2);
  padding: var(--space-2) var(--space-3);
  border-radius: var(--radius-ctl);
  background: var(--danger-soft);
  border: 1px solid var(--danger-line);
  font-size: var(--fs-ui);
}
.reflection-head {
  display: flex;
  align-items: center;
  gap: var(--space-2);
}
.reflection-subject {
  font-weight: 600;
  color: var(--text);
}
.reflection-diagnosis {
  color: var(--danger-ink);
  line-height: var(--lh-body);
}
.reflection-evidence {
  margin: 0;
  padding-left: var(--space-5);
  color: var(--danger-ink-soft);
  font-size: var(--fs-sm);
  line-height: var(--lh-body);
}
.reflection-suggestions {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-2);
}
.reflection-llm-row {
  display: flex;
}
.reflection-llm {
  padding: var(--space-2) var(--space-3);
  background: var(--surface);
  border-radius: var(--radius-ctl);
  border: 1px solid var(--danger-line);
  color: var(--text);
  line-height: var(--lh-body);
  white-space: pre-wrap;
}
/* 同类合并：×N 计数徽标 + 展开/折叠开关 + 实例清单 */
.count-badge { margin-right: var(--space-1); }
.expand-toggle { margin-right: var(--space-1); }
.instances {
  display: flex;
  flex-direction: column;
  gap: var(--space-1);
  margin-top: var(--space-2);
  padding: var(--space-2) var(--space-3);
  background: var(--surface-2);
  border: 1px solid var(--border);
  border-radius: var(--radius-ctl);
}
.instance-item {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  font-size: var(--fs-sm);
  color: var(--text-2);
}
.instance-msg { color: var(--danger); }
/* 汇总条「批量采纳」按钮（分类级批量操作入口） */
.batch-adopt-btn { margin-left: var(--space-2); }
/* 「合并同类」开关（分组 / 不分组视图切换） */
.merge-toggle {
  margin-left: var(--space-3);
  display: inline-flex;
  align-items: center;
  gap: var(--space-1);
  font-size: var(--fs-ui);
  color: var(--text-2);
}
</style>
