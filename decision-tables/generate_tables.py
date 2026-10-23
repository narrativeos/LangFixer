#!/usr/bin/env python3
"""生成 LangFixer 三张 JDM 决策表（zen-engine 2.1.2 方言）+ 知识库。

- 三张表结构同构：4 输入（errorType/confidence/history/isNovice）+ 7 输出，
  只承载**可提取的规则**（错误类型 × 新手/置信度 → 归因策略 + 通用补救 + CEFR）。
- 英文表不内嵌知识点详情：知识点（讲解/例句/改错对）外置到知识库
  `knowledge-base/`：知识源注册表（sources.json）+ 知识点集合（points/{lang}.json），
  知识点 → 知识源按 1:N 关联。仓库内置的英文知识源是**自撰示例源**
  （`src-sample-en`，LangFixer 示例英语语法指南，Apache-2.0），
  用于演示数据形态与回归测试；真实书籍属用户数据，经导入管线摄取，
  `license_scope != public` 的源不随发行物分发（见 README「IP 边界」）。
"""
import json

def q(s):
    return json.dumps(s, ensure_ascii=False)

INPUTS = [
    {"id": "errorType", "name": "errorType", "field": "errorType"},
    {"id": "confidence", "name": "confidence", "field": "confidence"},
    {"id": "history", "name": "studentHistoryCount", "field": "studentHistoryCount"},
    {"id": "isNovice", "name": "isNovice", "field": "isNovice"},
]
OUTPUTS = [
    {"id": "attribution", "name": "attribution", "field": "attribution"},
    {"id": "weinerLocus", "name": "weinerLocus", "field": "weiner.locus"},
    {"id": "weinerStability", "name": "weinerStability", "field": "weiner.stability"},
    {"id": "weinerControllability", "name": "weinerControllability", "field": "weiner.controllability"},
    {"id": "remedial", "name": "remedial", "field": "remedial[]"},
    {"id": "matchedRule", "name": "matchedRule", "field": "matchedRule"},
    {"id": "cefrLevel", "name": "cefrLevel", "field": "cefrLevel"},
]

def rule(rid, conds, attribution, locus, stability, ctrl, remedial=None, cefr="A1", source_id=None):
    # JDM 行级元数据（zen-engine 2.1.2 方言要求规则行全部为字符串键值）：
    #   _id   = 规则稳定 ID；
    #   _meta = 派生该规则的知识源 source_id（空 = 产品内置通用规则）。
    # 资源包导出/删除按 _meta 摘除对应源的规则行。
    r = {"_id": rid}
    if source_id:
        r["_meta"] = source_id
    r.update(conds)
    r["attribution"] = q(attribution)
    r["weinerLocus"] = q(locus)
    r["weinerStability"] = q(stability)
    r["weinerControllability"] = q(ctrl)
    if remedial is not None:
        r["remedial"] = q(remedial)
    r["matchedRule"] = q(rid)
    r["cefrLevel"] = q(cefr)
    return r

def table(name, rules, inputs=INPUTS, outputs=OUTPUTS):
    return {
        "nodes": [
            {"id": "input", "name": "input", "type": "inputNode"},
            {"id": "table", "name": name + "_table", "type": "decisionTableNode",
             "content": {"inputs": inputs, "outputs": outputs, "rules": rules, "hitPolicy": "first"}},
            {"id": "output", "name": "output", "type": "outputNode"},
        ],
        "edges": [
            {"id": "e1", "sourceId": "input", "targetId": "table"},
            {"id": "e2", "sourceId": "table", "targetId": "output"},
        ],
    }

S, U, C = "stable", "unstable", "controllable"
I = "internal"

# 归因标签
KG = "knowledge_gap"      # 知识缺口
RM = "rule_missing"       # 规则缺失
RF = "rule_forgotten"     # 规则遗忘
HS = "habit_solidified"   # 习惯固化
CW = "collocation_weak"   # 搭配薄弱

# 归因 → 韦纳三维度（内外源 × 稳定性 × 可控性）
WEINER = {
    KG: (I, U, C),
    RM: (I, U, C),
    RF: (I, S, C),
    HS: (I, S, C),
    CW: (I, S, C),
}

zh_rules = [
    rule("spelling_high_conf_old_hand",
         {"errorType": '== "spelling"', "confidence": ">= 0.9", "isNovice": "== false", "history": "> 1"},
         "habit_solidified", I, S, C, "对比示例：辨析同音/形近字的语义差异", cefr="A2"),
    rule("spelling_novice", {"errorType": '== "spelling"', "isNovice": "== true"},
         "knowledge_gap", I, U, C, "规则讲解：同音字/形近字辨析", cefr="A2"),
    rule("spelling_low_conf", {"errorType": '== "spelling"', "confidence": "< 0.9"},
         "rule_forgotten", I, S, C, "复核提示：置信度较低，建议人工确认", cefr="A2"),
    rule("grammar_novice", {"errorType": '== "grammar"', "isNovice": "== true"},
         "rule_missing", I, U, C, "规则讲解：句子成分与语序", cefr="A2"),
    rule("grammar_high_conf_old_hand",
         {"errorType": '== "grammar"', "confidence": ">= 0.8", "isNovice": "== false"},
         "rule_forgotten", I, S, C, "规则讲解：针对性回顾语法规则", cefr="A2"),
    rule("grammar_old_hand", {"errorType": '== "grammar"', "isNovice": "== false"},
         "knowledge_gap", I, U, C, "规则讲解：补充缺失的语法知识", cefr="A2"),
    rule("collocation_any", {"errorType": '== "collocation"'},
         "collocation_weak", I, S, C, "搭配专项训练：高频搭配强化", cefr="B1"),
    rule("punctuation_novice", {"errorType": '== "punctuation"', "isNovice": "== true"},
         "rule_missing", I, U, C, "规则讲解：中文标点使用规范", cefr="A1"),
    rule("punctuation_old_hand", {"errorType": '== "punctuation"', "isNovice": "== false"},
         "habit_solidified", I, S, C, "规则讲解：标点规范", cefr="A1"),
    rule("default_knowledge_gap", {}, "knowledge_gap", I, U, C, cefr="A1"),
]

