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

/// The `source` `memory.import` stores for `file` — a doc's identity (D174),
/// so it has to read the same on every machine and from every starting
/// directory. Three cases, in order:
/// 1. `file` is inside a git work tree (found by walking up from its
///    canonicalised directory for a `.git` entry — a worktree's is a FILE,
///    not a directory, so either counts): the toplevel's directory name,
///    then the path relative to that toplevel — `clouter/README.md`. A
///    worktree is named after its MAIN checkout (`repo_dir`), so it keys the
///    same docs the main checkout does.
/// 2. No `.git` above it, but `file` is under the current directory: the
///    cwd's directory name, then the path relative to the canonicalised cwd.
/// 3. Neither (an `../elsewhere` import): the canonicalised absolute path.
///
/// The leading directory name is #972/D203: without it every repo's
/// `README.md` shared one key, and importing a second repo's silently
/// replaced the first's doc.
///
/// Always `/`-joined, even on Windows, so the same folder imported from
/// either platform names the same doc (task #784).
pub fn import_source(file: &std::path::Path) -> String {
    let canon = canonical(file);
    if let Some(toplevel) = git_toplevel(&canon) {
        if let Ok(rel) = canon.strip_prefix(&toplevel) {
            return led_by(&repo_dir(&toplevel), rel);
        }
    }
    if let Ok(cwd) = std::env::current_dir().and_then(std::fs::canonicalize) {
        if let Ok(rel) = canon.strip_prefix(&cwd) {
            return led_by(&cwd, rel);
        }
    }
    slash_joined(&canon)
}

/// `rel`, `/`-joined, under `dir`'s last component — or bare when `dir` has
/// none (the filesystem root).
fn led_by(dir: &std::path::Path, rel: &std::path::Path) -> String {
    match dir.file_name() {
        Some(name) => slash_joined(&std::path::Path::new(name).join(rel)),
        None => slash_joined(rel),
    }
}

/// `rel` spelled with `/`, so a source string is the same text on Windows and
/// on Unix — the one line all three `import_source` branches share, the
/// absolute fallback included (#797).
///
/// Windows-only replace: `\` IS that platform's path separator, so
/// `PathBuf`'s own components already used it. On unix `\` is legal INSIDE a
/// single filename — `docs/a\b.md` is one file, not two path segments — so
/// replacing it there turned a literal backslash into a `/` and made that
/// file compute the same `source` as an unrelated `docs/a/b.md` (#798).
#[cfg(windows)]
fn slash_joined(rel: &std::path::Path) -> String {
    rel.to_string_lossy().replace('\\', "/")
}

#[cfg(not(windows))]
fn slash_joined(rel: &std::path::Path) -> String {
    rel.to_string_lossy().into_owned()
}

/// `file`'s canonical absolute path, or the path as given when the filesystem
/// cannot answer (a broken symlink, a race with an editor).
fn canonical(file: &Path) -> std::path::PathBuf {
    std::fs::canonicalize(file).unwrap_or_else(|_| file.to_path_buf())
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
    /// The doc's own D203 source, relative to the toplevel by construction,
    /// so checked even when its first segment is absent. A body span is
    /// checked only when its first segment exists at the toplevel: a branch
    /// name (`origin/main`), a bare file name living deeper (`memory.rs`) or
    /// a crate-relative path (`engine/memory.rs`) is never reported missing.
    pub anchored: bool,
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
/// if any, and its refs in citation order, deduplicated.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DocRefs {
    /// The `<repo>` segment of a D203 source, when the source is a file key.
    pub repo: Option<String>,
    /// The refs, in the order they were cited.
    pub refs: Vec<CodeRef>,
}

/// The most refs one doc is checked for (D235): a stat per ref per hit. A
/// ref [`verify_refs`] skips does not count against it, so junk spans cannot
/// crowd out the real ones.
pub const MAX_REFS: usize = 20;

/// The bytes of a file a symbol is looked for in (D235). A larger file is
/// not read, and its symbol counts as present: unverified, never missing.
pub const SYMBOL_SCAN_CAP: u64 = 4 * 1024 * 1024;

