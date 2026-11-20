fn main() {
    // 把发布源烧进二进制（github | gitea | atomgit）：安装包从哪个源分发，
    // 运行时就从哪个源自动检查更新（desktop-app/src/update.rs）。
    // 由 scripts/release.sh 按源分别打包时通过环境变量注入，默认 github。
    let source = std::env::var("LANGFIXER_RELEASE_SOURCE").unwrap_or_else(|_| "github".into());
    println!("cargo:rustc-env=LANGFIXER_RELEASE_SOURCE={source}");
    println!("cargo:rerun-if-env-changed=LANGFIXER_RELEASE_SOURCE");
    // 自测钩子（update.rs::endpoints）：烧入自定义更新端点，正式发布不设置
    println!("cargo:rerun-if-env-changed=LANGFIXER_UPDATE_TEST_ENDPOINT");
    tauri_build::build()
}
