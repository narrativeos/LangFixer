// 与 shared-model 对齐的前端类型

export interface SuggestionItem {
  text: string
  score: number
}

export interface ErrorItem {
  lang: string
  error_type: string
  confidence: number
  span: [number, number]
  original: string
  correction: string | null
  message: string
  /** 规则分类（spelling / grammar / typography / style） */
  category?: string
  /** 规则唯一标识（词表 rules[].id） */
  rule_id?: string
  /** 多个候选修正（按 score 降序） */
  suggestions?: SuggestionItem[]
  /** W3C ITS 2.0 问题类型 */
  issue_type?: string
  /** 错误来源引擎（rule / statistical / decision / knowledge / feature / llm）；
   *  LLM 兜底层产出标记为 "llm"，L3 特征模型产出标记为 "feature" */
  source?: string
  /** 除 `source`（最终产出层）外，其他也独立命中过同一错误的层
   *  （多层共识 = 更强的可信信号，前端展示「多层共识」徽章） */
  also_found_by?: string[]
}

/** 层级贡献报告（与 gec-core `LayerReport` 对齐；本次检测实际参与的每层一条） */
export interface LayerReport {
  /** 层名：rule / decision / knowledge / feature / llm / statistical */
  layer: string
  /** 层状态：ran（执行并合并）/ decided（特征模型定案）/ abstained（弃权）/
   *  unavailable（服务失败已回退）/ skipped（被评估但未执行，如 LLM 未触发） */
  status: string
  /** 该层检出的错误条数 */
  found: number
}

/** 检测各阶段计时（毫秒；与 gec-core `DetectTimings` 对齐）
 *  ①/②/③-F 并行执行，各阶段之和 ≠ total_ms（total 为整链 wall time） */
export interface DetectTimings {
  /** ① 规则检测（NLP 服务调用 + 本地规则匹配） */
  rule_ms: number
  /** ① 中 NLP 服务调用耗时（null = 纯本地检测） */
  nlp_ms?: number | null
  /** NLP 预热（冷启动）耗时：仅启动 / 配置重建后首次 NLP 调用非空 */
  nlp_warmup_ms?: number | null
  /** ② 决策模型 */
  decision_ms?: number | null
  /** ③-F L3 特征模型（仅 level 3 且非 force_llm） */
  l3_ms?: number | null
  /** ②.5 知识向量召回 */
  recall_ms?: number | null
  /** ③ LLM 兜底 */
  llm_ms?: number | null
  /** 本次检测总耗时 */
  total_ms: number
}

/** 检测响应（API / Tauri 返回；含决策模型弃权信号 + 各阶段计时） */
export interface DetectResponse {
  errors: ErrorItem[]
  /** 决策模型是否弃权（仅决策层启用时有意义） */
  decision_abstained?: boolean
  /** 各阶段计时（缓存命中等未实际检测时不返回） */
  timings?: DetectTimings
  /** 本次结果来自历史复用（同文本 + 参数一致 + 无 LLM 的近期记录） */
  from_history?: boolean
  /** LLM 兜底层实际执行且未检出错误（「LLM 复查确认干净」） */
  llm_confirmed_clean?: boolean
  /** 层级贡献摘要（前端「检测路径」；未实际检测时不返回） */
  layers?: LayerReport[]
}

/** 检测 + 归因一站式响应（含决策模型弃权信号 + 各阶段计时） */
export interface AnalyzeResult {
  lang: string
  errors: AnalyzedError[]
  session_id: string
  /** 决策模型是否弃权 */
  decision_abstained?: boolean
  /** 各阶段计时 */
  timings?: DetectTimings
  /** 本次结果来自历史复用（同文本 + 参数一致 + 无 LLM 的近期记录） */
  from_history?: boolean
  /** LLM 兜底层实际执行且未检出错误（「LLM 复查确认干净」） */
  llm_confirmed_clean?: boolean
  /** 层级贡献摘要（前端「检测路径」；未实际检测时不返回） */
  layers?: LayerReport[]
}

export interface RuleInfo {
  id: string
  category: string
  priority: number
  enabled: boolean
  description: string
  /** 该规则示例对总数（示例对经 /gec/rules/:id/examples 按需分页加载） */
  example_count: number
  /** 来源（manual / llm_distill） */
  source: string
}

/** 规则示例对（wrong → right） */
export interface RuleExample {
  id: number
  lang: string
  rule_id: string
  wrong: string
  right: string
  source: string
  created_at: string
}

/** GET /gec/rules/:id/examples 分页响应 */
export interface ExampleList {
  lang: string
  rule_id: string
  total: number
  page: number
  page_size: number
  examples: RuleExample[]
}

export interface CategoryInfo {
  id: string
  name: string
  rule_count: number
}

export interface RulesResponse {
  lang: string
  /** 满足过滤条件（category / q）的规则总数 */
  total?: number
  page?: number
  page_size?: number
  categories: CategoryInfo[]
  rules: RuleInfo[]
}

export interface ToggleResponse {
  id: string
  enabled: boolean
}

export interface CustomWord {
  id: number
  lang: string
  word: string
  reason: string
  created_at: string
}

export interface ListWordsResponse {
  lang: string
  words: CustomWord[]
}

