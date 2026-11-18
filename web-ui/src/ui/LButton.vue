<script setup lang="ts">
withDefaults(
  defineProps<{
    variant?: 'default' | 'primary' | 'ghost' | 'danger'
    size?: 'md' | 'sm'
    disabled?: boolean
    loading?: boolean
    title?: string
    ariaLabel?: string
  }>(),
  { variant: 'default', size: 'md' },
)
</script>

<template>
  <button
    type="button"
    class="l-btn"
    :class="[variant !== 'default' ? `l-${variant}` : '', size === 'sm' ? 'l-sm' : '']"
    :disabled="disabled || loading"
    :aria-busy="loading ? 'true' : undefined"
    :title="title"
    :aria-label="ariaLabel"
  >
    <span v-if="loading" class="l-btn-spin" aria-hidden="true"></span>
    <slot />
  </button>
</template>
