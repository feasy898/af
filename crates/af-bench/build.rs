//! fix4-observability（红队④）：构建期烧录 git 溯源标签。
//! 只在运行时 rev-parse HEAD 会把"提交前/脏树构建"的二进制标错 commit——
//! 这里在构建机现算 HEAD 与脏文件数，经 rustc-env 进二进制：
//!   AF_GIT_REV   = git rev-parse HEAD（取不到则 unknown）
//!   AF_GIT_DIRTY = git status --porcelain 行数（"0"=干净）
//! git 不可用（非仓库/离线）时静默降级为 unknown，不阻断构建。

use std::process::Command;

fn main() {
    let manifest = env!("CARGO_MANIFEST_DIR");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rustc-env=AF_GIT_REV={}", git_rev(manifest));
    println!("cargo:rustc-env=AF_GIT_DIRTY={}", git_dirty(manifest));
}

fn git(manifest: &str, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(manifest).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn git_rev(manifest: &str) -> String {
    git(manifest, &["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into())
}

fn git_dirty(manifest: &str) -> String {
    match git(manifest, &["status", "--porcelain"]) {
        Some(text) => text.lines().count().to_string(),
        None => "-1".into(),
    }
}
