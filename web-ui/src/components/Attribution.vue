<script setup lang="ts">
import { useGecStore } from '../stores/gec'
import { ATTRIBUTION_LABELS, WEINER_LABELS } from '../types'
import { errorTypeLabel } from '../lib/labels'

const store = useGecStore()

function label(key: string, table: Record<string, string>): string {
  return table[key] ?? key
}
</script>

<template>
  <el-empty
    v-if="store.analyzed.length === 0"
    :description="store.detectCompleted ? '检测完成，未发现需要归因的错误' : '暂无归因结果 — 点击「检测 + 归因」生成韦纳三维度归因与补救措施'"
    :image-size="80"
  />
  <div v-else>
    <div v-for="(item, i) in store.analyzed" :key="i" class="attr-card">
      <div class="attr-head">
        <el-tag type="danger" size="small">{{ errorTypeLabel(item.error.error_type) }}</el-tag>
        <div class="fix">
          <span class="original">{{ item.error.original }}</span>
          <template v-if="item.error.correction">
            <span class="arrow">→</span>
            <span class="correction">{{ item.error.correction }}</span>
          </template>
        </div>
        <span class="spacer"></span>
        <!-- 逐错误讲解：请求对第 i 条生成 LLM 个性化讲解（ResultsPanel 切到 LLM tab 并触发生成） -->
        <el-button size="small" text type="primary" @click="store.requestExplain(i)">讲解</el-button>
      </div>
      <div class="attribution-box" v-if="item.attribution">
        <div class="attr-main">
          <el-tag>{{ label(item.attribution.attribution, ATTRIBUTION_LABELS) }}</el-tag>
          <el-tag
            v-if="item.attribution.cefr && item.attribution.cefr !== 'unknown'"
            type="primary"
            effect="light"
          >
            CEFR {{ item.attribution.cefr }}
          </el-tag>
          <!-- 知识源引用（知识库富化；同一规则/点可被多个源背书）；旧历史数据回退 source 字符串 -->
          <template v-if="item.attribution.sources && item.attribution.sources.length > 0">
            <el-tag
              v-for="(s, j) in item.attribution.sources"
              :key="j"
              type="info"
              effect="plain"
              size="small"
            >
              {{ (s.title || s.source_id) + (s.location ? ' · ' + s.location : '') }}
            </el-tag>
          </template>
          <el-tag v-else-if="item.attribution.source" type="info" effect="plain" size="small">
            {{ item.attribution.source }}
          </el-tag>
        </div>
        <el-descriptions :column="3" size="small" class="dims">
          <el-descriptions-item label="归因方向">
            <el-tag type="success" effect="light" size="small">
              {{ label(item.attribution.weiner.locus, WEINER_LABELS) }}
            </el-tag>
          </el-descriptions-item>
          <el-descriptions-item label="稳定性">
            <el-tag type="success" effect="light" size="small">
              {{ label(item.attribution.weiner.stability, WEINER_LABELS) }}
            </el-tag>
          </el-descriptions-item>
          <el-descriptions-item label="可控性">
            <el-tag type="success" effect="light" size="small">
              {{ label(item.attribution.weiner.controllability, WEINER_LABELS) }}
            </el-tag>
          </el-descriptions-item>
        </el-descriptions>
        <div v-if="item.attribution.remedial.length > 0" class="remedial">
          <div class="remedial-title">补救措施</div>
          <ul>
            <li v-for="(r, j) in item.attribution.remedial" :key="j">{{ r }}</li>
          </ul>
        </div>
        <div
          v-if="item.attribution.examples && item.attribution.examples.length > 0"
          class="examples"
        >
          <div class="remedial-title">教材示例</div>
          <div v-for="(ex, j) in item.attribution.examples" :key="j" class="example">{{ ex }}</div>
        </div>
        <div class="trace">
          命中规则：{{ item.attribution.trace.matched_rule }} · 决策表：{{ item.attribution.trace.decision_table }}
        </div>
      </div>
      <el-alert
        v-else
        type="info"
        :closable="false"
        title="该语言暂无决策表，无法归因"
      />
    </div>
  </div>
</template>

<style scoped>
.attr-head .spacer { flex: 1; }
</style>
