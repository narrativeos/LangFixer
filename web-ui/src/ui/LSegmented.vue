<script setup lang="ts" generic="T extends string | number">
defineProps<{
  modelValue: T
  options: { value: T; label: string }[]
}>()
const emit = defineEmits<{ 'update:modelValue': [value: T] }>()
</script>

<template>
  <div class="l-segmented" role="tablist">
    <button
      v-for="o in options"
      :key="String(o.value)"
      type="button"
      role="tab"
      :aria-selected="o.value === modelValue ? 'true' : 'false'"
      @click="emit('update:modelValue', o.value)"
    >
      {{ o.label }}
    </button>
  </div>
</template>
