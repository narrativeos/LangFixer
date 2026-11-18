<script setup lang="ts">
defineProps<{ modelValue: boolean; disabled?: boolean; ariaLabel?: string }>()
const emit = defineEmits<{ 'update:modelValue': [value: boolean] }>()
</script>

<template>
  <button
    type="button"
    class="l-switch"
    :class="{ 'l-on': modelValue }"
    role="switch"
    :aria-checked="modelValue ? 'true' : 'false'"
    :aria-label="ariaLabel"
    :disabled="disabled"
    @click="emit('update:modelValue', !modelValue)"
  >
    <i />
  </button>
</template>
