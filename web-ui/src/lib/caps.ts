/**
 * 能力位（capability flags）：产品形态 = 共享核心工作区 + 三个扩展
 *
 * - learning     学习扩展：练习与复习、学习者画像入口
 * - proofreading 编校扩展：载体审校工作区、正误表导出
 * - ops          平台运营扩展：规则管理 / 知识维护 / 进化中心 / 高级设置
 *
 * 能力位是**本地偏好**（localStorage），不是身份认证：
 * ops 位在 web 形态的解锁需 `POST /gec/ops/unlock` token 验证，
 * 真正的服务端防线在受 token 保护的写端点（决策表 / 知识库 / 数据集）。
 */
export interface Caps {
  learning: boolean
  proofreading: boolean
  ops: boolean
}

export const CAPS_KEY = 'langfixer.caps.v1'

/** 默认：核心工作区对所有人开放；学习场景默认开（核心用户），编校 / 运营默认关 */
export const DEFAULT_CAPS: Caps = { learning: true, proofreading: false, ops: false }

/** 纯函数清洗（可测）：非法值 / 缺字段回落默认 */
export function sanitizeCaps(raw: unknown): Caps {
  if (typeof raw !== 'object' || raw === null) return { ...DEFAULT_CAPS }
  const c = raw as Partial<Caps>
  return {
    learning: typeof c.learning === 'boolean' ? c.learning : DEFAULT_CAPS.learning,
    proofreading:
      typeof c.proofreading === 'boolean' ? c.proofreading : DEFAULT_CAPS.proofreading,
    ops: typeof c.ops === 'boolean' ? c.ops : DEFAULT_CAPS.ops,
  }
}

export function loadCaps(): Caps {
  try {
    const raw = localStorage.getItem(CAPS_KEY)
    if (!raw) return { ...DEFAULT_CAPS }
    return sanitizeCaps(JSON.parse(raw))
  } catch {
    // 隐私模式 / 非浏览器环境（node 测试）：静默回落默认
    return { ...DEFAULT_CAPS }
  }
}

export function saveCaps(caps: Caps): void {
  try {
    localStorage.setItem(CAPS_KEY, JSON.stringify(caps))
  } catch {
    /* 忽略 */
  }
}

/** 顶层视图（App.vue 与能力门控共用） */
export type AppView =
  | 'workspace'
  | 'learning'
  | 'media'
  | 'rules'
  | 'knowledge'
  | 'evolution'
  | 'settings'

/** 视图依赖的能力位（null = 共享核心视图，始终可见） */
export function viewCapKey(v: AppView): keyof Caps | null {
  if (v === 'learning') return 'learning'
  if (v === 'media') return 'proofreading'
  if (v === 'rules' || v === 'knowledge' || v === 'evolution') return 'ops'
  return null
}

export function viewAllowed(v: AppView, caps: Caps): boolean {
  const key = viewCapKey(v)
  return key === null || caps[key]
}