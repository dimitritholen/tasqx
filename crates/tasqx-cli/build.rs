//! Stamps the commit the binary was built from into `tasqx --version`, and
//! reserves a larger main-thread stack for the binary on Windows.
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
            if dirty {
                format!("{sha}-dirty")
            } else {
                sha
            }
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

    reserve_windows_stack();
}

/// Windows' PE default (the linker's `/STACK` reserve) is 1 MiB, against
/// ~8 MiB for a Unix pthread's default stack. clap's derived
/// `FromArgMatches`/`Parser` code for `Cli`/`Command` — one large enum whose
/// `list` variant alone carries five fields — got heavy enough in an
/// unoptimized debug build that ordinary argv parsing, for ANY subcommand,
/// overflowed that 1 MiB on Windows while the identical binary runs fine on
/// Linux and macOS at their larger default. Bisected (CI-driven `git bisect
/// run` against `test (windows-latest)`, grepping for the exact panic text)
/// to 0e945ef, `feat(list): --sort, --limit, --offset and --fields`: every
/// Windows CI run since has failed "thread 'main' has overflowed its stack"
/// on plain `tasqx add`/`tasqx init` — commands that touch none of the new
/// flags, so this is parse-time cost, not a runtime bug in the new code.
///
/// `cargo:rustc-link-arg-bins`, not a `[target.*] rustflags` entry in a
/// `.cargo/config.toml`: this crate's CI sets `RUSTFLAGS: -D warnings` as a
/// step env var, and cargo does not merge an env `RUSTFLAGS` with a config
/// file's — the env wins outright, silently discarding the config file's
/// flags. A build-script link-arg directive is a separate channel from
/// rustflags entirely, so it survives that override (verified: the
/// `.cargo/config.toml` version of this fix measurably did nothing against
/// real CI; this one does).
fn reserve_windows_stack() {
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if os != "windows" {
        return;
    }
    // 8 MiB — the same order of magnitude Unix's default already gives every
    // build of this binary without anyone having to think about it.
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let arg = if env == "msvc" {
        "/STACK:8388608"
    } else {
        "-Wl,--stack,8388608"
    };
    println!("cargo:rustc-link-arg-bins={arg}");
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
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}
