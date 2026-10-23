/**
 * 展示词表汉化（纯函数 + 常量，可单测）
 *
 * 后端 error_type / LayerReport.status 是英文 snake_case 实现细节，
 * 直接展示会泄漏到界面。本模块提供中文词表与回落规则（未收录 → 原文）。
 */

/** 错误类型（与 shared-model::error_types 全量常量对齐 + vegaml 映射后可能出现值） */
export const ERROR_TYPE_LABELS: Record<string, string> = {
  // 中文管线
  spelling: '拼写错误',
  grammar: '语法错误',
  collocation: '搭配不当',
  punctuation: '标点错误',
  anomaly: '文本污染',
  // 英文管线
  article: '冠词错误',
  agreement: '一致错误',
  tense: '时态错误',
  preposition: '介词错误',
  morphology: '词形错误',
  adjective: '形容词错误',
  adverb: '副词错误',
  noun: '名词错误',
  pronoun: '代词错误',
  verb: '动词错误',
  infinitive: '不定式错误',
  gerund: '动名词错误',
  subjunctive: '虚拟语气错误',
  negation: '否定错误',
  // 日文管线
  particle: '助词错误',
  conjugation: '活用错误',
  honorific: '敬语错误',
  word_order: '语序错误',
  // 兜底类别
  other: '其他',
}

export function errorTypeLabel(key: string): string {
  return ERROR_TYPE_LABELS[key] ?? key
}

/** 层级状态（LayerReport.status，与 gec-core LayerReport 对齐） */
export const LAYER_STATUS_LABELS: Record<string, string> = {
  ran: '已执行',
  decided: '定案',
  abstained: '弃权',
  unavailable: '不可用已回退',
  skipped: '未触发',
}

export function layerStatusLabel(status: string): string {
  return LAYER_STATUS_LABELS[status] ?? status
}