# ─────────────────────────────────────────────────────────────────────────────
# 英文表：纯规则（errorType × 新手/置信度 → 归因策略），与 zh/ja 表结构同构。
# 知识点详情（讲解 / 例句 / 改错对 / 来源）不进表：外置于知识库 knowledge-base/，
# 运行时由归因引擎按 grammar_point 富化（知识点 → 1:N 知识源）。
#
# 错误类型体系（25 类）为产品内置的英文语法错误分类，与检测器
# （gec-en）的 error_types 对齐；每类给兜底 CEFR / 补救 / 归因。
# ─────────────────────────────────────────────────────────────────────────────

EN_ERROR_TYPES = [
    ("article",            "A2", "规则讲解：冠词（定冠词/不定冠词/零冠词）用法", KG),
    ("numeral",            "A2", "规则讲解：数词（基数词/序数词/分数/主谓一致）用法", KG),
    ("pronoun",            "A2", "规则讲解：代词（人称/物主/反身/指示）用法", KG),
    ("preposition",        "B2", "搭配专项训练：介词搭配强化", CW),
    ("adjective",          "B1", "规则讲解：形容词（位置/比较级/最高级）用法", KG),
    ("adverb",             "B1", "规则讲解：副词（种类/位置/比较）用法", KG),
    ("noun",               "A2", "规则讲解：名词（单复数/所有格/可数与不可数）用法", KG),
    ("verb",               "B1", "规则讲解：动词（及物与不及物/动词句型）用法", KG),
    ("tense",              "B1", "规则讲解：时态系统（常见十二时态）", KG),
    ("passive",            "B1", "规则讲解：被动语态构成与用法", KG),
    ("infinitive",         "B1", "规则讲解：不定式（to do）用法", KG),
    ("gerund",             "B1", "规则讲解：动名词（-ing 作名词）用法", KG),
    ("participle",         "B2", "规则讲解：现在分词/过去分词用法", KG),
    ("subjunctive",        "B2", "规则讲解：虚拟语气（if 条件/wish 等）", KG),
    ("sentence_structure", "B1", "规则讲解：基本句型与并列句", KG),
    ("noun_clause",        "B2", "规则讲解：名词性从句（主语/宾语/表语/同位语）", KG),
    ("attributive_clause", "B2", "规则讲解：定语从句（关系词/限制性/非限制性）", KG),
    ("adverbial_clause",   "B2", "规则讲解：状语从句（时间/条件/让步/原因）", KG),
    ("agreement",          "B1", "规则讲解：主谓一致（语法一致/意义一致/就近原则）", KG),
    ("inversion",          "B2", "规则讲解：倒装（全部倒装/部分倒装）", KG),
    ("tag_question",       "B1", "规则讲解：反意疑问句构成", KG),
    ("ellipsis",           "B2", "规则讲解：省略（重复成分省略）", KG),
    ("emphasis",           "B2", "规则讲解：强调句型（It is … that）", KG),
    ("negation",           "B1", "规则讲解：否定（not/no/部分否定/双重否定）", KG),
    ("rhetoric",           "C1", "规则讲解：常见修辞（排比/对比/设问）", KG),
]

# 内置示例知识点（自撰内容，Apache-2.0；条目 = 小节 / 标题 / 补救 / CEFR / 归因 /
# 例句 / 改错对）。知识点 ID = {errorType}.{序号}，与决策表规则、检测器归点
# （gec-en::classify_point）对齐；示例点刻意覆盖归点映射所需的关键序号：
#   article.3（不定冠词）/ tense.8（时态呼应）/ agreement.1（集体名词）/
#   preposition.24（in）/ adjective.22 / adverb.11 / noun.16 / pronoun.9 /
#   verb.8 / infinitive.4 / gerund.4 / subjunctive.8 / negation.3。
def E(section, title, remedial, cefr, attr, examples=(), corrections=()):
    return {"section": section, "title": title, "remedial": remedial,
            "cefr": cefr, "attr": attr, "examples": list(examples),
            "corrections": [{"wrong": w, "right": r} for w, r in corrections]}

