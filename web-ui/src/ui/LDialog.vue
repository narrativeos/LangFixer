<script setup lang="ts">
import { ref, watch } from 'vue'

const props = defineProps<{ visible: boolean; title: string; width?: string }>()
const emit = defineEmits<{ 'update:visible': [value: boolean] }>()
const el = ref<HTMLDialogElement | null>(null)

watch(
  () => props.visible,
  (v) => {
    const d = el.value
    if (!d) return
    if (v && !d.open) d.showModal()
    else if (!v && d.open) d.close()
  },
)
function onClose() {
  emit('update:visible', false)
}
function onBackdrop(e: MouseEvent) {
  if ((e.target as HTMLElement).tagName === 'DIALOG') emit('update:visible', false)
}
</script>

<template>
  <dialog ref="el" class="l-dialog" :style="width ? { width } : undefined" @close="onClose" @click="onBackdrop">
    <h2>{{ title }}</h2>
    <div><slot /></div>
    <menu class="l-dialog__foot"><slot name="footer" /></menu>
  </dialog>
</template>
