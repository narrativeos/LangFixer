import axios from 'axios'
import type {
  AdoptResult,
  AnalyzedError,
  AnalyzeResult,
  AppSettings,
  ApplyReport,
  AttributionResult,
  CategoryInfo,
  CustomWord,
  DecisionTableMeta,
  DecisionTableView,
  DetectResponse,
  DetectTimings,
  ErrorItem,
  ExampleList,
  ExplainResponse,
  FeedbackHistory,
  FeedbackResponse,
  HistoryDetail,
  HistoryListItem,
  HistoryListPage,
  IngestDraftItem,
  IngestSessionDetail,
  IngestSessionSummary,
  IngestStartResponse,
  KnowledgePoint,
  KnowledgeSource,
  L3FlywheelStatus,
  MergePlan,
  PackCleanResponse,
  PackExportResult,
  PackImportResponse,
  PackSummary,
  ParsedDocPreview,
  L3RetrainResult,
  LanguageInfo,
  LearnedKnowledge,
  ListWordsResponse,
  LlmExplainStreamHandlers,
  PutTogglesResponse,
  RecallTestResult,
  ReflectLlmResult,
  RuleInfo,
  RulePlan,
  RulesResponse,
  SettingsTestResult,
  SettingsTestTarget,
  SessionAnalysis,
  SessionMeta,
  SessionRecord,
  TableVersion,
  ToggleResponse,
  UpdateSettingsBody,
  ValidateResponse,
  EvalRunRequest,
  EvalJob,
  EvalReport,
  EvalReportSummary,
  ReflectRunRequest,
  ReflectRunResponse,
  ReflectionReportSummary,
  ReflectionReportFull,
  ReflectionSuggestionRow,
  TrainingJobRow,
  TrainPoolResponse,
  TrainLlmDataResponse,
  TrainL3RetrainResponse,
  TrainLexiconApplyResponse,
  TrainRunSuggestionResponse,
  ContributionResponse,
  OptimizationRound,
  RoundEvaluateResponse,
  RoundReport,
  MediaApplyMode,
  MediaInspectResult,
  MediaJob,
  MediaMode,
  MediaProbes,
  MediaReportBody,
  MediaReportResult,
} from '../types'

const http = axios.create({ baseURL: '/', timeout: 60000 })

/** responseType: 'blob' 时错误响应体也是 Blob → 读回文本还原为常规 Error（保留服务端错误消息） */
async function normalizeBlobError(e: unknown): Promise<unknown> {
  const data = (e as { response?: { data?: unknown } })?.response?.data
  if (data instanceof Blob) {
    try {
      const text = await data.text()
      const parsed = JSON.parse(text) as { error?: string }
      return new Error(parsed.error ?? text)
    } catch {
      /* 非 JSON 错误体：保持原错误 */
    }
  }
  return e
}

/** 浏览器端导出 zip：请求 ?format=zip 附件并触发浏览器下载 */
async function exportZipDownload(sourceId: string): Promise<PackExportResult> {
  try {
    const resp = await http.post(
      `/gec/knowledge/dataset/${encodeURIComponent(sourceId)}/export`,
      null,
      { params: { format: 'zip' }, responseType: 'blob' },
    )
    const blob = resp.data as Blob
    const disposition = String(resp.headers?.['content-disposition'] ?? '')
    const match = disposition.match(/filename="?([^";]+)"?/)
    const filename = match?.[1] ?? `${sourceId}-resource-pack.zip`
    const url = URL.createObjectURL(blob)
    const a = document.createElement('a')
    a.href = url
    a.download = filename
    a.click()
    URL.revokeObjectURL(url)
    return { filename, size_bytes: blob.size }
  } catch (e) {
    throw await normalizeBlobError(e)
  }
}

/** 载体（图片 / PDF / Word）审校两端都可用：链路在 `media-flow` crate 里只有一份——
 *  Web 端打 api-server 的 HTTP，桌面端 invoke `desktop-app/src/media.rs` 的同一套编排
 *  （注入的是本地检测级联）。字节传输差异由下面两个 helper 抹平：`invoke` 只能走 JSON */
interface BinaryPayload {
  content_type: string
  data_b64: string
  size_bytes?: number
}

/** Uint8Array → base64（分块：整图几 MB，直接 spread 会爆调用栈） */
function bytesToB64(data: Uint8Array): string {
  let bin = ''
  const chunk = 0x8000
  for (let i = 0; i < data.length; i += chunk) {
    bin += String.fromCharCode(...data.subarray(i, i + chunk))
  }
  return btoa(bin)
}

/** `{ content_type, data_b64 }` → Blob（预览图 / 导出产物） */
function b64ToBlob(p: BinaryPayload): Blob {
  const bin = atob(p.data_b64)
  const bytes = new Uint8Array(bin.length)
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i)
  return new Blob([bytes], { type: p.content_type || 'application/octet-stream' })
}

/** Tauri 桌面端内部 invoke（window.__TAURI_INTERNALS__，无需 @tauri-apps/api 依赖）
 *  桌面端离线运行：检测/归因/决策表/讲解全部走 Rust 侧命令，不依赖 api-server */
type TauriInternals = {
  invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>
}
const tauri = (
  typeof window !== 'undefined' ? (window as unknown as { __TAURI_INTERNALS__?: TauriInternals }) : undefined
)?.__TAURI_INTERNALS__
const inTauri = !!tauri
/** 是否运行在 Tauri 桌面端（前端自适应布局 / 面板分支用） */
export const isTauri = inTauri

/** Tauri 命令参数（与 desktop-app/src/commands.rs 的 Rust 参数名一一对应，snake_case） */
interface ExplainBody {
  lang: string
  error_type: string
  attribution?: string
  cefr?: string
  session_id?: string
  session_error_count?: number
  target_language?: string
}

/** 桌面端 LLM 讲解：invoke('explain') 返回 JSON 字符串；LLM 未配置时 resolve(null)（与 Web 端 502 语义一致） */
async function tauriExplain(body: ExplainBody): Promise<ExplainResponse | null> {
  try {
    const raw = await tauri!.invoke('explain', {
      lang: body.lang,
      errorType: body.error_type,
      attribution: body.attribution ?? null,
      cefr: body.cefr ?? null,
      sessionId: body.session_id ?? null,
      sessionErrorCount: body.session_error_count ?? null,
      targetLanguage: body.target_language ?? null,
    })
    return JSON.parse(raw as string) as ExplainResponse
  } catch (e) {
    const msg = typeof e === 'string' ? e : (e as Error)?.message ?? String(e)
    // 桌面端 explain 未配置 LLM 时报错含「未配置」→ 回退静态建议
    if (msg.includes('未配置')) return null
    throw new Error(msg)
  }
}

/** 桌面端历史详情 → 列表项（与 Web 端 /gec/history 列表结构一致） */
function toHistoryListItem(h: HistoryDetail): HistoryListItem {
  let errorCount = 0
  try {
    errorCount = (JSON.parse(h.errors_json) as unknown[]).length
  } catch {
    /* 忽略无法解析的 errors_json */
  }
  let totalMs: number | null = null
  if (h.timings_json) {
    try {
      totalMs = (JSON.parse(h.timings_json) as DetectTimings).total_ms ?? null
    } catch {
      /* 忽略无法解析的 timings_json */
    }
  }
  return {
    id: h.id,
    session_id: h.session_id,
    lang: h.lang,
    text_preview: Array.from(h.text).slice(0, 80).join(''),
    error_count: errorCount,
    total_ms: totalMs,
    version: h.version ?? null,
    created_at: h.created_at,
  }
}

