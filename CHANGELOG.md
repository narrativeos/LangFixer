# 更新日志

本项目遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/) 格式与语义化版本。

## [Unreleased]

### 新增

- **依赖服务监督器（`api-server/src/deps.rs` + `routes/deps.rs`）**：api-server 启动即自检六项依赖
  （decision:4804 / l3:4806 / char_gate:4811 / hanlp / LLM 网关 / 嵌入），本机可管服务在
  loopback 地址 + 仓内 venv + 资源门槛（decision/l3 300MB、char_gate 2048MB，可用内存读
  /proc/meminfo 或 vm_stat）满足时主动拉起（日志 `log/supervisor/`，子进程不随 api-server 退出被杀）；
  拉不起来不静默放弃：`GET /gec/deps` 状态面逐项给出原因 + 「是否有其他机器可部署」提示，
  运营台「设置 → 依赖服务」卡片提供「拉起服务」/「保存其他机器地址并重测」两条补救路径，
  应用启动 toast 与检测服务状态条 chip 同步点名未就绪项；`POST /gec/deps/start|recheck`（ops 面，
  token 防线）；PUT 设置改动相关端点后台自动重探。实机重启验证 6/6 全 ok（decision/l3/char_gate 均
  auto 拉起）。途中修复两处 spawn 坑：posix_spawn 下相对程序路径在切 current_dir 后才解析致 ENOENT；
  canonicalize 解析 venv python 符号链（→homebrew）破坏 pyvenv.cfg 探测致子进程丢 site-packages
  （改绝对不解析符号链）。
- **label-adjudicate 公网发布（token 分发模式）**：登录支持 `--token-db` 直连网关
  auth.db（sha256 校验 access_tokens，吊销即断，HMAC cookie `lf_adj` 12h），
  一人一枚评审令牌；公网实例跑在 47.94.172.217（systemd
  langfixer-adjudicate.service，quorum=2），经 Caddy `handle_path /adjudicate/*`
  挂到 https://langfixer.jiulu.ltd/adjudicate/；serve.py 兼容服务器 CPython 3.6.8。
- **label-adjudicate 多人协作产品化（`tools/label-adjudicate/`）**：登录页+房间口令
  （HMAC 签名 cookie）、裁决逐条署名 `who`（旧 journal 无署名归房主）、每人一票
  同名重裁覆盖自己、条目软领取防冲突、`--quorum K` 多数票共识门槛（平票→存疑，
  默认 1 保持旧行为）、crew 审计面板；judge.py report 新增裁决人两两人人一致率
  （逐位/采纳方向）并改比人工共识。v2.3 抽检自此双裁定账，单人裁决不再单独过闸门。
- **label-judge 裁判口径 v2→v3（`tools/label-judge/`，未提交工具首次入账）**：
  SuperVL 二审裁判 prompt 对齐人工金标口径（判修正本身而非原文是否必错）+〖〗就地
  定位被标记范围+空 fix 显式注明删除；auto 桶 58 条人工交集上 一致率 50.0%→74.1%→75.9%，
  误拦（裁reject/人adopt）18→5→4，**误放行（裁adopt/人reject）三版始终为 0**。
- **router_ev 归因终版定账（`tools/tonight-queue/`，EVOLUTION §11.14 补七）**：
  train LLM 缓存扩容夜跑把 p̂ 测量面从 611 行补到 5,068/5,070（99.96%，缓存
  5,657 行；`.expand` 自包含落盘经两次中断零丢失）；`_v3` 终版重赛：真 p̂
  锚点 router_ev 55.2% PASS / uniform-p 对照 54.6% → p̂ 分化增益 0→**+0.6pp**，
  计数头 +10.0pp 仍为主体；p̂(word_order)=0.541 显著低于先验 0.916 →
  **wo_exempt 退役判定坐实**；v2.3 rollout 收账（共识 9,202/新增 6,733/
  漏标 22,313）入 `~/tmp/dataset_v23/`。
- **ZEN 轨迹表 v2 冲突谓词（`tools/zen-trajectory/`，EVOLUTION §11.14 补六）**：
  table.json v2 四冲突谓词（straddle_pair/num_same_unit_diff/year_pair/
  conj_pair_break 整句 scope）+ 双锚定引擎（error-anchored/standalone）；
  三段校验定案：冲突谓词不获得独立路由通道（gold 锚定对 clean 基线无 lift、
  打扰 9.64%/4.75% 超 ≤2% 闸门；仅 year_pair 10× lift 但召回 0.4%），
  present 证据口径保留且逐位不变；已入 §13 否定表 #13。
- **ZEN 轨迹表首版（`tools/zen-trajectory/`，EVOLUTION §11.14 补五）**：声明式
  判定表（四族 ±6 窗口谓词→consistency/cohesion 通道）+ 在场命中账引擎，
  逐条轨迹账落盘；must 桶可达性与补二实验2 逐位一致（52.3%/42.4%）；
  v2 冲突谓词已在表内挂账。
- **规则臂前置扣除入账（EVOLUTION §11.14 补四）**：`rule_offline.py` 单一
  台账复刻（含 punct 文档风格门，语料毒账 7,587→1,219，punct 通道精度
  0.03%→9.5%）+ `rule_probe.py` 离线 EV（recall 0.3%/precision 10.5%，定案
  打扰 6.4/万字）；cycle2-matrix 分臂（whitelist/yield 只出自 llm 臂；dev 库
  规则臂反馈 6 条首次入账）；eval `--rule-subtract` 残差曲线：扣除仅 0.2%，
  规则臂价值定调=免费定案+事实核查挂钩而非预算节省。
- **路由第四臂 router_ev（方案 A′ 落地，EVOLUTION §11.14 补三）**：每型 Poisson
  计数头（蒸馏 gold_counts 型多重集）× p̂(t) 求和打分；缓存口径锚点总体
  **PASS**（54.6% vs router 44.6/histgb 40.9/random 48.6，非 oracle 首次）；
  gold_types.py --counts-out 同扫描产出计数+spans；p̂ 测量面待 train 缓存扩容。
- **分型分层三空白量化实验与方案定案（EVOLUTION §11.14 补二）**：①句内同族多
  错 61.3%（≥2 错句内，10 型口径）→ 型头定为每型期望计数头 A′+稀型先验 C，
  非 presence 二值；②must 桶 consistency/cohesion 的 ±6 窗口规则可达性
  42–52%（span 内仅 17.7%）→ ZEN 轨迹表谓词定为 ±6 窗口式；③规则臂同词典
  离线复刻实测 recall 0.3%/precision 11.1%，punct 子通道语料毒（需文档标点风
  格门）→ 规则臂定位坐实为前置扣除层+高敏感事实核查挂钩，EV(t) 以实测小值
  入收益矩阵。
