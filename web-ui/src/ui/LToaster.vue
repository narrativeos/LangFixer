<script setup lang="ts">
import { dismissToast, toasts } from './toast'
</script>

<template>
  <div class="l-toaster" role="status" aria-live="polite">
    <div v-for="t in toasts" :key="t.id" class="l-toast">
      <span :class="t.tone !== 'info' ? `l-t-${t.tone}` : ''">{{ t.message }}</span>
      <button v-if="t.action" type="button" @click="t.action.onClick(); dismissToast(t.id)">
        {{ t.action.label }}
      </button>
    </div>
  </div>
</template>
