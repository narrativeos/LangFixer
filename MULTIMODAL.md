# 多模态审校（Multimodal Proofreading）

LangFixer 的输入面从「纯文本」扩展为「**载体（carrier）→ 文本单元 → 既有检测/归因内核 → 回写载体**」，
外部协作工具（PhotoCraft / WordCraft / PdfCraft）承担「打开载体 + 回写标注」两端，
LangFixer 承担「取文本、校对、归因、决定怎么标、把标注指令翻译给协作工具」中段。

设计基线：**检测内核不动**。`GECDetector::detect(&str) -> Vec<Error>`（`Error.span` 仍是字节偏移）
是所有多模态形态的共同内核；多模态只是在它外面加一层**投影（projection）与回映射（remap）**。

## 0. 一句话流程

```
上传 image/docx/pdf
  → 能力探测（inspect）：能不能拿到可编辑文本对象？有几页/多大？
  → 取文本（三档：结构层 > 视觉 > OCR）→ TextUnit[]（每单元带 region）
  → 拼接成 stitched text + offset map（unit_id ↔ 字节区间）
  → POST /gec/detect 语义（level / force_llm 不变）→ Error[]（字节 span）
  → 回映射：span → (unit, 单元内局部 span) → region → Annotation[]
  → 人工勾选（Confirm 门）
  → 回写：协作工具原生命令（无损改写 / 叠加标注 / 批注·修订）
  → 工件：可编辑源文件（.psd/.docx/.pdf）+ 扁平预览（.png/.pdf）+ annotations.json
  → 采纳/放弃回流 feedback_log（图片误报同样进飞轮）
```

## 1. 已验证事实基线（2026-10-26 实测，勿凭记忆实现）

### 1.1 PhotoCraft CLI（本机已装 `~/.local/bin/photocraft-cli`，`0.16.0 (b1794c6e0, 2026-10-25)`）

子命令：`convert · info · run · batch · droplet · commands · mcp · serve`。命令注册表 **940 条**
（`photocraft-cli commands`），`commands --json --filter <TEXT>` 返回每条的 `params` 模式串——
这是实现标注指令时的**权威参数文档**，可直接解析成工具面。

单发模式（已跑通，端到端可用）：

```sh
photocraft-cli run <FILE> \
  --cmd shape.create --params '{"kind":"rect","rect":[40,40,300,120],"fill":null,"stroke":{"width":3,"color":"#e03131"},"name":"mark-1-box"}' \
  --cmd type.create  --params '{"x":50,"y":90,"text":"Hello world","size":24,"color":"#e03131","name":"mark-1-text"}' \
  --out out.psd
```

实测要点：

| 事实 | 结论 |
|---|---|
| 每条 `--cmd` 逐行打印 `{"command":..,"result":..}`；`shape.create` 返回 `bounds/layer/path/stroke`，`type.create` 返回 `bounds/layer` | 命令返回的 `bounds` 可直接用作标注落点校验与 UI 命中区 |
| 输出 `.png` 时打印 `warning: N layer(s) flattened; layers, masks and blend modes are not kept` | **必须双输出**：`.psd`（分层，用户可继续编辑）+ `.png`（预览/给视觉模型看） |
| `type.create` params：`{x,y}`（point text 基线）/ `{box:[x,y,w,h]}`（paragraph text）/ `text` / `name` / `align` / `orientation` / `font` / `size`(pt) / `color` | 长建议用 `box`（自动换行），短标记用 `x,y` |
| `type.edit` params：`{layer, text}`（整层替换，样式保留）/ `{layer, replace:{start,end,text}}`（**字符区间替换**）/ `runs[]`（分段样式）/ `box` / `point` / `move` / `transform` | 图片里的**文字层可无损改字**——这是「改原文」而非「贴标记」的关键能力 |
| `edit.findAndReplaceText {find,replace,action:changeAll\|find\|change\|changeFind,caseSensitive,wholeWord,allLayers}` | 批量替换的兜底路径（不做位置映射时可用） |
| `layer.new.group {name}` → `{layer}`；`layer.setProps {layer,name,visible,opacity,blend,locked...}` | 所有 LangFixer 标注收进一个组（`LangFixer Marks`），一键显隐/删除 |
| `type.fonts` → `result.families[]`：含 `Heiti SC / Kaiti SC / Hiragino Sans GB / LXGW WenKai / Alibaba PuHuiTi 2.0 / BIZ UD*` 等 CJK；**默认字体是 Inter** | 中文标注必须显式指定 CJK family（实测指定 `Heiti SC` 与默认渲染结果不同：暗像素带签名 `1620` vs `1736`，逐行签名不等） |
| `info <FILE>` → size/mode/depth/layer tree JSON（`--compact` 单行） | 上传后先 `info` 做能力探测 |

常驻会话（批量/低延迟路径）：

```sh
photocraft-cli serve [--port 4812] \
  --automation-read-root  /abs/work/job-42 \
  --automation-write-root /abs/work/job-42
# 每行一个请求：{"id":1,"method":"doc.open","params":{"path":"/abs/work/job-42/a.psd"}}
```

methods（实测 `{"method":"methods"}` 返回）：
`engine.execute · jobs.list · jobs.cancel · engine.commands · session.list · doc.open · doc.place ·
doc.replaceSmartObject · doc.new · doc.save · doc.exportLayers · doc.exportDataSets ·
doc.importDataSets · doc.inspect · doc.render · doc.select · doc.close · batch · methods`

实测坑（必须写进实现）：

- **不给 automation root 就拒绝绝对路径**：`automation path rejected: absolute paths are not allowed`。
  → 每个 job 一个工作目录，`--automation-read-root/--automation-write-root` 都指向它。
- `doc.inspect` 返回 `layers[]`，文字层带 `text:{font,sizePt,text}` 与 `bounds:[x,y,w,h]`，
  组带 `children[]`。→ **PSD/分层源文件的文字无需 OCR，直接读出来，且能无损改回去。**
- `doc.render {path}` → `{bytes, path}`（合成后的扁平 PNG）→ 视觉档的输入、UI 预览的产物。
- 冷启动不便宜（universal 二进制 ~146 MB）：单发 `run` 每次约数秒，批量必须走 `serve` 或 `batch`。

桌面 App 在环（用户正打开 PhotoCraft 时，标注实时出现在他眼前）：

```sh
/Applications/PhotoCraft.app/Contents/MacOS/PhotoCraft --control 7878 \
  --control-token-file /private/path/photocraft-control.token \
  --automation-read-root /work/project --automation-write-root /work/project
photocraft-cli mcp --bridge 127.0.0.1:7878      # 或直连 TCP：先 auth 帧，再 JSON lines
```

控制协议要点（官方文档）：每连接第一帧必须是
`{"id":"auth","method":"auth","params":{"token":"<64 hex>"}}`；token 文件 0600；
60s 超时有两种语义——`timeout: not run`（未派发，可安全重发）与
`timeout: no reply in 60s`（已派发，可能已生效，**重发前先 inspect**）；
自动化下 `file.open/file.save/file.saveAs` 一律被拒，必须用 capability-scoped 的
`doc.open/doc/save`（`file.new` 例外）。

### 1.2 同类协作工具（同族约定一致，但**参数名不同**，必须各自适配）

| 载体 | 工具（本机已装版本） | 取文本 | 定位 | 回写标注 | 会话/传输 |
|---|---|---|---|---|---|
| 图片 | **PhotoCraft** `photocraft-cli 0.16.0` | `info` / `doc.inspect`（文字层 `text.text`） | 同命令返回 `bounds`（px） | `type.edit`（无损改字）/ `shape.create`+`type.create`（叠加） | `run` / `serve` / `mcp --bridge` |
| PDF | **PdfCraft** `pdfcraft-cli 0.14.0` | `text`（阅读顺序，页间 `\f`）/ 工具 `text_extract` `text_lines` `text_paragraphs` | **`text_find` 返回 `matches[].rects`（points，左上原点）** | `comment_add`（`highlight/underline/squiggly/replace/rectangle/textbox…`）、`comment_reply/edit/delete`、`page_add_text`、`text_edit`（改 PDF 文本，字体仅 helvetica/times/courier） | `run <tool> k=v [--root DIR]` / `run --script steps.json --report`（**一个会话**）/ `mcp [--root]` / `ui --control FILE` |
| Word | **WordCraft** `wordcraft-cli 0.11.0` | `text` / `inspect`（blocks + runs + `parts[]`） | `inspect` 的 `blocks[].runs[]`（段落内字符区间 `start/end`） | **`review.trackChanges` + `select.text` + `text.insert`（跟踪替换）**、`review.newComment`（选中处批注）、`edit.replaceAll` | `run --file F --cmd 'id={json}' --save OUT --author NAME` / `mcp [--connect HOST:PORT]` |

实测规模：`pdfcraft-cli tools` = **179 个工具**（含 `ocr_recognize` / `ocr_status` / `doc_revisions` /
`image_hit` / `text_edit`）；`wordcraft-cli commands` = **643 条命令**（含 `review.*` 全套修订与批注）。

PdfCraft 工具调用形态（与 PhotoCraft 完全不同，必须各自实现）：

```sh
pdfcraft-cli run comment_add doc=1 page=1 type=squiggly find=wrold color=red \
                   author=LangFixer contents="#1 spelling" --root "$JOB_DIR"
pdfcraft-cli run --script steps.json --root "$JOB_DIR" --report
# steps.json = [{"tool":"doc_open","args":{"path":"…"}}, {"tool":"comment_add","args":{…}}, …]
# --report → {"ok":bool,"steps":[{"step":n,"tool":id,"status":"ok","result":[{…}]}]}
#            ↑ result 恒为「单元素数组」；doc id 在同一 script 会话内保持
pdfcraft-cli doc_save 需显式 {"doc":1,"path":"…"}；默认增量写（保留原字节），full=true 才整体重写
```

WordCraft 命令形态（`params` 是内联 JSON，`--author` 决定修订/批注署名）：

```sh
wordcraft-cli run --file in.docx --author LangFixer \
  --cmd 'review.trackChanges={"value":true}' \
  --cmd 'select.text={"text":"wrold"}' \
  --cmd 'text.insert={"text":"world"}' \
  --cmd 'select.text={"text":"He'"'"'llo"}' \
  --cmd 'review.newComment={"text":"#2 spelling: Hello"}' \
  --save out.docx
```

> 注意：三家 flag 名不一致（`--automation-read-root` vs `--root`；`--bridge` vs `--connect`），
> 所以「transport」必须是适配器各自实现，不能共用一个命令行拼装器。

### 1.3 已验证配方（P0 直接照抄，`./scripts/media_smoke.sh` 就是它的可执行契约）

```sh
# ① 造/开载体 → ② 读文字层（A 档，零 OCR）→ ③ 无损改字 + 叠加标注 → ④ 预览 → ⑤ 入组
photocraft-cli run --new '{"width":900,"height":300,"background":"white"}' \
  --cmd type.create --params '{"x":60,"y":120,"text":"He'"'"'llo wrold","size":48,"name":"headline"}' \
  --out in.psd
photocraft-cli info in.psd --compact      # → layers[].kind=="Type" 带 text:{font,sizePt,text} + bounds
photocraft-cli run in.psd \
  --cmd type.edit     --params '{"layer":3,"replace":{"start":0,"end":6,"text":"Hello"}}' \
  --cmd layer.new.group --params '{"name":"LangFixer Marks"}' \
  --cmd shape.create  --params '{"kind":"rect","rect":[52,72,340,64],"fill":null,"stroke":{"width":3,"color":"#e03131"},"name":"mark-1-box"}' \
  --cmd type.create   --params '{"x":56,"y":200,"text":"#1 spelling","size":20,"font":"Heiti SC","name":"mark-1-text"}' \
  --out out.psd
photocraft-cli run out.psd --cmd layer.moveTo \
  --params '{"layers":[5,6],"target":4,"position":"into"}' --out result.psd   # → {"moved":2}
photocraft-cli convert result.psd result.png                                  # 预览（会 flatten）
```

实测断言（脚本已验证，实现必须保持）：

- `type.edit {replace:{start,end,text}}` 后文字层文本 `He'llo wrold → Hello wrold`，
  **字体 `Inter` / 字号 `48pt` / 颜色全部保留**，`bounds` 自动重算 → 「改原文」是真无损。
