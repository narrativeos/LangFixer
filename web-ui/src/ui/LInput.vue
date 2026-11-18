<script setup lang="ts">
withDefaults(
  defineProps<{ modelValue: string; placeholder?: string; disabled?: boolean }>(),
  { placeholder: '' },
)
const emit = defineEmits<{ 'update:modelValue': [value: string] }>()
</script>

<template>
  <input
    class="l-input"
    type="text"
    :value="modelValue"
    :placeholder="placeholder"
    :disabled="disabled"
    @input="emit('update:modelValue', ($event.target as HTMLInputElement).value)"
  />
</template>
