<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { LButton, LEmpty, LInput, LProgress, LSection, LTable, LTag } from '../ui'
import { toast } from '../ui/toast'
import { api } from '../api/client'
import { errMsg } from '../lib/err'
import { identity, mcpConfigJson, signOut, webShareUrl } from '../lib/auth'
import type { LColumn } from '../ui'

/**
 * 密钥分发（管理面，仅超管可见）：一张令牌表就是唯一身份原语——
 * 这里签发的评审密钥同时用于 web 会话与 MCP，吊销后在途会话立刻失效。
 * 明文只在签发那一次响应里出现（库里只存 SHA-256 摘要），所以签发面板必须当场复制。
 */
interface TokenRow {
  id: string
  label: string
  quota_chars: number
  revoked: boolean
  used_today: number
  used_retained: number
  days_used: number
  last_day: number | null
  /** 调用级日志（gw_call_log）口径，含被拒请求；老服务端无此字段 */
  calls_today?: number
  calls_retained?: number
}

interface Stats {
  tokens_total: number
  tokens_active: number
  active_sessions: number
  chars_today: number
  chars_retained: number
  calls_today?: number
  calls_retained?: number
  retention_days: number
}

const rows = ref<TokenRow[]>([])
const stats = ref<Stats | null>(null)
const loading = ref(false)
const error = ref('')
const label = ref('')
const quotaWan = ref(200) // 万字/日：评审日测够用的默认档
const issuing = ref(false)
/** 刚签发的一枚：{token} 只存在于这里，关掉就再也取不到 */
const fresh = ref<{ id: string; label: string; token: string } | null>(null)

/** 用户名账号（密钥的口令别名）：按 token_id 对到密钥行上 */
interface UserRow {
  username: string
  token_id: string
  must_change: boolean
  revoked: boolean
  created_at: number
}
const users = ref<UserRow[]>([])
/** 行内建账号草稿：正在填用户名的密钥行 id → 用户名 */
const accountDraft = ref<{ id: string; username: string } | null>(null)
const acctBusy = ref(false)
/** 代生成的临时口令：{password} 只存在于这里，关掉就取不回 */
const freshPw = ref<{ username: string; password: string } | null>(null)

const userByToken = computed(() => {
  const m = new Map<string, UserRow>()
  for (const u of users.value) m.set(u.token_id, u)
  return m
})

/**
 * 邀请码：一枚一次有效的注册邀请。对方用 `?invite=…` 链接打开，设用户名与密码
 * 即完成注册并登录；code 明文只在生成那一次响应里出现（同密钥明文的口径）。
 */
interface InviteRow {
  id: string
  label: string
  quota_chars: number
  created_at: number
  expires_at: number
  used_at: number | null
  used_by: string | null
}
const invites = ref<InviteRow[]>([])
const inviteLabel = ref('')
const inviteQuota = ref('')
const inviteHours = ref('72')
const inviting = ref(false)
/** 刚生成的邀请码：{code} 只存在于这里，关掉就取不回 */
const freshInvite = ref<{ label: string; code: string } | null>(null)
const inviteConfirmId = ref<string | null>(null)

const inviteColumns: LColumn<InviteRow>[] = [
  { key: 'label', label: '用途' },
  { key: 'state', label: '状态', width: '84px' },
  { key: 'quota', label: '配额' },
  { key: 'created', label: '创建' },
  { key: 'expires', label: '到期' },
  { key: 'used_by', label: '使用者' },
  { key: 'ops', label: '操作', width: '150px', align: 'right' },
]

/** 行内两步吊销确认：处于确认态的行 id（不落弹窗，按 ui 契约用行内确认） */
const confirmId = ref<string | null>(null)

const tokenColumns: LColumn<TokenRow>[] = [
  { key: 'label', label: '用途' },
  { key: 'state', label: '状态', width: '84px' },
  { key: 'user', label: '账号', width: '190px' },
  { key: 'usage', label: '今日用量' },
  { key: 'total', label: '累计 / 最近' },
  { key: 'calls', label: '调用次数' },
  { key: 'id', label: '密钥 id' },
  { key: 'ops', label: '操作', width: '150px', align: 'right' },
]

/** 字符数量级人话：万位起折算 */
function chars(n: number): string {
  if (!n) return '0'
  return n >= 10000 ? `${(n / 10000).toFixed(n >= 100000 ? 0 : 1)} 万字` : `${n} 字`
}