- **Cycle 2 收益矩阵消费端（`tools/cycle2-matrix/matrix.py`）**：写侧三本账
  早已在产（detect_history.errors_json 检出+来源层、layers_json 路由决策账、
  feedback_log 全层采纳/拒绝留痕），零 Rust 改动；消费端 join 成按型收益矩阵
  （precision=adopt/(adopt+reject)、yield=每千句复查换回的真修正、复查句
  ran/backfill/skip 三口径不双计、feedback 无外键按 original+14 天窗回指且
  命中/落空两列并报）；达门槛型输出 `--hit-types` 串回喂路由，报告文案内化
  §13 #12（低 precision 型不降级，出口是预算权重/型头）；错题本
  error_book.jsonl=被拒 LLM 检出含原句（负例蒸馏原料）。e2e 合成库验证通过；
  dev 实库 llm 侧反馈 0 条、矩阵全薄——机件等账，账等生产使用。周期表
  Cycle 2 行改「机件就位」（§11.12 补六）。
- **gold 侧型标签（`tools/true-gold/gold_types.py` + `router_labels.py
  --gold-types`）**：补四处方落地——text→v2 型码两通道并集（datasets 行级
  error_type 与 eval 侧同源映射 ∪ true_gold 编辑级六类经 v22 行 join；
  cohesion→logic、consistency→reference、fact 不映射）。池 train label=1
  join 命中 4,400/4,870（90.3%），纯 word_order 110 句从此可见。实验结论：
  全量 wo 降级两口径 −1.1~−1.5pp 判死（§13 #12）；型标签价值在
  error_types 坐标系（4,400 行带型，蒸馏/收益矩阵输入）而非降级。
  同时从 982 行缓存唯一反解出补四实际白名单（含 word_order/reference）。
- **规则通道（rule-channel）**：true-gold 规则候选的消费端四件——
  规则族谓词（标点惯例自适应/叠用/严式时间/地名共现）、量化器（族×
  候选召回/点火/已标免问/纯打扰每万字）、运行时命中即问
  （adjudicate.py --check）、用户判断回写同一本 gold journal（--record，
  merge 重跑自动吸收）。量化结论：静态规则对 mark 桶只回收 0.6%
  （主体是漏字/搭配/语序类，规则结构上够不着），定位调整为未来文本
  的高敏感事实核查要点（打扰 ≤31/万字）；mark 主体留给 rollout 与
  知识源通道。e2e：命中→回写→merge 吸收（2,386→2,387）验证通过。
- **v2 级 true gold 合并器**（`tools/true-gold/merge.py`）：true gold =
  共识 ∪ 已采纳模型新增 ∪ 金集真错，噪声/reject 进 excluded，门控未见
  进规则通道（rule-candidates 带 ctx，运行时命中由用户判断，判断回流
  同一本 gold journal）。裁决 journal 为空时出按桶默认版，抽检推进后
  重跑收紧。首跑：标签行 12,542（共识 10,156 + 金集真错 2,386），
  gold 字符覆盖 20,053/38,924 = 51.5%，零重叠；两本 journal 响应性
  e2e 验证通过。类型分布（typo 5,994/grammar 3,495/cohesion 324/
  consistency 205/punct 99/fact 39）即 route_label 重生成的输入。
- **merge 前置复读拦截 dup_artifact + 抽检后 gold 重建重跑**（§S）：
  五桶各抽 58 条全部未过 2% 门禁（auto_real 3.4 / auto_noise 25.9 /
  label auto 6.9 / must 17.2 / audit 20.7），不整桶采信；adopt 131 进
  gold。v2.2 插入残片复读定性为渲染归一化缺陷（抽检 reject 主因），
  以「fix 前缀==span 左侧邻接 ∥ 后缀==右侧邻接」规则入 merge 前置：首跑
  拦 1,060（人工 adopt 覆盖 2 均为 fix 尾字 off-by-one 真错；单字动词 AA
  重叠「讲讲」类守卫豁免）；标签行 12,686（共识 10,156 + 采纳 131 + 金集
  真错 2,399），pending 7,376→6,176，coverage 51.55%。
  重跑 gold_types+labels+eval：两锚点 44.6/64.1 与补五逐位相同——gold
  重建改资产与坐标系，不动 anchors（决策面输入未变），§13 #12 再确认。
- **金集灰区裁决页（gold-adjudicate）**：对 v2.2 未命中的 21,257 个
  gold 字符（`~/tmp/grey_zone/`）尽量自动裁决：回退（v2.1 已给 fix）
  2,386 默认真错带旧 fix/type；两轮均问均否 8,649 默认噪声（采纳走
  ≥58/≤2% 抽检门）；门控未见 10,222 不硬判，默认标记导出为规则候选
  （`action=rule-candidate`，后续落规则引擎由用户运行时命中判断）。
  裁决 journal 落盘续跑，快捷键 R/N/M，批量按桶默认判。
  顺手修了 dump_grey.py 的引用错位（多 edit 行内 gold a/b 误取循环变量，
  影响 8,985 条记录，已重生成分区不变）。
- **裁决页泛化支持 dataset_v2x 队列**（v2.2 rollout 收尾）：
  `tools/label-adjudicate/serve.py` 数据装载从 `*_v21.jsonl` 泛化为
  `*_v2*.jsonl`（排除 review_queue），版本从文件名尾缀识别并映射
  `REPORT_REF` 逐条口径参照（v21: 12,970/12,789；v22: 7,376/10,156），
  默认 `--data-dir` 切到 `~/tmp/dataset_v22`。v22 队列端到端验证：
  total_new 7,376、labels 17,532、行计数不一致 0，分档 auto 934 /
  must 849 / audit 5,593；v22 无 fix==原文 缺陷，预筛信号仅剩
  lex_ok 115 / lex_blind 62（v21 各信号兼容性回归通过）。
- **路由三臂评测 harness**（EVOLUTION.md §11.12，分支 feat/router-harness）：
  `feature-model/router_labels.py` 在 jev zh_v2 池上产句级路由标签（格式对齐
  池行，配方 = gold∪LLM∪lexical∪采样地板并集，§11.10）；
  `feature-model/eval_router_arms.py` 三臂等调用率曲线（histgb 代理/random/
  router）+ §11.11 类型分层判定（薄金标只报数不裁决、skipped≠covered）。
  首跑总体 PASS（锚点 63.1%：router 65.2% ≥ histgb 64.7% ≥ random 64.0%），
  优势幅度待 LLM 臂直跑补边际收益维度；gold 集与池的结构性污染以
  「默认仅池外行」口径披露。