// ─── 检测历史 ───

export interface HistoryListItem {
  id: number
  session_id: string | null
  lang: string
  text_preview: string
  error_count: number
  /** 检测总耗时（毫秒；无计时记录时为 null） */
  total_ms?: number | null
  /** 产生该记录时的程序版本号（旧记录无版本时为 null） */
  version?: string | null
  /** 检测时的参数快照（JSON 序列化的 DetectParams；旧记录为 null = 不可复用） */
  params_json?: string | null
  created_at: string
}

/** GET /gec/history 分页响应（桌面端 invoke list_history 同构） */
export interface HistoryListPage {
  items: HistoryListItem[]
  /** 同过滤口径（lang / session / q）的总条数 */
  total: number
  offset: number
  limit: number
}

export interface HistoryDetail {
  id: number
  session_id: string | null
  lang: string
  text: string
  errors_json: string
  analyzed_json: string | null
  /** JSON 序列化的各阶段计时（DetectTimings；无计时记录时为 null） */
  timings_json?: string | null
  /** JSON 序列化的层级贡献摘要（LayerReport[]；旧记录为 null） */
  layers_json?: string | null
  /** 产生该记录时的程序版本号（旧记录无版本时为 null） */
  version?: string | null
  /** 检测时的参数快照（JSON 序列化的 DetectParams；旧记录为 null） */
  params_json?: string | null
  created_at: string
}

export interface WeinerDimensions {
  locus: string
  stability: string
  controllability: string
}

export interface AttributionTrace {
  matched_rule: string
  decision_table: string
}

export interface AttributionResult {
  attribution: string
  weiner: WeinerDimensions
  remedial: string[]
  trace: AttributionTrace
  /** CEFR 难度等级（A1-C2；决策表未提供时为 'unknown'） */
  cefr?: string
  /** 知识点来源（旧版决策表 source 输出；新表不再内嵌，改用下方 sources；保留以兼容历史数据） */
  source?: string
  /** 知识源引用列表（知识库按知识点富化；同一规则/点可被多个知识源背书） */
  sources?: KnowledgeSourceRef[]
  /** 典型"错误→改正"实例（决策表 examples 输出，或知识库按知识点富化） */
  examples?: string[]
}

// ─── 知识库（知识源注册表 + 知识点集合；与决策表分离） ───

/** 知识源引用：知识点 → 知识源的关联条目（1:N） */
export interface KnowledgeSourceRef {
  source_id: string
  /** 源内位置（如 "第一讲 冠词（Article） · 一.1 定冠词的使用"） */
  location?: string
  /** 源标题（由知识库从注册表解析回填；未知源回退 source_id） */
  title?: string
}

/** 知识源（注册表条目：仅元数据，不登记正文内容） */
export interface KnowledgeSource {
  source_id: string
  title: string
  author?: string
  edition?: string
  lang: string
  /** 许可范围：internal_test（仅内部测试）/ licensed / public */
  license_scope?: string
  /** 关联知识点数（服务端统计） */
  point_count?: number
}

/** "错误 → 改正"对 */
export interface CorrectionPair {
  wrong: string
  right: string
}

/** 知识点：语言内稳定 ID，关联 1..N 个知识源 */
export interface KnowledgePoint {
  point_id: string
  lang: string
  error_type: string
  title: string
  section?: string
  cefr?: string
  remedial?: string
  examples?: string[]
  corrections?: CorrectionPair[]
  sources?: KnowledgeSourceRef[]
}

// ---------------------------------------------------------------------------
// 知识源资源包（盘点 / 打包导出 / 导入 / 删除；一个知识源 = 一个资源包单元）
// ---------------------------------------------------------------------------

/** 盘点：某语言下该源的知识数据统计 */
export interface PackLangStats {
  lang: string
  total_points: number
  /** 该源背书的点数 */
  backed_points: number
  /** 仅该源背书（删除资源包时整体删除）的点数 */
  exclusive_points: number
  /** 还被其他源背书（删除时只剥离引用）的点数 */
  shared_points: number
}

/** 盘点：该源在决策表中标记（_meta 溯源）的规则行统计 */
export interface PackRuleStats {
  lang: string
  total_rules: number
  tagged_rules: number
}

/** 盘点：未发布导入会话摘要 */
export interface PackStagingBrief {
  status: string
  chunks_total: number
  chunks_done: number
  staged_points: number
}

/** 资源包盘点（删除前确认预览 + 导出组成清单） */
export interface PackSummary {
  source: KnowledgeSource
  langs: PackLangStats[]
  total_points: number
  rules: PackRuleStats[]
  aux_points_present: boolean
  aux_index_present: boolean
  staging?: PackStagingBrief | null
}

/** 资源包组件：某语言归因决策表整表快照（JDM，规则行带 _id/_meta 溯源） */
export interface PackDecisionTable {
  lang: string
  jdm: unknown
}

/** 知识源资源包（自描述单文件归档） */
export interface ResourcePack {
  format: string
  format_version: number
  exported_at: string
  checksum: string
  source: KnowledgeSource
  components: {
    knowledge_points: KnowledgePoint[]
    staging?: unknown
    /** 含该源标记规则行的各语言决策表快照（每语言一项） */
    decision_tables?: PackDecisionTable[]
    /** 该源条目（entry.source_id 归属标记过滤后的决策数据） */
    aux_points?: unknown
    aux_index?: unknown
  }
}

