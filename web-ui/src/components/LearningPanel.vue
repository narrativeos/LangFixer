<script setup lang="ts">
/**
 * 练习与复习（学习扩展，Phase 2 建设中）
 *
 * 规划闭环：检测确认的错误 → 错题本 → 间隔复习（同型错改写题）→ 掌握度回顾。
 * 当前为占位视图：说明规划 + 引导完善学习者画像（讲解自动个性化的前提）。
 */
import { useGecStore } from '../stores/gec'

const store = useGecStore()

function goProfile() {
  store.requestNav('settings', 'profile')
}
</script>

<template>
  <el-card shadow="never">
    <template #header>
      <span class="card-title">练习与复习</span>
    </template>
    <p>
      学习扩展闭环正在建设中：<b>错题沉淀 → 间隔复习 → 掌握度回顾</b>。
      你在「检测」中确认并采纳的纠错，将逐步变成同型错的改写练习与周期性复习。
    </p>
    <p>
      现在就可以完善学习者档案（母语 + CEFR 自评）——LLM 讲解会据此自动个性化，
      讲解深度与示例语言贴合你的水平。
    </p>
    <el-button type="primary" plain @click="goProfile">完善我的档案</el-button>
  </el-card>
</template>

<style scoped>
.card-title { font-weight: 600; }
p { margin: 0 0 12px; line-height: 1.7; }
</style>