<script setup lang="ts">
import { useGecStore } from '../stores/gec'
import { ATTRIBUTION_LABELS, WEINER_LABELS } from '../types'
import { errorTypeLabel } from '../lib/labels'
import { isVerdictOnly } from '../lib/errorDisplay'
import { computed } from 'vue'
import { LButton, LEmpty, LTag } from '../ui'

const store = useGecStore()

/** 句级怀疑（待复查）不进归因面：它没有具体错误点，归因只能落兜底桶，
 *  展示出来是误导；它的处置入口在「检测结果」（待复查标注 + 采纳/放弃） */
const attributable = computed(() => store.analyzed.filter((a) => !isVerdictOnly(a.error)))

function label(key: string, table: Record<string, string>): string {
  return table[key] ?? key
}

/** 新手兜底桶：grammar_novice 是「未命中细粒度桶」的占位归因（决策表兜底行），
 *  补救/教材内容是泛化引导文案而非有效教学内容，展示反而误导 → 告警样式 + 隐藏两卡 */
function isNoviceBucket(item: (typeof store.analyzed)[number]): boolean {
  return item.attribution?.trace?.matched_rule === 'grammar_novice'
}
</script>

<template>
  <LEmpty
    v-if="attributable.length === 0"
    :text="store.detectCompleted ? '检测完成，未发现需要归因的错误' : '暂无归因结果'"
    :hint="
      store.detectCompleted
        ? undefined
        : '点击「检测 + 归因」生成韦纳三维度归因与补救措施。'
    "
  />
  <div v-else class="attr-list">
    <div v-for="(item, i) in attributable" :key="i" class="attr-card" :class="{ novice: isNoviceBucket(item) }">
      <div class="attr-head">
        <LTag>{{ errorTypeLabel(item.error.error_type) }}</LTag>
        <div class="fix">
          <span class="original">{{ item.error.original }}</span>
          <template v-if="item.error.correction">
            <span class="arrow">→</span>
            <span class="correction">{{ item.error.correction }}</span>
          </template>
        </div>
        <span class="l-spacer"></span>
        <!-- 逐错误讲解：请求对第 i 条生成 LLM 个性化讲解（ResultsPanel 切到 AI 讲解并触发生成） -->
        <LButton variant="ghost" size="sm" @click="store.requestExplain(i)">讲解</LButton>
      </div>
      <div v-if="item.attribution" class="attribution-box">
        <div class="attr-main">
          <LTag>{{ label(item.attribution.attribution, ATTRIBUTION_LABELS) }}</LTag>
          <LTag v-if="item.attribution.cefr && item.attribution.cefr !== 'unknown'">
            CEFR {{ item.attribution.cefr }}
          </LTag>
          <!-- 知识源引用（知识库富化；同一规则/点可被多个源背书）；旧历史数据回退 source 字符串 -->
          <template v-if="item.attribution.sources && item.attribution.sources.length > 0">
            <LTag v-for="(s, j) in item.attribution.sources" :key="j" :title="s.source_id">
              {{ (s.title || s.source_id) + (s.location ? ' · ' + s.location : '') }}
            </LTag>
          </template>
          <LTag v-else-if="item.attribution.source">{{ item.attribution.source }}</LTag>
        </div>
        <dl class="dims">
          <div class="dim">
            <dt>归因方向</dt>
            <dd><LTag>{{ label(item.attribution.weiner.locus, WEINER_LABELS) }}</LTag></dd>
          </div>
          <div class="dim">
            <dt>稳定性</dt>
            <dd><LTag>{{ label(item.attribution.weiner.stability, WEINER_LABELS) }}</LTag></dd>
          </div>
          <div class="dim">
            <dt>可控性</dt>
            <dd><LTag>{{ label(item.attribution.weiner.controllability, WEINER_LABELS) }}</LTag></dd>
          </div>
        </dl>
        <div v-if="isNoviceBucket(item)" class="novice-note" role="note">
          命中新手兜底桶（grammar_novice）：只能判定为新手知识缺口，未命中具体规则点；
          补救措施与教材示例为占位引导，已隐藏，等待规则覆盖细化
        </div>
        <div v-else-if="item.attribution.remedial.length > 0" class="remedial">
          <div class="remedial-title">补救措施</div>
          <ul>
            <li v-for="(r, j) in item.attribution.remedial" :key="j">{{ r }}</li>
          </ul>
        </div>
        <div
          v-if="!isNoviceBucket(item) && item.attribution.examples && item.attribution.examples.length > 0"
          class="remedial"
        >
          <div class="remedial-title">教材示例</div>
          <div v-for="(ex, j) in item.attribution.examples" :key="j" class="example">{{ ex }}</div>
        </div>
        <div class="trace">
          命中规则：{{ item.attribution.trace.matched_rule }} · 决策表：{{ item.attribution.trace.decision_table }}
        </div>
      </div>
      <p v-else class="no-table">该语言暂无决策表，无法归因</p>
    </div>
  </div>
</template>

<style scoped>
.attr-list {
  display: flex;
  flex-direction: column;
  gap: var(--space-3);
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