SAMPLE_POINTS = {
"article": [
    E("一.1", "定冠词 the 的基本用法", "规则讲解：the 用于特指（已知/独一无二/上文提及）", "A2", KG,
      ["The sun rises in the east. 太阳从东方升起。"]),
    E("一.2", "零冠词：泛指与专有名词", "规则讲解：复数/不可数泛指及多数专有名词不加冠词", "A2", KG,
      ["Cats like fish. 猫喜欢吃鱼。"]),
    E("二.1", "不定冠词 a/an 的选择", "规则讲解：a/an 取决于读音而非拼写——元音音素前用 an", "A2", KG,
      ["She is reading an interesting book. 她正在读一本有趣的书。"],
      [("a ancient source", "an ancient source")]),
    E("二.2", "元音音素开头的名词短语", "规则讲解：university/year 以辅音音素开头用 a", "A2", KG,
      [],
      [("an university", "a university")]),
    E("三.1", "泛指单数：a + 职业/身份", "规则讲解：职业身份首次提及用 a/an", "A2", KG,
      ["My sister is a doctor. 我姐姐是医生。"]),
    E("三.2", "抽象名词具体化", "规则讲解：抽象名词表一件具体事情时可加 a/an", "B2", KG),
    E("四.1", "冠词与最高级/序数词", "规则讲解：最高级与序数词前用 the", "A2", KG),
    E("四.2", "固定短语中的冠词", "规则讲解：in the end / at once 等固定短语的冠词", "B1", KG,
      [],
      [("in morning", "in the morning")]),
    E("五.1", "冠词遗漏常见场景", "规则讲解：可数单数名词不能裸用——缺冠词高频错", "B1", KG),
    E("五.2", "冠词误用小结", "规则讲解：a/the/零冠词三选一的判断流程", "B1", KG),
],
"numeral": [
    E("一.1", "基数词的读写", "规则讲解：基数词拼写与数字读法", "A2", KG),
    E("一.2", "hundred/thousand 的单复数", "规则讲解：具体数字后不加 s，hundreds of 除外", "A2", KG),
    E("二.1", "序数词的构成", "规则讲解：序数词拼写规则与 the 搭配", "A2", KG),
    E("二.2", "序数词的用法", "规则讲解：第几……用 the + 序数词", "A2", KG),
    E("三.1", "分数与百分数", "规则讲解：分子基数/分母序数，分子大于一分母加 s", "B1", KG),
    E("三.2", "小数与倍数", "规则讲解：倍数表达 times + 名词", "B1", KG),
    E("四.1", "时间与日期表达", "规则讲解：日期与时刻的读写", "A2", KG),
    E("四.2", "数量与主谓一致", "规则讲解：数量短语作主语的主谓一致", "B1", KG),
],
"pronoun": [
    E("一.1", "人称代词主格与宾格", "规则讲解：主格作主语、宾格作宾语/介词后", "A2", KG),
    E("一.2", "宾格与介词宾语", "规则讲解：介词后代词用宾格", "A2", KG),
    E("二.1", "形容词性与名词性物主代词", "规则讲解：my/mine 类物主代词区分", "A2", KG),
    E("二.2", "物主代词与句尾指代", "规则讲解：句尾指代用名词性物主代词", "A2", KG),
    E("三.1", "反身代词", "规则讲解：主语与宾语同指时用反身代词", "A2", KG,
      ["He taught himself to play the guitar. 他自学弹吉他。"]),
    E("三.2", "反身代词作同位语", "规则讲解：同位语强调用反身代词", "B1", KG),
    E("四.1", "指示代词 this/that", "规则讲解：近远指与上下文回指", "A2", KG),
    E("四.2", "指示代词与主谓一致", "规则讲解：this/these 与动词数的搭配", "A2", KG),
    E("五.1", "关系代词在定语从句中", "规则讲解：关系代词引导定语从句（详见从句讲）", "B1", KG),
    E("五.2", "易混代词小结", "规则讲解：it/one/that 与 them/those 辨析", "B1", KG),
],
"preposition": [
    E("一.1", "时间介词 in/on/at", "规则讲解：in（月/年/季节）on（日）at（时刻）", "A2", CW),
    E("一.2", "方位介词 in/on/at", "规则讲解：in（内部）on（表面）at（点）", "A2", CW),
    E("二.1", "方向介词 to/into", "规则讲解：to 表方向、into 表进入", "A2", CW),
    E("二.2", "位置介词 between/among", "规则讲解：两者之间用 between，三者以上 among", "B1", CW),
    E("三.1", "原因与理由介词 for/of", "规则讲解：thank sb for sth；proud of", "B1", CW),
    E("三.2", "工具与方式介词 with/by", "规则讲解：with 工具、by 方式", "B1", CW),
    E("四.1", "关于介词 about", "about 表关于及大致数量", "B1", CW),
    E("四.2", "上方介词 above/over", "above/over 辨析：垂直与一般高于", "B1", CW),
    E("四.3", "之后介词 after", "after 表时间/顺序在后", "A2", CW),
    E("四.4", "反对介词 against", "against 表反对、对抗", "B1", CW),
    E("四.5", "地点介词 at", "at 表小地点与具体位置", "A2", CW),
    E("四.6", "之前介词 before", "before 表时间/顺序在前", "A2", CW),
    E("四.7", "超出介词 beyond", "beyond 表超出范围", "B2", CW),
    E("四.8", "除……外介词 but", "but 表除……之外", "B1", CW),
    E("四.9", "方式介词 by", "by 表方式、交通方式与被动施动者", "B1", CW),
    E("四.10", "目的介词 for", "for 表目的、受益对象", "A2", CW),
    E("四.11", "来源介词 from", "from 表来源、起点", "A2", CW),
    E("四.12", "地点介词 in", "in 表在……之内（大地点/时间范围）", "A2", CW,
      ["He was wounded in the leg. 他腿部受了伤。"],
      [("I'll meet you at Friday.", "I'll meet you on Friday.")]),
    E("五.1", "进入介词 into", "into 表进入内部的动态", "A2", CW),
    E("五.2", "所属介词 of", "of 表所属、部分与数量", "A2", CW),
    E("五.3", "地点介词 on", "on 表在……表面/特定日", "A2", CW),
    E("五.4", "外出短语 out of", "out of 表从内部向外", "A2", CW),
    E("五.5", "方向介词 to", "to 表方向与间接宾语位置", "A2", CW),
    E("五.6", "下方介词 under", "under 表正下方", "A2", CW),
    E("五.7", "伴随介词 with", "with 表工具、伴随、具有", "B1", CW),
    E("五.8", "介词 + 疑问代词", "介词后接 what/whom 的疑问结构", "B1", CW),
    E("五.9", "介词 + 关系代词", "介词后接 whom/which 的定语从句", "B2", CW),
    E("五.10", "介词 + 动名词", "介词后动词一律用 -ing 形", "B1", CW),
    E("五.11", "易混介词小结", "in/on/at、to/for、of/with 等易混介词对比", "B1", CW),
],
"adjective": [
    E("一.1", "形容词作定语的位置", "规则讲解：单个形容词前置、短语/从句后置", "A2", KG),
    E("一.2", "多个形容词的排列顺序", "规则讲解：限定-大小-形状-年龄-颜色-来源-材料", "B1", KG),
    E("二.1", "作表语的形容词", "规则讲解：系动词后接形容词作表语", "A2", KG),
    E("二.2", "以 -ing/-ed 结尾的形容词", "规则讲解：-ing 表性质、-ed 表感受", "B1", KG),
    E("三.1", "原级比较 as…as", "规则讲解：as + 原级 + as 表同级比较", "A2", KG),
    E("三.2", "比较级的构成", "规则讲解：单音节 -er；多音节 more + 原级", "A2", KG),
    E("三.3", "最高级的构成", "规则讲解：单音节 the -est；多音节 the most", "A2", KG),
    E("四.1", "比较级的修饰语", "规则讲解：much/far/a little + 比较级", "B1", KG),
    E("四.2", "最高级的范围表达", "规则讲解：the + 最高级 + in/of 范围", "A2", KG),
    E("四.3", "双音节形容词的比较级", "规则讲解：双音节 -er 与 more 两可的情形", "B1", KG,
      ["This film is more interesting than that one. 这部电影比那部有趣。"]),
    E("五.1", "易错形容词小结", "规则讲解：比较对象一致、避免双重比较", "B1", KG),
    E("五.2", "形容词与副词混淆", "规则讲解：系动词用形容词、实义动词用副词", "B1", KG),
    E("六.1", "形容词作宾语补足语", "规则讲解：make/find + 宾语 + 形容词", "B1", KG),
    E("六.2", "形容词短语作后置定语", "规则讲解：interested in 等短语后置", "B1", KG),
    E("六.3", "the + 形容词表一类人", "规则讲解：the rich/the young 谓语用复数", "B1", KG),
    E("六.4", "only/own 等形容词的位置", "规则讲解：限定词与形容词的顺序", "B2", KG),
    E("六.5", "以 a- 开头的形容词", "规则讲解：afraid/alone 等只作表语", "B1", KG),
    E("六.6", "分词形容词", "规则讲解：interesting/tired 类分词形容词", "B1", KG),
    E("七.1", "double comparatives", "规则讲解：the more…the more 结构", "B2", KG),
    E("七.2", "比较对象一致性", "规则讲解：比较项须同类，避免错配", "B1", KG),
    E("七.3", "形容词与名词搭配", "规则讲解：good/bad at 等形容词介词搭配", "B1", CW),
    E("七.4", "形容词易错补充", "规则讲解：alive/lonely 等位置受限形容词", "B1", KG),
],
"adverb": [
    E("一.1", "方式副词", "规则讲解：-ly 方式副词与不规则形式", "A2", KG),
    E("一.2", "副词修饰动词的位置", "规则讲解：方式/地点/时间副词的顺序", "B1", KG),
    E("二.1", "频度副词与位置", "规则讲解：always/often/seldom 在实义动词前 be 后", "A2", KG),
    E("二.2", "程度副词", "规则讲解：very/too/rather 修饰形容词副词", "B1", KG),
    E("二.3", "时间副词与完成时", "规则讲解：already/yet/since/for 与完成时搭配", "B1", KG),
    E("二.4", "副词的比较级与最高级", "规则讲解：-ly 副词用 more/most", "A2", KG),
    E("二.5", "程度副词与形容词搭配", "规则讲解：so/such 与 very 的区分", "B1", KG),
    E("二.6", "地点方向副词", "规则讲解：here/there/away 等副词用法", "B1", KG),
    E("二.7", "连接性副词", "规则讲解：however/therefore 的句间连接", "B2", KG),
    E("二.8", "时间顺序副词", "规则讲解：first/then/finally 的篇章作用", "B1", KG),
    E("二.9", "评注副词", "规则讲解：perhaps/luckily 表说话人态度", "B2", KG),
    E("二.10", "形容词与副词同形", "规则讲解：early/late/fast/hard 形副同形", "B1", KG,
      ["He arrived late. 他迟到了。"]),
    E("二.11", "易混副词 -ly", "规则讲解：hard/hardly、late/lately 辨析", "B1", KG),
    E("三.1", "易错副词小结", "规则讲解：双重比较与位置错位高频错", "B1", KG),
],
"noun": [
    E("一.1", "规则复数变化", "规则讲解：-s/-es/-y→-ies 规则变化", "A2", KG),
    E("一.2", "不规则复数", "规则讲解：man/men、child/children 等不规则", "A2", KG),
    E("一.3", "单复数同形", "规则讲解：sheep/deer/fish 单复数同形", "A2", KG),
    E("二.1", "不可数名词", "规则讲解：不可数名词不加 a、不用复数", "A2", KG),
    E("二.2", "量词与不可数名词", "规则讲解：a piece of 等量词结构", "A2", KG),
    E("二.3", "主谓一致概说", "规则讲解：主语数决定谓语数（详见一致关系讲）", "B1", KG),
    E("三.1", "-'s 所有格", "规则讲解：有名词用 -'s，复数词尾 s 只加撇", "A2", KG),
    E("三.2", "of 所有格", "规则讲解：无名词用 of 结构", "A2", KG),
    E("三.3", "双重所有格", "规则讲解：a friend of mine 双重所有格", "B1", KG),
    E("三.4", "名词作定语", "规则讲解：名词定语通常用单数（例外：sports）", "B1", KG),
    E("三.5", "集体名词", "规则讲解：集体名词整体单数/成员复数两可", "B1", KG),
    E("三.6", "可数不可数两可的名词", "规则讲解：chicken/room 等词义不同数不同", "A2", KG),
    E("三.7", "抽象名词", "规则讲解：抽象名词一般不可数", "B1", KG),
    E("三.8", "易错名词小结", "规则讲解：popular mistakes——informations/advices", "A2", KG),
    E("四.1", "材料名词与物质名词", "规则讲解：wood/glass 物质名词不可数", "A2", KG),
    E("四.2", "复合名词的复数", "规则讲解：boyfriends/passers-by 复数变化", "B1", KG),
],
"verb": [
    E("一.1", "及物与不及物", "规则讲解：及物动词直接带宾语、不及物需介词", "B1", KG),
    E("一.2", "易混及物动词", "规则讲解：discuss/enter/answer 不打介词", "B1", KG),
    E("二.1", "动词 + to do 句型", "规则讲解：want/decide/hope 后接不定式", "B1", KG),
    E("二.2", "动词 + doing 句型", "规则讲解：enjoy/finish/mind 后接动名词", "B1", KG),
    E("二.3", "接两种形式意义不同", "规则讲解：remember/stop/try 接 to do 与 doing 之别", "B2", KG),
    E("三.1", "系动词 + 形容词", "规则讲解：look/taste/feel 后接形容词", "A2", KG),
    E("三.2", "助动词 do/does/did", "规则讲解：疑问与否定中的助动词", "A2", HS,
      [],
      [("He don't like tea.", "He doesn't like tea."),
       ("Do he like music?", "Does he like music?")]),
    E("三.3", "情态动词后接原形", "规则讲解：can/must/should 后接动词原形", "A2", HS,
      [],
      [("She can sings.", "She can sing."),
       ("He must goes now.", "He must go now.")]),
    E("四.1", "there be 句型", "规则讲解：there be 的就近一致", "A2", KG),
    E("四.2", "易错动词小结", "规则讲解：及物性误加介词高频错", "B1", KG),
],
"tense": [
    E("一.1", "时态系统总览", "规则讲解：常见十二时态的时间轴定位", "B1", KG),
    E("一.2", "一般现在时构成", "规则讲解：三单 -s 与动词变化", "A2", KG),
    E("一.3", "一般现在时用法", "规则讲解：习惯、事实与时刻表", "A2", KG),
    E("二.1", "现在进行时", "规则讲解：be + doing 表正在进行", "A2", KG),
    E("三.1", "一般将来时构成", "规则讲解：will / be going to", "A2", KG),
    E("三.2", "一般将来时用法", "规则讲解：意愿、预测与计划的区别", "A2", KG),
    E("四.1", "一般过去时构成", "规则讲解：规则 -ed 与不规则过去式", "B1", KG,
      ["Yesterday I visit my aunt. 昨天我看望了姑姑。（应为 visited）",
       "He eated the whole cake. 他把整个蛋糕吃了。（应为 ate）"]),
    E("四.2", "一般过去时用法", "规则讲解：明确过去时间与叙述链", "B1", KG,
      [],
      [("I lose my key last Friday.", "I lost my key last Friday."),
       ("They was at home yesterday.", "They were at home yesterday.")]),
    E("五.1", "过去进行时", "规则讲解：was/were + doing 表过去进行", "B1", KG),
    E("六.1", "现在完成时构成", "规则讲解：have/has + 过去分词", "B1", KG),
    E("六.2", "现在完成时用法", "规则讲解：经历、结果与持续（since/for）", "B2", KG),
    E("六.3", "完成时与过去时对比", "规则讲解：有无明确过去时间的选择", "B2", KG),
    E("七.1", "过去完成时", "规则讲解：had done 表过去的过去", "B2", KG),
    E("七.2", "将来完成与完成进行", "规则讲解：will have done 等扩展时态", "C1", KG),
    E("八.1", "时态呼应", "规则讲解：主句过去时从句时态后退", "B2", KG,
      ["He said that he will come. 他说过他会来。（主句过去时，从句应后退为 would）"]),
    E("八.2", "易错时态小结", "规则讲解：时间状语与从句中的时态选择", "B1", KG),
],
"passive": [
    E("一.1", "被动语态的构成", "规则讲解：be + 过去分词及时态变化", "B1", KG),
    E("一.2", "何时使用被动语态", "规则讲解：施动者未知/不必指明/受事优先", "B1", KG),
    E("二.1", "情态动词的被动", "规则讲解：情态动词 + be done", "B1", KG),
    E("二.2", "by 短语与施动者", "规则讲解：by 引出施动者及省略原则", "B1", KG),
    E("三.1", "易错被动小结", "规则讲解：及物性误用与 be done 形错位", "B2", KG),
],
"infinitive": [
    E("一.1", "不定式的形式与否定", "规则讲解：(not) to do；完成/进行形式", "B1", KG),
    E("一.2", "不定式作宾语", "规则讲解：want/decide/hope 等后接 to do", "B1", KG,
      ["We look forward to see you. 我们期待见到你。（to 后应接 seeing）"]),
    E("二.1", "不定式作目的状语", "规则讲解：in order to / so as to", "B1", KG),
    E("二.2", "不定式作宾语补足语", "规则讲解：ask/tell sb (not) to do", "B1", KG),
    E("三.1", "It is + 形容词 + to do", "规则讲解：形式主语 it 结构", "B1", KG),
    E("三.2", "疑问词 + 不定式", "规则讲解：what to do / how to do it", "B1", KG),
    E("四.1", "使役与感官动词", "规则讲解：make/let sb do 省 to", "B2", KG),
    E("四.2", "不定式作定语与结果", "规则讲解：the first to arrive；only to find", "B2", KG),
    E("五.1", "易错不定式小结", "规则讲解：to 后接原形与介词 to 的区分", "B2", KG),
],
"gerund": [
    E("一.1", "动名词的形式与否定", "规则讲解：(not) doing；完成式 having done", "B1", KG),
    E("一.2", "动名词作动词宾语", "规则讲解：enjoy/finish/mind 等后接 doing", "B1", KG,
      ["She enjoys to read. 她喜欢读书。（应接 reading）",
       "Please avoid to make the same mistake. 请避免犯同样的错误。（应接 making）"]),
    E("二.1", "动名词作主语", "规则讲解：动名词主语谓语用单数", "B1", KG),
    E("二.2", "介词后接动名词", "规则讲解：介词后动词一律 -ing", "B1", KG),
    E("三.1", "固定结构的动名词", "规则讲解：be worth doing / can't help doing", "B2", KG),
    E("三.2", "动名词与不定式对比", "规则讲解：stop/remember/try 两侧意义差别", "B2", KG),
    E("四.1", "易错动名词小结", "规则讲解：动名词与现在分词形近辨析", "B2", KG),
],
"participle": [
    E("一.1", "现在分词", "规则讲解：doing 表主动/进行", "B1", KG),
    E("一.2", "过去分词", "规则讲解：done 表被动/完成", "B1", KG),
    E("二.1", "分词作定语", "规则讲解：单个前置、短语后置的位置", "B2", KG),
    E("二.2", "分词作状语", "规则讲解：分词逻辑主语须与主句主语一致", "B2", KG),
    E("三.1", "分词作宾补", "规则讲解：see/hear/find + 宾语 + 分词", "B2", KG),
    E("三.2", "独立分词结构", "规则讲解：时间/条件分词独立结构", "C1", KG),
    E("四.1", "易错分词小结", "规则讲解：垂悬分词高频错", "B2", KG),
],
"subjunctive": [
    E("一.1", "wish 后的宾语从句", "规则讲解：wish + 过去/过去完成/would", "B2", KG,
      ["I wish I can fly. 但愿我会飞。（应为 could）"]),
    E("一.2", "非真实条件句", "规则讲解：if 从句与主句的时态后退", "B2", KG,
      [],
      [("If I am you, I would wait.", "If I were you, I would wait.")]),
    E("二.1", "as if / as though", "规则讲解：好像虚拟的时态后退", "C1", KG),
    E("二.2", "would rather 与 it's time", "规则讲解：后接过去时表虚拟", "C1", KG),
    E("三.1", "建议要求类从句", "规则讲解：suggest/insist 后 (should) do", "B2", KG),
    E("三.2", "易错虚拟小结", "规则讲解：真实与虚拟条件的混用", "B2", KG),
    E("四.1", "含蓄条件与介词短语", "规则讲解：without/but for 含蓄虚拟", "C1", KG),
    E("四.2", "虚拟语气时态混用", "规则讲解：混合条件句的时间对应", "C1", KG),
],
"sentence_structure": [
    E("一.1", "五种基本句型", "规则讲解：SV/SVO/SVC/SVOO/SVOC", "A2", KG),
    E("一.2", "简单句", "规则讲解：一个主谓结构的边界", "A2", KG),
    E("二.1", "并列句与并列连词", "规则讲解：and/but/or/so 的连接", "B1", KG),
    E("二.2", "连写句与残缺句", "规则讲解：逗号粘连与缺主语高频错", "B1", KG,
      [],
      [("I like summer, my sister likes winter.", "I like summer, but my sister likes winter.")]),
    E("三.1", "存在句与引导句", "规则讲解：there be 与 it 引导", "A2", KG),
    E("三.2", "平衡结构", "规则讲解：并列成分形式对称", "B1", KG),
    E("四.1", "易错结构小结", "规则讲解：句子成分残缺与多余", "B1", KG),
],
"noun_clause": [
    E("一.1", "主语从句", "规则讲解：what/that 引导的主语从句", "B2", KG),
    E("一.2", "宾语从句与 that 省略", "规则讲解：that 可省、语序用陈述序", "B2", KG,
      [],
      [("I think that he are right.", "I think that he is right.")]),
    E("二.1", "宾语从句的时态呼应", "规则讲解：主句过去时从句后退", "B2", KG),
    E("二.2", "疑问词引导的宾语从句", "规则讲解：从句用陈述语序", "B1", KG),
    E("三.1", "表语从句", "规则讲解：be 后 that/what 从句", "C1", KG),
    E("三.2", "同位语从句", "规则讲解：fact/idea 后 that 从句", "C1", KG),
    E("四.1", "易错名词性从句小结", "规则讲解：语序错位与连接词缺失", "B2", KG),
],
"attributive_clause": [
    E("一.1", "限制性定语从句与关系词", "规则讲解：who/which/that 的选择", "B1", KG,
      [],
      [("This is the book what I bought.", "This is the book that I bought.")]),
    E("一.2", "that 与 which 的选择", "规则讲解：只能用 that 的情形", "B2", KG),
    E("二.1", "关系词的省略", "规则讲解：作宾语的关系词可省", "B2", KG),
    E("二.2", "介词 + 关系代词", "规则讲解：preposition + whom/which", "C1", KG),
    E("三.1", "非限制性定语从句", "规则讲解：逗号后 which/who，不用 that", "B2", KG),
    E("三.2", "whose 与 where/when", "规则讲解：所有格与关系副词", "B2", KG),
    E("四.1", "易错定语从句小结", "规则讲解：关系词与介词多余/缺失", "B2", KG),
],
"adverbial_clause": [
    E("一.1", "时间状语从句", "规则讲解：when/while/before/after/until", "B1", KG),
    E("一.2", "时间从句的时态", "规则讲解：主将从现（when 从句用现在时）", "B1", KG),
    E("二.1", "条件状语从句", "规则讲解：if/unless 与主将从现", "B1", KG),
    E("二.2", "让步状语从句", "规则讲解：although/though，不与 but 连用", "B2", KG,
      [],
      [("Although he is young, but he is brave.", "Although he is young, he is brave.")]),
    E("三.1", "原因状语从句", "规则讲解：because/since/as 的语用差别", "B1", KG),
    E("三.2", "结果与目的状语从句", "规则讲解：so…that / so that", "B2", KG),
    E("四.1", "比较状语从句", "规则讲解：than/as…as 从句", "B2", KG),
    E("四.2", "易错状语从句小结", "规则讲解：连词双用与逗号规则", "B2", KG),
],
"agreement": [
    E("一.1", "集体名词作主语", "规则讲解：集体名词指整体用单数、指成员用复数", "B1", KG,
      ["The team is in good shape. 队伍状态良好。",
       "The team are arguing about their plans. 队员们正在争论他们的方案。"]),
    E("一.2", "主谓一致：就近原则", "规则讲解：or/neither…nor 就近取数", "B1", KG),
    E("二.1", "不定代词作主语", "规则讲解：each/everybody 用单数谓语", "B1", KG),
    E("二.2", "数量短语作主语", "规则讲解：时间/金钱/距离整体用单数", "B1", KG),
    E("三.1", "A with B 就远原则", "规则讲解：with/as well as 不影响主语数", "B2", KG),
    E("三.2", "the + 形容词作主语", "规则讲解：the young 等复数含义", "B2", KG),
    E("四.1", "从句与一致", "规则讲解：从句主语的数决定从句谓语", "B1", KG),
    E("四.2", "易错一致小结", "规则讲解：邻近干扰导致的一致性高频错", "B1", KG),
],
"inversion": [
    E("一.1", "否定副词置句首的部分倒装", "规则讲解：never/hardly/seldom + 助动词提前", "C1", KG),
    E("一.2", "only + 状语置句首", "规则讲解：only 短语前置引发部分倒装", "C1", KG),
    E("二.1", "there be 结构", "规则讲解：存在句的真正主语在 be 后", "A2", KG),
    E("二.2", "so/neither 简答倒装", "规则讲解：So do I / Neither do you", "B1", KG),
    E("三.1", "易错倒装小结", "规则讲解：倒装后动词形错位", "C1", KG),
],
"tag_question": [
    E("一.1", "反意疑问句基本构成", "规则讲解：前肯后否、前否后肯", "B1", KG,
      ["You've finished your homework, haven't you? 你完成作业了，是吗？"]),
    E("一.2", "特殊情形", "规则讲解：I am…aren't I / let's…shall we", "B2", KG),
    E("二.1", "易错反问小结", "规则讲解：助动词与时态镜像原则", "B2", KG),
],
"ellipsis": [
    E("一.1", "对话中的省略", "规则讲解：答句省略重复成分", "B1", KG,
      ["Would you like some tea? — Yes, please. 省略重复成分"]),
    E("一.2", "不定式的省略", "规则讲解：I'd love to 后省动词原形", "B2", KG),
    E("二.1", "并列句中的省略", "规则讲解：并列分句省相同主语/宾语", "B2", KG),
    E("二.2", "易错省略小结", "规则讲解：省略过度导致句意残缺", "B2", KG),
],
"emphasis": [
    E("一.1", "强调句型 It is…that/who", "规则讲解：被强调成分与 that/who 选择", "B2", KG,
      ["It was Tom who broke the window. 是汤姆打破了窗户。"]),
    E("一.2", "do/does/did 强调谓语", "规则讲解：助动词 do 表强调", "B1", KG),
    E("二.1", "前置与对比强调", "规则讲解：话题前置与重音对比", "B2", KG),
    E("二.2", "易错强调小结", "规则讲解：强调句与定语从句的区分", "B2", KG),
],
"negation": [
    E("一.1", "not 与 no", "规则讲解：not 修饰动词/形容词，no 修饰名词", "B1", KG),
    E("一.2", "部分否定与全部否定", "规则讲解：not…both/all 表部分否定", "B1", KG,
      ["I don't know both of them. 我并非两个都认识。（部分否定）"]),
    E("二.1", "双重否定表肯定", "规则讲解：don't…nothing 属误用", "B1", KG,
      [],
      [("I don't know nothing about it.", "I don't know anything about it.")]),
    E("二.2", "neither/nor 呼应", "规则讲解：否定句用 either，neither 自带否定", "B1", KG),
    E("三.1", "否定转移与简答", "规则讲解：think/believe 的否定前移", "B2", KG),
    E("三.2", "易错否定小结", "规则讲解：多重否定与反问叠加的语义", "B1", KG),
],
"rhetoric": [
    E("一.1", "排比", "规则讲解：并列结构形式对称增强气势", "C1", KG,
      ["She came in, sat down, and opened the book. 她进来、坐下、翻开书。"]),
    E("一.2", "对比", "规则讲解：对照结构突出差异", "C1", KG),
    E("二.1", "修辞问句", "规则讲解：无疑而问加强语气", "C1", KG),
    E("二.2", "易错修辞小结", "规则讲解：结构失衡破坏排比", "C1", KG),
],
}

