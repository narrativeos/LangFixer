import { describe, expect, it } from 'vitest'
import { guideFor } from './guide'

describe('guideFor（常见错误 → 修复指引）', () => {
  it('决策模型类错误 → 指向「设置 → 决策模型」', () => {
    const g = guideFor('决策模型不可达：connect ECONNREFUSED 127.0.0.1:8001')
    expect(g?.view).toBe('settings')
    expect(g?.text).toContain('决策模型')
  })

  it('feature-model 服务错误（L3 / 测评共用）→ 指向「设置 → L3 特征模型」', () => {
    const g = guideFor('feature-model 不可达：timeout')
    expect(g?.view).toBe('settings')
    expect(g?.text).toContain('L3 特征模型')
    const g2 = guideFor('特征模型服务返回 500')
    expect(g2?.view).toBe('settings')
  })

  it('LLM 类错误 → 指向「设置 → LLM 参数」', () => {
    const g = guideFor('LLM 调用失败：401 invalid api key')
    expect(g?.view).toBe('settings')
    expect(g?.text).toContain('LLM 参数')
  })

  it('HanLP / NLP 错误 → 指向「设置 → NLP 服务」并提示离线模式', () => {
    const g = guideFor('HanLP 服务不可达')
    expect(g?.view).toBe('settings')
    expect(g?.text).toContain('离线模式')
  })

  it('嵌入引擎错误 → 指向「设置 → 嵌入引擎」', () => {
    const g = guideFor('嵌入引擎 embedding 未响应')
    expect(g?.view).toBe('settings')
    expect(g?.text).toContain('嵌入引擎')
  })

  it('关键字大小写不敏感', () => {
    expect(guideFor('DECISION model down')?.view).toBe('settings')
    expect(guideFor('LLM timeout')?.view).toBe('settings')
  })

  it('具体短语优先：「决策表」错误指向数据目录，而非决策模型', () => {
    const g = guideFor('决策表加载失败：目录不存在')
    expect(g?.text).toContain('数据目录')
  })

  it('无关错误 → null（调用方只显示原始错误）', () => {
    expect(guideFor('数据库连接池耗尽')).toBeNull()
    expect(guideFor('')).toBeNull()
  })
})
