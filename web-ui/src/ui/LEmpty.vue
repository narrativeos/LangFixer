<script setup lang="ts">
defineProps<{ text: string; hint?: string }>()
</script>

<template>
  <div class="l-empty">
    <div>{{ text }}</div>
    <div v-if="hint" class="l-empty__hint">{{ hint }}</div>
    <div class="l-empty__action"><slot name="action" /></div>
  </div>
</template>