# 【改正错误】映射（示例）：wrong/right → 知识点，合并进点的 corrections
# （与点级自带改错对去重合并；对应旧版 en_point_index 的角色）。
SAMPLE_INDEX = {
    "article": [
        {"wrong": "a ancient source", "right": "an ancient source", "point": "article.3"},
    ],
    "preposition": [
        {"wrong": "by", "right": "in", "point": "preposition.24"},
    ],
}

# ─────────────────────────────────────────────────────────────────────────────
# 知识库：知识源注册表 + 知识点集合（与决策表分离）
#   ① sources.json        知识源注册表（仅元数据：标题/作者/版次/许可范围）
#   ② points/{lang}.json  知识点（语言内稳定 ID；内容 = 讲解/例句/改错对）
#   ③ 点 → 源关联（1:N）  同一知识点可被多个知识源背书；新增源只需追加关联，
#                          点 ID 与决策表规则均不变
# ─────────────────────────────────────────────────────────────────────────────
import os as _os

KB_DIR = _os.path.join(_os.path.dirname(_os.path.abspath(__file__)), "..", "knowledge-base")

# ① 知识源注册表：内置示例源（自撰内容，随仓库以 Apache-2.0 分发）
KB_SOURCES = [
    {
        "source_id": "src-sample-en",
        "title": "LangFixer 示例英语语法指南",
        "author": "LangFixer Project",
        "edition": "第一版",
        "lang": "en",
        "license_scope": "public",  # 自撰示例内容，可随仓库分发
    },
]



