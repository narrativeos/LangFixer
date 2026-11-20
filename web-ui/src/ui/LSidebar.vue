<script setup lang="ts">
import type { LSidebarSection } from './types'

defineProps<{
  modelValue: string
  sections: LSidebarSection[]
}>()
const emit = defineEmits<{ select: [id: string] }>()
</script>

<template>
  <nav class="l-sidebar" aria-label="运营台导航">
    <div v-for="(s, si) in sections" :key="si">
      <div v-if="s.title" class="l-sb-title">{{ s.title }}</div>
      <button
        v-for="it in s.items"
        :key="it.id"
        type="button"
        class="l-sb-item"
        :aria-current="it.id === modelValue ? 'page' : undefined"
        @click="emit('select', it.id)"
      >
        {{ it.label }}
        <span v-if="it.badge && it.badge > 0" class="l-sb-badge">{{ it.badge }}</span>
      </button>
    </div>
  </nav>
</template>
