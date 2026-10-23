//! 回归测试：用一批"正确英语句子"喂给检测器，任何输出都算误报。
//!
//! 保护 [`gec_en::rules`] 各检测规则（尤其扩展的 9 类）不把正确英语标错：
//! - `verb`：rely on / consist of / participate in / regard as 等正确固定搭配
//! - `adverb`：系动词（look/feel/seem）+ 形容词表语
//! - `gerund`：keep to / look forward to 等 "to+冠词/介词" 短语
//! - `pronoun` / `subjunctive` / `negation`：非反身、真实条件、单重否定
use gec_en::rules::{Lexicon, detect_local};

#[test]
fn review_false_positives() {
    let lex = Lexicon::default();
    // 这些句子都是正确英语，不应产生任何错误
    let clean_sentences = [
        // verb：正确搭配 / 及物动词正确用法
        "We rely on you.",
        "It consists of three parts.",
        "She participates in sports.",
        "We regard it as important.",
        "We discussed the plan.",
        // adverb：系动词 + 形容词表语（正确），含屈折形式 looks/feels/seems
        "You look quiet.",
        "He looks quiet.",
        "She feels eager today.",
        "He seems gentle.",
        // gerund / infinitive：正确用法
        "I enjoy playing piano.",
        "I want to go home.",
        "I keep to the rules.",
        "I look forward to the trip.",
        // pronoun：不同人称，非反身
        "He saw me at the store.",
        "I told her the news.",
        // subjunctive：真实条件（无 was）
        "If it rains tomorrow, we will stay home.",
        // negation：单重否定 / 无否定
        "I don't have a car.",
        "I know nothing about it.",
        "She can't swim.",
    ];
    let mut total = 0;
    for s in clean_sentences {
        let errs = detect_local(s, &lex);
        let bad: Vec<String> = errs
            .iter()
            .map(|e| {
                format!(
                    "{}: '{}' -> {:?} ({})",
                    e.error_type, e.original, e.correction, e.message
                )
            })
            .collect();
        if !bad.is_empty() {
            total += bad.len();
            eprintln!("误报 [{}]:", s);
            for b in &bad {
                eprintln!("    {}", b);
            }
        }
    }
    eprintln!("\n总计误报: {}", total);
    assert!(total == 0, "存在 {} 处误报（见上）", total);
}
