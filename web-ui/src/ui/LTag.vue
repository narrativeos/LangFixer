<script setup lang="ts">
defineProps<{ tone?: 'neutral' | 'ok' | 'warn' | 'danger' | 'accent' }>()
</script>

<template>
  <span class="l-tag" :class="tone && tone !== 'neutral' ? `l-t-${tone}` : ''"><slot /></span>
</template>
