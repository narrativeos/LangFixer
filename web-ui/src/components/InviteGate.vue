<script setup lang="ts">
/**
 * 邀请激活页：`?invite=…` 直达（initGate 已把 code 存进 inviteCode 并剥掉地址栏参数）。
 * 一枚邀请码只认一次：激活即建号并登录。App.vue 让它优先于普通界面展示，
 * 激活成功（emit activated，身份已刷新为 fresh 账号）或点「返回」（emit dismissed）
 * 后回到正常流程。
 */
import { ref } from 'vue'
import { api } from '../api/client'
import { LButton, LInput } from '../ui'
import { errMsg } from '../lib/err'
import { clearInvite, identity, inviteCode, mergeIdentity, type Whoami } from '../lib/auth'

const emit = defineEmits<{ (e: 'activated'): void; (e: 'dismissed'): void }>()

const username = ref('')
const password = ref('')
const busy = ref(false)
const error = ref('')
/** 邀请码本身已失效（invalid / expired / used）：表单不再可提交，只留返回 */
const dead = ref(false)

const DEAD_TEXT: Record<string, string> = {
  invite_expired: '这枚邀请码已过期。',
  invite_used: '这枚邀请码已被使用。',
  invite_invalid: '这枚邀请码无效或已被撤销。',
}

/** 与后端 normalize_username 同一规则（小写化后校验，admin 为保留名） */
const USER_RE = /^[a-z0-9._-]{2,32}$/

async function activate() {
  const uname = username.value.trim()
  if (!uname) {
    error.value = '请填写用户名（2–32 位小写字母/数字/._-）'
    return
  }
  const lower = uname.toLowerCase()
  if (!USER_RE.test(lower) || lower === 'admin') {
    error.value = uname.includes('@')
      ? '用户名不能是邮箱：去掉 @ 和域名部分，或换一个普通用户名'
      : '用户名需为 2–32 位小写字母/数字/._-'
    return
  }
  if (Array.from(password.value).length < 8) {
    error.value = '密码至少 8 位'
    return
  }
  busy.value = true
  error.value = ''
  try {
    const r = await api.activateInvite(inviteCode.value ?? '', username.value.trim(), password.value)
    clearInvite()
    // 激活即登录（Set-Cookie）：补 whoami 取权威快照，手法与登录路径一致
    let me: Whoami | null = null
    try {
      me = await api.whoami()
    } catch {
      /* 老服务端：按已登录保守处理 */
    }
    identity.value = mergeIdentity(identity.value, me, {
      admin: false,
      token_id: r.token_id ?? null,
    })
    emit('activated')
  } catch (e) {
    const resp = (
      e as {
        response?: { status?: number; data?: { error?: string; message?: string } }
      }
    )?.response
    const code = resp?.data?.error
    if (resp?.status === 409 && code === 'username_taken') {
      error.value = '该用户名已被占用，请换一个再试。'
    } else if (resp?.status === 409) {
      dead.value = true
      error.value = resp?.data?.message || DEAD_TEXT[code ?? ''] || '邀请码无效或已失效。'
    } else if (resp?.status === 429) {
      error.value = '尝试过于频繁，请稍后再试。'
    } else {
      // 服务端错误体优先（message=人读详情，error=带枚举前缀的原文），最后才退到 axios 泛型文案
      error.value = resp?.data?.message ?? resp?.data?.error ?? errMsg(e)
    }
  } finally {
    busy.value = false
  }
}

function dismiss() {
  clearInvite()
  emit('dismissed')
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
          <h1>邀请激活</h1>
          <p>这枚邀请码一次有效：设置用户名与密码即完成注册并登录。</p>
        </div>
      </div>

      <label class="gate-label" for="invite-user">用户名</label>
      <LInput
        id="invite-user"
        v-model="username"
        autofocus
        :disabled="busy || dead"
        placeholder="用户名（2–32 位小写字母/数字/._-）"
        autocomplete="username"
        @keyup.enter="dead ? dismiss() : activate()"
      />
      <label class="gate-label" for="invite-pass">设置密码</label>
      <LInput
        id="invite-pass"
        v-model="password"
        type="password"
        :disabled="busy || dead"
        placeholder="至少 8 位"
        autocomplete="new-password"
        @keyup.enter="activate"
      />
      <LButton v-if="!dead" variant="primary" :loading="busy" class="gate-enter" @click="activate">
        激活并登录
      </LButton>
      <LButton v-else class="gate-enter" @click="dismiss">返回</LButton>
      <p v-if="error" class="gate-error" role="alert">{{ error }}</p>
      <p class="gate-foot">
        <span>邀请码由管理员在「接入 → 邀请码」生成；忘记链接请向管理员重新索取。</span>
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
.gate-foot {
  margin: var(--space-1) 0 0;
  font-size: var(--fs-sm);
  color: var(--muted);
}
</style>