export const api = {
  /** GET /gec/languages — 语言 / 引擎元信息（桌面端：invoke('languages')）
   *  前端语言切换器据此展示真实引擎信息 */
  listLanguages(): Promise<LanguageInfo[]> {
    if (inTauri) return tauri!.invoke('languages').then((r) => r as LanguageInfo[])
    return http.get<LanguageInfo[]>('/gec/languages').then((r) => r.data)
  },

  /** POST /gec/detect（桌面端：invoke detect）；支持可选 category 过滤 + 漏报强制兜底（forceLlm）；
   *  Web 端支持 signal 取消；level>=2 或 forceLlm 时超时放宽到 180s（LLM 兜底常超 60s） */
  detect(
    lang: string,
    text: string,
    category?: string,
    level?: number,
    forceLlm?: boolean,
    opts?: { signal?: AbortSignal },
  ): Promise<DetectResponse> {
    if (inTauri)
      return tauri!
        .invoke('detect', {
          lang,
          text,
          category: category ?? null,
          level: level ?? null,
          forceLlm: forceLlm ?? false,
        })
        .then((r) => r as DetectResponse)
    const url = category ? `/gec/detect?category=${encodeURIComponent(category)}` : '/gec/detect'
    return http
      .post<DetectResponse>(
        url,
        { lang, text, level: level ?? 1, force_llm: forceLlm ?? false },
        { signal: opts?.signal, timeout: forceLlm || (level ?? 1) >= 2 ? 180_000 : 60_000 },
      )
      .then((r) => r.data)
  },

  /** GET /gec/rules?lang= — 列出指定语言的所有规则（桌面端：invoke list_rules） */
  listRules(lang: string): Promise<RulesResponse> {
    if (inTauri) return tauri!.invoke('list_rules', { lang }).then((r) => r as RulesResponse)
    return http.get<RulesResponse>('/gec/rules', { params: { lang } }).then((r) => r.data)
  },

  /** GET /gec/knowledge/sources — 知识源注册表（桌面端：invoke knowledge_sources） */
  listKnowledgeSources(): Promise<KnowledgeSource[]> {
    if (inTauri) return tauri!.invoke('knowledge_sources').then((r) => r as KnowledgeSource[])
    return http.get<KnowledgeSource[]>('/gec/knowledge/sources').then((r) => r.data)
  },

  /** GET /gec/knowledge/points?lang=&source_id= — 知识点列表（可按知识源过滤；
   *  桌面端：invoke knowledge_points） */
  listKnowledgePoints(lang: string, sourceId?: string): Promise<KnowledgePoint[]> {
    if (inTauri)
      return tauri!
        .invoke('knowledge_points', { lang, sourceId: sourceId ?? null })
        .then((r) => r as KnowledgePoint[])
    return http
      .get<KnowledgePoint[]>('/gec/knowledge/points', {
        params: { lang, ...(sourceId ? { source_id: sourceId } : {}) },
      })
      .then((r) => r.data)
  },

  /** POST /gec/knowledge/ingest — 一键导入：注册知识源 + 正文分块 + 后台逐块 agent 挖掘
   *  （桌面端：invoke start_ingest） */
  startIngest(body: {
    source_id: string
    title: string
    author?: string
    edition?: string
    lang: string
    license_scope?: string
    text: string
  }): Promise<IngestStartResponse> {
    if (inTauri)
      return tauri!.invoke('start_ingest', { req: body }).then((r) => r as IngestStartResponse)
    return http.post<IngestStartResponse>('/gec/knowledge/ingest', body).then((r) => r.data)
  },

  /** GET /gec/knowledge/ingest — 导入会话摘要列表（桌面端：invoke ingest_sessions） */
  listIngestSessions(): Promise<IngestSessionSummary[]> {
    if (inTauri) return tauri!.invoke('ingest_sessions').then((r) => r as IngestSessionSummary[])
    return http.get<IngestSessionSummary[]>('/gec/knowledge/ingest').then((r) => r.data)
  },

  /** GET /gec/knowledge/ingest/:id — 会话详情（进度/草稿/错误/状态；桌面端：invoke ingest_session） */
  getIngestSession(sourceId: string): Promise<IngestSessionDetail> {
    if (inTauri)
      return tauri!.invoke('ingest_session', { sourceId }).then((r) => r as IngestSessionDetail)
    return http.get<IngestSessionDetail>(`/gec/knowledge/ingest/${encodeURIComponent(sourceId)}`).then(
      (r) => r.data,
    )
  },

  /** GET /gec/knowledge/ingest/:id/drafts — 草稿明细（含逐条归并预判；桌面端：invoke ingest_drafts） */
  listIngestDrafts(sourceId: string): Promise<IngestDraftItem[]> {
    if (inTauri) return tauri!.invoke('ingest_drafts', { sourceId }).then((r) => r as IngestDraftItem[])
    return http
      .get<IngestDraftItem[]>(`/gec/knowledge/ingest/${encodeURIComponent(sourceId)}/drafts`)
      .then((r) => r.data)
  },

  /** GET /gec/knowledge/ingest/:id/plan — 发布预览（归并计划；桌面端：invoke ingest_plan） */
  getIngestPlan(sourceId: string): Promise<MergePlan> {
    if (inTauri) return tauri!.invoke('ingest_plan', { sourceId }).then((r) => r as MergePlan)
    return http
      .get<MergePlan>(`/gec/knowledge/ingest/${encodeURIComponent(sourceId)}/plan`)
      .then((r) => r.data)
  },

  /** POST /gec/knowledge/ingest/:id/publish — 人工一键发布（热更新知识库；
   *  服务器设置 GEC_TABLES_UPDATE_TOKEN 时需 token；桌面端：invoke publish_ingest） */
  publishIngest(sourceId: string, token?: string): Promise<ApplyReport> {
    if (inTauri)
      return tauri!.invoke('publish_ingest', { sourceId }).then((r) => r as ApplyReport)
    return http
      .post<ApplyReport>(`/gec/knowledge/ingest/${encodeURIComponent(sourceId)}/publish`, null, {
        headers: token ? { 'X-Tables-Update-Token': token } : undefined,
      })
      .then((r) => r.data)
  },

  /** POST /gec/knowledge/ingest/:id/discard — 放弃会话（丢弃草稿；桌面端：invoke discard_ingest） */
  discardIngest(sourceId: string): Promise<{ source_id: string; dropped_drafts: number }> {
    if (inTauri)
      return tauri!
        .invoke('discard_ingest', { sourceId })
        .then((r) => r as { source_id: string; dropped_drafts: number })
    return http
      .post<{ source_id: string; dropped_drafts: number }>(
        `/gec/knowledge/ingest/${encodeURIComponent(sourceId)}/discard`,
      )
      .then((r) => r.data)
  },

  /** POST /gec/knowledge/parse-preview — 书本文件解析预览（不落状态；
   *  桌面端：invoke parse_preview） */
  parsePreview(filename: string, data: Uint8Array): Promise<ParsedDocPreview> {
    if (inTauri)
      return tauri!
        .invoke('parse_preview', { filename, data: Array.from(data) })
        .then((r) => r as ParsedDocPreview)
    return http
      .post<ParsedDocPreview>('/gec/knowledge/parse-preview', data, {
        params: { filename },
        headers: { 'Content-Type': 'application/octet-stream' },
      })
      .then((r) => r.data)
  },

  /** 文件上传导入：解析 + 章节选择 + 分块 + 后台逐块挖掘
   *  （桌面端：invoke start_ingest_file） */
  startIngestFile(body: {
    filename: string
    data: Uint8Array
    source_id: string
    title?: string
    author?: string
    edition?: string
    lang: string
    license_scope?: string
    chapters?: number[]
  }): Promise<IngestStartResponse> {
    if (inTauri)
      return tauri!
        .invoke('start_ingest_file', {
          req: { ...body, data: Array.from(body.data) },
        })
        .then((r) => r as IngestStartResponse)
    return http
      .post<IngestStartResponse>('/gec/knowledge/ingest/upload', body.data, {
        params: {
          filename: body.filename,
          source_id: body.source_id,
          title: body.title ?? '',
          author: body.author ?? '',
          edition: body.edition ?? '',
          lang: body.lang,
          license_scope: body.license_scope ?? 'internal_test',
          ...(body.chapters && body.chapters.length ? { chapters: body.chapters.join(',') } : {}),
        },
        headers: { 'Content-Type': 'application/octet-stream' },
      })
      .then((r) => r.data)
  },

  /** POST /gec/knowledge/ingest/:id/derive-rules — 开启归因规则派生
   *  （知识点挖掘完成后、发布前；桌面端：invoke derive_rules） */
  deriveIngestRules(sourceId: string): Promise<{ source_id: string; batches_total: number }> {
    if (inTauri)
      return tauri!
        .invoke('derive_rules', { sourceId })
        .then((r) => r as { source_id: string; batches_total: number })
    return http
      .post<{ source_id: string; batches_total: number }>(
        `/gec/knowledge/ingest/${encodeURIComponent(sourceId)}/derive-rules`,
      )
      .then((r) => r.data)
  },

  /** GET /gec/knowledge/ingest/:id/rules-plan — 规则归并预览（null = 无规则草稿；
   *  桌面端：invoke ingest_rules_plan） */
  getIngestRulesPlan(sourceId: string): Promise<RulePlan | null> {
    if (inTauri) return tauri!.invoke('ingest_rules_plan', { sourceId }).then((r) => r as RulePlan | null)
    return http
      .get<RulePlan | null>(`/gec/knowledge/ingest/${encodeURIComponent(sourceId)}/rules-plan`)
      .then((r) => r.data)
  },

  /** GET /gec/knowledge/dataset/:id — 资源包盘点（点数/规则数/aux/草稿清单；
   *  删除前预览；桌面端：invoke dataset_summary） */
  datasetSummary(sourceId: string): Promise<PackSummary> {
    if (inTauri) return tauri!.invoke('dataset_summary', { sourceId }).then((r) => r as PackSummary)
    return http
      .get<PackSummary>(`/gec/knowledge/dataset/${encodeURIComponent(sourceId)}`)
      .then((r) => r.data)
  },

  /** POST /gec/knowledge/dataset/:id/export?format=zip — 打包导出为 zip 压缩包：
   *  浏览器端直接触发下载（文件名 {source_id}-resource-pack.zip）；
   *  桌面端先弹原生「另存为」选择保存位置（dialog 插件），取消返回 null */
  async exportDataset(sourceId: string): Promise<PackExportResult | null> {
    if (inTauri) {
      const target = (await tauri!
        .invoke('plugin:dialog|save', {
          options: {
            title: '导出资源包（zip 压缩包）',
            defaultPath: `${sourceId}-resource-pack.zip`,
            filters: [{ name: '资源包压缩包', extensions: ['zip'] }],
          },
        })
        .catch(() => null)) as string | null
      if (!target) return null
      return (await tauri!.invoke('dataset_export', {
        sourceId,
        path: target,
      })) as PackExportResult
    }
    return exportZipDownload(sourceId)
  },

  /** 删除前自动备份：不弹「另存为」——桌面端写默认导出目录（同名自动加时间戳），
   *  浏览器端直接下载；失败由调用方中止删除 */
  async exportDatasetBackup(sourceId: string): Promise<PackExportResult> {
    if (inTauri)
      return (await tauri!.invoke('dataset_export', { sourceId })) as PackExportResult
    return exportZipDownload(sourceId)
  },

  /** POST /gec/knowledge/dataset/import — 导入资源包（请求体 = zip 压缩包或裸
   *  资源包 JSON 字节；校验通过后知识库热更新 + 决策表按 _meta 合并 + 决策数据
   *  写回；服务器设置 token 时需携带；桌面端：invoke dataset_import_bytes） */
  importDataset(data: Uint8Array, token?: string): Promise<PackImportResponse> {
    if (inTauri)
      return tauri!
        .invoke('dataset_import_bytes', { data: Array.from(data) })
        .then((r) => r as PackImportResponse)
    return http
      .post<PackImportResponse>('/gec/knowledge/dataset/import', data, {
        headers: {
          'Content-Type': 'application/octet-stream',
          ...(token ? { 'X-Tables-Update-Token': token } : {}),
        },
        transformRequest: (d) => d,
      })
      .then((r) => r.data)
  },

  /** POST /gec/knowledge/dataset/:id/clean — 删除资源包（知识库 + 决策表该源规则行 +
   *  决策数据 + 草稿；keepOrphanPoints=true 保留失去全部背书的点；
   *  桌面端：invoke dataset_clean） */
  cleanDataset(
    sourceId: string,
    keepOrphanPoints?: boolean,
    token?: string,
  ): Promise<PackCleanResponse> {
    if (inTauri)
      return tauri!
        .invoke('dataset_clean', { sourceId, keepOrphanPoints: keepOrphanPoints ?? false })
        .then((r) => r as PackCleanResponse)
    return http
      .post<PackCleanResponse>(
        `/gec/knowledge/dataset/${encodeURIComponent(sourceId)}/clean`,
        null,
        {
          params: keepOrphanPoints ? { keep_orphan_points: '1' } : undefined,
          headers: token ? { 'X-Tables-Update-Token': token } : undefined,
        },
      )
      .then((r) => r.data)
  },

  /** PUT /gec/rules/:id/toggle — 切换规则开关（桌面端：invoke toggle_rule） */
  toggleRule(lang: string, id: string, enabled: boolean): Promise<ToggleResponse> {
    if (inTauri)
      return tauri!.invoke('toggle_rule', { lang, id, enabled }).then((r) => r as ToggleResponse)
    return http
      .put<ToggleResponse>(`/gec/rules/${encodeURIComponent(id)}/toggle`, null, {
        params: { lang, enabled },
      })
      .then((r) => r.data)
  },

  /** POST /gec/custom-words — 添加自定义词（桌面端：invoke add_custom_word） */
  addCustomWord(lang: string, word: string, reason?: string): Promise<CustomWord> {
    if (inTauri)
      return tauri!
        .invoke('add_custom_word', { lang, word, reason: reason ?? null })
        .then((r) => r as CustomWord)
    return http
      .post<CustomWord>('/gec/custom-words', { lang, word, reason: reason ?? '' })
      .then((r) => r.data)
  },

  /** GET /gec/custom-words?lang= — 列出自定义词（桌面端：invoke list_custom_words） */
  listCustomWords(lang: string): Promise<ListWordsResponse> {
    if (inTauri)
      return tauri!
        .invoke('list_custom_words', { lang })
        .then((r) => ({ lang, words: r as CustomWord[] }))
    return http.get<ListWordsResponse>('/gec/custom-words', { params: { lang } }).then((r) => r.data)
  },

  /** DELETE /gec/custom-words/:id — 删除自定义词（桌面端：invoke remove_custom_word） */
  removeCustomWord(id: number): Promise<void> {
    if (inTauri) return tauri!.invoke('remove_custom_word', { id }).then(() => undefined)
    return http.delete(`/gec/custom-words/${id}`).then(() => undefined)
  },

  /** POST /gec/llm-feedback — LLM 兜底结果反馈（采纳/拒绝）（桌面端：invoke llm_feedback）
   *  采纳返回 `AdoptResult`（`via: 'rule'` 规则分支 / `via: 'knowledge'` 知识分支）；
   *  拒绝返回 `{ action: 'reject', message, word_id? }` */
  llmFeedback(
    lang: string,
    error: ErrorItem,
    action: 'adopt' | 'reject',
  ): Promise<AdoptResult | { action: 'reject'; message: string; word_id?: number }> {
    if (inTauri)
      return tauri!
        .invoke('llm_feedback', { lang, error, action })
        .then((r) => r as AdoptResult | { action: 'reject'; message: string; word_id?: number })
    return http
      .post<AdoptResult | { action: 'reject'; message: string; word_id?: number }>('/gec/llm-feedback', {
        lang,
        error,
        action,
      })
      .then((r) => r.data)
  },

  /** POST /gec/knowledge/recall-test — 知识召回测试（桌面端：invoke test_knowledge_recall）
   *  给定全文与候选知识，判定 ②.5 知识召回层是否命中；未配置嵌入服务时 would_hit = null */
  testKnowledgeRecall(
    lang: string,
    text: string,
    candidate: { text: string; error_type: string; correction: string },
  ): Promise<RecallTestResult> {
    if (inTauri)
      return tauri!
        .invoke('test_knowledge_recall', { lang, text, candidate })
        .then((r) => r as RecallTestResult)
    return http
      .post<RecallTestResult>('/gec/knowledge/recall-test', { lang, text, candidate })
      .then((r) => r.data)
  },

  /**
   * GET /gec/history — 分页列出检测历史（桌面端：invoke list_history，同构 { items, total, offset, limit }）
   * q = 全文关键词（服务端字面子串匹配）；offset = 页偏移
   */
  listHistory(params?: {
    session_id?: string
    lang?: string
    limit?: number
    offset?: number
    q?: string
  }): Promise<HistoryListPage> {
    if (inTauri)
      return tauri!
        .invoke('list_history', {
          sessionId: params?.session_id ?? null,
          lang: params?.lang ?? null,
          limit: params?.limit ?? null,
          offset: params?.offset ?? null,
          q: params?.q ?? null,
        })
        .then((r) => {
          const p = r as { items: HistoryDetail[]; total: number; offset: number; limit: number }
          return {
            items: p.items.map(toHistoryListItem),
            total: p.total,
            offset: p.offset,
            limit: p.limit,
          }
        })
    return http.get<HistoryListPage>('/gec/history', { params }).then((r) => r.data)
  },

  /** GET /gec/history/:id — 查看单条详情（桌面端：invoke get_history） */
  getHistory(id: number): Promise<HistoryDetail> {
    if (inTauri) return tauri!.invoke('get_history', { id }).then((r) => r as HistoryDetail)
    return http.get<HistoryDetail>(`/gec/history/${id}`).then((r) => r.data)
  },

  /** DELETE /gec/history/:id — 删除单条（桌面端：invoke remove_history） */
  removeHistory(id: number): Promise<void> {
    if (inTauri) return tauri!.invoke('remove_history', { id }).then(() => undefined)
    return http.delete(`/gec/history/${id}`).then(() => undefined)
  },

  /** DELETE /gec/history — 清空全部（桌面端：invoke clear_history） */
  clearHistory(): Promise<{ deleted: number }> {
    if (inTauri) return tauri!.invoke('clear_history').then((n) => ({ deleted: n as number }))
    return http.delete<{ deleted: number }>('/gec/history').then((r) => r.data)
  },

  /** POST /gec/attribute */
  attribute(body: {
    lang: string
    error_type: string
    confidence: number
    session_error_count: number
    session_meta?: SessionMeta | null
  }): Promise<AttributionResult> {
    return http.post<AttributionResult>('/gec/attribute', body).then((r) => r.data)
  },

  /** POST /gec/analyze — 检测 + 归因一站式（桌面端：invoke analyze）；level 为三级响应级别（1/2/3，缺省 1）；
   *  Web 端支持 signal 取消；level>=2 时超时放宽到 180s */
  analyze(
    lang: string,
    text: string,
    sessionId?: string,
    level?: number,
    opts?: { signal?: AbortSignal },
  ): Promise<AnalyzeResult> {
    if (inTauri)
      return tauri!
        .invoke('analyze', {
          lang,
          text,
          sessionId: sessionId ?? null,
          level: level ?? null,
          forceLlm: false,
        })
        .then((r) => r as AnalyzeResult)
    return http
      .post<AnalyzeResult>(
        '/gec/analyze',
        {
          lang,
          text,
          session_id: sessionId ?? null,
          level: level ?? 1,
        },
        { signal: opts?.signal, timeout: (level ?? 1) >= 2 ? 180_000 : 60_000 },
      )
      .then((r) => r.data)
  },

  /** GET /gec/tables（桌面端：invoke list_tables） */
  listTables(): Promise<DecisionTableMeta[]> {
    if (inTauri) return tauri!.invoke('list_tables').then((r) => r as DecisionTableMeta[])
    return http.get<DecisionTableMeta[]>('/gec/tables').then((r) => r.data)
  },

  /** GET /gec/tables/:lang（桌面端：invoke get_table，仅返回原始 JDM JSON） */
  getTable(lang: string): Promise<DecisionTableView> {
    if (inTauri)
      return tauri!.invoke('get_table', { lang }).then((r) => ({
        meta: { lang, version: '', updated_at: '' },
        raw: r as string,
      }))
    return http.get<DecisionTableView>(`/gec/tables/${lang}`).then((r) => r.data)
  },

  /** PUT /gec/tables/:lang — 热更新（原始 JDM JSON 字符串；桌面端：invoke update_table） */
  putTable(lang: string, raw: string): Promise<DecisionTableMeta> {
    if (inTauri) return tauri!.invoke('update_table', { lang, tableJson: raw }).then((r) => r as DecisionTableMeta)
    return http
      .put<DecisionTableMeta>(`/gec/tables/${lang}`, raw, {
        headers: { 'Content-Type': 'application/json' },
        transformRequest: [(data: unknown) => data],
      })
      .then((r) => r.data)
  },

  /** POST /gec/tables/:lang/validate — dry-run 校验（不替换当前表，返回具体错误） */
  validateTable(lang: string, raw: string): Promise<ValidateResponse> {
    return http
      .post<ValidateResponse>(`/gec/tables/${lang}/validate`, raw, {
        headers: { 'Content-Type': 'application/json' },
        transformRequest: [(data: unknown) => data],
      })
      .then((r) => r.data)
  },

  /** POST /gec/tables/:lang/try — 用给定上下文对当前内存表试算（改行 → 试算 → 确认） */
  tryTable(
    lang: string,
    ctx: {
      error_type: string
      confidence?: number
      session_error_count?: number
      grammar_point?: string | null
    },
  ): Promise<AttributionResult> {
    return http
      .post<AttributionResult>(`/gec/tables/${lang}/try`, {
        lang,
        error_type: ctx.error_type,
        confidence: ctx.confidence ?? 0.9,
        session_error_count: ctx.session_error_count ?? 0,
        session_meta: null,
        grammar_point: ctx.grammar_point ?? null,
      })
      .then((r) => r.data)
  },

  /** GET /gec/tables/:lang/versions — 版本历史（新 → 旧） */
  listTableVersions(lang: string, limit = 50): Promise<TableVersion[]> {
    return http
      .get<TableVersion[]>(`/gec/tables/${lang}/versions`, { params: { limit } })
      .then((r) => r.data)
  },

  /** POST /gec/tables/:lang/rollback — 回滚到指定版本（重新走校验 + 原子替换） */
  rollbackTable(lang: string, version: number): Promise<DecisionTableMeta> {
    return http
      .post<DecisionTableMeta>(`/gec/tables/${lang}/rollback`, { version })
      .then((r) => r.data)
  },

  /** PUT /gec/rules/:lang/toggles — 批量设置规则开关（持久化，重启不丢；桌面端：invoke put_toggles） */
  putRuleToggles(lang: string, toggles: Record<string, boolean>): Promise<PutTogglesResponse> {
    if (inTauri)
      return tauri!.invoke('put_toggles', { lang, toggles }).then((r) => r as PutTogglesResponse)
    return http
      .put<PutTogglesResponse>(`/gec/rules/${lang}/toggles`, { toggles })
      .then((r) => r.data)
  },

  /** GET /gec/rules/:id/examples — 规则示例对分页列表（桌面端：invoke list_rule_examples） */
  listRuleExamples(
    lang: string,
    id: string,
    page = 1,
    pageSize = 50,
  ): Promise<ExampleList> {
    if (inTauri)
      return tauri!
        .invoke('list_rule_examples', { lang, id, page, pageSize })
        .then((r) => r as ExampleList)
    return http
      .get<ExampleList>(`/gec/rules/${encodeURIComponent(id)}/examples`, {
        params: { lang, page, page_size: pageSize },
      })
      .then((r) => r.data)
  },

  /** POST /gec/sessions — 创建会话 */
  createSession(body: { lang: string; upstream_id?: string }): Promise<SessionMeta> {
    return http.post<SessionMeta>('/gec/sessions', body).then((r) => r.data)
  },

  /** GET /gec/sessions/:id — 获取会话详情 */
  getSession(id: string): Promise<SessionRecord> {
    return http.get<SessionRecord>(`/gec/sessions/${id}`).then((r) => r.data)
  },

  /** PUT /gec/sessions/:id/archive — 归档 */
  archiveSession(id: string): Promise<{ session_id: string; status: string }> {
    return http.put<{ session_id: string; status: string }>(`/gec/sessions/${id}/archive`).then((r) => r.data)
  },

  /** PUT /gec/sessions/:id/restore — 恢复 */
  restoreSession(id: string): Promise<{ session_id: string; status: string }> {
    return http.put<{ session_id: string; status: string }>(`/gec/sessions/${id}/restore`).then((r) => r.data)
  },

  /** DELETE /gec/sessions/:id — 删除会话（级联删除历史） */
  deleteSession(id: string): Promise<{ deleted: boolean; session_id: string }> {
    return http.delete<{ deleted: boolean; session_id: string }>(`/gec/sessions/${id}`).then((r) => r.data)
  },

  /** GET /gec/sessions/:id/analysis — 会话习惯性错误分析 */
  getSessionAnalysis(id: string): Promise<SessionAnalysis> {
    return http.get<SessionAnalysis>(`/gec/sessions/${id}/analysis`).then((r) => r.data)
  },

  /** GET /gec/settings — 运行时设置（HanLP 地址 + 离线开关 + 目录配置 + LLM 参数 + LLM 请求模式）
   *  桌面端：invoke('get_settings')（设置文件 > 环境变量 > 内置默认，桌面端默认离线） */
  getSettings(): Promise<AppSettings> {
    if (inTauri) return tauri!.invoke('get_settings').then((r) => r as AppSettings)
    return http.get<AppSettings>('/gec/settings').then((r) => r.data)
  },

  /** PUT /gec/settings — 更新运行时设置（部分更新：仅提供要修改的字段；即时生效 + 持久化）
   *  桌面端：invoke('update_settings')（同上，持久化到本地设置文件） */
  updateSettings(body: UpdateSettingsBody): Promise<AppSettings> {
    if (inTauri) return tauri!.invoke('update_settings', { body }).then((r) => r as AppSettings)
    return http.put<AppSettings>('/gec/settings', body).then((r) => r.data)
  },

  /** POST /gec/settings/test — 测试指定配置项的服务连通性 / 可用性（只读探测，不修改配置）
   *  桌面端：invoke('test_setting')（同构）
   *  target: hanlp / llm / decision / l3 / embedding / tables_dir / lexicons_dir / all */
  testSetting(target: SettingsTestTarget): Promise<SettingsTestResult> {
    if (inTauri) return tauri!.invoke('test_setting', { target }).then((r) => r as SettingsTestResult)
    // 服务不可达时后端按配置的超时返回失败结果（非 HTTP 错误）；axios 超时放宽到 90s 兜底
    return http
      .post<SettingsTestResult>('/gec/settings/test', { target }, { timeout: 90000 })
      .then((r) => r.data)
  },

  /** POST /gec/explain — LLM 个性化讲解
   *  未配置 LLM_API_KEY 时服务端返回 503，此处 resolve(null)，调用方回退静态建议；
   *  桌面端走 invoke('explain')（未配置 LLM 时同样 resolve(null)） */
  async explain(body: {
    lang: string
    error_type: string
    attribution?: string
    cefr?: string
    session_id?: string
    session_error_count?: number
    target_language?: string
  }): Promise<ExplainResponse | null> {
    if (inTauri) return tauriExplain(body)
    try {
      const r = await http.post<ExplainResponse>('/gec/explain', body)
      return r.data
    } catch (e) {
      // 服务端 ApiError::ServiceUnavailable → 503（LLM 未配置）
      if (axios.isAxiosError(e) && e.response?.status === 503) return null
      throw e
    }
  },

  /** POST /gec/explain/stream — LLM 个性化讲解（SSE，逐 token 回调）；未配置时回退静态建议。
   *  桌面端无 SSE 通道：回退为非流式 explain，一次性投递 meta + 全文 */
  async explainStream(
    body: {
      lang: string
      error_type: string
      attribution?: string
      cefr?: string
      session_id?: string
      target_language?: string
    },
    handlers: LlmExplainStreamHandlers,
  ): Promise<boolean | null> {
    if (inTauri) {
      const res = await tauriExplain(body)
      if (res === null) return null
      handlers.onMeta?.({
        model: res.model,
        template: res.template,
        template_version: res.template_version,
      })
      handlers.onToken?.(res.explanation)
      handlers.onDone?.({ explanation: res.explanation, exercises: res.exercises })
      return true
    }
    const res = await fetch('/gec/explain/stream', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
    })
    if (!res.ok) return null
    const reader = res.body?.getReader()
    if (!reader) return null
    const decoder = new TextDecoder()
    let buf = ''
    let event = ''
    for (;;) {
      const { done, value } = await reader.read()
      if (done) break
      buf += decoder.decode(value, { stream: true })
      const lines = buf.split('\n')
      buf = lines.pop() ?? ''
      for (const line of lines) {
        if (line.startsWith('event:')) {
          event = line.slice(6).trim()
        } else if (line.startsWith('data:')) {
          const data = line.slice(5).trim()
          if (!data || event === 'meta') continue
          try {
            const payload = JSON.parse(data)
            if (event === 'token') handlers.onToken?.(payload.token ?? '')
            else if (event === 'done') handlers.onDone?.(payload)
          } catch {
            // 忽略无法解析的 data 行
          }
        }
      }
    }
    return true
  },

  // ─── 学习知识管理（向量召回） ─────────────────────────────────────────────

  /** GET /gec/learned-knowledge?lang= — 列出指定语言的学习知识条目 */
  listLearnedKnowledge(lang: string): Promise<LearnedKnowledge[]> {
    if (inTauri) return tauri!.invoke('list_learned_knowledge', { lang }).then((r) => r as LearnedKnowledge[])
    return http.get<LearnedKnowledge[]>('/gec/learned-knowledge', { params: { lang } }).then((r) => r.data)
  },

  /** POST /gec/learned-knowledge — 添加学习知识条目（自动生成嵌入向量） */
  addLearnedKnowledge(body: {
    lang: string
    text: string
    error_type: string
    correction: string
    source?: string
  }): Promise<{ id: number }> {
    if (inTauri)
      return tauri!
        .invoke('add_learned_knowledge', {
          lang: body.lang,
          text: body.text,
          errorType: body.error_type,
          correction: body.correction,
          source: body.source ?? 'manual',
        })
        .then((r) => ({ id: r as number }))
    return http.post<{ id: number }>('/gec/learned-knowledge', body).then((r) => r.data)
  },

  /** DELETE /gec/learned-knowledge/:id — 删除指定知识条目 */
  removeLearnedKnowledge(id: number): Promise<{ ok: boolean }> {
    if (inTauri) return tauri!.invoke('remove_learned_knowledge', { id }).then(() => ({ ok: true }))
    return http.delete<{ ok: boolean }>(`/gec/learned-knowledge/${id}`).then((r) => r.data)
  },

  // ─── L3 特征模型训练飞轮（三期闭环：确认样本汇入 → 积累提醒 → 重训练） ───

  /** POST /gec/l3/collect — 采集一条飞轮样本（采纳 LLM 错误 = 正样本；干净句 = 负样本）
   *  非阻断：失败仅 console.warn，不抛错 */
  async l3Collect(body: {
    lang: string
    text: string
    label?: number
    error_type?: string
    source?: string
  }): Promise<{ added: boolean; total: number } | null> {
    try {
      if (inTauri)
        return (await tauri!.invoke('l3_collect', {
          lang: body.lang,
          text: body.text,
          label: body.label ?? 1,
          errorType: body.error_type ?? null,
          source: body.source ?? null,
        })) as { added: boolean; total: number }
      return await http.post<{ added: boolean; total: number }>('/gec/l3/collect', body).then((r) => r.data)
    } catch (e) {
      console.warn('[l3-flywheel] 样本采集失败（不影响主流程）', e)
      return null
    }
  },

  /** GET /gec/l3/status?lang= — 飞轮状态（样本数 + 上次重训练 + 当前模型 F1） */
  l3Status(lang: string): Promise<L3FlywheelStatus> {
    if (inTauri)
      return tauri!.invoke('l3_status', { lang }).then((r) => r as L3FlywheelStatus)
    return http.get<L3FlywheelStatus>('/gec/l3/status', { params: { lang } }).then((r) => r.data)
  },

  /** POST /gec/l3/retrain — 触发重训练（合并样本 → 重训练 → dev 评测 → 热切换） */
  l3Retrain(lang: string): Promise<L3RetrainResult> {
    if (inTauri)
      return tauri!.invoke('l3_retrain', { lang }).then((r) => r as L3RetrainResult)
    return http.post<L3RetrainResult>('/gec/l3/retrain', { lang }).then((r) => r.data)
  },

  /** POST /gec/feedback — 检测结果统一采纳 / 放弃（全部来源层）（桌面端：invoke error_feedback）
   *  采纳：llm → 规则蒸馏 / 知识 / anomaly；rule → 确认 + 示例补强；模型层 → 蒸馏 + L3 正样本；
   *  放弃：白名单 + 反馈留痕 + 反思报告（诊断 + 可执行建议）+ 模型层 L3 负样本 */
  errorFeedback(
    lang: string,
    error: ErrorItem,
    action: 'adopt' | 'reject',
    reason?: string,
    ctx?: { src_text: string; corrected_text: string },
  ): Promise<FeedbackResponse> {
    if (inTauri)
      return tauri!
        .invoke('error_feedback', {
          lang,
          error,
          action,
          reason: reason ?? null,
          ctx: ctx ?? null,
        })
        .then((r) => r as FeedbackResponse)
    return http
      .post<FeedbackResponse>('/gec/feedback', {
        lang,
        error,
        action,
        reason: reason ?? null,
        ...(ctx ?? {}),
      })
      .then((r) => r.data)
  },

  /** POST /gec/feedback/action — 执行反思建议（停用规则 / 删示例 / 删知识 / L3 负样本）（桌面端：invoke feedback_action） */
  feedbackAction(
    action: string,
    payload: Record<string, unknown>,
  ): Promise<{ ok: boolean; message: string }> {
    if (inTauri)
      return tauri!
        .invoke('feedback_action', { action, payload })
        .then((r) => r as { ok: boolean; message: string })
    return http
      .post<{ ok: boolean; message: string }>('/gec/feedback/action', { action, payload })
      .then((r) => r.data)
  },

  /** POST /gec/feedback/reflect-llm — LLM 深度反思（{lang}_reflect 模板；与确定性反思分离）（桌面端：invoke reflect_llm） */
  reflectLlm(lang: string, error: ErrorItem, reason?: string): Promise<ReflectLlmResult> {
    if (inTauri)
      return tauri!
        .invoke('reflect_llm', { lang, error, reason: reason ?? null })
        .then((r) => r as ReflectLlmResult)
    return http
      .post<ReflectLlmResult>('/gec/feedback/reflect-llm', { lang, error, reason: reason ?? null })
      .then((r) => r.data)
  },

  /** GET /gec/feedback/history — 反馈历史（按规则/层聚合统计 + 最近明细）（桌面端：invoke feedback_history） */
  feedbackHistory(lang: string, limit = 50): Promise<FeedbackHistory> {
    if (inTauri)
      return tauri!.invoke('feedback_history', { lang, limit }).then((r) => r as FeedbackHistory)
    return http
      .get<FeedbackHistory>('/gec/feedback/history', { params: { lang, limit } })
      .then((r) => r.data)
  },

  // ─── 测评中心（进化中心 P0：层级归因 + 消融 + A/B + 漏斗） ───

  /** POST /gec/eval/run — 发起评测任务（返回 job_id；桌面端：invoke run_eval） */
  runEval(body: EvalRunRequest): Promise<{ job_id: string; status: string }> {
    if (inTauri)
      return tauri!.invoke('run_eval', { body }).then((r) => r as { job_id: string; status: string })
    return http.post<{ job_id: string; status: string }>('/gec/eval/run', body).then((r) => r.data)
  },

  /** GET /gec/eval/job/:id — 评测任务进度（轮询；桌面端：invoke eval_job） */
  evalJob(id: string): Promise<EvalJob> {
    if (inTauri) return tauri!.invoke('eval_job', { id }).then((r) => r as EvalJob)
    return http.get<EvalJob>(`/gec/eval/job/${id}`).then((r) => r.data)
  },

  /** GET /gec/eval/reports — 历史报告列表（桌面端：invoke list_eval_reports） */
  listEvalReports(): Promise<{ reports: EvalReportSummary[] }> {
    if (inTauri)
      return tauri!.invoke('list_eval_reports').then((r) => r as { reports: EvalReportSummary[] })
    return http.get<{ reports: EvalReportSummary[] }>('/gec/eval/reports').then((r) => r.data)
  },

  /** GET /gec/eval/report/:id — 报告详情（桌面端：invoke get_eval_report） */
  getEvalReport(id: string): Promise<EvalReport> {
    if (inTauri) return tauri!.invoke('get_eval_report', { id }).then((r) => r as EvalReport)
    return http.get<EvalReport>(`/gec/eval/report/${id}`).then((r) => r.data)
  },

  // ─────────────────────────── 反思中心（P1） ───────────────────────────

  /** POST /gec/reflect/run — 生成反思报告（反馈聚合 + 测评证据 → 报告 + 建议队列） */
  runReflect(body: ReflectRunRequest): Promise<ReflectRunResponse> {
    if (inTauri) return tauri!.invoke('run_reflect', { body }).then((r) => r as ReflectRunResponse)
    return http.post<ReflectRunResponse>('/gec/reflect/run', body).then((r) => r.data)
  },

  /** GET /gec/reflect/reports — 反思报告历史（桌面端：invoke list_reflect_reports） */
  listReflectReports(lang?: string, limit = 20): Promise<{ reports: ReflectionReportSummary[] }> {
    if (inTauri)
      return tauri!
        .invoke('list_reflect_reports', { lang, limit })
        .then((r) => r as { reports: ReflectionReportSummary[] })
    const q = new URLSearchParams()
    if (lang) q.set('lang', lang)
    q.set('limit', String(limit))
    return http
      .get<{ reports: ReflectionReportSummary[] }>(`/gec/reflect/reports?${q.toString()}`)
      .then((r) => r.data)
  },

  /** GET /gec/reflect/report/:id — 报告详情（桌面端：invoke get_reflect_report） */
  getReflectReport(id: number): Promise<ReflectionReportFull> {
    if (inTauri) return tauri!.invoke('get_reflect_report', { id }).then((r) => r as ReflectionReportFull)
    return http.get<ReflectionReportFull>(`/gec/reflect/report/${id}`).then((r) => r.data)
  },

  /** GET /gec/reflect/suggestions — 建议队列（桌面端：invoke list_reflect_suggestions） */
  listReflectSuggestions(
    status?: string,
    lang?: string,
    limit = 50,
  ): Promise<{ suggestions: ReflectionSuggestionRow[] }> {
    if (inTauri)
      return tauri!
        .invoke('list_reflect_suggestions', { status, lang, limit })
        .then((r) => r as { suggestions: ReflectionSuggestionRow[] })
    const q = new URLSearchParams()
    if (status) q.set('status', status)
    if (lang) q.set('lang', lang)
    q.set('limit', String(limit))
    return http
      .get<{ suggestions: ReflectionSuggestionRow[] }>(`/gec/reflect/suggestions?${q.toString()}`)
      .then((r) => r.data)
  },

  /** PUT /gec/reflect/suggestion/:id — 更新建议状态（P2 消费入口） */
  updateReflectSuggestion(id: number, status: string): Promise<{ id: number; status: string }> {
    if (inTauri)
      return tauri!
        .invoke('update_reflect_suggestion', { id, status })
        .then((r) => r as { id: number; status: string })
    return http
      .put<{ id: number; status: string }>(`/gec/reflect/suggestion/${id}`, { status })
      .then((r) => r.data)
  },

  // ─────────────────────────── 训练中心（P2） ───────────────────────────

  /** POST /gec/train/pool — 领域池构建（模型层反馈 + 飞轮样本 + 增强器） */
  trainBuildPool(body: { lang: string; clean_sentences?: string[] }): Promise<TrainPoolResponse> {
    if (inTauri) return tauri!.invoke('train_build_pool', { body }).then((r) => r as TrainPoolResponse)
    return http.post<TrainPoolResponse>('/gec/train/pool', body).then((r) => r.data)
  },

  /** POST /gec/train/llm-data — LLM 训练数据构建（SFT JSONL） */
  trainBuildLlmData(body: { lang: string; arm?: string }): Promise<TrainLlmDataResponse> {
    if (inTauri)
      return tauri!.invoke('train_build_llm_data', { body }).then((r) => r as TrainLlmDataResponse)
    return http.post<TrainLlmDataResponse>('/gec/train/llm-data', body).then((r) => r.data)
  },

  /** POST /gec/train/l3/retrain — L3 重训练（feature-model 飞轮） */
  trainRunL3Retrain(body: { lang: string }): Promise<TrainL3RetrainResponse> {
    if (inTauri)
      return tauri!.invoke('train_run_l3_retrain', { body }).then((r) => r as TrainL3RetrainResponse)
    return http.post<TrainL3RetrainResponse>('/gec/train/l3/retrain', body).then((r) => r.data)
  },

  /** POST /gec/train/lexicon/apply — 词表条件化入库（条件化排除条目） */
  trainApplyLexicon(
    body: {
      lang: string
      rule_id?: string
      words?: string[]
      suggestion_id?: number
      check_ablation?: boolean
    },
  ): Promise<TrainLexiconApplyResponse> {
    if (inTauri)
      return tauri!.invoke('train_apply_lexicon', { body }).then((r) => r as TrainLexiconApplyResponse)
    return http.post<TrainLexiconApplyResponse>('/gec/train/lexicon/apply', body).then((r) => r.data)
  },

  /** POST /gec/train/suggestion/:id/run — P1 → P2 消费入口（建议 → 训练作业） */
  trainRunSuggestion(suggestionId: number): Promise<TrainRunSuggestionResponse> {
    if (inTauri)
      return tauri!
        .invoke('train_run_suggestion', { body: { suggestion_id: suggestionId } })
        .then((r) => r as TrainRunSuggestionResponse)
    return http
      .post<TrainRunSuggestionResponse>(`/gec/train/suggestion/${suggestionId}/run`, {})
      .then((r) => r.data)
  },

  /** GET /gec/train/jobs — 训练作业列表（新 → 旧） */
  listTrainingJobs(
    lang?: string,
    status?: string,
    limit = 50,
  ): Promise<{ jobs: TrainingJobRow[] }> {
    if (inTauri)
      return tauri!
        .invoke('train_list_jobs', { lang, status, limit })
        .then((r) => r as { jobs: TrainingJobRow[] })
    const q = new URLSearchParams()
    if (lang) q.set('lang', lang)
    if (status) q.set('status', status)
    q.set('limit', String(limit))
    return http
      .get<{ jobs: TrainingJobRow[] }>(`/gec/train/jobs?${q.toString()}`)
      .then((r) => r.data)
  },

  /** GET /gec/train/job/:id — 训练作业详情 */
  getTrainingJob(id: number): Promise<{ job: TrainingJobRow }> {
    if (inTauri) return tauri!.invoke('train_get_job', { id }).then((r) => r as { job: TrainingJobRow })
    return http.get<{ job: TrainingJobRow }>(`/gec/train/job/${id}`).then((r) => r.data)
  },
  /** POST /gec/eval/contributions — 层级贡献测量（消融 Δ 常态化；桌面端：invoke eval_contributions） */
  evalContributions(body: {
    benchmark?: string | null
    split?: string | null
    arm?: string | null
    mode?: string | null
  }): Promise<ContributionResponse> {
    if (inTauri) return tauri!.invoke('eval_contributions', { body }).then((r) => r as ContributionResponse)
    return http.post<ContributionResponse>('/gec/eval/contributions', body).then((r) => r.data)
  },

  /** POST /gec/round/evaluate — 轮次自动评估（提案 或 「无需行动」）（桌面端：invoke round_evaluate） */
  roundEvaluate(body: { lang: string; report_id?: string | null }): Promise<RoundEvaluateResponse> {
    if (inTauri) return tauri!.invoke('round_evaluate', { body }).then((r) => r as RoundEvaluateResponse)
    return http.post<RoundEvaluateResponse>('/gec/round/evaluate', body).then((r) => r.data)
  },

  /** GET /gec/rounds — 轮次列表（新 → 旧）（桌面端：invoke round_list） */
  roundList(lang?: string, status?: string, limit = 50): Promise<{ rounds: OptimizationRound[] }> {
    if (inTauri)
      return tauri!.invoke('round_list', { lang, status, limit }).then((r) => r as { rounds: OptimizationRound[] })
    const q = new URLSearchParams()
    if (lang) q.set('lang', lang)
    if (status) q.set('status', status)
    q.set('limit', String(limit))
    return http.get<{ rounds: OptimizationRound[] }>(`/gec/rounds?${q.toString()}`).then((r) => r.data)
  },

  /** GET /gec/round/:id — 轮次详情（桌面端：invoke round_get） */
  roundGet(id: number): Promise<{ round: OptimizationRound }> {
    if (inTauri) return tauri!.invoke('round_get', { id }).then((r) => r as { round: OptimizationRound })
    return http.get<{ round: OptimizationRound }>(`/gec/round/${id}`).then((r) => r.data)
  },

  /** POST /gec/round/:id/confirm — 人工确认门（proposed → confirmed）（桌面端：invoke round_confirm） */
  roundConfirm(id: number): Promise<{ round: OptimizationRound }> {
    if (inTauri) return tauri!.invoke('round_confirm', { id }).then((r) => r as { round: OptimizationRound })
    return http.post<{ round: OptimizationRound }>(`/gec/round/${id}/confirm`, {}).then((r) => r.data)
  },

  /** POST /gec/round/:id/reject — 人工拒绝（proposed → rejected）（桌面端：invoke round_reject） */
  roundReject(id: number): Promise<{ round: OptimizationRound }> {
    if (inTauri) return tauri!.invoke('round_reject', { id }).then((r) => r as { round: OptimizationRound })
    return http.post<{ round: OptimizationRound }>(`/gec/round/${id}/reject`, {}).then((r) => r.data)
  },

  /** POST /gec/round/:id/execute — 执行轮次动作（confirmed → running → 终态）（桌面端：invoke round_execute） */
  roundExecute(id: number): Promise<{ round: OptimizationRound }> {
    if (inTauri) return tauri!.invoke('round_execute', { id }).then((r) => r as { round: OptimizationRound })
    return http.post<{ round: OptimizationRound }>(`/gec/round/${id}/execute`, {}).then((r) => r.data)
  },
  /** 轮次报告（聚合：背景 + 各动作完整作业结果 + 执行后测评证据） */
  roundReport(id: number): Promise<RoundReport> {
    if (inTauri) return tauri!.invoke('round_report', { id }).then((r) => r as RoundReport)
    return http.get<RoundReport>(`/gec/round/${id}/report`).then((r) => r.data)
  },

  // ── 载体（图片）审校：MULTIMODAL.md §7 面板 / §8 设置 ──────────────────────
  // 链路只有一份（`media-flow` crate）：Web 端打 api-server 的 HTTP，桌面端 invoke
  // 同一套编排 + 本地检测级联。字节流差异：HTTP 走 octet-stream，invoke 只能传
  // JSON → 上传用 base64，预览 / 导出产物回传 `{ content_type, data_b64 }`。

  /** POST /gec/media/probes — 各家工具 + 视觉档就绪探测（只读，不写文件） */
  mediaProbes(): Promise<MediaProbes> {
    if (inTauri) return tauri!.invoke('media_probes').then((r) => r as MediaProbes)
    return http.post<MediaProbes>('/gec/media/probes').then((r) => r.data)
  },

  /** POST /gec/media/inspect?filename= — 上传前能力探测（尺寸 / 文字层 / 可编辑性） */
  mediaInspect(filename: string, data: Uint8Array): Promise<MediaInspectResult> {
    if (inTauri)
      return tauri!
        .invoke('media_inspect', { filename, dataB64: bytesToB64(data) })
        .then((r) => r as MediaInspectResult)
    return http
      .post<MediaInspectResult>('/gec/media/inspect', data, {
        params: { filename },
        headers: { 'Content-Type': 'application/octet-stream' },
      })
      .then((r) => r.data)
  },

  /** POST /gec/media/jobs?filename= — 建 job 并后台跑全链路（副本隔离，**永不改原件**）；
   *  返回 job 供轮询。视觉档上传前压缩（`vision_max_edge` / `vision_max_bytes`），
   *  但整页转写仍受网关排队影响 → 上传本身给 3 分钟 */
  mediaCreateJob(body: {
    filename: string
    data: Uint8Array
    apply_mode?: MediaApplyMode
    lang?: string
    level?: number
    force_llm?: boolean
    category?: string
    /** 工作模式（§15.1）：缺省走设置里的 `media.mode`；不可用时后端报错并给出退路，不会静默换档 */
    mode?: MediaMode
  }): Promise<MediaJob> {
    if (inTauri)
      return tauri!
        .invoke('media_create_job', {
          filename: body.filename,
          dataB64: bytesToB64(body.data),
          applyMode: body.apply_mode ?? null,
          lang: body.lang ?? null,
          level: body.level ?? null,
          forceLlm: body.force_llm ?? null,
          category: body.category ?? null,
          mode: body.mode ?? null,
        })
        .then((r) => r as MediaJob)
    return http
      .post<MediaJob>('/gec/media/jobs', body.data, {
        params: {
          filename: body.filename,
          ...(body.apply_mode ? { apply_mode: body.apply_mode } : {}),
          ...(body.lang ? { lang: body.lang } : {}),
          ...(body.level !== undefined ? { level: body.level } : {}),
          ...(body.force_llm !== undefined ? { force_llm: body.force_llm } : {}),
          ...(body.category ? { category: body.category } : {}),
          ...(body.mode ? { mode: body.mode } : {}),
        },
        headers: { 'Content-Type': 'application/octet-stream' },
        timeout: 180000,
      })
      .then((r) => r.data)
  },

  /** GET /gec/media/jobs — job 列表（创建时间倒序） */
  mediaJobs(): Promise<MediaJob[]> {
    if (inTauri) return tauri!.invoke('media_jobs').then((r) => r as MediaJob[])
    return http.get<MediaJob[]>('/gec/media/jobs').then((r) => r.data)
  },

  /** GET /gec/media/jobs/:id — 状态轮询（stage 到终态即停） */
  mediaJob(id: string): Promise<MediaJob> {
    if (inTauri) return tauri!.invoke('media_job', { id }).then((r) => r as MediaJob)
    return http.get<MediaJob>(`/gec/media/jobs/${encodeURIComponent(id)}`).then((r) => r.data)
  },

  /** POST /gec/media/jobs/:id/apply — 回写（默认 Overlay，幂等：重跑先清历史 mark-* 层） */
  mediaApply(id: string, applyMode?: MediaApplyMode): Promise<MediaJob> {
    if (inTauri)
      return tauri!
        .invoke('media_apply', { id, applyMode: applyMode ?? null })
        .then((r) => r as MediaJob)
    return http
      .post<MediaJob>(`/gec/media/jobs/${encodeURIComponent(id)}/apply`, applyMode ? { apply_mode: applyMode } : {}, {
        timeout: 180000,
      })
      .then((r) => r.data)
  },

  /** 预览图地址（`result.png`，无则回退副本原件）——给 `<img>` 直接用 */
  mediaPreviewUrl(id: string): string {
    return `/gec/media/jobs/${encodeURIComponent(id)}/preview`
  },

  /** 预览字节（blob）：回退原件时后端给的是 `application/octet-stream`，
   *  直接塞 `<img>` 会被浏览器按类型拒掉 → 取字节自建 objectURL，顺便可下载。
   *  桌面端：invoke media_preview 回传 `{ content_type, data_b64 }` → 还原成 Blob */
  async mediaPreviewBlob(id: string): Promise<Blob> {
    if (inTauri) {
      const p = (await tauri!.invoke('media_preview', { id })) as BinaryPayload
      return b64ToBlob(p)
    }
    try {
      const resp = await http.get<Blob>(`/gec/media/jobs/${encodeURIComponent(id)}/preview`, {
        responseType: 'blob',
        timeout: 120000,
      })
      return resp.data as Blob
    } catch (e) {
      throw await normalizeBlobError(e)
    }
  },

  /** POST /gec/media/report — 勾选后的结果汇成正误表（JSON / Markdown / HTML / docx） */
  mediaReport(body: MediaReportBody): Promise<MediaReportResult> {
    if (inTauri)
      return tauri!.invoke('media_report', { body }).then((r) => r as MediaReportResult)
    return http
      .post<MediaReportResult>('/gec/media/report', body, { timeout: 180000 })
      .then((r) => r.data)
  },

  /** 导出产物下载地址（format = json | md | html | docx）——仅 Web 端可用 */
  mediaReportUrl(id: string, format: 'json' | 'md' | 'html' | 'docx'): string {
    return `/gec/media/reports/${encodeURIComponent(id)}?format=${format}`
  },

  /** 导出产物落盘：Web 端 fetch + 浏览器下载；桌面端弹原生「另存为」后写文件
   *  （与资源包导出同一口径）。取消返回 null */
  async downloadMediaReport(
    reportId: string,
    format: 'json' | 'md' | 'html' | 'docx',
    filename: string,
  ): Promise<PackExportResult | null> {
    if (inTauri) {
      const ext = format === 'md' ? 'md' : format
      const target = (await tauri!
        .invoke('plugin:dialog|save', {
          options: {
            title: `导出正误表（${format}）`,
            defaultPath: filename,
            filters: [{ name: `正误表 ${format}`, extensions: [ext] }],
          },
        })
        .catch(() => null)) as string | null
      if (!target) return null
      const r = (await tauri!.invoke('media_report_file', {
        reportId,
        format,
        path: target,
      })) as BinaryPayload
      return { filename: target, size_bytes: Number(r.size_bytes ?? 0) }
    }
    const resp = await http.get<Blob>(this.mediaReportUrl(reportId, format), {
      responseType: 'blob',
      timeout: 180000,
    })
    const url = URL.createObjectURL(resp.data as Blob)
    const a = document.createElement('a')
    a.href = url
    a.download = filename
    a.click()
    URL.revokeObjectURL(url)
    return { filename, size_bytes: (resp.data as Blob).size }
  },
}

