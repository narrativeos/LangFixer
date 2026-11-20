<script setup lang="ts">
defineProps<{
  modelValue: string | number
  options: { value: string | number; label: string }[]
  disabled?: boolean
  ariaLabel?: string
}>()
const emit = defineEmits<{ 'update:modelValue': [value: string | number] }>()
</script>

<template>
  <select
    class="l-select"
    :disabled="disabled"
    :aria-label="ariaLabel"
    :value="modelValue"
    @change="emit('update:modelValue', ($event.target as HTMLSelectElement).value)"
  >
    <option v-for="o in options" :key="String(o.value)" :value="o.value">{{ o.label }}</option>
  </select>
</template>
