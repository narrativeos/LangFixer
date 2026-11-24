# 下一步研发计划（2026-11-24）

基于四仓实读证据（LangFixer main `0e4466b`、jev-gec `PLAN_v2.md`、jev-llm
`PLAN_LLM.md`、EVOLUTION.md 判决线）综合成文。

**总原则**：模型主线（jev 系）以「收口上线 > 新训练」为序；产品侧以「喂数据 >
新特性」为序。EVOLUTION.md §13 已测否定项（17 条）一律不重提。

## 轨道 A：③ 自持臂收口上线（在研最远，只差灰度）

目标：把已过闸的 L2-kto-v7 从「参考臂」变成线上可选主力。

| # | 任务 | 现状 → 待做 |
|---|---|---|
| A1 | 灰度切流 | **已上线（2026-11-24 实况）**：主臂 Qwen3.8（:4000 代理）+ 影子臂 arm3-l2kto-v7（llm.jiulu.ltd），双判开关已开，dual_judge_log 已记 3078 条 |
| A2 | 双判裁决出账 | **首轮账已出**：both 773 / none 596 / primary_only 1908 / shadow_only 23（全 en 拼写，抽样看均为真实错字——自持臂 en 侧意外增益，待逐条复核确认真阳）/ shadow_parse_failed 34 / primary_parse_failed 21。zh 侧 shadow_only=0，与 gray380 判决一致（unique recall 等 ②.5/L3，不在双判账里等）。**取证缺口**：parse_failed 落盘的是规范化 `[]`，违规原文未存（gec-core/src/llm.rs 写路径），待补 raw 落盘 |
| A3 | 净收益 + 延迟终审 | gray380 带内 F 不低于现 API；3090 vLLM P95 满足 ③ 钩子预算。两闸过 → 主臂切 arm3，外部 API 降级为备援 |
| A4 | Qwen 改错模型微调执行 | §11.16 状态「起草，待执行」→ 执行。四道闸照单：契约门 / 主门 mucgec F0.5≥0.805（cec3 基线）/ 泛化门 capability 全集 / 接线 models.json。§11.15 定案不变：只换执行体（手），不碰触发权（闸） |

成本：A1–A3 近零算力（机件全就位，是运营动作）；A4 一次 KTO 周期。依赖：无阻塞，A 是最先能出线上收益的轨道。

## 轨道 B：I 位漏字盲区（北极星唯一非零的增量线）

目标：真实漏字（I 位）检出率从 ≈0 爬起来。

- **B1 PLAN_v2 拍板起跑**：§5 四个开放问题默认值均可直接采纳（model_miss 不进池 /
  字头阈值 0.5 / 句头 1:1 下采样 / 合成首批 5 万行）。拍板即跑。
- **B2 D2 配方 + 消融**：纯池流移 synth + I×4 过采样（§11.19 已让北极星首次非零，
  dev FP 1.6% / gov 0.7%）；追加干净侧 hard-negative 消融。
- **B3 取证换宿主**：干净侧样本从合成注入位换真实 learner 干净行（§11.18 已证伪
  「合成注入位可学」，此步是修正地基）。
- **B4 I 位数据回流**：生产 accepted edits 仅 9 个 I 位——存量不足是硬瓶颈，靠轨道 D
  的网关流量喂。纪律：北极星只认 gray380 I 位 + 句 FP≤2%；注意 `locate_eval.py`
  取数 bug 勘误，此前旧 I 位读数一律作废。

成本：Mac MPS 单卡天级（首轮实测口径）。依赖：B4 联动 D。

## 轨道 C：决策模型 serve 侧融合 + 残留观察销账

- **C1** Track B 多 head 融合落盘：`ensemble_fuse.py` 目前仅研究口径，移进 serve
  路径并过同把尺。
- **C2** EVOLUTION §8 残留观察：「训练后/轮次后 MuCGEC A/B」用 feature-model 实跑
  一次，把挂着的观察项变成有数结论。

成本：小（均为已训模型的接线与验证）。依赖：C1 在 B2 出配方后做，避免融错版本。

## 轨道 D：网关生产放量喂数据（多条线的共同水源）

- **D1** 生产反馈进 `feedback_log` → Cycle 2「生产收益矩阵」出账。**实况**：
  双判已在跑但 feedback_log 仅 2 行——用户采纳/放弃反馈几乎没有，机件在等
  真实用户放量（邀请多发、反馈入口曝光），这是判决线最后一块账的真瓶颈。
- **D2** 数据闭环时序实跑：白天 T1 灰区 → 夜间 T2 判官（32B@3090 常驻 :8090，与
  训练互斥需排班）→ pref 行双写 jev-llm-data/jev-data → 周度 KTO 重训。T2 跑批已能
  自动出 pref，缺的是把它排成夜间例行。
- **D3** zh 知识源导入：知识积累目前 0 条（commit 01e4803 状态条点名）——纯运营
  动作，导入存量语料。
- **D4** 网关 D3 残余：过期清理 systemd timer 补上。

红线：生产用户文本只在域内（jev-llm + jev-llm-data + jev-data + 内部 3090）流转；
jev-llm-data 永不加公开远端；外租算力只碰公开+合成。

成本：运维小时级。依赖：无；D 是 B4 / A2 / Cycle 2 的上游，越早上量越好。

## 轨道 E：多模态 P2 收口

契约基线已 PASS（`media_smoke_p2.sh`，pdfcraft-cli/wordcraft-cli），差四件事：

- pdfcraft/wordcraft adapter 接线入 product 内 media-flow；
- C 档 OCR（先试 PdfCraft 内置 `ocr_recognize`，不够再挂外部引擎）；
- 桌面端命令同构；
- PDF/Word 重演验收。

成本：工程天级，无 GPU。依赖：无，可与 A/B 并行。

## 轨道 F：游戏化层启动（先补依赖，再最小闭环）

依赖盘点结论：进度持久化基本就位——`session_store.rs` 已有 mistakes /
mistake_mastery / global_mastery / review_undo / learning_events 五表 + profile
复习状态机；CEFR 难度分级已有。缺口：

- **F1** 游戏化设计文档（当前零设计）：先写一页——区域进阶地图怎么映射 mastery
  数据；副本/成长线后置。
- **F2** LLM 出题模板：从用户 mistakes 行生成针对性练习，难度锚 CEFR，复用
  learning_events 里的错因分类。
- **F3** 最小可玩闭环：只做「错题 → 出题 → 复习调度 → mastery 更新」一圈；FSRS
  若未实装先退化为简单间隔重复。

成本：设计半天 + 工程周级。依赖：F2 依赖 T1 出题质量（与 A 共用端点即可）。

## 顺序与并行建议

```
第 1 周    A1 灰度开双判（零成本）＋ D1/D3 放量导入 ＋ B1 拍板
第 2-3 周  B2/B3 训练消融 ＋ A4 微调执行 ＋ D2 夜间批排班 ＋ E 接线
第 4 周    A2/A3 双判出账定切流 ＋ C1/C2 销账 ＋ F1 设计稿
之后       F2/F3 视 A/B 余量启动
```

三条纪律提醒：

1. A1 切流节奏由维护者在设置页定，注册表 enabled 只控制选择器露出；
2. KTO 后续迭代（v8/L3）盯生成侧 bad_json，不能只看 r/loss；
3. 生产文本不出域、v23 禁入 Qwen3.8 系评测，两闸不变。