/** 导入报告（知识数据 + 规则 + 决策数据 + 草稿恢复） */
export interface PackImportResponse {
  knowledge: {
    source_id: string
    source_added: boolean
    points_endorsed: number
    points_created: number
    points_skipped: number
  }
  table_installed: boolean
  rules_replaced: number
  rules_added: number
  aux_written: number
  staging_restored: boolean
}

/** 删除报告（知识数据 + 规则摘除 + 决策数据 + 草稿） */
export interface PackCleanResponse {
  knowledge: {
    source_id: string
    source_removed: boolean
    refs_detached: number
    points_removed: number
    orphans_kept: number
  }
  rules_removed: number
  aux_removed: number
  staging_removed: boolean
}

/** 导出结果：zip 压缩包（浏览器直接下载 / 桌面端写盘返回保存地址） */
export interface PackExportResult {
  filename: string
  size_bytes: number
  /** 桌面端：zip 写盘的完整文件地址 */
  file_path?: string
  points?: number
  has_table?: boolean
  has_aux?: boolean
  has_staging?: boolean
}

// ---------------------------------------------------------------------------
// 知识源导入管线（一键导入：挖掘 → 草稿区 → 归并预览 → 发布）
// ---------------------------------------------------------------------------

/** 导入会话状态：running 挖掘中 / mined 挖掘完成待审 / failed 熔断或中断 */
export type IngestStatus = 'running' | 'mined' | 'failed'

/** 导入会话摘要（列表展示） */
export interface IngestSessionSummary {
  source_id: string
  title: string
  lang: string
  chunks_total: number
  chunks_done: number
  chunks_failed: number
  staged_points: number
  error_count: number
  created_at: string
  status: IngestStatus
  /** 规则草稿数（0 = 未派生） */
  rule_drafts?: number
  /** 规则派生状态：'' 未派生 / running / mined / failed */
  rules_status?: string
}

/** 抽取草稿点（agent 产物，尚未分配 point_id） */
export interface DraftPoint {
  error_type: string
  title: string
  section?: string
  remedial?: string
  cefr?: string
  examples?: string[]
  corrections?: CorrectionPair[]
  location?: string
  /** agent 指定的既有匹配点（如 article.3）：存在则只追加背书 */
  match_point_id?: string
}

/** 导入会话详情 */
export interface IngestSessionDetail {
  source: KnowledgeSource
  chunks_total: number
  chunks_done: number
  chunks_failed: number
  created_at: string
  drafts: { seq: number; draft: DraftPoint; staged_at: string }[]
  errors: string[]
  status: IngestStatus
  /** 归因规则草稿（决策表候选） */
  rule_drafts: { seq: number; draft: DraftRule; staged_at: string }[]
  /** 规则派生状态：'' 未派生 / running / mined / failed */
  rules_status: string
  rules_total: number
  rules_done: number
  rules_errors: string[]
}

/** 归因规则草稿（语义级；JDM 行由宿主生成） */
export interface DraftRule {
  error_type: string
  novice?: boolean | null
  min_confidence?: number | null
  attribution: string
  weiner_locus?: string
  weiner_stability?: string
  weiner_controllability?: string
  cefr?: string
  remedial: string
}

/** 规则归并计划行（new 新增 / replace 本源替换 / conflict 跳过） */
export interface RulePlanRow {
  rule_id: string
  action: 'new' | 'replace' | 'conflict'
  row: Record<string, unknown>
}

/** 规则归并计划（发布预览） */
export interface RulePlan {
  lang: string
  rows: RulePlanRow[]
  new: number
  replaced: number
  conflicts: number
}

/** 文档解析预览（书本文件上传预检） */
export interface ParsedDocPreview {
  format: string
  title: string
  author: string
  lang_hint: string
  char_count: number
  warnings: string[]
  suggested_source_id: string
  chapters: { index: number; heading: string; chars: number }[]
}

/** 草稿审阅条目（草稿 + 归并预判） */
export interface IngestDraftItem {
  seq: number
  staged_at: string
  draft: DraftPoint
  /** endorse 背书既有点 / create 造新点 / covered 本源已背书 */
  action: 'endorse' | 'create' | 'covered'
  point_id: string
}

/** 归并计划（发布预览） */
export interface MergePlan {
  new_points: KnowledgePoint[]
  endorsements: { point_id: string; source_ref: KnowledgeSourceRef }[]
  already_covered: number
  actions: { action: string; point_id: string }[]
}

/** 发布回报 */
export interface ApplyReport {
  source_id: string
  lang: string
  new_points: number
  endorsed: number
  already_covered: number
  total_points: number
  /** 决策表规则：新增 / 替换 / 冲突跳过（0 = 未派生规则） */
  rules_new: number
  rules_replaced: number
  rules_conflicts: number
}

/** 一键导入开始回报 */
export interface IngestStartResponse {
  source_id: string
  chunks_total: number
}

export interface AnalyzedError {
  error: ErrorItem
  attribution: AttributionResult | null
}

