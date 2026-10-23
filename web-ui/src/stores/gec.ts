import { errMsg, isCancelError } from '../lib/err'
import { applyCorrections } from '../lib/correct'
import { defineStore } from 'pinia'
import { api } from '../api/client'
import type {
  AnalyzedError,
  DecisionTableMeta,
  DetectTimings,
  LayerReport,
  ErrorItem,
  ExplainResponse,
  LanguageInfo,
} from '../types'

export interface LlmExplainState {
  loading: boolean
  explanation: string
  exercises: string
  meta: { model: string; template: string; template_version: string } | null
  /** true = LLM 未配置（503/502），已回退静态建议 */
  fallback: boolean
}

/** 后端不可达时的兜底文案（与 gec-zh / gec-en / gec-ja 检测器实现一致） */
const FALLBACK_LANGS: LanguageInfo[] = [
  { lang: 'zh', name: '中文', nlp_service: 'HanLP v2（分词 + POS + NER）', engine: 'rule' },
  { lang: 'en', name: '英文', nlp_service: 'HanLP 英文（分词 + POS）', engine: 'rule' },
  { lang: 'ja', name: '日文', nlp_service: 'Sudachi（分词 + 词性）', engine: 'rule' },
]

/** 草稿持久化（防误刷丢稿）：text / lang / level / sessionId 定时写 localStorage。
 *  隐私模式等写入失败静默降级（下次照常尝试）。 */
const DRAFT_KEY = 'langfixer.draft.v1'

interface Draft {
  text: string
  lang: string
  level: number
  sessionId: string
}

function restoreDraft(): Partial<Draft> {
  try {
    const raw = localStorage.getItem(DRAFT_KEY)
    if (!raw) return {}
    const d = JSON.parse(raw) as Partial<Draft>
    return {
      text: typeof d.text === 'string' ? d.text : '',
      lang: typeof d.lang === 'string' ? d.lang : 'zh',
      level: d.level === 2 || d.level === 3 ? d.level : 1,
      sessionId: typeof d.sessionId === 'string' ? d.sessionId : '',
    }
  } catch {
    return {}
  }
}

/** 检测请求的中止控制器（非响应性：仅 Web 端透传 axios signal；
 *  桌面端 invoke 无法中断，取消语义 = 返回结果到达后丢弃） */
let detectController: AbortController | null = null

