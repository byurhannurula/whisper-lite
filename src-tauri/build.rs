fn main() {
    link_clang_runtime();
    tauri_build::build()
}

/// ggml's Metal backend uses `@available(macOS ..)`, which compiles to a call to
/// `__isPlatformVersionAtLeast` in clang's compiler runtime. rustc links with `-nodefaultlibs`, so
/// that runtime is left out and the release link fails with Apple clang 21 (Xcode 26). Linking
/// `libclang_rt.osx.a` explicitly restores it; older toolchains that resolved the symbol another
/// way are unaffected by the extra archive.
fn link_clang_runtime() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let Ok(out) = std::process::Command::new("xcrun")
        .args(["clang", "--print-resource-dir"])
        .output()
    else {
        return;
    };
    let dir = std::path::PathBuf::from(String::from_utf8_lossy(&out.stdout).trim())
        .join("lib")
        .join("darwin");
    if dir.join("libclang_rt.osx.a").exists() {
        println!("cargo:rustc-link-search=native={}", dir.display());
        println!("cargo:rustc-link-lib=static=clang_rt.osx");
    }
}