- `layer.new.group` 建的是**空组**，之后创建的层不会自动进组（实测 `children: []`）。
  入组必须显式 `layer.moveTo {"layers":[…],"target":<组 id|名>,"position":"into"}`（实测 `{"moved":2}`）；
  备选：`layer.new.groupFromLayers {layer}` / `layer.groupLayers`（作用于选中层，配 `layer.select`）。
- **图层 id 会在结构变动后重编号**（实测入组后组 id 4 → 6）→ 绝不跨命令缓存 id，
  结构性操作后必须重新 `info` 取 id。
- `type.info {layer}` 返回 `text / runs（字符偏移 + 样式）/ paragraphs / transform /
  laid-out lines（text space px）/ bounds`；`type.edit` 的返回里已含
  `lines:[{start,end,x0,x1,ascent,descent}]` + `transform` →
  **可以按字符区间算出精确像素几何**：文字层上的错误不是画整层框，而是画到具体那几个字（下划线/紧框）。
  这是「A 档」比视觉档更强的地方，UI 上应体现为更紧的高亮。


### 1.4 已验证配方：PDF 批注 + Word 修订/批注（`./scripts/media_smoke_p2.sh` = 可执行契约，PASS）

```sh
# PDF：定位 → 波浪线 + 替换批注 → 存盘 → 读回
pdfcraft-cli run --script steps.json --root "$JOB" --report
# steps: doc_open{path} → text_find{doc,query} → comment_add{doc,page,type:"squiggly",find,color,author,contents}
#      → comment_add{type:"replace",find,contents} → comment_list{doc} → doc_save{doc,path}
# Word：修订 + 批注
wordcraft-cli run --file in.docx --author LangFixer --cmd 'review.trackChanges={"value":true}' \
  --cmd 'select.text={"text":"wrold"}' --cmd 'text.insert={"text":"world"}' \
  --cmd 'select.text={"text":"He'"'"'llo"}' --cmd 'review.newComment={"text":"#2 spelling"}' --save out.docx
```

实测断言（实现必须保持）：

- `text_find` 命中即给 `matches[].rects = [x0,y0,x1,y1]`（**points，从显示页左上角**）
  → PDF 的 `Region::PageBox` 直接由它构造，无需渲染后自己算坐标。
- `comment_add` 返回 `{comment:{id,index,page}, dirty:true, undo:"Add squiggly underline"}`；
  `comment_list` 读回的 **type 首字母大写**（`Squiggly` / `StrikeOut` / `Caret` / `Highlight`），
  且 `type:"replace"` 被落成**两条**：`StrikeOut` 批注 + 携带建议文本的 `Caret` 回复
  （PDF 规范没有 replace 注释类型）。回写幂等/删除时必须按这两条处理。
- Word：`info` 读回 `{"comments":1,"trackChanges":true,"properties.lastModifiedBy":"LangFixer"}`；
  `inspect` 的 `blocks[].runs[]` 带 `props:{ins:n}` / `props:{del:n}`（跟踪替换），
  `parts[]` 里 `kind:"comment"` 带批注文本；产物内是**真 OOXML**（`word/comments.xml`），Word/WPS 能逐条接受/拒绝。
- **陷阱**：跟踪替换后，`inspect` 段落 `text` 会把插入与删除的文本拼在一起
  （实测 `"The He'llo worldwrold sample…"`），而 `wordcraft-cli text` 给的是「接受后」视图。
  → 读回校验**必须看 runs 的 `ins/del`**，不能比对整段 `text`。
- `select.text {text, occurrence?}` 是按字面文本定位（`occurrence` 消歧）——
  这是 docx 侧最省事的锚点；需要精确偏移时用 `select.range {anchor:Pos, focus:Pos}`。

## 2. 核心抽象：文本单元 + 区域锚点（新 crate `media-core`，决策 ④）

检测内核不知道「图片」是什么。它只看到一段 stitched text。所有载体差异收敛到四个类型：

```rust
// media-core/src/model.rs
/// 文本在载体里的落点（各载体用自己的变体，回写时由适配器翻译）
pub enum Region {
    /// 位图/图片：像素坐标（原点左上）
    ImageBox   { x: f64, y: f64, w: f64, h: f64 },
    /// 图片里的文字层（可无损编辑的文本对象）
    LayerRef   { layer_id: u32, kind: LayerKind },   // Type | Shape | Pixel
    /// 分页文档：页 + 页内归一化坐标（0..1，避免 DPI 假设）
    PageBox    { page: u32, x: f64, y: f64, w: f64, h: f64 },
    /// 文档流内的可编辑对象引用（docx run 区间 / PDF 文本命中）
    RunRef     { part: String, run_range: [usize; 2] },
    /// 无法定位（抽取置信度过低）→ 只能进「列表视图」，不画到载体上
    Unlocated,
}

/// 一个可独立校对的文本单元（一行/一段/一个文字层/一个 OCR 块）
pub struct TextUnit {
    pub id: u32,
    pub text: String,
    /// 来源：structure（文字层/文本层）| vision（LLM 视觉）| ocr
    pub source: ExtractSource,
    pub region: Region,
    /// 抽取置信度（structure 恒 1.0；vision/ocr 由来源给出）
    pub confidence: f64,
    /// 是否可无损回写（图片文字层 / docx run / PDF 文本层 = true；扁平位图 = false）
    pub editable: bool,
    /// 单元内可改写目标：给定 (local_span, correction) 能否落到原对象
    pub page: Option<u32>,
}

/// 拼接产物：检测输入 + 反查表（内核只见 text，我们保住 span↔unit 的双向映射）
pub struct Stitched {
    pub text: String,
    /// unit_id → stitched 中的字节区间 [start, end)
    pub unit_ranges: Vec<(u32, [usize; 2])>,
    /// 单元间分隔符（必须是单字节可数，保证 offset 可逆）
    pub separator: &'static str,   // 默认 "\n"
}

/// 校对结果 + 落点：检测到的 Error 回映射后的产物（回写与 UI 的共同输入）
pub struct Annotation {
    pub id: u32,
    pub error: shared_model::Error,   // 原样保留：归因/讲解/反馈全都接得上
    pub unit_id: u32,
    /// 单元内字节区间（= error.span 减去 unit 起始偏移）
    pub local_span: [usize; 2],
    pub region: Region,
    /// 建议的回写方式（由能力探测决定，UI 可改）
    pub apply: ApplyMode,
    /// 标记样式（红框/下划/高亮 + 编号气泡），保证同类错误视觉一致
    pub style: MarkStyle,
}

pub enum ApplyMode {
    Overlay,     // 默认（决策 ①）：叠加标注——图片 mark-* 层 / PDF squiggly-highlight 批注 / Word 批注
    Comment,     // 批注（PDF comment_add / docx comment），正文零改动
    InPlace,     // 显式开启才用：type.edit（图片）/ 修订（docx）/ text_edit（PDF，CJK 不可用）
    ListOnly,    // 只出清单，不回写载体
}
```

**为什么用 stitched text + offset map 而不是逐单元调用检测**：逐单元调用会丢跨行上下文
（主谓一致、指代、重复），且 N 次 NLP 往返。拼接一次调用，保留全文上下文；
offset map 保证 `Error.span` 语义不变，`/gec/detect` 的缓存键、历史复用、归因、
反馈日志全部照旧可用。分隔符用 `\n`（单字节），映射可逆且无歧义。

## 3. 取文本三档（图片侧最关键的设计决策）

| 档 | 条件 | 文本来源 | 坐标来源 | 能否无损回写 | 成本 |
|---|---|---|---|---|---|
| **A 结构层**（默认优先） | 载体含文字对象：`.psd/.psb` 分层 `.tif/.pcraft/.xcf/.kra`、docx、有文本层 PDF | PhotoCraft `info`/`doc.inspect`（文字层 `text.text`）/ WordCraft `inspect`（blocks+runs）/ PdfCraft `text_extract` `text_lines` `text_paragraphs` | 同一命令返回的 `bounds`(px) / `runs[].start..end` / `text_find` 的 `rects`(points) | ✅ | 一次进程调用，零误差 |
| **B 视觉**（本期） | 扁平位图（PNG/JPEG/HEIC）或 A 覆盖不到的区域 | 多模态 LLM（`doc.render` 出 PNG → `image_url` parts）→ `blocks[{text, bbox}]`；**模型复用 `llm.model`（决策 ②）** | 模型返回 bbox（归一化 0..1，再乘回像素） | ❌ → Overlay | LLM 调用 |
| **C OCR**（后续） | 批量/离线/隐私要求 | **PdfCraft 内置 `ocr_recognize`（`ocr_status` 探测）**；或外部引擎（tesseract / PaddleOCR / 已装的 MinerU） | `ocr_recognize` 后可用 `text_find`/`text_lines` 取 rects；外部引擎给块框 | 半（OCR 后 PDF 有文本层，可 `text_find` 定位） | 本地模型 |

> **OCR 语言边界（实测）**：`pdfcraft-cli run ocr_status` 在 CLI 独立安装时 `available:false`
> ——模型只在 App bundle 里。已用软链接启用：
> `ln -sfn /Applications/PdfCraft.app/Contents/Resources/models ~/.local/share/pdfcraft/models`
> → `available:true`，但 **`languages` 只有 `en`**（bundle 内 `text-detection.rten` /
> `text-recognition.rten` 为英文模型）。**中文 OCR 必须走外部引擎**，不能指望 PdfCraft 内置；
> 设置里的 `ocr_endpoint` 就是为这条留的。

**融合策略**（A 与 B/C 同时可用时）：以 A 为准（文本与坐标都精确），
用 bbox 相交（IoU > 0.5）把 B/C 的块挂到 A 的单元上做「视觉补全」，
A 未覆盖且 B 置信度 ≥ 阈值（默认 0.6）的区域补为新 `TextUnit(source=vision)`。
**绝不用 B/C 的文本覆盖 A 的文本**——文字层文本是原文真值，视觉文本是猜测。

视觉档的提示词产物契约（放入 `prompts/`，与既有 `*_prompt.json` 同版本化机制）：

```json
{"blocks":[{"text":"He'llo world","bbox":[0.12,0.34,0.31,0.06],"conf":0.88}]}
```

校验规则（不满足即降级为 ListOnly 并告警，不硬猜）：bbox 归一化且在 0..1 内、
块数 ≤ 上限、块文本与 `doc.inspect` 已有文字层不重复、返回文本必须能覆盖
被标记 span（否则 span 无法回映射）。

## 4. 回写三模式（决定「校对」还是「贴条」）

| 模式（**默认 Overlay**，决策 ①） | 载体条件 | 图片（PhotoCraft） | PDF（PdfCraft） | Word（WordCraft） |
|---|---|---|---|---|
| **Overlay**（默认） | 任意位图 / 视觉档 | `layer.new.group` + `shape.create {kind:"rect",stroke:{width:3,color:"#e03131"},name:"mark-1-box"}` + `type.create {box,text:"建议：…",font:"Heiti SC"}` | `comment_add {type:"squiggly"/"highlight"/"underline", find, all:true, color:"red", contents}` | `review.newComment`（选中处） |
| **Comment** | PDF / docx | —（无批注原语） | 同上 + `type:"replace"`（实测落成 StrikeOut + Caret 回复两条） | `review.newComment` / `review.reply` |
| **InPlace**（需显式开启） | 含文字对象 | `type.edit {"layer":4,"replace":{"start":2,"end":7,"text":"Hello"}}`（实测样式保留） | `text_edit`（字体仅 3 种 → CJK 不可用） | `review.trackChanges` + `select.text` + `text.insert`（实测 ins/del 修订）或 `edit.replaceAll` |
| **ListOnly**（兜底） | 工具缺失 / 载体不可写 | — | — | — |

统一约定：

1. 所有 LangFixer 标注收进一个组（`layer.new.group {"name":"LangFixer Marks"}` 建空组，
   再用 `layer.moveTo {"layers":[…],"target":<组>,"position":"into"}` 把 `mark-*` 层移入——
   实测前者不会自动收纳后续图层），命名 `mark-<id>-box` / `mark-<id>-text`，
   `layer.setProps` 一键显隐；重跑幂等：先按名字前缀删除旧标注层，再写新的。
2. **InPlace 默认不自动执行**，除非该错误 `confidence ≥ 0.85` 且用户在设置里开了
   「高置信自动改字」；其余走 UI 勾选（与既有「优化轮次：自动评估 + 人工确认」一致的门）。
