//! lexicon-store CLI：词表 JSON ⇄ SQLite 库 的导入 / 导出 / 查询
//!
//! ```text
//! lexicon-store import --db langfixer-lexicon.db --dir lexicons
//! lexicon-store export --db langfixer-lexicon.db --lang en --out en_gec_lexicon.json
//! lexicon-store list   --db langfixer-lexicon.db --lang en [--q 搜索词] [--category 分类] [--page 1] [--page-size 50]
//! ```

use std::path::Path;

use lexicon_store::LexiconStore;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("用法: lexicon-store <import|export|list> --db <路径> --lang <语言> [选项]");
        std::process::exit(2);
    }
    let cmd = args[1].as_str();
    let mut db: Option<String> = None;
    let mut lang: Option<String> = None;
    let mut dir: Option<String> = None;
    let mut out: Option<String> = None;
    let mut q: Option<String> = None;
    let mut category: Option<String> = None;
    let mut page: u32 = 1;
    let mut page_size: u32 = 50;
    let mut i = 2;
    while i < args.len() {
        let a = args[i].clone();
        // 取下一个参数值并推进索引；返回 (值, 新索引)。
        // 注意：不能把闭包定义在循环体内——闭包会捕获外层 i，
        // 而闭包体内的参数 i 又遮蔽它，导致 *i += 1 作用不到外层 i，
        // 索引永不前进 → 死循环（反复打印“忽略未知参数”）。
        let take_next = |i: usize| -> (Option<String>, usize) { (args.get(i).cloned(), i + 1) };
        match a.as_str() {
            "--db" => {
                let (v, ni) = take_next(i);
                i = ni;
                let Some(v) = v else {
                    eprintln!("--db 缺少参数值");
                    std::process::exit(2)
                };
                db = Some(v);
            }
            "--lang" => {
                let (v, ni) = take_next(i);
                i = ni;
                let Some(v) = v else {
                    eprintln!("--lang 缺少参数值");
                    std::process::exit(2)
                };
                lang = Some(v);
            }
            "--dir" => {
                let (v, ni) = take_next(i);
                i = ni;
                let Some(v) = v else {
                    eprintln!("--dir 缺少参数值");
                    std::process::exit(2)
                };
                dir = Some(v);
            }
            "--out" => {
                let (v, ni) = take_next(i);
                i = ni;
                let Some(v) = v else {
                    eprintln!("--out 缺少参数值");
                    std::process::exit(2)
                };
                out = Some(v);
            }
            "--q" => {
                let (v, ni) = take_next(i);
                i = ni;
                let Some(v) = v else {
                    eprintln!("--q 缺少参数值");
                    std::process::exit(2)
                };
                q = Some(v);
            }
            "--category" => {
                let (v, ni) = take_next(i);
                i = ni;
                let Some(v) = v else {
                    eprintln!("--category 缺少参数值");
                    std::process::exit(2)
                };
                category = Some(v);
            }
            "--page" => {
                let (v, ni) = take_next(i);
                i = ni;
                let Some(v) = v else {
                    eprintln!("--page 缺少参数值");
                    std::process::exit(2)
                };
                page = v.parse().unwrap_or(1);
            }
            "--page-size" => {
                let (v, ni) = take_next(i);
                i = ni;
                let Some(v) = v else {
                    eprintln!("--page-size 缺少参数值");
                    std::process::exit(2)
                };
                page_size = v.parse().unwrap_or(50);
            }
            _ => eprintln!("忽略未知参数: {a}"),
        }
    }
    match cmd {
        "import" => {
            let (db, dir) = req2(db, dir, "import 需要 --db 与 --dir");
            let store = open_store(Path::new(&db));
            let (ok, failed) = store.seed_from_dir(Path::new(&dir)).unwrap_or_else(|e| {
                eprintln!("导入失败: {e}");
                std::process::exit(1);
            });
            println!(
                "导入完成: {ok} 个语言成功, {failed} 个失败 → {}",
                Path::new(&db).display()
            );
            if failed > 0 {
                std::process::exit(1);
            }
        }
        "export" => {
            let (db, lang, out) = req3(db, lang, out, "export 需要 --db --lang --out");
            let store = open_store(Path::new(&db));
            let v = store.export_lexicon(&lang).unwrap_or_else(|e| {
                eprintln!("导出失败: {e}");
                std::process::exit(1);
            });
            std::fs::write(&out, serde_json::to_string_pretty(&v).unwrap()).unwrap_or_else(|e| {
                eprintln!("写文件失败: {e}");
                std::process::exit(1);
            });
            println!("已导出 {} → {}", lang, Path::new(&out).display());
        }
        "list" => {
            let (db, lang) = req2(db, lang, "list 需要 --db --lang");
            let store = open_store(Path::new(&db));
            let list = store
                .list_rules(
                    &lang,
                    category.as_deref(),
                    q.as_deref(),
                    page as usize,
                    page_size as usize,
                )
                .unwrap_or_else(|e| {
                    eprintln!("查询失败: {e}");
                    std::process::exit(1);
                });
            println!(
                "{} 条规则（第 {} 页，每页 {}）",
                list.total, list.page, list.page_size
            );
            for r in &list.rules {
                println!(
                    "{:<4}  {:<40}  [{:<12}]  示例 {:>6}  {}",
                    if r.def.enabled { "on" } else { "off" },
                    r.def.id,
                    r.def.category,
                    r.example_count,
                    r.def.description
                );
            }
        }
        _ => {
            eprintln!("未知命令: {cmd}");
            std::process::exit(2);
        }
    }
}

fn open_store(db: &Path) -> LexiconStore {
    LexiconStore::open(db, None).unwrap_or_else(|e| {
        eprintln!("打开词表库失败: {e}");
        std::process::exit(1);
    })
}

fn req2<T: std::fmt::Display>(a: Option<T>, b: Option<T>, msg: &str) -> (T, T) {
    match (a, b) {
        (Some(a), Some(b)) => (a, b),
        _ => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
    }
}

fn req3<T: std::fmt::Display>(a: Option<T>, b: Option<T>, c: Option<T>, msg: &str) -> (T, T, T) {
    match (a, b, c) {
        (Some(a), Some(b), Some(c)) => (a, b, c),
        _ => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
    }
}
