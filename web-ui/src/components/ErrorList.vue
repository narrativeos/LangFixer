<script setup lang="ts">
import { computed, nextTick, reactive, ref, watch } from 'vue'
import { useGecStore } from '../stores/gec'
import { useErrorFeedback } from '../composables/useErrorFeedback'
import {
  CAT_LABELS,
  CAT_TYPES,
  SOURCE_LABELS,
  SOURCE_TAG_TYPES,
  SOURCE_TIPS,
  confBand,
  consensusText,
  instanceContext,
} from '../lib/errorDisplay'
import {
  buildMergedGroups,
  buildSoloGroups,
  summarizeCategories,
  type MergedGroup,
} from '../lib/errorGroups'
import { errorTypeLabel } from '../lib/labels'

const store = useGecStore()

// 汇总条分类计数（category 缺省归入 other）
const summary = computed(() => summarizeCategories(store.errors))

// 分类过滤（纯前端展示过滤）：点击汇总条分类标签切换；空 = 全部
// 保留原始下标 i 作各反馈状态 key —— 过滤只影响展示，不影响状态映射
const visible = computed(() =>
  store.errors
    .map((e, i) => ({ i, e }))
    .filter(({ e }) => !store.categoryFilter || (e.category || 'other') === store.categoryFilter),
)
function toggleCategory(k: string) {
  store.categoryFilter = store.categoryFilter === k ? '' : k
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
const listRef = ref<HTMLElement | null>(null)
const flashIdx = ref(-1)
let flashTimer: ReturnType<typeof setTimeout> | null = null
watch(
  () => store.focusErrorNonce,
  (n) => {
    if (n === 0) return
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

// 新检测替换错误列表 → 同类合并展开状态随新列表重置
watch(
  () => store.errors,
  () => {
    for (const k of Object.keys(expanded)) delete expanded[k]
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
</script>


<template>
  <div v-if="store.decisionAbstained" class="abstain-banner">
    <span class="abstain-icon">⚠</span>
    <span>决策模型弃权 → LLM 兜底已触发</span>
  </div>
  <el-empty
    v-if="store.errors.length === 0"
    :description="
      store.detectCompleted
        ? store.llmConfirmedClean
          ? '检测完成 — LLM 复查确认无错误 ✓'
          : '检测完成 — 未发现语法错误 ✓（仅本次参与的检测层覆盖范围，见下方检测路径）'
        : '暂无检测结果 — 在左侧输入文本，点击「检测 + 归因」或「仅检测」开始'
    "
    :image-size="80"
  />
  <div v-else ref="listRef">
    <div class="summary-bar">
      <span class="total">
        <template v-if="store.categoryFilter">
          筛选后 <b>{{ visible.length }}</b> / 共 {{ store.errors.length }} 处问题
        </template>
        <template v-else>
          共发现 <b>{{ store.errors.length }}</b> 处问题
        </template>
      </span>
      <el-tag
        size="small"
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
      </el-tag>
      <el-tag
        v-for="(n, k) in summary"
        :key="k"
        :type="CAT_TYPES[k] ?? 'info'"
        size="small"
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
      </el-tag>
      <span class="merge-toggle">
        <el-switch v-model="mergeEnabled" size="small" active-text="合并同类" />
      </span>
      <el-tooltip
        content="批量采纳当前筛选作用域内的全部错误（选了分类 = 该分类；未选 = 全部）；已反馈与无修正文本的条目自动跳过"
        placement="top"
      >
        <el-button
          size="small"
          type="success"
          plain
          class="batch-adopt-btn"
          :disabled="pendingCount === 0 || !!batch.scope"
          @click="batchAdopt(visible, store.categoryFilter ? `「${CAT_LABELS[store.categoryFilter] ?? store.categoryFilter}」分类` : '全部')"
        >
          {{ batch.scope ? `采纳中 ${batch.done}/${batch.total}…` : `批量采纳${pendingCount > 0 ? `（${pendingCount}）` : ''}` }}
        </el-button>
      </el-tooltip>
    </div>
    <div
      v-for="g in listGroups"
      :key="g.key"
      class="error-item"
      :class="[
        'cat-' + (g.first.e.category || 'other'),
        { merged: g.items.length > 1, flash: flashIdx === g.first.i },
      ]"
      :data-idx="g.first.i"
    >
      <div class="line">
        <template v-if="g.items.length > 1">
          <el-tag type="info" size="small" class="count-badge">×{{ g.items.length }}</el-tag>
          <el-button
            size="small"
            text
            type="info"
            class="expand-toggle"
            @click="expanded[g.key] = !expanded[g.key]"
          >
            {{ expanded[g.key] ? '收起 ▲' : `展开 ${g.items.length} 处 ▼` }}
          </el-button>
        </template>
        <!-- 标签瘦身：主行 = 错误类型（汉化）+ 分类 + 来源；共识保留（可信信号）；
             置信度只显示等级（数值移入 tooltip，各层口径不同不可直接比较）；issue_type 移入类型 tag tooltip -->
        <el-tooltip
          :content="`错误类型：${g.first.e.error_type}${g.first.e.issue_type ? ` · ITS 问题类型：${g.first.e.issue_type}` : ''}`"
          placement="top"
        >
          <el-tag type="info" size="small">{{ errorTypeLabel(g.first.e.error_type) }}</el-tag>
        </el-tooltip>
        <el-tag
          v-if="g.first.e.category"
          :type="CAT_TYPES[g.first.e.category] ?? 'info'"
          size="small"
        >
          {{ CAT_LABELS[g.first.e.category] ?? g.first.e.category }}
        </el-tag>
        <el-tooltip
          v-if="g.first.e.source"
          :content="SOURCE_TIPS[g.first.e.source] ?? `该错误由 ${SOURCE_LABELS[g.first.e.source] ?? g.first.e.source} 检出`"
          placement="top"
        >
          <el-tag :type="SOURCE_TAG_TYPES[g.first.e.source] ?? 'info'" size="small">
            {{ SOURCE_LABELS[g.first.e.source] ?? g.first.e.source }}
          </el-tag>
        </el-tooltip>
        <el-tooltip
          v-if="g.first.e.also_found_by?.length"
          :content="`多层共识：该错误被 ${consensusText(g.first.e)} 独立检出——多个层一致确认，可信度更高`"
          placement="top"
        >
          <el-tag type="success" effect="dark" size="small">
            共识 ×{{ (g.first.e.also_found_by?.length ?? 0) + 1 }}
          </el-tag>
        </el-tooltip>
        <el-tooltip
          :content="`置信度：${confBand(g.first.e).label}（原始数值 ${g.first.e.confidence.toFixed(2)}；各层口径不同，不可直接比较）`"
          placement="top"
        >
          <el-tag :type="confBand(g.first.e).type" effect="plain" size="small">
            {{ confBand(g.first.e).label }}
          </el-tag>
        </el-tooltip>
      </div>
      <div class="fix">
        <span class="original">{{ g.first.e.original }}</span>
        <template v-if="g.first.e.correction">
          <span class="arrow">→</span>
          <span class="correction">{{ g.first.e.correction }}</span>
        </template>
      </div>
      <div class="message">{{ g.first.e.message }}</div>
      <div class="llm-feedback">
        <!-- 同类合并组（N>1）：卡级「采纳 ×N」批量按钮 + 放弃（两段式，作用于组内全部实例）+ 展开的实例清单 -->
        <template v-if="g.items.length > 1">
          <el-button
            size="small"
            type="success"
            plain
            :loading="batch.scope === g.key && batch.done < batch.total"
            :disabled="
              g.items.every((it) => feedbackDone[it.i]) ||
              (batch.scope !== '' && batch.scope !== g.key)"
            @click="batchAdopt(g.items, `「${g.first.e.original} → ${g.first.e.correction ?? '—'}」同类组`)"
          >
            {{
              batch.scope === g.key && batch.done < batch.total
                ? `采纳中 ${batch.done}/${g.items.length}…`
                : `采纳 ×${g.items.length}`
            }}
          </el-button>
          <el-button
            size="small"
            type="warning"
            :plain="!showReason[g.first.i]"
            :loading="feedbackLoading === g.first.i && feedbackAction === 'reject'"
            :disabled="
              feedbackDone[g.first.i] ||
              (feedbackLoading === g.first.i && feedbackAction === 'adopt')"
            @click="rejectOne(g.first.i)"
          >
            {{ showReason[g.first.i] ? '✗ 待确认放弃…' : '✗ 放弃' }}
          </el-button>
          <div v-if="showReason[g.first.i]" class="reason-row">
            <el-input
              v-model="reasonInput[g.first.i]"
              size="small"
              placeholder="放弃原因（可选，有助反思诊断）"
              style="width: 220px"
              @keyup.enter="batchReject(g.items, reasonInput[g.first.i] || undefined)"
            />
            <el-button
              size="small"
              type="warning"
              :loading="g.items.some((it) => feedbackLoading === it.i && feedbackAction === 'reject')"
              @click="batchReject(g.items, reasonInput[g.first.i] || undefined)"
            >
              确认放弃
            </el-button>
            <el-button size="small" text @click="showReason[g.first.i] = false">取消</el-button>
          </div>
          <span v-if="feedbackMsg[g.first.i]" class="feedback-msg">
            {{ feedbackMsg[g.first.i] }}
          </span>
          <div v-if="expanded[g.key]" class="instances">
            <div v-for="it in g.items" :key="it.i" class="instance-item">
              <span class="instance-idx">#{{ it.i }}</span>
              <span class="instance-span">{{ instanceContext(store.text ?? '', it.e) }}</span>
              <el-tag
                :type="feedbackDone[it.i] ? (feedbackVia[it.i] ? 'success' : 'info') : 'warning'"
                size="small"
              >
                {{ feedbackDone[it.i] ? (feedbackVia[it.i] ? '已采纳' : '已放弃') : '待反馈' }}
              </el-tag>
              <span v-if="feedbackMsg[it.i] && !feedbackDone[it.i]" class="instance-msg">
                {{ feedbackMsg[it.i] }}
              </span>
            </div>
          </div>
        </template>
        <!-- 单条错误：原有逐条反馈结构（全部来源层） -->
        <template v-else>
          <el-button
            size="small"
            type="success"
            plain
            :loading="feedbackLoading === g.first.i && feedbackAction === 'adopt'"
            :disabled="
              feedbackDone[g.first.i] ||
              (feedbackLoading === g.first.i && feedbackAction === 'reject')"
            @click="adoptOne(g.first.i)"
          >
            ✓ {{ g.first.e.error_type === 'anomaly' ? '确认污染' : '采纳' }}
          </el-button>
          <el-button
            size="small"
            type="warning"
            :plain="!showReason[g.first.i]"
            :loading="feedbackLoading === g.first.i && feedbackAction === 'reject'"
            :disabled="
              feedbackDone[g.first.i] ||
              (feedbackLoading === g.first.i && feedbackAction === 'adopt')"
            @click="rejectOne(g.first.i)"
          >
            {{ showReason[g.first.i] ? '✗ 待确认放弃…' : '✗ 放弃' }}
          </el-button>
          <!-- 放弃（两段式）：首次点击展开原因输入框，「确认放弃」才执行 -->
          <div v-if="showReason[g.first.i]" class="reason-row">
            <el-input
              v-model="reasonInput[g.first.i]"
              size="small"
              placeholder="放弃原因（可选，有助反思诊断）"
              style="width: 220px"
              @keyup.enter="onFeedback(g.first.i, 'reject')"
            />
            <el-button
              size="small"
              type="warning"
              :loading="feedbackLoading === g.first.i && feedbackAction === 'reject'"
              @click="onFeedback(g.first.i, 'reject')"
            >
              确认放弃
            </el-button>
            <el-button size="small" text @click="showReason[g.first.i] = false">取消</el-button>
          </div>
          <span v-if="feedbackMsg[g.first.i] && !reflection[g.first.i]" class="feedback-msg">
            <el-tag v-if="feedbackVia[g.first.i] === 'knowledge'" type="warning" size="small" effect="plain">知识</el-tag>
            <el-tag v-else-if="feedbackVia[g.first.i] === 'rule'" type="success" size="small" effect="plain">规则</el-tag>
            <el-tag v-else-if="feedbackVia[g.first.i] === 'anomaly'" type="danger" size="small" effect="plain">污染</el-tag>
            {{ feedbackMsg[g.first.i] }}
          </span>
          <!-- 采纳 → anomaly 分支：蒸馏结果卡片（过门槛 = 模式已沉淀；未过 = 降级知识候选） -->
          <div v-if="anomalyOf(g.first.i)" class="adopt-anomaly-card">
            <el-tag type="danger" size="small">污染</el-tag>
            <div v-if="anomalyOf(g.first.i)!.accepted" class="adopt-card-text">
              已蒸馏新模式 <code class="anomaly-pattern">{{ anomalyOf(g.first.i)!.pattern?.pattern }}</code>
              <el-tag v-if="anomalyOf(g.first.i)!.pattern?.strong" type="success" size="small">强信号</el-tag>
              <el-tag v-else type="warning" size="small">弱信号</el-tag>
              <span v-if="anomalyOf(g.first.i)!.gate" class="recall-test-note">（{{ anomalyOf(g.first.i)!.gate?.reason }}）</span>
            </div>
            <div v-else class="adopt-card-text">
              蒸馏未沉淀：{{ anomalyOf(g.first.i)!.reason || anomalyOf(g.first.i)!.gate?.reason }}
            </div>
          </div>
          <!-- 采纳 → 规则分支：规则卡片（默认停用；启用/停用即时生效，无需重建检测器） -->
          <div v-if="ruleOf(g.first.i)" class="adopt-rule-card">
            <el-tag type="success" size="small">规则</el-tag>
            <span class="adopt-card-text">
              已编译为规则 <b>{{ ruleOf(g.first.i)?.id }}</b>：「{{ ruleOf(g.first.i)?.wrong }}」→「{{ ruleOf(g.first.i)?.right
              }}」（默认停用）
            </span>
            <el-tag :type="ruleEnabled[g.first.i] ? 'success' : 'info'" size="small">
              {{ ruleEnabled[g.first.i] ? '已启用' : '已停用' }}
            </el-tag>
            <el-button size="small" type="success" plain :disabled="ruleEnabled[g.first.i]" @click="quickToggleRule(g.first.i, true)"
              >启用</el-button
            >
            <el-button size="small" type="info" plain :disabled="!ruleEnabled[g.first.i]" @click="quickToggleRule(g.first.i, false)"
              >停用</el-button
            >
          </div>
          <!-- 采纳 → 知识分支：知识卡片（自动测试召回效果；用户选择保留/放弃） -->
          <div v-else-if="knowledgeOf(g.first.i)" class="adopt-knowledge-card">
            <el-tag type="warning" size="small">知识</el-tag>
            <div class="adopt-card-text">
              建议沉淀为知识：「{{ knowledgeOf(g.first.i)?.text }}」（{{ knowledgeOf(g.first.i)?.error_type }}）→「{{
                knowledgeOf(g.first.i)?.correction
              }}」
            </div>
            <div class="recall-test-row">
              <span v-if="recallTesting === g.first.i" class="recall-test-note">正在测试召回效果…</span>
              <template v-else-if="recallTest[g.first.i]">
                <el-tag v-if="recallTest[g.first.i].would_hit === true" type="success" size="small">召回测试：将命中</el-tag>
                <el-tag v-else-if="recallTest[g.first.i].would_hit === false" type="info" size="small"
                  >召回测试：不会命中</el-tag
                >
                <el-tag v-else type="warning" size="small">召回测试：无法测试</el-tag>
                <span v-if="recallTest[g.first.i].similarity !== null" class="recall-test-note"
                  >相似度 {{ recallTest[g.first.i].similarity!.toFixed(2) }} / 阈值 {{ recallTest[g.first.i].threshold }}</span
                >
                <span class="recall-test-note">{{ recallTest[g.first.i].note }}</span>
              </template>
            </div>
            <div v-if="knowledgeKept[g.first.i]" class="adopt-knowledge-kept">
              <el-tag type="success" size="small">已保留（#{{ knowledgeKept[g.first.i] }}）</el-tag>
              <span class="recall-test-note">已写入知识存储：重检测相似文本时 ②.5 知识召回将直接命中</span>
            </div>
            <div v-else class="adopt-knowledge-actions">
              <el-button size="small" type="success" :loading="knowledgeSaving === g.first.i" @click="keepKnowledge(g.first.i)"
                >保留</el-button
              >
              <el-button size="small" :disabled="knowledgeSaving === g.first.i" @click="discardKnowledge(g.first.i)">放弃</el-button>
            </div>
          </div>
          <!-- 放弃 → 反思报告卡片（诊断 + 证据 + 可执行建议 + LLM 深度反思） -->
          <div v-if="reflection[g.first.i]" class="reflection-card">
            <div class="reflection-head">
              <el-tag type="danger" size="small">反思</el-tag>
              <span class="reflection-subject">{{ reflection[g.first.i].subject }}</span>
            </div>
            <div class="reflection-diagnosis">{{ reflection[g.first.i].diagnosis }}</div>
            <ul v-if="reflection[g.first.i].evidence.length" class="reflection-evidence">
              <li v-for="(ev, j) in reflection[g.first.i].evidence" :key="j">{{ ev }}</li>
            </ul>
            <div v-if="reflection[g.first.i].suggestions.length" class="reflection-suggestions">
              <template v-for="(s, j) in reflection[g.first.i].suggestions" :key="j">
                <el-button
                  size="small"
                  plain
                  :type="s.kind === 'disable_rule' ? 'warning' : s.kind === 'view_rule' || s.kind === 'retrain' ? 'primary' : s.kind === 'exclude_word' ? 'success' : 'danger'"
                  :loading="suggestionLoading === g.first.i + ':' + j"
                  @click="runSuggestion(g.first.i, j)"
                >
                  {{ s.label }}
                </el-button>
                <span v-if="suggestionMsg[g.first.i + ':' + j]" class="recall-test-note">{{ suggestionMsg[g.first.i + ':' + j] }}</span>
              </template>
            </div>
            <div v-if="llmReflectReady[g.first.i]" class="reflection-llm-row">
              <el-button size="small" text type="primary" :loading="llmReflecting === g.first.i" @click="runLlmReflect(g.first.i)">
                🤖 深度分析（LLM）
              </el-button>
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
    <div v-if="visible.length === 0" class="filtered-empty">
      该分类下无错误 — 点击上方「全部」恢复显示
    </div>
  </div>
</template>

<style scoped>
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
/* 过滤后无可见项时的轻量提示（区别于整块 el-empty 的「无错误」） */
.filtered-empty {
  padding: 16px 12px;
  text-align: center;
  font-size: 13px;
  color: var(--text-2);
  background: var(--surface-2);
  border: 1px dashed var(--border);
  border-radius: 6px;
}
.abstain-banner {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 8px 12px;
  margin-bottom: 12px;
  background: var(--warn-soft);
  border: 1px solid var(--warn-line);
  border-radius: 6px;
  font-size: 13px;
  color: var(--warn-ink);
}
.abstain-icon {
  font-size: 16px;
}
.adopt-rule-card,
.adopt-knowledge-card,
.adopt-anomaly-card {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
  margin-top: 8px;
  padding: 8px 10px;
  border-radius: 6px;
  font-size: 13px;
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
  padding: 1px 6px;
  border-radius: 4px;
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 12px;
  word-break: break-all;
}
.adopt-card-text {
  flex: 1;
  min-width: 200px;
  color: var(--text);
}
.recall-test-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 6px;
}
.recall-test-note {
  font-size: 12px;
  color: var(--muted);
}
.adopt-knowledge-kept {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
}
.adopt-knowledge-actions {
  display: flex;
  gap: 8px;
}
/* 反思流程：放弃原因输入行 + 反思报告卡片 */
.reason-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 6px;
  margin-top: 6px;
}
.reflection-card {
  flex-direction: column;
  align-items: stretch;
  gap: 6px;
  margin-top: 8px;
  padding: 10px 12px;
  border-radius: 6px;
  background: var(--danger-soft);
  border: 1px solid var(--danger-line);
  font-size: 13px;
}
.reflection-head {
  display: flex;
  align-items: center;
  gap: 8px;
}
.reflection-subject {
  font-weight: 600;
  color: var(--text);
}
.reflection-diagnosis {
  color: var(--danger-ink);
  line-height: 1.6;
}
.reflection-evidence {
  margin: 0;
  padding-left: 18px;
  color: var(--danger-ink-soft);
  font-size: 12px;
  line-height: 1.7;
}
.reflection-suggestions {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
}
.reflection-llm-row {
  display: flex;
}
.reflection-llm {
  padding: 8px 10px;
  background: var(--surface);
  border-radius: 6px;
  border: 1px solid var(--danger-line);
  color: var(--text);
  line-height: 1.7;
  white-space: pre-wrap;
}
/* 同类合并：×N 计数徽标 + 展开/折叠开关 + 实例清单 */
.count-badge { margin-right: 4px; }
.expand-toggle { margin-right: 4px; height: auto; }
.instances {
  display: flex;
  flex-direction: column;
  gap: 4px;
  margin-top: 8px;
  padding: 8px 10px;
  background: var(--surface-2);
  border: 1px solid var(--border);
  border-radius: 6px;
}
.instance-item {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 12.5px;
  color: var(--text-2);
}
.instance-msg { color: var(--danger); }
/* 汇总条「批量采纳」按钮（分类级批量操作入口） */
.batch-adopt-btn { margin-left: 8px; }
/* 「合并同类」开关（分组 / 不分组视图切换） */
.merge-toggle { margin-left: 12px; }
</style>
