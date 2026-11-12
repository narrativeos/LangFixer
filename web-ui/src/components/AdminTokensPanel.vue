<script setup lang="ts">
import { onMounted, ref } from 'vue'
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
}

const rows = ref<TokenRow[]>([])
const loading = ref(false)
const error = ref('')
const label = ref('')
const quotaWan = ref(200) // 万字/日：评审日测够用的默认档
const issuing = ref(false)
/** 刚签发的一枚：{token} 只存在于这里，关掉就再也取不到 */
const fresh = ref<{ id: string; label: string; token: string } | null>(null)

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
</script>

<template>
  <el-card shadow="never" class="keys-card">
    <template #header>
      <div class="card-header">
        <span class="card-title">密钥分发</span>
        <span class="card-sub">
          {{ identity.token_id ?? '超管会话' }} · 共 {{ rows.length }} 枚（{{
            rows.filter((r) => !r.revoked).length
          }}
          枚有效）
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
      配额按自然日累计字符数，超限回 429；填 0 = 不限。<br />
      密钥明文<strong>只在签发表单里出现一次</strong>——列表与后续接口都取不回来，忘了只能重发。
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
      <el-table-column label="日配额" width="110">
        <template #default="{ row }">{{ quotaText(row.quota_chars) }}</template>
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
</style>
