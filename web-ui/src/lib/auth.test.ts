import { describe, expect, it } from 'vitest'
import { mergeIdentity, type Whoami } from './auth'

/**
 * 身份视图合成：这个对象此前是**逐字段手写字面量**重建的，于是后端每加一个可选字段
 * 都会在「登录后」那条路径上被静默丢掉（实测 `?token=` 直链登录后看不到密钥来源与指纹）。
 */
const anonymous: Whoami = {
  gateway: true,
  admin: false,
  token_id: null,
  authenticated: false,
  locked: true,
}

describe('mergeIdentity', () => {
  it('登录后保留密钥来源与指纹（手写字面量时丢的就是它俩）', () => {
    const me: Whoami = {
      ...anonymous,
      admin: true,
      authenticated: true,
      admin_token_source: 'env',
      admin_token_fp: '68f0f109',
    }
    expect(mergeIdentity(anonymous, me, { admin: true, token_id: null })).toEqual({
      gateway: true,
      admin: true,
      token_id: null,
      authenticated: true,
      locked: true,
      // 用户名账号视图：本次 whoami 未带 username → 按契约回 null（非旧字段丢失）
      username: null,
      admin_token_source: 'env',
      admin_token_fp: '68f0f109',
    })
  })

  it('换实例时不得留上一台的指纹：本次 whoami 没带就是没有', () => {
    const prev: Whoami = {
      ...anonymous,
      admin: true,
      authenticated: true,
      admin_token_source: 'generated',
      admin_token_fp: 'ffffffff',
    }
    const next = mergeIdentity(prev, { ...anonymous, locked: false })
    expect(next.admin_token_source).toBeUndefined()
    expect(next.admin_token_fp).toBeNull()
    expect(next.locked).toBe(false)
  })

  it('老服务端 whoami 失败（me = null）时登录仍算已验证，也不带密钥信息', () => {
    const next = mergeIdentity(anonymous, null, { admin: false, token_id: 'tok_9' })
    expect(next.authenticated).toBe(true)
    expect(next.admin).toBe(false)
    expect(next.token_id).toBe('tok_9')
    expect(next.gateway).toBe(true) // 回退到上一份快照
    expect(next.admin_token_fp).toBeNull()
  })

  it('未登录探测：身份字段以 whoami 为准', () => {
    const next = mergeIdentity(anonymous, { ...anonymous, gateway: false, locked: false })
    expect(next.gateway).toBe(false)
    expect(next.authenticated).toBe(false)
  })
})
