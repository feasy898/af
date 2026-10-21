//! fix4-observability（红队④）：构建期烧录 git 溯源标签。
//! 只在运行时 rev-parse HEAD 会把"提交前/脏树构建"的二进制标错 commit——
//! 这里在构建机现算 HEAD 与脏文件数，经 rustc-env 进二进制：
//!   AF_GIT_REV   = git rev-parse HEAD（取不到则 unknown）
//!   AF_GIT_DIRTY = git status --porcelain 行数（"0"=干净；git 不可用为 "-1"）
//! git 不可用（非仓库/离线）时静默降级，不阻断构建。
//!
//! 重跑策略（a3 实证教训）：声明 `rerun-if-changed=build.rs` 后，commit 只移动 ref 文件、
//! 不改任何被监视文件 → 构建脚本不重跑 → 二进制烧到陈旧 rev；"指向恒不存在路径"也无效
//! （cargo 对不存在路径只在它出现时重跑）。故本脚本显式监视：
//!   HEAD（符号引用，checkout/分支切换会动）、当前分支的 ref 文件、packed-refs（提交落盘处）
//!   以及 src/（脏树计数随源码编辑变化）。git 文件布局兼容普通仓库与链接工作树
//!   （.git 为文件 → gitdir → commondir）。

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest = env!("CARGO_MANIFEST_DIR");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src");
    watch_git(manifest);
    println!("cargo:rustc-env=AF_GIT_REV={}", git_rev(manifest));
    println!("cargo:rustc-env=AF_GIT_DIRTY={}", git_dirty(manifest));
}

fn emit_watch(path: &Path) {
    println!("cargo:rerun-if-changed={}", path.display());
}

/// 向上找 .git（目录=普通仓库；文件=链接工作树，内容 "gitdir: <路径>"）。
fn find_git(manifest: &str) -> Option<PathBuf> {
    let mut dir = PathBuf::from(manifest);
    loop {
        let candidate = dir.join(".git");
        if candidate.exists() {
            return Some(candidate);
        }
        dir = dir.parent()?.to_path_buf();
    }
}

fn head_branch(head_path: &Path) -> Option<String> {
    let head = std::fs::read_to_string(head_path).ok()?;
    let branch = head.strip_prefix("ref: ")?.trim().to_string();
    Some(branch)
}

/// 声明监视 git 溯源相关文件：提交/checkout 移动这些文件，构建脚本随之重跑。
fn watch_git(manifest: &str) {
    let Some(git_path) = find_git(manifest) else {
        return;
    };
    if git_path.is_dir() {
        emit_watch(&git_path.join("HEAD"));
        emit_watch(&git_path.join("packed-refs"));
        if let Some(branch) = head_branch(&git_path.join("HEAD")) {
            emit_watch(&git_path.join(&branch));
        }
        return;
    }
    // 链接工作树：.git 是文件。
    let Ok(text) = std::fs::read_to_string(&git_path) else {
        return;
    };
    let Some(w) = text.strip_prefix("gitdir: ") else {
        return;
    };
    let w = PathBuf::from(w.trim());
    let w = if w.is_absolute() { w } else {
        git_path.parent().unwrap_or(Path::new(".")).join(w)
    };
    emit_watch(&w.join("HEAD"));
    // commondir：工作树共享的主 .git 目录（分支 ref 与 packed-refs 在那里）。
    let common = std::fs::read_to_string(w.join("commondir"))
        .ok()
        .map(|t| {
            let t = t.trim();
            let c = PathBuf::from(t);
            if c.is_absolute() {
                c
            } else {
                w.join(c)
            }
        });
    if let Some(c) = common {
        emit_watch(&c.join("packed-refs"));
        if let Some(branch) = head_branch(&w.join("HEAD")) {
            emit_watch(&c.join(branch));
        }
    }
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