/** 日账的 day 是 UTC 日序（epoch 天数） */
function dayText(d: number | null): string {
  return d ? new Date(d * 86400000).toISOString().slice(0, 10) : '—'
}

/** 今日剩余配额：不限配额回 null */
function remainToday(row: TokenRow): number | null {
  return row.quota_chars > 0 ? Math.max(0, row.quota_chars - row.used_today) : null
}

/** 今日配额已用百分比（进度条；越线由服务端回 429，这里只到 100） */
function pct(row: TokenRow): number {
  if (row.quota_chars <= 0) return 0
  return Math.min(100, Math.round((row.used_today / row.quota_chars) * 100))
}

async function load() {
  loading.value = true
  error.value = ''
  confirmId.value = null
  try {
    rows.value = await api.adminTokens()
  } catch (e) {
    error.value = errMsg(e)
  } finally {
    loading.value = false
  }
  // 概览是第二个端点：老服务端没有它不该影响签发与列表，单独吞错
  try {
    stats.value = await api.adminStats()
  } catch {
    stats.value = null
  }
  // 用户名账号：同样对老服务端宽容（没有这个面就当没有账号）
  try {
    users.value = await api.adminUsers()
  } catch {
    users.value = []
  }
  await loadInvites()
}

async function loadInvites() {
  // 邀请码面同样对老服务端宽容（没有这个端点就当没生成过）
  try {
    invites.value = await api.adminListInvites()
  } catch {
    invites.value = []
  }
}

async function createInvite() {
  const l = inviteLabel.value.trim()
  const q = inviteQuota.value.trim() === '' ? undefined : Math.round(Number(inviteQuota.value))
  const h = inviteHours.value.trim() === '' ? undefined : Math.round(Number(inviteHours.value))
  inviting.value = true
  try {
    const r = await api.adminCreateInvite(
      l || undefined,
      q !== undefined && Number.isFinite(q) && q > 0 ? q : undefined,
      h !== undefined && Number.isFinite(h) && h > 0 ? h : undefined,
    )
    freshInvite.value = { label: l || '未命名邀请', code: r.code }
    inviteLabel.value = ''
    inviteQuota.value = ''
    inviteHours.value = '72'
    await loadInvites()
  } catch (e) {
    toast(`生成邀请码失败：${errMsg(e)}`, 'danger')
  } finally {
    inviting.value = false
  }
}

async function doInviteDelete(row: InviteRow) {
  inviteConfirmId.value = null
  try {
    await api.adminDeleteInvite(row.id)
    toast(`已撤销邀请「${row.label}」：此码不能再激活`, 'ok')
    await loadInvites()
  } catch (e) {
    toast(`撤销失败：${errMsg(e)}`, 'danger')
  }
}

/** 状态chip：用过=已用；过期未用=已过期；否则待用 */
function inviteState(inv: InviteRow): { label: string; tone: 'ok' | 'warn' | 'neutral' } {
  if (inv.used_at) return { label: '已用', tone: 'neutral' }
  if (inv.expires_at * 1000 <= Date.now()) return { label: '已过期', tone: 'warn' }
  return { label: '待用', tone: 'ok' }
}
function invitePending(inv: InviteRow): boolean {
  return inviteState(inv).label === '待用'
}
function inviteQuotaText(n: number): string {
  return n > 0 ? `${n} 字` : '不限'
}
/** unix 秒 → 日期文本（与密钥表的日账读法同粒度） */
function tsText(t: number | null): string {
  return t ? new Date(t * 1000).toISOString().slice(0, 10) : '—'
}
function inviteUrl(code: string): string {
  return `${window.location.origin}/?invite=${code}`
}

onMounted(load)

async function issue() {
  const l = label.value.trim()
  if (!l) {
    toast('先填写用途标签（谁在用这枚密钥）', 'warn')
    return
  }
  issuing.value = true
  try {
    const r = await api.adminIssueToken(l, Math.max(0, Math.round(quotaWan.value * 10000)))
    fresh.value = { id: r.id, label: l, token: r.token }
    label.value = ''
    await load()
  } catch (e) {
    toast(`签发失败：${errMsg(e)}`, 'danger')
  } finally {
    issuing.value = false
  }
}

/** 两步吊销：第一步进入行内确认态，第二步才真正调接口 */
function askRevoke(row: TokenRow) {
  confirmId.value = row.id
}
function cancelRevoke() {
  confirmId.value = null
}
async function doRevoke(row: TokenRow) {
  confirmId.value = null
  try {
    await api.adminRevokeToken(row.id)
    toast(`已吊销「${row.label}」：该密钥的 web 会话与 MCP 连接已失效`, 'ok')
    await load()
  } catch (e) {
    toast(`吊销失败：${errMsg(e)}`, 'danger')
  }
}