export interface DecisionTableMeta {
  lang: string
  version: string
  updated_at: string
  /** 规则行数（真实统计） */
  rule_count?: number
}

export interface DecisionTableView {
  meta: DecisionTableMeta
  raw: string
}

/** POST /gec/tables/:lang/validate 响应（dry-run 校验） */
export interface ValidateResponse {
  ok: boolean
  errors: string[]
}

/** GET /gec/tables/:lang/versions 列表项 */
export interface TableVersion {
  id: number
  lang: string
  saved_at: string
  rule_count: number
}

/** PUT /gec/rules/:lang/toggles 请求体 */
export interface PutTogglesRequest {
  toggles: Record<string, boolean>
}

/** PUT /gec/rules/:lang/toggles 响应 */
export interface PutTogglesResponse {
  lang: string
  count: number
  toggles: Record<string, boolean>
}

export interface SessionMeta {
  session_id: string
  lang: string
  status: string
  created_at: string
}

export interface ErrorHistoryEntry {
  lang: string
  error_type: string
  occurred_at: string
}

export interface SessionRecord {
  meta: SessionMeta
  history: ErrorHistoryEntry[]
  error_type_counts: Record<string, number>
}

/** 会话习惯性错误分析结果 */
export interface SessionAnalysis {
  session_id: string
  total_errors: number
  type_distribution: Record<string, number>
  habitual: HabitualErrorType[]
  top_corrections: TopCorrection[]
}

export interface HabitualErrorType {
  error_type: string
  count: number
  ratio: number
  is_dominant: boolean
  examples: string[]
}

export interface TopCorrection {
  from: string
  to: string
  count: number
}

/** POST /gec/explain 响应 */
export interface ExplainResponse {
  explanation: string
  exercises: string
  model: string
  template_version: string
  template: string
}

/** SSE 流式事件（/gec/explain/stream） */
export interface LlmExplainStreamHandlers {
  onMeta?: (meta: { model: string; template: string; template_version: string }) => void
  onToken?: (token: string) => void
  onDone?: (done: { explanation: string; exercises: string }) => void
}

/** 语言 / 引擎元信息（GET /gec/languages；桌面端：invoke('languages')）
 *  前端语言切换器据此展示真实引擎信息，避免与后端实际能力不一致 */
export interface LanguageInfo {
  lang: string
  name: string
  /** NLP 主路径服务（如「HanLP 英文（分词 + POS）」） */
  nlp_service: string
  /** 引擎类型（rule / statistical / llm） */
  engine: string
}

// ─── 运行时设置（GET/PUT /gec/settings） ───

/** LLM 参数（GET /gec/settings → llm） */
export interface LlmSettingsView {
  base_url: string
  api_key: string
  model: string
  timeout_secs: number
  /** 提示词模板目录（等价环境变量 LLM_PROMPTS_DIR） */
  prompts_dir: string
  /** true = LLM 已配置（api_key 非空） */
  enabled: boolean
}

/** LLM 请求模式（固定默认：前端用流式，外部 API 用非流式） */
export interface LlmModes {
  frontend: string
  external_api: string
}

/** 纠错模型预设视图（GET /gec/settings `models`；设置界面「纠错模型」选择器列表） */
export interface ModelView {
  /** 预设 id（`prompts/models.json`；如 `generic` / `cec4` / `cec3`） */
  id: string
  /** 展示名 */
  name: string
  /** 输出格式（`json` / `plain`） */
  output_format: string
  /** 是否启用（选择器仅列 enabled 条目） */
  enabled: boolean
}

/** 决策模型（vegaml）层参数（GET /gec/settings → decision） */
export interface DecisionSettings {
  /** 决策模型服务地址（如 http://127.0.0.1:4804） */
  endpoint: string
  /** vegaml 模型名 */
  model: string
  /** 请求超时（秒） */
  timeout_secs: number
  /** 弃权阈值：has_error_prob 低于该值视为无错误 */
  abstain_threshold: number
  /** true = 决策层已启用（默认开启；GEC_DEC_ENABLED=0/false 或设置界面可关闭） */
  enabled: boolean
}

/** L3 特征模型层参数（GET /gec/settings → l3；三期：level 3 先跑特征模型，LLM 收缩到纯长尾） */
export interface L3Settings {
  /** 特征模型服务地址（如 http://127.0.0.1:4806） */
  endpoint: string
  /** 模型名（微调后为 checkpoint 名） */
  model: string
  /** 请求超时（秒） */
  timeout_secs: number
  /** 弃权阈值：概率灰区上界；灰区 / 服务不可用时 LLM 长尾兜底 */
  abstain_threshold: number
  /** true = 特征模型层已启用（默认开启；GEC_L3_ENABLED=0/false 或设置界面可关闭） */
  enabled: boolean
}

/** L3 飞轮状态（GET /gec/l3/status；三期闭环：确认样本汇入 → 积累提醒 → 重训练） */
export interface L3FlywheelStatus {
  /** 该语言样本总数（正 + 负） */
  samples: number
  /** 采纳平行语料句对数（adopted.jsonl；旧服务端无此字段） */
  adopted?: number
  /** 负偏好条数（rejected.jsonl；旧服务端无此字段） */
  rejected?: number
  /** 上次重训练时间（ISO 8601；从未训练过为 null） */
  last_retrain_at: string | null
  /** 是否建议重训练（样本积累够 + 冷却期已过） */
  retrain_ready: boolean
  /** feature-model 服务是否可达 */
  service_available: boolean
  /** 当前模型名（服务不可达时 null） */
  model: string | null
  /** 当前模型 dev F1（服务不可达 / 无 dev 集时 null） */
  f1: number | null
}