- **路由 harness 非 oracle 口径与 LLM 臂直跑机件**（EVOLUTION.md §11.12 补）：
  `router_labels.py --llm-run` 支持并发（`--llm-workers`）与检出缓存断点续跑，
  失败行不再伪装成已查实干净；`eval_router_arms.py --llm-run-gold` 在池外
  gold 行分层抽样直跑 LLM 臂落盘检出，`--only-cached` 只在已查实行上出
  公平曲线。实测（398 已查实行）：总体 FAIL（random 48.6% > router 42.5%），
  定量实证「gold 存在性代理标签缺 LLM 边际收益维度」（LLM 对 word_order
  检出召回仅 41.7%，路由预算被低收益句占住）。llm_hit 入标签重赛后：oracle
  总体 PASS 幅度拓宽（锚点 65.4%）；cache 口径仍 FAIL 但 ≤20% 预算段转为
  压倒领先，处方指向按型条件化路由（收益矩阵 P(LLM 检出|型)，§11.12 补二）；
  池臂跑满后另实：原始 llm_hit 信号饱和（82% 行非空），需∩gold 背书再入标签
  （§11.12 补三）。

- **route_label v2 机件：按型背书白名单 + word_order 降级**（EVOLUTION.md
  §11.12 补四；均默认关不改旧配方）：`router_labels.py --hit-types` 仅白内
  检出入 llm_review（白名单由 gold 侧按型精确率定，punctuation 类 14% 滤除），
  `--wo-exempt` 纯 word_order 检出型 gold 句降 rule_decide（高精确 81% 低召回
  42%→规则/专项侧）。按型收益矩阵首批成表；标签配方迭代使 cache 口径锚点
  41.7%→45.7%（对 random 差距 −6.9→−2.9pp），oracle 同步创新高 65.6%，
  按型条件化方向被数据连续验证。

- **模型新增标注分档抽检裁决页**（tools/label-adjudicate，dataset_v21 的 12,970
  条待抽检队列）：启动时按**逐条口径**从 `*_v21.jsonl` 现算队列（复现更正后
  12,970 新增 / 12,789 共识 / 25,759 合计；旧行级 `model_new` 与旧队列是累计
  口径 9,285，只对照不信任）；分档 auto=标点 / must=fact·consistency·cohesion
  / audit=typo·grammar，门槛每桶 ≥58 条抽检且放弃率 ≤2% 才整桶升级；裁决
  journal 追加落盘可续跑，导出已采纳 ndjson。

- **裁决页自动预筛信号**（tools/label-adjudicate）：每条新增标注按词表/
  模式打信号（判无错误入 no_error 403 / 删除意图 del_intent 228 / fix==原文
  矛盾 noop_contra 1,404 / 词表背书 lex_ok 193 / 词典矛盾 lex_contra 0 / 标点
  盲点 lex_blind 335 / 无信号 10,407），支持按信号筛选 + 当前筛选集批量
  裁决；fix==原文三档批量采纳一律服务端跳过，导出时对已采纳的
  del_intent 归一为 delete（fix 置空）、其余矛盾档标 manual 待修。由此
  暴露 rollout 解析缺陷约 2,035 条：模型 why 已给出实质修正或删除意图，
  但归一化后的 fix 仍等于原片段（v2.2 需在归一处修正）。

- **检测结果支持按细分类型二次筛选**（web-ui，分型架构的界面投影）：汇总
  分类（5 大类）之下新增「细分类型」过滤行（error_type 维度，17 类 T 族），
  与分类过滤级联（换分类自动清类型过滤、新检测重置）；批量采纳作用域
  文案同步；层显示词表收敛单源（errorDisplay.SOURCE_LABELS 改为复用
  labels.ts 人话表，消除双头维护漂移风险；word_order 注释归位 zh/ja 共用）；
  检查模式名收敛 labels.ts `modeLabel` 单源（原 Editor/HistoryPanel/
  MediaWorkspace/SettingsPanel 四处硬编码，描述性后缀仍在各组件）。

- **zh 错误分类法 v2 落地：LLM 细类型不再坍缩**（EVOLUTION.md §11.11）：
  `map_llm_error_type` 按 CEC 细分一对一映射（词性/语序/成分残缺/赘余/
  关联词/指代→各自细类型码，语义逻辑不通转正为 `logic` 常量），
  `error_types` 补 11 个细类型常量；兼容不迁 DB：新函数
  `coarse_error_type` 查询期粗族回退——归因决策表（analyze/attribute 按
  zh 门控，ja 表 `word_order` 命中行不受影响）、错误画像统计
  （type_distribution/habitual）与会话分布按粗族聚合，行级数据保留细类型；
  W3C issue_type 推导补细类型分支；前端 `ERROR_TYPE_LABELS` 补 T 族与数字
  通道展示词。

- **zh 数字/量值/序号三条规则通道**（分类法 v2 首批，EVOLUTION.md §11.11）：
  新规则 `ZH_NUMERAL`（万万并置/阿拉伯-汉字数字混写，GB/T 15835—2011）、
  `ZH_QUANTITY_UNIT`（数值区间单侧缺单位、中外单位并注，GB 3101/GB/T 1.1）、
  `ZH_SERIAL_NO`（行首序号与「第X章/节/条」同上下文 run 跳号/重号），token/
  离线双路径；lexicon 新增 `numeric` 分类，`error_types` 新常量 `numeral`/
  `quantity_unit`/`serial_no`；停用/白名单经中央过滤自动继承。
- **各层贡献度报告界面**（参赛需求「展示各层在评测报告中的贡献」）：新端点
  `GET /gec/eval/layers` 由 api-server 直读报告档案（`GEC_EVAL_REPORTS_DIR` >
  `datasets/eval_reports` > `src/datasets/eval_reports`，坏文件按条跳过），不依赖
  Python 测评服务——生产评测实例未起也能看；已入评审密钥 fail-closed 白名单
  （只读质量透明面，评测**发起**仍属运营面）。前端新「各层贡献」视图
  （LayerContributions）：报告选择 + 两报告 Δ 对比、层级贡献表（预测 / 严格
  P·R·Fβ / 自定案金标 / 召回份额条）、config 开关 chips 与级联漏斗；网关形态
  全员可见（评委可达），单机仅运营位显示入口。
- **L3 特征模型公网上线**（EVOLUTION.md §8「2026-11-13 · L3 特征模型公网上线」）：
  公网实例 4806 服务当日从无到有——sklearn 句级模型（45.8 万句训练特征，dev
  F1 0.549）+ docker（python:3.12-slim）+ systemd `langfixer-l3`，`l3.enabled`  开回 true；level 2 恢复「规则 + L3 门控 + LLM 长尾」的真实三级形态。
- **level 3 不得被 L3 门控终结全文 LLM**（L3 上线当日公网实测抓到）：③-F 定案
  分支把 level 3 的 `need_llm` 也置 false，深度扫描退化为门控臂——内置示例
  3 例有 2 例被句级「判定无错」放行（门控 R≈0.45），「漏报兜底」名存实亡。
  level 3 定案错误照合并，但全文 LLM 无条件保留；level 2 放行语义不变。
  反转 `cascade_level3_l3_*_skips_llm` 两用例并改名固化。