async function leave() {
  try {
    await signOut()
    toast('已退出登录', 'ok')
  } catch (e) {
    toast(`退出失败：${errMsg(e)}`, 'danger')
  }
}

/** 行内建账号：服务端生成临时口令（只回这一次），首次登录要求改密 */
async function createUser(row: TokenRow) {
  const uname = accountDraft.value?.id === row.id ? accountDraft.value.username.trim() : ''
  if (!uname) {
    toast('先填写用户名（小写字母/数字/._- 或邮箱）', 'warn')
    return
  }
  acctBusy.value = true
  try {
    const r = await api.adminCreateUser(row.id, uname)
    freshPw.value = r.password ? { username: r.username, password: r.password } : null
    accountDraft.value = null
    toast(`已为「${row.label}」创建账号 ${r.username}`, 'ok')
    await load()
  } catch (e) {
    toast(`创建账号失败：${errMsg(e)}`, 'danger')
  } finally {
    acctBusy.value = false
  }
}

async function resetUser(u: UserRow) {
  acctBusy.value = true
  try {
    const r = await api.adminResetUser(u.username)
    freshPw.value = r.password ? { username: u.username, password: r.password } : null
    toast(`已重置「${u.username}」的口令：旧口令与其会话作废`, 'ok')
    await load()
  } catch (e) {
    toast(`重置失败：${errMsg(e)}`, 'danger')
  } finally {
    acctBusy.value = false
  }
}

async function copy(text: string, what: string) {
  try {
    await navigator.clipboard.writeText(text)
    toast(`已复制${what}`, 'ok')
  } catch (e) {
    toast(`复制失败：${errMsg(e)}；可在页面里手动选中复制`, 'danger')
  }
}

const quotaText = (n: number) => (n > 0 ? `${(n / 10000).toFixed(0)} 万字/日` : '不限')

const quotaWanNum = computed({
  get: () => String(quotaWan.value),
  set: (v: string) => {
    const n = Number(v)
    if (v !== '' && Number.isFinite(n)) quotaWan.value = Math.max(0, Math.round(n))
  },
})

/** 概览拿不到时退回逐令牌求和（同一日账的两个读法，口径一致）；账窗默认与清理周期同值 */
const totalRetained = computed(() => rows.value.reduce((s, r) => s + r.used_retained, 0))
const windowDays = computed(() => stats.value?.retention_days ?? 7)

/** 调用次数：与「字」不同单位；无字段（老服务端）如实回「—」，不假装是 0 */
function calls(n?: number): string {
  if (n === undefined) return '—'
  return n >= 10000 ? `${(n / 10000).toFixed(n >= 100000 ? 0 : 1)} 万次` : `${n} 次`
}

const totalCalls = computed(() => {
  if (stats.value?.calls_retained !== undefined) return stats.value.calls_retained
  if (rows.value.every((r) => r.calls_retained === undefined)) return undefined
  return rows.value.reduce((s, r) => s + (r.calls_retained ?? 0), 0)
})

const overview = computed(() => {
  const active = rows.value.filter((r) => !r.revoked).length
  return `共 ${rows.value.length} 枚（${active} 枚有效）· 在途会话 ${
    stats.value ? stats.value.active_sessions : '—'
  } · 近 ${windowDays.value} 天已测 ${chars(stats.value ? stats.value.chars_retained : totalRetained.value)} · 调用 ${calls(totalCalls.value)}（含被拒）`
})

/**
 * 超级管理员密钥的来源（whoami 只对已验证的超管回这两个字段；桌面端 invoke 不带 = 整段不显示）。
 * 目的是让运维看清「这把钥匙归谁管」——env 持有的那把轮换端点不接管，只能改文件。
 */
const keyOrigin = computed(() => {
  const src = identity.value.admin_token_source
  if (!src) return null
  return src === 'env'
    ? {
        holder: 'LANGFIXER_ADMIN_TOKEN（进程环境 / .env）',
        howto: '；这把钥匙该端点会回 409 拒接，换法只有改 env 里的值再重启',
      }
    : { holder: 'gw_meta（安装时自动生成）', howto: '' }
})
</script>

