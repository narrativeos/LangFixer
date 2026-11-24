# LangFixer

[![CI](https://github.com/narrativeos/LangFixer/actions/workflows/ci.yml/badge.svg)](https://github.com/narrativeos/LangFixer/actions/workflows/ci.yml)
[![version](https://img.shields.io/github/v/tag/narrativeos/LangFixer?label=version&color=blue)](https://github.com/narrativeos/LangFixer/releases)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache--2.0-blue.svg)](LICENSE)

**LangFixer：会教你为什么做错的语法检查器。**
除了指出中 / 英 / 日文本里的错误，它还讲**这个错为什么会犯**，并给出知识点讲解和分级练习——目标不是「改对这一句」，而是「消灭这一类错」，这就是把语法纠错做成语法教学；你采纳的反馈会沉为训练数据，**越用越准**。个人装在自己机器上完全离线用；团队和机构自部署在内部网络，词表、术语与知识规则按自己的领域持续定制，文本默认不出域。

**线上体验** [langfixer.jiulu.ltd](https://langfixer.jiulu.ltd/) · **桌面版下载** [GitHub Releases](https://github.com/narrativeos/LangFixer/releases)

## 目录

- [特性一览](#特性一览)
- [产品定位](#产品定位)
- [快速开始](#快速开始)
- [部署：硬件要求](#部署硬件要求)
- [项目结构](#项目结构)
- [API](#api)
- [三级响应（level 1 / 2 / 3）](#三级响应level-1--2--3)
- [质量评测（公开基准）](#质量评测公开基准)
- [测试](#测试)
- [路线图](#路线图)
- [文档地图](#文档地图)
- [社区与支持](#社区与支持)
- [许可](#许可)

## 特性一览

**这是什么**：会「教」的语法检查器——指出中 / 英 / 日文本的错误后，还回答「这个错为什么会犯」和「该怎么补」，并把用户的采纳反馈沉为训练数据，越用越准。英文检测最成熟；中文覆盖词典规则类目（同音/形近错别字、的地得、标点排版、时间格式混用等）；日文为早期阶段。

**给谁用**：个人——语言学习者与专业编校者，对「检测 → 归因 → 人工确认 → 采纳」这套范式零先验，共用同一个工作台、引导与信任机制；团队与机构——自部署后词表、决策表与知识库按自己的领域持续定制，邀请制账号全组共用；平台运营者（决策表 / 词表 / 知识库 / 进化中心）经管理功能进入。

| 特性 | 说明 |
|---|---|
| 🔍 检测 | 中 / 英 / 日三语，规则层完全离线可用（外部 HanLP / Sudachi 服务不可达时自动降级为本地词典 / 规则）；词表外置 JSON 不重编译即可扩充，另有领域术语库防误纠 |
| 🧭 归因 | ZEN 决策表（GoRules JDM 标准）输出归因标签 + 韦纳三维（内外源 × 稳定性 × 可控性）+ **CEFR A1–C2 难度分级** + 补救措施 + 决策追踪 |
| 📚 知识库 | 知识源导入 → 挖掘知识点 → 派生决策表规则 → 人工确认发布，一本书的端到端自学习已产品化（[docs/KNOWLEDGE.md](docs/KNOWLEDGE.md)） |
| 🎓 教学 | LLM 个性化讲解（提示词按语言 × CEFR 分级）+ 错题本与 FSRS 复习学习闭环 |
| 🔄 训练飞轮 | 确认采纳汇入 L3 训练样本，积累到阈值重训练 + dev 评测、F1 不退化即热切换——特征模型随使用变强、LLM 长尾持续收缩 |
| 🖼 载体审校 | 图片 / PDF / Word 直接审校：在原件上**叠加标注、永不改原件**，可重演专家正误表并导出——编校 / 出版 / 内容平台可用在自己的稿流程 |
| 🖥 离线桌面 | Tauri 2 桌面端完全离线运行；Axum API 服务（Docker 化）+ Vue 3 Web，两端共享同一套核心引擎 |

**做不到什么**：**不保证全量召回**——本地规则层对中文载体审校金集召回 0%（见下文「路线图」与 [MULTIMODAL.md](MULTIMODAL.md)）；LLM 复查需自行配置端点且有成本；它不是风格编辑器，不改写内容。能力边界以各语言词表实际覆盖与测评中心实测报告为准。

## 产品定位

用业界通用术语概括，LangFixer 的定位是两个词：**自学习飞轮**与**本地私域**。

**自学习飞轮**（data flywheel——「使用数据驱动模型持续改进的自提升闭环」的标准提法）：
LangFixer 把它在产品里走成了检测 → 反馈 → 反思 → 训练 → 测评的完整回路——采纳确认
自动汇入 L3 训练样本，积累到阈值重训练、dev 评测门槛不退化即热切换；LLM 层的
「采纳 + 修正」反馈沉淀为 SFT 训练数据（可导出供微调）。能力的增长来自领域数据的
沉淀而非模型规模堆砌，因此**越用越准**是架构属性而非宣传语（机制详见
[EVOLUTION.md](EVOLUTION.md)）。

**本地私域**（local-first / self-hosted——数据与算力在自己手里的部署形态）：桌面端
完全离线运行，服务器形态自部署；规则层与 L3 特征模型跑在自己的机器上，文本只在
显式配置了 LLM 端点时才外送复查讲解，不配置不影响离线能力；网关形态的日志与保留
策略同样以「不记录用户文本」为红线（见 [docs/GATEWAY_AUTH.md](docs/GATEWAY_AUTH.md)）。

**个人与机构是两条平行线**：前者要的是装在自家机器上、完全离线的私人工具；后者
要的是装在自己内网里的编校基础设施——文本不出域，词表、决策表与知识库按本机构
的领域和行规持续定制，反馈沉下来变成机构自己的资产而非某家的训练集。两者同一个
内核：个人侧桌面端开箱即用，机构侧 `./setup.sh server` 自部署后由管理面承接定制。

**与云端大模型服务互补，而非替代**：LangFixer 不以替代资源与模型规模更大的云端
服务为目标——云端大模型负责通用能力与长尾复查，本地环境负责私域数据、领域定制与
持续沉淀，把「这个领域、这个用户的错」学成自己的资产。这正是自托管类工具的共同
姿势：本地能独立干活，云端按需接入，二选一是错的，组合才是对的。

## 快速开始

**马上体验**：浏览器打开 [langfixer.jiulu.ltd](https://langfixer.jiulu.ltd/) 即用，无需安装；桌面端完全离线可用（本地规则层），安装包见 [GitHub Releases](https://github.com/narrativeos/LangFixer/releases)。

**一行 API**（对已启动的本机 api-server，默认 `:4801`）：

```bash
curl -s http://127.0.0.1:4801/gec/detect \
  -H 'Content-Type: application/json' \
  -d '{"lang":"en","text":"She go to school yesterday."}'
```

**从源码跑**：

```bash
./scripts/dev.sh      # 本地开发（API :4801 + 前端 :4805，Vite 代理 /gec /student）
./scripts/build.sh    # 全量构建
./scripts/release.sh  # 发布（release + 桌面安装包）
./setup.sh local      # 首次部署并启动（local = 本机网关；server = 公网评测实例）
./update.sh server    # 日常更新（后端二进制 + 前端 dist + 运行时资产，失败自动回滚）
```

部署脚本的目标地址、服务名等可用 `LF_SSH / LF_URL / LF_DIR / LF_SRC / LF_WEB_ROOT /
LF_SERVICE / LF_PORT` 覆盖；两条命令的落地约定与红线见
[`deploy/gateway/README.md`](deploy/gateway/README.md)。VS Code 调试配置见
[CONTRIBUTING.md](CONTRIBUTING.md)。

**环境变量**：本地开发通常只需 `PORT`（默认 4801）、`GEC_OFFLINE=1`（强制离线）、
`LLM_API_KEY`（启用讲解与 AI 复查）。多数配置可在「设置」界面直接改（即时生效 +
持久化，优先级高于环境变量）；完整矩阵见 [docs/OPERATIONS.md](docs/OPERATIONS.md)。
本仓库自有服务统一使用 4800+ 端口段，清单见 docs/OPERATIONS.md「端口约定」。

**桌面端**：Tauri 内前端经 `invoke()` 直连 Rust 命令，**完全离线**、不依赖
api-server；窗口大小 / 位置自动持久化、下次启动恢复，桌面布局与设置持久化细节见
[docs/desktop-hig.md](docs/desktop-hig.md)。

## 部署：硬件要求

两种形态，硬件要求差一个量级。

### PC 单机（自用为主）

桌面安装包（Tauri，Win / macOS）：本地规则层 + 词表**完全离线**，普通办公机
（2 核 / 4GB）即用；③ LLM 讲解 / 复查可选——在「设置」配外部 API 即可，无需自备
GPU 或跑模型。想在本机启用 level 2 NLP 层的，可自起 HanLP sidecar（镜像来自姊妹
项目 narrative-operator，Docker，~1.3GB 常驻）。`./setup.sh local` 的 web 形态与
服务器同构，按下方服务器要求。

### 服务器（团队 / 公开实例）

核心能力（规则层 + NLP 层 + SQLite）**纯 CPU，无 GPU**；③ LLM 复查默认接外部
OpenAI 兼容 API，零硬件成本。各部件需求与 2026-11 生产实例实测：

| 部件 | 需求 | 实测（生产） |
|---|---|---|
| api-server（Rust 单二进制 + SQLite） | 2 核 / 1GB | 启动常驻 ~20MB，决策表 / 词典按需加载 |
| HANLP NLP sidecar（docker，**外部依赖不在本仓**） | 2 核 / 4GB | 常驻 ~1.3GB，内存大头 |
| L3 深扫 worker 容器 | 1 核 / 512MB | 常驻 ~116MB |
| Caddy 反代 | 忽略不计 | ~30MB |
| 磁盘 | 全新装 ≥10GB 空闲 | SQLite 单库随反馈与缓存增长 |

**最小配置 4 核 / 8GB / 20GB 磁盘**即可跑全部 web 形态与三级响应；生产参考机型
为 4 核 / 16GB 云主机（同机还并跑着别的服务，整机占用约 3.7GB）。

HANLP sidecar 与日文侧的 Sudachi 服务一样是**可选的外部服务**（镜像由姊妹项目
NarrativeOS narrative-operator 构建）：不部署也能跑，中文 / 英文自动降级为本地
词典与规则层；起法见 [`deploy/gateway/README.md`](deploy/gateway/README.md)
「中文 NLP sidecar」节。

**GPU 可选**（仅自持 LLM 臂需要，核心能力不需要）：

- ③ 自持臂 arm3（Qwen3-4B 级，vLLM）：单张 RTX 3090（24GB）；
- 夜间判官 32B-Q4：单张 RTX 3090，常驻与训练互斥需排班。

CPU 推理可跑但延迟满足不了 ③ 钩子预算，不建议。反向代理与端口约定见
[docs/OPERATIONS.md](docs/OPERATIONS.md)。

## 项目结构

```
LangFixer/
├── Cargo.toml            # workspace（14 个 Rust crate）
├── shared-model/         # 统一数据模型（Error / 归因 / 用户 / 决策表元数据）
├── gec-core/             # GECDetector trait + GecEngine 引擎路由（rule/statistical/llm）+ 语言路由
├── gec-zh/               # 中文检测器（HanLP /analyze + 规则，离线兜底）
├── gec-en/               # 英文检测器（HanLP /analyze + 规则，离线兜底）
├── gec-ja/               # 日文检测器（Sudachi /parse + 规则，离线兜底）
├── attribution-engine/   # ZEN 决策表引擎 + 热更新 TableStore + 知识库 KnowledgeStore + 韦纳映射
├── langfixer-llm/        # LLM 扩展（OpenAI 兼容客户端 + SSE 流式 + 提示词模板）
├── lexicon-store/        # SQLite 词表存储（规则定义 + 规模化示例对，WAL + FTS5）
├── document-parse/       # 书本文件解析（epub / txt / markdown / docx / pdf → 结构化正文）
├── media-core/           # 多模态审校核心层（文本单元投影 + 偏移回映射）
├── media-flow/           # 多模态审校编排层（挂载既有检测级联）
├── decision-model/       # 决策模型（vegaml，Python）：实验路径（已退役出产品）
├── feature-model/        # L3 特征模型（Python）：FastAPI 服务 + 训练工作流 + 训练飞轮
├── api-server/           # Axum API 服务（Dockerfile 内置）
├── desktop-app/          # Tauri 2 桌面端（离线运行）
├── web-ui/               # Vue 3 + Vite + Pinia 前端
├── tools/gec-eval/       # 公开基准评测 harness（CI 回归门）
├── lexicons/             # zh / en / ja 三张 GEC 规则词表（JSON，可热扩充）
├── prompts/              # LLM 提示词模板（3 语言 × CEFR 分级 + generic + detect 兜底，版本化）
├── decision-tables/      # zh / en / ja 三张 JDM 决策表（纯规则：错误类型 × 新手/置信度 → 归因策略）
├── knowledge-base/       # 知识库：知识源注册表（sources.json）+ 知识点（points/{lang}.json，1:N 背书）
│                         #   仅内置自撰示例源（Apache-2.0）；用户导入的书籍属用户数据、
│                         #   不随发行物分发（IP 边界见 docs/KNOWLEDGE.md）
└── scripts/              # build.sh / dev.sh / release.sh
```

规则与知识是分离的两层：**决策表**只承载可提取的规则（错误类型 × 新手/置信度 →
归因策略 + 通用补救 + CEFR），随产品发布；**知识库**承载知识内容与知识源（1:N 背书
关联），用户导入的书籍属用户数据、按 IP 边界不进发行物。完整机制见
[docs/KNOWLEDGE.md](docs/KNOWLEDGE.md)。

## API

主入口：`POST /gec/detect` / `POST /gec/analyze`（`level` 三级响应见下节）；其余 40+
端点覆盖决策表热更新与版本回滚、知识源导入与资源包、规则开关、用户画像与运行时
设置。完整端点参考见 [docs/API.md](docs/API.md)。

## 三级响应（level 1 / 2 / 3）

检测接口（`/gec/detect`、`/gec/:lang/detect`、`/gec/analyze`）支持 `level` 参数（缺省 1），在「规则 → 决策」级联之上按需触发 L3 特征模型门控与 LLM 兜底层：

| level | 行为 | LLM 触发条件 |
|---|---|---|
| 1（缺省） | 规则（+ 决策模型**实验层**，仅显式开启时） | 不触发 |
| 2 | 1 级 + **L3 特征模型门控**（默认开启，唯一的 pre-LLM 门控） | L3 **弃权**（灰区）或**服务不可用**时触发；L3 高置信直接定案、**不调用 LLM** |
| 3 | 1 级 + L3 特征模型 + LLM 兜底 | **无条件**触发（「漏报」兜底）；特征模型高置信直接定案，LLM 收缩到纯长尾 |

编辑器提供「检查模式」选择器（快速检查 / 标准检查 / 深度扫描，对应 `level` 1/2/3）与「深度重查」按钮；LLM 产出标 `source: "llm"`、特征模型产出标 `source: "feature"`，前端以标签区分。LLM 兜底引擎按请求现构建（支持热更新）、结果不进缓存；缓存失效机制与 503/502 语义见 [docs/OPERATIONS.md](docs/OPERATIONS.md)「检测缓存与纠正」，训练飞轮与采纳资产沉淀见 [EVOLUTION.md](EVOLUTION.md) §5 / §9。

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

**如何解读**：规则层刻意只做高置信词级错误，对通用语法金集召回低是设计内的边界而非缺陷——语法与语义纠错的召回由 L3 LLM 层承担（在线路径单独评测，不进 CI 门）。这张表的作用是**守住规则层现有能力**：任何词表 / 规则改动导致误报上升或命中下降，CI 直接红。

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

| 状态 | 方向 | 说明 |
|---|---|---|
| ✅ 已完成 | LLM 个性化讲解、统计 GEC 引擎挂载点、SQLite 持久化、vegaml 决策层评估与退役、多模态 P0/P1 | 过程与取证见 [CHANGELOG.md](CHANGELOG.md) 与 [EVOLUTION.md](EVOLUTION.md) |
| 🚧 后续 | 游戏化层：区域进阶、考试副本、成长系统 | 依赖 LLM 出题 + CEFR 难度分级 + 用户进度持久化就位后启动 |
| 📋 规划 | ChineseErrorCorrector（CEC，ACL 2026）六项设计吸收 | 落地方案见 [CEC_ADOPTION.md](CEC_ADOPTION.md)，数据清单见 [datasets/cec_datasets.md](datasets/cec_datasets.md) |

**多模态与载体审校**（P0 结构层档 + P1 视觉档已落地）：输入面从纯文本扩展到图片 /
PDF / Word——「文本单元投影 + 偏移回映射」层（`media-core/` + `media-flow/`）挂既有
检测级联；默认回写 = 叠加标注，每个 job 独立目录、**永不改原件**；可重演专家正误表
并导出 docx / html / md / json。**诚实披露**：本地规则层对中文载体审校金集
（`datasets/media_gold.json`）召回 0%——正对照稳定检出，瓶颈在中文词表覆盖，不在
抽取。开启方式与命令契约见 [MULTIMODAL.md](MULTIMODAL.md)。

## 文档地图

| 文档 | 内容 |
|---|---|
| [CHANGELOG.md](CHANGELOG.md) | 版本变更历史（Keep a Changelog） |
| [CONTRIBUTING.md](CONTRIBUTING.md) | 开发环境（含 VS Code 调试）、代码约定、提交与发布流程 |
| [SECURITY.md](SECURITY.md) | 漏洞报告渠道与支持版本 |
| [LICENSES-THIRD-PARTY.md](LICENSES-THIRD-PARTY.md) | 第三方依赖许可清单 |
| [docs/DETECT_PIPELINE.md](docs/DETECT_PIPELINE.md) | 检测处理流程设计（级联各层） |
| [docs/API.md](docs/API.md) | REST API 端点参考 |
| [docs/KNOWLEDGE.md](docs/KNOWLEDGE.md) | 规则与知识库设计（知识库架构 / 导入管线 / 规则派生 / 资源包，含「IP 边界」） |
| [docs/GATEWAY_AUTH.md](docs/GATEWAY_AUTH.md) | 评测通道（网关形态）鉴权设计 |
| [docs/OPERATIONS.md](docs/OPERATIONS.md) | 运维配置（环境变量矩阵 / 端口约定 / 检测缓存与纠正） |
| [docs/RELEASING.md](docs/RELEASING.md) | 版本发布流程与产物 |
| [docs/UI_REDESIGN.md](docs/UI_REDESIGN.md) | 界面改造方案 |
| [docs/UX_REVIEW.md](docs/UX_REVIEW.md) | 产品交互设计评审 |
| [docs/desktop-hig.md](docs/desktop-hig.md) | 桌面端设计规范对照（macOS HIG） |
| [EVOLUTION.md](EVOLUTION.md) | 进化中心设计 |
| [MULTIMODAL.md](MULTIMODAL.md) | 多模态审校（图片 / PDF / Word 载体） |
| [CEC_ADOPTION.md](CEC_ADOPTION.md) | ChineseErrorCorrector 吸收方案 |
| [deploy/gateway/README.md](deploy/gateway/README.md) | 评测通道部署（网关形态） |

## 社区与支持

- **如果这个项目帮到了你**：点个 ⭐ Star 是对我们最大的支持；
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
