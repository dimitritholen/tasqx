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

#[cfg(test)]
mod tests {
    use super::*;

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
}
