<script setup lang="ts">
defineProps<{ modelValue: boolean; disabled?: boolean; label?: string }>()
const emit = defineEmits<{ 'update:modelValue': [value: boolean] }>()
</script>

<template>
  <label class="l-checkbox">
    <input
      type="checkbox"
      :checked="modelValue"
      :disabled="disabled"
      @change="emit('update:modelValue', ($event.target as HTMLInputElement).checked)"
    />
    <span v-if="label">{{ label }}</span>
  </label>
</template>
