/**
 * 验证机制前端态：token 门禁 + 当前身份 + 两种分发形态的生成
 *
 * - 门禁：`GET /auth/whoami` 回 `authenticated=false` 时产品界面不渲染（先过 TokenGate）；
 * - 密钥来源只有两处：地址栏 `?token=`（管理员分发的直链）与门禁页输入，
 *   两者都只进 sessionStorage（关标签页即失效，不落 localStorage）；
 * - 桌面端（Tauri）invoke 不走 HTTP，天然无门禁：恒视为已验证超管。
 */
import { ref } from 'vue'
import { api, isTauri, type Whoami } from '../api/client'

export type { Whoami } from '../api/client'

/** 本次会话使用的密钥（超管或评审）：仅 sessionStorage，供「我的接入」生成 MCP 配置 */
const ACCESS_KEY = 'langfixer.access.key'

export const identity = ref<Whoami>({
  gateway: false,
  admin: true,
  token_id: null,
  authenticated: true,
  locked: false,
})
/** 身份探测完成（完成前界面留白，避免产品界面闪一下） */
export const gateReady = ref(false)
/** 当前身份的人类可读标签（登录响应里的 label；超管为「管理员」） */
export const identityLabel = ref('')

/**
 * 把一次 `whoami` 快照（和可选的登录响应）合成身份视图。
 *
 * 存在的理由：这个视图靠逐字段手写字面量重建过两次，**新加的可选字段必然被丢掉**
 * （实测：`?token=` 直链登录后「密钥分发」上看不到密钥来源与指纹，而直接
 * `fetch('/auth/whoami')` 有）。统一走这里，字段只在一处列举。
 *
 * - `login` 给了 = 刚刚用这枚密钥换到会话：`admin/token_id` 以登录响应为准，
 *   `authenticated` 恒 true（老服务端 whoami 失败也不影响「已登录」这个事实）；
 * - 密钥来源与指纹**只取本次 `whoami`**：换了一台实例时不得留着上一台那把的指纹。
 */
export function mergeIdentity(
  prev: Whoami,
  me: Whoami | null,
  login?: { admin: boolean; token_id: string | null },
): Whoami {
  return {
    gateway: me?.gateway ?? prev.gateway,
    admin: login ? login.admin : (me?.admin ?? prev.admin),
    token_id: login ? login.token_id : (me?.token_id ?? prev.token_id),
    authenticated: login ? true : (me?.authenticated ?? prev.authenticated),
    locked: me?.locked ?? prev.locked,
    password_login: me?.password_login ?? prev.password_login,
    admin_token_source: me?.admin_token_source,
    admin_token_fp: me?.admin_token_fp ?? null,
  }
}

/** 会话在途失效（TTL / 重启）时 client.ts 的 401 拦截器广播此事件，把界面拉回门禁页 */
const LOCKED_EVENT = 'langfixer:locked'
let lockedArmed = false

/** 挂 401→回门禁 的监听（幂等；initGate 与登录路径都调） */
function armLockedListener(): void {
  if (lockedArmed || typeof window === 'undefined') return
  lockedArmed = true
  window.addEventListener(LOCKED_EVENT, () => {
    identity.value = { ...identity.value, authenticated: false }
    gateReady.value = true
  })
}

/** 用户名密码登录（单机形态登录门的内建 admin 账号）：会话走 httpOnly cookie，本地不留凭证 */
export async function loginWithPassword(username: string, password: string): Promise<void> {
  const r = await api.loginPassword(username, password)
  armLockedListener()
  identityLabel.value = r.label
  // 与 loginWithKey 同理：登录改变 locked 语义，补一次 whoami 取权威快照
  let me: Whoami | null = null
  try {
    me = await api.whoami()
  } catch {
    /* 老服务端：按已登录保守处理 */
  }
  identity.value = mergeIdentity(identity.value, me, {
    admin: r.admin,
    token_id: r.token_id,
  })
}

