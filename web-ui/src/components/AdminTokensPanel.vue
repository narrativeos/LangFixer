<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { ElMessage, ElMessageBox } from 'element-plus'
import { api } from '../api/client'
import { errMsg } from '../lib/err'
import { identity, mcpConfigJson, signOut, webShareUrl } from '../lib/auth'

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
}

interface Stats {
  tokens_total: number
  tokens_active: number
  active_sessions: number
  chars_today: number
  chars_retained: number
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
}
onMounted(load)

async function issue() {
  const l = label.value.trim()
  if (!l) {
    ElMessage.warning('先填写用途标签（谁在用这枚密钥）')
    return
  }
  issuing.value = true
  try {
    const r = await api.adminIssueToken(l, Math.max(0, Math.round(quotaWan.value * 10000)))
    fresh.value = { id: r.id, label: l, token: r.token }
    label.value = ''
    await load()
  } catch (e) {
    ElMessage.error(`签发失败：${errMsg(e)}`)
  } finally {
    issuing.value = false
  }
}

async function revoke(row: TokenRow) {
  try {
    await ElMessageBox.confirm(
      `吊销「${row.label}」？该密钥的 web 会话与 MCP 连接会立刻失效。`,
      '吊销密钥',
      { type: 'warning', confirmButtonText: '吊销', cancelButtonText: '取消' },
    )
  } catch {
    return
  }
  try {
    await api.adminRevokeToken(row.id)
    ElMessage.success('已吊销')
    await load()
  } catch (e) {
    ElMessage.error(`吊销失败：${errMsg(e)}`)
  }
}

async function leave() {
  try {
    await signOut()
    ElMessage.success('已退出登录')
  } catch (e) {
    ElMessage.error(`退出失败：${errMsg(e)}`)
  }
}

async function copy(text: string, what: string) {
  try {
    await navigator.clipboard.writeText(text)
    ElMessage.success(`已复制${what}`)
  } catch (e) {
    ElMessage.error(`复制失败：${errMsg(e)}`)
  }
}

const quotaText = (n: number) => (n > 0 ? `${(n / 10000).toFixed(0)} 万字/日` : '不限')

