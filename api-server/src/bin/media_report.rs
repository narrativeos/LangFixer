//! `media-report`：把「推演候选 + 人工勾选」变成与专家同构的正误表（`MULTIMODAL.md` §18）
//!
//! 三段式重演的第 ③ 段——前两段（跑载体、拿候选）由评测 harness 或后端 job 产出
//! `candidates.json`，人工在这个文件里勾 `checked`、改各列，然后：
//!
//! ```text
//! cargo run -p api-server --bin media-report -- <candidates.json> [选项]
//!
//!   --out DIR          产物目录（默认：输入同级目录下的 report/）
//!   --gold PATH        与 gold 集对齐，打印「重演到什么程度」
//!   --accepted 1,3,5   只导出这些标注 id（覆盖文件里的 checked 位）
//!   --rejected 2,4     显式剔除
//!   --title T          表标题
//!   --no-docx          只出 json / md / html，不调用 wordcraft-cli
//! ```
//!
//! 产物：`report.json`（含溯源）、`report.md`（人读）、`report.html`、`report.docx`
//! （由 `wordcraft-cli convert` 生成，列结构与专家正误表一致）。

use std::path::{Path, PathBuf};

use media_core::{ReportFile, apply_selection, attach_gold, parse_gold_set};
use media_flow::report::{self, WordCraft};

fn usage() {
    eprintln!(
        "用法：media-report <candidates.json> [--out DIR] [--gold PATH] \
         [--accepted 1,3] [--rejected 2] [--title T] [--no-docx]"
    );
}

fn parse_ids(s: &str) -> Result<Vec<u32>, String> {
    s.split(',')
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .map(|p| p.parse::<u32>().map_err(|e| format!("非法 id {p:?}：{e}")))
        .collect()
}

/// 取 `--flag` 后面那个参数（写成函数而不是闭包：闭包会借住 `i`，下面还要改它）
fn value_of(args: &[String], i: usize, flag: &str) -> Result<String, String> {
    args.get(i + 1)
        .cloned()
        .ok_or_else(|| format!("{flag} 缺参数"))
}

fn main() {
    std::process::exit(match run() {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("media-report 失败：{e}");
            1
        }
    });
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut input: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut gold: Option<PathBuf> = None;
    let mut accepted: Option<Vec<u32>> = None;
    let mut rejected: Vec<u32> = Vec::new();
    let mut title: Option<String> = None;
    let mut docx = true;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--out" => {
                out = Some(PathBuf::from(value_of(&args, i, "--out")?));
                i += 2;
            }
            "--gold" => {
                gold = Some(PathBuf::from(value_of(&args, i, "--gold")?));
                i += 2;
            }
            "--accepted" => {
                accepted = Some(parse_ids(&value_of(&args, i, "--accepted")?)?);
                i += 2;
            }
            "--rejected" => {
                rejected = parse_ids(&value_of(&args, i, "--rejected")?)?;
                i += 2;
            }
            "--title" => {
                title = Some(value_of(&args, i, "--title")?);
                i += 2;
            }
            "--no-docx" => {
                docx = false;
                i += 1;
            }
            "-h" | "--help" => {
                usage();
                return Ok(());
            }
            other if other.starts_with('-') && other != "-" => {
                usage();
                return Err(format!("未知参数：{other}"));
            }
            other => {
                input = Some(PathBuf::from(other));
                i += 1;
            }
        }
    }
    let input = match input {
        Some(p) => p,
        None => {
            usage();
            return Err("缺少 candidates.json 路径".to_string());
        }
    };

    let file: ReportFile = report::read_report_file(&input).map_err(|e| e.to_string())?;
    if file.rows.is_empty() {
        return Err(format!("{} 里没有候选行", input.display()));
    }
    let mut rows = apply_selection(&file.rows, accepted.as_deref(), &rejected, &[]);
    if rows.is_empty() {
        return Err(
            "勾选后没有条目（全是 checked=false？用 --accepted 指定要导出的 id）".to_string(),
        );
    }
    let mut meta = file.meta.clone();
    if let Some(t) = title {
        meta.title = t;
    }

    // 与 gold 对齐：给出「重演到什么程度」，并把命中的专家序号回填到行里
    let gold_path = gold.or_else(|| {
        let d = PathBuf::from("datasets/media_gold.json");
        if d.is_file() { Some(d) } else { None }
    });
    if let Some(gp) = &gold_path {
        let raw = std::fs::read_to_string(gp).map_err(|e| format!("读 gold 失败：{e}"))?;
        let set = parse_gold_set(&raw).map_err(|e| format!("解析 gold 失败：{e}"))?;
        let score = attach_gold(&mut rows, &set.items, 0.5);
        println!(
            "重演对照：专家 {} 条（计差错 {}）/ 本次导出 {} 条 / 命中 {} 条 → 加权召回 {:.1}%、精确率 {:.1}%",
            score.gold_total,
            score.gold_counted,
            rows.len(),
            score.matched,
            score.recall.max(0.0) * 100.0,
            score.precision.max(0.0) * 100.0
        );
        for (no, hit, kind, overlap) in &score.per_gold {
            match hit {
                Some(id) => {
                    let row_no = rows.iter().find(|r| r.annotation_id == *id).map(|r| r.no);
                    println!(
                        "  专家 #{no} ← 标注 #{id}{}  {kind:?} 重叠 {overlap:.2}",
                        row_no
                            .map(|n| format!("（表内序号 {n}）"))
                            .unwrap_or_default()
                    );
                }
                None => {
                    let orig = set
                        .items
                        .iter()
                        .find(|g| g.no == *no)
                        .map(|g| g.original.chars().take(24).collect::<String>())
                        .unwrap_or_default();
                    println!("  专家 #{no} ← 未命中（{orig}…）");
                }
            }
        }
        if !score.unmatched_ours.is_empty() {
            println!(
                "  我们多出的条目（专家未列，也可能是真错）：{:?}",
                score.unmatched_ours
            );
        }
    } else {
        println!("（未找到 gold 集，跳过重演对照；用 --gold PATH 指定）");
    }

    let out_dir = out.unwrap_or_else(|| input.parent().unwrap_or(Path::new(".")).join("report"));
    let files = report::write_artifacts(&out_dir, &rows, &meta).map_err(|e| e.to_string())?;
    println!("导出 {} 条 → {}", rows.len(), out_dir.display());
    println!("  JSON     {}", files.json.display());
    println!("  Markdown {}", files.markdown.display());
    println!("  HTML     {}", files.html.display());
    if docx {
        match WordCraft::new(None, 120) {
            Ok(wc) => {
                let target = out_dir.join("report.docx");
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| format!("启动 tokio 失败：{e}"))?;
                match rt.block_on(wc.html_to_docx(&files.html, &target)) {
                    Ok(()) => println!("  Word     {}（wordcraft-cli）", target.display()),
                    Err(e) => println!("  Word     未生成：{e}"),
                }
            }
            Err(e) => println!("  Word     未生成：{e}"),
        }
    }
    Ok(())
}
