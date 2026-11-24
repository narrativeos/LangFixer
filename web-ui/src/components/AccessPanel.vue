<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { api } from '../api/client'
import { LButton, LCheckbox, LInput, LSection } from '../ui'
import { toast } from '../ui/toast'
import AdminTokensPanel from './AdminTokensPanel.vue'
import AdminInitForm from './AdminInitForm.vue'
import { errMsg } from '../lib/err'
import {
  accessKey,
  identity,
  loginWithKey,
  mcpConfigJson,
  mcpEndpoint,
  refreshIdentity,
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

/** 管理员初始化：409 already_initialized 时回退到认领/登录 UI 并提示一次 */
const initHint = ref(false)
function initDone() {
  toast('管理员账号已初始化：本窗口已用 admin 登录，此后改设置用 admin + 密码登录', 'ok')
}

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
  if (Array.from(pwNew.value).length < 8) {
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
    toast('密码已更新：旧密码与其他会话作废，本窗口已换新会话', 'ok')
  } catch (e) {
    toast(`改密失败：${cleanError(e)}`, 'danger')
  } finally {
    pwBusy.value = false
  }
}

/**
 * 评审账号自助（仅网关形态的非管理员已登录用户）：用户名是同一枚密钥的口令别名。
 * 未设过 → 首次创建（用户名+口令）；已设 → 凭旧口令改新口令。
 * 成功后补一次 whoami 刷新 username/must_change（must_change 由自设回 false）。
 */
const myUname = ref('')
const myPwOld = ref('')
const myPwNew = ref('')
const myPwConfirm = ref('')
const myBusy = ref(false)
const showMyAccount = computed(
  () => identity.value.gateway && !identity.value.admin && identity.value.authenticated,
)
async function saveMyAccount() {
  const create = !identity.value.username
  const uname = myUname.value.trim()
  if (create && !uname) {
    toast('先填写用户名（2–32 位小写字母/数字/._-）', 'warn')
    return
  }
  if (!create && !myPwOld.value) {
    toast('请填写当前口令', 'warn')
    return
  }
  if (Array.from(myPwNew.value).length < 8) {
    toast('新口令至少 8 位', 'warn')
    return
  }
  if (myPwNew.value !== myPwConfirm.value) {
    toast('两次输入的新口令不一致', 'warn')
    return
  }
  myBusy.value = true
  try {
    await api.setCredentials(myPwNew.value, create ? { username: uname } : { oldPassword: myPwOld.value })
    myPwOld.value = ''
    myPwNew.value = ''
    myPwConfirm.value = ''
    await refreshIdentity()
    toast(
      create
        ? `已创建账号 ${uname}：以后可用它登录，MCP 仍用这枚密钥`
        : '口令已更新：旧口令与其他登录会话作废，本窗口已换新会话',
      'ok',
    )
  } catch (e) {
    toast(`保存失败：${cleanError(e)}`, 'danger')
  } finally {
    myBusy.value = false
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

      <!-- 单机形态装完即开：界面不锁，这里是唯一的「认领本机」入口（网关形态走门禁页）；
           管理员账号从未初始化时，换成初始化表单取代密钥认领 -->
      <template v-if="!identity.authenticated">
        <template v-if="identity.admin_initialized === false">
          <div class="banner">
            这台机器还没有管理员账号：先设置 <code>admin</code> 账号密码，用 env 里那把
            <code>lf_…</code> 超管密钥激活一次；此后改设置用 admin + 密码登录。
          </div>
          <AdminInitForm @done="initDone" @already="initHint = true" />
          <div v-if="initHint" class="banner banner-warn">
            管理员账号已初始化，请用超管密钥或 admin 密码登录。
          </div>
        </template>
        <template v-else>
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
          <div v-if="initHint" class="banner banner-warn">
            管理员账号已初始化，请用 admin + 密码登录或重新粘贴密钥认领。
          </div>
        </template>
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

      <!-- 评审账号：密钥身份的口令别名自助（网关形态）；配额/历史/MCP 仍按密钥计 -->
      <section v-if="showMyAccount" class="access-block">
        <h3>我的账号</h3>
        <p v-if="!identity.username">
          当前密钥还没有登录口令。设置后即可用「用户名 + 口令」直接登录本页；
          MCP 仍用这枚密钥，配额与检测历史都是同一本账。
        </p>
        <p v-else>
          账号 <code>{{ identity.username }}</code>：口令是这枚密钥的登录别名。
          改完旧口令与其他登录会话立即作废，本窗口自动换新会话。
        </p>
        <div v-if="identity.must_change" class="banner banner-warn">
          这是管理员代发的临时口令，请立即改为自己的口令。
        </div>
        <div class="pw-form">
          <LInput
            v-if="!identity.username"
            v-model="myUname"
            :disabled="myBusy"
            placeholder="用户名（2–32 位小写）"
            autocomplete="username"
          />
          <LInput
            v-else
            v-model="myPwOld"
            type="password"
            :disabled="myBusy"
            placeholder="当前口令"
            autocomplete="current-password"
          />
          <LInput
            v-model="myPwNew"
            type="password"
            :disabled="myBusy"
            placeholder="新口令（至少 8 位）"
            autocomplete="new-password"
          />
          <LInput
            v-model="myPwConfirm"
            type="password"
            :disabled="myBusy"
            placeholder="确认新口令"
            autocomplete="new-password"
            @keyup.enter="saveMyAccount"
          />
          <LButton variant="primary" :loading="myBusy" @click="saveMyAccount">
            {{ identity.username ? '更新口令' : '创建账号' }}
          </LButton>
        </div>
      </section>

      <!-- 改密：只对已验证超管且后端确实有密码登录时露出（桌面端不经 whoami，不展示） -->
      <section v-if="identity.authenticated && identity.admin && identity.password_login" class="access-block">
        <h3>修改管理员密码</h3>
        <p>
          用于 <code>admin</code> 账号的登录密码。改完旧密码与其余登录会话立即作废，
          本窗口自动换新会话继续可用；忘记时可用 <code>LANGFIXER_ADMIN_PASSWORD</code> 环境变量重置。
        </p>
        <div class="pw-form">
          <LInput v-model="pwOld" type="password" placeholder="当前密码" :disabled="pwBusy" autocomplete="current-password" />
          <LInput v-model="pwNew" type="password" placeholder="新密码（至少 8 位）" :disabled="pwBusy" autocomplete="new-password" />
          <LInput v-model="pwConfirm" type="password" placeholder="确认新密码" :disabled="pwBusy" autocomplete="new-password" @keyup.enter="changePw" />
          <LButton variant="primary" :loading="pwBusy" @click="changePw">更新密码</LButton>
        </div>
      </section>
    </LSection>

    <!-- 用户管理不限网关形态：单机形态用 admin + 密码登录后同样要能管账号 -->
    <AdminTokensPanel v-if="identity.authenticated && identity.admin" />
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
