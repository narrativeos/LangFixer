<script setup lang="ts">
const props = defineProps<{ value: number }>()
void props
</script>

<template>
  <div
    class="l-progress"
    role="progressbar"
    :aria-valuenow="Math.round(value * 100)"
    aria-valuemin="0"
    aria-valuemax="100"
  >
    <i :style="{ width: `${Math.min(100, Math.max(0, value * 100))}%` }" />
  </div>
</template>
