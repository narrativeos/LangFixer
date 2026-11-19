<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { api } from '../api/client'
import { LButton, LCheckbox, LInput, LSection } from '../ui'
import { toast } from '../ui/toast'
import AdminTokensPanel from './AdminTokensPanel.vue'
import { errMsg } from '../lib/err'
import {
  accessKey,
  identity,
  loginWithKey,
  mcpConfigJson,
  mcpEndpoint,
  signOut,
  webShareUrl,
} from '../lib/auth'

/**
 * 接入页：把当前密钥变成两种可直接发出去的形态——
 * 一、web 直链（打开即自动进入产品界面）；二、MCP 配置（AI 客户端直连 /mcp）。
 * 普通密钥用户只能拿到「自己这一枚」的配置；给别人签发密钥是管理面的事。
 *
 * 单机形态刚装完时界面不锁（零变化），这里就是「认领本机」的唯一入口：
 * 粘贴安装日志里的超管密钥验证一次，设置面从此要求登录态。
 */
const key = ref<string | null>(accessKey())
const showSecret = ref(false)
const keyInput = ref('')
const busy = ref(false)
onMounted(() => {
  key.value = accessKey()
})

const headerSub = computed(() => {
  const who = identity.value.authenticated
    ? identity.value.admin
      ? '超级管理员密钥'
      : '当前评审密钥'
    : '本机尚未认领'
  return `${who} · MCP 端点 ${mcpEndpoint()}`
})

/** 认领 / 登录：换到会话后重新取密钥（超管密钥可直接粘进 MCP 用） */
async function claim() {
  const t = keyInput.value.trim()
  if (!t) {
    toast('请粘贴超管密钥', 'warn')
    return
  }
  busy.value = true
  try {
    await loginWithKey(t)
    key.value = accessKey()
    keyInput.value = ''
    toast('已认领本机：设置面现在需要超管登录后才能修改', 'ok')
  } catch (e) {
    toast(`验证失败：${cleanError(e)}`, 'danger')
  } finally {
    busy.value = false
  }
}

/** 服务端错误体是 `{error:"unauthorized: …"}`：去掉状态码前缀只留人话 */
function cleanError(e: unknown): string {
  const raw =
    (e as { response?: { data?: { error?: string } } })?.response?.data?.error ?? errMsg(e)
  return raw.replace(/^(unauthorized|forbidden|bad request|not found):\s*/i, '') || '密钥无效'
}

const webUrl = computed(() => (key.value ? webShareUrl(key.value) : ''))
const mcpJson = computed(() => (key.value ? mcpConfigJson(key.value) : ''))

async function copy(text: string, what: string) {
  try {
    await navigator.clipboard.writeText(text)
    toast(`已复制${what}`, 'ok')
  } catch (e) {
    toast(`复制失败：${errMsg(e)}；可在页面里手动选中复制`, 'danger')
  }
}

/** 退出：服务端结束会话 + 丢弃本次密钥（回到门禁页） */
async function leave() {
  try {
    await signOut()
  } catch (e) {
    toast(`退出失败：${errMsg(e)}`, 'danger')
  }
}

/**
 * 改管理员密码：需已验证超管（会话或超管 Bearer 都算）。
 * 后端先落库再换内存，旧密码即时作废；新密码至少 8 位（与后端同口径）。
 */
const pwOld = ref('')
const pwNew = ref('')
const pwConfirm = ref('')
const pwBusy = ref(false)
async function changePw() {
  if (!pwOld.value || !pwNew.value) {
    toast('请填写当前密码与新密码', 'warn')
    return
  }
  if (pwNew.value.length < 8) {
    toast('新密码至少 8 位', 'warn')
    return
  }
  if (pwNew.value !== pwConfirm.value) {
    toast('两次输入的新密码不一致', 'warn')
    return
  }
  pwBusy.value = true
  try {
    await api.changePassword(pwOld.value, pwNew.value)
    pwOld.value = ''
    pwNew.value = ''
    pwConfirm.value = ''
    toast('密码已更新：旧密码立即作废，下次登录用新密码', 'ok')
  } catch (e) {
    toast(`改密失败：${cleanError(e)}`, 'danger')
  } finally {
    pwBusy.value = false
  }
}
</script>

