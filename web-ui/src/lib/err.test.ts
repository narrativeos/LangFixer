/**
 * err.ts 单元测试（vitest）
 */
import { describe, expect, it } from 'vitest'
import { errMsg } from './err'

describe('errMsg', () => {
  it('Error 对象取 message', () => {
    expect(errMsg(new Error('作业列表查询失败: database is locked'))).toBe(
      '作业列表查询失败: database is locked',
    )
  })

  it('Tauri invoke 的字符串 reject 原样返回', () => {
    expect(errMsg('作业列表查询失败: database is locked')).toBe(
      '作业列表查询失败: database is locked',
    )
  })

  it('普通对象 → JSON 字符串', () => {
    expect(errMsg({ code: 'E_BUSY', msg: 'x' })).toBe('{"code":"E_BUSY","msg":"x"}')
  })

  it('null / undefined → 未知错误', () => {
    expect(errMsg(null)).toBe('未知错误')
    expect(errMsg(undefined)).toBe('未知错误')
  })

  it('数字 / 布尔 → String', () => {
    expect(errMsg(42)).toBe('42')
    expect(errMsg(true)).toBe('true')
  })
})
