# LangFixer

[![CI](https://github.com/narrativeos/LangFixer/actions/workflows/ci.yml/badge.svg)](https://github.com/narrativeos/LangFixer/actions/workflows/ci.yml)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache--2.0-blue.svg)](LICENSE)

**线上体验** [langfixer.jiulu.ltd](https://langfixer.jiulu.ltd/) · **桌面版下载** [GitHub Releases](https://github.com/narrativeos/LangFixer/releases)

**LangFixer：把「语法纠错」升级为「语法教学」。**
检测中英文日文文本的语法错误之后，它回答教育上真正重要的问题——**这个错误为什么犯**（归因：知识缺口 / 习惯固化 / 规则缺失 / 规则遗忘 / 搭配薄弱 + 韦纳三维），**该用什么补救**（知识点讲解、CEFR 分级练习），并让系统随使用**越用越准**（特征模型训练飞轮）。

## 这是什么 / 给谁用 / 能做什么 / 做不到什么

- **这是什么**：语法错误检测 + 教育归因工具——检出错误后回答「为什么犯」（归因）与「该用什么补救」（知识点讲解、CEFR 分级），并内置自学习管线（知识源导入、规则派生、训练飞轮）让系统越用越准。
- **给谁用**：**核心审校用户**是语言学习者与专业编校者——对「检测 → 归因 → 人工确认 → 采纳」这套范式零先验，共用同一个工作台、引导与信任机制；**平台运营者**（决策表 / 词表 / 知识库 / 进化中心）经管理功能进入。
- **能做什么**：英文检测最成熟；中文覆盖词典规则类目（同音/形近错别字、的地得、标点排版、时间格式混用等）；日文为早期阶段。归因与讲解三语可用；本地规则层离线完全可用。按能力面展开：
  - **检测**：中文、英文（HanLP 服务 + Rust 侧规则）、日文（Sudachi + 本地规则），每种语言均带**离线兜底**（外部 NLP 服务不可达时自动降级为本地词典 / 规则检测）；API `level` 参数的**三级响应**在规则层之上按需触发 L3 特征模型门控与 LLM 兜底（机制见下文「三级响应」，~~vegaml 决策模型层~~已退役出产品路径、仅显式开启）。
  - **词表**：各语言规则词表（混淆词、过去式表、助词组合等）外置于 `lexicons/*.json`，不重新编译即可扩充，缺失 / 损坏时回退内置默认。另有**领域术语库** `lexicons/terminology_{lang}.json`（`protected` 受保护 / `forced` 强制更正）在引擎层对所有检测臂生效，防止把正确专业词误纠。
  - **归因**：ZEN Engine（GoRules JDM 标准）决策表，输出归因标签（知识缺口 / 习惯固化 / 规则缺失 / 规则遗忘 / 搭配薄弱）+ 韦纳三维度（内外源 × 稳定性 × 可控性）+ **CEFR 难度分级（A1-C2）** + 补救措施 + 决策追踪。
  - **交付**：Axum API 服务（Docker 化）+ Vue 3 Web UI + Tauri 2 桌面端（离线模式）；工作台带**分阶段检测进度条**，实时展示各层推进与耗时（Web 端轮询、桌面端事件，见 docs/OPERATIONS.md「检测阶段进度」）。
- **做不到什么**：**不保证全量召回**——本地规则层对中文载体审校金集召回 0%（见下文「多模态与载体审校」与 [MULTIMODAL.md](MULTIMODAL.md)），LLM 复查需自行配置端点且有成本；它不是风格编辑器，不改写内容。能力边界以各语言词表实际覆盖与测评中心实测报告为准。

## 立即上手

- **线上体验**：[https://langfixer.jiulu.ltd/](https://langfixer.jiulu.ltd/) —— 浏览器打开即用，无需安装；
- **桌面端**：完全离线可用（本地规则层），安装包见 [GitHub Releases](https://github.com/narrativeos/LangFixer/releases)；
- **一行 API**（对已启动的本机 api-server，默认 `:4801`）：

  ```bash
  curl -s http://127.0.0.1:4801/gec/detect \
    -H 'Content-Type: application/json' \
    -d '{"lang":"en","text":"She go to school yesterday."}'
  ```

- **从源码跑**：见「快速开始」。

## 项目结构

```
LangFixer/
├── Cargo.toml            # workspace（11 个 Rust crate）
├── shared-model/         # 统一数据模型（Error / 归因 / 用户 / 决策表元数据）
├── gec-core/             # GECDetector trait + GecEngine 引擎路由（rule/statistical/llm）+ 语言路由
├── gec-zh/               # 中文检测器（HanLP /analyze + 规则，离线兜底）
├── gec-en/               # 英文检测器（HanLP /analyze + 规则，离线兜底）
├── gec-ja/               # 日文检测器（Sudachi /parse + 规则，离线兜底）
├── attribution-engine/   # ZEN 决策表引擎 + 热更新 TableStore + 知识库 KnowledgeStore + 韦纳映射
├── langfixer-llm/        # LLM 扩展（OpenAI 兼容客户端 + SSE 流式 + 提示词模板）
├── decision-model/       # 决策模型（vegaml）：实验路径（已退役出产品），FastAPI 服务 + 微调工作流
├── feature-model/        # L3 特征模型：FastAPI 服务 + 训练工作流 + 训练飞轮（越用越准，自进化）
├── api-server/           # Axum API 服务（Dockerfile 内置）
├── desktop-app/          # Tauri 2 桌面端（离线运行）
├── web-ui/               # Vue 3 + Vite + Pinia 前端
├── lexicons/             # zh / en / ja 三张 GEC 规则词表（JSON，可热扩充）
├── prompts/              # LLM 提示词模板（3 语言 × CEFR 分级 + generic + 3 语言 detect 兜底检测模板，版本化）
├── decision-tables/      # zh / en / ja 三张 JDM 决策表（纯规则：错误类型 × 新手/置信度 → 归因策略）
├── knowledge-base/       # 知识库：知识源注册表（sources.json）+ 知识点（points/{lang}.json，1:N 背书）
│                         #   仅内置自撰示例源（Apache-2.0）；用户导入的书籍属用户数据、
│                         #   不随发行物分发（IP 边界见 docs/KNOWLEDGE.md）
└── scripts/              # build.sh / dev.sh / release.sh
```

规则与知识是分离的两层：**决策表**只承载可提取的规则（错误类型 × 新手/置信度 →
归因策略 + 通用补救 + CEFR），随产品发布；**知识库**承载知识内容与知识源（1:N 背书
关联），用户导入的书籍属用户数据、按 IP 边界不进发行物。一本书的端到端自学习
（解析 → 挖掘知识点 → 派生决策表规则 → 人工确认发布）与资源包导出 / 导入 / 删除均已
产品化，完整机制见 [docs/KNOWLEDGE.md](docs/KNOWLEDGE.md)。

## 快速开始

以下为源码 / 开发者路径。

```bash
# 1. 本地开发（API :4801 + 前端 :4805，Vite 代理 /gec /student）
./scripts/dev.sh

# 2. 全量构建
./scripts/build.sh

# 3. 发布（release + 桌面安装包）
./scripts/release.sh

# 4. 首次部署并启动（local = 本机网关 :4801 + Vite :4805；server = 公网评测实例）
./setup.sh local
./setup.sh server

# 5. 日常更新（server 会同步后端二进制 + 前端 dist + 运行时资产，失败自动回滚）
./update.sh local
./update.sh server
```

部署脚本的目标地址、服务名等可用 `LF_SSH / LF_URL / LF_DIR / LF_SRC / LF_WEB_ROOT /
LF_SERVICE / LF_PORT` 覆盖；两条命令的落地约定与红线见
[`deploy/gateway/README.md`](deploy/gateway/README.md)。

### VS Code 调试

`.vscode/launch.json` 内置三组配置（Rust 侧需安装 [CodeLLDB](https://marketplace.visualstudio.com/items?itemName=vadimcn.vscode-codelldb) 扩展，首次打开会提示安装）：

| 配置 | 说明 |
|---|---|
| `api-server (Rust)` | 构建并调试 API 服务（:4801），断点打在 Rust 源码 |
| `web-ui (Vite dev server)` | 内置 node 调试器直接 launch Vite（:4805），断点打在 vite 配置 / 中间件 |
| `desktop-app (Tauri)` | preLaunch 自动构建 + 确保 Vite 在跑（`scripts/ensure-vite.sh`），然后调试桌面端。前端在 Tauri 内 `invoke()` 直连 Rust 命令，**完全离线**、不依赖 api-server；Vite 仅提供页面与 HMR |
| `LangFixer (API + 前端)` | compound：同时调试 API 服务 + Vite |

### 桌面端窗口

- 首次启动窗口居中（`tauri.conf.json` `center: true`），默认 1280×860，最小尺寸 960×640（与前端布局下限一致）；
- 窗口位置 / 大小 / 最大化状态自动持久化（`app_config_dir()/window-state.json`，macOS 为 `~/Library/Application Support/com.langfixer.desktop/`），下次启动恢复；保存位置已离屏（如外接显示器断开）时回退为居中（`desktop-app/src/window_state.rs`）；
- 前端在 Tauri 环境内自动切换桌面自适应布局（`<html class="desktop">`）：应用壳式全高布局（body 不滚动、标签内容区内部滚动）、检测页双独立滚动列（输入列 5:7 比例随窗口伸缩，结果列限宽 900px）、文本输入区加高；
- 「设置」页在桌面端展示进程内运行说明（默认离线），且表单所有项（HanLP 地址 / 离线开关 / LLM 参数 / 决策表与词表目录）均可直接修改：保存后即时生效（热重建 zh/en/ja 检测器 / 重建 LLM 讲解服务 / 重载决策表），并持久化到本地设置文件（`GEC_SETTINGS_FILE`，默认 `langfixer-settings.json`，优先级高于环境变量，重启后仍生效）。

### 环境变量

完整环境变量矩阵（级联 / 模型层 / LLM / 鉴权 / 持久化全部变量与默认值）见 [docs/OPERATIONS.md](docs/OPERATIONS.md)。

本地开发最常用的只有：`PORT`（API 端口，默认 4801）、`GEC_OFFLINE=1`（强制离线检测）、`LLM_API_KEY`（启用讲解与 AI 复查）。多数配置可在「设置」界面直接修改（即时生效 + 持久化到 `GEC_SETTINGS_FILE`，优先级高于环境变量）；桌面端离线开关默认开启，与 api-server 的默认在线相反。

### 端口约定

本仓库自有服务统一使用 **4800+ 空闲段**（避免与常见服务端口冲突；各服务默认仅绑定本机）：

| 端口 | 服务 | 配置来源 |
|---|---|---|
| `4801` | api-server（Axum API） | `PORT`（Docker `EXPOSE 4801`） |
| `4802` | 日文 Sudachi 分词服务 | `SUDACHI_ENDPOINT` |
| `4803` | 统计 GEC 服务（Harper / HF transformers server 类） | `GEC_STAT_ENDPOINT` |
| `4804` | vegaml 决策模型服务（`POST /decide`；**实验层，默认不启动**——该层已退役出产品路径） | `GEC_DEC_ENDPOINT` / `decision-model` 的 `DEC_PORT` |
| `4805` | Vite dev server（前端开发 + Tauri devUrl） | `web-ui/vite.config.ts` / `tauri.conf.json` |
| `4806` | L3 特征模型服务（`POST /l3-detect`；飞轮：`POST /l3-retrain` / `GET /l3/status`） | `GEC_L3_ENDPOINT` / `feature-model` 的 `L3_PORT` |

外部依赖（不在本仓库、端口不纳入本约定）：HanLP NLP 服务（NarrativeOS，`HANLP_ENDPOINT`，默认 `127.0.0.1:8000`）、LLM 服务（`LLM_BASE_URL`，远端 OpenAI 兼容端点）。

## API

主入口：`POST /gec/detect` / `POST /gec/analyze`（`level` 三级响应见下节）；其余 40+
端点覆盖决策表热更新与版本回滚、知识源导入与资源包、规则开关、用户画像与运行时
设置。完整端点参考见 [docs/API.md](docs/API.md)。

## 三级响应（level 1 / 2 / 3）

检测接口（`/gec/detect`、`/gec/:lang/detect`、`/gec/analyze`）支持 `level` 参数（缺省 1），在前述「规则 → 决策」级联之上按需触发 LLM 兜底层：

| level | 行为 | LLM 触发条件 |
|---|---|---|
| 1（缺省） | 规则（+ 决策模型**实验层**，仅显式开启时） | 不触发 |
| 2 | 1 级 + **L3 特征模型门控**（默认开启；L3 是唯一的 pre-LLM 门控） | L3 **弃权**（灰区 `[0.25, GEC_L3_ABSTAIN_THRESHOLD)`）或**服务不可用**时触发；L3 高置信（有错 / 明确无错）时直接定案，**不调用 LLM**；L3 未启用时回退旧语义：决策模型**实验层**（默认退役关闭）明确弃权时 |
| 3 | 1 级 + **L3 特征模型**（默认开启）+ LLM 兜底 | **无条件**触发（「漏报」兜底）；特征模型高置信时直接定案，**不调用 LLM**（LLM 收缩到纯长尾） |

- LLM 兜底引擎按请求从运行时 LLM 参数 + 检测模板（`prompts/{lang}_detect_prompt.json`）现构建，支持设置热更新；
- LLM 产出的错误标记 `source: "llm"`（前端错误列表展示「LLM 兜底」标签）；特征模型产出的错误标记 `source: "feature"`（前端展示「特征模型」标签，含模式化定位 + 机械修正）；
- LLM 结果**不进 LRU 缓存**（非确定性 + 成本高）；level 1 结果照常缓存；缓存键包含规则指纹 + 模型版本（规则开关 / 决策表热更新 / 模型变更后旧结果自动失效，无需手动清理；极端情况可用 `refresh: true` 或 `POST /gec/cache/clear` 纠正，见 docs/OPERATIONS.md「检测缓存与纠正」）；
- 触发条件命中但 LLM 未配置（无 API Key / 缺检测模板）→ **503**（提示去设置界面配置）；LLM 服务调用失败 → **502**；
- 前端：编辑器提供「检查模式」选择器（快速检查 / 标准检查 / 深度扫描，对应 API `level` 1/2/3）与「深度重查」按钮（= level 3）。
- **训练飞轮（闭环）**：确认采纳的 LLM 检测结果（正样本）与复查确认干净的句子（负样本）汇入
  `feature-model/l3_data/<lang>/confirmed.jsonl`（`POST /gec/l3/collect`）；积累到阈值（新增 ≥ 10 /
  首次 ≥ 30，冷却 7 天）后设置页「L3 训练飞轮」卡片提示重训练——重训练 + dev 评测，F1 未显著
  退化即**热切换**模型，特征模型随使用变强、LLM 长尾持续收缩。
- **采纳资产沉淀**：检测同步写入「应用全部建议后」的纠正版（`detect_history.corrected_text`）；
  **采纳**时沉淀句级平行对 `adopted.jsonl`（去重、编辑密度 >0.4 打 `weak`、上限 20000），**放弃**
  时沉淀 `rejected.jsonl`。开关 `GEC_L3_ADOPT_COLLECT`（api-server 默认关、桌面端默认开）。

## 质量评测（公开基准）

评测 harness 在 `tools/gec-eval`：对四份金集逐句跑**规则层**（`detect_local`，
离线、确定性、无 LLM），统计 precision / recall / **F0.5**（GEC 评测惯例，β=0.5，
精确率权重是召回的 2 倍——纠错产品误报比漏报更伤信任）与每千字符延迟中位数
（release 构建）。命中判定与各语言 `tests/*_eval.rs` 一致：预测 span 与 gold
编辑 span 重叠（零宽 gold span 计入分母）。

```bash
# 打印 markdown 报告；--json 另存机器可读报告
cargo run --release -p gec-eval
# CI 回归门：与基线对比，任一数据集 P/R/F0.5 下降超过容差（默认 0.02）即失败
cargo run --release -p gec-eval -- --check datasets/eval_reports/baseline_rules.json
```

实测（2026-10，M 系列 Mac，release；延迟与机器相关，回归门只约束 P/R/F0.5）：

| 数据集 | 语言 | 句子 | gold 编辑 | 精确率 | 召回率 | F0.5 | 延迟 ms/千字符 |
|---|---|---:|---:|---:|---:|---:|---:|
| CSCD + SIGHAN-2015 / EC-Med / EC-LAW | zh | 2600 | 1792 | 0.3% | 0.1% | 0.002 | 59.7 |
| MuCGEC (dev) | zh | 1137 | 7258 | 84.4% | 0.5% | 0.024 | 52.7 |
| NaCGEC | zh | 6369 | 9460 | 0.0% | 0.0% | 0.000 | 31.8 |
| JFLEG | en | 1501 | 15775 | 93.1% | 1.2% | 0.058 | 14.4 |

**如何解读**：规则层刻意只做高置信词级错误（混淆字 / 拼写混淆），对通用语法金集
（MuCGEC / NaCGEC / JFLEG 的 gold 编辑以语法、语序编辑为主）召回低是设计内的
边界，不是缺陷——语法与语义纠错的召回由 L3 LLM 层承担（在线路径，用
`datasets/score_f05.py` 单独评测，不进 CI 门）。这张表的作用是**守住规则层现有
能力**：任何词表 / 规则改动导致误报上升或命中下降，CI 直接红。

**定位对照**（定性——各家评测口径不同且不可独立复核，不做跨产品定量对比）：

| 维度 | LangFixer | Harper | LanguageTool |
|---|---|---|---|
| 中文 GEC / 编校 | ✅ 核心场景（CEC 词表 + 中文金集） | ❌ 仅英文 | ⚠️ 中文为社区维护 |
| 完全本地 / 离线 | ✅ 规则层全离线，LLM 可选外接 | ✅ | ⚠️ 部分能力需云端账户 |
| 错误归因（为什么错） | ✅ 知识库归因 + 决策表 | ⚠️ 规则描述 | ⚠️ 规则描述 |
| 学习闭环（错题本 + FSRS 复习） | ✅ | ❌ | ❌ |

金集仅用于本地评测（许可见 `datasets/README.md`），产品发行物不含数据集。

## 测试

```bash
cargo test --workspace   # 各 crate 单元/集成测试
cd web-ui && npm test    # 前端单元测试（vitest：JDM 解析/序列化/diff）
cd web-ui && npm run typecheck && npm run build
```

## 路线图

当前实现聚焦**语法层**（检测 + 归因 + 决策表热更新），输入面已扩展到图片 / PDF / Word 载体。
已完成项（LLM 个性化讲解、统计 GEC 引擎挂载点、SQLite 持久化、vegaml 决策层评估与退役、
多模态 P0/P1）的过程与取证见 [CHANGELOG.md](CHANGELOG.md) 与 [EVOLUTION.md](EVOLUTION.md)。

- **游戏化层**（后续加强）：区域进阶、考试副本、成长系统，依赖 LLM 出题 + CEFR 难度分级 +
  用户进度持久化就位后启动。
- **ChineseErrorCorrector 吸收**（规划中）：CEC（ACL 2026）六项设计的落地方案
  （prompt 教学分类 / 混淆字集 / 评测基准矩阵 / L3 大规模重训练 / 专用模型 / 数据增强），
  见 [CEC_ADOPTION.md](CEC_ADOPTION.md)；数据清单见 [datasets/cec_datasets.md](datasets/cec_datasets.md)。
- **多模态与载体审校**（P0 结构层档 + P1 视觉档已落地）：输入面从纯文本扩展到图片 / PDF /
  Word——「文本单元投影 + 偏移回映射」层（新 crate `media-core/` + 编排层 `media-flow/`）挂
  既有检测级联，api-server 与桌面端都只是宿主；默认回写 = 叠加标注，每个 job 独立目录、
  **永不改原件**；可重演专家正误表并导出 docx / html / md / json（同一把尺子）。
  **诚实披露**：本地规则层对中文载体审校金集（`datasets/media_gold.json`）召回 0%——
  正对照稳定检出，瓶颈在中文词表覆盖（词典缺条目），不在抽取。开启方式、界面与
  命令契约见 [MULTIMODAL.md](MULTIMODAL.md)（§15 金集 / §16 P0 / §17 P1 / §18 重演导出）。

## 文档地图

| 文档 | 内容 |
|---|---|
| [CHANGELOG.md](CHANGELOG.md) | 版本变更历史（Keep a Changelog） |
| [CONTRIBUTING.md](CONTRIBUTING.md) | 开发环境、代码约定、提交与发布流程 |
| [SECURITY.md](SECURITY.md) | 漏洞报告渠道与支持版本 |
| [LICENSES-THIRD-PARTY.md](LICENSES-THIRD-PARTY.md) | 第三方依赖许可清单 |
| [docs/DETECT_PIPELINE.md](docs/DETECT_PIPELINE.md) | 检测处理流程设计（级联各层） |
| [docs/API.md](docs/API.md) | REST API 端点参考 |
| [docs/KNOWLEDGE.md](docs/KNOWLEDGE.md) | 规则与知识库设计（知识库架构 / 导入管线 / 规则派生 / 资源包，含「IP 边界」） |
| [docs/GATEWAY_AUTH.md](docs/GATEWAY_AUTH.md) | 评测通道（网关形态）鉴权设计 |
| [docs/OPERATIONS.md](docs/OPERATIONS.md) | 运维配置（环境变量矩阵） |
| [docs/RELEASING.md](docs/RELEASING.md) | 版本发布流程与产物 |
| [docs/UI_REDESIGN.md](docs/UI_REDESIGN.md) | 界面改造方案 |
| [docs/UX_REVIEW.md](docs/UX_REVIEW.md) | 产品交互设计评审 |
| [docs/desktop-hig.md](docs/desktop-hig.md) | 桌面端设计规范对照（macOS HIG） |
| [EVOLUTION.md](EVOLUTION.md) | 进化中心设计 |
| [MULTIMODAL.md](MULTIMODAL.md) | 多模态审校（图片 / PDF / Word 载体） |
| [CEC_ADOPTION.md](CEC_ADOPTION.md) | ChineseErrorCorrector 吸收方案 |
| [deploy/gateway/README.md](deploy/gateway/README.md) | 评测通道部署（网关形态） |

## 社区与支持

- **参与贡献**：开发环境、代码约定与提交 / 发布流程见 [CONTRIBUTING.md](CONTRIBUTING.md)；
- **问题反馈**：[GitHub Issues](https://github.com/narrativeos/LangFixer/issues)；
- **漏洞报告**：请勿开公开 issue，报送渠道见 [SECURITY.md](SECURITY.md)；
- **变更历史**：[CHANGELOG.md](CHANGELOG.md)（Keep a Changelog + SemVer；发布流程见 [docs/RELEASING.md](docs/RELEASING.md)）。

## 许可

- **代码与内置资产**（`decision-tables/`、`lexicons/`、`prompts/` 等产品自带内容）：
  [Apache-2.0](LICENSE)（全文见仓库根目录 `LICENSE`）。
- **第三方依赖**：许可清单见 [LICENSES-THIRD-PARTY.md](LICENSES-THIRD-PARTY.md)
  （按 `Cargo.lock` / `package-lock.json` 生成，均为 MIT / Apache-2.0 / BSD / ISC 等
  与 Apache-2.0 兼容的宽松许可）。
- **外部语料**（`datasets/`）：**第三方研究语料**，保留各自原始许可（JFLEG 为
  非商业研究语料、MuCGEC 为 Apache-2.0、CSCD / NaCGEC / CTCDataset 衍生集为研究
  用途等），**不在本项目 Apache-2.0 授权范围内**；仅用于本地评测，产品发行物不含
  本目录，逐集许可清单见 [datasets/README.md](datasets/README.md)。
- **知识源内容**（`knowledge-base/`）：仓库内置的唯一知识源是**自撰示例源**
  `src-sample-en`（LangFixer 示例英语语法指南，234 个示例知识点），随仓库以
  Apache-2.0 授权分发。用户经导入管线摄取的书籍属**用户数据**：权利归原权利人，
  本仓库不授予任何教材内容许可；`license_scope != public` 的源不得随发行物分发
  （机制见 [docs/KNOWLEDGE.md](docs/KNOWLEDGE.md)「IP 边界」）。