def build_knowledge_base():
    """生成 knowledge-base/sources.json + knowledge-base/points/{lang}.json。"""
    _os.makedirs(_os.path.join(KB_DIR, "points"), exist_ok=True)
    with open(_os.path.join(KB_DIR, "sources.json"), "w", encoding="utf-8") as f:
        json.dump(KB_SOURCES, f, ensure_ascii=False, indent=2)
    src = KB_SOURCES[0]
    unit_titles = {etype: (i + 1) for i, (etype, _c, _t, _a) in enumerate(EN_ERROR_TYPES)}
    all_points = []
    for etype, fcefr, frem, fattr in EN_ERROR_TYPES:
        unit = unit_titles[etype]
        for i, p in enumerate(SAMPLE_POINTS.get(etype, []), 1):
            merged = list(p["corrections"])
            seen_pairs = {(c["wrong"], c["right"]) for c in merged}
            for c in SAMPLE_INDEX.get(etype, []):
                if c.get("point") != f"{etype}.{i}":
                    continue
                pair = (c["wrong"], c["right"])
                if pair not in seen_pairs:
                    merged.append({"wrong": c["wrong"], "right": c["right"]})
                    seen_pairs.add(pair)
            point = {
                "point_id": f"{etype}.{i}",
                "lang": "en",
                "error_type": etype,
                "section": p["section"],
                "title": p["title"],
                "cefr": p["cefr"],
                "remedial": p["remedial"],
                # ③ 点 → 源关联（location = 源内位置）
                "sources": [
                    {
                        "source_id": src["source_id"],
                        "location": f"Unit {unit:02d} · {p['section']} {p['title']}",
                    }
                ],
            }
            if p["examples"]:
                point["examples"] = list(p["examples"])
            if merged:
                point["corrections"] = merged
            all_points.append(point)
    with open(_os.path.join(KB_DIR, "points", "en.json"), "w", encoding="utf-8") as f:
        json.dump(all_points, f, ensure_ascii=False, indent=2)
    print(f"knowledge-base written: {len(KB_SOURCES)} sources, {len(all_points)} points")