/** L3 飞轮重训练结果（POST /gec/l3/retrain） */
export interface L3RetrainResult {
  /** 模型是否已热切换（false = F1 显著退化，保留旧模型） */
  swapped: boolean
  /** 未切换时的原因 */
  reason?: string
  /** 重训练前 dev F1 */
  before_f1?: number | null
  /** 重训练后 dev F1 */
  after_f1?: number | null
  /** 样本统计 {confirmed, base, total} */
  samples?: { confirmed: number; base: number; total: number }
  /** 本次重训练时间（ISO 8601） */
  retrain_at: string
}

/** GET /gec/settings 响应 */
export interface AppSettings {
  hanlp_endpoint: string
  llm: LlmSettingsView
  llm_modes: LlmModes
  /** 当前选中的纠错模型预设 id（`prompts/models.json` 注册表；缺省 `generic`） */
  llm_model_id: string
  /** 可用纠错模型预设列表（「纠错模型」选择器；仅含 enabled 条目） */
  models: ModelView[]
  /** 离线模式（等价环境变量 GEC_OFFLINE=1）：检测器仅用本地规则，跳过远端 NLP 服务 */
  offline_mode: boolean
  /** 决策表目录（等价环境变量 GEC_TABLES_DIR） */
  tables_dir: string
  /** 规则词表目录（等价环境变量 GEC_LEXICONS_DIR；zh/en/ja 共用） */
  lexicons_dir: string
  /** 决策模型（vegaml）层参数 */
  decision: DecisionSettings
  /** L3 特征模型层参数（三期：level 3 先跑特征模型，LLM 收缩到纯长尾） */
  l3?: L3Settings
  /** 嵌入引擎配置（向量召回用；未配置时召回静默禁用） */
  embedding: EmbeddingConfig
}

/** PUT /gec/settings 请求体（部分更新：仅提供要修改的字段） */
export interface UpdateSettingsBody {
  hanlp_endpoint?: string
  /** 当前选中的纠错模型预设 id（`prompts/models.json` 注册表；缺省 `generic`） */
  llm_model_id?: string
  llm?: {
    base_url?: string
    api_key?: string
    model?: string
    timeout_secs?: number
    prompts_dir?: string
  }
  offline_mode?: boolean
  tables_dir?: string
  lexicons_dir?: string
  decision?: {
    endpoint?: string
    model?: string
    timeout_secs?: number
    abstain_threshold?: number
    enabled?: boolean
  }
  l3?: {
    endpoint?: string
    model?: string
    timeout_secs?: number
    abstain_threshold?: number
    enabled?: boolean
  }
  embedding?: {
    endpoint?: string
    api_key?: string
    model?: string
    timeout_secs?: number
  }
}

/** 设置测试目标 */
export type SettingsTestTarget =
  | 'hanlp'
  | 'llm'
  | 'decision'
  | 'l3'
  | 'embedding'
  | 'tables_dir'
  | 'lexicons_dir'
  | 'all'

/** POST /gec/settings/test 响应（桌面端 invoke('test_setting') 同构） */
export interface SettingsTestResult {
  target: string
  ok: boolean
  /** 结果说明（通过时为能力摘要，失败时为具体原因） */
  message: string
  /** 耗时（毫秒） */
  latency_ms: number
  /** target = "all" 时：全部项目的逐项结果 */
  results?: SettingsTestResult[]
}

export const ATTRIBUTION_LABELS: Record<string, string> = {
  knowledge_gap: '知识缺口',
  habit_solidified: '习惯固化',
  rule_missing: '规则缺失',
  rule_forgotten: '规则遗忘',
  collocation_weak: '搭配薄弱',
}

export const WEINER_LABELS: Record<string, string> = {
  internal: '内源',
  external: '外源',
  stable: '稳定',
  unstable: '不稳定',
  controllable: '可控',
  uncontrollable: '不可控',
}

/** 嵌入引擎配置（向量召回用；未配置 endpoint 时召回静默禁用） */
export interface EmbeddingConfig {
  endpoint: string
  api_key: string
  model: string
  timeout_secs: number
}

/** 学习知识条目（向量召回用；LLM 反馈 + 手动录入） */
export interface LearnedKnowledge {
  id: number
  lang: string
  text: string
  error_type: string
  correction: string
  source: string
  created_at: string
}

/** 采纳 → 规则分支：规则构建成功（默认停用，检测页可快捷启用/停用） */
export interface AdoptRuleResult {
  via: 'rule'
  message: string
  rule: {
    id: string
    wrong: string
    right: string
    /** 当前开关状态（构建成功时恒为 false = 默认停用） */
    enabled: boolean
    /** true = 新创建；false = 示例已存在于该规则 */
    created: boolean
  }
}

