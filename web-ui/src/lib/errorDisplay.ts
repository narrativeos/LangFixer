/**
 * 检测结果展示词表与纯函数（可单测）
 *
 * 从 ErrorList.vue 拆出：分类 / 来源层词表、置信度等级、共识文案、实例上下文。
 * 全部无副作用（instanceContext 接收 text 参数而非读 store）。
 */
import type { ErrorItem } from '../types'

export const CAT_LABELS: Record<string, string> = {
  grammar: '语法',
  spelling: '拼写',
  typography: '标点',
  style: '风格',
  other: '其他',
}
export const CAT_TYPES: Record<string, 'danger' | 'warning' | 'primary' | 'info' | 'success'> = {
  grammar: 'danger',
  spelling: 'warning',
  typography: 'primary',
  style: 'primary',
  other: 'info',
}

/** 来源层显示（徽章 + tooltip），与后端 `Error.source` / `LayerReport.layer` 同词表 */
export const SOURCE_LABELS: Record<string, string> = {
  rule: '规则',
  decision: '决策模型',
  statistical: '统计模型',
  knowledge: '知识召回',
  feature: '特征模型',
  llm: 'LLM 兜底',
}
export const SOURCE_TIPS: Record<string, string> = {
  rule: '该错误由规则引擎检出（词表 / 规则匹配，确定性最高的层）',
  decision: '该错误由决策模型层检出（vegaml：处理规则覆盖不到的模糊情况）',
  statistical: '该错误由统计 GEC 模型检出',
  knowledge: '该错误由知识召回检出（历史同类案例命中，②.5）',
  feature: '该错误由 L3 特征模型检出（level 3 特征模型高置信定案，LLM 收缩到纯长尾）',
  llm: '该错误由 LLM 兜底层检出（三级响应：规则与决策模型未覆盖，LLM 全文复查发现）',
}
export const SOURCE_TAG_TYPES: Record<string, 'primary' | 'success' | 'info'> = {
  feature: 'primary',
  llm: 'success',
}

/** 置信度等级：各层数值不可直接比较（规则是启发式常数 / 特征模型是分类概率 /
 *  LLM 无校准数值），按来源映射为高 / 中 / 低等级，原始数值放 tooltip */
export function confBand(e: ErrorItem): { label: string; type: 'success' | 'warning' | 'info' } {
  if (e.source === 'llm') return { label: 'LLM 判断', type: 'info' }
  if (e.confidence >= 0.8) return { label: '高置信', type: 'success' }
  if (e.confidence >= 0.55) return { label: '中置信', type: 'warning' }
  return { label: '低置信', type: 'info' }
}

/** 多层共识徽章文案：source + also_found_by 的全部命中层 */
export function consensusText(e: ErrorItem): string {
  const layers = [e.source, ...(e.also_found_by ?? [])].filter(Boolean) as string[]
  return layers.map((s) => SOURCE_LABELS[s] ?? s).join(' / ')
}

/** 同类组实例清单的上下文文案：以「…前文【original】后文…」展示该实例在原文中的位置，
 *  替代裸 UTF-8 字节偏移（对用户无意义）。span 换算与 lib/correct.ts 同口径。 */
export function instanceContext(text: string, e: ErrorItem): string {
  const byteToU16 = new Map<number, number>()
  let byte = 0
  for (let u = 0; u < text.length; ) {
    byteToU16.set(byte, u)
    const cp = text.codePointAt(u) ?? 0
    byte += cp < 0x80 ? 1 : cp < 0x800 ? 2 : cp < 0x10000 ? 3 : 4
    u += cp > 0xffff ? 2 : 1
  }
  const a = byteToU16.get(e.span[0]) ?? -1
  const b = byteToU16.get(e.span[1]) ?? -1
  if (a < 0 || b <= a || b > text.length) return e.original
  const before = text.slice(Math.max(0, a - 8), a)
  const after = text.slice(b, Math.min(text.length, b + 8))
  return `${a > 8 ? '…' : ''}${before}【${text.slice(a, b)}】${after}${b < text.length - 8 ? '…' : ''}`
}