def build_aux_files():
    """生成 aux 决策数据（资源包组件：随源导出/导入/删除）：
    - en_points.json      知识点原始提取数据（条目带 source_id 归属标记）
    - en_point_index.json 改错对 → 知识点映射（条目带 source_id 归属标记）
    """
    here = _os.path.dirname(_os.path.abspath(__file__))
    src_id = KB_SOURCES[0]["source_id"]
    aux_points = {}
    for etype, entries in SAMPLE_POINTS.items():
        aux_points[etype] = [
            {"section": p["section"], "title": p["title"], "remedial": p["remedial"],
             "cefr": p["cefr"], "attr": p["attr"], "examples": p["examples"],
             "source_id": src_id}
            for p in entries
        ]
    with open(_os.path.join(here, "en_points.json"), "w", encoding="utf-8") as f:
        json.dump(aux_points, f, ensure_ascii=False, indent=2)
    aux_index = {}
    for etype, entries in SAMPLE_INDEX.items():
        aux_index[etype] = [
            {"q": i, "wrong": e["wrong"], "right": e["right"], "point": e["point"],
             "source_id": src_id}
            for i, e in enumerate(entries, 1)
        ]
    with open(_os.path.join(here, "en_point_index.json"), "w", encoding="utf-8") as f:
        json.dump(aux_index, f, ensure_ascii=False, indent=2)
    print("aux written:", sum(len(v) for v in aux_points.values()), "entries,",
          sum(len(v) for v in aux_index.values()), "index entries")