/// The extensions that make a span read as a file name.
const CODE_EXTENSIONS: &[&str] = &[
    "rs", "md", "toml", "json", "yml", "yaml", "ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "sh",
    "go", "java", "kt", "swift", "c", "h", "cc", "cpp", "hpp", "cs", "rb", "php", "html", "css",
    "scss", "sql", "lock", "txt", "xml", "svelte", "vue",
];

/// `s` as a repo-relative path, or `None` when it cannot be one:
/// whitespace, a placeholder, ellipsis or URL character, an absolute or home
/// path, or a segment of dots alone (`..` would leave the toplevel, `...` is
/// an elision). A leading `./` and a trailing `/` are cut.
fn clean_path(s: &str) -> Option<String> {
    let s = s.strip_prefix("./").unwrap_or(s).trim_end_matches('/');
    if s.is_empty()
        || s.contains("...")
        || !s
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'))
        || s.starts_with('/')
        || s.split('/')
            .any(|seg| seg.is_empty() || seg.chars().all(|c| c == '.'))
    {
        return None;
    }
    Some(s.to_string())
}

/// A body span as a path: [`clean_path`], plus a `/` or a known extension,
/// which the flag says it has.
fn path_like(s: &str) -> Option<(String, bool)> {
    let s = clean_path(s)?;
    let slash = s.contains('/');
    let ext = has_code_ext(&s);
    (slash || ext).then_some((s, ext))
}

/// Does the path's last segment end in a known extension?
fn has_code_ext(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .and_then(|name| name.rsplit_once('.'))
        .is_some_and(|(stem, ext)| !stem.is_empty() && CODE_EXTENSIONS.contains(&ext))
}

/// One backticked span as a ref: `path`, `path:line`, `path:line:col` or
/// `path::A::b`. A symbol whose segments are not all identifiers (a call
/// with arguments, a generic) is dropped and the path kept.
fn span_ref(span: &str) -> Option<CodeRef> {
    let (path, symbol) = match span.split_once("::") {
        Some((path, rest)) => {
            let rest = rest.trim_end_matches("()");
            let ok = rest
                .split("::")
                .all(|seg| !seg.is_empty() && seg.chars().all(|c| c.is_alphanumeric() || c == '_'));
            (path, ok.then(|| rest.to_string()))
        }
        None => {
            // Up to two trailing `:N` parts: a line, then a column.
            let mut path = span;
            for _ in 0..2 {
                match path.rsplit_once(':') {
                    Some((p, n))
                        if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit() || c == '-') =>
                    {
                        path = p
                    }
                    _ => break,
                }
            }
            if path.contains(':') {
                return None;
            }
            (path, None)
        }
    };
    let (path, _) = path_like(path)?;
    Some(CodeRef {
        path,
        symbol,
        anchored: false,
    })
}

/// The code spans on one line, as CommonMark reads them: a run of N
/// backticks opens a span that the next run of exactly N closes; a run with
/// no such partner is literal text. One leading and one trailing space are
/// cut when both are there, so a double-backtick span around a backticked
/// name keeps the inner backticks. A span is not followed across lines.
fn code_spans(line: &str) -> Vec<&str> {
    let b = line.as_bytes();
    let run = |i: usize| b[i..].iter().take_while(|c| **c == b'`').count();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'`' {
            i += 1;
            continue;
        }
        let n = run(i);
        let start = i + n;
        let mut j = start;
        let mut close = None;
        while j < b.len() {
            if b[j] == b'`' {
                let m = run(j);
                if m == n {
                    close = Some(j);
                    break;
                }
                j += m;
            } else {
                j += 1;
            }
        }
        match close {
            Some(j) => {
                let span = &line[start..j];
                let span = match span.strip_prefix(' ').and_then(|s| s.strip_suffix(' ')) {
                    Some(inner) if !inner.trim().is_empty() => inner,
                    _ => span,
                };
                out.push(span);
                i = j + n;
            }
            None => i = start,
        }
    }
    out
}