<template>
  <LSection class="keys-card" title="密钥分发" :desc="overview">
    <template #actions>
      <LButton variant="ghost" size="sm" :loading="loading" @click="load">刷新</LButton>
    </template>

    <!-- 错误态：带下一步指引（重试），不落弹窗 -->
    <LEmpty v-if="error" text="密钥列表加载失败" :hint="error">
      <template #action>
        <LButton @click="load">重试</LButton>
      </template>
    </LEmpty>

    <div class="keys-issue">
      <LInput v-model="label" placeholder="用途标签，例如：评审-张老师" class="keys-label" />
      <input
        v-model="quotaWanNum"
        class="l-input keys-quota"
        type="number"
        min="0"
        step="50"
        aria-label="每日配额（万字）"
      />
      <span class="keys-unit">万字/日</span>
      <LButton variant="primary" :loading="issuing" @click="issue">签发</LButton>
    </div>
    <p class="keys-tip">
      配额按自然日累计字符数（web 与 MCP 同一本账），超限回 429；填 0 = 不限。<br />
      用量来自按天聚合的日账，只保留 {{ windowDays }} 天（与文本清理同周期），
      所以「累计」是<strong>保留期内</strong>合计，不是历史总量；要查某人测过什么看检测历史，同样 7 天。<br />
      密钥明文<strong>只在签发表单里出现一次</strong>——列表与后续接口都取不回来，忘了只能重发。
    </p>

    <p v-if="keyOrigin" class="keys-tip">
      本实例的超级管理员密钥由 <strong>{{ keyOrigin.holder }}</strong> 持有<template
        v-if="identity.admin_token_fp"
        > · 指纹 <code>{{ identity.admin_token_fp }}</code></template
      ><br />
      重启服务<strong>不会</strong>换钥匙。换钥匙是显式动作：<code>POST /admin/admin-token/rotate</code>
      带 <code>{"confirm":"ROTATE"}</code>（新明文只回一次，旧密钥与其会话即刻失效）{{
        keyOrigin.howto
      }}。
    </p>

    <div v-if="fresh" class="keys-fresh">
      <div class="keys-fresh-head">
        <span>新密钥 · {{ fresh.label }}</span>
        <code>{{ fresh.token }}</code>
      </div>
      <div class="keys-dist">
        <section>
          <h4>发给他人的 Web 直链</h4>
          <div class="keys-dist-row">
            <input class="l-input" :value="webShareUrl(fresh.token)" readonly />
            <LButton size="sm" @click="copy(webShareUrl(fresh.token), '直链')">复制</LButton>
          </div>
        </section>
        <section>
          <h4>MCP 配置</h4>
          <LButton size="sm" @click="copy(mcpConfigJson(fresh.token), 'MCP 配置')">
            复制配置 JSON
          </LButton>
        </section>
      </div>
      <div class="keys-fresh-warn">
        关掉这里就再也看不到这枚明文，请先复制发给对方。
      </div>
    </div>

    <!-- 代生成的临时口令：同样只出现这一次，首次登录必须改密 -->
    <div v-if="freshPw" class="keys-fresh">
      <div class="keys-fresh-head">
        <span>临时口令 · {{ freshPw.username }}</span>
        <code>{{ freshPw.password }}</code>
        <LButton size="sm" @click="copy(freshPw.password, '临时口令')">复制</LButton>
        <LButton size="sm" variant="ghost" @click="freshPw = null">知道了</LButton>
      </div>
      <div class="keys-fresh-warn">
        口令只显示这一次，请复制发给对方；{{ freshPw.username }} 首次登录后需自行改密。
      </div>
    </div>

    <LEmpty
      v-if="!error && !loading && rows.length === 0"
      text="还没有签发过密钥"
      hint="在上方填写用途标签与每日配额，点「签发」生成第一枚评审密钥。"
    />
    <LTable
      v-else-if="!error"
      :columns="tokenColumns"
      :rows="rows"
      row-key="id"
      compact
      class="keys-table"
    >
      <template #col-state="{ row }">
        <LTag :tone="row.revoked ? 'neutral' : 'ok'">{{ row.revoked ? '已吊销' : '有效' }}</LTag>
      </template>
      <template #col-user="{ row }">
        <!-- 账号是这枚密钥的口令别名：一行一个，未建可在行内直接建 -->
        <div v-if="userByToken.get(row.id)" class="keys-user">
          <code>{{ userByToken.get(row.id)!.username }}</code>
          <LTag v-if="userByToken.get(row.id)!.must_change" tone="warn">临时口令</LTag>
          <LButton
            variant="ghost"
            size="sm"
            :loading="acctBusy"
            :disabled="row.revoked"
            @click="resetUser(userByToken.get(row.id)!)"
          >
            重置口令
          </LButton>
        </div>
        <div v-else-if="accountDraft?.id === row.id" class="keys-user-form">
          <LInput
            v-model="accountDraft.username"
            placeholder="用户名"
            class="keys-user-input"
            @keydown.enter="createUser(row)"
          />
          <LButton size="sm" :loading="acctBusy" @click="createUser(row)">建账号</LButton>
          <LButton size="sm" variant="ghost" @click="accountDraft = null">取消</LButton>
        </div>
        <LButton
          v-else
          size="sm"
          variant="ghost"
          :disabled="row.revoked"
          @click="accountDraft = { id: row.id, username: '' }"
        >
          建账号
        </LButton>
      </template>
      <template #col-usage="{ row }">
        <div class="keys-usage">
          <span>{{ chars(row.used_today) }}</span>
          <span class="keys-usage-sub">{{ quotaText(row.quota_chars) }}</span>
        </div>
        <div v-if="row.quota_chars > 0 && !row.revoked" :class="{ 'bar-warn': pct(row) >= 90 }">
          <LProgress :value="pct(row) / 100" />
        </div>
        <div v-if="remainToday(row) !== null" class="keys-usage-sub">
          今日剩 {{ chars(remainToday(row) as number) }}
        </div>
      </template>
      <template #col-total="{ row }">
        <div>{{ chars(row.used_retained) }}</div>
        <div class="keys-usage-sub">{{ dayText(row.last_day) }}</div>
      </template>
      <template #col-calls="{ row }">
        <div>{{ calls(row.calls_today) }}</div>
        <div class="keys-usage-sub">累计 {{ calls(row.calls_retained) }}</div>
      </template>
      <template #col-id="{ row }"><code class="keys-id">{{ row.id }}</code></template>
      <template #col-ops="{ row }">
        <!-- 吊销 = 行内两步确认（弹窗配额只留给 发布 / 删资源包 / 回滚） -->
        <template v-if="confirmId === row.id">
          <span class="keys-confirm">会话与 MCP 立即失效</span>
          <LButton variant="danger" size="sm" @click="doRevoke(row)">确认吊销</LButton>
          <LButton size="sm" @click="cancelRevoke">取消</LButton>
        </template>
        <LButton
          v-else
          variant="ghost"
          size="sm"
          :disabled="row.revoked"
          @click="askRevoke(row)"
        >
          吊销
        </LButton>
      </template>
    </LTable>

    <!-- 邀请码：一次性注册邀请（激活入口 = 链接 ?invite=…） -->
    <section class="keys-invites">
      <h3>邀请码</h3>
      <p class="keys-tip">
        对方打开邀请链接（<code>…/?invite=…</code>）设置用户名与密码即完成注册并登录；
        一枚只用一次，到期自动作废，未用的可随时撤销。配额为总量字数（可选），有效期默认 72 小时。<br />
        邀请码明文<strong>只在生成那一次响应里出现</strong>，忘了只能作废重发。
      </p>
      <div class="keys-issue">
        <LInput v-model="inviteLabel" placeholder="用途（可不填），例如：评审-李老师" class="keys-label" />
        <input
          v-model="inviteQuota"
          class="l-input keys-quota"
          type="number"
          min="0"
          placeholder="配额字数"
          aria-label="配额字数（可选）"
        />
        <input
          v-model="inviteHours"
          class="l-input keys-quota"
          type="number"
          min="1"
          aria-label="有效小时数"
        />
        <span class="keys-unit">小时后失效</span>
        <LButton variant="primary" :loading="inviting" @click="createInvite">生成邀请码</LButton>
      </div>

      <div v-if="freshInvite" class="keys-fresh">
        <div class="keys-fresh-head">
          <span>邀请码 · {{ freshInvite.label }}</span>
          <code>{{ freshInvite.code }}</code>
          <LButton size="sm" @click="copy(inviteUrl(freshInvite.code), '邀请链接')">复制链接</LButton>
          <LButton size="sm" variant="ghost" @click="freshInvite = null">知道了</LButton>
        </div>
        <div class="keys-fresh-warn">邀请码只显示这一次，请复制发给对方；激活一次后即作废。</div>
      </div>

      <LEmpty
        v-if="!loading && invites.length === 0"
        text="还没有生成过邀请码"
        hint="在上方填用途（可不填）点「生成邀请码」，把带 ?invite= 的链接发给对方。"
      />
      <LTable
        v-else
        :columns="inviteColumns"
        :rows="invites"
        row-key="id"
        compact
        class="keys-table"
      >
        <template #col-state="{ row }">
          <LTag :tone="inviteState(row).tone">{{ inviteState(row).label }}</LTag>
        </template>
        <template #col-quota="{ row }">{{ inviteQuotaText(row.quota_chars) }}</template>
        <template #col-created="{ row }">{{ tsText(row.created_at) }}</template>
        <template #col-expires="{ row }">{{ tsText(row.expires_at) }}</template>
        <template #col-used_by="{ row }">{{ row.used_by ?? '—' }}</template>
        <template #col-ops="{ row }">
          <!-- 撤销同样走行内两步确认 -->
          <template v-if="inviteConfirmId === row.id">
            <span class="keys-confirm">此码立即失效</span>
            <LButton variant="danger" size="sm" @click="doInviteDelete(row)">确认撤销</LButton>
            <LButton size="sm" @click="inviteConfirmId = null">取消</LButton>
          </template>
          <LButton
            v-else-if="invitePending(row)"
            variant="ghost"
            size="sm"
            @click="inviteConfirmId = row.id"
          >
            撤销
          </LButton>
          <span v-else class="keys-usage-sub">—</span>
        </template>
      </LTable>
    </section>
  </LSection>
