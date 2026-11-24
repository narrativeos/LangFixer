<script setup lang="ts">
import { useGecStore } from '../stores/gec'
import { ATTRIBUTION_LABELS, WEINER_LABELS } from '../types'
import { errorTypeLabel } from '../lib/labels'
import { isVerdictOnly } from '../lib/errorDisplay'
import { computed, reactive } from 'vue'
import { api } from '../api/client'
import { toast } from '../ui/toast'
import { applyCorrections } from '../lib/correct'
import { LButton, LEmpty, LTag } from '../ui'

const store = useGecStore()

/** 句级怀疑（待复查）不进归因面：它没有具体错误点，归因只能落兑底桶，
 *  展示出来是误导；它的处置入口在「检测结果」（待复查标注 + 采纳/放弃）。
 *  LLM 复查合流入的条目无归因（attribution=null），卡片会错显「暂无决策表」，同样排除。
 *  条目携带 analyzed 原始下标 idx：忽略态与已反馈镜像都以它为键（过滤只改展示，不改身份；
 *  旧实现用过滤列表下标当讲解键，过滤后错位，本轮一并修正） */
const entries = computed(() => {
  const out: { item: (typeof store.analyzed)[number]; idx: number }[] = []
  store.analyzed.forEach((item, idx) => {
    if (item.attribution && !isVerdictOnly(item.error)) out.push({ item, idx })
  })
  return out
})

/** 整页签已忽略（R04 空态出口）：有条目但全部被本轮忽略 */
const allIgnored = computed(
  () => entries.value.length > 0 && entries.value.every(({ idx }) => !!store.ignored[idx]),
)

/** 忽略 = 本轮中性处置：不发反馈、不触发反思、不作负样本；6s 撤销与「恢复」同一标记 */
function ignoreItem(idx: number) {
  if (store.ignored[idx]) return
  store.ignoreError(idx)
  toast(
    '已忽略 1 条：不计入待处理，也不会作为负样本进训练飞轮',
    'info',
    { label: '撤销', onClick: () => store.restoreError(idx) },
    6000,
  )
}

function label(key: string, table: Record<string, string>): string {
  return table[key] ?? key
}

/** 新手兜底桶：grammar_novice 是「未命中细粒度桶」的占位归因（决策表兜底行），
 *  补救/教材内容是泛化引导文案而非有效教学内容，展示反而误导 → 告警样式 + 隐藏两卡 */
function isNoviceBucket(item: (typeof store.analyzed)[number]): boolean {
  return item.attribution?.trace?.matched_rule === 'grammar_novice'
}

/** 审计 F6：兜底桶不是流程终点——送进练习队列 = 与「采纳」同一条反馈路径
 *  （服务端 record_mistake 写会话错题本 → 「练习与复习」可回顾），
 *  仅 reason 标注入口来源，不另发明端点 */
const noviceBusy = reactive<Record<number, boolean>>({})

async function sendToPractice(item: (typeof store.analyzed)[number]) {
  const i = store.analyzed.indexOf(item)
  const e = store.errors[i]
  if (!e || noviceBusy[i]) return
  noviceBusy[i] = true
  try {
    const text = store.text ?? ''
    const corrected = store.resultsStale ? text : applyCorrections(text, [e])
    const ctx = corrected !== text ? { src_text: text, corrected_text: corrected } : undefined
    const res = await api.errorFeedback(
      store.lang,
      e,
      'adopt',
      '新手兜底桶·送进练习队列',
      ctx,
      store.ensureSessionId(),
    )
    toast(`已送进练习队列（错题本已收录，可在「练习与复习」回顾）：${res.message}`, 'ok')
  } catch (err) {
    toast(`送进练习队列失败：${err instanceof Error ? err.message : String(err)}`, 'danger')
  } finally {
    noviceBusy[i] = false
  }
}

/** 审计 F6：知识提案暂无直达后端入口，诚实降级——不伪造提交，
 *  引导到真实存在知识登记通道（运营台→知识维护：导入向导 / 学习知识） */
function proposeKnowledge() {
  store.requestNav('knowledge')
  toast(
    '兜底桶条目缺具体规则点，知识提案需经知识入库流程登记（运营台 → 知识维护：导入向导 / 学习知识）',
    'info',
  )
}
</script>