export const useGecStore = defineStore('gec', {
  state: () => {
    const draft = restoreDraft()
    return {
      lang: draft.lang ?? 'zh',
      text: draft.text ?? '',
      errors: [] as ErrorItem[],
    analyzed: [] as AnalyzedError[],
    loading: false,
    /** 当前正在执行的检测动作（区分「检测 + 归因」/「仅检测」/「漏报检测」三个按钮：
     *  一个在执行时，其余按钮应呈禁用态而非等待态；null = 空闲） */
    detectKind: null as 'analyze' | 'detect' | 'detectLlm' | null,
    errorMsg: '',
    /** 检测是否已完成（区分"尚未检测"与"检测完成但未发现错误"） */
    detectCompleted: false,
    /** 本次结果来自历史复用（重复检测 + 参数一致 + 无 LLM；前端据此展示「来自历史」提示） */
    fromHistory: false,
    /** 决策模型是否弃权（仅决策层启用时有意义；前端据此展示提示） */
    decisionAbstained: false,
    /** LLM 兜底层实际执行且未检出错误（「LLM 复查确认干净」；
     *  前端区分「仅规则层未查出」与「LLM 复查确认无错误」两种空态） */
    llmConfirmedClean: false,
    /** 本次检测的层级贡献摘要（ResultsPanel「检测路径」；未实际检测时为 null） */
    layers: null as LayerReport[] | null,
    tables: [] as DecisionTableMeta[],
    activeLang: 'zh',
    tableDraft: '',
    tableStatus: '',
    /** 语言 / 引擎元信息（来自后端，前端不再硬编码引擎文案） */
    languages: [] as LanguageInfo[],
    /** 会话 ID（Editor 输入，讲解/归因共用） */
    sessionId: draft.sessionId ?? '',
    /** 讲解目标语言（空 = 跟随检测语言） */
    targetLanguage: '',
    /** 分类过滤（纯前端展示过滤，作用于检测结果列表；空 = 不过滤；不再作为检测参数下发） */
    categoryFilter: '',
    /** 三级响应级别：1 = 规则（+ 决策模型，若显式启用）；2 = + L3 门控（L3 弃权 → LLM 兜底；L3 未启用时回退决策模型弃权语义）；3 = + 强制 LLM */
    level: draft.level ?? 1,
    /** 决策模型（vegaml）层启用状态（来自 GET /gec/settings 的 decision.enabled；
     *  P0.5 起默认关闭，opt-in；与检测页「响应级别」L2 对齐：L3 未启用时 L2 的弃权兜底依赖它） */
    decisionEnabled: false,
    /** LLM 已配置（来自 GET /gec/settings 的 llm.enabled；检测管线 ③ LLM 兜底层状态显示用） */
    llmEnabled: false,
    /** 嵌入引擎已配置（来自 GET /gec/settings 的 embedding.endpoint；未配置时 ②.5 知识向量召回静默跳过） */
    embeddingConfigured: false,
    /** L3 特征模型层已启用（来自 GET /gec/settings 的 l3.enabled；三期：level 3 先跑特征模型，LLM 收缩到纯长尾；
     *  服务端默认开启，旧设置文件缺失该字段时按开启处理） */
    l3Enabled: true,
    /** 预览高亮联动：待定位的错误下标（-1 = 无） */
    focusErrorIndex: -1,
    /** 预览高亮联动：递增序号（重复点击同一片段也能重新触发滚动定位） */
    focusErrorNonce: 0,
    /** 检测历史自动刷新：detect / detectLlm / analyze 成功后递增（HistoryPanel 监听此值） */
    historyNonce: 0,
    /** 反思流程导航请求：「到规则面板查看 / 重训练」建议触发（App.vue 监听并切换视图） */
    navView: null as 'workspace' | 'rules' | 'settings' | 'evolution' | null,
    navNonce: 0,
    /** 进化中心 tab 级导航：反思「执行建议」成功后跳到「训练」tab 查看作业结果
     *  （App.vue 不感知；EvolutionCenter 监听 navTabNonce 切换 activeTab） */
    navTab: null as 'eval' | 'reflect' | 'train' | 'contribution' | null,
    navTabNonce: 0,
    /** 本次检测各阶段计时（detect / detectLlm / analyze 成功后保存；
     *  ResultsPanel 据此渲染「本次管线」摘要：哪些层实际跑了 / 各层耗时） */
    timings: null as DetectTimings | null,
    llm: {
      loading: false,
      explanation: '',
      exercises: '',
      meta: null,
      fallback: false,
    } as LlmExplainState,
    /** 结果快照：检测成功时的 text / lang（resultsStale 判定基准；null = 尚无结果） */
    resultText: null as string | null,
    resultLang: '',
    /** 结果版本号：每次检测成功 +1（ResultsPanel 仅在版本变化时自动切 tab，不打断手动浏览） */
    resultsVersion: 0,
    /** 最近一次检测动作（错误 alert「重试」按钮重放用；null = 无可重放动作） */
    lastDetectKind: null as 'analyze' | 'detect' | 'detectLlm' | null,
    /** 逐错误 LLM 讲解：目标下标 + 请求序号（Attribution 卡片「讲解」按钮 → ResultsPanel 切 tab → LlmExplain 触发） */
    explainIndex: 0,
    explainNonce: 0,
    /** 设置页锚点（PipelineStatus 点击 → 设置页滚动定位并高亮对应卡片） */
    settingsAnchor: '',
    }
  },
  getters: {
    /** 展示用语言列表：后端不可达时回退默认文案（卡片标题行语言切换器与正文管线提示共用） */
    displayLangs(): LanguageInfo[] {
      return this.languages.length > 0 ? this.languages : FALLBACK_LANGS
    },
    /** 「检测 + 归因」按钮是否正在执行（执行中显示 loading；其余检测按钮应禁用） */
    isAnalyzing(): boolean {
      return this.detectKind === 'analyze'
    },
    /** 「漏报检测」按钮是否正在执行（执行中显示 loading；其余检测按钮应禁用） */
    isDetectingLlm(): boolean {
      return this.detectKind === 'detectLlm'
    },
    /** 结果是否已过期：检测后文本被编辑 / 语言被切换（结果展示区据此提示并降级高亮） */
    resultsStale(): boolean {
      return this.resultText !== null && (this.text !== this.resultText || this.lang !== this.resultLang)
    },
    /** 是否存在可应用的修正（「复制修正全文 / 替换为修正文本」按钮显示条件） */
    hasCorrections(): boolean {
      return this.errors.some((e) => e.correction != null)
    },
    /** 对当前文本应用全部修正后的全文（与服务端 apply_corrections 同构，lib/correct.ts） */
    correctedText(): string {
      return applyCorrections(this.text, this.errors)
    },
  },
  actions: {
    /** 拉取语言 / 引擎元信息（GET /gec/languages 或 invoke('languages')） */
    async loadLanguages() {
      try {
        this.languages = await api.listLanguages()
      } catch {
        // 拉取失败时保持空列表，Editor 会回退到默认文案
      }
    },
    /** 拉取设置并同步检测管线各层启用状态（「响应级别」L2 状态显示 + 检测管线提示用） */
    async loadSettings() {
      try {
        const s = await api.getSettings()
        this.decisionEnabled = !!s.decision?.enabled
        this.llmEnabled = !!s.llm?.enabled
        this.embeddingConfigured = !!s.embedding?.endpoint
        // 旧服务端 / 旧设置文件缺失 l3 字段时按默认开启处理
        this.l3Enabled = s.l3?.enabled ?? true
      } catch {
        // 拉取失败时保持默认（未启用），L2 会显示「决策模型未启用」
      }
    },
    /** 从服务端重新校验检测管线各层启用状态（切回检测页 / 设置变更后调用，
     *  保证 L2 状态标签与管线提示和服务端真实状态一致；返回刷新后的决策模型状态，拉取失败返回 null） */
    async refreshEngineStatus(): Promise<boolean | null> {
      try {
        const s = await api.getSettings()
        this.decisionEnabled = !!s.decision?.enabled
        this.llmEnabled = !!s.llm?.enabled
        this.embeddingConfigured = !!s.embedding?.endpoint
        this.l3Enabled = s.l3?.enabled ?? true
        return this.decisionEnabled
      } catch {
        return null
      }
    },
    /** 清空全部检测 / 归因 / 讲解结果（新检测开始 / 语言切换 / 历史载入 / 替换修正文本时调用） */
    clearResults() {
      this.errors = []
      this.analyzed = []
      this.timings = null
      this.layers = null
      this.detectCompleted = false
      this.fromHistory = false
      this.decisionAbstained = false
      this.llmConfirmedClean = false
      this.focusErrorIndex = -1
      this.resultText = null
      this.resultLang = ''
      this.resetLlm()
    },
    /** 三个检测动作的统一入口：重置状态 + 启动中止控制器 */
    beginDetect(kind: 'analyze' | 'detect' | 'detectLlm') {
      this.loading = true
      this.detectKind = kind
      this.lastDetectKind = kind
      this.errorMsg = ''
      this.clearResults()
      detectController = new AbortController()
    },
    finishDetect() {
      this.loading = false
      this.detectKind = null
      detectController = null
    },
    /** 检测成功后快照结果基准（resultsStale 判定用） */
    snapshotResult() {
      this.resultText = this.text
      this.resultLang = this.lang
    },
    async detect() {
      this.beginDetect('detect')
      try {
        // 分类过滤为前端展示层行为（ErrorList 即时过滤），不再下发 category 参数
        const res = await api.detect(this.lang, this.text, undefined, this.level, false, {
          signal: detectController?.signal,
        })
        if (detectController?.signal.aborted) return // 已取消：结果到达也直接丢弃
        this.errors = res.errors
        this.decisionAbstained = !!res.decision_abstained
        this.fromHistory = !!res.from_history
        this.llmConfirmedClean = !!res.llm_confirmed_clean
        this.layers = res.layers ?? null
        this.timings = res.timings ?? null
        this.snapshotResult()
        this.detectCompleted = true
        this.resultsVersion += 1
        this.historyNonce += 1
      } catch (e) {
        if (!isCancelError(e) && !detectController?.signal.aborted) this.errorMsg = errMsg(e)
      } finally {
        this.finishDetect()
      }
    },
    /** 漏报检测（force_llm 强制兜底）：规则 + 决策模型完成后，无条件对全文跑一次 LLM 兜底 */
    async detectLlm() {
      this.beginDetect('detectLlm')
      try {
        // 分类过滤为前端展示层行为（ErrorList 即时过滤），不再下发 category 参数
        const res = await api.detect(this.lang, this.text, undefined, this.level, true, {
          signal: detectController?.signal,
        })
        if (detectController?.signal.aborted) return
        this.errors = res.errors
        this.decisionAbstained = !!res.decision_abstained
        this.llmConfirmedClean = !!res.llm_confirmed_clean
        this.layers = res.layers ?? null
        this.timings = res.timings ?? null
        this.snapshotResult()
        this.detectCompleted = true
        this.resultsVersion += 1
        this.historyNonce += 1
      } catch (e) {
        if (!isCancelError(e) && !detectController?.signal.aborted) this.errorMsg = errMsg(e)
      } finally {
        this.finishDetect()
      }
    },
    async analyze(sessionId?: string) {
      this.beginDetect('analyze')
      try {
        const res = await api.analyze(this.lang, this.text, sessionId, this.level, {
          signal: detectController?.signal,
        })
        if (detectController?.signal.aborted) return
        this.analyzed = res.errors
        this.errors = this.analyzed.map((a) => a.error)
        this.decisionAbstained = !!res.decision_abstained
        this.fromHistory = !!res.from_history
        this.llmConfirmedClean = !!res.llm_confirmed_clean
        this.layers = res.layers ?? null
        this.timings = res.timings ?? null
        this.snapshotResult()
        this.detectCompleted = true
        this.resultsVersion += 1
        this.historyNonce += 1
      } catch (e) {
        if (!isCancelError(e) && !detectController?.signal.aborted) this.errorMsg = errMsg(e)
      } finally {
        this.finishDetect()
      }
    },
    /** 取消进行中的检测：Web 端中止请求；桌面端 invoke 无法中断 → 语义为「结果到达后丢弃」 */
    cancelDetect() {
      detectController?.abort()
    },
    /** 重放最近一次检测动作（错误提示区「重试」按钮） */
    retryLast() {
      switch (this.lastDetectKind) {
        case 'analyze':
          void this.analyze(this.sessionId || undefined)
          break
        case 'detect':
          void this.detect()
          break
        case 'detectLlm':
          void this.detectLlm()
          break
      }
    },
    /** 载入文本到工作区（历史「载入 / 载入并重新检测」用）；清空旧结果 */
    loadText(text: string, lang?: string) {
      this.text = text
      if (lang) this.lang = lang
      this.clearResults()
    },
    /** 逐错误 LLM 讲解：请求对第 index 条归因结果生成讲解
     *  （ResultsPanel 监听 explainNonce 切到「LLM 个性化讲解」tab，LlmExplain 触发生成） */
    requestExplain(index: number) {
      this.explainIndex = index
      this.explainNonce += 1
    },
    resetLlm() {
      this.llm = { loading: false, explanation: '', exercises: '', meta: null, fallback: false }
    },
    /** 预览高亮联动：请求右侧结果面板滚动定位到指定错误（nonce 递增触发） */
    focusError(index: number) {
      this.focusErrorIndex = index
      this.focusErrorNonce += 1
    },
    /** 反思流程导航：请求切换到指定顶层视图（检测工作区 / 规则管理 / 设置 / 进化中心）；
     *  anchor 仅对 settings 有效：设置页滚动定位并高亮对应卡片（PipelineStatus 点击直达） */
    requestNav(view: 'workspace' | 'rules' | 'settings' | 'evolution', anchor?: string) {
      this.navView = view
      this.settingsAnchor = anchor ?? ''
      this.navNonce += 1
    },
    /** 进化中心 tab 级导航：请求切换到指定 tab（测评 / 反思 / 训练 / 贡献与轮次） */
    requestNavTab(tab: 'eval' | 'reflect' | 'train' | 'contribution') {
      this.navTab = tab
      this.navTabNonce += 1
    },
    /** LLM 个性化讲解（非流式）；LLM 未配置（503/502）时回退静态建议 */
    async explain(index: number) {
      const item = this.analyzed[index]
      if (!item) return
      this.llm = { loading: true, explanation: '', exercises: '', meta: null, fallback: false }
      try {
        const res = await api.explain({
          lang: this.lang,
          error_type: item.error.error_type,
          attribution: item.attribution?.attribution,
          cefr: item.attribution?.cefr,
          session_id: this.sessionId || undefined,
          target_language: this.targetLanguage || undefined,
        })
        if (res === null) {
          this.llm.fallback = true
          return
        }
        this.llm.explanation = res.explanation
        this.llm.exercises = res.exercises
        this.llm.meta = {
          model: res.model,
          template: res.template,
          template_version: res.template_version,
        }
      } catch (e) {
        this.errorMsg = errMsg(e)
      } finally {
        this.llm.loading = false
      }
    },
    /** LLM 个性化讲解（SSE 流式，逐 token 更新）；未配置时回退静态建议 */
    async explainStream(index: number) {
      const item = this.analyzed[index]
      if (!item) return
      this.llm = { loading: true, explanation: '', exercises: '', meta: null, fallback: false }
      try {
        const ok = await api.explainStream(
          {
            lang: this.lang,
            error_type: item.error.error_type,
            attribution: item.attribution?.attribution,
            cefr: item.attribution?.cefr,
            session_id: this.sessionId || undefined,
            target_language: this.targetLanguage || undefined,
          },
          {
            onMeta: (meta) => {
              this.llm.meta = meta
            },
            onToken: (token) => {
              this.llm.explanation += token
            },
            onDone: (done) => {
              this.llm.explanation = done.explanation || this.llm.explanation
              this.llm.exercises = done.exercises
            },
          },
        )
        if (ok === null) {
          this.llm.fallback = true
        }
      } catch (e) {
        this.errorMsg = errMsg(e)
      } finally {
        this.llm.loading = false
      }
    },
    async fetchTables() {
      this.tables = await api.listTables()
    },
    async fetchTable(lang: string) {
      this.activeLang = lang
      const view = await api.getTable(lang)
      try {
        this.tableDraft = JSON.stringify(JSON.parse(view.raw), null, 2)
      } catch {
        this.tableDraft = view.raw
      }
    },
    async saveTable() {
      this.tableStatus = ''
      try {
        const meta = await api.putTable(this.activeLang, this.tableDraft)
        this.tableStatus = `已保存 ${meta.lang} 决策表（${meta.updated_at}）`
        await this.fetchTables()
      } catch (e) {
        this.tableStatus = `保存失败：${errMsg(e)}`
      }
    },
  },
})
