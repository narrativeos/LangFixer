<script setup lang="ts">
/**
 * 首次启动引导（T1-2 引导增强）
 *
 * 新用户打开应用直接落在「检测」页，但不知道：
 * - 系统当前能力到哪一层（哪些外部服务已配置）
 * - 想解锁更多能力该去哪配
 * - 「进化中心」是干什么的
 *
 * 首次进入检测视图时弹出三步引导（localStorage 记录，关闭后不再出现）。
 */
import { ref, watch } from 'vue'
import { useGecStore } from '../stores/gec'

const props = defineProps<{ visible: boolean }>()
const emit = defineEmits<{ (e: 'done'): void }>()

const store = useGecStore()
const shown = ref(false)

watch(
  () => props.visible,
  (v) => {
    if (v) shown.value = true
  },
  { immediate: true },
)

function finish() {
  emit('done')
}

function goSettings() {
  store.requestNav('settings')
  finish()
}
function goEvolution() {
  store.requestNav('evolution')
  finish()
}
</script>

<template>
  <el-dialog
    v-model="shown"
    title="欢迎使用 LangFixer"
    width="620px"
    :close-on-click-modal="false"
    :before-close="() => finish()"
  >
    <p class="onboard-lead">
      多语言语法错误检测 × ZEN 决策表教育归因。三步上手：
    </p>
    <ol class="onboard-steps">
      <li>
        <b>开始检测</b>
        <span>
          在左侧「文本输入」粘贴或输入待检测文本，点
          <el-button size="small" type="primary" plain>检测 + 归因</el-button>
          —— 中列实时高亮错误位置，右列查看检测结果与教育归因（错误为什么发生）。
        </span>
      </li>
      <li>
        <b>按需增强能力</b>
        <span>
          顶部「系统状态」条显示检测管线各层是否可用。想启用决策模型 / 特征模型 /
          LLM 兜底，去
          <el-button size="small" @click="goSettings">设置</el-button>
          配置对应服务（可先点「测试全部」检查连通性）。未配置不影响基础检测——本地规则层始终可用。
        </span>
      </li>
      <li>
        <b>让系统自我改进</b>
        <span>
          使用「检测」积累的反馈可汇入
          <el-button size="small" @click="goEvolution">进化中心</el-button>
          ：测评（量化能力）→ 反思（发现问题与建议）→ 训练（应用改进）→ 贡献与轮次（验证效果）。
        </span>
      </li>
    </ol>
    <template #footer>
      <el-button type="primary" @click="finish">开始使用</el-button>
    </template>
  </el-dialog>
</template>

<style scoped>
.onboard-lead { margin: 0 0 10px; color: var(--el-text-color-regular); }
.onboard-steps { margin: 0; padding-left: 20px; display: flex; flex-direction: column; gap: 12px; }
.onboard-steps li { display: flex; gap: 8px; line-height: 1.7; font-size: 13.5px; }
.onboard-steps li > b { flex-shrink: 0; }
.onboard-steps li > span { color: var(--el-text-color-regular); }
</style>