def _en_rule(rid, conds, attr, remedial, cefr):
    """英文表规则：weiner 由归因推导（与 zh/ja 表同一 `rule()` 结构）。
    示例源派生的规则行标记 _meta = 内置示例源 source_id（资源包按源导出/清理）。"""
    locus, stability, ctrl = WEINER[attr]
    return rule(rid, conds, attr, locus, stability, ctrl, remedial, cefr=cefr,
                source_id=KB_SOURCES[0]["source_id"])

def _legacy_en_fallback():
    """5 类 legacy（article/agreement/tense/preposition/morphology）的兜底多规则，
    保留既有 novice/old-hand 行为；仅含通用规则讲解，不含知识源内容。"""
    return {
        "article": [
            _en_rule("article_novice", {"errorType": '== "article"', "isNovice": "== true"},
                     RM, "规则讲解：a/an/the 冠词用法", "A2"),
            _en_rule("article_old_hand",
                     {"errorType": '== "article"', "confidence": ">= 0.8", "isNovice": "== false"},
                     HS, "规则讲解：冠词用法回顾", "A2"),
            _en_rule("article_any", {"errorType": '== "article"'},
                     KG, "规则讲解：冠词基础", "A2"),
        ],
        "agreement": [
            _en_rule("agreement_novice", {"errorType": '== "agreement"', "isNovice": "== true"},
                     RM, "规则讲解：第三人称单数 -s", "A2"),
            _en_rule("agreement_old_hand", {"errorType": '== "agreement"', "isNovice": "== false"},
                     RF, "规则讲解：主谓一致回顾", "A2"),
        ],
        "tense": [
            _en_rule("tense_novice", {"errorType": '== "tense"', "isNovice": "== true"},
                     KG, "规则讲解：一般过去时构成", "B1"),
            _en_rule("tense_old_hand", {"errorType": '== "tense"', "isNovice": "== false"},
                     RF, "规则讲解：时态系统回顾", "B1"),
        ],
        "preposition": [
            _en_rule("preposition_any", {"errorType": '== "preposition"'},
                     CW, "搭配专项训练：介词搭配强化", "B2"),
        ],
        "morphology": [
            _en_rule("morphology_any", {"errorType": '== "morphology"'},
                     KG, "规则讲解：名词/形容词/副词词形变化", "B2"),
        ],
    }

