//! Reading a single markdown file into a memory doc's `(title, body)`.
//!
//! Moved out of `tasqx-cli`'s `memory_docs_from_path` (#787) so the engine
//! can re-read the same file itself — the refresh sweep (#789) and the
//! stale-flag check (#790) both run inside the engine, including under the
//! MCP server, where `tasqx-cli` is not linked.

use std::path::Path;

use crate::ApiError;

/// Read `path` into `(title, body)`. Pure I/O — no store access.
///
/// A UTF-8 BOM would defeat the `# ` heading match below AND end up in the
/// stored body and the index; strip it once, here.
///
/// #228.4: YAML frontmatter was indexed and shown as document body — every
/// file in a `~/.claude/.../memory/` directory (the corpus this importer's
/// own `--help` example points at, `docs/adr`, is the same idiom) opens with
/// one, and `originSessionId`/`modified`/`type` then dominated search
/// snippets over the prose that answers the query. Cut before the
/// title/heading scan below, so a frontmatter `title:` does not race the
/// body's own `# ` heading. `frontmatter::block` is the one fence-finder
/// (task #12/D135) — shared with the memory browser's own renderer and
/// `render::doc_summary` instead of each reading `---\n...\n---\n` its own
/// way.
///
/// Title: the first `# ` heading, else frontmatter's `title:`/`name:`, else
/// the file stem. The heading STAYS in the body — the title is an index
/// entry, not a cut. Frontmatter is cut; nothing there is prose meant to be
/// read.
pub fn read_doc(path: &Path) -> Result<(String, String), ApiError> {
    let body = std::fs::read_to_string(path)
        .map_err(|e| ApiError::bad_request(format!("cannot read {}: {e}", path.display())))?;
    let body = body.strip_prefix('\u{FEFF}').unwrap_or(&body);
    let (frontmatter, body) = match crate::frontmatter::block(body) {
        Some((fm, rest)) => (Some(fm), rest),
        None => (None, body),
    };
    let title = body
        .lines()
        .find_map(|l| l.strip_prefix("# "))
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(String::from)
        .or_else(|| frontmatter.and_then(frontmatter_title))
        .unwrap_or_else(|| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("untitled")
                .to_string()
        });
    Ok((title, body.to_string()))
}

/// `meta`'s modification time in whole seconds since the unix epoch, or
/// `None` where the platform has no answer — `modified()` is documented as
/// unavailable on some, and a file stamped before 1970 fails the subtraction
/// rather than the call.
///
/// Moved out of `tasqx-cli`'s importer (#789) so the side that WRITES
/// `origin_mtime` and the side that compares against it derive the number the
/// same way (D180). Two derivations of "the file's mtime" that round
/// differently would make every doc look changed.
pub fn unix_seconds(meta: &std::fs::Metadata) -> Option<i64> {
    let since = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    i64::try_from(since.as_secs()).ok()
}