- **质量指标定版**（EVOLUTION.md §8「2026-11-12 · 质量指标定版」）：capability
  全集两份报告（mucgec 1137 句 / jfleg 1501 句，seed 42，按 gold 全集行数收敛，
  网关健康门下 `llm_ran` 100%）。句级门控 mucgec P/R **0.975/0.850**、jfleg
  **0.954/0.944**；编辑级严格 Fβ 0.0134 / 0.1038（generic 臂支配，与 §8 P0 的
  cec3 微调臂不可横向比；3090 新 gec 模型出来后补对比版）。报告
  `20261112T223729Z_{mucgec,jfleg}_current_capability` 随本条入库。
- **调用级日志（`gw_call_log`）**：网关形态下每个经过鉴权门的请求记一行
  （`ts / token_id / endpoint / status / latency_ms`）——「谁在几点调了哪个端点、
  被拒的 401/403 尝试」从此可查（D1 遗留项落地）。立场与红线：
  ① **放行与拒绝都记**，核验成功的评审令牌即使越权被拒也记本人，不洗成匿名；
  ② **只存路径**——query string 可能含用户文本（`?text=`）、请求体更是绝不落，
  header 不落；`token_id` 是身份不是凭证；③ 单机形态**不落**（零新增写盘，不把
  私人使用记成「调用」）；④ 记账失败只 warn，日志是运维资料不是事务参与方；
  ⑤ 保留期与日账同源（retention timer 一并删超期行）。读端点：`/admin/stats` 带
  `calls_today / calls_retained`（进行中的请求完成后才入账，读到的永远是此前全部），
  `/admin/tokens` 逐令牌带同口径计数；「密钥分发」界面加「调用次数」列与概览合计。
  验收 `api-server/tests/gateway_call_log.rs`（放行/拒绝/越权逐行对齐 + query 不落 +
  合计与逐行一致）。`docs/GATEWAY_AUTH.md` D1 段与 `deploy/gateway/langfixer-retention.sh`
  同步。

### 修复

- **设置页「测试全部」报红两条——待发布层探测 + L3 关闭门控语义缺口**（11-13 走查
  发现）：① 快速定案层当前**待发布**（`enabled=false`，11-13 定位：线不放弃，待
  更合适的决策模型；见 EVOLUTION §13 #10），`settings_test` 不看
  开关直接探测，公网部署必然报红——disabled 层现短路回「未启用」（绿）不探测；
  ② L3 生产开关开而 4806 服务从未部署到公网实例，这条红属实；处理时牵出**文档与
  代码矛盾**：`l3.rs` enabled 字段口径「显式关闭 → 回退无条件 LLM」，但 engine 的
  level 2 门控在 L3 未挂载分支 `None => false` 关门——旧路径「服务不可用」是
  fail-open 让 LLM 全文复查，直接翻开关会让 level 2 静默退化为纯规则（决策层已
  退役，弃权触发不复存在）。`CascadeDetector::l3_disabled` 区分**显式关闭**（开闸
  回退无条件 LLM）与**从未配置 L3**（保持决策弃权门控旧语义）两态，补两用例。
  公网实例已 `PUT /gec/settings` 关 `l3.enabled`（4806 未部署、3090 模型未出，
  与关闸前 fail-open 行为一致）；gec 模型出来后随 4806 服务部署原位打开。
- **jfleg 的 strict 命中恒为 0——量具坐标系错位**（11-12 定版抓到：4489 条预测、
  0 条严格命中，中文链路数字却正常）：JFLEG gold 按 `make_gold.py` 明文口径「S/D
  span 含尾随空白、插入锚点取下词起点」，而生产侧 LLM 句级 diff 的预测 span 词界
  精确（gold `[56,64]` vs 预测 `[56,63]`），strict 键 (op, ref, span) 永远差一个
  空白位——英文数字是量具问题，不是检测能力。`eval_server.norm_span` 把严格键与
  重叠判定归一到同一空白不敏感坐标（中文恒等变换，zh 全集数字与修复前逐位一致）；
  selftest 补英文坐标系用例。修复后 jfleg 严格 P/R/Fβ = 0.1559/0.0444/0.1038。
- **评审密钥可以改写共享规则库、重训模型、起长跑**（网关形态可达面没收住，11-12
  评委体验走查发现）：`/gec/l3/retrain`、`/gec/agent/run`（带自主档写工具）、
  `/gec/eval/run`、`/gec/llm-feedback`（`adopt` → `adopt_as_rule`）、
  `/gec/ops/unlock`（无 `GEC_TABLES_UPDATE_TOKEN` 时直接 `unlocked:true`）对评审身份
  全部敞开——参赛密钥是全体评委共享的一枚，一次误触就是全体评委的现场。改为鉴权门
  上的 **fail-closed 白名单**（`review_denied`）：只放行「检测 + 本人历史 + 只读说明」，
  其余 403 + 说明文案；界面同步收起评审身份下必然 403 的「测试全部」。立场与 MCP
  写档工具改提案信封一致：**共享身份不得改写共享状态**。验收
  `api-server/tests/gateway_review_surface.rs`，口径见 `docs/GATEWAY_AUTH.md` D2。
- **不挂鉴权门装配 `routes::router` 时，业务端点一律 500**（`Missing request
  extension: Tenant / AuthContext / Arc<GatewayConfig>`）：这三样由 `gate` 注入，
  生产 `main.rs` 恒挂门所以线上无恙，但集成测试与「把 api-server 当库用」的宿主
  绕开门装配就全线报错——`api-server` 测试套件因此已经红了很久（`tests/api.rs`
  51 项里 32 项失败，症状是空响应体 / 500，很容易被误读成业务坏了）。新增
  `middleware::auth::with_standalone_tenant()`（注入与门在单机形态等价的三件扩展，
  配 `GatewayConfig::standalone()` + 内存令牌库，不落 `auth.db`），13 个测试装配点
  统一改走它。现在 `-p api-server` 全套绿。