</template>

<style scoped>
.keys-issue {
  display: flex;
  gap: var(--space-2);
  align-items: center;
}
.keys-label {
  flex: 1;
  min-width: 0;
}
.keys-quota {
  width: calc(var(--space-7) * 2);
}
.keys-unit {
  font-size: var(--fs-sm);
  color: var(--muted);
}
.keys-tip {
  margin: var(--space-2) 0 0;
  font-size: var(--fs-sm);
  line-height: var(--lh-body);
  color: var(--muted);
}
.keys-tip code {
  background: var(--surface-2);
  border: 1px solid var(--border);
  border-radius: var(--radius-ctl);
  padding: 0 var(--space-1);
}
.keys-fresh {
  margin-top: var(--space-3);
  padding: var(--space-3);
  border: 1px solid var(--warn-line);
  border-radius: var(--radius-sm);
  background: var(--warn-soft);
  display: flex;
  flex-direction: column;
  gap: var(--space-2);
}
.keys-fresh-head {
  display: flex;
  gap: var(--space-2);
  align-items: center;
  font-size: var(--fs-ui);
  color: var(--text);
}
.keys-fresh-head code {
  font-size: var(--fs-sm);
  word-break: break-all;
}
.keys-dist {
  display: flex;
  gap: var(--space-4);
  flex-wrap: wrap;
}
.keys-dist section {
  display: flex;
  flex-direction: column;
  gap: var(--space-1);
  min-width: 260px;
}
.keys-dist h4 {
  margin: 0;
  font-size: var(--fs-sm);
  color: var(--text-2);
  font-weight: 600;
}
.keys-dist-row {
  display: flex;
  gap: var(--space-2);
  align-items: center;
}
.keys-dist-row > input {
  flex: 1;
  min-width: 0;
}
.keys-fresh-warn {
  font-size: var(--fs-sm);
  color: var(--warn-ink);
}
.keys-table {
  margin-top: var(--space-3);
}
.keys-id {
  font-size: var(--fs-sm);
  color: var(--muted);
}
.keys-user {
  display: flex;
  gap: var(--space-1);
  align-items: center;
  flex-wrap: wrap;
}
.keys-user code {
  font-size: var(--fs-sm);
}
.keys-user-form {
  display: flex;
  gap: var(--space-1);
  align-items: center;
}
.keys-user-input {
  width: 120px;
  min-width: 0;
}
.keys-usage {
  display: flex;
  gap: var(--space-1);
  align-items: baseline;
}
.keys-usage-sub {
  font-size: var(--fs-sm);
  color: var(--muted);
}
.keys-invites {
  display: flex;
  flex-direction: column;
  gap: var(--space-2);
  margin-top: var(--space-4);
  padding-top: var(--space-4);
  border-top: 1px solid var(--border);
}
.keys-invites h3 {
  margin: 0;
  font-size: var(--fs-md);
  color: var(--text);
}
.keys-confirm {
  font-size: var(--fs-xs);
  color: var(--muted);
  margin-right: var(--space-1);
}
/* 配额将满属「注意」而非「错误」：进度条转琥珀，不用红 */
.bar-warn :deep(.l-progress > i) {
  background: var(--warn);
}
</style>
