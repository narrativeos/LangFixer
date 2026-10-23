//! Tauri 命令契约测试
//!
//! Tauri 按**参数名**绑定前端 invoke 的 args key：Rust 侧是单个 `body`
//! 参数时前端必须传 `{ body: {...} }`，独立参数时必须平铺
//! （`{ id }` / `{ lang, status, limit }`）。两侧风格不匹配会得到
//! `invalid args ... missing required key body`（真实事故：P2/P3 查询
//! 命令曾全部因此失败，UI 显示「作业列表加载失败」）。
//!
//! 本测试解析 `src/commands.rs` 的命令签名参数名，断言与 web-ui
//! `api/client.ts` 的调用风格一致（查询类平铺 / 复杂写操作 body）。

const SRC: &str = include_str!("../src/commands.rs");

/// 提取命令签名的参数名列表（跳过 `core`；兼容单行 / 多行签名）
fn param_names(cmd: &str) -> Vec<String> {
    let pat = format!("pub async fn {cmd}(");
    let start = SRC
        .find(&pat)
        .unwrap_or_else(|| panic!("命令 {cmd} 未在 commands.rs 找到"));
    let rest = &SRC[start + pat.len()..];
    let end = rest
        .find(") -> ")
        .unwrap_or_else(|| panic!("命令 {cmd} 签名无 `) -> ` 结尾"));
    let sig = &rest[..end];
    sig.split(",\n")
        .map(|seg| seg.trim())
        .filter(|seg| !seg.is_empty())
        .filter_map(|seg| {
            let name = seg.split(':').next()?.trim().to_string();
            (name != "core").then_some(name)
        })
        .collect()
}

/// P2 / P3 查询 / 操作命令：前端平铺传参 → Rust 独立参数
#[test]
fn p2_p3_query_commands_use_flat_params() {
    assert_eq!(
        param_names("train_list_jobs"),
        vec!["lang", "status", "limit"]
    );
    assert_eq!(param_names("train_get_job"), vec!["id"]);
    assert_eq!(param_names("round_list"), vec!["lang", "status", "limit"]);
    for c in [
        "round_get",
        "round_confirm",
        "round_reject",
        "round_execute",
        "round_report",
    ] {
        assert_eq!(param_names(c), vec!["id"], "{c}");
    }
}

/// 复杂写操作命令：前端传 `{ body }` → Rust 单个 `body` 参数
#[test]
fn body_style_commands_keep_body_param() {
    for c in [
        "run_eval",
        "run_reflect",
        "train_build_pool",
        "train_build_llm_data",
        "train_run_l3_retrain",
        "train_apply_lexicon",
        "train_run_suggestion",
        "eval_contributions",
        "round_evaluate",
    ] {
        assert_eq!(param_names(c), vec!["body"], "{c}");
    }
}