- **LLM 网关失败演练暴露的三个坑**（2026-11 演练，隔离实例黑洞网关实测）：
  ① **卡死网关把整次 level 3 检测挂 6 分钟**——重试预算原来只累加退避睡眠，
  每次尝试自身烧满超时不记账：30s 超时 × 3 次尝试 + 25s 退避实测 115s，按线上
  120s 超时放大 ~6'25"，浏览器 180s 先掐。`LLM_RETRY_BUDGET_SECS` 改**挂钟总预算**
  语义（含尝试耗时），挂钟越过预算直接收手，退避表 0 间隔不再是「预算外再加一次
  满超时尝试」的后门；② **LLM 层运行时失败把整次 detect 打回 502**——level 3 /
  force_llm 的全文 LLM 兜底失败（超时、排队耗尽、熔断开断）现**降级保留规则层
  结果**，层级摘要记 `llm: unavailable`（前端已有「不可用已回退」映射，零改动；
  与定向复查、L3 失败同一先例）。未配置 LLM 仍明确报错——那是配置问题不是运行时
  故障；③ **LLM 聊天客户端没有熔断**（决策 / L3 / 嵌入客户端早有，LLM 漏了）——
  接入同款三态熔断器（连续失败 3 次开断 30s，冷却到期自动探测恢复）；api-server
  形态 LLM 引擎每次现构建，故熔断器进程级共享注入（`State::llm_breaker`），桌面端
  同理（`llm_breaker()` 单例）。演练对账：加固前卡死网关 level 3 = **115.2s→502**、
  连不上 = 25.3s→502；加固后 = **200**（规则结果保留 + `llm: unavailable`），
  熔断开启后第 2 次请求起 0.1-4s 级失败不再逐个用户交超时税；level 1 路径
  （评审常规路径）始终不受波及（0.26s）。重试错误文案「共等待 Ns」改「已耗时
  Ns」（挂钟口径）。回归测试 4 条：熔断开断/冷却探测/挂钟预算/level 3 降级。
- **登录后 whoami 的可选字段在界面上消失**（`web-ui/src/lib/auth.ts`）：`loginWithKey` 用
  逐字段字面量重建身份视图，后端每加一个可选字段就在这条路径上被静默丢掉（`?token=` 直链登录后
  「密钥分发」看不到密钥来源与指纹，而同一页面直接 `fetch('/auth/whoami')` 带着它们）。
  现抽出纯函数 `mergeIdentity(prev, me, login?)`，字段只在一处列举；来源与指纹只取本次 whoami
  （换实例不得留上一台的指纹），`signOut` 一并清掉。`web-ui/src/lib/auth.test.ts` 锁住语义。

### 新增

- **显式轮换超级管理员密钥**（`POST /admin/admin-token/rotate`）：换钥匙从此是一个动作，
  不是重启或改文件的副作用。body 必须 `{"confirm":"ROTATE"}`（缺失或别的值 → 400）；
  密钥来自 `gw_meta`（安装生成）时 200，新明文只在这一次响应出现，旧密钥与其超管会话
  （`gw_sessions.token_id='~admin'`）即刻失效，评审密钥不受影响，重启复用新值；
  密钥来自 `LANGFIXER_ADMIN_TOKEN` 时 **409** 并给出改 env 的流程——**故意不做**「库里换一把、
  env 那把仍然赢」的分裂行为。顺序是先落库再换内存摘要（`AdminCred` 摘要改内部可变并随
  `Clone` 共享，故已注入各路由的句柄同一时刻认新钥匙），库写失败回 500 且密钥未变更；
  日志只记指纹。`whoami` 对**已验证的超管**额外回 `admin_token_source`（`env`/`generated`）与
  `admin_token_fp`（SHA-256 前 8 位，核对用，非密钥），匿名探测与桌面端 invoke 不带这两个字段；
  「密钥分发」界面据此显示「这把由谁持有 · 指纹 xxxx」。env 与 `gw_meta` 同时有值时启动
  `WARN` 指明库里那枚不会被使用。`docs/GATEWAY_AUTH.md`「轮换超级管理员密钥」按来源写清泄露处置。
  HTTP 层验收：`api-server/tests/gateway_admin_api.rs`（confirm 必填 / 旧钥匙与旧会话即刻失效 /
  评审密钥不受波及 / env 模式 409 且不写库 / 评审密钥 403 / 匿名看不到来源）
- **根目录部署脚本 `./setup.sh` / `./update.sh`**（共用件 `scripts/deploy-common.sh`；约定见
  `deploy/gateway/README.md`「日常就用两条命令」）：都带 `local|server`，首次安装与日常更新各一条，
  幂等——已存在的密钥、`langfixer.env`、systemd 单元一律不覆盖。`update.sh server` 一次跑完
  「铺源码 → 服务器后台编译 release（期间服务不动）→ 换二进制并留 `api-server.prev` → 重启 →
  探活 → 铺前端 dist（`index.html` 用 `mv` 原子替换）→ 同步运行时资产」，探活不过**自动回滚**
  二进制并重启。探活不是只看 200：绕开 Caddy 直连网关读 `whoami`、无凭证打管理面应 401、
  带超管密钥真实跑一次 `detect`。`--dry-run` 只报差异不动手，`--no-web` / `--no-assets` 缩范围。
  运行时资产（`decision-tables` / `lexicons` / `prompts`）铺之前先按 git 清单比指纹再留档——规则
  开关与决策表 PUT 会被服务**写回**这些 JSON，无脑覆盖等于静默抹掉运维在界面上做的改动；自上次
  部署被删除的文件按 git diff 清理，`lexicons/.lexicon-seed.lock` 这类未跟踪锁文件不碰。三条红线：
  脚本只报密钥来源与指纹、绝不打印明文（首装那一次例外）；脚本永不换钥匙（`systemctl restart`
  永远复用同一把）；密钥文件、数据库与 `langfixer-settings.json` 永不出现在任何同步路径里。
  本机实例强制 `.dev/{auth.db,core.db}` 隔离库（缺省会落仓库根的真实产品库，而
  `DELETE /gec/history` 是真删）；4805 已被旧 Vite 占用时沿用不重复起（Vite 会静默换端口到 4806，
  反代与文档全都对不上）。两个已踩过的坑写进 README：编译判据只用日志哨兵（`pgrep`/`pkill -f
  "cargo build --release"` 会匹配到执行它的命令自身），铺源码与启动编译必须分两步（`git archive |
  ssh 'tar -x && cargo build &'` 会把整条管道一起后台化，tar 被截断）

### 修复

- **嵌入服务「Model not exist.」**（`gec-core/src/embedding.rs`）：`.json()` 之后又 `.header("Content-Type", "application/json")` 走的是 reqwest 的**追加**语义，请求实际带两个同名 `content-type`。宽松服务端容忍，严格的 OpenAI 兼容网关（实测阿里云 DashScope compatible-mode）把请求体判成无效并回 `400 {"error":{"message":"Model not exist.","code":"model_not_found"}}`——与模型名无关，配置怎么改都失败。现由 `.json()` 单独设置该头，并补 mock 严格网关回归测试（`embed_sends_exactly_one_content_type`，同名头多于一个即判 400）；`embed` / `embed_batch` 两条路径同修

### 新增

