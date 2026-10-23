<script setup lang="ts">
/**
 * 规则管理面板
 *
 * - 搜索（规则 ID / 描述 / 示例）+ 分类过滤
 * - 单条开关：PUT /gec/rules/:lang/toggles（持久化，重启不丢）
 * - 分类批量开关（同端点批量）
 * - 示例展开（错误 → 改正）
 * - 试算：用规则示例的错误句跑检测，验证规则当前是否生效
 */
import { ref, watch, computed, onMounted } from 'vue'
import { errMsg } from '../lib/err'

import { ElMessage } from 'element-plus'
import { api } from '../api/client'
import type { ErrorItem, RuleExample, RuleInfo, RulesResponse } from '../types'

const props = defineProps<{ lang: string }>()

const rules = ref<RulesResponse | null>(null)
const loading = ref(false)
const error = ref('')
const search = ref('')
const category = ref('')
const pending = ref<Set<string>>(new Set())
const pendingCat = ref<Record<string, boolean>>({})
const expanded = ref<Set<string>>(new Set())
const tryResults = ref<Record<string, { hit: boolean; errors: ErrorItem[] }>>({})
const trying = ref<Set<string>>(new Set())
/** 示例对缓存（id → 示例列表；列表接口只带 example_count，示例按需分页加载） */
const examplesCache = ref<Record<string, RuleExample[]>>({})
const examplesLoading = ref<Set<string>>(new Set())

async function load() {
  loading.value = true
  error.value = ''
  try {
    rules.value = await api.listRules(props.lang)
    pending.value = new Set()
    pendingCat.value = {}
    expanded.value = new Set()
    examplesCache.value = {}
    examplesLoading.value = new Set()
    tryResults.value = {}
  } catch (e) {
    error.value = errMsg(e)
  } finally {
    loading.value = false
  }
}

/** 按需加载某规则的示例对（带缓存；返回示例列表，失败/无示例返回空） */
async function loadExamples(r: RuleInfo): Promise<RuleExample[]> {
  if (examplesCache.value[r.id]) return examplesCache.value[r.id]
  if (r.example_count === 0) return []
  examplesLoading.value.add(r.id)
  try {
    const list = await api.listRuleExamples(props.lang, r.id, 1, 50)
    examplesCache.value = { ...examplesCache.value, [r.id]: list.examples }
    return list.examples
  } catch (e) {
    ElMessage.error(`示例加载失败：${errMsg(e)}`)
    return []
  } finally {
    examplesLoading.value.delete(r.id)
  }
}

const filtered = computed(() => {
  if (!rules.value) return []
  const q = search.value.trim().toLowerCase()
  return rules.value.rules
    .filter((r) => (category.value ? r.category === category.value : true))
    .filter((r) => {
      if (!q) return true
      if (r.id.toLowerCase().includes(q)) return true
      return r.description.toLowerCase().includes(q)
    })
})

function isEnabled(r: RuleInfo): boolean {
  return r.enabled
}
function isPending(id: string): boolean {
  return pending.value.has(id)
}

async function toggle(id: string, enabled: boolean) {
  if (!rules.value) return
  pending.value.add(id)
  try {
    const res = await api.putRuleToggles(props.lang, { [id]: enabled })
    // 用服务端返回的全量状态刷新（含其他并行变更）
    for (const r of rules.value.rules) {
      if (res.toggles[r.id] !== undefined) r.enabled = res.toggles[r.id]
    }
    pending.value.delete(id)
  } catch (e) {
    pending.value.delete(id)
    ElMessage.error(`开关失败：${errMsg(e)}`)
  }
}

async function toggleCategory(catId: string, enabled: boolean) {
  if (!rules.value) return
  const ids = rules.value.rules.filter((r) => r.category === catId).map((r) => r.id)
  if (ids.length === 0) return
  pendingCat.value[catId] = enabled
  try {
    const res = await api.putRuleToggles(
      props.lang,
      Object.fromEntries(ids.map((id) => [id, enabled])),
    )
    for (const r of rules.value.rules) {
      if (res.toggles[r.id] !== undefined) r.enabled = res.toggles[r.id]
    }
    delete pendingCat.value[catId]
  } catch (e) {
    delete pendingCat.value[catId]
    ElMessage.error(`批量开关失败：${errMsg(e)}`)
  }
}

async function toggleExpand(id: string) {
  if (expanded.value.has(id)) {
    expanded.value.delete(id)
    return
  }
  expanded.value.add(id)
  // 展开时按需加载示例对（带缓存）
  const r = rules.value?.rules.find((x) => x.id === id)
  if (r) await loadExamples(r)
}

/** 试算：用规则第一条示例的错误句跑检测，验证规则是否命中 */
async function tryRule(r: RuleInfo) {
  const examples = await loadExamples(r)
  const text = examples[0]?.wrong
  if (!text) {
    ElMessage.warning('该规则没有示例句，无法试算')
    return
  }
  trying.value.add(r.id)
  try {
    const resp = await api.detect(props.lang, text)
    const errors = resp.errors
    const hit = errors.some((e) => e.rule_id === r.id)
    tryResults.value[r.id] = { hit, errors }
    if (hit) ElMessage.success(`试算命中 ${r.id}`)
    else ElMessage.warning(`试算未命中 ${r.id}（当前 ${errors.length} 个错误，可能规则已停用或条件不满足）`)
  } catch (e) {
    ElMessage.error(`试算失败：${errMsg(e)}`)
  } finally {
    trying.value.delete(r.id)
  }
}