/// A fence opener or closer: the fence character and the run's length, when
/// `line` starts (after indentation) with three or more backticks or tildes.
fn fence(line: &str) -> Option<(char, usize)> {
    // Four columns of indentation make an indented code line, not a fence.
    let indent: usize = line
        .chars()
        .take_while(|c| matches!(c, ' ' | '\t'))
        .map(|c| if c == '\t' { 4 } else { 1 })
        .sum();
    if indent >= 4 {
        return None;
    }
    let t = line.trim_start();
    let c = t.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let n = t.chars().take_while(|x| *x == c).count();
    // A backtick fence's info string holds no backtick: "```x```" is inline.
    let inline = c == '`' && t[n..].contains('`');
    (n >= 3 && !inline).then_some((c, n))
}

/// The paths and symbols a doc cites (D235): its `source` when that is a
/// D203 `<repo>/<path>` key naming a file (the repo segment stripped and
/// returned), plus every backticked span in the body, outside fenced blocks,
/// that reads as `path`, `path:line` or `path::symbol`. Deduplicated, in
/// order; [`verify_refs`] applies [`MAX_REFS`]. Pure: nothing is read from
/// disk here.
///
/// A fence closes, as CommonMark has it, only on a line of the opening
/// character at least as long as the opener and carrying nothing else.
pub fn extract_refs(source: Option<&str>, body: &str) -> DocRefs {
    let mut out = DocRefs::default();
    let push = |r: CodeRef, out: &mut DocRefs| {
        if !out
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
            // Any D203 key names its repo and a file, extension or not
            // (`tasqx/Dockerfile`): the key is a path by construction.
            if let (true, Some(path)) = (repo_ok, clean_path(rest)) {
                out.repo = Some(repo.to_string());
                let r = CodeRef {
                    path,
                    symbol: None,
                    anchored: true,
                };
                push(r, &mut out);
            }
        }
    }
    let mut open: Option<(char, usize)> = None;
    for line in body.lines() {
        match (open, fence(line)) {
            (None, Some(f)) => {
                open = Some(f);
                continue;
            }
            (Some((c, n)), Some((c2, n2))) => {
                let rest = line.trim_start().trim_start_matches(c2);
                if c2 == c && n2 >= n && rest.trim().is_empty() {
                    open = None;
                }
                continue;
            }
            (Some(_), None) => continue,
            (None, None) => {}
        }
        for span in code_spans(line) {
            if let Some(r) = span_ref(span) {
                push(r, &mut out);
            }
        }
    }
    out
}

/// The working tree cited code is checked against (D235): a git toplevel and
/// the names a doc's repo may go by there — the toplevel's own and its main
/// checkout's ([`repo_dir`]). Resolved once per request, not per doc.
#[derive(Debug, Clone)]
pub struct RefRoot {
    toplevel: std::path::PathBuf,
    names: Vec<String>,
    /// Top-level entries the root `.gitignore` ignores (`target`,
    /// `node_modules`): build artifacts a doc may cite that a fresh checkout
    /// lacks, so a ref into one gets no verdict.
    ignored: Vec<String>,
    /// Each cited file's text for a symbol search, read at most once per
    /// root, so a request's docs citing one file read it once. `None` is a
    /// file that cannot prove a symbol absent ([`scannable_text`]).
    texts:
        std::cell::RefCell<std::collections::HashMap<std::path::PathBuf, Option<std::rc::Rc<str>>>>,
    /// How many files were actually read, for the test that pins the cache.
    #[cfg(test)]
    reads: std::cell::Cell<usize>,
}