/** 采纳 → 知识分支：规则构建失败，知识候选（未落库，等待用户保留/放弃） */
export interface AdoptKnowledgeResult {
  via: 'knowledge'
  message: string
  knowledge: {
    text: string
    error_type: string
    correction: string
  }
}

/** 采纳 → anomaly 分支：AI 污染「确认污染」后的模式蒸馏结果
 *  accepted=true：新模式过金集门槛，已写回词表（检测器重建后即时生效）；
 *  accepted=false：蒸馏失败/未过门槛，降级知识候选（knowledge 字段，用户保留/放弃） */
export interface AdoptAnomalyResult {
  via: 'anomaly'
  message: string
  anomaly: {
    pattern: { pattern: string; strong: boolean; note: string } | null
    accepted: boolean
    gate?: {
      compiles: boolean
      matches_instance: boolean
      fp_negatives: string[]
      tp_positives: number
      total_positives: number
      accepted: boolean
      reason: string
    } | null
    rule_id: string
    reason?: string
    created?: boolean
  }
  /** 蒸馏未沉淀时的知识候选（与知识分支同构） */
  knowledge?: {
    text: string
    error_type: string
    correction: string
  }
}

/** LLM 兜底结果「采纳」响应（规则分支 / 知识分支 / anomaly 蒸馏分支） */
export type AdoptResult = AdoptRuleResult | AdoptKnowledgeResult | AdoptAnomalyResult

/** 知识召回测试结果（「沉淀为知识」流程：测试 ②.5 是否命中） */
export interface RecallTestResult {
  /** 余弦相似度（null = 无法测试） */
  similarity: number | null
  /** 召回阈值（与 ②.5 知识召回层一致） */
  threshold: number
  /** 是否预期命中（null = 无法测试：未配置嵌入服务 / 嵌入调用失败） */
  would_hit: boolean | null
  note: string
}

// ─────────────────────────────────────────────────────────────────────────────
// 统一反馈 + 反思流程（/gec/feedback*；桌面端 invoke error_feedback 等）
// ─────────────────────────────────────────────────────────────────────────────

/** 反思建议类型（前端渲染为一键按钮，经 /gec/feedback/action 执行） */
export type ReflectionSuggestionKind =
  | 'disable_rule'
  | 'exclude_word'
  | 'remove_example'
  | 'remove_knowledge'
  | 'l3_negative'
  | 'retrain'
  | 'view_rule'

/** 反思建议（可执行） */
export interface ReflectionSuggestion {
  kind: ReflectionSuggestionKind
  label: string
  payload: Record<string, unknown>
}

/** 反思报告（放弃检测结果后的偏差诊断） */
export interface ReflectionReport {
  /** 判断来源层（rule / knowledge / decision / feature / statistical / llm） */
  source: string
  /** 具体对象（规则 id / 知识条目 / 模型层名） */
  subject: string
  /** 一句话诊断：偏差从哪来 */
  diagnosis: string
  /** 证据列表 */
  evidence: string[]
  /** 可执行建议 */
  suggestions: ReflectionSuggestion[]
  /** LLM 深度反思（可选；配置 LLM 后追加） */
  llm_analysis?: string
}

/** 统一反馈响应（采纳 / 放弃；全部来源层） */
export interface FeedbackResponse {
  action: 'adopt' | 'reject'
  message: string
  /** adopt：实际走的路径（rule / knowledge / anomaly / confirm） */
  via?: string
  rule?: { id: string; wrong?: string; right?: string; enabled?: boolean; created?: boolean }
  knowledge?: { text: string; error_type: string; correction: string }
  anomaly?: {
    pattern: { pattern: string; strong: boolean; note: string } | null
    accepted: boolean
    gate?: { reason: string } | null
    rule_id: string
    reason?: string
    created?: boolean
  }
  /** reject：白名单词 ID */
  word_id?: number
  /** reject：反思报告（诊断 + 证据 + 可执行建议） */
  reflection?: ReflectionReport
  /** LLM 深度反思是否可用（可进一步调 /gec/feedback/reflect-llm） */
  llm_reflect_ready?: boolean
}

/** 反馈统计（按来源层 / 规则聚合；反馈历史面板用） */
export interface FeedbackStats {
  source: string
  rule_id?: string
  adopted: number
  rejected: number
  last_at: string
}

/** 反馈日志明细 */
export interface FeedbackLogEntry {
  id: number
  lang: string
  source: string
  rule_id?: string
  error_type: string
  original: string
  correction: string
  action: 'adopt' | 'reject'
  reason: string
  created_at: string
}

/** 反馈历史（设置界面「反馈历史」面板） */
export interface FeedbackHistory {
  lang: string
  stats: FeedbackStats[]
  recent: FeedbackLogEntry[]
}

/** LLM 深度反思结果 */
export interface ReflectLlmResult {
  llm_analysis: string
  model: string
  template: string
}

// ─────────────────────────────────────────────────────────────────────────────
// 测评中心（进化中心 P0：层级归因 + 消融 + A/B + 漏斗）
// ─────────────────────────────────────────────────────────────────────────────