3. 双产物：`.psd`（分层可编辑）+ `.png`（预览）。PDF 用 PdfCraft 增量保存（原字节保留）；
   docx 用 WordCraft 修订模式（可在 Word 里逐条接受/拒绝）。
4. 标注样式按 `error_type` 固定色板（拼写红 / 语法蓝 / 标点黄 / 风格灰），
   与 web-ui `ErrorList` 的分类色一致，避免「图上颜色与列表对不上」。

## 5. 适配器 trait：一个内核，三个协作工具

```rust
// media-core/src/collaborator.rs
#[async_trait]
pub trait Collaborator: Send + Sync {
    /// 载体类型：image | docx | pdf
    fn kind(&self) -> &'static str;
    /// 只读探测：二进制是否存在、版本、能力集（是否支持文字层读取/批注/修订）
    async fn probe(&self) -> Probe;
    /// 打开载体（copy 进 job 工作目录后打开），返回能力画像
    async fn inspect(&self, job: &JobCtx) -> Result<CarrierInfo, MediaError>;
    /// 取文本单元（A 档；B/C 档由 extractor 层负责，不在此 trait）
    async fn extract_units(&self, job: &JobCtx) -> Result<Vec<TextUnit>, MediaError>;
    /// 出预览（给视觉模型 / 给 UI）
    async fn render_preview(&self, job: &JobCtx) -> Result<PathBuf, MediaError>;
    /// 把 Annotation[] 翻译为该工具的原生命令并执行，产出工件
    async fn apply(&self, job: &JobCtx, marks: &[Annotation], mode: ApplyMode)
        -> Result<Artifacts, MediaError>;
}
```

trait 之下三种 transport（同一 trait，不同实现；**flag 名各自适配**）：

| transport | 命令形态 | 用在哪 | 代价 |
|---|---|---|---|
| `run`（默认） | `photocraft-cli run … --cmd … --out …` | 单文件、一次性、隔离性最好 | 每次进程冷启动（秒级） |
| `serve` | `photocraft-cli serve --port … --automation-*-root …`，JSON lines | 批量、同一文件多轮（inspect→detect→apply） | 需管进程生命周期 + 端口 + token |
| `bridge` | `photocraft-cli mcp --bridge 127.0.0.1:<port>` 或直连 TCP（先 `auth` 帧） | **在环模式**：用户正开着 PhotoCraft/WordCraft，标注实时出现在他眼前 | 依赖用户 App 在跑；60s 超时语义要按 `not run` / `no reply` 分流处理 |

选择规则（自动）：`settings.media.transport = auto` 时——
探测到用户已启动该 App（loopback 端口可连）→ `bridge`；
同一 job 内命令数 > 3 → `serve`；否则 `run`。

## 6. API 面（`api-server/src/routes/media.rs`）+ 桌面端同构

沿用既有约定：文件走 **raw `Bytes` body + `?filename=` query**（与 `parse-preview` 一致），
错误走 `ApiError`，耗时动作走 **job + 轮询**（与 `eval` / `train` 一致），
破坏性动作过 **Confirm 门**（与 `agent/confirm`、`round/:id/confirm` 一致）。

```
POST   /gec/media/inspect?filename=x.png        raw body → 能力探测（不落状态）
                                             {kind, width, height, has_text_layers, text_units,
                                              pages, editable, collaborators:[{app,version,ready}],
                                              extract_plan:[structure|vision|ocr], warnings}
POST   /gec/media/jobs                          raw body + ?filename=&lang=&level=&force_llm=
                                             → {job_id, status}（后台：copy→inspect→extract→detect→remap）
GET    /gec/media/jobs/:id                      → {status, stage, stages[], warnings,
                                              units:[{id,text,region,source,confidence,editable}],
                                              errors:Vec<Error>, annotations:Vec<Annotation>,
                                              layers:Vec<LayerReport>}   ← 检测路径复用
GET    /gec/media/jobs/:id/preview              → preview.png（渲染后的扁平预览）
GET    /gec/media/jobs/:id/unit/:unit_id        → 单单元上下文（列表↔图像联动用）
POST   /gec/media/jobs/:id/apply                {annotation_ids:[…], mode:in_place|overlay|comment,
                                              auto_high_confidence:bool}   ← Confirm 门
                                             → {artifacts:{editable:"result.psd",
                                                preview:"result.png", sidecar:"annotations.json"}}
GET    /gec/media/jobs/:id/artifact?name=…      下载工件（sidecar 为 JSON，其余二进制）
GET    /gec/media/jobs                          历史列表（复用 detect_history 的会话/归档语义）
DELETE /gec/media/jobs/:id                      清理工作目录
POST   /gec/media/report                        {job_ids?, accepted_ids?, rejected_ids?, overrides?, docx?, title?}
                                             → 勾选后的正误表（6 列与专家表同构，见 §18）
                                             → {report_id, candidates, exported, dropped_ids,
                                                rows:[ReportRow], paths:{json,markdown,html,docx}, warnings}
GET    /gec/media/reports/:id?format=json|md|html|docx
                                             → 取导出产物（id 限 [A-Za-z0-9_-]，防目录穿越）
```

桌面端（`desktop-app/src/media.rs`）跑的是**同一条链路**：编排已抽到 `media-flow` crate，
api-server 与桌面端都只是宿主——各自注入自己的检测级联（服务端：缓存 / 历史复用 / 规则开关 /
白名单；桌面端：本地引擎 + 本地决策表 + 离线模式），其余（副本隔离、抽取档位、拼接回映射、
sidecar、回写、正误表）只有一份实现。同构命令：

```
media_probes                      ← POST /gec/media/probes
media_inspect                     ← POST /gec/media/inspect
media_create_job                  ← POST /gec/media/jobs
media_jobs / media_job            ← GET  /gec/media/jobs[/:id]
media_preview                     ← GET  /gec/media/jobs/:id/preview
media_apply                       ← POST /gec/media/jobs/:id/apply
media_report                      ← POST /gec/media/report
media_report_file                 ← GET  /gec/media/reports/:id?format=
```

字节传输差异由前端抹平（`invoke` 只能走 JSON）：上传传 `data_b64`，预览回传
`{content_type, data_b64}` 再还原成 Blob；导出产物走原生「另存为」后由命令直接写盘
（与资源包导出同一口径）。`media_open_in_app`（把结果丢回用户编辑器）仍未做。

job 状态机：

```
queued → copying → inspecting → extracting → detecting → remapping → ready
                                                    ↘ failed(stage, reason)
ready → applying → applied(artifacts) | apply_partial(applied[], skipped[])
```

`detecting` 阶段直接调 `routes::detect::run_detection`（同一函数），
所以 level 1/2/3、force_llm、规则开关、自定义词白名单、缓存、`layers` 报告全部一致。

## 7. web-ui 交互（新面板 `MediaWorkspace.vue`）

```
┌ 导入：拖入 image / docx / pdf ─────────────────────────────────────┐
│ 探测结果：PNG 1200×800 · 无文字层 → 走视觉档 · PhotoCraft 0.16.0 ✓ │  ← 探测失败（未装 CLI）就地提示去设置
├───────────────────────────────┬───────────────────────────────────┤
│ 预览画布（preview.png + SVG   │  错误列表（复用 ErrorList）        │
│ overlay 画框，标号 ①②③）      │  分类过滤 / 置信度 / 检测路径徽章   │
│ 滚轮缩放 · 点框↔点行联动       │  每条：采纳(改字/标注) · 放弃 · 讲解 │
├───────────────────────────────┴───────────────────────────────────┤
│ 回写模式：(•) 叠加标注  ( ) 改原文（仅文字层可选）  ( ) 只出清单     │
│ [应用并生成结果]  → result.psd / result.png / annotations.json      │
│ [在 PhotoCraft 中打开]（桌面端）                                    │
└──────────────────────────────────────────────────────────────────┘
```

关键交互约束：

- **点图上的框 = 选中列表里那条**（反之高亮框）——靠 `Annotation.id` ↔ SVG `data-id` 绑定，
  这是多模态审校可用性的命门，不能只给列表。
- 视觉档的框必须显示「来源 = 视觉（conf 0.88）」徽章；`Unlocated` 的错误单独归入
  「未能定位」区，绝不画假框。
- 「改原文」按钮只在 `unit.editable == true` 时出现（扁平位图上不出现，避免误导）。

**实现状态（2026-10-28）**：`web-ui/src/components/MediaWorkspace.vue` 已按上图落地，几条实测口径：

- 顶层导航「载体审校」，深链 `?view=media`（刷新 / 分享链接停在同一视图，也便于无头浏览器冒烟）；
- 探测卡片直连 `POST /gec/media/probes`：PhotoCraft 版本、视觉档模型与**上传前压缩口径**
  （`≤2000px / ≤1.5MB JPEG q80`）。`probes` 还上报 `enabled`——总开关关掉时面板直接说明
  怎么补（含「设置文件存在时 `GEC_MEDIA_ENABLED` 会被整体覆盖」这个坑），而不是等用户点
  「开始审校」吃一句 503；
- 预览取 `GET …/jobs/:id/preview` 的字节自建 objectURL：无 `result.png` 时后端回退原件并给
  `application/octet-stream`，直接塞 `<img>` 会被浏览器按类型拒掉；
- SVG `viewBox` = 载体像素（`0 0 3072 4096`）→ 画框坐标与 `annotations.json` 逐字一致
  （实测 `x=522.24 y=2519.04 w=2012.16 h=69.63` 两边相同），缩放交给 viewBox，不自己换算；
- 点框 ↔ 点行联动靠 `Annotation.id`；画不出来的（`unlocated` / 无 bounds / 载体尺寸未知 /
  零面积 / NaN）进列表并标原因，**不画假框**；
- 告警原样列出：视觉上传前压缩、网关重试、跨单元丢弃计数、LLM 层降级、视觉档彻底失败
  （`extraction = none` 必须看得见，否则空结果会被读成「这张图没错」）；
- 载体**串行**处理：视觉档共用一个 LLM 网关，并发只会互相排队并触发 503；
- job 列表按创建时间**升序**显示，与正误表里的「图片1 / 图片2」同一编号——后端
  `GET /gec/media/jobs` 是倒序，UI 必须自己排，否则对着表找不到图。

## 8. 设置（`AppSettings.media`，沿用「设置文件 > 环境变量 > 默认」）

```jsonc
"media": {
  "enabled": true,
  "workdir": "media-work",                 // 每个 job 一个子目录，同时作为 automation root
  "max_bytes": 67108864,                   // 与既有上传上限一致（64MB）
  "timeout_secs": 120,                     // 单次协作工具调用超时
  "transport": "auto",                     // auto | run | serve | bridge
  "extract": { "prefer": "structure",      // structure | vision | ocr
               "vision_model": "",         // 空 = 复用 llm.model（须支持 image parts）
               "min_confidence": 0.6,
               "ocr_endpoint": "" },
  "apply_mode": "overlay",                   // 默认（决策 ①）：overlay | comment | in_place | list_only
  "auto_inplace_min_confidence": 0.85,       // 高置信自动改字（默认高于此值 + 显式开关才改）
  "cjk_font": "Heiti SC",                    // 标注文字字体（实测默认 Inter 下 CJK 依赖回退）
  "collaborators": [
    { "kind": "image", "app": "photocraft", "bin": "~/.local/bin/photocraft-cli",
      "enabled": true, "bridge_port": null, "token_file": null },
    { "kind": "pdf",   "app": "pdfcraft",   "bin": "~/.local/bin/pdfcraft-cli",
      "enabled": true, "ocr_models_dir": "~/.local/share/pdfcraft/models" },
    { "kind": "docx",  "app": "wordcraft",  "bin": "~/.local/bin/wordcraft-cli",
      "enabled": true, "author": "LangFixer" }
  ]
}
```

环境变量等价：`GEC_MEDIA_ENABLED` / `GEC_MEDIA_WORKDIR` / `GEC_MEDIA_APPLY_MODE` /
`PHOTOCRAFT_CLI` / `WORDCRAFT_CLI` / `PDFCRAFT_CLI` / `GEC_MEDIA_TRANSPORT` / `GEC_MEDIA_VISION_MODEL`。