def build_en_rules():
    """英文表：5 类 legacy 兜底多规则 + 其余各错误类型 1 条通用规则 + 全局 default。
    三张表（zh/en/ja）结构同构（4 输入 + 7 输出），只含可提取的规则；
    知识点详情由知识库在运行时富化（见 build_knowledge_base）。"""
    legacy = _legacy_en_fallback()
    rules = []
    for etype, fcefr, frem, fattr in EN_ERROR_TYPES:
        if etype in legacy:
            rules.extend(legacy[etype])
        else:
            rules.append(_en_rule(f"{etype}_any", {"errorType": f'== "{etype}"'},
                                 fattr, frem, fcefr))
    rules.extend(legacy["morphology"])
    rules.append(rule("default_knowledge_gap", {}, KG, I, U, C,
                     "建议结合规则讲解进行针对性练习", cefr="A1"))
    return rules

en_rules = build_en_rules()

ja_rules = [
    rule("particle_novice", {"errorType": '== "particle"', "isNovice": "== true"},
         "rule_missing", I, U, C, "规则讲解：は/を/に/で 助词用法", cefr="A2"),
    rule("particle_old_hand", {"errorType": '== "particle"', "isNovice": "== false"},
         "habit_solidified", I, S, C, "规则讲解：助词用法回顾", cefr="A2"),
    rule("conjugation_novice", {"errorType": '== "conjugation"', "isNovice": "== true"},
         "knowledge_gap", I, U, C, "规则讲解：动词五段/一段变形", cefr="A2"),
    rule("conjugation_old_hand", {"errorType": '== "conjugation"', "isNovice": "== false"},
         "rule_forgotten", I, S, C, "规则讲解：变形规则回顾", cefr="A2"),
    rule("honorific_any", {"errorType": '== "honorific"'},
         "collocation_weak", I, S, C, "搭配专项训练：敬语体系一致性", cefr="B1"),
    rule("word_order_any", {"errorType": '== "word_order"'},
         "knowledge_gap", I, U, C, "规则讲解：日语 SOV 语序", cefr="B1"),
    rule("default_knowledge_gap", {}, "knowledge_gap", I, U, C, cefr="A1"),
]

for name, rules in [
    ("zh_gec_attribution", zh_rules),
    ("en_gec_attribution", en_rules),
    ("ja_gec_attribution", ja_rules),
]:
    with open(f"{name}.json", "w", encoding="utf-8") as f:
        json.dump(table(name, rules), f, ensure_ascii=False, indent=2)
    print(name, "written,", len(rules), "rules")

build_knowledge_base()
build_aux_files()