<template>
  <div class="access">
    <LSection class="access-card" title="我的接入" :desc="headerSub">
      <template #actions>
        <LButton v-if="identity.authenticated" variant="ghost" size="sm" @click="leave">
          退出登录
        </LButton>
      </template>

      <!-- 单机形态装完即开：界面不锁，这里是唯一的「认领本机」入口（网关形态走门禁页） -->
      <template v-if="!identity.authenticated">
        <div class="banner">
          这台机器还没有人认领：检测功能照常可用，设置也照常可改。想让「设置」只由本机管理员修改，
          粘贴<strong>首次启动日志</strong>里那行 <code>lf_…</code> 超管密钥验证一次即可认领；
          认领之后改设置就要带着这把密钥登录，忘记只能改环境变量重置。
        </div>
        <div class="access-row">
          <LInput
            v-model="keyInput"
            placeholder="超管密钥 lf_…（首次启动日志）"
            :disabled="busy"
            @keyup.enter="claim"
          />
          <LButton variant="primary" :loading="busy" @click="claim">认领并锁定设置</LButton>
        </div>
      </template>

      <div v-else-if="!key" class="banner">
        本次是浏览器会话登录（服务端签发的小时级 cookie），地址栏与本地都不保存密钥，
        因此无法直接导出配置。需要 MCP 配置或直链时，退出后重新输入一次密钥即可。
      </div>

      <template v-else>
        <div v-if="identity.admin" class="banner banner-warn">
          这是<strong>超级管理员</strong>密钥：能改服务端配置、不进配额计量。
          自己用（含桌面 AI 客户端连本机）没问题，但<strong>不要发给他人</strong>——
          <template v-if="identity.gateway">给别人请到下方「密钥分发」签发评审密钥。</template>
          <template v-else>本机形态不签发他人密钥：要给多人评测请改用评测网关形态启动，再在这里分发。</template>
        </div>

        <section class="access-block">
          <h3>形态一 · Web 直链</h3>
          <p>打开即自动通过验证页，适合发给用浏览器的评审。</p>
          <div class="access-row">
            <input
              class="l-input access-secret"
              :value="webUrl"
              readonly
              :type="showSecret ? 'text' : 'password'"
            />
            <LButton @click="copy(webUrl, '直链')">复制</LButton>
          </div>
          <LCheckbox v-model="showSecret" label="显示密钥" />
        </section>

        <section class="access-block">
          <h3>形态二 · MCP 配置</h3>
          <p>
            贴进 AI 客户端的 <code>mcpServers</code>（Claude Desktop / Cline / mcp.json 同构），
            与 web 共用同一枚密钥与同一份配额。
          </p>
          <textarea
            class="l-input access-json"
            :value="mcpJson"
            rows="8"
            readonly
            style="height: auto; padding: var(--space-2); font-family: var(--font-mono); font-size: var(--fs-sm)"
          />
          <LButton @click="copy(mcpJson, 'MCP 配置')">复制配置</LButton>
        </section>
      </template>

      <!-- 改密：只对已验证超管且后端确实有密码登录时露出（桌面端不经 whoami，不展示） -->
      <section v-if="identity.authenticated && identity.admin && identity.password_login" class="access-block">
        <h3>修改管理员密码</h3>
        <p>
          用于 <code>admin</code> 账号的登录密码。改完旧密码立即作废，
          当前会话不受影响；忘记时可用 <code>LANGFIXER_ADMIN_PASSWORD</code> 环境变量重置。
        </p>
        <div class="pw-form">
          <LInput v-model="pwOld" type="password" placeholder="当前密码" :disabled="pwBusy" autocomplete="current-password" />
          <LInput v-model="pwNew" type="password" placeholder="新密码（至少 8 位）" :disabled="pwBusy" autocomplete="new-password" />
          <LInput v-model="pwConfirm" type="password" placeholder="确认新密码" :disabled="pwBusy" autocomplete="new-password" @keyup.enter="changePw" />
          <LButton variant="primary" :loading="pwBusy" @click="changePw">更新密码</LButton>
        </div>
      </section>
    </LSection>

    <AdminTokensPanel v-if="identity.gateway && identity.admin" />
  </div>
</template>

<style scoped>
.access {
  display: flex;
  flex-direction: column;
  gap: var(--space-3);
}
.banner {
  display: flex;
  align-items: flex-start;
  gap: var(--space-3);
  padding: var(--space-3) var(--space-4);
  margin-bottom: var(--space-3);
  font-size: var(--fs-ui);
  line-height: var(--lh-body);
  color: var(--text-2);
  background: var(--surface-2);
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
}
.banner code {
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: var(--radius-ctl);
  padding: 0 var(--space-1);
}
.banner-warn {
  color: var(--warn-ink);
  background: var(--warn-soft);
  border-color: var(--warn-line);
}
.access-block {
  display: flex;
  flex-direction: column;
  gap: var(--space-2);
  padding-top: var(--space-2);
}
.access-block + .access-block {
  margin-top: var(--space-3);
  border-top: 1px solid var(--border);
  padding-top: var(--space-4);
}
.access-block h3 {
  margin: 0;
  font-size: var(--fs-md);
  color: var(--text);
}
.access-block p {
  margin: 0;
  font-size: var(--fs-sm);
  color: var(--text-2);
  line-height: var(--lh-body);
}
.access-block code {
  background: var(--surface-2);
  border: 1px solid var(--border);
  border-radius: var(--radius-ctl);
  padding: 0 var(--space-1);
}
.access-row {
  display: flex;
  gap: var(--space-2);
  align-items: center;
}
.access-row > :first-child {
  flex: 1;
  min-width: 0;
}
.access-secret {
  font-family: var(--font-mono);
}
.pw-form {
  display: grid;
  grid-template-columns: 1fr 1fr 1fr auto;
  gap: var(--space-2);
  align-items: center;
}
@media (max-width: 720px) {
  .pw-form {
    grid-template-columns: 1fr;
  }
}
</style>