- **密钥分发界面用量统计**：`GET /admin/tokens` 一条 SQL 带出每枚密钥的日账读数（`used_today` / `used_retained` / `days_used` / `last_day`），`GET /admin/stats` 增 `chars_today` / `chars_retained` / `retention_days`；「接入 → 密钥分发」表格加「今日用量」（含配额进度条与今日剩）与「累计 / 最近」两列，摘要行显示在途会话与保留期内合计字符数。口径如实标注为**保留期内**累计——日账跟检测文本一起按 `LANGFIXER_RETENTION_DAYS`（默认 7 天）清理，不是历史总量；调用级日志（谁何时调了哪个端点）仍未做，`docs/GATEWAY_AUTH.md` 记录了两档补法
- **验证机制（D4，`docs/GATEWAY_AUTH.md`）**：超管密钥装完即有——env 未设 `LANGFIXER_ADMIN_TOKEN` 时首次启动自动生成 `lf_<40hex>` 落 auth.db `gw_meta`（重启复用不漂移，明文只在生成那一次的启动日志出现一次；env 设了以 env 为准，设了但 <32 字符仍按配置错误拒绝启动）。界面按 `GET /auth/whoami` 的 `locked` 决定是否门禁（网关形态恒锁；单机形态看 owner 是否已认领），未验证前产品界面整体不渲染，密钥只进 `sessionStorage`、`?token=` 直链登录后即从地址栏剥掉。单机「零变化」口径收窄为：业务端点与桌面端 `invoke` 零变化，仅设置写入在 owner 认领后要求登录态（认领入口在「接入」页）。网关形态配置写入与探测仅超管可达（其余 403），非超管读设置时 LLM / embedding 密钥回 `***`。管理界面「接入 + 密钥分发」把一枚密钥变成两种分发形态——Web 直链与 MCP 配置 JSON（同一密钥、同一份配额）；普通密钥用户只能导出自己这一枚，签发与吊销只对超管开放
- **评测通道 owner 隔离与配额（D2）**：网关形态下会话首次使用即绑定评审账号（跨账号 403），检测历史按账号伪会话 `s_gw:<token>` 归户（列表过滤 / 详情与删除跨账号 404），detect / analyze 按字符数日配额计量（越线 429，`quota_chars=0` 只记账）；清空全部 / 按语言批删 / 全局错题本等跨账号端点网关形态一律 403；网关形态跳过历史复用与进程级缓存保证每账号归属完整。单机形态全部行为零变化
- **MCP 面（`POST /mcp`，`docs/GATEWAY_AUTH.md`）**：JSON-RPC 2.0 无状态形态（initialize / tools/list / tools/call / ping），工具集 = proofread / explain 检测管线 + agent 工具集全量；与 web 面同一道 gate、同一套 token、同一本字符配额账。网关形态只读工具直执行、写档一律返回提案不执行（executed:false + [confirm-required]，与 web 侧 Confirm gate 同源叙事）；单机形态全部直执行
- **评测通道（网关形态）身份层**（`docs/GATEWAY_AUTH.md`）：`LANGFIXER_GATEWAY=1` 启用，单机形态行为零变化。评审令牌（明文一次、摘要入库、吊销连会话即断）+ 免注册会话 cookie + 管理面 `/admin/tokens|stats`；超级管理员令牌只来自 `.env`（`LANGFIXER_ADMIN_TOKEN`，≥32 字符，内存留摘要、不落库、不发 cookie、独占 admin 面）。为 web 与 MCP 共用一套 token 的接入形态打好同一身份底座
- **评测通道部署件**（`deploy/gateway/`）：systemd 服务 + 每日数据保留清理 timer（默认 7 天删除检测文本与配额日账，收敛已吊销令牌会话归属）+ Caddy 站点片段与部署 runbook；服务器形态 api-server 绑 docker 网桥宿主侧地址（`LANGFIXER_BIND=172.18.0.1:4801`），公网入口只由 Caddy(443) 持有，源码/env/数据库放 web 根之外

## [v1.1.0] - 2026-11-08

桌面端从「能跑」走到「像一台 Mac 应用」：自动更新按发布源烧入更新端点，原生菜单栏 /
右键菜单 / Finder 拖拽落地，更新流程改为先征同意；用户面信息减负——检测层级的人话词表
统一到一张表并收拢三处漂移，引擎细节走渐进披露；编辑器补「试一试」示例文本降低首次使用门槛。

### 新增

- **桌面端自动更新**：编译期按发布源烧入更新端点（`LANGFIXER_RELEASE_SOURCE` = github / gitea / atomgit，默认 github），启动 30 秒后与此后每 6 小时检查一次；macOS / Linux 静默下载安装完成后提示重启（「已装好待重启」版本落盘，冷启动不重复下载与弹框），Windows 先征得同意再下载安装（安装器拉起即退出进程）。配套发布链：`scripts/release.sh` 按源分别打包并断言双源版本号一致、`make_latest_json.py` 由签名生成 `latest.json`、`publish.sh` 多源推送单源失败不中断；前端不调用 updater IPC，故未授予 `updater:default` 权限
- **macOS 原生集成（`docs/desktop-hig.md`）**：原生菜单栏 App / File / Edit / View / Window / Help 全量带快捷键（⌘, 设置、⌘1–7 视图切换、⌘O 导入、⇧⌘E 导出、⌥⌘Z 撤销应用修正、⌥⌘T 主题），经 `menu-action` 事件转发前端路由；检测历史行 / 错误条目 / 载体任务页签三处原生右键菜单；Finder 拖拽导入（按路径读盘，不走 base64）；窗口背景随系统外观 + 手动主题同步（消除深色模式启动闪白）
- **应用级「撤销全部采纳」**：批量采纳后在非编辑区按 ⌥⌘Z（或菜单项）可整批还原为采纳前文本（`store.applySnapshot` 快照）
- **用户面信息减负（渐进披露）**：检测层级的人话词表统一到 `lib/labels.ts`（本地检查 / 快速定案 / 知识查询 / AI 复查），修复结果面板 / 历史 / 载体工作台三处词表各自漂移；主界面只留人话摘要，①②.5③ 层编号与引擎细节收进「详情」popover / tooltip；错误来源徽章、把握程度（→「把握大 / 中 / 小」）、共识文案、修复指引、设置页卡片名与引导文案同表统一；桌面形态的检测管线可用性收进顶栏紧凑 chip（● N/4 项可用，全视图可见），Web 形态保留整行状态条
- **编辑器「试一试」示例文本**：待检测文本标题行右侧提供三个短句示例（错别字 / 多字少字 / 语序颠倒），点击即填入并立即检测，降低首次使用的空白页成本；覆盖非空原文前存快照，⌥⌘Z 可找回原文
- **会话删除入口（C4，`docs/UX_REVIEW.md`）**：设置页「我的档案」新增「删除当前会话」——确认框明示「将移除该会话的 N 条错题；你的复习进度会保留」（N 为该会话错题本实际条数）；确认后级联移除会话 / 犯错流水 / 错题本与会话级掌握度 / 撤销快照 / 检测历史，**全局复习进度（FSRS 记忆）保留**，同一错误日后重现记忆复活；桌面端 `delete_session` 命令同构；删除后当前会话 ID 清空（下次检测自动新建会话）

### 变更

