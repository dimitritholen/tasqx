//! Stamps the commit the binary was built from into `tasqx --version`.
//!
//! `CARGO_PKG_VERSION` alone cannot answer "am I running the latest build?".
//! Nothing bumps it during ordinary development, so a locally installed
//! `~/.cargo/bin/tasqx` reports `0.1.0` whether it was built from HEAD or from
//! a commit six bug-fixes ago — which is exactly what happened on 2026-07-19,
//! where only the binary's mtime revealed the staleness. The commit id turns
//! that from an inference into a fact the binary states about itself.
//!
//! Git absence is not an error: a build from a source tarball has no `.git`,
//! and failing there would make the crate unbuildable outside a checkout. Such
//! builds report `unknown`, which is honest — the commit genuinely is unknown.

use std::path::Path;
use std::process::Command;

fn main() {
    let build_id = match git(&["rev-parse", "--short=12", "HEAD"]) {
        Some(sha) => {
            // `--untracked-files=no`: a stray scratch file in the working tree
            // is not a difference between the source and the commit, and
            // flagging it would make `-dirty` mean so little it gets ignored.
            let dirty = git(&["status", "--porcelain", "--untracked-files=no"])
                .is_some_and(|s| !s.is_empty());
            if dirty { format!("{sha}-dirty") } else { sha }
        }
        None => "unknown".to_string(),
    };
    println!("cargo:rustc-env=TASQX_BUILD_ID={build_id}");

    // Without these, cargo caches this script's output and the stamped id
    // outlives the commit it names — the same lie in a new place.
    if let Some(git_dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        rerun_if_present(&format!("{git_dir}/HEAD"));
        rerun_if_present(&format!("{git_dir}/index"));
        // On a branch, `HEAD` is a symref that never changes on commit; the
        // ref it points at is what moves. Absent when the ref is packed, and
        // absent on a detached HEAD, hence the existence check.
        if let Some(head_ref) = git(&["symbolic-ref", "--quiet", "HEAD"]) {
            rerun_if_present(&format!("{git_dir}/{head_ref}"));
        }
    }
}

/// Emit a rerun trigger only for a path that exists.
///
/// Cargo re-runs the script unconditionally when a declared path is missing,
/// which would rebuild the crate on every single `cargo build`.
fn rerun_if_present(path: &str) {
    if Path::new(path).exists() {
        println!("cargo:rerun-if-changed={path}");
    }
}

/// Run git in the crate directory, or `None` if git is missing or fails.
///
/// git walks up to the repository root itself, so the crate directory works
/// from a workspace member without hard-coding how deep it sits.
fn git(args: &[&str]) -> Option<String> {
    let dir = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let out = Command::new("git").args(args).current_dir(dir).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}
