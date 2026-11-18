export interface LSidebarItem {
  id: string
  label: string
  badge?: number
}
export interface LSidebarSection {
  title?: string
  items: LSidebarItem[]
}
export interface LColumn<T = any> {
  key: string
  label: string
  sortable?: boolean
  width?: string
  align?: 'left' | 'right'
}
export interface ToastAction {
  label: string
  onClick: () => void
}
export type ToastTone = 'info' | 'ok' | 'warn' | 'danger'
export interface ToastItem {
  id: number
  message: string
  tone: ToastTone
  action?: ToastAction
}
