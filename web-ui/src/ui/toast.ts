import { ref } from 'vue'
import type { ToastAction, ToastItem, ToastTone } from './types'

export const toasts = ref<ToastItem[]>([])
let seq = 1

export function toast(
  message: string,
  tone: ToastTone = 'info',
  action?: ToastAction,
  duration = 4000,
): number {
  const item: ToastItem = { id: seq++, message, tone, action }
  toasts.value.push(item)
  if (duration > 0) setTimeout(() => dismissToast(item.id), duration)
  return item.id
}

export function dismissToast(id: number): void {
  toasts.value = toasts.value.filter((t) => t.id !== id)
}