/** 概览拿不到时退回逐令牌求和（同一日账的两个读法，口径一致）；账窗默认与清理周期同值 */
const totalRetained = computed(() => rows.value.reduce((s, r) => s + r.used_retained, 0))
const windowDays = computed(() => stats.value?.retention_days ?? 7)

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
  <el-card shadow="never" class="keys-card">
    <template #header>
      <div class="card-header">
        <span class="card-title">密钥分发</span>
        <span class="card-sub">
          共 {{ rows.length }} 枚（{{
            rows.filter((r) => !r.revoked).length
          }}
          枚有效）· 在途会话 {{ stats ? stats.active_sessions : '—' }} · 近 {{ windowDays }}
          天已测 {{ chars(stats ? stats.chars_retained : totalRetained) }}
        </span>
        <span class="spacer"></span>
        <el-button size="small" text :loading="loading" @click="load">刷新</el-button>
      </div>
    </template>

    <el-alert v-if="error" type="error" :closable="false" show-icon class="keys-note">
      {{ error }}
    </el-alert>

    <div class="keys-issue">
      <el-input v-model="label" size="large" placeholder="用途标签，例如：评审-张老师" />
      <el-input-number v-model="quotaWan" :min="0" :step="50" size="large" controls-position="right">
        <template #suffix><span class="keys-unit">万字/日</span></template>
      </el-input-number>
      <el-button type="primary" size="large" :loading="issuing" @click="issue">签发</el-button>
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
          <el-input :model-value="webShareUrl(fresh.token)" size="small" readonly>
            <template #append>
              <el-button @click="copy(webShareUrl(fresh.token), '直链')">复制</el-button>
            </template>
          </el-input>
        </section>
        <section>
          <h4>MCP 配置</h4>
          <el-button size="small" @click="copy(mcpConfigJson(fresh.token), 'MCP 配置')">
            复制配置 JSON
          </el-button>
        </section>
      </div>
      <el-alert type="warning" :closable="false" show-icon>
        关掉这里就再也看不到这枚明文，请先复制发给对方。
      </el-alert>
    </div>

    <el-table :data="rows" size="small" class="keys-table">
      <el-table-column prop="label" label="用途" min-width="150" />
      <el-table-column label="状态" width="90">
        <template #default="{ row }">
          <el-tag :type="row.revoked ? 'info' : 'success'" size="small">
            {{ row.revoked ? '已吊销' : '有效' }}
          </el-tag>
        </template>
      </el-table-column>
      <el-table-column label="今日用量" min-width="170">
        <template #default="{ row }">
          <div class="keys-usage">
            <span>{{ chars(row.used_today) }}</span>
            <span class="keys-usage-sub">{{ quotaText(row.quota_chars) }}</span>
          </div>
          <el-progress
            v-if="row.quota_chars > 0 && !row.revoked"
            :percentage="pct(row)"
            :stroke-width="4"
            :show-text="false"
            :status="pct(row) >= 90 ? 'exception' : undefined"
          />
          <div v-if="remainToday(row as TokenRow) !== null" class="keys-usage-sub">
            今日剩 {{ chars(remainToday(row as TokenRow) as number) }}
          </div>
        </template>
      </el-table-column>
      <el-table-column label="累计 / 最近" min-width="140">
        <template #default="{ row }">
          <div>{{ chars(row.used_retained) }}</div>
          <div class="keys-usage-sub">{{ dayText(row.last_day) }}</div>
        </template>
      </el-table-column>
      <el-table-column prop="id" label="密钥 id" min-width="180">
        <template #default="{ row }"><code class="keys-id">{{ row.id }}</code></template>
      </el-table-column>
      <el-table-column label="操作" width="90" align="right">
        <template #default="{ row }">
          <el-button
            size="small"
            text
            type="danger"
            :disabled="row.revoked"
            @click="revoke(row as TokenRow)"
          >
            吊销
          </el-button>
        </template>
      </el-table-column>
    </el-table>
  </el-card>
</template>

<style scoped>
.card-header {
  display: flex;
  align-items: center;
  gap: 8px;
}
.spacer {
  flex: 1;
}
.card-title {
  font-weight: 600;
  color: var(--text);
}
.card-sub {
  font-size: 12px;
  color: var(--muted);
}
.keys-note {
  margin-bottom: 10px;
}
.keys-issue {
  display: flex;
  gap: 8px;
  align-items: center;
}
.keys-issue .el-input {
  flex: 1;
}
.keys-unit {
  font-size: 12px;
  color: var(--muted);
}
.keys-tip {
  margin: 8px 0 0;
  font-size: 12px;
  line-height: 1.7;
  color: var(--muted);
}
.keys-fresh {
  margin-top: 12px;
  padding: 12px;
  border: 1px solid var(--warn-line);
  border-radius: var(--radius-sm);
  background: var(--warn-soft);
  display: flex;
  flex-direction: column;
  gap: 10px;
}
.keys-fresh-head {
  display: flex;
  gap: 8px;
  align-items: center;
  font-size: 13px;
  color: var(--text);
}
.keys-fresh-head code {
  font-size: 12px;
  word-break: break-all;
}
.keys-dist {
  display: flex;
  gap: 16px;
  flex-wrap: wrap;
}
.keys-dist section {
  display: flex;
  flex-direction: column;
  gap: 6px;
  min-width: 260px;
}
.keys-dist h4 {
  margin: 0;
  font-size: 12px;
  color: var(--text-2);
  font-weight: 600;
}
.keys-table {
  margin-top: 14px;
}
.keys-id {
  font-size: 12px;
  color: var(--muted);
}
.keys-usage {
  display: flex;
  gap: 6px;
  align-items: baseline;
}
.keys-usage-sub {
  font-size: 12px;
  color: var(--muted);
}
</style>
