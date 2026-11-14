<script setup lang="ts">
import { ref } from 'vue'
import { errMsg } from '../lib/err'
import { accessKey, clearAccessKey, identity, loginWithKey, mcpEndpoint } from '../lib/auth'

/**
 * Token 门禁页：验证通过前产品界面不渲染（App.vue 用它替换整个界面）。
 * 密钥来源两条：管理员分发的 `?token=` 直链（自动登录后地址栏即干净）与手输；
 * 无注册、无找回密码——服务端也只认这一张令牌表。
 */
const keyInput = ref('')
const busy = ref(false)
const error = ref('')

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
          <p>{{ identity.gateway ? '此实例已开启访问验证' : '本机设置已被超级管理员锁定' }}</p>
        </div>
      </div>

      <template v-if="!identity.authenticated">
        <label class="gate-label" for="gate-key">访问密钥</label>
        <el-input
          id="gate-key"
          v-model="keyInput"
          autofocus
          size="large"
          placeholder="lf_…（管理员分发）"
          :disabled="busy"
          @keyup.enter="enter"
        />
        <el-button type="primary" size="large" :loading="busy" class="gate-enter" @click="enter">
          进入
        </el-button>
        <el-alert v-if="error" class="gate-error" type="error" :closable="false" show-icon>
          {{ error }}
        </el-alert>
        <el-button v-if="hadKey" size="small" text @click="reset">清除本标签页记住的密钥</el-button>
        <ul class="gate-hint">
          <li>无需注册：密钥由管理员签发，直接粘贴即可。</li>
          <li>收到的是 <code>?token=…</code> 链接？打开就会自动进入，不用手输。</li>
          <li>密钥只保存在本次标签页，关闭浏览器即失效。</li>
        </ul>
      </template>

      <p class="gate-foot">
        <span>同一密钥可用于 MCP：{{ mcpEndpoint() }}</span>
      </p>
    </div>
  </div>
</template>

<style scoped>
.gate {
  min-height: 100vh;
  display: grid;
  place-items: center;
  padding: 24px;
  background: var(--bg);
}
.gate-card {
  width: min(420px, 100%);
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  box-shadow: var(--shadow-md);
  padding: 26px;
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.gate-brand {
  display: flex;
  align-items: center;
  gap: 10px;
  margin-bottom: 4px;
}
.gate-mark {
  width: 34px;
  height: 34px;
  display: grid;
  place-items: center;
  border-radius: var(--radius-sm);
  background: var(--primary-soft);
  color: var(--primary);
}
.gate-brand h1 {
  margin: 0;
  font-size: 17px;
  color: var(--text);
}
.gate-brand p {
  margin: 2px 0 0;
  font-size: 12.5px;
  color: var(--text-2);
}
.gate-label {
  font-size: 12.5px;
  color: var(--text-2);
}
.gate-enter {
  width: 100%;
}
.gate-row {
  display: flex;
  gap: 8px;
}
.gate-row .gate-enter {
  flex: 1;
  width: auto;
}
.gate-error {
  margin: 0;
}
.gate-hint {
  margin: 2px 0 0;
  padding-left: 18px;
  font-size: 12px;
  line-height: 1.7;
  color: var(--muted);
}
.gate-hint code {
  background: var(--surface-2);
  border: 1px solid var(--border);
  border-radius: 4px;
  padding: 0 4px;
}
.gate-foot {
  margin: 4px 0 0;
  display: flex;
  gap: 8px;
  align-items: center;
  font-size: 12px;
  color: var(--muted);
}
.gate-foot a {
  color: var(--primary);
  text-decoration: none;
}
</style>
