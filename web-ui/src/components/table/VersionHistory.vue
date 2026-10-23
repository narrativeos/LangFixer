<script setup lang="ts">
/**
 * 决策表版本历史（对话框）
 *
 * - 列出该语言的版本（新 → 旧；每次 PUT / 回滚都产生一条）
 * - 一键回滚：把历史版本重新走校验 + 原子替换（回滚本身也产生新版本）
 */
import { ref } from 'vue'
import { errMsg } from '../../lib/err'

import { ElMessage, ElMessageBox } from 'element-plus'
import { api } from '../../api/client'
import type { TableVersion } from '../../types'
import { formatWhen } from '../../lib/format'

const props = defineProps<{
  lang: string
  visible: boolean
}>()

const emit = defineEmits<{
  close: []
  changed: []
}>()

const versions = ref<TableVersion[]>([])
const loading = ref(false)
const rolling = ref(0)

async function load() {
  if (!props.visible) return
  loading.value = true
  try {
    versions.value = await api.listTableVersions(props.lang, 100)
  } catch (e) {
    ElMessage.error(`加载版本历史失败：${errMsg(e)}`)
  } finally {
    loading.value = false
  }
}

async function rollback(v: TableVersion) {
  try {
    await ElMessageBox.confirm(
      `回滚到 ${v.saved_at} 的版本（${v.rule_count} 行）？\n当前表将被该版本内容替换（回滚本身也会存为新版本，可再回滚回来）。`,
      '回滚确认',
      { type: 'warning', confirmButtonText: '回滚', cancelButtonText: '取消' },
    )
  } catch {
    return
  }
  rolling.value = v.id
  try {
    await api.rollbackTable(props.lang, v.id)
    ElMessage.success(`已回滚到版本 #${v.id}（${v.saved_at}）`)
    await load()
    emit('changed')
  } catch (e) {
    ElMessage.error(`回滚失败：${errMsg(e)}`)
  } finally {
    rolling.value = 0
  }
}
</script>

<template>
  <el-dialog :model-value="props.visible" title="版本历史" width="680px" @update:model-value="(v: boolean) => !v && emit('close')" @open="load">
    <div v-if="loading" class="sub">加载中…</div>
    <el-table v-else :data="versions" size="small" empty-text="暂无版本（保存一次决策表后自动生成）">
      <el-table-column prop="id" label="#" width="60" />
      <el-table-column label="保存时间" width="180">
        <template #default="{ row }">
          <span :title="row.saved_at">{{ formatWhen(row.saved_at) }}</span>
        </template>
      </el-table-column>
      <el-table-column prop="rule_count" label="规则数" width="90" />
      <el-table-column label="操作" width="100">
        <template #default="{ row }">
          <el-button
            size="small"
            :loading="rolling === row.id"
            :disabled="rolling !== 0 && rolling !== row.id"
            @click="rollback(row)"
          >
            回滚
          </el-button>
        </template>
      </el-table-column>
    </el-table>
    <template #footer>
      <el-button @click="emit('close')">关闭</el-button>
    </template>
  </el-dialog>
</template>

<style scoped>
.sub { color: var(--muted); font-size: 13px; padding: 8px 0; }
</style>