impl RefRoot {
    /// The git toplevel at or above `workdir`, or `None` outside a work tree.
    pub fn at(workdir: &Path) -> Option<RefRoot> {
        // `git_toplevel` takes a FILE and starts at its parent.
        let toplevel = git_toplevel(&workdir.join("_"))?;
        let mut names = Vec::new();
        for dir in [toplevel.clone(), repo_dir(&toplevel)] {
            if let Some(n) = dir.file_name().and_then(|n| n.to_str()) {
                if !names.iter().any(|x| x == n) {
                    names.push(n.to_string());
                }
            }
        }
        let ignored = std::fs::read_to_string(toplevel.join(".gitignore"))
            .map(|t| ignored_top_level(&t))
            .unwrap_or_default();
        Some(RefRoot {
            toplevel,
            names,
            ignored,
            texts: Default::default(),
            #[cfg(test)]
            reads: Default::default(),
        })
    }

    /// `file`'s text, read on first ask and kept for the root's lifetime.
    fn text(&self, file: &Path) -> Option<std::rc::Rc<str>> {
        if let Some(t) = self.texts.borrow().get(file) {
            return t.clone();
        }
        #[cfg(test)]
        self.reads.set(self.reads.get() + 1);
        let t = scannable_text(file).map(std::rc::Rc::from);
        self.texts
            .borrow_mut()
            .insert(file.to_path_buf(), t.clone());
        t
    }

    /// The toplevel directory.
    pub fn toplevel(&self) -> &Path {
        &self.toplevel
    }

    /// The names a doc's repo may go by here: the toplevel's own, then its
    /// main checkout's ([`repo_dir`]) when that differs.
    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// Forget the files read so far: a `memory verify` sweep clears it
    /// between docs, so a whole store's cited files are never held at once.
    pub fn clear_cache(&self) {
        self.texts.borrow_mut().clear();
    }
}

/// The top-level names a root `.gitignore` ignores: a pattern naming one
/// entry, with or without a leading or trailing `/` (`/target`, `target/`,
/// `node_modules`). Comments, negations and anything with an inner `/` or a
/// glob character are passed over.
// ponytail: root .gitignore, plain names only. Nested .gitignore files,
// globs and .git/info/exclude are not parsed; a ref into what they ignore
// is still checked. Shell out to `git check-ignore` if that ever matters.
fn ignored_top_level(gitignore: &str) -> Vec<String> {
    gitignore
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('!'))
        .map(|l| l.trim_start_matches('/').trim_end_matches('/'))
        .filter(|l| !l.is_empty() && !l.contains(['/', '*', '?', '[', '\\']))
        .map(String::from)
        .collect()
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

