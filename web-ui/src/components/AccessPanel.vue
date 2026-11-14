<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { ElMessage } from 'element-plus'
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

/** 认领 / 登录：换到会话后重新取密钥（超管密钥可直接粘进 MCP 用） */
async function claim() {
  const t = keyInput.value.trim()
  if (!t) {
    ElMessage.warning('请粘贴超管密钥')
    return
  }
  busy.value = true
  try {
    await loginWithKey(t)
    key.value = accessKey()
    keyInput.value = ''
    ElMessage.success('已认领本机：设置面现在需要超管登录后才能修改')
  } catch (e) {
    ElMessage.error(`验证失败：${cleanError(e)}`)
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
    ElMessage.success(`已复制${what}`)
  } catch (e) {
    ElMessage.error(`复制失败：${errMsg(e)}`)
  }
}

/** 退出：服务端结束会话 + 丢弃本次密钥（回到门禁页） */
async function leave() {
  try {
    await signOut()
  } catch (e) {
    ElMessage.error(`退出失败：${errMsg(e)}`)
  }
}
</script>

<template>
  <div class="access">
    <el-card shadow="never" class="access-card">
      <template #header>
        <div class="card-header">
          <span class="card-title">我的接入</span>
          <span class="card-sub">
            {{ identity.authenticated ? (identity.admin ? '超级管理员密钥' : '当前评审密钥') : '本机尚未认领' }} · MCP 端点 {{ mcpEndpoint() }}
          </span>
          <span class="spacer"></span>
          <el-button v-if="identity.authenticated" size="small" text @click="leave">
            退出登录
          </el-button>
        </div>
      </template>

      <!-- 单机形态装完即开：界面不锁，这里是唯一的「认领本机」入口（网关形态走门禁页） -->
      <template v-if="!identity.authenticated">
        <el-alert type="info" :closable="false" show-icon class="access-note">
          这台机器还没有人认领：检测功能照常可用，设置也照常可改。想让「设置」只由本机管理员修改，
          粘贴<strong>首次启动日志</strong>里那行 <code>lf_…</code> 超管密钥验证一次即可认领；
          认领之后改设置就要带着这把密钥登录，忘记只能改环境变量重置。
        </el-alert>
        <div class="access-row">
          <el-input
            v-model="keyInput"
            size="large"
            placeholder="超管密钥 lf_…（首次启动日志）"
            :disabled="busy"
            @keyup.enter="claim"
          />
          <el-button type="primary" size="large" :loading="busy" @click="claim">
            认领并锁定设置
          </el-button>
        </div>
      </template>

      <el-alert v-else-if="!key" type="info" :closable="false" show-icon class="access-note">
        本次是浏览器会话登录（服务端签发的小时级 cookie），地址栏与本地都不保存密钥，
        因此无法直接导出配置。需要 MCP 配置或直链时，退出后重新输入一次密钥即可。
      </el-alert>

      <template v-else>
        <el-alert
          v-if="identity.admin"
          type="warning"
          :closable="false"
          show-icon
          class="access-note"
        >
          这是<strong>超级管理员</strong>密钥：能改服务端配置、不进配额计量。
          自己用（含桌面 AI 客户端连本机）没问题，但<strong>不要发给他人</strong>——
          <template v-if="identity.gateway">给别人请到下方「密钥分发」签发评审密钥。</template>
          <template v-else
            >本机形态不签发他人密钥：要给多人评测请改用评测网关形态启动，再在这里分发。</template
          >
        </el-alert>

        <section class="access-block">
          <h3>形态一 · Web 直链</h3>
          <p>打开即自动通过验证页，适合发给用浏览器的评审。</p>
          <div class="access-row">
            <el-input
              :model-value="webUrl"
              size="large"
              readonly
              :type="showSecret ? 'text' : 'password'"
            />
            <el-button size="large" @click="copy(webUrl, '直链')">复制</el-button>
          </div>
          <el-checkbox v-model="showSecret">显示密钥</el-checkbox>
        </section>

        <section class="access-block">
          <h3>形态二 · MCP 配置</h3>
          <p>
            贴进 AI 客户端的 <code>mcpServers</code>（Claude Desktop / Cline / mcp.json 同构），
            与 web 共用同一枚密钥与同一份配额。
          </p>
          <el-input :model-value="mcpJson" type="textarea" :rows="8" readonly class="access-json" />
          <el-button size="large" @click="copy(mcpJson, 'MCP 配置')">复制配置</el-button>
        </section>
      </template>
    </el-card>

    <AdminTokensPanel v-if="identity.gateway && identity.admin" />
  </div>
</template>

<style scoped>
.access {
  display: flex;
  flex-direction: column;
  gap: 14px;
}
.card-header {
  display: flex;
  align-items: baseline;
  gap: 10px;
}
.card-title {
  font-weight: 600;
  color: var(--text);
}
.card-sub {
  font-size: 12px;
  color: var(--muted);
}
.access-note {
  margin-bottom: 4px;
}
.access-note :deep(code) {
  background: var(--surface-2);
  border: 1px solid var(--border);
  border-radius: 4px;
  padding: 0 4px;
}
.access-block {
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding-top: 6px;
}
.access-block + .access-block {
  margin-top: 14px;
  border-top: 1px solid var(--border);
  padding-top: 16px;
}
.access-block h3 {
  margin: 0;
  font-size: 14px;
  color: var(--text);
}
.access-block p {
  margin: 0;
  font-size: 12.5px;
  color: var(--text-2);
  line-height: 1.7;
}
.access-block code {
  background: var(--surface-2);
  border: 1px solid var(--border);
  border-radius: 4px;
  padding: 0 4px;
}
.access-row {
  display: flex;
  gap: 8px;
}
.access-row .el-input {
  flex: 1;
}
.access-json :deep(textarea) {
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 12px;
}
</style>
