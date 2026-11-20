<script setup lang="ts">
/**
 * 决策表版本历史（对话框）
 *
 * - 列出该语言的版本（新 → 旧；每次 PUT / 回滚都产生一条）
 * - 一键回滚（行内 LPopover 确认）：把历史版本重新走校验 + 原子替换
 *   （回滚本身也产生新版本）
 */
import { ref, watch } from 'vue'
import { errMsg } from '../../lib/err'

import { api } from '../../api/client'
import type { TableVersion } from '../../types'
import { formatWhen } from '../../lib/format'
import { LButton, LDialog, LEmpty, LPopover, LTable, toast } from '../../ui'
import type { LColumn } from '../../ui'

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

const cols: LColumn<TableVersion>[] = [
  { key: 'id', label: '版本', width: '64px' },
  { key: 'saved_at', label: '保存时间', width: '180px' },
  { key: 'rule_count', label: '规则数', width: '90px' },
  { key: 'actions', label: '操作', width: '104px' },
]

watch(
  () => props.visible,
  (v) => {
    if (v) void load()
  },
)

async function load() {
  if (!props.visible) return
  loading.value = true
  try {
    versions.value = await api.listTableVersions(props.lang, 100)
  } catch (e) {
    toast(`加载版本历史失败：${errMsg(e)}`, 'danger')
  } finally {
    loading.value = false
  }
}

async function rollback(v: TableVersion) {
  rolling.value = v.id
  try {
    await api.rollbackTable(props.lang, v.id)
    toast(`已回滚到版本 #${v.id}（${formatWhen(v.saved_at)}）`, 'ok')
    await load()
    emit('changed')
  } catch (e) {
    toast(`回滚失败：${errMsg(e)}`, 'danger')
  } finally {
    rolling.value = 0
  }
}
</script>

<template>
  <LDialog
    :visible="props.visible"
    title="版本历史"
    width="680px"
    @update:visible="(v: boolean) => !v && emit('close')"
  >
    <div v-if="loading" class="vh-loading">加载中…</div>
    <LEmpty
      v-else-if="versions.length === 0"
      text="暂无版本"
      hint="保存一次决策表后自动生成，之后每次保存 / 回滚都会留档"
    />
    <LTable v-else :columns="cols" :rows="versions" row-key="id" compact>
      <template #col-saved_at="{ row }">
        <span :title="row.saved_at">{{ formatWhen(row.saved_at) }}</span>
      </template>
      <template #col-actions="{ row }">
        <LPopover align="end">
          <template #trigger="{ toggle }">
            <LButton
              size="sm"
              :loading="rolling === row.id"
              :disabled="rolling !== 0 && rolling !== row.id"
              @click="toggle()"
            >
              回滚
            </LButton>
          </template>
          <template #default="{ close }">
            <div class="vh-confirm">
              <p>
                回滚到 {{ formatWhen(row.saved_at) }} 的版本（{{ row.rule_count }} 行）？当前表将被该版本内容替换（回滚本身也会存为新版本，可再回滚回来）。
              </p>
              <div class="vh-confirm__foot">
                <LButton size="sm" @click="close()">取消</LButton>
                <LButton size="sm" variant="primary" @click="rollback(row); close()">回滚</LButton>
              </div>
            </div>
          </template>
        </LPopover>
      </template>
    </LTable>
    <template #footer>
      <LButton @click="emit('close')">关闭</LButton>
    </template>
  </LDialog>
</template>

<style scoped>
.vh-loading { color: var(--muted); font-size: var(--fs-sm); padding: var(--space-2) 0; }
.vh-confirm { max-width: 320px; }
.vh-confirm p { margin: 0 0 var(--space-3); font-size: var(--fs-sm); color: var(--text-2); }
.vh-confirm__foot { display: flex; justify-content: flex-end; gap: var(--space-2); }
</style>