/** 发起评测请求（POST /gec/eval/run 透传） */
export interface EvalRunRequest {
  /** mucgec / cscd / nacgec / jfleg */
  benchmark: string
  /** 子集过滤（cscd: cscd/sig15/med/law；jfleg: dev/test） */
  split?: string | null
  /** 纠错模型臂（prompts/models.json 注册表 id；current = 不切换） */
  arm?: string
  /** capability = 强制 LLM（层级能力上限）；funnel = 自然级联（LLM 触达率） */
  mode?: 'capability' | 'funnel'
  /** 消融：临时关闭某层（{"decision": false} / {"l3": false}） */
  ablation?: { decision?: boolean; l3?: boolean; knowledge?: boolean } | null
  /** 句数上限（缺省 = 全量） */
  subset?: number | null
  seed?: number
  workers?: number
  /** 清空该配置的逐句缓存重跑 */
  fresh?: boolean
}

/** 评测任务进度（GET /gec/eval/job/:id） */
export interface EvalJob {
  id: string
  status: 'pending' | 'running' | 'done' | 'error'
  config?: {
    llm_model_id?: string
    llm_model?: string
    llm_enabled?: boolean
    decision_enabled?: boolean
    l3_enabled?: boolean
    embedding_configured?: boolean
  } | null
  progress: { done: number; total: number }
  report_id?: string | null
  error?: string | null
  created_at: string
  done_at?: string
  warnings?: string[]
}

/** 层级归因行（报告内 layers 值） */
export interface EvalLayerStats {
  n_pred: number
  tp_strict: number
  p_strict: number
  r_strict: number
  f_beta: number
  tp_overlap: number
  r_overlap: number
  fp_strict: number
  fp_overlap: number
  gate: { tp: number; fp: number; fn: number; tn: number; precision: number; recall: number; f1: number }
  recall_share: number
  attributed_gold: number
}

/** 报告详情（GET /gec/eval/report/:id） */
export interface EvalReport {
  id: string
  ts: string
  benchmark: string
  split?: string | null
  lang: string
  beta: number
  arm: string
  mode: string
  ablation?: { decision?: boolean; l3?: boolean; knowledge?: boolean } | null
  config?: EvalJob['config']
  sentences: { total: number; ok: number; failed: number }
  n_sentences: number
  n_gold_sentences: number
  n_gold_edits: number
  overall: Omit<EvalLayerStats, 'recall_share' | 'attributed_gold'>
  layers: Record<string, EvalLayerStats>
  funnel: {
    llm_calls?: number
    llm_calls_per_100?: number
    llm_reach: number
    avg_llm_ms: number
    cumulative: { prefix: string; resolved: number; rate: number }[]
  }
  report_md?: string
}

/** 报告列表行（GET /gec/eval/reports） */
export interface EvalReportSummary {
  id: string
  ts: string
  benchmark: string
  split?: string | null
  arm: string
  mode: string
  ablation?: { decision?: boolean; l3?: boolean; knowledge?: boolean } | null
  n_sentences?: number
  n_gold_edits?: number
  f_beta?: number
  p_strict?: number
  r_strict?: number
  llm_reach?: number
}

// ─────────────────────────── 反思中心（P1） ───────────────────────────

/** 反馈统计（按来源层 / 规则 id 聚合） */
export interface ReflectionStat {
  source: string
  rule_id?: string | null
  adopted: number
  rejected: number
  last_at?: string
}

/** 测评证据：单层归因摘要 */
export interface ReflectionEvalLayer {
  name: string
  n_pred?: number
  f_beta?: number
  tp_overlap?: number
  fp_overlap?: number
  recall_share?: number
  gate_f1?: number | null
}

/** 测评证据（从测评中心最近报告提取；缺失 = 测评服务不可达） */
export interface ReflectionEvalEvidence {
  report_id: string
  benchmark?: string
  arm?: string
  f_beta?: number
  llm_reach?: number | null
  layers?: ReflectionEvalLayer[]
}

/** 可执行建议（P2 训练中心消费） */
export interface ReflectionCenterSuggestion {
  /** lexicon_condition / rule_tune / l3_sample / llm_arm */
  kind: string
  target: string
  summary: string
  evidence?: string[]
  payload?: Record<string, unknown>
  priority?: number
}

/** 反思报告（一次「生成反思报告」的完整产出） */
export interface ReflectionCenterReport {
  lang: string
  created_at: string
  feedback_total?: number
  stats?: ReflectionStat[]
  findings?: string[]
  eval_evidence?: ReflectionEvalEvidence | null
  suggestions?: ReflectionCenterSuggestion[]
}

/** 反思报告列表行 */
export interface ReflectionReportSummary {
  id: number
  lang: string
  created_at: string
  n_suggestions: number
}

/** 反思报告详情（完整 JSON 产出） */
export interface ReflectionReportFull {
  id: number
  lang: string
  created_at: string
  report: ReflectionCenterReport
}

/** 建议队例行（带状态机：pending / applied / consumed / dismissed） */
export interface ReflectionSuggestionRow {
  id: number
  report_id: number
  lang: string
  kind: string
  target: string
  summary: string
  payload?: Record<string, unknown>
  evidence?: string[]
  priority?: number
  status: string
  status_at?: string | null
  created_at: string
}

/** POST /gec/reflect/run 请求 */
export interface ReflectRunRequest {
  lang: string
  include_eval?: boolean
  eval_report_id?: string | null
}