<template>
  <LEmpty
    v-if="entries.length === 0"
    :text="store.detectCompleted ? '检测完成，未发现需要归因的错误' : '暂无归因结果'"
    :hint="
      store.detectCompleted
        ? undefined
        : '点击「检测 + 归因」生成韦纳三维度归因与补救措施。'
    "
  />
  <!-- R04：整页签忽略到底不是死角——恢复全部 / 重新检测两条活路 -->
  <LEmpty
    v-else-if="allIgnored"
    text="错误归因已全部忽略"
    hint="忽略只作用于本轮；重新检测或修改文本后自动清空"
  >
    <template #action>
      <div class="empty-actions">
        <LButton size="sm" @click="store.restoreAllIgnored()">恢复全部</LButton>
        <LButton
          size="sm"
          variant="primary"
          :disabled="store.loading"
          @click="store.analyze(store.sessionId || undefined)"
          >重新检测</LButton
        >
      </div>
    </template>
  </LEmpty>
  <div v-else class="attr-list">
    <template v-for="entry in entries" :key="entry.idx">
      <!-- R03：已忽略 = 折叠 chip（不删数据），带恢复出口 -->
      <div v-if="store.ignored[entry.idx]" class="ignored-chip">
        <span>
          已忽略 · {{ entry.item.error.original }}<template v-if="entry.item.error.correction">
            → {{ entry.item.error.correction }}</template
          >（不发反馈 · 不触发反思）
        </span>
        <LButton variant="ghost" size="sm" @click="store.restoreError(entry.idx)">恢复</LButton>
      </div>
      <div v-else class="attr-card" :class="{ novice: isNoviceBucket(entry.item) }">
        <div class="attr-head">
          <LTag>{{ errorTypeLabel(entry.item.error.error_type) }}</LTag>
          <div class="fix">
            <span class="original">{{ entry.item.error.original }}</span>
            <template v-if="entry.item.error.correction">
              <span class="arrow">→</span>
              <span class="correction">{{ entry.item.error.correction }}</span>
            </template>
          </div>
          <span class="l-spacer"></span>
          <!-- 逐错误讲解：请求对该条（analyzed 下标）生成 LLM 个性化讲解（ResultsPanel 切到 AI 讲解并触发生成） -->
          <LButton variant="ghost" size="sm" @click="store.requestExplain(entry.idx)">讲解</LButton>
          <!-- 忽略与讲解并排；已反馈（采纳/放弃落定）的条目不出忽略钮 -->
          <LButton
            v-if="!store.feedbackDoneByIndex[entry.idx]"
            variant="ghost"
            size="sm"
            @click="ignoreItem(entry.idx)"
            >忽略</LButton
          >
        </div>
        <div v-if="entry.item.attribution" class="attribution-box">
          <div class="attr-main">
            <LTag>{{ label(entry.item.attribution.attribution, ATTRIBUTION_LABELS) }}</LTag>
            <LTag
              v-if="entry.item.attribution.cefr && entry.item.attribution.cefr !== 'unknown'"
            >
              CEFR {{ entry.item.attribution.cefr }}
            </LTag>
            <!-- 知识源引用（知识库富化；同一规则/点可被多个源背书）；旧历史数据回退 source 字符串 -->
            <template v-if="entry.item.attribution.sources && entry.item.attribution.sources.length > 0">
              <LTag v-for="(s, j) in entry.item.attribution.sources" :key="j" :title="s.source_id">
                {{ (s.title || s.source_id) + (s.location ? ' · ' + s.location : '') }}
              </LTag>
            </template>
            <LTag v-else-if="entry.item.attribution.source">{{ entry.item.attribution.source }}</LTag>
          </div>
          <dl class="dims">
            <div class="dim">
              <dt>归因方向</dt>
              <dd><LTag>{{ label(entry.item.attribution.weiner.locus, WEINER_LABELS) }}</LTag></dd>
            </div>
            <div class="dim">
              <dt>稳定性</dt>
              <dd><LTag>{{ label(entry.item.attribution.weiner.stability, WEINER_LABELS) }}</LTag></dd>
            </div>
            <div class="dim">
              <dt>可控性</dt>
              <dd>
                <LTag>{{ label(entry.item.attribution.weiner.controllability, WEINER_LABELS) }}</LTag>
              </dd>
            </div>
          </dl>
          <div v-if="isNoviceBucket(entry.item)" class="novice-note" role="note">
            命中新手兜底桶（grammar_novice）：只能判定为新手知识缺口，未命中具体规则点；
            补救措施与教材示例为占位引导，已隐藏，等待规则覆盖细化
            <div class="novice-actions">
              <LButton
                size="sm"
                variant="primary"
                :loading="noviceBusy[entry.idx]"
                @click="sendToPractice(entry.item)"
                >送进练习队列</LButton
              >
              <LButton size="sm" @click="proposeKnowledge">加入知识提案</LButton>
            </div>
          </div>
          <div
            v-else-if="entry.item.attribution.remedial.length > 0"
            class="remedial"
          >
            <div class="remedial-title">补救措施</div>
            <ul>
              <li v-for="(r, j) in entry.item.attribution.remedial" :key="j">{{ r }}</li>
            </ul>
          </div>
          <div
            v-if="
              !isNoviceBucket(entry.item) &&
              entry.item.attribution.examples &&
              entry.item.attribution.examples.length > 0
            "
            class="remedial"
          >
            <div class="remedial-title">教材示例</div>
            <div v-for="(ex, j) in entry.item.attribution.examples" :key="j" class="example">
              {{ ex }}
            </div>
          </div>
          <div class="trace">
            命中规则：{{ entry.item.attribution.trace.matched_rule }} ·
            决策表：{{ entry.item.attribution.trace.decision_table }}
          </div>
        </div>
        <p v-else class="no-table">该语言暂无决策表，无法归因</p>
      </div>
    </template>
  </div>
