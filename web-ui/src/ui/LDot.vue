<script setup lang="ts">
defineProps<{ tone: 'ok' | 'warn' | 'danger' | 'muted' }>()
</script>

<template>
  <i class="l-dot" :class="`l-t-${tone}`" />
</template>
