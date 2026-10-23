/**
 * 检测管线层展示（① 本地 ZEN 规则 → ② 决策模型 → ②.5 知识召回 → ③-F 特征模型 → ③ LLM 兜底）
 *
 * - ①②②.5 状态来自服务端设置（决策模型是否启用 / 嵌入引擎是否配置）；
 * - ③-F 特征模型（三期）：level 2/3 且启用时显示（P0.5：L3 是唯一的 pre-LLM 门控）；
 *   高置信定案，LLM 收缩到纯长尾；
 * - ③ 按「响应级别」动态显示：L1 不启用 / L2 L3 门控弃权（或 L3 未启用时决策模型
 *   弃权）时触发 / L3 特征模型弃权（或特征模型未启用）时触发（无条件）。
 *
 * 纯函数：Editor.vue 的「检测管线」提示行据此渲染 el-tag，便于单测。
 */
export interface PipelineLayer {
  /** 层编号（① / ② / ②.5 / ③-F / ③），用作列表 key */
  no: string
  /** 展示文案（含状态后缀） */
  label: string
  /** el-tag 类型：'' = 主色（条件触发）/ success = 生效 / info = 未启用 / warning = 未配置 */
  type: '' | 'success' | 'info' | 'warning'
}

export interface PipelineState {
  /** 响应级别：1 = 规则（+ 决策模型，若显式启用）；2 = + L3 门控（L3 弃权 → LLM 兜底）；3 = + 强制 LLM */
  level: number
  /** 决策模型（vegaml）层是否启用（P0.5 起默认关闭；opt-in） */
  decisionEnabled: boolean
  /** LLM 是否已配置（api_key 非空） */
  llmEnabled: boolean
  /** 嵌入引擎是否已配置（未配置时 ②.5 知识向量召回静默跳过） */
  embeddingConfigured: boolean
  /** L3 特征模型层是否启用（P0.5：level 2/3 的 pre-LLM 门控；高置信定案，LLM 收缩到纯长尾） */
  l3Enabled: boolean
}

export function computePipelineLayers(s: PipelineState): PipelineLayer[] {
  const layers: PipelineLayer[] = [
    { no: '①', label: '① 本地 ZEN 规则', type: 'success' },
    {
      no: '②',
      label: `② 决策模型${s.decisionEnabled ? ' · 已启用' : ' · 未启用'}`,
      type: s.decisionEnabled ? 'success' : 'info',
    },
    {
      no: '②.5',
      label: `②.5 知识召回${s.embeddingConfigured ? ' · 已配置' : ' · 未配置'}`,
      type: s.embeddingConfigured ? 'success' : 'info',
    },
  ]
  if (s.level === 3) {
    if (s.l3Enabled) {
      // 三期：特征模型高置信定案，LLM 收缩到纯长尾
      layers.push({ no: '③-F', label: '③-F 特征模型 · 已启用', type: 'success' })
      layers.push(
        s.llmEnabled
          ? { no: '③', label: '③ LLM 兜底 · 仅长尾（特征模型弃权时）', type: 'success' }
          : { no: '③', label: '③ LLM 兜底 · 未配置', type: 'warning' },
      )
    } else {
      layers.push(
        s.llmEnabled
          ? { no: '③', label: '③ LLM 兜底 · 无条件', type: 'success' }
          : { no: '③', label: '③ LLM 兜底 · 未配置', type: 'warning' },
      )
    }
  } else if (s.level === 2) {
    // P0.5 重绑：L3 是唯一的 pre-LLM 门控——L3 启用时高置信定案、弃权 → LLM 兜底；
    // L3 未启用时回退旧语义：决策模型弃权 → LLM 兜底
    if (s.l3Enabled) {
      layers.push({ no: '③-F', label: '③-F 特征模型 · 已启用', type: 'success' })
      layers.push(
        s.llmEnabled
          ? { no: '③', label: '③ LLM 兜底 · 仅长尾（L3 弃权时）', type: 'success' }
          : { no: '③', label: '③ LLM 兜底 · 未配置', type: 'warning' },
      )
    } else if (s.decisionEnabled) {
      layers.push(
        s.llmEnabled
          ? { no: '③', label: '③ LLM 兜底 · 弃权时触发', type: '' }
          : { no: '③', label: '③ LLM 兜底 · 未配置', type: 'warning' },
      )
    } else {
      layers.push({
        no: '③',
        label: '③ LLM 兜底 · 未启用（等效 L1）',
        type: 'info',
      })
    }
  } else {
    layers.push({
      no: '③',
      label: '③ LLM 兜底 · 未启用',
      type: 'info',
    })
  }
  return layers
}

/**
 * 「本次管线」摘要（T1-3）：检测完成后，从各阶段计时推断哪些层实际跑了。
 *
 * 与 {@link computePipelineLayers}（检测前：按设置/级别推演「将走哪些层」）互补：
 * 本函数回答「这次实际跑了什么、各花了多久」。
 * 计时字段为 null / 缺失 = 该层本次未运行（未启用 / 未触发 / 服务不可用）。
 */
export interface RunLayerSummary {
  /** 层编号（① / ② / ②.5 / ③-F / ③ / Σ），用作列表 key */
  no: string
  /** 展示文案（含耗时） */
  label: string
  /** 该层本次是否实际运行 */
  ran: boolean
}

export function summarizeRun(t: {
  rule_ms: number
  nlp_ms?: number | null
  nlp_warmup_ms?: number | null
  decision_ms?: number | null
  l3_ms?: number | null
  recall_ms?: number | null
  llm_ms?: number | null
  total_ms: number
}): RunLayerSummary[] {
  const out: RunLayerSummary[] = []
  let ruleLabel = `① 规则 ${t.rule_ms}ms`
  if (t.nlp_ms != null) {
    ruleLabel += `（NLP ${t.nlp_ms}ms${t.nlp_warmup_ms != null ? ` 含预热 ${t.nlp_warmup_ms}ms` : ''}）`
  }
  out.push({ no: '①', label: ruleLabel, ran: true })
  out.push({
    no: '②',
    label: t.decision_ms != null ? `② 决策模型 ${t.decision_ms}ms` : '② 决策模型 · 未运行',
    ran: t.decision_ms != null,
  })
  out.push({
    no: '②.5',
    label: t.recall_ms != null ? `②.5 知识召回 ${t.recall_ms}ms` : '②.5 知识召回 · 未运行',
    ran: t.recall_ms != null,
  })
  out.push({
    no: '③-F',
    label: t.l3_ms != null ? `③-F 特征模型 ${t.l3_ms}ms` : '③-F 特征模型 · 未运行',
    ran: t.l3_ms != null,
  })
  out.push({
    no: '③',
    label: t.llm_ms != null ? `③ LLM 兜底 ${t.llm_ms}ms` : '③ LLM 兜底 · 未运行',
    ran: t.llm_ms != null,
  })
  out.push({ no: 'Σ', label: `总耗时 ${t.total_ms}ms`, ran: true })
  return out
}