⚠ **优先级坑（实测踩过）**：设置文件存在且可解析时 `AppSettings::load` **整体**采用文件内容，
`GEC_MEDIA_*` 环境变量被忽略（不是逐项合并）。要开多模态就在设置文件里加
`"media": { "enabled": true, "use_vision": true }` 再重启。`POST /gec/media/probes` 会上报
`enabled` 与压缩口径（`max_edge` / `max_bytes` / `jpeg_quality`），web-ui 的载体审校面板据此
把「为什么不能开工」直接写出来。

设置界面新增「外部协作工具」卡片，复用既有 **只读探测** 语义（`POST /gec/settings/test`）：
每个工具跑 `<bin> --version`（不启动 GUI、不写文件）→ 报告 `ready / 版本 / 可执行路径`；
PhotoCraft 额外探测 `type.fonts` 里是否存在配置字体（缺字体 → 告警「中文标注可能显示为方框」）；
PdfCraft 额外探测 `run ocr_status` → 报告 `available` 与 **可用语言**（实测仅 `en`，
中文须外部引擎，卡片必须如实显示，不能只显示「OCR 就绪」）；
WordCraft 探测 `commands --json` 是否含 `review.newComment` / `review.trackChanges` / `select.text`
（缺任一 → 该载体标记 `degraded`，回写降级 ListOnly）。

## 9. 安全与资源边界

| 边界 | 做法 |
|---|---|
| 文件可见范围 | 每个 job 独立目录，**同时**作为 `--automation-read-root` 与 `--automation-write-root`（实测：不给 root 时绝对路径一律被拒，这是天然沙箱） |
| 覆盖原件 | 协作工具本身拒绝 `--out == <FILE>`（需 `--in-place`）。我们**永不** `--in-place`：原件只读，产物写到 job 目录 |
| 控制通道鉴权 | bridge 必须 token 文件（0600），只连 `127.0.0.1`；token 不进命令行（避免 `ps` 泄露），走 `--control-token-file` |
| 进程与超时 | 每个 transport 管自己的子进程（`kill_on_drop`）；60s 语义分流：`timeout: not run` 可重发，`no reply in 60s` 先 `doc.inspect` 再决定 |
| 体积/时长 | `max_bytes` 拒大文件；图片送视觉端点前在进程内降采样 + 重编码（默认长边 2000px / 1.5MB / JPEG q80，`vision_max_edge` 等三个开关，见 §14），**回写仍在原分辨率**（视觉 bbox 是归一化坐标，缩放不影响定位） |
| IP 边界 | 沿用知识库既有约定：视觉档把图像内容送到用户自带 LLM 端点，属用户数据，不进镜像、不进日志（日志只记 job/尺寸/单元数/错误数） |
| 依赖缺失 | 协作工具不可用 → 不是 500：`inspect` 返回 `collaborators[].ready=false` + 明确告警，job 降级为 ListOnly（出清单 + annotations.json，不回写载体） |

## 10. 与既有闭环的接线（不做「孤岛多模态」）

多模态只换输入/输出端，中间的教学闭环原样复用：

- **归因**：`Annotation.error` → 既有 `POST /gec/attribute` 语义（决策表 + 知识库富化 + CEFR + 韦纳三维）。
  图片里犯的冠词错误和文本里犯的冠词错误，归因与补救必须一致。
- **讲解/练习**：`explain`、补救练习按 `error` 走，与载体无关。
- **反馈飞轮**：UI 上「采纳 / 放弃」写 `feedback_log`（带 `rule_id`）——
  视觉档带来的误报会同样进入反思中心建议队列（这是有意的：图片 OCR/视觉噪声应表现为
  规则误报统计上升，而不是被藏在多模态分支里）。
- **会话与历史**：job 挂到 `session`（`/gec/sessions/:id`），历史列表出现载体类型徽章；
  `detect_history` 缓存键仍是「文本 + 参数」，所以同一图片重复上传（同一 stitched text）命中缓存。
- **agent 工具面**（`gec_core::ManagedTool` + `ActionTier`）新增：
  `list_media_jobs`（Autonomous）、`stage_media_annotations`（Autonomous，只进草稿）、
  `apply_media_annotations`（**Confirm 级**，改用户文件必须过确认门）。

## 11. 分阶段落地

| 期 | 范围 | 验收（可跑的命令级标准） |
|---|---|---|
| **P0 图片·结构层档** | `media-core` crate + PhotoCraft adapter（`run` transport）+ `doc.inspect` 文字层抽取 + offset map + 检测复用 + `type.edit` 无损回写 + Overlay 标注 + 双产物 + 设置探测 | 契约基线已跑通：`./scripts/media_smoke.sh`（PASS）。给一个含文字层的 `.psd`：`POST /gec/media/jobs` → `GET …/jobs/:id` 出 errors + annotations（region 带 layer_id）→ `POST …/apply` 出 `result.psd`（文字层被改，字体/字号保留）+ `result.png` + `annotations.json`；重跑幂等（旧 `mark-*` 层被替换） |
| **P1 图片·视觉档**（✅ 已实现，见 §17） | `langfixer-llm` 客户端支持多模态 parts（`content: [{type:"text"},{type:"image_url"}]`）+ `prompts/media_vision_prompt.json` + bbox 校验 + Overlay 回写（`shape.create`+`type.create`，CJK 字体配置） | ✅ 多模态 parts（纯文本序列化逐字节不变）、bbox 尺度判定、`conf < 0.6` 不画框只进「未定位」均已落地并有 mock 端到端测试；✅ 已做：扁平预览上的 Overlay 绘制与 `MediaWorkspace.vue` 联动——画框坐标与 sidecar 逐字一致、点框↔点行联动、未定位不画框（见 §7 实现状态）；✅ 重演 → 勾选 → 输出正误表（§18，含 Word 导出） |
| **P2 PDF + Word** | `pdfcraft-cli`（`text_find` + `comment_add`）与 `wordcraft-cli`（修订 + 批注）adapter；同一 `Annotation` 模型，不同 writer；分页 `Region::PageBox` / `RunRef` | **契约基线已 PASS**：`./scripts/media_smoke_p2.sh`。PDF：`text_find` 给 `matches[].rects`、`comment_add` 落对页对短语、`doc_save` 增量保存原字节不动；docx：`info` 读回 `trackChanges:true` + `comments≥1`，`inspect` 的 runs 带 `ins/del`，Word/WPS 里能逐条接受/拒绝 |
| **P3 在环 + OCR** | `bridge` transport（用户开着 App，标注实时出现）+ `media_open_in_app`；OCR 档（本地引擎）与 A/B 融合 | 用户在 PhotoCraft 里看到 LangFixer 写入的 `LangFixer Marks` 组并可直接编辑；OCR 档与视觉档在同一张图上产出可 A/B（测评中心加 `extract` 维度消融） |

**先做 P0 的理由**：它不依赖 LLM 视觉、不依赖未安装的 CLI、坐标零误差，
且已经能端到端证明「载体 → 检测 → 回写」这条骨架正确；P1 只是往骨架里换一个 extractor。

## 12. 风险与降级矩阵

| 风险 | 征兆 | 降级动作 |
|---|---|---|
| PhotoCraft 是 early alpha，命令参数可能随版本变 | `run` 报 unknown command / params 不符 | 启动时 `commands --json` 快照 → 缺关键命令（`type.create/shape.create/type.edit`）则该工具标记 `degraded`，回写降级 ListOnly；把 `params` 模式串写进 job 报告便于定位版本漂移 |
| PNG 输出 flatten（实测 warning） | 用户拿到的结果无法再编辑 | 强制双产物：`.psd` 分层 + `.png` 预览；UI 下载区把 `.psd` 标为「可继续编辑」 |
| CJK 标注字体回退（实测默认 Inter） | 标注显示方框 | 配置 `cjk_font`，探测 `type.fonts` 校验；缺字体则告警并回退系统 CJK family 链 |
| 视觉 bbox 漂移 | 框框不住错误文本 | 校验 + `conf` 阈值 + 只 Overlay 不改字；漂移严重时该错误进「未定位」区 |
| 文字层文本 ≠ 视觉文本（同一区域） | 双源冲突 | 以文字层为真值（§3 融合策略），冲突记入 warnings |
| 协作工具未安装/版本过旧 | `--version` 失败 | `inspect` 明确报「未就绪 + 安装命令」，job 仍可跑（ListOnly + sidecar） |
| 大图像素超限导致 LLM 成本/超时 | 视觉档超时 | 先降采样做分析，回写按比例还原 bbox（§9） |
| PdfCraft 内置 OCR 只有英文模型 | 已确认：`ocr_status` → `available:false`（CLI 独立安装）/ `languages=["en"]` | 中文 OCR 走外部引擎（`extract.ocr_endpoint`：tesseract chi_sim / PaddleOCR / MinerU）；PdfCraft OCR 只用于英文扫描件；模型接通 = 软链 `~/.local/share/pdfcraft/models` → App bundle 的 `Resources/models`；设置卡片显示可用语言，不假装支持中文 |
| PDF `text_edit`（改正文）字体受限 | 已确认：仅 `helvetica/times/courier` | PDF 侧 **InPlace 默认关闭**，Overlay/Comment 为默认（决策 ①）；含 CJK 的 PDF 绝不尝试 `text_edit` |
| `comment_add type:"replace"` 落成两条批注 | 已确认：`StrikeOut` 批注 + 带建议文本的 `Caret` 回复 | 幂等清理与「撤销标注」必须成对处理（按 `comment_list` 读回的 id 删），不要假设一条 |
| Word 跟踪替换后段落 `text` 含被删文本 | 已确认：实测 `"The He'llo worldwrold sample…"` | 读回校验看 `runs[].props.ins/del`；`wordcraft-cli text` 是「接受后」视图，两者语义不同不可混用 |

## 13. 本机环境状态（2026-10-26）

三个协作工具的 CLI 均已安装并实测：

- ✅ `~/.local/bin/photocraft-cli` = `0.16.0 (b1794c6e0, 2026-10-25)`
- ✅ `~/.local/bin/pdfcraft-cli` = `0.14.0`（179 工具；`--version` / `tools` / `run` / `ocr_status` 已验）
- ✅ `~/.local/bin/wordcraft-cli` = `0.11.0`（643 命令；`convert` / `run` / `info` / `inspect` 已验）
- ✅ OCR 模型已接通：`~/.local/share/pdfcraft/models` → `/Applications/PdfCraft.app/Contents/Resources/models`
  （**仅英文**，见 §3 语言边界）
- ✅ `/Applications/{PhotoCraft,PdfCraft,WordCraft}.app`（GUI，bridge 用）
- ✅ 契约冒烟：`./scripts/media_smoke.sh`（P0 图片，PASS）· `./scripts/media_smoke_p2.sh`（P2 PDF+Word，PASS）
  ——两条回写路径（图片无损改字/叠加标注、PDF 批注、Word 修订+批注）的命令契约已锁死
- ⚠️ **GUI 启动的进程不继承交互 shell 的 `PATH`**：Tauri 打包的 `.app`（经 launchd/Finder 启动）、
  Docker 容器内的 `api-server` 都解析不到 `~/.local/bin`。实现里按「配置里的绝对路径优先 →
  `$PATH` 兜底 → 常见安装位置候选（`~/.local/bin`、`/usr/local/bin`、`/opt/homebrew/bin`）」三级解析，
  设置界面显示解析后的真实路径；桌面端 `.app` 默认走绝对路径

## 14. 决策定案（2026-10-26 拍板）

| # | 决策 | 落地含义 |
|---|---|---|
| ① | **默认回写模式 = Overlay** | 任何载体默认**不改原文**：图片写 `mark-*` 标注层、PDF 写 `squiggly`/`highlight` 批注、Word 写批注。`InPlace`（图片 `type.edit` / Word 修订 / PDF `text_edit`）只在用户显式选择、或 `confidence ≥ auto_inplace_min_confidence` 且开关打开时启用。UI 上 Overlay 是默认选中项 |
| ② | **视觉档复用 `llm.model`** | 不新增模型配置项（保留 `extract.vision_model` 作为可选覆盖）。前置：`langfixer-llm` 的 `ChatMessage.content` 从 `String` 扩展为「string 或 parts[]」（OpenAI 兼容），保持旧请求体不变（`String` 分支序列化结果与现在逐字节一致）；端点不支持图像 parts 时（400）→ 明确报错并提示改用 OCR 档，不静默降级 |
| ③ | **P0 只做后端** | P0 交付 = `media-core` crate + PhotoCraft adapter + `/gec/media/*` + 契约冒烟；`MediaWorkspace.vue` 与 P1 视觉档同期做（避免定位/联动逻辑返工） |
| ④ | **`media-core` 为新 crate** | workspace 新增 `media-core`（模型 + trait + adapters）；`document-parse` 保持「只读抽正文」不动，P2 的 Word/PDF adapter **复用**它的 docx/pdf 解析，不重写 |