/// The git toplevel above `file`: the nearest ancestor directory (starting at
/// `file`'s own parent) that carries a `.git` entry, file or directory.
///
/// Moved out of `tasqx-cli`'s `import_source` with [`repo_dir`] (#972) so the
/// key the CLI computes and the engine's replace guard agree on which
/// repository a file belongs to.
pub fn git_toplevel(file: &Path) -> Option<std::path::PathBuf> {
    let mut dir = file.parent()?;
    loop {
        if dir.join(".git").exists() {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
}

/// The checkout whose directory name keys `toplevel`'s docs (D203): the main
/// checkout when `toplevel` is a linked worktree — its `.git` file names a
/// gitdir whose `commondir` leads back to the main `.git` — and `toplevel`
/// itself otherwise, including whenever that chain cannot be read (a
/// submodule, a bare common dir, a main checkout that moved).
pub fn repo_dir(toplevel: &Path) -> std::path::PathBuf {
    let main = || -> Option<std::path::PathBuf> {
        let link = std::fs::read_to_string(toplevel.join(".git")).ok()?;
        let gitdir = toplevel.join(link.trim().strip_prefix("gitdir:")?.trim());
        let common = std::fs::read_to_string(gitdir.join("commondir")).ok()?;
        let common = std::fs::canonicalize(gitdir.join(common.trim())).ok()?;
        if common.file_name()? != ".git" {
            return None;
        }
        common.parent().map(Path::to_path_buf)
    };
    main().unwrap_or_else(|| toplevel.to_path_buf())
}

/// The title a frontmatter block would have given the document, if any:
/// `title:` or `name:` (`title` first), a bare or single-quoted scalar value.
/// Deliberately not a YAML parser — the values this needs to read are the
/// simple ones a memory doc's frontmatter actually carries, and a partial
/// parser that silently mis-reads a list or a block scalar would be worse
/// than not trying.
fn frontmatter_title(fm: &str) -> Option<String> {
    for key in ["title", "name"] {
        for line in fm.lines() {
            let Some(rest) = line.strip_prefix(key) else {
                continue;
            };
            let Some(value) = rest.trim_start().strip_prefix(':') else {
                continue;
            };
            let value = value.trim().trim_matches(['"', '\'']);
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// One file or symbol a memory doc cites (D235).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeRef {
    /// Relative to the git toplevel, `/`-joined.
    pub path: String,
    /// The `path::symbol` part, when the doc named one.
    pub symbol: Option<String>,
    /// A span that is only half path-like (a `/` but no known extension, or
    /// an extension but no `/`): `origin/main`, `memory.rs`. Checked only
    /// when its first segment exists under the root, so a branch name or a
    /// bare file name living somewhere deeper is never reported as missing.
    pub loose: bool,
}

impl CodeRef {
    /// How the ref is named in `stale_refs`: `path` or `path::symbol`.
    pub fn label(&self) -> String {
        match &self.symbol {
            Some(s) => format!("{}::{s}", self.path),
            None => self.path.clone(),
        }
    }
}

/// What a doc cites: the repo its `source` names (D203's `<repo>/<path>`),
/// if any, and its refs, capped at [`MAX_REFS`].
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DocRefs {
    /// The `<repo>` segment of a D203 source, when the source is a file key.
    pub repo: Option<String>,
    /// The refs, in the order they were cited.
    pub refs: Vec<CodeRef>,
}

/// The most refs one doc is checked for (D235): a stat per ref per hit.
pub const MAX_REFS: usize = 20;

/// The extensions that make a span read as a file name.
const CODE_EXTENSIONS: &[&str] = &[
    "rs", "md", "toml", "json", "yml", "yaml", "ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "sh",
    "go", "java", "kt", "swift", "c", "h", "cc", "cpp", "hpp", "cs", "rb", "php", "html", "css",
    "scss", "sql", "lock", "txt", "xml", "svelte", "vue",
];

/// `s` as a repo-relative path, or `None` when it does not look like one:
/// whitespace, a placeholder or URL character, an absolute or home path, or a
/// `..` segment that would leave the toplevel. A leading `./` and a trailing
/// `/` are cut. The flag says whether it is half path-like ([`CodeRef::loose`]).
fn path_like(s: &str) -> Option<(String, bool)> {
    let s = s.strip_prefix("./").unwrap_or(s).trim_end_matches('/');
    if s.is_empty()
        || !s
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'))
        || s.starts_with('/')
        || s.split('/')
            .any(|seg| seg.is_empty() || seg == "." || seg == "..")
    {
        return None;
    }
    let slash = s.contains('/');
    let ext = s
        .rsplit('/')
        .next()
        .and_then(|name| name.rsplit_once('.'))
        .is_some_and(|(stem, ext)| !stem.is_empty() && CODE_EXTENSIONS.contains(&ext));
    (slash || ext).then(|| (s.to_string(), !(slash && ext)))
}

/// One backticked span as a ref: `path`, `path:line` or `path::symbol`.
fn span_ref(span: &str) -> Option<CodeRef> {
    let (path, symbol) = match span.split_once("::") {
        Some((path, rest)) => {
            let last = rest.rsplit("::").next()?.trim_end_matches("()");
            let ok = !last.is_empty() && last.chars().all(|c| c.is_alphanumeric() || c == '_');
            (path, ok.then(|| last.to_string()))
        }
        None => match span.rsplit_once(':') {
            Some((path, line))
                if !line.is_empty() && line.chars().all(|c| c.is_ascii_digit() || c == '-') =>
            {
                (path, None)
            }
            Some(_) => return None,
            None => (span, None),
        },
    };
    let (path, loose) = path_like(path)?;
    Some(CodeRef {
        path,
        symbol,
        loose,
    })
}

/// The paths and symbols a doc cites (D235): its `source` when that is a
/// D203 `<repo>/<path>` key naming a file (the repo segment stripped and
/// returned), plus every backticked span in the body, outside fenced blocks,
/// that reads as `path`, `path:line` or `path::symbol`. Deduplicated, in
/// order, capped at [`MAX_REFS`]. Pure: nothing is read from disk here.
pub fn extract_refs(source: Option<&str>, body: &str) -> DocRefs {
    let mut out = DocRefs::default();
    let push = |r: CodeRef, out: &mut DocRefs| {
        if out.refs.len() < MAX_REFS
            && !out
                .refs
                .iter()
                .any(|o| o.path == r.path && o.symbol == r.symbol)
        {
            out.refs.push(r);
        }
    };
    if let Some(src) = source {
        let src = src.split('#').next().unwrap_or(src);
        // An empty repo segment is an absolute path, not a D203 key.
        if let Some((repo, rest)) = src.split_once('/') {
            let repo_ok = !repo.is_empty()
                && repo
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
            if let (true, Some((path, false))) = (repo_ok, path_like(rest)) {
                out.repo = Some(repo.to_string());
                let r = CodeRef {
                    path,
                    symbol: None,
                    loose: false,
                };
                push(r, &mut out);
            }
        }
    }
    let mut fenced = false;
    for line in body.lines() {
        let t = line.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        let parts: Vec<&str> = line.split('`').collect();
        for i in (1..parts.len().saturating_sub(1)).step_by(2) {
            if let Some(r) = span_ref(parts[i]) {
                push(r, &mut out);
            }
        }
    }
    out
}

/// A doc's verdict from [`verify_refs`] (D235).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefStatus {
    /// Every checked ref is there.
    Ok,
    /// A cited path is gone.
    MissingPath,
    /// Every path is there, but a cited symbol is not in its file.
    MissingSymbol,
    /// Nothing could be checked: no refs, no toplevel, or another repo.
    Unverifiable,
}

impl RefStatus {
    /// The status as `memory verify` spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            RefStatus::Ok => "ok",
            RefStatus::MissingPath => "missing_path",
            RefStatus::MissingSymbol => "missing_symbol",
            RefStatus::Unverifiable => "unverifiable",
        }
    }
}

/// [`verify_refs`]'s answer: the verdict and the refs that failed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefCheck {
    /// The verdict.
    pub status: RefStatus,
    /// The failing refs, missing paths first, each as [`CodeRef::label`].
    pub stale_refs: Vec<String>,
}