onMounted(load)
watch(() => props.lang, load)
</script>

<template>
  <el-card shadow="never">
    <template #header>
      <div class="card-header">
        <span class="card-title">规则管理</span>
        <span class="sub">开关即时生效并持久化（重启不丢）</span>
        <span class="spacer"></span>
        <el-button size="small" :loading="loading" @click="load">
          <el-icon v-if="!loading"><Refresh /></el-icon>
          刷新
        </el-button>
      </div>
    </template>
    <div v-if="loading" class="loading">
      <el-icon class="is-loading"><Loading /></el-icon>
      加载中…
    </div>
    <el-alert v-else-if="error" type="error" :closable="false" :title="error" />
    <div v-else-if="rules">
      <div class="filters">
        <el-input
          v-model="search"
          placeholder="搜索规则 ID / 描述"
          clearable
          style="width: 260px"
        />
        <el-select v-model="category" clearable placeholder="全部分类" style="width: 180px">
          <el-option v-for="c in rules.categories" :key="c.id" :label="c.name" :value="c.id" />
        </el-select>
        <span class="spacer"></span>
        <span class="sub">
          {{ filtered.length }} / {{ rules.rules.length }} 条
        </span>
      </div>

      <div v-for="cat in rules.categories" :key="cat.id" class="category-section">
        <div class="category-header">
          <strong>{{ cat.name }}</strong>
          <span class="count">
            {{ rules.rules.filter((x) => x.category === cat.id && (category === '' || category === cat.id)).length }} 条
          </span>
          <span class="spacer"></span>
          <el-button
            size="small"
            :loading="pendingCat[cat.id] !== undefined"
            @click="toggleCategory(cat.id, true)"
          >
            全部启用
          </el-button>
          <el-button
            size="small"
            :loading="pendingCat[cat.id] !== undefined"
            @click="toggleCategory(cat.id, false)"
          >
            全部停用
          </el-button>
        </div>
        <div
          v-for="r in filtered.filter((x) => x.category === cat.id)"
          :key="r.id"
          class="rule-row"
        >
          <el-switch
            :model-value="r.enabled"
            :loading="isPending(r.id)"
            @change="(v: string | number | boolean) => toggle(r.id, v === true)"
          />
          <div class="rule-main">
            <div class="rule-line">
              <span class="rule-id">{{ r.id }}</span>
              <span class="rule-desc">{{ r.description }}</span>
            </div>
            <div class="rule-actions">
              <el-button
                link
                size="small"
                :loading="examplesLoading.has(r.id)"
                @click="toggleExpand(r.id)"
                v-if="r.example_count"
              >
                {{
                  expanded.has(r.id)
                    ? '收起示例'
                    : `示例（${examplesCache[r.id]?.length ?? r.example_count}）`
                }}
              </el-button>
              <el-button
                link
                size="small"
                :loading="trying.has(r.id)"
                @click="tryRule(r)"
                v-if="r.example_count"
              >
                试算
              </el-button>
            </div>
            <div v-if="expanded.has(r.id) && r.example_count" class="examples">
              <div v-if="!examplesCache[r.id]?.length" class="sub">示例加载中…</div>
              <div
                v-for="ex in examplesCache[r.id]"
                :key="ex.id"
                class="example"
              >
                <span class="wrong">{{ ex.wrong }}</span>
                <span class="arrow">→</span>
                <span class="right">{{ ex.right }}</span>
              </div>
              <div v-if="tryResults[r.id]" class="try-out" :class="tryResults[r.id].hit ? 'hit' : 'miss'">
                <strong>{{ tryResults[r.id].hit ? '✓ 试算命中该规则' : '✗ 试算未命中该规则' }}</strong>
                <span class="sub">
                  （检测出 {{ tryResults[r.id].errors.length }} 个错误：
                  {{ tryResults[r.id].errors.map((e) => e.error_type).join('、') || '无' }}）
                </span>
              </div>
            </div>
          </div>
        </div>
      </div>
      <el-empty v-if="filtered.length === 0" description="无匹配规则" :image-size="60" />
    </div>
  </el-card>
</template>

<style scoped>
.filters { display: flex; align-items: center; gap: 10px; margin-bottom: 12px; }
.filters .spacer { flex: 1; }
.category-section { margin-bottom: 14px; }
.category-header {
  display: flex; align-items: center; gap: 8px;
  margin-bottom: 6px; padding-bottom: 6px; border-bottom: 1px solid var(--border);
}
.category-header strong { font-size: 13.5px; }
.category-header .count { color: var(--muted); font-size: 12px; }
.category-header .spacer { flex: 1; }
.rule-row { display: flex; align-items: flex-start; gap: 12px; padding: 6px 8px; border-radius: 8px; }
.rule-row:hover { background: var(--surface-2); }
.rule-main { flex: 1; min-width: 0; }
.rule-line { display: flex; align-items: baseline; gap: 10px; }
.rule-id { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12.5px; min-width: 200px; }
.rule-desc { color: var(--muted); font-size: 13px; }
.rule-actions { margin-top: 2px; }
.examples { margin-top: 6px; display: flex; flex-direction: column; gap: 4px; }
.example { font-size: 13px; }
.example .wrong { color: #c05252; }
.example .right { color: #3f9e63; }
.example .arrow { color: var(--muted); margin: 0 4px; }
.try-out { margin-top: 6px; font-size: 13px; }
.try-out.hit { color: #3f9e63; }
.try-out.miss { color: #c05252; }
.loading { display: flex; align-items: center; gap: 8px; color: var(--muted); }
</style>