<script setup lang="ts">
/**
 * 管理员账号初始化表单（首次启动）：用 env 里那把 `lf_…` 超管密钥激活一次，
 * 同时设置 admin 密码，成功即拿到 admin 会话。这是 whoami 回
 * `admin_initialized === false` 时取代普通登录的入口（TokenGate 门禁页与
 * AccessPanel 认领位共用；说明文案由父组件按场景给出）。
 *
 * - 成功：emit('done')，身份已在 initAdminAccount 里刷新，界面自行解锁；
 * - 409 already_initialized：重取 whoami 后 emit('already')，由父组件回退到
 *   普通登录 UI 并给提示；
 * - 429：尝试过于频繁，就地提示。
 */
import { ref } from 'vue'
import { LButton, LInput } from '../ui'
import { errMsg } from '../lib/err'
import { initAdminAccount, refreshIdentity } from '../lib/auth'

withDefaults(defineProps<{ layout?: 'row' | 'stack' }>(), { layout: 'row' })
const emit = defineEmits<{ (e: 'done'): void; (e: 'already'): void }>()

const username = ref('admin')
const password = ref('')
const key = ref('')
const busy = ref(false)
const error = ref('')

/** 服务端错误体是 `{error:"…"}`：去掉状态码前缀，只留人话 */
function clean(e: unknown): string {
  const raw =
    (e as { response?: { data?: { error?: string } } })?.response?.data?.error ?? errMsg(e)
  return raw.replace(/^(unauthorized|forbidden|bad request|not found|conflict):\s*/i, '')
}

async function submit() {
  if (!key.value.trim()) {
    error.value = '请填写激活密钥（首次启动日志 / env 里的 lf_…）'
    return
  }
  if (Array.from(password.value).length < 8) {
    error.value = '密码至少 8 位'
    return
  }
  busy.value = true
  error.value = ''
  try {
    await initAdminAccount(username.value.trim(), password.value, key.value.trim())
    emit('done')
  } catch (e) {
    const resp = (e as { response?: { status?: number; data?: { error?: string } } })?.response
    if (resp?.status === 409 && resp.data?.error === 'already_initialized') {
      await refreshIdentity()
      emit('already')
    } else if (resp?.status === 429) {
      error.value = '尝试过于频繁，请稍后再试'
    } else {
      error.value = clean(e) || '初始化失败'
    }
  } finally {
    busy.value = false
  }
}
</script>

<template>
  <form class="init-form" :class="$props.layout === 'stack' ? 'init-form-stack' : ''" @submit.prevent="submit">
    <LInput
      v-model="username"
      :disabled="busy"
      placeholder="用户名"
      title="首个管理员须为 admin"
      autocomplete="username"
    />
    <LInput
      v-model="password"
      type="password"
      :disabled="busy"
      placeholder="至少 8 位"
      autocomplete="new-password"
      @keyup.enter="submit"
    />
    <LInput
      v-model="key"
      :disabled="busy"
      placeholder="超管密钥 lf_…（首次启动日志 / env 可查）"
      @keyup.enter="submit"
    />
    <LButton variant="primary" :loading="busy" @click="submit">初始化并认领</LButton>
    <p class="init-hint">首个管理员须为 <code>admin</code>；激活之后改设置用 admin + 密码登录。</p>
    <p v-if="error" class="init-error" role="alert">{{ error }}</p>
  </form>
</template>

<style scoped>
.init-form {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-2);
  align-items: center;
}
.init-form > :first-child {
  flex: 0 1 180px;
  min-width: 0;
}
.init-form > :nth-child(2),
.init-form > :nth-child(3) {
  flex: 1 1 200px;
  min-width: 0;
}
.init-form-stack {
  flex-wrap: nowrap;
  flex-direction: column;
  align-items: stretch;
}
.init-form-stack > * {
  flex: none;
  min-width: 0;
}
.init-hint {
  flex-basis: 100%;
  margin: 0;
  font-size: var(--fs-sm);
  line-height: var(--lh-body);
  color: var(--muted);
}
.init-hint code {
  background: var(--surface-2);
  border: 1px solid var(--border);
  border-radius: var(--radius-ctl);
  padding: 0 var(--space-1);
}
.init-error {
  flex-basis: 100%;
  margin: 0;
  font-size: var(--fs-sm);
  line-height: var(--lh-body);
  color: var(--danger);
}
</style>