/// Check a doc's refs against `toplevel`'s working tree (D235).
///
/// Existence and text checks only, nothing cached or indexed (D182): a path
/// is `exists()`, a symbol is a word-boundary match of its last segment in
/// the named file. `toplevel` is `None` outside a git work tree. A doc whose
/// source names another repo than `toplevel` (its own name or its main
/// checkout's, [`repo_dir`]) is `unverifiable` — and so is one with no such
/// source whose `project` is not that name, since D218 names a session's
/// project after the toplevel — as is one with nothing left to check, never a
/// bare `ok`.
pub fn verify_refs(
    source: Option<&str>,
    body: &str,
    project: Option<&str>,
    toplevel: Option<&Path>,
) -> RefCheck {
    let unverifiable = RefCheck {
        status: RefStatus::Unverifiable,
        stale_refs: Vec::new(),
    };
    let Some(root) = toplevel else {
        return unverifiable;
    };
    let doc = extract_refs(source, body);
    if let Some(repo) = doc.repo.as_deref().or(project) {
        let named = |p: &Path| p.file_name().and_then(|n| n.to_str()) == Some(repo);
        if !named(root) && !named(&repo_dir(root)) {
            return unverifiable;
        }
    }
    let (mut checked, mut missing_path, mut missing_symbol) = (0, Vec::new(), Vec::new());
    for r in &doc.refs {
        let first = r.path.split('/').next().unwrap_or_default();
        if r.loose && !root.join(first).exists() {
            continue;
        }
        checked += 1;
        let file = root.join(&r.path);
        if !file.exists() {
            missing_path.push(r.label());
        } else if let Some(sym) = &r.symbol {
            if !mentions(&file, sym) {
                missing_symbol.push(r.label());
            }
        }
    }
    let status = if checked == 0 {
        RefStatus::Unverifiable
    } else if !missing_path.is_empty() {
        RefStatus::MissingPath
    } else if !missing_symbol.is_empty() {
        RefStatus::MissingSymbol
    } else {
        RefStatus::Ok
    };
    missing_path.extend(missing_symbol);
    RefCheck {
        status,
        stale_refs: missing_path,
    }
}