</template>

<style scoped>
.attr-list {
  display: flex;
  flex-direction: column;
  gap: var(--space-3);
}
/* 已忽略折叠条（R03）：中性处置，不删数据 */
.ignored-chip {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: var(--space-2);
  padding: var(--space-2) var(--space-3);
  background: var(--surface-2);
  border: 1px dashed var(--border);
  border-radius: var(--radius-md);
  color: var(--muted);
  font-size: var(--fs-sm);
}
.empty-actions {
  display: flex;
  gap: var(--space-2);
  justify-content: center;
}
.attr-card {
  display: flex;
  flex-direction: column;
  gap: var(--space-2);
  padding: var(--space-3);
  border: 1px solid var(--border);
  border-radius: var(--radius-md);
  background: var(--surface);
}
.attr-card.novice {
  border-color: var(--warn-line);
}
.novice-note {
  font-size: var(--fs-sm);
  color: var(--warn-ink);
  background: var(--warn-soft);
  border: 1px solid var(--warn-line);
  border-radius: var(--radius-sm);
  padding: var(--space-2) var(--space-3);
  line-height: var(--lh-body);
}
.novice-actions {
  display: flex;
  gap: var(--space-2);
  margin-top: var(--space-2);
}
.attr-head {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  min-width: 0;
}
.fix {
  display: inline-flex;
  align-items: center;
  gap: var(--space-2);
  min-width: 0;
}
.original {
  color: var(--muted);
  text-decoration: line-through;
  text-decoration-color: var(--mark-grammar);
}
.arrow {
  color: var(--muted);
}
.correction {
  color: var(--mark-ins);
}
.attribution-box {
  display: flex;
  flex-direction: column;
  gap: var(--space-2);
}
.attr-main {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-2);
}
.dims {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-2) var(--space-6);
  margin: 0;
}
.dim {
  display: flex;
  align-items: center;
  gap: var(--space-2);
}
.dims dt {
  color: var(--muted);
  font-size: var(--fs-sm);
}
.dims dd {
  margin: 0;
}
.remedial-title {
  color: var(--muted);
  font-size: var(--fs-sm);
  margin-bottom: var(--space-1);
}
.remedial ul {
  margin: 0;
  padding-left: var(--space-4);
  line-height: var(--lh-body);
}
.example {
  padding: var(--space-2);
  background: var(--surface-2);
  border-radius: var(--radius-md);
  margin-top: var(--space-1);
  line-height: var(--lh-body);
}
.trace {
  color: var(--muted);
  font-family: var(--font-mono);
  font-size: var(--fs-sm);
}
.no-table {
  margin: 0;
  padding: var(--space-2) var(--space-3);
  background: var(--surface-2);
  border-radius: var(--radius-md);
  color: var(--muted);
  font-size: var(--fs-sm);
}
</style>
