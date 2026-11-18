<script setup lang="ts">
import { onBeforeUnmount, ref, watch } from 'vue'

withDefaults(defineProps<{ align?: 'start' | 'end' }>(), { align: 'start' })
const open = ref(false)
const root = ref<HTMLElement | null>(null)

function onDocMouse(e: MouseEvent) {
  if (open.value && root.value && !root.value.contains(e.target as Node)) open.value = false
}
function onKey(e: KeyboardEvent) {
  if (e.key === 'Escape' && open.value) {
    open.value = false
    e.stopPropagation()
  }
}
watch(open, (v) => {
  if (v) {
    document.addEventListener('mousedown', onDocMouse)
    window.addEventListener('keydown', onKey)
  } else {
    document.removeEventListener('mousedown', onDocMouse)
    window.removeEventListener('keydown', onKey)
  }
})
onBeforeUnmount(() => {
  document.removeEventListener('mousedown', onDocMouse)
  window.removeEventListener('keydown', onKey)
})
</script>

<template>
  <span ref="root" class="l-popover">
    <slot name="trigger" :toggle="() => (open = !open)" :open="open" />
    <div v-if="open" class="l-popover__panel" :class="{ 'l-align-end': align === 'end' }">
      <slot :close="() => (open = false)" />
    </div>
  </span>
</template>