下一步（P0 开工顺序）：`media-core` 模型与 offset map（纯函数 + 单测）→
PhotoCraft adapter（`run` transport，含 `--version`/`commands` 探测与字体校验）→
`routes/media.rs` job 流水线（复用 `run_detection`）→ 桌面端命令同构 →
把 `scripts/media_smoke.sh` 的断言搬成 Rust 集成测试（`api-server/tests/media_api.rs`）。



---

## 15. 验收金集：专家正误表（2026-10-26 入库）

来源：`~/Documents/校对测试数据1/`——两张原图（`origin_pic_1.jpg` / `origin_pic_2.jpg`，
3072×4096 实拍住院告知）+ `住院告知文字质检正误表.docx`（专家判定，6 列表格）。

已抽取为 `datasets/media_gold.json`（重跑：`scripts/media_gold_extract.sh <docx>`）：

| 项 | 值 |
|---|---|
| 条目 | 15 |
| 计差错（`计错数` ≠ 不计） | 7 |
| 不计（表达优化 4 / 表述不清 2 / 未标注 2） | 8 |
| 载体 | 2 张扁平照片（`text_layer: false`） |

**关键事实（决定这份金集能验收什么）**：两张原图是**扁平位图，没有文字层**——
A 档（结构层）取不到任何文本。因此这份金集只能验收 **B 档（视觉抽取）/ C 档（OCR）**
的端到端召回；A 档的回归靠造的含文字层 PSD（见 `media_api.rs` e2e）。

打分口径（`media_core::gold`）：
- 归一化（去空白 + 全角标点转半角）后，**包含关系直接算命中**——专家常引整句，
  我们命中其中片段（实测：金集第 9 项整句 vs 我们命中「下午15点」→ `Contained`）；
  但**较短一方不足 3 字符时不按包含计分**（`MIN_CONTAINMENT_CHARS`）：
  半角标点错误的 `original` 只有一个 `,`，与任何含逗号的整句互为包含，
  那是巧合不是命中——实测评测里它曾把金集第 2 项白送成 100% 命中；
- 否则用字符 bigram 重叠度 ≥ 阈值（默认 0.5）；
- 贪心一对一匹配：一条 gold 最多配一条我们的结果（防重复计分）；
- 加权召回：计差错权重 1.0，不计条目 0.5（真实缺陷但不计硬差错）→ 总分母 11。

回归测试：`media-core/tests/gold_dataset.rs`（5 项：数据契约 / 满分召回 /
部分召回的加权 / 片段命中 / 无关输出零分）。

评测 harness（`api-server/tests/media_gold_eval.rs`）分两层，**衡量的是不同东西**：

```bash
# ① 上限评测：把 gold 原文当「完美抽取」喂进检测级联 → 只衡量 检测 + remap
cargo test -p api-server --test media_gold_eval -- --nocapture
# ② 真实评测：原图 → 视觉抽取 → 检测（level 3）→ remap → 打分（需凭据与原图）
#    注意：harness 把 GEC_SETTINGS_FILE 指到临时空文件 → 设置文件**不生效**，LLM 只能来自环境变量；
#    模型名必须是该网关 `/v1/models` 里存在的名字（名字错 → 每载体 HTTP 400，LLM 层整层白跑）；
#    `LLM_TIMEOUT_SECS` 必须放宽（不设 = 默认 30s，思考模型整载体检测实测 60s+ → LLM 层必然超时失败）
LLM_BASE_URL=http://<网关>/v1 LLM_API_KEY=<key> LLM_MODEL=<网关上的模型名> \
LLM_TIMEOUT_SECS=300 LANGFIXER_MEDIA_DIR=<原图目录> \
cargo test -p api-server --test media_gold_eval -- --ignored --nocapture
# ③ 跑完看产物（只读）：每载体抽取/告警 → 候选行（规则·定位·命中口径）→ 漏报归因（含强弱）
scripts/media_replay_inspect.py            # 默认 target/media-replay，可传某一轮的目录
```

**①的实测结论（重要，决定后面往哪投人力）**：离线仅本地规则时，金集 15 条
**召回 0%**，而同一批文本里的正对照（半角逗号）稳定检出——
即管线没坏，是 **zh 规则层覆盖不足**。两个具体原因：
- 出厂词表里 `ZH_CONFUSABLE` / `ZH_MISSING_SUBJECT` 当时是 `enabled = false`
  （抑制层 `apply_suppression_filters` 会把它们检出的错误全部丢掉；**现已开启**，见 §19 实测），
  所以「报道→报到」这类同音误用**连规则层都不参与**；
- 词典里也没有这些条目（`lexicons/zh_gec_lexicon.json` 的 `confusables` 是 220 条繁简混用为主）。

所以多模态载体的召回瓶颈**不在抽取，在检测层的中文覆盖**：
金集错误集中在搭配 / 语义 / 标点规范，必须靠 LLM 层（level 2/3）+ 词典扩充。

原图**不入库**（院方告示实拍，含机构信息），金集 JSON 只存判定文本与图片规格。

## 16. P0 落地状态（2026-10-26 实现完成）

**新增 `media-core` crate**（workspace 成员）：
`model.rs`（`TextUnit` / `Region` / `Stitched` / `Annotation` / `ApplyMode` / `MarkStyle` /
`CarrierInfo` / `Artifacts`）、`stitch.rs`（`stitch` / `stitch_units` / `remap` / `plan_apply` /
`style_for`，纯函数）、`gold.rs`（金集解析 + 打分）、`adapter.rs`（`Collaborator` trait +
`MediaError` + `ToolProbe`）、`photo.rs`（PhotoCraft adapter）。单测 15 项全绿。

**api-server 接线**：
- `AppSettings.media`（`MediaSettings`，默认 `enabled: false`，设置文件 > env > 默认）；
- `media_jobs.rs`：job 状态机（`queued→copying→inspecting→extracting→detecting→remapping→ready→applying→applied`，失败 → `failed`）+ `{workdir}/{job_id}/` 目录隔离；
- `routes/media.rs`：`POST /gec/media/probes` / `POST /gec/media/inspect` /
  `POST|GET /gec/media/jobs` / `GET /gec/media/jobs/:id` / `GET …/preview` / `POST …/apply`
  （嵌套 Router 单独放宽 body limit 到 64MB）；
- `run_detection` 提为 `pub(crate)`：多模态 job 走的仍是**同一条级联**
  （缓存 / 历史复用 / 规则开关 / 白名单 / level 1-3 / force_llm / layers 报告）。

**测试**：`api-server/tests/media_api.rs` 4 项——未开启 503、未知 job 404、
非载体文件必须 `failed`（不卡死）、真机 CLI 全链路 e2e（A 档取文本 → 冠词错误检出 →
回映射到正确单元 → **字节级不变量** `stitched[unit_start + local_span] == error.original` →
Overlay 回写 → **重跑幂等**：`mark-*` 图层数不翻倍 → 预览 PNG）。

**实测新增的坑（补 §1）**：
- `info` 的 `layers[].bounds` = `[x, y, w, h]`（像素）；组的子层在 **`children`** 不是 `layers`；
- `info` 列层顺序是**图层面板自下而上**，与创建顺序相反（别对单元顺序做假设）；
- `type.edit` 的 `replace.start/end` 是 **UTF-8 字符下标**（不是字节）——CJK 实测正确，
  且改字后字体/字号保留（无损回写的前提成立）；
- 批量删层：`layer.select {layer:id, mode:"add"}` × N + `layer.delete {}` 可在**同一个 session**
  内完成，避开「删一个层后其余 id 漂移」的坑；
- 导出 PNG 的 flatten 警告走 stderr 且退出码 0 → 不能把 stderr 非空当失败。

**与 §13 设计的有意差异**（P0 收敛范围）：设置结构是扁平的
（`media.photocraft_bin` / `media.cjk_font` / `media.apply_mode` …），
`collaborators[]` / `bridge_port` / `token_file` / `ocr_endpoint` /
`auto_inplace_min_confidence` 属 P2 传输层与自动改字策略，本轮未引入；
`InPlace` 目前**只按显式选择**，不做「高置信自动改字」。

**未做（按 §11 排期）**：PDF / Word adapter（P2）、C 档 OCR、桌面端命令同构、
`MediaWorkspace.vue`。（B 档视觉抽取已在 §17 落地。）

## 17. P1 落地状态（2026-10-26 实现完成）

**B 档（多模态 LLM 视觉抽取）打通**，扁平位图（无文字层）现在能出结果了。

**前置：`langfixer-llm` 支持多模态 `content`**
- `ChatMessage.content` 由 `String` 改为 `MessageContent`（`untagged`：`Text(String)` / `Parts(Vec<ContentPart>)`）；
- **纯文本分支序列化逐字节不变**（单测锁死 `{"role":"user","content":"你好"}`），
  既有讲解 / 检测 / 兜底链路的请求体不受影响；
- 新增 `ChatMessage::user_parts` / `user_with_image`、`LlmClient::complete_messages(messages, temperature)`、
  `LlmClient::with_model()`（复用连接池换视觉模型）；
- 响应侧宽松解析：`choices[0].message.content` 为字符串**或** parts 数组都能取回文本
  （部分视觉端点返回数组形态）。

