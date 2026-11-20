<script setup lang="ts" generic="T extends Record<string, any>">
import { computed, ref } from 'vue'
import type { LColumn } from './types'

const props = withDefaults(
  defineProps<{
    columns: LColumn<T>[]
    rows: T[]
    rowKey?: string
    selection?: 'multiple'
    selected?: (string | number)[]
    compact?: boolean
  }>(),
  { compact: false, selected: () => [] },
)
const emit = defineEmits<{
  'update:selected': [keys: (string | number)[]]
  'row-click': [row: T, index: number]
  'row-contextmenu': [row: T, index: number, e: MouseEvent]
}>()

function keyOf(row: T, i: number): string | number {
  return props.rowKey ? (row[props.rowKey] as string | number) : i
}
const isSelected = (row: T, i: number) => props.selected.includes(keyOf(row, i))

function toggle(row: T, i: number, append: boolean) {
  const k = keyOf(row, i)
  const cur = props.selected
  const next = append
    ? cur.includes(k)
      ? cur.filter((x) => x !== k)
      : [...cur, k]
    : cur.length === 1 && cur[0] === k
      ? []
      : [k]
  emit('update:selected', next)
}
function toggleAll() {
  emit(
    'update:selected',
    props.selected.length === props.rows.length ? [] : props.rows.map((r, i) => keyOf(r, i)),
  )
}
function onRowClick(row: T, i: number, e: MouseEvent) {
  if (props.selection === 'multiple' && (e.metaKey || e.ctrlKey || e.shiftKey)) {
    toggle(row, i, e.metaKey || e.ctrlKey)
    return
  }
  if (props.selection === 'multiple') toggle(row, i, false)
  emit('row-click', row, i)
}

const sortKey = ref<string | null>(null)
const sortDir = ref(1)
function sortBy(key: string) {
  if (sortKey.value === key) sortDir.value = -sortDir.value
  else {
    sortKey.value = key
    sortDir.value = 1
  }
}
const sortedRows = computed(() => {
  if (!sortKey.value) return props.rows
  const k = sortKey.value
  return [...props.rows].sort((a, b) => {
    const av = a[k]
    const bv = b[k]
    if (av === bv) return 0
    return (av === null || av === undefined ? -1 : av > bv ? 1 : -1) * sortDir.value
  })
})
</script>

<template>
  <table class="l-table" :class="{ 'l-compact': compact }">
    <thead>
      <tr>
        <th v-if="selection === 'multiple'" style="width: 34px">
          <input
            type="checkbox"
            style="accent-color: var(--accent)"
            :checked="rows.length > 0 && selected.length === rows.length"
            @change="toggleAll"
          />
        </th>
        <th
          v-for="c in columns"
          :key="c.key"
          :style="{ width: c.width, textAlign: c.align }"
        >
          <button v-if="c.sortable" type="button" class="l-sort" @click="sortBy(c.key)">
            {{ c.label }}{{ sortKey === c.key ? (sortDir === 1 ? ' ↑' : ' ↓') : '' }}
          </button>
          <template v-else>{{ c.label }}</template>
        </th>
      </tr>
    </thead>
    <tbody>
      <tr
        v-for="(row, i) in sortedRows"
        :key="keyOf(row, i)"
        :class="{ 'l-selected': isSelected(row, i) }"
        @click="onRowClick(row, i, $event)"
        @contextmenu="emit('row-contextmenu', row, i, $event)"
      >
        <td v-if="selection === 'multiple'">
          <input
            type="checkbox"
            style="accent-color: var(--accent)"
            :checked="isSelected(row, i)"
            @click.stop
            @change="toggle(row, i, true)"
          />
        </td>
        <td v-for="c in columns" :key="c.key" :style="{ textAlign: c.align }">
          <slot :name="`col-${c.key}`" :row="row" :index="i">{{ row[c.key] }}</slot>
        </td>
      </tr>
    </tbody>
  </table>
</template>