- 桌面端更新流程改为**先征同意**：所有平台先弹框确认再下载 / 安装，「稍后再说」落盘后不再反复打扰，手动检查更新走帮助菜单（macOS HIG「更新须用户知情选择」）
- 桌面端开发启动修复：`beforeDevCommand` 未正确拉起 Vite（端口 4805）导致 dev 加载失败

## [v1.0.0] - 2026-11-03

首个正式版本（前作 v0.1.0-preview）。质量门收拢：clippy 全量清零并转 CI `-D warnings`
阻断、评测基线回归门、vegaml 决策层退役出产品路径；学习闭环闭合（全局错题本 + FSRS
四键复习 + 撤销 + 学习统计）与交互切片 A–D 全部落地。

### 新增

- **交互切片 D（P2，见 `docs/UX_REVIEW.md`）**：学习价值可视化——新增 `learning_events` 事件流水表（采纳 / 复习只增不改，删除会话不清除：学习历史属于全局记忆）与 `GET /gec/stats?days=`（近 N 天逐日采纳/复习计数 + 窗口内评分分布 + 全时累计；桌面 `global_stats` 同构），练习页底部「学习统计 · 近 30 天」堆叠柱图 + 评分分布；采纳价值感——`POST /gec/feedback` 响应新增 `adopt_count`（此类错误累计采纳次数含本次），同类错误再次确认时 toast 显示「第 N 次采纳此类错误」；复习文案 CEFR 适配——复习卡提示语按学习者档案档位变化（A 段具体抓手 / B 段回忆导向 / C 段最小引导，无档案不出提示）
- **交互切片 C（P1，见 `docs/UX_REVIEW.md`）**：画像与术语收拢——练习页卡片标题「错题本 · 跨文稿」显式范围（走查更正：检测页本无画像卡片，「两套画像并存」不成立）；「习惯性」徽章加统计口径 tooltip；术语外化——「画像基于确认错误（误报与未采纳建议不计入）」→「只统计你确认过的错误」、UI 文案去 FSRS 字样；徽章语言统一——「待复习」→「没练过」（与「待复习」到期页签区分，一个概念一个名字）、复习徽章「复习中 · 下次 11-05」。删会话确认文案（C4）暂缓：当前 UI 无删除会话入口，实现时必须说明「错题移除、复习进度保留」
- **交互切片 B（P1，见 `docs/UX_REVIEW.md`）**：复习评分从两键升级为 **FSRS 四键**——忘了 / 有点难 / 记住了 / 很轻松（`again/hard/good/easy`，间隔粒度直接反映自评；API `rating` 字段优先、旧 `passed` 契约保持兼容，桌面命令同构）；复习节奏可视化——「今日已复习 x/10」进度 + 队列清空后的完成收尾态（本地按日计数）；复习**5 秒撤销**——`review_undo` 快照表存每次复习前的记忆状态（每卡仅最近一次），`POST /gec/mistakes/:gid/review/undo` 与 `POST /gec/sessions/:id/mistakes/:mid/review/undo`（桌面 `undo_global_review` / `undo_review_mistake`）还原排期/状态/稳定性/难度，撤销后卡片重回到期队列；错题本浏览控制——待复习 / 全部 / 已掌握页签 + 关键词搜索 + 错误类型折叠
- **交互切片 A（P0，见 `docs/UX_REVIEW.md`）**：导航「练习与复习」显示今日到期复习徽章；采纳成功提示补「已进入错题本」（批量采纳汇总一次，不再逐条弹）；复习队列改按**到期最久优先**（新卡其次，错题本列表保持最近出现浏览序）；复习卡挖空修复——`original` 不在句中时不再整句展示泄露答案，改为提示文案 + 揭晓后补原句
- **全局错题本（跨会话，学习闭环功能闭合）**：错题本从会话级升级为跨会话全局视图——`GET /gec/mistakes`（按 语言+类型+原文+修正 聚合全部会话，带出现次数与 `sessions_seen` 出现会话数）、`GET /gec/review`（全局到期复习队列）、`POST /gec/mistakes/:gid/review`（FSRS 复习）、`GET /gec/analysis`（全局统一错误画像，确认错误基准）；桌面端同构命令 `global_mistakes / global_review_queue / review_global_mistake / global_analysis`。设计：错题本体单一事实源（`mistakes` 表跨会话投影，无聚合表无双写无回填迁移），FSRS 记忆状态按错题键挂 `global_mastery` 表——**跨会话共享、删除会话不丢，同一错误日后重现记忆复活**；「练习与复习」面板切到全局错题本（不再依赖当前会话，展示「来自 N 个会话」标记）；会话级错题端点保留（契约不变）
- **评测基准与 CI 回归门（P0-1）**：新 workspace crate `tools/gec-eval`——对四份金集（CSCD+SIGHAN/EC-Med/EC-LAW、MuCGEC、NaCGEC、JFLEG）逐句跑规则层（离线确定性，无 LLM），产出 precision / recall / F0.5（GEC 惯例 β=0.5）与每千字符延迟中位数（release）；`--json` 写机器可读报告，`--check <baseline> [--tolerance]` 与基线对比、任一数据集 P/R/F0.5 下降超容差（默认 0.02）退出码 1；基线固化在 `datasets/eval_reports/baseline_rules.json`，CI 新增回归门步骤；README 新增「质量评测」小节（实测表 + 解读 + 定性定位对照）
- **统一错误画像（Phase 3）**：学生错题本 / 间隔复习与审校侧「作者习惯性错误」合并为同一分析模型（`shared-model::profile::ErrorProfile`）——同一主体（会话）随时间沉淀的错误模式的统一投影：类型分布 / 习惯性（主导，占比 ≥ 0.3）错误 + 示例 / 高频改错对 / 掌握度摘要。`GET /gec/sessions/:id/analysis`（桌面端 `session_analysis`）改为该投影：错题本非空时以**确认错误**为基准（`basis = confirmed`，误报与未采纳建议不再污染画像），旧会话回退检测记录（`basis = detected`，向后兼容）；「练习与复习」错题类型按同一口径标记「习惯性」
- **间隔复习与掌握度（Phase 2 第二片）**：错题本新增 `mistake_mastery` 表（`new / reviewing / mastered` + 下次复习调度：连对间隔 1/3/7/14/30 天、连对 6 次 = 已掌握；没记住清零次日再练）；新增 `GET /gec/sessions/:id/review` 与 `POST /gec/sessions/:id/mistakes/:mid/review`（桌面端 `session_review_queue` / `review_mistake`）；「练习与复习」新增「今日复习」练习卡（句子挖空 → 显示答案 → 记住 / 没记住）与掌握度徽章、汇总行
- **错题本（Phase 2 第一片）**：检测反馈「采纳」带会话时自动沉淀进会话错题本（SQLite `mistakes` 表；同类型 + 同原文 + 同修正聚合计数，句级上下文按错误 span 从原文提取）；新增 `GET /gec/sessions/:id/mistakes`（桌面端 `session_mistakes` 命令）；「练习与复习」视图从占位升级为错题本（按类型分组 + 计数 + 修正对照）
- **会话持久化（Phase 1.5）**：会话与学习者画像（母语 / 自评 CEFR）、犯错流水从内存实现迁移到 SQLite（`shared-model` 新增 feature-gated `SqliteSessionStore`，api-server / 桌面端与 detect_history 共用同一 db 文件），重启不丢档案；内存版 `SessionStore` 移除
- **能力位角色门控（Phase 1）**：前端 `lib/caps` 能力位（learning / proofreading / ops，本机持久化）门控顶层导航与设置卡片——载体审校 = 编校扩展、规则管理 / 知识维护 / 进化中心 / 高级设置 = 平台运营扩展；web 形态新增 `POST /gec/ops/unlock`（与决策表热更新共用 `GEC_TABLES_UPDATE_TOKEN` 鉴权）解锁运营扩展；深链打开未解锁视图时展示一次性说明卡而非硬跳转；首次引导增加「我的工作场景」选择
- **学习者档案（Phase 1）**：会话画像新增母语与自评 CEFR（`PUT /gec/sessions/:id/profile`，桌面端 `update_session_profile` 同构）；设置页「我的档案」卡片；LLM 讲解 prompt 注入画像母语（修正此前把会话检测语言误当母语注入的问题），讲解模板缺省 CEFR 档位取画像自评（请求显式指定 > 画像自评 > A1）；「练习与复习」占位视图（学习扩展入口）
- **载体审校（图片 / PDF / Word）**：`media-core` / `media-flow` 内核与编排层；api-server HTTP 面 + 桌面端同构命令；web-ui「载体审校」工作台（探测 → 抽取 → 检测 → 落点画框 → 勾选 → 回写标注 → 正误表导出 docx / html / md / json）
- **载体审校简单 / 高级模式**：模式门（`mode_gate`）与简单模式文档解析；PdfCraft / WordCraft 高级模式适配器；视觉档送端点前压缩
- **反思中心接入金集闭卷证据**：载体金集漏报 → 反思中心可执行靶子（rule_tune / L3 样本 / LLM 层分类）
- **gec-zh 新规则**：12/24 小时制混用、「名称后冒号」；出厂词表 `ZH_CONFUSABLE` / `ZH_MISSING_SUBJECT` 改为出厂开启
- **LLM 网关重试退避与预算 + 思考块 / 答案分离**