**`media-core::vision`（纯函数，零网络依赖，离线可测）**
- 提示词契约：转录不是编辑——**错别字必须原样保留**（否则校对无从下手）；
- `parse_vision_blocks`：容忍 ```json 围栏 / 前后解释文字 / `{"blocks":[…]}` 包装 /
  字段别名（`text|content|line|原文`、`bbox|box|bbox_2d|rect`、`confidence|conf|score`）；
  解析失败**按行回退并告警**，不静默吞；
- `bbox_to_region`：各家模型尺度不一（0..1 / 0..100 / Qwen-VL 的 0..1000 / 直接像素），
  按数值量级排优先级逐个试，取第一个整体落在载体内的口径；
  都不成立 → `None`（**绝不画越界或错尺度的假框**）；
- `vision_units`：`source = vision`、`editable = false`（扁平位图无原对象可改）、
  按 `(y1, x1)` 阅读顺序编号；**不去重**（告示里同一句可能真的出现多次，金集第 14 项即如此）；
  低置信度 / 无 bbox → `Region::Unlocated` + 告警（只进列表，不画框）。

**api-server 接线**
- `media_vision.rs`：读图 → `data:` URL（不上传、不多开凭据面）→ 多模态请求（`temperature = 0`，
  转录要稳定）→ 解析 → `TextUnit`；提示词走 `prompts/media_vision_prompt.json`
  （`lang: "media"`, `cefr: "vision"`，可热更新），缺文件回退内置默认；
- **优先级铁律**：A 档取到文本就绝不用视觉文本覆盖；只在 A 档为空时兜底；
- `job.extraction`（`structure` / `vision` / `none`）+ `job.extraction_model` 上报来源档与模型；
- 视觉档失败**不判死整个 job**（降级为告警）；模型原始响应落盘 `vision_raw.txt`；
- 非位图载体（`psd` / `tif` / `heic`）先由 PhotoCraft `render_preview` 导出 png 再送端点
  （多数端点不收 psd）；
- `POST /gec/media/probes` 新增 `vision` / `ocr` 就绪状态（只看配置面，不发网络请求）。

**测试**（`api-server/tests/media_vision_api.rs`，mock 视觉端点，零凭据）：
无文字层 PSD → A 档为空 → 导出 png → mock 断言收到 `image_url` data URL 与
`temperature = 0` → 2 个视觉单元（`image_box` 像素坐标，不越出载体，`editable = false`）→
冠词错误检出并回映射到 `image_box` → **字节级不变量在 B 档同样成立**；
另一测：未开 B 档时 `extraction = none` + 明确告警，不静默返回空。

**与 §14 P1 计划的差异**：提示词文件是 `prompts/media_vision_prompt.json`
（单一媒体档），不是 `prompts/{lang}_vision_extract_prompt.json`；
扁平位图上的 **Overlay 标注回写**（`shape.create` + `type.create` 画到 jpg/png 上）
与 `MediaWorkspace.vue` 联动尚未做——目前 B 档产出的是「文本 + 定位 + 标注清单 + sidecar」，
回写仍只对含文字层的载体（P0 路径）有效。
**P1 补完（§18）**：推演结果 → 人工勾选 → 导出与专家正误表同构的 Word，已实现并有集成测试。


---

## 18. 重演 → 勾选 → 输出正误表（2026-10-26 实现完成）

**目标**：不是「再多做一个功能」，而是让**当前软件能把专家的验收产物重演出来**——
跑载体拿到推演结果，人工勾选，再输出与专家 `住院告知文字质检正误表.docx` 同构的 Word 表格。

**三段式（`scripts/media_expert_replay.sh`）**

| 段 | 命令 | 产物 |
| --- | --- | --- |
| ① 推演 | `./scripts/media_expert_replay.sh replay`（有凭据 = 真实载体，无凭据 = 离线上限评测） | `target/media-replay/candidates.json` |
| ② 勾选 | 直接编辑 `candidates.json`：`"checked": false` 剔除，各列内容（原文 / 建议 / 计错数 / 备注）可改 | 同一文件 |
| ③ 导出 | `./scripts/media_expert_replay.sh export` = `cargo run -p api-server --bin media-report -- candidates.json` | `report.json` / `report.md` / `report.html` / **`report.docx`** |

**列结构 = 专家表 = gold 解析器（互逆锁死）**
`media_core::report` 输出的 6 列就是 `序号 | 页码及位置 | 错误或待核原文 | 正确或处理建议 | 计错数 | 备注及判定依据`，
`to_columns()` 的产物必须能被 `gold::parse_gold_rows()` 原样读回（`report.rs` 有回归测试锁死）。
这条不变量的意义：**「重演出的表」与「评测召回」是同一把尺子**，不会各算各的。

**每行都带溯源**（JSON 里，不进 Word 列）：`job_id` / `annotation_id` / `unit_id` /
`confidence` / `extraction` / `detector` / `located` / `gold_no`（命中的专家条目号）。
备注列内嵌 `【类别】说明 · 规则 X · 检测 rule · 置信度 0.85 · 抽取 vision · ⚠ 未定位 · 上下文「…」`——
人工核对时不用回到载体也能判断这条是不是误报。

**「未定位」照实写**（与 §7「不画假框」同一条立场）：`Region::Unlocated` 的行
「页码及位置」写 `图片1 未定位`，绝不编造位置。

**跨文本单元的判定直接丢弃（`stitch::crosses_unit()`）**：判定的字节 span 跨越 stitched 单元边界，
说明它把两个独立文本对象当成了连续文本——视觉转录把「强电间」切成两块时，`电` + `间` 在载体上并不相邻，
据此得出的「句末缺句号」根本不成立。这类判定**不是「定位不到」**：留成「未定位」行等于拿假相邻充数，
还会把位置口径统计带偏。`routes/media.rs` 在 remap 前过滤掉并计数告警
（「丢弃 N 条跨文本单元的判定（假相邻：判定文本跨越两个独立文本对象）：规则列表」）。
它与 `remap()` 的「跨单元 → Unlocated」是同一立场的两个环节：**能定位但落不到单个对象的**仍进「未定位」，
**判定本身不成立的**直接出局——出局必须可见，不能悄悄少几条。

**「计错数」默认 + 人工改**：`表达优化 / 其他` 默认「不计」，硬差错默认「计1」；
人工通过 `candidates.json` 或 API 的 `overrides.counted` 改写（与专家「不计」条目仍计入召回 0.5 权重一致）。

**Word 生成走 WordCraft**（`api-server/src/media_report.rs::WordCraft`）：
`wordcraft-cli convert report.html report.docx`。实测（本机 WordCraft CLI）：带边框 `<table>`
转 docx 后表格结构与中文完好，`wordcraft-cli text` 读回是制表符分隔的行。
`wordcraft-cli` 缺失时**降级为告警**（仍出 json/md/html），不谎报有 Word。

**API 面新增**
- `POST /gec/media/report`：`{job_ids?, accepted_ids?, rejected_ids?, overrides?, docx?, title?}`
  → 按载体顺序（= 提交顺序）汇表、`序号` 重排、写产物、返回 `{report_id, candidates, exported, dropped_ids, rows, paths, warnings}`；
  各 job 的告警一并回传（空抽取 / 未定位不会悄悄变成「没错误」）。
- `GET /gec/media/reports/:id?format=json|md|html|docx`：取产物；`id` 只允许 `[A-Za-z0-9_-]`（防目录穿越）。

**评测口径对齐**：`media_gold_eval` 两个评测（上限 / 真实）现在都写 `candidates.json` + 正误表，
并断言 `attach_gold()` 的命中数 == `score_gold()` 的命中数（两套实现必须一致）；
`media-core::gold::score_texts()` 是 `score()` 的文本版，报告与标注共用。

**实测（本机，离线零凭据）**
- 上限评测 → `candidates.json` 6 行（`ZH_TIME_MIX` 的「下午15点」命中 gold #9、
  `ZH_COLON_AFTER_NAME` 的「小程序：」命中 #14、`ZH_CONFUSABLE` 的「报道」只有**位置口径**认（见 §19）、
  其余为多报与正对照句；全部 `计1` + `⚠ 未定位`）→ `report.docx`，
  `wordcraft-cli text` 读回表头与各行一致；
- `media_report_api` 集成测试：真实 PSD（A 档结构层）→ 导出 → 6 列断言 → 剔除一条后 `exported` 少 1 →
  `docx: false` 必须给出「未生成 docx」原因 → **装了 wordcraft-cli 就必须有 docx**，
  且读回文本含表头与该行原文；
- 真实重演（原图 + 视觉模型）待凭据，命令与前置见 §15；届时 `candidates.json` 里每行会带
  `extraction = vision`、`confidence`、`gold_no`，勾选后导出的就是可与专家表逐行 diff 的 Word。

**已做（2026-10-28）**：`web-ui` 的 `MediaWorkspace.vue` 提供 ② 段的勾选界面（勾选 / 计错数 /
改写建议 → `overrides`）。**UI 只用 job 作用域的 `overrides[].checked=false`，不用
`rejected_ids`**：`annotation_id` 每个 job 都从 1 编号，裸 id 列表跨载体必重号——取消勾一张图
上的一条，可能顺手删掉另一张图的同 id 行。`apply_selection` 先应用改写再 `retain(checked)`，
所以「取消勾选」经 override 生效，语义与 UI 一致且按 `job_id` 精确匹配。
**尚未做**：PDF / Word 载体的重演（依赖 §11 P2 适配器）。

## 19. 复现率口径：开卷 / 闭卷 / 位置 / 四维（2026-10-26 实现完成）

**为什么必须拆**：一个「召回 13%」同时混着「抽取没做到」「判定没做到」「判定对了但计分口径没认」——
这种数字没法指导下一步。拆成四个正交口径后，每个数只回答一个问题。

**① 开卷 · 案例库（`media_core::casebase`）**
把「已确认过的判定」做成案例库：归一化（去空白 + 全角标点转半角）精确子串匹配，
命中返回 stitched text 的字节 span（与 `stitch::remap` 同一坐标契约）。
定位：**记忆，不是能力**。它只证明两件事——
「抽取 → 定位 → 回映射 → 导出」链路无损（回归门）、学习闭环可用（确认过的判定下次不再付检测成本）。
命中行一律带 `detector = "case"` 溯源。
**不得**拿开卷数字宣称识别能力；能力必须用没进案例库的材料（留出集 / 第二份专家报告）测。

**② 闭卷 · 文本口径（`gold::score_texts()`）**：既有口径，语义未改。

**③ 闭卷 · 位置口径（`gold::score_spanned()`，新增 `MatchKind::Positional`）**
专家常引整句（`分时段进入病区报道。`），检测器标的是词级片段（`报道`）。文本口径下 2 字符片段
按包含匹配会白送整条 gold（`MIN_CONTAINMENT_CHARS` 拦住了），于是「同一处判定」被判不命中。
`score_spanned()` 在文本口径判不上、但字节区间重合时记 `Positional`；
gold 原文在载体文本中定位不到时传 `None`，**不许**用 `(0,0)` 假 span 蒙命中。

**导出表也接位置口径（`report::attach_gold_spanned()`）**
`ReportRow` 新增两个不进 Word 列的字段：`local_span`（标注在**所属单元内**的字节区间——
不存 stitched 全局坐标，因为全局坐标取决于 stitched 用哪一批单元，存全局值会埋「两套坐标系
对不上」的雷，换算留在调用方看得见的地方）与 `gold_match`（`"text"` / `"positional"`，命中口径留痕）。
评测 harness 先 `attach_gold()`（文本口径）再 `attach_gold_spanned()`（补位置口径的漏），
并断言「表内位置口径命中数 == 评测位置口径命中数」——否则同一份产物会出现「表里 2 条 / 评测输出 3 条」。
真实载体重演**也接了**（`report::attach_gold_by_carrier()`）：每载体一套 stitched 坐标，
所以必须按载体分别打分再汇总（`gold::merge_scores()`：同一条 gold 只记一次，取口径最优者）。
两条理由都是硬伤级的：①A 图的 offset 100 与 B 图的 offset 100 毫无关系，混进一个坐标系就是假命中；
②标注 id 是**每载体各自编号**的，跨载体合并打分时 `annotation_id` 撞号会把 gold 挂到别的载体的行上。
换算由 `report::stitched_spans()` 做（`unit_id` → 该布局里的单元起点 + `local_span`），并**校验该区间
里的文本与行原文一致**——不同载体的 `unit_id` 可能恰好相同、span 也放得下，这时算出的坐标是假的，
宁可不参与位置口径。四维复现同样按载体分别算再汇总（`report::reproduction_by_carrier()`）。

**定位器只容忍空白（`gold::locate_spans()`）**：专家引的整句常被视觉块切成两块，而 stitched 用 `\n`
连接单元——精确字节匹配会把「抽取其实正确」的整句判成未定位。真实载体实测（`origin_pic_1/2.jpg` +
`qwen3.8-flash-next-iq3_s`）：15 条 gold 在转录文本里寻得到 12 条，精确字节匹配下两个载体各自只定位到 6 条 / 1 条（修复后同一批载体升到 11 条 / 2 条），
位置口径与规则回填随之全灭。现在归一化（去空白 + 全半角）后**逐字符相等**才定位，返回的仍是
**原文字节区间**（含中间那个分隔符），所以位置比对与 `stitched_spans()` 的文本一致性校验都不受影响；
**不容忍任何编辑距离**（`分时段进入病区通知。` 定位不到，回归测试锁死）。
同一批真实载体修复前后：文本口径 2 条 / 文本+位置口径 2 条（加权召回 18.2%）
→ 文本口径 2 条 / 文本+位置口径 4 条（加权召回 31.8%），闭卷漏报 13 → 11 条。
新增的两条正是「词级片段靠位置对上专家整句」：`报道`（`ZH_CONFUSABLE` → gold #13）、
`）。`（`ZH_PUNCTUATION` → gold #10）——这类漏报的靶子从此可以是 `rule_tune` 而不是「再补一条词典」。

**规则归因分强弱，兜底必须留痕（`report::missed_rule_evidence()`）**：位置重合是强归因（同一处判定）。
但真实转录常改写整句，gold 原文定位不到时**坐标根本没有**，这类漏报此前完全无法归因
（实测：定位器修好后仍有漏报的 `rule_id` 回填为空，靶子只能退回「补词典」）。
现在加一级**弱兜底**：该行原文归一化后是 gold 原文的子串，归因该规则并标 `rule_via = text`；
有坐标且位置重合的仍是 `positional`，跨载体合并时强归因不被弱归因顶掉。`positional` / `text`
一路带进反思中心：摘要写明「归因＝原文重叠（弱：gold 原文在载体上定位不到，先核对是不是同一条差错）」，
payload 带 `rule_via`。**弱归因不得冒充位置重合**。

下限是**实测定的**，不是拍的：下限取 2 字符那一轮（同一批载体）回填出 7 条归因，
其中 5 条是 `探视` / `办理` / `避免` 这类**两字动词片段**撞进专家整句
（「病房工作电话不咨询病情及商讨探视。」里的「探视」和那条 缺主语 判定毫无关系），
靶子被指向毫不相关的规则触发点。下限抬到 3 字符（gold 原文仍须 ≥ 6 字符）后同一批载体回填 0 条——
**没有归因比假归因好**：漏报仍进 `lexicon_condition` / `l3_sample`，但不会多出一条把人带偏的 `rule_tune`。
两字片段不得归因这条也有回归测试锁死。

**规则层的误报只按「语言层面确定成立」修，不按样本调**（`gec-zh::detect_missing_subject`）：
真实载体上 `ZH_MISSING_SUBJECT` 的命中全是 2 字符片段（`避免` / `办理` / `探视`，置信度 0.50），
但这条规则在 CSCD / NaCGEC 上**一次都没触发**（离线路径没有 POS），所以现有语料**测不出**
收紧它的收益与代价——拿 17 条候选调它就是不测就改。本轮只加一条语言层面站得住的守卫：
**句首是「请…」的祈使式不报缺主语**（「请家属配合」「请勿倚靠」：汉语祈使句省主语是规范用法），
它消掉了真实载体上的 `请` 误报，CSCD / NaCGEC 数字不变（消融基线：401 条 FP 里没有一条 grammar 类）。
剩下的 `避免 / 办理 / 探视` 与「标题/落款缺句号」（`病区` / `医院`，置信度 0.40）、
以及 `ZH_ANOMALY_ASSISTANT_REPLY` 把「若需购车票返回，请买下午15点以后的车票。」判成
AI 污染（置信度 0.90）——都是**该由人工否决进飞轮**的：否决 → `feedback_log` →
反思中心 `lexicon_condition`（`append_rule_exclude`）→ 入库前过测评中心消融。
在载体侧手改出厂词表等于绕过这条回路。

**真实评测有抖动，照实记账**：同一金集六次 `--ignored` 跑出候选 18 / 10 / 18 / 0 / 19 / 17 条。
其中一次载体1 视觉调用 HTTP 503 → job 按设计降级（`来源档=none` + 告警，该载体记 0 命中），
另一次 level 3 的 LLM 层 503 → 告警「降级为规则 + 决策模型」。**不重跑挑好的、也不把降级当成功**；
比对口径时要用同一轮产物（`target/media-replay/`）。
视觉档现在对**瞬时**故障退避重试（`LlmError::is_transient()`：5xx / 429 / 传输层 / 超时，
最多 3 次、退避 2s→5s），重试次数写进 job 告警；未配置与 4xx（除 429）**不重试**，
那是要人修配置，重试只是拖慢失败。
超时也单独拆开了：协作工具 120s（`timeout_secs`）不变，视觉档单独 `vision_timeout_secs`
默认 **300s**（`GEC_MEDIA_VISION_TIMEOUT_SECS`）。理由是实测：4.9MB 实拍告示送短输出只要 60s，
但整页转录（每块带 bbox）会顶到 120s 上限——有一轮两个载体全在视觉档失败、
整轮召回 0%（`来源档=none`，告警照实写），重试 + 放宽超时后同一批载体抽出 38 / 33 单元。
CLI 调用不跟着放宽：挂住的工具要早点失败，不能把一次导出拖成 5 分钟。

视觉档的**重试退避**也按网关实际脾气重定（`media_vision::vision_retry_backoff`）：旧值 2 次 / 2s+5s
在单槽网关上等于没重试——一次 2 分钟转录之后紧跟的请求必落 503 窗口，连续两轮重演的载体 2 全灭，
而同一张图隔几分钟单独探测 108s 就 200。默认改成 **3 次 / 20s+60s+120s**，失败告警里带上
「已重试 N 次、共退避 M 秒」；`GEC_MEDIA_VISION_RETRY_BACKOFF_SECS=0,0` 可免等待（测试用），
全是垃圾值时回默认而不是回「不重试」。

**视觉上传前先压缩（`media_vision::compress_for_vision`）**：原图直接 base64 上送是这段最贵的开销。
实测同一张 4.9MB / 3000+px 实拍告示，短输出请求原图要 **108s**，降到长边 2000px + JPEG q80
（725KB）后 **2.6s** 就回 200；另一张 3.6MB → 516KB（约 7 倍）。所以三个旋钮进 `MediaSettings`：
`vision_max_edge`（默认 2000，`GEC_MEDIA_VISION_MAX_EDGE`）、`vision_max_bytes`（默认 1.5MB，
`GEC_MEDIA_VISION_MAX_BYTES`）、`vision_jpeg_quality`（默认 80，`GEC_MEDIA_VISION_JPEG_QUALITY`）；
超预算先降质量、质量见底再降边长，带 alpha 的走无损 PNG（JPEG 会把透明填黑）。
**bbox 是归一化坐标，降采样不影响回写定位**——这正是 §9「体积/时长」那行的落地方式：
不需要外部 CLI，压缩在 api-server 进程内做（`image` crate，只开 jpeg/png/webp 解码）。
压了多少（前后字节 + 像素 + 质量）写进 job 告警；解不开 / 压了反而更大时**原样上传但留告警**，
不能静默把「没压」读成「压过了」。

**压缩上线后的首轮真实重演（run13）**：两个载体都出单元（38 / 34 单元），**图2 首次尝试即成功**——
它在 run11 / run12 连续两轮以 503 全灭（run12 退避加到 200s 仍失败）。压缩比 4.95MB→508KB、
3.62MB→366KB（≈10×），抽取质量没掉：图1 的 `vision_raw.txt` 与全分辨率那轮**同规格**
（4710B / 38 块 / 788 字），图2 34 块（全分辨率是 33 块）。产出 25 条（`detector`：rule 15 / llm 10），
文本口径命中 3、位置口径 4（加权召回 31.8%）；按载体：图1 召回 22.2% / 精确率 22.2%，
图2 召回 50.0% / 精确率 6.2%。精确率比 run12（22.2%）低是**口径变了不是变差了**：run12 图2 直接
0 产出，这轮图2 交出 16 条（多数是 FP）——召回 18.2%→27.3% 与精确率下降是同一件事的两面，
只报好看的那半就是自欺。

**LLM 层的两个「静默白跑」必须先排掉（2026-10-27 实测）**：
① **模型名不存在** → 网关回 HTTP 400（`Invalid model name passed in model=…`），media job 侧
表现为告警「LLM 层失败…降级为规则 + 决策模型（level 1）」，level 3 实际只跑了 level 1。
本机网关 `192.168.1.130:4000/v1` 上只有 `qwen3.8-flash-next-iq3_s` / `qwen3.8-27b`，
设置文件此前写的是 `qwen3.8-flash-next`（少后缀）→ 每一轮重演的 LLM 层都是白跑。
另注意 `llm_model_id`（`prompts/models.json` 预设）会**覆盖**端点与模型名：`cec3` / `cec4`
指向本机 vllm（127.0.0.1:8011 / 8010），这两个端口没起时该选 `generic`（走用户当前 LLM 端点）。
①b **超时默认太短**：`LlmConfig::from_env()` 的 `LLM_TIMEOUT_SECS` 默认 **30s**，而真实载体
整载体文本（≈2.5k 字）送思考模型实测 **60.3s**（8087 个 completion token，绝大部分是思考）——
30s 下每一载体的 LLM 层必然以「error sending request」失败并降级 level 1。重演必须显式
`LLM_TIMEOUT_SECS=300`；传输层错误现在带上 `source` 链与超时上限（`transport_chain()`），
不再只能猜。
② **思考模型的 JSON 契约被思考块污染**（`gec-core::llm`）：thinking 档把最终答案放
`content`、思考放 `reasoning_content`，而 `chat_completion()` 把思考块拼在答案**前面**；
思考里会回显指令原文（指令里就含 `[]`），`extract_json_array()` 取「首个 `[` 到末个 `]`」
就把说明文字吞进 JSON → 解析失败 → **该层返回 0 条且不报错**。
实测同一句「他做事很任信…」：直连网关给出 `任信→任性`，服务端却是 `llm.found=0` +
`llm_confirmed_clean=true`（55s 空转）。修法是 JSON 契约走 `chat_completion_answer()`
（只取 `content`，空时才回退思考块），数组提取改成**配平扫描 + 取第一个非空数组**
（`[]` 仍是「无错误」的合法答案，字符串内括号不计深度，截断的数组不猜）；
`plain` 分支（CEC think 块）仍用拼接版。顺带排掉污染面：`llm_confirmed_clean` 会喂
L3 飞轮的「干净」负样本，但 `l3_data/zh/confirmed.jsonl` 里 509 条 `llm_clean` 全部
产自 2026-10-16/17（早于思考档配置），没有被这个 bug 污染，不需要回滚训练集。

**闭卷 + LLM 层实测（2026-10-27，配置修好后第一次有意义的对照）**：把金集 15 条「错误或待核原文」
拼成一段送 `/gec/detect` level 3（走设置文件），**108.3s** 返回：`rule` 层 7 条、`llm` 层 **5 条**，
合并产出 6 条（`source=llm` 5 / `rule` 1），触及金集 6/15——含金集 #13 硬差错 `报道→报到`、
#9 计错 `下午15点`、#11 `病房工作电话不咨询病情及商讨探视`。此前同一入口 `llm.found` 恒为 0
（思考块污染解析 + 30s 超时），所以「LLM 层没贡献」从来不是模型能力问题，是配置与解析问题。
108s 也印证超时旋钮：30s（默认）与 120s 都会把这一层掐死，必须 ≥300s。

**网关抖动照实记账**：紧接着的真实重演（run8/run9）两个载体视觉档全部 HTTP 503（重试 2 次仍失败），
整轮召回 0%——这两轮不能与 run6/run7 比对。同一时间窗 `/v1/models` 与文本补全又正常，
说明不是配置问题。探测坐实了原因（`/tmp/vision_probe.py`，三条并发）：3.6MB 图 **108.4s 返回 200**，
而同一时刻 1×1 小图排队 **300s 超时**——这个网关实质是**单槽串行**，重请求排队时后来者要么 503
要么超时。结论：跑真实重演时**不要并发任何别的 LLM 调用**（探测、消融、第二次重演都不行），
一轮跑完再跑下一轮。

**全层对比（level 1/2/3 消融，2026-10-27）**：视觉档不可用的那段时间，把上一轮抽好的载体文本
（`lf_gold_run_*/vision_raw.txt`）当**固定输入**，只变 level——层的差异就不被抽取抖动污染。
脚本 `/tmp/layer_ablation.py`（起服务 → 同一文本送 level 1/2/3 → 对金集重算命中）：

| 载体 | level | 耗时 | 层状态（layer=status/检出） | 合并产出 | 命中 gold | 计差错命中 |
| --- | --- | --- | --- | --- | --- | --- |
| 图1（835 字） | 1 | 10.6s | rule ran/7 · knowledge ran/0 | 2 | 1/13 | 1/5 |
| 图1 | 2 | 50.5s | + llm ran/3 · feature unavailable/0 | 4 | 2/13 | 2/5 |
| 图1 | 3 | 80.8s | llm ran/4 | 6 | 3/13 | 3/5 |
| 图2（567 字） | 1 | 2.9s | rule ran/10 · knowledge ran/0 | 3 | 1/2 | 1/2 |
| 图2 | 2 | 98.0s | + llm ran/1 | 4 | 2/2 | 2/2 |
| 图2 | 3 | 72.9s | llm ran/2 | 4 | 2/2 | 2/2 |

读数（照实）：

- **LLM 层的增量是真的**：图1 命中从 1 条涨到 3 条，新增 `难以程度→难易程度`、
  `科室开始办理出院病历→手续`、`住院可通过电话…→住院期间可通过电话…`、
  `病房工作电话不咨询病情及商讨探视→不用于咨询…`；图2 的 `报道→报到`（金集 #13 硬差错）
  在 level 2/3 以 **llm+rule 共识**命中（`also_found_by` 生效，前端「共识 ×2」徽章有数据了）。
- **层检出数 ≠ 合并产出数**：`rule=ran/7` 最终只产出 2 条——`merge_errors` 把重叠 span 合并、
  落选层记入胜出者的 `also_found_by`。报告里两个数都得给：只报产出条数，看不出规则层在忙什么，
  也看不出「多层共识」这种可信度信号。
- **决策层没部署时 level 2 不比 level 3 省**：`decision.enabled=false`（4804 未起）→ 引擎按弃权处理
  → LLM 兜底照样触发。图2 level 2 花了 98.0s 反而比 level 3 的 72.9s 慢，命中不多一条。
  别把「level 2 更便宜」当默认假设，先确认决策层在不在。
- `feature=unavailable/0`（L3 4806 未起）照实上报，没有静默当成「没错误」。
- 消融期间撞上过一次 HTTP 502（网关单槽排队），脚本侧退避重试吸收掉了——否则整层数据缺失。

**端到端全层对比（同一金集、同一批原图，2026-10-27）**：

| 轮次 | 视觉档 | LLM 层 | 图1 标注 | 图1 召回 | 图1 精确率 | 合计召回 | 合计精确率 | 产出 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| run6 / run7 | 38 / 31–33 单元 | 坏（模型名 400、超时 30s） | 7 | 11.1% | 14.3% | 18.2% | 11.8% | 17 |
| run8 / run9 | 503 → 0 单元 | 未参与 | 0 | 0% | — | 0% | — | 0 |
| run10 | 图1 38 单元 / 图2 503 | **好** | **11** | **44.4%** | **36.4%** | 36.4% | 36.4% | 11 |
| run11 | 图1 39 单元 / 图2 503 | 503 → 降级 level 1 | 7 | 11.1% | 14.3% | 9.1% | 14.3% | 7 |

- **同一载体、几乎同一份抽取**（38 vs 39 单元）：LLM 层在场 → 标注 7→11、命中 1→4、
  召回 11.1%→44.4%、精确率 14.3%→36.4%。LLM 层不只加召回，**也加精确率**——它给的 5 条里
  3 条命中金集（`血压心率增高→血压、心率增高`、`需等…无特殊变化→…后`、`病房工作电话不咨询…`），
  另两条（`难以程度→难易程度`、`下午15点→15点`）金集没标但确实是错：金集不是完备集，
  所以精确率的分母里天然含「金集漏标」的伪 FP。
- run10 的 11 条候选拆开（`scripts/media_replay_inspect.py`）：`无规则(LLM)×5`、
  `ZH_MISSING_SUBJECT×3`（conf 0.5，全是 FP）、`ZH_PUNCTUATION×2`（`）。→。` 以**位置口径**
  命中金集 #10；`病区→。` conf 0.4 明显 FP）、`ZH_ANOMALY_ASSISTANT_REPLY×1`
  （命中金集 #9 但 `counted=不计`）。精确率的拖累集中在 `ZH_MISSING_SUBJECT`（3/11）
  与低置信标点（1/11）——这是反馈/消融该干的活，不是照着两条载体手改词典。
- **降级现在会自己说明白**：run11 图1 告警是
  `LLM 层失败（upstream error: LLM 服务返回 HTTP 503 Service Unavailable）：本次降级为规则 +
  决策模型（level 1）；修好 LLM 配置重跑可得全层结果`——旧文案只说「降级」，不说掉到哪层、怎么修。
- **反思靶子自己会随层数收敛**：run11（LLM 掉线）闭卷漏报 13 条，靶子覆盖 `[3, 8, 11, 12, 13, 14]`，
  其中 `lexicon_condition` 点名金集 #3（补词表条件）；run10（LLM 在场）漏报降到 10 条，靶子只剩
  `[12, 13, 14]`——**#3 / #8 / #11 正好被 LLM 层吃掉**，一条词典没改。也就是说 `llm_arm` 那条
  「编辑判断类不该由词典硬判」的建议是对的：先开层，再谈补词典，别反过来。
- 闭卷漏报 10 条的规则归因全为 `(无归因)`：图2 那轮 0 单元，`#13 报道` / `#14 小程序：` 根本没有
  载体文本可对位；图1 的漏报多为表达优化类，规则本来打不到。**归因为空 ≠ 归因逻辑坏了**，
  是载体缺失——报告里必须写清，否则会被读成「规则层没能力」。