/// Check a doc's refs against `root`'s working tree (D235).
///
/// Existence and text checks only, nothing indexed (D182): a path is
/// `exists()`; a symbol is a word-boundary match of every one of its `::`
/// segments in the named file (`mentions`), each file read at most once per
/// root. Segments are matched apart, so a symbol whose parts all survive
/// elsewhere in the file (`Engine` still there, its `open` moved to another
/// type that also has one) is not flagged. `root` is `None` outside a git
/// work tree. The doc's repo is the one its D203 source names, else its
/// `project`; a doc naming neither (a global doc with no repo source) is
/// `unverifiable`, never checked against whichever repo the reader is in,
/// and so is one naming another repo than `root`'s, or with nothing left to
/// check. Never a bare `ok`. A body ref whose first segment is absent at the
/// toplevel is skipped without a verdict or a count ([`CodeRef::anchored`]):
/// a crate-relative citation cannot be told from one about another tree, so
/// a file renamed under such a citation goes unflagged. At most [`MAX_REFS`]
/// refs are checked.
pub fn verify_refs(
    source: Option<&str>,
    body: &str,
    project: Option<&str>,
    root: Option<&RefRoot>,
) -> RefCheck {
    let unverifiable = RefCheck {
        status: RefStatus::Unverifiable,
        stale_refs: Vec::new(),
    };
    let Some(root) = root else {
        return unverifiable;
    };
    let doc = extract_refs(source, body);
    match doc.repo.as_deref().or(project) {
        Some(repo) if root.names.iter().any(|n| n == repo) => {}
        _ => return unverifiable,
    }
    let top = root.toplevel();
    let (mut checked, mut missing_path, mut missing_symbol) = (0, Vec::new(), Vec::new());
    for r in &doc.refs {
        if checked == MAX_REFS {
            break;
        }
        let first = r.path.split('/').next().unwrap_or_default();
        // A source key with no known extension that is not there is no
        // claim about a file (`hubq/ORC-01/intake`, a `memory add` key): no
        // verdict. It still named the doc's repo above.
        let file_less_key = r.anchored && !has_code_ext(&r.path) && !top.join(&r.path).exists();
        if (!r.anchored && !top.join(first).exists())
            || file_less_key
            || root.ignored.iter().any(|i| i == first)
        {
            continue;
        }
        checked += 1;
        let file = top.join(&r.path);
        if !file.exists() {
            missing_path.push(r.label());
        } else if let (Some(sym), Some(text)) = (&r.symbol, root.text(&file)) {
            // Every segment must be there: a removed `Engine` is caught even
            // while an unrelated `open` survives.
            if sym.split("::").any(|seg| !mentions(&text, seg)) {
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

/// `file`'s text when it can prove a symbol absent: a regular file of at
/// most [`SYMBOL_SCAN_CAP`] bytes that reads cleanly. A directory, a FIFO, a
/// larger file or a read error is `None`, its symbols present-unverified, so
/// the check never reads a pipe or a huge file and never cries missing for
/// what it could not look at.
fn scannable_text(file: &Path) -> Option<String> {
    use std::io::Read;
    let meta = std::fs::metadata(file).ok()?;
    if !meta.is_file() || meta.len() > SYMBOL_SCAN_CAP {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open(file)
        .and_then(|f| f.take(SYMBOL_SCAN_CAP + 1).read_to_end(&mut bytes))
        .ok()?;
    (bytes.len() as u64 <= SYMBOL_SCAN_CAP).then(|| String::from_utf8_lossy(&bytes).into_owned())
}

/// Does `text` hold `word` with no identifier character on either side?
fn mentions(text: &str, word: &str) -> bool {
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    text.match_indices(word).any(|(i, _)| {
        !text[..i].chars().next_back().is_some_and(ident)
            && !text[i + word.len()..].chars().next().is_some_and(ident)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(path: &str, symbol: Option<&str>, anchored: bool) -> CodeRef {
        CodeRef {
            path: path.into(),
            symbol: symbol.map(Into::into),
            anchored,
        }
    }

    /// `(source, body, expected repo, expected refs)`.
    type Case<'a> = (Option<&'a str>, &'a str, Option<&'a str>, Vec<CodeRef>);

    /// D235: one table over every span shape the extractor reads or refuses.
    #[test]
    fn extract_refs_reads_source_and_backticked_paths() {
        let branches: String = (0..20).map(|i| format!("`origin/b{i}` ")).collect();
        let after_branches = format!("{branches}`src/real.rs`");
        let cases: &[Case] = &[
            // D203 source: the repo segment is stripped and remembered.
            (
                Some("tasqx/docs/a.md"),
                "",
                Some("tasqx"),
                vec![r("docs/a.md", None, true)],
            ),
            // A fragment is cut; a key with no extension is a path too.
            (
                Some("klustaken/src/reminders.js#shape"),
                "",
                Some("klustaken"),
                vec![r("src/reminders.js", None, true)],
            ),
            (
                Some("tasqx/Dockerfile"),
                "",
                Some("tasqx"),
                vec![r("Dockerfile", None, true)],
            ),
            (
                Some("hubq/ORC-01/intake"),
                "",
                Some("hubq"),
                vec![r("ORC-01/intake", None, true)],
            ),
            // Not a D203 key: no repo, no ref.
            (Some("task:#12"), "", None, vec![]),
            (Some("SPEC.md (#2277)"), "", None, vec![]),
            (Some("/abs/path/x.md"), "", None, vec![]),
            // Body: path, path:line, path::symbol, half path-like spans.
            (
                None,
                "see `crates/a/src/b.rs`, `crates/a/src/c.rs:42` and \
                 `crates/a/src/d.rs::Engine::open()`",
                None,
                vec![
                    r("crates/a/src/b.rs", None, false),
                    r("crates/a/src/c.rs", None, false),
                    r("crates/a/src/d.rs", Some("Engine::open"), false),
                ],
            ),
            (
                None,
                "`origin/main`, `memory.rs`, `docs/wiki/`",
                None,
                vec![
                    r("origin/main", None, false),
                    r("memory.rs", None, false),
                    r("docs/wiki", None, false),
                ],
            ),
            // Refused: no path shape, a URL, a placeholder, an escape, an
            // elision, a rust path, whitespace, an absolute or home path, a
            // fenced block.
            (
                None,
                "`foo` `https://x.y/z.md` `task/<id>` `../x.rs` `crates/.../engine.rs` \
                 `a/.../b` `.../x.rs` `crates/…/x.rs` `crate::memory_doc::read_doc` \
                 `a b.rs` `/etc/x.rs` `~/.claude/x.md`\n```\n`in/fence.rs`\n```\n`after/fence.rs`",
                None,
                vec![r("after/fence.rs", None, false)],
            ),
            // CommonMark fences: a ```` block holds a ``` line, a ``` block
            // holds a ~~~ line, and neither closes it.
            (
                None,
                "````\n```\n`in/a.rs`\n```\n`in/b.rs`\n````\n`out/a.rs`\n\
                 ```\n~~~\n`in/c.rs`\n~~~\n```\n`out/b.rs`\n```x```\n`out/c.rs`",
                None,
                vec![
                    r("out/a.rs", None, false),
                    r("out/b.rs", None, false),
                    r("out/c.rs", None, false),
                ],
            ),
            // `path:line:col` strips both numbers; a third colon part, or a
            // non-numeric one, is no path.
            (
                None,
                "`src/a.rs:12:4` `src/b.rs:1:2:3` `src/c.rs:x`",
                None,
                vec![r("src/a.rs", None, false)],
            ),
            // A fence line indented four columns is code, not a fence: it
            // opens nothing, so the span after it is read.
            (
                None,
                "    ```\n`after/indented.rs`\n\t~~~\n`after/tab.rs`",
                None,
                vec![
                    r("after/indented.rs", None, false),
                    r("after/tab.rs", None, false),
                ],
            ),
            // A root-level source file is a ref too.
            (
                Some("tasqx/DESIGN.md"),
                "",
                Some("tasqx"),
                vec![r("DESIGN.md", None, true)],
            ),
            // CommonMark code spans: a double-backtick span closes on the next
            // double run, so the backticked name inside it is not a path; an
            // unmatched run is literal and does not swallow the next span.
            (
                None,
                "``crates/x.rs`` and `` `y/z.rs` `` then a `` b `c/d.rs` e",
                None,
                vec![r("crates/x.rs", None, false), r("c/d.rs", None, false)],
            ),
            // Duplicates fold.
            (
                Some("r/a/b.rs"),
                "`a/b.rs` `a/b.rs:3`",
                Some("r"),
                vec![r("a/b.rs", None, true)],
            ),
        ];
        for (source, body, repo, refs) in cases {
            let got = extract_refs(*source, body);
            assert_eq!(got.repo.as_deref(), *repo, "repo of {source:?} / {body:?}");
            assert_eq!(&got.refs, refs, "refs of {source:?} / {body:?}");
        }
        // No cap at extraction: twenty branch names cannot crowd out a path.
        let got = extract_refs(None, &after_branches);
        assert_eq!(got.refs.last(), Some(&r("src/real.rs", None, false)));
    }

    /// A temp git checkout named `proj`, with `src/live.rs`.
    fn fixture_repo(tag: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir()
            .join(format!("tasqx-verify-{tag}-{}", std::process::id()))
            .join("proj");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/new_name.rs"), "fn x() {}").unwrap();
        std::fs::write(
            root.join("src/live.rs"),
            "pub fn kept_fn() {}\nfn kept_fn_two() {}",
        )
        .unwrap();
        root
    }

    /// D235's fixtures: a renamed file is flagged, an existing one passes,
    /// a removed function in a file that still exists is flagged, and a doc
    /// with nothing to check is `unverifiable`, never a bare ok.
    #[test]
    fn verify_refs_flags_renamed_files_and_removed_symbols() {
        let root = fixture_repo("refs");
        let rr = RefRoot::at(&root.join("src")).expect("a toplevel");
        assert_eq!(rr.toplevel(), root.as_path());
        let top = Some(&rr);
        let check = |source: Option<&str>, body: &str| verify_refs(source, body, Some("proj"), top);

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

        // Every `::` segment must be there: `Engine::open` is gone when
        // `Engine` is, although another type's `open` survives.
        std::fs::write(
            root.join("src/engine.rs"),
            "struct Other;\nimpl Other { fn open() {} }",
        )
        .unwrap();
        let gone = check(None, "`src/engine.rs::Engine::open()`");
        assert_eq!(gone.status, RefStatus::MissingSymbol);
        assert_eq!(
            gone.stale_refs,
            vec!["src/engine.rs::Engine::open".to_string()]
        );
        // The limit, by design: segments are matched apart, so with both
        // words still in the file the moved method is not flagged.
        std::fs::write(
            root.join("src/engine.rs"),
            "struct Engine;\nstruct Other;\nimpl Other { fn open() {} }",
        )
        .unwrap();
        let rr2 = RefRoot::at(&root).unwrap();
        let limit = verify_refs(
            None,
            "`src/engine.rs::Engine::open`",
            Some("proj"),
            Some(&rr2),
        );
        assert_eq!(limit.status, RefStatus::Ok);

        // A missing path outranks a missing symbol.
        let both = check(None, "`src/live.rs::removed` `src/gone.rs`");
        assert_eq!(both.status, RefStatus::MissingPath);

        // Nothing to check: no refs, only refs whose first segment is
        // absent, another repo's source or project, a global doc naming no
        // repo at all, or no toplevel.
        for (source, body, project, root) in [
            (None, "no refs here", Some("proj"), top),
            (None, "`origin/main` `lib.rs`", Some("proj"), top),
            (Some("otherrepo/src/live.rs"), "", Some("proj"), top),
            (None, "`src/gone.rs`", Some("otherrepo"), top),
            (None, "`src/gone.rs`", None, top),
            (None, "`src/gone.rs`", Some("proj"), None),
        ] {
            let got = verify_refs(source, body, project, root);
            assert_eq!(got.status, RefStatus::Unverifiable, "{source:?} {body:?}");
            assert!(got.stale_refs.is_empty());
        }
        // A ref with no extension whose first segment exists IS checked.
        assert_eq!(check(None, "`src/gone`").status, RefStatus::MissingPath);
        // An extensionless D203 source is checked as a path.
        // An extensionless D203 source that exists is checked as a path; one
        // that is absent is a key, not a file: no verdict for it, and the doc
        // is judged by its body refs.
        std::fs::write(root.join("Dockerfile"), "FROM scratch").unwrap();
        assert_eq!(check(Some("proj/Dockerfile"), "").status, RefStatus::Ok);
        let key = check(Some("proj/ORC-01/intake"), "");
        assert_eq!(key.status, RefStatus::Unverifiable);
        let key = check(Some("proj/ORC-01/intake"), "`src/gone.rs`");
        assert_eq!(key.stale_refs, vec!["src/gone.rs".to_string()]);
        // With a known extension an absent source is still a renamed file.
        assert_eq!(
            check(Some("proj/GONE.md"), "").stale_refs,
            vec!["GONE.md".to_string()]
        );
        // A build artifact the root .gitignore ignores gets no verdict, even
        // with its directory present: a fresh checkout would not have it.
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::write(
            root.join(".gitignore"),
            "# build\n/target\nnode_modules/\n*.log\n",
        )
        .unwrap();
        let ignoring = RefRoot::at(&root).unwrap();
        let artifact = verify_refs(None, "`target/debug/tasqx`", Some("proj"), Some(&ignoring));
        assert_eq!(artifact.status, RefStatus::Unverifiable);
        assert_eq!(
            ignored_top_level("# c\n/target\nnode_modules/\n!keep\n*.log\na/b\n"),
            vec!["target".to_string(), "node_modules".to_string()]
        );
        // DESIGN.md's crate-relative "Where" style names no top-level
        // segment, so it is skipped, not cried missing; a real path beside
        // it still is checked.
        let crate_relative = "`engine/memory.rs`, `tui/settings.rs`";
        assert_eq!(check(None, crate_relative).status, RefStatus::Unverifiable);
        let mixed = check(None, &format!("{crate_relative} `src/gone.rs`"));
        assert_eq!(mixed.stale_refs, vec!["src/gone.rs".to_string()]);
        // The doc's own source is toplevel-relative by construction: a
        // renamed root-level file is flagged although its segment is gone.
        let source = check(Some("proj/GONE.md"), "");
        assert_eq!(source.stale_refs, vec!["GONE.md".to_string()]);
        // A source naming this repo is checked even for a global doc, and
        // outranks a project naming another.
        let sourced = verify_refs(Some("proj/src/live.rs"), "", None, top);
        assert_eq!(sourced.status, RefStatus::Ok);
        let sourced = verify_refs(Some("proj/src/live.rs"), "", Some("otherrepo"), top);
        assert_eq!(sourced.status, RefStatus::Ok);

        // MAX_REFS counts checked refs only: twenty skipped branch names
        // before a real missing path do not hide it, and the cap holds.
        let branches: String = (0..20).map(|i| format!("`origin/b{i}` ")).collect();
        let got = check(None, &format!("{branches}`src/gone.rs`"));
        assert_eq!(got.stale_refs, vec!["src/gone.rs".to_string()]);
        let many: String = (0..30).map(|i| format!("`src/f{i}.rs` ")).collect();
        assert_eq!(check(None, &many).stale_refs.len(), MAX_REFS);
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }

    /// D235: a symbol is looked for only in a regular file under the scan
    /// cap. A directory, a FIFO or a file past the cap is present-unverified,
    /// never missing, and the FIFO is never opened (a read would block).
    #[test]
    fn a_symbol_in_a_non_regular_or_oversized_file_is_never_missing() {
        let root = fixture_repo("sym");
        std::fs::create_dir_all(root.join("src/dir.rs")).unwrap();
        let big = root.join("src/big.rs");
        let mut text = "x".repeat(SYMBOL_SCAN_CAP as usize);
        text.push_str("\nfn absent_fn() {}");
        std::fs::write(&big, text).unwrap();
        let rr = RefRoot::at(&root).unwrap();
        let check = |body: &str| verify_refs(None, body, Some("proj"), Some(&rr));
        assert_eq!(check("`src/big.rs::nowhere_fn`").status, RefStatus::Ok);
        assert_eq!(check("`src/dir.rs::nowhere_fn`").status, RefStatus::Ok);
        #[cfg(unix)]
        {
            let made = std::process::Command::new("mkfifo")
                .arg(root.join("src/pipe.rs"))
                .status()
                .is_ok_and(|s| s.success());
            if made {
                assert_eq!(check("`src/pipe.rs::nowhere_fn`").status, RefStatus::Ok);
            }
        }
        // The control: a regular small file without the symbol is missing.
        assert_eq!(
            check("`src/live.rs::nowhere_fn`").status,
            RefStatus::MissingSymbol
        );
        // One root reads each file once, however many docs and symbols cite
        // it: three docs over `src/live.rs` add one read to the one above.
        let before = rr.reads.get();
        for body in [
            "`src/live.rs::kept_fn`",
            "`src/live.rs::kept_fn_two` `src/live.rs::gone_fn`",
            "`src/live.rs::kept_fn`",
        ] {
            check(body);
        }
        assert_eq!(rr.reads.get(), before, "src/live.rs was already read");
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
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
