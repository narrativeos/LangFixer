<script setup lang="ts">
/**
 * 门禁页：验证通过前产品界面不渲染（App.vue 用它替换整个界面）。
 * 两种凭证：单机登录门的内建 admin 账号密码（主），与管理员分发的
 * `?token=` 直链 / 访问密钥（评审令牌与超管密钥）；无注册、无找回密码。
 */
import { computed, ref } from 'vue'
import { LButton, LInput } from '../ui'
import { errMsg } from '../lib/err'
import {
  accessKey,
  clearAccessKey,
  identity,
  loginWithKey,
  loginWithPassword,
  mcpEndpoint,
} from '../lib/auth'

const keyInput = ref('')
const busy = ref(false)
const error = ref('')

/** 单机登录门默认走账号密码；网关形态维持令牌主路。两种凭证都能进，留切换入口 */
const mode = ref<'password' | 'token'>(
  identity.value.password_login && !identity.value.gateway ? 'password' : 'token',
)
const showPasswordForm = computed(
  () => mode.value === 'password' && !!identity.value.password_login,
)
const usernameInput = ref('admin')
const passwordInput = ref('')

const emit = defineEmits<{ (e: 'entered'): void }>()

/** 服务端错误体是 `{error:"unauthorized: …"}`：去掉状态码前缀，只留人话 */
function clean(e: unknown): string {
  const raw =
    (e as { response?: { data?: { error?: string } } })?.response?.data?.error ?? errMsg(e)
  return raw.replace(/^(unauthorized|forbidden|bad request|not found):\s*/i, '')
}

async function enter() {
  const t = keyInput.value.trim()
  if (!t) {
    error.value = '请输入访问密钥'
    return
  }
  busy.value = true
  error.value = ''
  try {
    await loginWithKey(t)
    keyInput.value = ''
    emit('entered')
  } catch (e) {
    error.value = clean(e) || '密钥无效或已吊销'
  } finally {
    busy.value = false
  }
}

async function enterAccount() {
  if (!passwordInput.value) {
    error.value = '请输入密码'
    return
  }
  busy.value = true
  error.value = ''
  try {
    await loginWithPassword(usernameInput.value.trim(), passwordInput.value)
    passwordInput.value = ''
    emit('entered')
  } catch (e) {
    error.value = clean(e) || '用户名或密码错误'
  } finally {
    busy.value = false
  }
}

/** 本标签页存过密钥却仍未通过（直链失效 / 被吊销）：给一个明确的清干净入口 */
const hadKey = ref(!!accessKey())
function reset() {
  clearAccessKey()
  hadKey.value = false
  error.value = ''
  keyInput.value = ''
}
</script>

<template>
  <div class="gate">
    <div class="gate-card">
      <div class="gate-brand">
        <span class="gate-mark">
          <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round">
            <path d="M5 12.5l4.5 4.5L19 7.5" />
          </svg>
        </span>
        <div>
          <h1>LangFixer</h1>
          <p>
            {{
              identity.gateway
                ? '此实例已开启访问验证'
                : identity.password_login
                  ? '请使用管理员账号登录'
                  : '本机设置已被超级管理员锁定'
            }}
          </p>
        </div>
      </div>

      <template v-if="!identity.authenticated">
        <template v-if="showPasswordForm">
          <label class="gate-label" for="gate-user">用户名</label>
          <LInput id="gate-user" v-model="usernameInput" autofocus :disabled="busy" placeholder="admin" />
          <label class="gate-label" for="gate-pass">密码</label>
          <LInput
            id="gate-pass"
            v-model="passwordInput"
            type="password"
            placeholder="管理员密码"
            :disabled="busy"
            @keyup.enter="enterAccount"
          />
          <LButton variant="primary" :loading="busy" class="gate-enter" @click="enterAccount">
            登录
          </LButton>
          <p v-if="error" class="gate-error" role="alert">{{ error }}</p>
          <LButton variant="ghost" size="sm" @click="mode = 'token'; error = ''">
            改用访问密钥
          </LButton>
          <ul class="gate-hint">
            <li>首次使用见部署文档的默认密码；登录后请尽快修改。</li>
            <li>收到的是 <code>?token=…</code> 链接？打开就会自动进入，不用手输。</li>
          </ul>
        </template>
        <template v-else>
          <label class="gate-label" for="gate-key">访问密钥</label>
          <LInput
            id="gate-key"
            v-model="keyInput"
            autofocus
            placeholder="lf_…（管理员分发）"
            :disabled="busy"
            @keyup.enter="enter"
          />
          <LButton variant="primary" :loading="busy" class="gate-enter" @click="enter">
            进入
          </LButton>
          <!-- 错误态：红色只用于这里；下一步指引就在文案里 -->
          <p v-if="error" class="gate-error" role="alert">
            {{ error }}——请确认密钥完整未过期，或直接使用管理员发来的 <code>?token=…</code> 直链。
          </p>
          <LButton v-if="hadKey" variant="ghost" size="sm" @click="reset">
            清除本标签页记住的密钥
          </LButton>
          <LButton v-if="identity.password_login" variant="ghost" size="sm" @click="mode = 'password'; error = ''">
            改用账号密码登录
          </LButton>
          <ul class="gate-hint">
            <li>无需注册：密钥由管理员签发，直接粘贴即可。</li>
            <li>收到的是 <code>?token=…</code> 链接？打开就会自动进入，不用手输。</li>
            <li>密钥只保存在本次标签页，关闭浏览器即失效。</li>
          </ul>
        </template>
      </template>

      <p class="gate-foot">
        <span>{{ showPasswordForm ? 'MCP 接入地址：' : '同一密钥可用于 MCP：' }}{{ mcpEndpoint() }}</span>
      </p>
    </div>
  </div>
</template>

<style scoped>
.gate {
  min-height: 100vh;
  display: grid;
  place-items: center;
  padding: var(--space-6);
  background: var(--bg);
}
.gate-card {
  width: min(420px, 100%);
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  box-shadow: var(--shadow-md);
  padding: var(--space-6);
  display: flex;
  flex-direction: column;
  gap: var(--space-3);
}
.gate-brand {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  margin-bottom: var(--space-1);
}
.gate-mark {
  width: var(--space-7);
  height: var(--space-7);
  display: grid;
  place-items: center;
  border-radius: var(--radius-sm);
  background: var(--primary-soft);
  color: var(--primary);
}
.gate-brand h1 {
  margin: 0;
  font-size: var(--fs-lg);
  color: var(--text);
}
.gate-brand p {
  margin: var(--space-1) 0 0;
  font-size: var(--fs-sm);
  color: var(--text-2);
}
.gate-label {
  font-size: var(--fs-sm);
  color: var(--text-2);
}
.gate-enter {
  width: 100%;
}
.gate-error {
  margin: 0;
  font-size: var(--fs-sm);
  line-height: var(--lh-body);
  color: var(--danger);
}
.gate-error code,
.gate-hint code {
  background: var(--surface-2);
  border: 1px solid var(--border);
  border-radius: var(--radius-ctl);
  padding: 0 var(--space-1);
}
.gate-hint {
  margin: var(--space-1) 0 0;
  padding-left: var(--space-5);
  font-size: var(--fs-sm);
  line-height: var(--lh-body);
  color: var(--muted);
}
.gate-foot {
  margin: var(--space-1) 0 0;
  display: flex;
  gap: var(--space-2);
  align-items: center;
  font-size: var(--fs-sm);
  color: var(--muted);
}
.gate-foot a {
  color: var(--primary);
  text-decoration: none;
}
</style>