**人工改写即失效（`apply_selection()`）**：覆盖里改到「错误或待核原文」时，一并清掉该行
`gold_match` 与 `local_span`——旧口径与旧文本坐标对新原文不再成立，留着就是冒充「机器验证过」；
`gold_no` 保留作人工对照（读者要的是「对应专家表第几条」），但它已不是打分依据。只改建议/计错数不清。

**④ 四维复现率（`report::reproduction()`）**——逐维给数，不合成一个含糊的百分比：

| 维度 | 判据 | 门槛 |
| --- | --- | --- |
| 原文对齐 | 命中行「错误或待核原文」与 gold 原文重叠度 | ≥ `REPRO_ORIGINAL_OK` 0.6 |
| 位置一致 | 两侧解析出同一「图片N」；任一侧「未定位」直接不达标 | — |
| 计错数一致 | 命中行 `counted` == gold `counted` | — |
| 建议接近 | 建议文本重叠度（**近似**口径，措辞等价的最终判定交人工） | ≥ `REPRO_SUGGESTION_OK` 0.35 |

四维全达标才算「严格复现」。命中关系有两个版本：`reproduction()`（文本口径）与
`reproduction_spanned()`（位置口径，评测默认用这个）——位置口径补进来的条目会计入「计错数一致」，
但「原文对齐」仍不达标（词级片段 ≠ 专家整句）：**计入同一处判定，不等于假装原文一致**。
上限评测里位置维必然是 0（单元是 `Region::Unlocated`，没有坐标）——
这不是漏报，是评测材料本身无位置；真实位置维只能来自真实载体重演（`--ignored`）。