### 变更

- **clippy 存量告警清零（1.0 质量门）**：全 workspace 250 条告警全部消除（机械修复 + `matches!` 化 + doc 列表续行规范化 + 复杂类型别名 + 测试环境守卫 newtype 化 + 有理由的定点 `#[allow]`）；CI clippy 步骤由「不阻断」改为 `-D warnings` 阻断门
- **vegaml 决策模型层退役出产品路径**（EVOLUTION.md §13 #10 判决执行）：设置界面「决策模型（vegaml）」卡片与「启用决策层」开关、系统状态条 ② 层、服务总览行移除；管线提示条默认不再列示 ② 层（仅实验开关开启时显示）；设置 API `decision` 字段保留透传（契约不变，服务端状态不受影响）；实验路径经 `GEC_DEC_ENABLED=1` / 设置文件显式开启（代码保留），检测默认路径统一表述为「规则 → L3 → LLM」
- **复习调度升级为 FSRS（P0 对标成熟度）**：错题本间隔复习从手搓「连对 1/3/7/14/30 天阶梯 + 连对 6 次 = 已掌握」升级为 **FSRS 算法**（`fsrs-rs`，Anki 内置同款；基于 DSR 记忆模型的稳定性/难度双参数调度，目标保留率 0.9）：记住 → Good 按钮按记忆状态计算个性化递增间隔，下次间隔 ≥30 天 = 已掌握；没记住 → Again 按钮、间隔钳到次日。`mistake_mastery` 新增 `stability / difficulty` 记忆状态列（旧库打开自动增列迁移），旧「连对阶梯」数据经 SM-2 近似自动迁移（Anki 同款迁移路径）；API / 桌面命令 / UI 契约不变
- 检测页「响应级别」（L1/L2/L3）改为用户语言「检查模式」：快速检查 / 标准检查 / 深度扫描；「漏报检测」按钮更名「深度重查」（内部 `level` 参数与 API 契约不变）
- `GEC_L3_ADOPT_COLLECT`（采纳 / 放弃样本沉淀）在 **api-server 形态默认改为关闭**——共享部署不默认把用户数据用于训练，显式 `=1` 开启；桌面端保持默认开（本机单用户）
- 设置页 LLM 卡片与首次引导增加数据流向告知（检测文本会发送到所配置的模型端点）
- README 顶部新增「这是什么 / 给谁用 / 能做什么 / 做不到什么」产品叙事；完整环境变量矩阵迁至 `docs/OPERATIONS.md`

### 移除

- `/student/:id/*` 遗留路由（与 `/gec/sessions/:id/*` 重复，前端从未调用）及 `routes/student.rs` 孤儿文件
- `SessionPanel.vue` 孤儿组件（「会话分析」独立卡片从未被任何视图引用；习惯性错误分析并入统一错误画像，入口收敛到「练习与复习」与 `GET /gec/sessions/:id/analysis`）

## [v0.1.0-preview] - 2026-10-23

首个公开预览版。

### 新增

- **英文 GEC 检测与归因**：规则引擎（`gec-en`）+ 韦纳归因三维标注（能力/努力/可控性），归因决策表（zh/en/ja，JDM 格式，zen-engine 执行）
- **知识库与知识源体系**：知识点（point）建模与静态归点；内置自撰示例源 `src-sample-en`（Apache-2.0）；书籍经文档解析 + 摄取管线导入（`document-parse` + `attribution-engine/ingest`）
- **知识源资源包**：盘点 / zip 导出 / 导入 / 删除，两端同构；`license_scope` IP 边界
- **个性化补救**：错误组聚合、补救练习与复习闭环（lexicon-store 训练中心 / 优化轮次）
- **LLM 长尾兜底**：OpenAI 兼容端点可配置（不内置任何 Key），L3 弃权 → LLM 级联
- **形态**：`api-server`（REST）+ `web-ui`（Vue 3）+ `desktop-app`（Tauri）
- **许可与合规**：Apache-2.0（LICENSE/NOTICE），第三方许可清单（LICENSES-THIRD-PARTY.md）

### 已知限制（预览）

- 未做安全审计；`api-server` 应部署在认证层之后
- 数据格式（决策表 / 知识库 / 资源包）可能随版本演进，暂不提供迁移工具
- 中文/日文（gec-zh / gec-ja）能力为早期阶段

[unreleased]: https://github.com/narrativeos/LangFixer/compare/v0.1.0-preview...HEAD
[v0.1.0-preview]: https://github.com/narrativeos/LangFixer/releases/tag/v0.1.0-preview
