/**
 * 检测结果反馈（采纳 / 放弃 / 反思 / 规则 / 知识 / L3 飞轮）—— ErrorList 的行为层
 *
 * 从 ErrorList.vue 拆出：全部「逐错误」反馈状态（以原始下标为 key）与动作。
 * 视图层（ErrorList.vue）只负责渲染与滚动联动；状态映射规则：
 *   - key = store.errors 原始下标（分类过滤 / 同类合并不改变 key）
 *   - store.errors 替换（新检测）→ 全部状态清空（防旧状态泄漏到同索引新错误）
 */
import { computed, reactive, ref, watch, type ComputedRef } from 'vue'
import { ElMessage, ElMessageBox } from 'element-plus'
import { useGecStore } from '../stores/gec'
import { api } from '../api/client'
import { applyCorrections } from '../lib/correct'
import type { MergedItem } from '../lib/errorGroups'
import type {
  AdoptResult,
  ErrorItem,
  RecallTestResult,
  ReflectionReport,
  ReflectionSuggestion,
} from '../types'

export function useErrorFeedback(visible: ComputedRef<MergedItem[]>) {
  const store = useGecStore()

  /** 本批检测内已采纳的错误索引（数据沉淀：累积应用修正 → corrected_text 送服务端） */
  const adoptedIdx = new Set<number>()
  /** 采纳沉淀的原文快照（新检测换文本时重置） */
  let adoptSrcText = ''

  /** 采纳沉淀上下文：原文 + 累积应用修正后的全文（含本次）；无原文/无变化 → undefined */
  function adoptCtx(index: number): { src_text: string; corrected_text: string } | undefined {
    const text = store.text ?? ''
    if (!text) return undefined
    if (text !== adoptSrcText) {
      adoptedIdx.clear()
      adoptSrcText = text
    }
    const errs = [
      ...[...adoptedIdx, index].map((i) => store.errors[i]).filter((x): x is ErrorItem => !!x),
    ]
    const corrected = applyCorrections(text, errs)
    return corrected !== text ? { src_text: text, corrected_text: corrected } : undefined
  }

  // LLM 兜底结果反馈（采纳/拒绝）
  const feedbackLoading = ref(-1)
  // 当前进行中的反馈动作：请求期间另一按钮禁用（而非双双转圈等待）
  const feedbackAction = ref<'adopt' | 'reject' | null>(null)
  // 反馈已完成（请求成功返回）：采纳/放弃按钮均禁用（该错误的反馈动作已落定）
  const feedbackDone = reactive<Record<number, boolean>>({})
  const feedbackMsg = reactive<Record<number, string>>({})
  const feedbackVia = reactive<Record<number, string>>({})

  // 采纳结果：规则分支（快捷启用/停用）/ 知识分支（召回测试 + 保留/放弃）
  const adoptResult = reactive<Record<number, AdoptResult>>({})
  const ruleEnabled = reactive<Record<number, boolean>>({})
  const knowledgeKept = reactive<Record<number, number>>({})
  const recallTest = reactive<Record<number, RecallTestResult>>({})
  const recallTesting = ref(-1)
  const knowledgeSaving = ref(-1)

  // 反思流程（放弃后）：报告 + 建议执行 + LLM 深度反思
  const reflection = reactive<Record<number, ReflectionReport>>({})
  const reasonInput = reactive<Record<number, string>>({})
  const showReason = reactive<Record<number, boolean>>({})
  const suggestionMsg = reactive<Record<string, string>>({})
  const suggestionLoading = ref('')
  const llmReflectReady = reactive<Record<number, boolean>>({})
  const llmReflecting = ref(-1)
  const llmReflectMsg = reactive<Record<number, string>>({})

  // 批量采纳运行态：scope 标签（分类级 / 合并组）+ 进度（0 = 未开始）
  const batch = reactive<{ scope: string; total: number; done: number }>({
    scope: '',
    total: 0,
    done: 0,
  })
  // 分类级「批量采纳」作用域 = 当前 visible（全部实例）；pending = 尚未落定反馈的条数
  const pendingCount = computed(() =>
    visible.value.filter(({ i }) => !feedbackDone[i]).length,
  )

  function ruleOf(i: number) {
    const r = adoptResult[i]
    return r && r.via === 'rule' ? r.rule : null
  }
  function anomalyOf(i: number) {
    const r = adoptResult[i]
    return r && r.via === 'anomaly' ? r.anomaly : null
  }
  function knowledgeOf(i: number) {
    const r = adoptResult[i]
    if (!r) return null
    if (r.via === 'knowledge') return r.knowledge
    // anomaly 蒸馏未沉淀 → 降级知识候选
    if (r.via === 'anomaly' && !r.anomaly.accepted) return r.knowledge ?? null
    return null
  }

  /** 单条采纳（统一反馈端点，全部来源层）；批量采纳与单条按钮共用本函数
   *  成功 → feedbackDone 落定 + 按 via 展示对应结果卡片；失败 → 卡片内错误信息 */
  async function adoptOne(index: number) {
    const e = store.errors[index]
    if (!e) return
    feedbackLoading.value = index
    feedbackAction.value = 'adopt'
    feedbackMsg[index] = ''
    feedbackVia[index] = ''
    try {
      const res = await api.errorFeedback(
        store.lang,
        e,
        'adopt',
        reasonInput[index] || undefined,
        adoptCtx(index),
      )
      // 成功返回 → 该错误的反馈动作已落定，采纳/放弃按钮均禁用
      feedbackDone[index] = true
      // 采纳沉淀：本条计入累积修正集（corrected_text 下次采纳时包含本条）
      adoptedIdx.add(index)
      // L3 飞轮：确认采纳的 LLM 检测结果汇入特征模型训练集（正样本，非阻断；服务端亦会采集，去重）
      if (e.source === 'llm') collectL3Sample(index)
      feedbackVia[index] = res.via ?? ''
      feedbackMsg[index] = res.message
      if (res.via === 'rule' || res.via === 'knowledge' || res.via === 'anomaly') {
        // 规则 / 知识 / anomaly 分支：与 LLM 采纳同构，复用既有卡片
        adoptResult[index] = res as unknown as AdoptResult
        if (res.via === 'rule') {
          ruleEnabled[index] = res.rule?.enabled ?? false
          ElMessage.success(res.message)
        } else if (res.via === 'anomaly' && res.anomaly?.accepted) {
          ElMessage.success(res.message)
        } else {
          if (store.text) {
            runRecallTest(index)
          } else {
            recallTest[index] = {
              similarity: null,
              threshold: 0.7,
              would_hit: null,
              note: '无全文可测：本次检测未提供原文',
            }
          }
        }
      } else {
        // 确认类（rule / knowledge 层采纳）：仅提示
        ElMessage.success(res.message)
      }
    } catch (err) {
      const msg = typeof err === 'string' ? err : (err as Error)?.message ?? String(err)
      feedbackMsg[index] = msg
      ElMessage.error(msg)
    } finally {
      feedbackLoading.value = -1
      feedbackAction.value = null
    }
  }

  /** 单条放弃（两段式）：首次点击展开原因输入框，「确认放弃」执行反思流程 */
  function rejectOne(index: number) {
    const e = store.errors[index]
    if (!e) return
    if (!showReason[index]) {
      showReason[index] = true
      reasonInput[index] = reasonInput[index] ?? ''
      return
    }
    void onFeedback(index, 'reject')
  }

  /** 执行放弃（白名单 + 反馈留痕 + 反思报告）；单条与合并组批量放弃共用本函数 */
  async function runReject(index: number, reason?: string) {
    const e = store.errors[index]
    if (!e) return
    feedbackLoading.value = index
    feedbackAction.value = 'reject'
    feedbackMsg[index] = ''
    feedbackVia[index] = ''
    try {
      const res = await api.errorFeedback(store.lang, e, 'reject', reason)
      // 成功返回 → 该错误的反馈动作已落定，采纳/放弃按钮均禁用
      feedbackDone[index] = true
      feedbackMsg[index] = res.message
      if (res.reflection) {
        reflection[index] = res.reflection
        llmReflectReady[index] = !!res.llm_reflect_ready
      }
      delete showReason[index]
      delete reasonInput[index]
      ElMessage.success(res.message)
    } catch (err) {
      const msg = typeof err === 'string' ? err : (err as Error)?.message ?? String(err)
      feedbackMsg[index] = msg
      ElMessage.error(msg)
    } finally {
      feedbackLoading.value = -1
      feedbackAction.value = null
    }
  }

  /** 批量放弃：合并组「确认放弃」作用于组内全部实例（共享同一放弃原因；
   *  相同 original → 白名单词相同，服务端按词去重；反思报告逐条留痕） */
  async function batchReject(items: MergedItem[], reason?: string) {
    const pending = items.filter(({ i }) => !feedbackDone[i])
    if (pending.length === 0) return
    let ok = 0
    let fail = 0
    for (const { i } of pending) {
      await runReject(i, reason)
      if (feedbackDone[i]) ok += 1
      else fail += 1
    }
    if (fail === 0) ElMessage.success(`已放弃 ${ok} 条`)
    else ElMessage.warning(`已放弃 ${ok} 条，失败 ${fail} 条（详见各卡片提示）`)
  }

  /** 统一反馈（全部来源层）：采纳 / 放弃（放弃 → 反思流程）
   *  放弃为两段式：首次点击展开原因输入框，「确认放弃」才执行（原因可选）；
   *  采纳分支委托 adoptOne（与批量采纳共用同一实现） */
  async function onFeedback(index: number, action: 'adopt' | 'reject') {
    const e = store.errors[index]
    if (!e) return
    if (action === 'reject' && !showReason[index]) {
      showReason[index] = true
      reasonInput[index] = reasonInput[index] ?? ''
      return
    }
    if (action === 'adopt') {
      await adoptOne(index)
      return
    }
    await runReject(index, reasonInput[index] || undefined)
  }


  /** 反思建议执行（排除词 / 停用规则 / 删示例 / 删知识 / L3 负样本 / 导航） */
  async function runSuggestion(index: number, j: number) {
    const r = reflection[index]
    const s: ReflectionSuggestion | undefined = r?.suggestions[j]
    if (!s) return
    // 导航类建议：切到对应视图（规则面板 / 设置页）
    if (s.kind === 'view_rule') {
      store.requestNav('rules')
      return
    }
    if (s.kind === 'retrain') {
      store.requestNav('settings')
      return
    }
    suggestionLoading.value = `${index}:${j}`
    try {
      const res = await api.feedbackAction(s.kind, s.payload)
      suggestionMsg[`${index}:${j}`] = res.message
      ElMessage.success(res.message)
    } catch (err) {
      const msg = typeof err === 'string' ? err : (err as Error)?.message ?? String(err)
      suggestionMsg[`${index}:${j}`] = msg
      ElMessage.error(msg)
    } finally {
      suggestionLoading.value = ''
    }
  }

  /** LLM 深度反思（可选；配置 LLM 后可用；与确定性反思分离，不阻塞放弃操作） */
  async function runLlmReflect(index: number) {
    const e = store.errors[index]
    if (!e) return
    llmReflecting.value = index
    llmReflectMsg[index] = ''
    try {
      const res = await api.reflectLlm(store.lang, e, reasonInput[index] || undefined)
      llmReflectMsg[index] = res.llm_analysis
    } catch (err) {
      const msg = typeof err === 'string' ? err : (err as Error)?.message ?? String(err)
      llmReflectMsg[index] = `LLM 反思失败：${msg}`
    } finally {
      llmReflecting.value = -1
    }
  }

  /** L3 飞轮：确认采纳的 LLM 检测结果汇入特征模型训练集（正样本；非阻断，失败静默） */
  function collectL3Sample(index: number) {
    const e = store.errors[index]
    if (!e) return
    void api.l3Collect({
      lang: store.lang,
      text: e.original,
      label: 1,
      error_type: e.error_type,
      source: 'llm_adopt',
    })
  }

  /** 知识分支：运行召回测试（判定 ②.5 知识召回层是否会命中） */
  async function runRecallTest(index: number) {
    const k = knowledgeOf(index)
    if (!k) return
    recallTesting.value = index
    try {
      recallTest[index] = await api.testKnowledgeRecall(store.lang, store.text, k)
    } catch (err) {
      const msg = typeof err === 'string' ? err : (err as Error)?.message ?? String(err)
      recallTest[index] = {
        similarity: null,
        threshold: 0.7,
        would_hit: null,
        note: `召回测试失败：${msg}`,
      }
    } finally {
      recallTesting.value = -1
    }
  }

  /** 知识分支：保留（写入知识存储；重检测相似文本时 ②.5 召回直接命中） */
  async function keepKnowledge(index: number) {
    const k = knowledgeOf(index)
    if (!k) return
    knowledgeSaving.value = index
    try {
      const res = await api.addLearnedKnowledge({
        lang: store.lang,
        text: k.text,
        error_type: k.error_type,
        correction: k.correction,
        source: 'llm_distill',
      })
      knowledgeKept[index] = res.id
      ElMessage.success('已保留到知识存储')
    } catch (err) {
      const msg = typeof err === 'string' ? err : (err as Error)?.message ?? String(err)
      ElMessage.error(msg)
    } finally {
      knowledgeSaving.value = -1
    }
  }

  /** 知识分支：放弃（不写入任何存储） */
  function discardKnowledge(index: number) {
    delete adoptResult[index]
    delete recallTest[index]
    delete knowledgeKept[index]
    delete feedbackMsg[index]
    delete feedbackVia[index]
    ElMessage.info('已放弃（未写入任何存储）')
  }

  /** 规则分支：快捷启用/停用（开关实时生效，无需重建检测器） */
  async function quickToggleRule(index: number, enabled: boolean) {
    const r = ruleOf(index)
    if (!r) return
    try {
      await api.toggleRule(store.lang, r.id, enabled)
      ruleEnabled[index] = enabled
      ElMessage.success(enabled ? `规则 ${r.id} 已启用（即时生效）` : `规则 ${r.id} 已停用`)
    } catch (err) {
      const msg = typeof err === 'string' ? err : (err as Error)?.message ?? String(err)
      ElMessage.error(msg)
    }
  }


  /** 批量采纳：分类级（作用域 = 当前 visible 全部实例）/ 同类合并级（作用域 = 组内全部实例）
   *  跳过已落定反馈的实例；无 correction 且非 anomaly 的实例服务端会拒（采纳需修正文本），预筛跳过；
   *  顺序执行（避免并发打规则蒸馏），单条失败不中断；结束弹汇总 */
  async function batchAdopt(items: MergedItem[], scope: string) {
    if (batch.scope) return
    const pending = items.filter(({ i }) => !feedbackDone[i])
    if (pending.length === 0) {
      ElMessage.info('作用域内没有待采纳的错误')
      return
    }
    const adoptable = pending.filter(
      ({ e }) => !!e.correction || e.error_type === 'anomaly',
    )
    const skipped = pending.length - adoptable.length
    const alreadyDone = items.length - pending.length
    try {
      await ElMessageBox.confirm(
        `将采纳 ${scope} 的 ${adoptable.length} 处错误` +
          (alreadyDone > 0 || skipped > 0
            ? `（共 ${items.length} 条，跳过 ${alreadyDone + skipped} 条：已反馈 ${alreadyDone}、无修正文本 ${skipped}）`
            : '') +
          '，确认继续？',
        '批量采纳',
        { confirmButtonText: '确认采纳', cancelButtonText: '取消', type: 'warning' },
      )
    } catch {
      return
    }
    batch.scope = scope
    batch.total = adoptable.length
    batch.done = 0
    let ok = 0
    let fail = 0
    for (const { i } of adoptable) {
      try {
        await adoptOne(i)
        if (feedbackDone[i]) ok += 1
        else fail += 1
      } catch {
        fail += 1
      }
      batch.done += 1
    }
    batch.scope = ''
    batch.total = 0
    batch.done = 0
    if (fail === 0) ElMessage.success(`批量采纳完成：成功 ${ok} 条`)
    else ElMessage.warning(`批量采纳完成：成功 ${ok} 条，失败 ${fail} 条（详见各卡片提示）`)
  }

  // 新检测替换错误列表 → 清空逐错误反馈状态（防旧状态泄漏到同索引的新错误，
  // 否则新错误的采纳/放弃按钮会被误禁用 / 显示上一次的反馈消息）
  function clearRecord<T>(r: Record<number, T>) {
    for (const k of Object.keys(r)) delete r[Number(k)]
  }
  watch(
    () => store.errors,
    () => {
      clearRecord(feedbackDone)
      clearRecord(feedbackMsg)
      clearRecord(feedbackVia)
      clearRecord(adoptResult)
      clearRecord(ruleEnabled)
      clearRecord(knowledgeKept)
      clearRecord(recallTest)
      clearRecord(reflection)
      clearRecord(reasonInput)
      clearRecord(showReason)
      clearRecord(llmReflectReady)
      clearRecord(llmReflectMsg)
      for (const k of Object.keys(suggestionMsg)) delete suggestionMsg[k]
    },
  )

  return {
    // 状态
    feedbackLoading,
    feedbackAction,
    feedbackDone,
    feedbackMsg,
    feedbackVia,
    adoptResult,
    ruleEnabled,
    knowledgeKept,
    recallTest,
    recallTesting,
    knowledgeSaving,
    reflection,
    reasonInput,
    showReason,
    suggestionMsg,
    suggestionLoading,
    llmReflectReady,
    llmReflecting,
    llmReflectMsg,
    batch,
    pendingCount,
    // 动作与派生
    ruleOf,
    anomalyOf,
    knowledgeOf,
    adoptOne,
    rejectOne,
    onFeedback,
    batchReject,
    batchAdopt,
    runSuggestion,
    runLlmReflect,
    runRecallTest,
    keepKnowledge,
    discardKnowledge,
    quickToggleRule,
  }
}