**实测（本机 2026-10-26，离线零凭据；出厂 zh 词表开启 `ZH_CONFUSABLE` / `ZH_MISSING_SUBJECT` /
`ZH_DE_DA_DE`，新增 `ZH_TIME_MIX` / `ZH_COLON_AFTER_NAME`）**

| 口径 | 结果 |
| --- | --- |
| ① 开卷 · 案例库 | 15/15（记忆，不是能力） |
| ② 闭卷 · 文本口径 | 2/15（计错项 2/7，加权召回 18.2%） |
| ③ 闭卷 · 位置口径 | 3/15（计错项 3/7，加权召回 27.3%） |
| ④ 四维 | 原文 2 · 位置 0（材料无坐标）· 计错 3 · 建议 0 → 严格复现 0 |

命中明细：#9 `下午15点`（`ZH_TIME_MIX`）、#14 `微信小程序：`（`ZH_COLON_AFTER_NAME`）、
#13 `报道`（`ZH_CONFUSABLE`，只有位置口径认）——导出表里这三行分别带
`gold_no` = 9 / 14 / 13、`gold_match` = `text` / `text` / `positional`。
稳定性实测：连跑两次金集重演数字逐行一致；跑完整个 `api-server` 套件后出厂词表 `diff` 为空。
剩余计错项（#3 并列缺分隔、#8/#11/#12 主谓宾残缺、#15 表述不清）是语义判断，
靶子在 LLM 层与反思中心，不是再加几条词典能解决的——这也是为什么必须把开卷/闭卷分开报。

**评测数字必须与「跑过哪些测试」无关（实测踩到的坑，已修）**
规则开关路由会把状态**回写词表文件**（`routes/rules.rs::sync_rule_toggles` →
`LexiconStore::sync_rules_to_file`；该设计是为防「空库重新播种把已停用规则静默重新启用」）。
`api` / `feedback_api` / `reflection_api` 三个测试套件的 `lexicons_abs()` 原本直接指向仓库 `lexicons/`，
于是 `put_rule_toggles_persists_and_reloads` 把出厂 `ZH_CONFUSABLE.enabled` 写回 `false`——
同一份代码两次跑金集重演，检出从 6 条掉到 5 条（金集召回随测试顺序漂移）。
修复：这三个套件的 `lexicons_abs()` 改为**出厂词表的临时副本**（`agent_api::lexicons_temp_copy()` 早就是这个做法），
写只落在临时目录。不变量：**出厂词表是工厂数据，测试不得改它**。

**LLM 层跑挂不得打死 job**：`run_media_job` 检测失败时，若请求的是 level 3 / `force_llm`，
自动退回 level 1（规则 + 决策模型）并把原因写进 `warnings`
（`LLM 层失败（…）：本次降级为规则 + 决策模型（level 1）；修好 LLM 配置重跑可得全层结果`）；
降级后仍失败才判 job 失败。与建 job 时「LLM 层未配置：本次降级…」同一立场——
**跑不动必须可见，不能变成「没错误」**。

**闭卷漏报必须落成靶子（`gec_core::classify_miss()` / `gold_suggestions()` / `build_report_with_gold()`）**
「召回 18%」这句结论对下一步没有指导作用——**具体哪几条没抓到、该由谁去抓**才有。
反思中心入口 `POST /gec/reflect/run` 因此新增可选 `gold` 字段（`GoldEvidenceIn`：
`source` / `gold_total` / `counted_total` / `text_hits` / `positional_hits` / `misses`），
金集 harness 每次跑都写产物 `target/media-replay/reflection_seeds.json`，并**立刻回灌**
`/gec/reflect/run` 验证建议队列（闭环，不靠人工复制粘贴）。

靶子分类是**确定性**映射（`classify_miss()`）：

| 漏报形态 | 靶子（建议 kind） | 为什么 |
| --- | --- | --- |
| 规则已触发但未 credited（`rule_id` 有值） | `rule_tune` | 是规则不对，不是缺规则——再补词典只会多一处误报 |
| 计差错 + 判定依据第一句含可形式化关键词（错别字 / 标点 / 分隔 / 时间数字…） | `lexicon_condition` | 局部可形式化，条件化就能抓 |
| 计差错其余形态（搭配 / 层次 / 主语残缺） | `l3_sample` | 规则写不动，把专家改法作为正样本喂 L3 飞轮 |
| 不计差错（表达优化 / 表述不清 / 未定） | `llm_arm` | 编辑判断，词典硬判就是误报源头 |

优先级：计差错漏报 `0`、编辑判断 `2`——**编辑判断不得抢计差错漏报的位置**。
关键词只看判定依据**第一句**：金集 #8 后面补了「具体时间由院方确认」，
整段扫关键词会把这条结构性差错误分进词表靶子（实测踩到，故 `first_clause()` 截断）。

`rule_tune` 靶子还必须带**归因强度**（`GoldMissIn.rule_via`：`positional` = 同载体坐标重合，
`text` = 原文子串重叠兜底）：弱归因要先人工核对「是不是同一条差错」，不能与强归因一样直接改规则。

合并同靶子建议时必须对 payload 的 `gold_nos` / `originals` **求并集**
（`push_gold_suggestion()`）：既有 `push_suggestion()` 只合并 evidence，
payload 被首条占住，后来条目的专家条目号就丢了——「这条建议对应专家表哪几条」必须可追溯，
不能靠运气（实测：#8 并入 #3 后 `gold_nos` 只剩 `[3]`，harness 断言当场抓住）。

不变量与实测：
- harness 断言**每条未命中的计差错条目都必须出现在某条建议的 `gold_nos` 里**（漏报不给靶子 = 接线坏了）；
- 金集证据**只补充、不覆盖**反馈侧判定：`build_report_with_gold(…, None)` 与 `build_report(…)`
  的 findings / suggestions 完全一致（测试锁住）；
- 实测（离线 level 1）：12 条闭卷漏报 → 3 条建议——
  `lexicon_condition`（#3 并列缺分隔）、`l3_sample`（#8 / #11 / #12，证据 3 条）、
  `llm_arm`（8 条编辑判断）；`api-server` 集成测试验证建议落队列（`pending`）且 payload 带 `gold_nos`；
- 建议是**靶子不是自动执行**：状态机 `pending → applied / consumed / dismissed`，人工可否决。
  分类错没关系，**不提才是问题**。
- `ReportRow` 新增 `rule_id`（`candidates()` 从 `Error::rule_id` 带出，不进 Word 列）；真实重演 harness
  用 `report::missed_rule_ids()` 回填「规则触发但未 credited」的规则 id：判据是该 gold 原文在此坐标系
  中有坐标，且存在某行（有坐标、带 `rule_id`、未 credited 到任何 gold）与它位置重合 → 靶子走
  `rule_tune`（改规则），不是「再补一条词典」；上限评测是闭卷跑的，没有触发记录，恒传空表；
- 重演产物回灌是**同一条命令内可验证**的：harness 写完 `reflection_seeds.json` 立刻把它 POST 回
  `/gec/reflect/run`（`post_seeds_to_reflection()`），断言每条未命中的计差错条目都出现在队列建议的
  `gold_nos` 里——不靠人工复制粘贴。