/** POST /gec/reflect/run 响应 */
export interface ReflectRunResponse {
  id: number
  report: ReflectionCenterReport
  suggestion_ids: number[]
}

// ─────────────────────────── 训练中心（P2） ───────────────────────────

/** 领域池样本（互补单侧分配：每句只贡献一个标签） */
export interface PoolSample {
  sentence: string
  /** error（有错，可带修正）/ clean（干净） */
  label: string
  error_type?: string | null
  correction?: string | null
  /** feedback_adopt / feedback_reject / flywheel / augment */
  source: string
}

/** 领域池（一次构建的完整产出） */
export interface DomainPool {
  lang: string
  samples: PoolSample[]
  total: number
  error_count: number
  clean_count: number
  by_source: [string, number][]
}

/** LLM 训练数据样本（SFT 格式） */
export interface LlmTrainingSample {
  sentence: string
  error_type: string
  correction: string
}

/** LLM 训练数据（jsonl 可直接导出外部训练） */
export interface LlmTrainingData {
  lang: string
  arm: string
  samples: LlmTrainingSample[]
  jsonl: string
}

/** 训练作业行（状态机：proposed → confirmed → running → completed / failed） */
export interface TrainingJobRow {
  id: number
  /** domain_pool / l3_retrain / lexicon_condition / llm_data */
  kind: string
  lang: string
  status: string
  source_suggestion?: number | null
  payload?: Record<string, unknown>
  result?: Record<string, unknown> | null
  created_at: string
  updated_at: string
}

/** POST /gec/train/pool 响应 */
export interface TrainPoolResponse {
  job_id: number
  pool: DomainPool
}

/** POST /gec/train/llm-data 响应 */
export interface TrainLlmDataResponse {
  job_id: number
  data: LlmTrainingData
}

/** POST /gec/train/l3/retrain 响应 */
export interface TrainL3RetrainResponse {
  job_id: number
  result: Record<string, unknown>
}

/** POST /gec/train/lexicon/apply 响应 */
export interface TrainLexiconApplyResponse {
  job_id: number
  applied: { rule_id: string; word: string }[]
  skipped: number
  eval_evidence?: unknown
  suggestion_consumed: boolean
}

/** POST /gec/train/suggestion/:id/run 响应 */
export interface TrainRunSuggestionResponse {
  job_id: number
  job_kind: string
  result: Record<string, unknown>
  suggestion_consumed: boolean
}

// ─────────────────────────── P3 贡献与轮次 ───────────────────────────

/** 单层贡献（边际：全开 Fβ − 该层关闭 Fβ） */
export interface ContributionLayer {
  layer: string
  f_beta_full: number
  f_beta_ablated: number
  delta: number
  /** 该层自己定案的 TP 数（0 = 自身零定案） */
  tp_self?: number
  /** 拓扑假象：自身零定案却有大 Δ（Δ 来自下游门控效应，非该层解决能力） */
  topological?: boolean
}

/** 层级贡献表（消融 Δ 常态化） */
export interface ContributionTable {
  benchmark: string
  split?: string | null
  arm: string
  f_beta_full: number | null
  f_beta_core: number | null
  layers: ContributionLayer[]
  missing: string[]
  reports: number
}

/** POST /gec/eval/contributions 响应 */
export interface ContributionResponse {
  group: { benchmark: string; split: string | null; arm: string; mode: string }
  table: ContributionTable
}

/** 轮次动作（P2 训练作业 kind + rule_tune 人工项） */
export interface RoundAction {
  kind: string
  target: string
  reason: string
  payload?: Record<string, unknown>
}

/** 优化轮次（状态机：proposed → confirmed → running → completed/failed；proposed → rejected） */
export interface OptimizationRound {
  id: number
  lang: string
  status: 'proposed' | 'confirmed' | 'running' | 'completed' | 'failed' | 'rejected'
  reason: string
  evidence: string[]
  actions: RoundAction[]
  result?: Record<string, unknown>
  created_at: string
  updated_at: string
}

/** POST /gec/round/evaluate 响应（提案 或 「无需行动」+ 各指标离阈值差距） */
export interface RoundEvaluateResponse {
  action: 'none' | 'round_proposed'
  reasons?: string[]
  round?: OptimizationRound
}

/** 轮次报告中的动作项（附派生作业完整结果 + 一行摘要） */
export interface RoundReportAction {
  kind: string
  target: string
  reason?: string
  job_id?: number
  job?: {
    id: number
    lang: string
    kind: string
    status: string
    result?: Record<string, unknown>
    created_at: string
    updated_at: string
  } | null
  summary: string
}

/** GET /gec/round/:id/report 响应（聚合报告：背景 + 各动作完整结果 + 执行后测评证据） */
export interface RoundReport {
  round: OptimizationRound
  /** 人类可读总述（各动作一行） */
  summary: string
  n_actions: number
  n_failed: number
  actions: RoundReportAction[]
  evidence: string[]
  /** 执行后该语言最新测评报告（服务不可达 → null） */
  eval_after: {
    report_id: string
    benchmark: string
    arm: string
    f_beta: number
    llm_reach?: number | null
    layers?: unknown[]
  } | null
}