export function accessKey(): string | null {
  try {
    return sessionStorage.getItem(ACCESS_KEY)
  } catch {
    return null
  }
}

export function setAccessKey(key: string): void {
  try {
    sessionStorage.setItem(ACCESS_KEY, key)
  } catch {
    /* 隐私模式：会话内内存态仍可用，刷新需重新输入 */
  }
}

export function clearAccessKey(): void {
  try {
    sessionStorage.removeItem(ACCESS_KEY)
  } catch {
    /* 忽略 */
  }
}

/** MCP 端点（与 web 同一地址同源，路径 /mcp） */
export function mcpEndpoint(): string {
  return `${window.location.origin}/mcp`
}

/** 分发形态一：web 直链（打开即自动登录） */
export function webShareUrl(key: string): string {
  return `${window.location.origin}/?token=${encodeURIComponent(key)}`
}

/** 分发形态二：MCP 配置 JSON（Claude Desktop / Cline 等 mcpServers 约定） */
export function mcpConfigJson(key: string): string {
  return JSON.stringify(
    {
      mcpServers: {
        langfixer: { url: mcpEndpoint(), headers: { Authorization: `Bearer ${key}` } },
      },
    },
    null,
    2,
  )
}

/** 用一枚密钥换会话（门禁页与 ?token= 直链共用；失败抛出服务端消息） */
export async function loginWithKey(key: string): Promise<void> {
  const r = await api.login(key)
  setAccessKey(key)
  identityLabel.value = r.label
  // 登录本身会改变「是否已锁定」（单机形态首次登录即认领），所以补一次 whoami
  // 取权威 gateway/locked，不能用登录前那份快照——否则退出后界面还会敞着
  let me: Whoami | null = null
  try {
    me = await api.whoami()
  } catch {
    /* 老版本服务端：按「已锁定」保守处理 */
  }
  identity.value = mergeIdentity(identity.value, me, {
    admin: r.admin,
    token_id: r.token_id,
  })
}

/**
 * 启动探测顺序：地址栏 ?token= 直链自动登录 →（剥掉 token 参数）→ 已有会话 cookie →
 * sessionStorage 里的密钥补登录 → 都不行则停在门禁页。
 */
export async function initGate(): Promise<void> {
  if (isTauri) {
    identity.value = {
      gateway: false,
      admin: true,
      token_id: null,
      authenticated: true,
      locked: false,
    }
    identityLabel.value = '本机'
    gateReady.value = true
    return
  }
  let urlKey: string | null = null
  try {
    const p = new URLSearchParams(window.location.search)
    urlKey = p.get('token')
    if (urlKey) {
      p.delete('token')
      const url = new URL(window.location.href)
      url.search = p.toString()
      window.history.replaceState(null, '', url.toString())
    }
  } catch {
    /* 非浏览器环境 */
  }
  let me: Whoami = { gateway: false, admin: false, token_id: null, authenticated: false, locked: true }
  try {
    me = await api.whoami()
  } catch {
    /* 老版本服务端 whoami 需鉴权：按「已锁定未登录」处理，门禁页照常出现 */
  }
  identity.value = me
  armLockedListener()
  const key = urlKey ?? (me.authenticated ? null : accessKey())
  if (key) {
    try {
      await loginWithKey(key)
    } catch {
      clearAccessKey() // 直链 / 缓存密钥已失效：停在门禁页让用户重填
    }
  }
  gateReady.value = true
}

/** 退出登录：服务端结束会话 + 丢弃本地密钥 */
export async function signOut(): Promise<void> {
  try {
    await api.logout()
  } finally {
    clearAccessKey()
    // 密钥来源与指纹一并清掉：退出后就不再是「已验证超管」，留着上一把的指纹会误导
    identity.value = {
      ...identity.value,
      authenticated: false,
      admin: false,
      token_id: null,
      admin_token_source: undefined,
      admin_token_fp: null,
    }
    identityLabel.value = ''
  }
}
