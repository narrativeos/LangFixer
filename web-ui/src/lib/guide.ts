/**
 * 常见错误 → 解决指引映射（T1-5 引导增强）
 *
 * 后端错误原文（如「决策模型不可达」「feature-model 不可达」）只有"出了什么错"，
 * 没有"怎么解决"。本模块按错误文本模式匹配，返回可操作的修复指引
 * （去哪里配置 / 如何降级），由前端在错误展示点（toast / alert）附加。
 *
 * 纯函数：便于单测；匹配不中返回 null（调用方只显示原始错误）。
 */

/** 一条修复指引：去哪里 + 做什么 + 如何降级 */
export interface ErrorGuide {
  /** 指引正文（展示在错误下方，前缀「如何修复：」由调用方加） */
  text: string
  /** 跳转目标顶层视图（'settings' = 设置页；null = 无需跳转） */
  view: 'settings' | null
}

interface GuideRule {
  /** 错误文本关键字（小写匹配；任一命中即算命中）。
   *  规则按「具体 → 宽泛」排序：更具体的短语（如「决策表」）必须排在宽泛词（如 'llm'）之前 */
  patterns: string[]
  guide: ErrorGuide
}

const SETTINGS_GUIDES: GuideRule[] = [
  {
    // 具体短语优先：「决策表 / 词表」错误指向数据目录，而非决策模型
    patterns: ['决策表', 'tables_dir', '词表', 'lexicon'],
    guide: {
      text: '「设置 → 数据目录」：检查决策表 / 词表目录并点「目录测试」。',
      view: 'settings',
    },
  },
  {
    patterns: ['决策模型', 'decision model', 'vegaml'],
    guide: {
      text: '「设置 → 服务总览」：决策模型是实验功能，出现此提示可忽略——检测会自动改用本地检查，不受影响。',
      view: 'settings',
    },
  },
  {
    patterns: ['特征模型', 'feature model', 'feature-model'],
    guide: {
      text: '「设置 → 快速定案（特征模型）」：检查服务地址并点「测试」；测评与贡献测量也依赖该服务。若暂不需要，可关闭该层，深度扫描将改由 AI 全文复查。',
      view: 'settings',
    },
  },
  {
    patterns: ['hanlp', 'nlp 服务'],
    guide: {
      text: '「设置 → 文本处理服务（HanLP）」：检查服务地址并点「测试」；若无法部署该服务，可开启「离线模式」（只用本地规则词表）。',
      view: 'settings',
    },
  },
  {
    patterns: ['嵌入', 'embedding'],
    guide: {
      text: '「设置 → 知识查询（向量检索）」：配置服务地址 / API Key 后点「测试」。未配置时知识查询自动跳过（不影响其他检测）。',
      view: 'settings',
    },
  },
  {
    patterns: ['llm', 'api key'],
    guide: {
      text: '「设置 → AI 复查（LLM）」：配置服务地址 / API Key / 模型后点「测试」。未配置时，标准 / 深度检查会回退本地检查，个性化讲解显示备选建议。',
      view: 'settings',
    },
  },
]

/** 按错误文本匹配修复指引；无匹配返回 null */
export function guideFor(message: string): ErrorGuide | null {
  const msg = message.toLowerCase()
  for (const rule of SETTINGS_GUIDES) {
    if (rule.patterns.some((kw) => msg.includes(kw))) return rule.guide
  }
  return null
}
