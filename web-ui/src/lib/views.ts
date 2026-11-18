import type { AppView } from './caps'

/** 双壳:工作台(学习者/编校者)与运营台(平台运营者);壳归属由注册表单点定义 */
export type Shell = 'workbench' | 'console'

export interface ViewDef {
  shell: Shell
  /** 侧栏 / 说明卡用名(与桌面 View 菜单词表一致的视图沿用原名) */
  label: string
}

export const VIEW_DEFS: Record<AppView, ViewDef> = {
  workspace: { shell: 'workbench', label: '校对工作台' },
  docjobs: { shell: 'workbench', label: '文档审校' },
  learning: { shell: 'workbench', label: '练习与复习' },
  media: { shell: 'workbench', label: '载体审校' },
  rules: { shell: 'console', label: '规则管理' },
  knowledge: { shell: 'console', label: '知识维护' },
  evolution: { shell: 'console', label: '进化中心' },
  layers: { shell: 'console', label: '各层贡献' },
  access: { shell: 'console', label: '接入' },
  settings: { shell: 'console', label: '设置' },
}

export const ALL_VIEWS = Object.keys(VIEW_DEFS) as AppView[]
export function shellOf(v: AppView): Shell {
  return VIEW_DEFS[v].shell
}
/** 运营台侧栏的固定顺序 */
export const CONSOLE_ORDER: AppView[] = [
  'rules',
  'knowledge',
  'evolution',
  'layers',
  'access',
  'settings',
]