/// Does `file` contain `word` with no identifier character on either side?
/// An unreadable file (a directory, a permission error) does not.
fn mentions(file: &Path, word: &str) -> bool {
    let Ok(bytes) = std::fs::read(file) else {
        return false;
    };
    let text = String::from_utf8_lossy(&bytes);
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    text.match_indices(word).any(|(i, _)| {
        !text[..i].chars().next_back().is_some_and(ident)
            && !text[i + word.len()..].chars().next().is_some_and(ident)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(path: &str, symbol: Option<&str>, loose: bool) -> CodeRef {
        CodeRef {
            path: path.into(),
            symbol: symbol.map(Into::into),
            loose,
        }
    }

    /// `(source, body, expected repo, expected refs)`.
    type Case<'a> = (Option<&'a str>, &'a str, Option<&'a str>, Vec<CodeRef>);

    /// D235: one table over every span shape the extractor reads or refuses.
    #[test]
    fn extract_refs_reads_source_and_backticked_paths() {
        let cases: &[Case] = &[
            // D203 source: the repo segment is stripped and remembered.
            (
                Some("tasqx/docs/a.md"),
                "",
                Some("tasqx"),
                vec![r("docs/a.md", None, false)],
            ),
            // A fragment is cut; a source that is not a file path is no ref.
            (
                Some("klustaken/src/reminders.js#shape"),
                "",
                Some("klustaken"),
                vec![r("src/reminders.js", None, false)],
            ),
            (Some("hubq/ORC-01/intake"), "", None, vec![]),
            (Some("task:#12"), "", None, vec![]),
            (Some("SPEC.md (#2277)"), "", None, vec![]),
            (Some("/abs/path/x.md"), "", None, vec![]),
            // Body: path, path:line, path::symbol, loose spans.
            (
                None,
                "see `crates/a/src/b.rs`, `crates/a/src/c.rs:42` and \
                 `crates/a/src/d.rs::Engine::open()`",
                None,
                vec![
                    r("crates/a/src/b.rs", None, false),
                    r("crates/a/src/c.rs", None, false),
                    r("crates/a/src/d.rs", Some("open"), false),
                ],
            ),
            (
                None,
                "`origin/main`, `memory.rs`, `docs/wiki/`",
                None,
                vec![
                    r("origin/main", None, true),
                    r("memory.rs", None, true),
                    r("docs/wiki", None, true),
                ],
            ),
            // Refused: no path shape, a URL, a placeholder, an escape, a
            // rust path, whitespace, an absolute or home path, a fenced block.
            (
                None,
                "`foo` `https://x.y/z.md` `task/<id>` `../x.rs` `crate::memory_doc::read_doc` \
                 `a b.rs` `/etc/x.rs` `~/.claude/x.md`\n```\n`in/fence.rs`\n```\n`after/fence.rs`",
                None,
                vec![r("after/fence.rs", None, false)],
            ),
            // Duplicates fold.
            (
                Some("r/a/b.rs"),
                "`a/b.rs` `a/b.rs:3`",
                Some("r"),
                vec![r("a/b.rs", None, false)],
            ),
        ];
        for (source, body, repo, refs) in cases {
            let got = extract_refs(*source, body);
            assert_eq!(got.repo.as_deref(), *repo, "repo of {source:?} / {body:?}");
            assert_eq!(&got.refs, refs, "refs of {source:?} / {body:?}");
        }
        let many: String = (0..30).map(|i| format!("`d/f{i}.rs` ")).collect();
        assert_eq!(extract_refs(None, &many).refs.len(), MAX_REFS);
    }

    /// D235's fixtures: a renamed file is flagged, an existing one passes,
    /// a removed function in a file that still exists is flagged, and a doc
    /// with nothing to check is `unverifiable`, never a bare ok.
    #[test]
    fn verify_refs_flags_renamed_files_and_removed_symbols() {
        let root = std::env::temp_dir().join(format!("tasqx-verify-refs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/new_name.rs"), "fn x() {}").unwrap();
        std::fs::write(
            root.join("src/live.rs"),
            "pub fn kept_fn() {}\nfn kept_fn_two() {}",
        )
        .unwrap();
        let top = Some(root.as_path());
        let check = |source: Option<&str>, body: &str| verify_refs(source, body, None, top);

        let renamed = check(None, "lives in `src/old_name.rs`");
        assert_eq!(renamed.status, RefStatus::MissingPath);
        assert_eq!(renamed.stale_refs, vec!["src/old_name.rs".to_string()]);

        let fine = check(None, "lives in `src/live.rs::kept_fn`");
        assert_eq!(fine.status, RefStatus::Ok);
        assert!(fine.stale_refs.is_empty());

        // `kept_fn` is a word-boundary match: `removed` must not be found
        // inside `kept_fn_two` or anywhere else.
        let removed = check(None, "calls `src/live.rs::removed` and `src/live.rs::kept`");
        assert_eq!(removed.status, RefStatus::MissingSymbol);
        assert_eq!(
            removed.stale_refs,
            vec![
                "src/live.rs::removed".to_string(),
                "src/live.rs::kept".to_string()
            ]
        );

        // A missing path outranks a missing symbol.
        let both = check(None, "`src/live.rs::removed` `src/gone.rs`");
        assert_eq!(both.status, RefStatus::MissingPath);

        // Nothing to check: no refs, only loose refs whose first segment is
        // absent, another repo's source or project, or no toplevel at all.
        for (source, body, project, root) in [
            (None, "no refs here", None, top),
            (None, "`origin/main` `lib.rs`", None, top),
            (Some("otherrepo/src/live.rs"), "", None, top),
            (None, "`src/live.rs`", Some("otherrepo"), top),
            (None, "`src/live.rs`", None, None),
        ] {
            let got = verify_refs(source, body, project, root);
            assert_eq!(got.status, RefStatus::Unverifiable, "{source:?} {body:?}");
            assert!(got.stale_refs.is_empty());
        }
        // A loose ref whose first segment exists IS checked.
        assert_eq!(check(None, "`src/gone`").status, RefStatus::MissingPath);
        // The toplevel's own name is the repo a source may name.
        let name = root.file_name().unwrap().to_str().unwrap().to_string();
        let own = format!("{name}/src/live.rs");
        assert_eq!(check(Some(&own), "").status, RefStatus::Ok);
        // A doc scoped to the project the toplevel names is checked, and a
        // source naming this repo outranks a project naming another.
        let scoped = verify_refs(None, "`src/gone.rs`", Some(&name), top);
        assert_eq!(scoped.status, RefStatus::MissingPath);
        let sourced = verify_refs(Some(&own), "", Some("otherrepo"), top);
        assert_eq!(sourced.status, RefStatus::Ok);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Review findings on `memory import`: a UTF-8 BOM defeated the `# `
    /// title match and leaked into the stored body.
    #[test]
    fn memory_import_reads_upper_case_md_and_strips_the_bom() {
        let dir = std::env::temp_dir().join(format!("tasqx-memimp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let lower = dir.join("lower.md");
        let upper = dir.join("UPPER.MD");
        std::fs::write(&lower, "# Lower doc\n\nbody").unwrap();
        std::fs::write(&upper, "\u{FEFF}# Upper doc\n\nbody").unwrap();

        let (lower_title, lower_body) = read_doc(&lower).expect("lower.md reads");
        let (upper_title, upper_body) = read_doc(&upper).expect("UPPER.MD reads");
        assert_eq!(lower_title, "Lower doc");
        assert_eq!(
            upper_title, "Upper doc",
            "the BOM must not defeat title derivation"
        );
        for body in [&lower_body, &upper_body] {
            assert!(
                !body.starts_with('\u{FEFF}'),
                "the BOM must not reach the stored body"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #228.4: YAML frontmatter (the shape every file in a
    /// `~/.claude/.../memory/` directory carries) was indexed and shown as
    /// document body, so `originSessionId`/`modified`/`type` dominated search
    /// snippets over the prose that answers the query. It must be cut before
    /// storage, and its `title:` used when the body has no `# ` heading of
    /// its own.
    #[test]
    fn memory_import_strips_frontmatter_and_reads_its_title() {
        let dir = std::env::temp_dir().join(format!("tasqx-memimp-fm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("note.md");
        std::fs::write(
            &file,
            "---\ntitle: \"release workflow\"\noriginSessionId: 71aa288e\nmodified: 2026-07-23\n---\nHow releases actually ship.\n",
        )
        .unwrap();

        let (title, body) = read_doc(&file).expect("note.md reads");
        assert_eq!(
            title, "release workflow",
            "frontmatter's `title:` must be used when there is no `# ` heading"
        );
        assert!(
            !body.contains("originSessionId"),
            "frontmatter metadata must not reach the stored/indexed body: {body:?}"
        );
        assert!(
            body.contains("How releases actually ship."),
            "the real prose must survive the cut: {body:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #789: the helper the CLI's importer used to own privately. It is the
    /// one derivation of `origin_mtime` now, so it answers in WHOLE seconds
    /// — the unit the column stores — and a file written just now has a
    /// plausible one, not a zero the freshness check would read as 1970.
    #[test]
    fn unix_seconds_answers_whole_seconds_for_a_file_written_now() {
        let dir = std::env::temp_dir().join(format!("tasqx-memdoc-mtime-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.md");
        std::fs::write(&file, "# A\n\nbody").unwrap();

        let meta = std::fs::metadata(&file).expect("metadata");
        let secs = unix_seconds(&meta).expect("a platform that answers modified()");
        // 2020-01-01, safely below any clock this runs on and safely above a
        // zero or a value in milliseconds.
        assert!(
            (1_577_836_800..4_102_444_800).contains(&secs),
            "not a plausible unix-seconds mtime: {secs}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
