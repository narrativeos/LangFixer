<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from 'vue'
import Editor from '../components/Editor.vue'
import PreviewPanel from '../components/PreviewPanel.vue'
import ResultsPanel from '../components/ResultsPanel.vue'
import HistoryPanel from '../components/HistoryPanel.vue'
import { LSegmented } from '../ui'
import { useGecStore } from '../stores/gec'

/**
 * 工作台壳主体(v2):左侧校样画布(编辑 ⇄ 校样两态,同一份文本),
 * 右侧实用面板(摘要 ⇄ 历史)。历史不再占独立视图,收纳为 rail 页签;
 * ⌘F(工具栏「检索历史」)通过全局事件切到历史页签并聚焦搜索框。
 */
const store = useGecStore()

type CanvasMode = 'edit' | 'proof'
const canvasMode = ref<CanvasMode>('edit')
const canvasOptions: { value: CanvasMode; label: string }[] = [
  { value: 'edit', label: '编辑' },
  { value: 'proof', label: '校样' },
]

type RailTab = 'summary' | 'history'
const railTab = ref<RailTab>('summary')
const railOptions: { value: RailTab; label: string }[] = [
  { value: 'summary', label: '摘要' },
  { value: 'history', label: '历史' },
]

function focusHistory() {
  railTab.value = 'history'
}
onMounted(() => window.addEventListener('langfixer:focus-history-search', focusHistory))
onBeforeUnmount(() => window.removeEventListener('langfixer:focus-history-search', focusHistory))
</script>

<template>
  <div class="l-workbench">
    <section class="l-canvas" :aria-label="canvasMode === 'edit' ? '编辑' : '校样'">
      <div class="l-canvas-head">
        <LSegmented v-model="canvasMode" :options="canvasOptions" />
        <span v-if="canvasMode === 'proof' && store.resultsStale" class="l-canvas-note">
          文本已改动,标记可能过期,按 ⌘Enter 重新检测
        </span>
      </div>
      <Editor v-show="canvasMode === 'edit'" />
      <PreviewPanel v-show="canvasMode === 'proof'" />
    </section>

    <aside class="l-rail" aria-label="摘要与历史">
      <div class="l-rail-head">
        <LSegmented v-model="railTab" :options="railOptions" />
      </div>
      <div class="l-rail-body">
        <ResultsPanel v-show="railTab === 'summary'" />
        <HistoryPanel v-show="railTab === 'history'" :lang="store.lang" />
      </div>
    </aside>
  </div>
</template>

<style scoped>
.l-canvas-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-3);
  margin-bottom: var(--space-3);
}
.l-canvas-note {
  font-size: var(--fs-sm);
  color: var(--warn-ink);
}
.l-rail-head {
  display: flex;
  align-items: center;
}
</style>
