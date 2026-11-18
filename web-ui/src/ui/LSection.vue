<script setup lang="ts">
defineProps<{ title: string; desc?: string }>()
</script>

<template>
  <section class="l-section">
    <header class="l-section__head">
      <h2 class="l-section__title">{{ title }}</h2>
      <span v-if="desc" class="l-section__desc">{{ desc }}</span>
      <span class="l-section__actions"><slot name="actions" /></span>
    </header>
    <slot />
  </section>
</template>
