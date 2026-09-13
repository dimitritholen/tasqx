//! A minimal YAML-frontmatter reader for memory docs.
//!
//! Deliberately not a YAML parser: the only shape this needs to read is a
//! leading `---`/`---` fence, and a partial parser that silently mis-read a
//! list or a block scalar would be worse than not trying. What it promises
//! instead is narrower and easier to keep true — find the fence (or don't),
//! and never lose a line of it.
//!
//! [`block`] is the one fence-finder: `memory.import`'s CLI-side cut
//! (`crates/tasqx-cli/src/verbs.rs`), the D121 memory browser's body renderer
//! (`crates/tasqx-cli/src/tui/memory.rs`), and `render::doc_summary`'s
//! description lookup all call it (or [`split`], built on it) instead of
//! each scanning for `---\n...\n---\n` by hand — one parser, not four.
//!
//! Task #12/D135: what a doc's `docs.body` HOLDS is not what its search
//! index reads. `body` is never rewritten by this module or its callers —
//! `memory.get`/`memory show`/`store.export` all see exactly what was
//! written, fence and all, and a `store.export`/`store.import` round-trip is
//! still byte-for-byte. [`flatten`] instead produces the text a SEARCH INDEX
//! should read (`docs.search_body`, see `crate::storage::migrate_memory`):
//! the fence's `key: value` lines read as D121(e)'s `key  value` prose, and
//! every OTHER line inside the block — a YAML list item, a folded block
//! scalar's continuation — is kept too, as plain text, so a term that lives
//! only inside a list or a folded paragraph still finds the doc.

/// Find a leading `---` … `---` fence and split `body` into the raw text
/// between the two fence lines and everything after the closing one.
///
/// Accepts `---\n` or `---\r\n` as the opening line, and a closing line whose
/// `trim_end()` is `---` (so a CRLF close matches too) — a Windows-authored
/// file must read exactly like a Unix one; CI runs on Windows. `fm` keeps
/// each line's own trailing `\r`, if any — callers that read it line by line
/// ([`split`]) strip that themselves.
///
/// Returns `None` when `body` does not open with a REAL closing fence: a
/// body that merely starts with a horizontal rule is not frontmatter, and
/// reading it as one would eat the whole body hunting a `---` that never
/// comes. Scanning line by line from the top (rather than a substring
/// search) is what makes the fence found the FIRST real one, not a
/// `\n---\n` that happens to sit inside a fenced code block.
pub fn block(body: &str) -> Option<(&str, &str)> {
    let after_open = body
        .strip_prefix("---\r\n")
        .or_else(|| body.strip_prefix("---\n"))?;
    let mut consumed = 0;
    for line in after_open.split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            let fm = &after_open[..consumed];
            let rest = &after_open[consumed + line.len()..];
            return Some((fm, rest));
        }
        consumed += line.len();
    }
    None
}

/// [`block`], read line by line: each non-blank line inside the fence becomes
/// one entry, in source order. A line with a `:` is `(Some(key), value)`,
/// `key` and `value` trimmed and `value` additionally stripped of one
/// wrapping `"` or `'`; any other non-blank line — a YAML list item
/// (`- deploy`), a folded block scalar's continuation, anything with no
/// colon — is `(None, line)`, trimmed but otherwise kept whole rather than
/// dropped, so [`flatten`] never loses a word of it.
///
/// `pairs` is empty and `rest` is `body` unchanged when `body` does not open
/// with a real fence (see [`block`]).
pub fn split(body: &str) -> (Vec<(Option<&str>, &str)>, &str) {
    let Some((fm, rest)) = block(body) else {
        return (Vec::new(), body);
    };
    let pairs = fm
        .lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.trim().is_empty())
        .map(|l| match l.split_once(':') {
            Some((k, v)) if !k.trim().is_empty() => {
                (Some(k.trim()), v.trim().trim_matches(['"', '\'']))
            }
            _ => (None, l.trim()),
        })
        .collect();
    (pairs, rest)
}

/// Render a leading frontmatter block as D121(e)'s `key  value` prose — the
/// convention the memory browser already reads one in — for a SEARCH INDEX
/// to read, not for `docs.body` to become. See the module docs: this is
/// `crate::storage::migrate_memory`'s `docs.search_body`, computed fresh
/// whenever `body` is written; `body` itself is never touched.
///
/// A line with a key renders `key  value`; a colonless line (list item,
/// continuation) renders as its own line of plain text, kept rather than cut
/// — unlike `memory.import`'s unrelated frontmatter CUT (#228.4), which
/// discards agent-session metadata nobody searches for, this is text a query
/// must still be able to find.
///
/// A body with no leading frontmatter block is returned unchanged (as a
/// borrow, so the common case allocates nothing new). A fence with nothing
/// in it — empty, or blank lines only — is still a fence: it flattens to the
/// text after it, delimiters gone, with no leading blank line.
pub fn flatten(body: &str) -> std::borrow::Cow<'_, str> {
    if block(body).is_none() {
        return std::borrow::Cow::Borrowed(body);
    }
    let (pairs, rest) = split(body);
    if pairs.is_empty() {
        return std::borrow::Cow::Borrowed(rest);
    }
    let mut out = String::with_capacity(body.len());
    for (k, v) in pairs {
        if let Some(k) = k {
            out.push_str(k);
            out.push_str("  ");
        }
        out.push_str(v);
        out.push('\n');
    }
    out.push('\n');
    out.push_str(rest);
    std::borrow::Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_body_with_no_frontmatter_is_untouched() {
        let (pairs, rest) = split("Just prose, no fence.");
        assert!(pairs.is_empty());
        assert_eq!(rest, "Just prose, no fence.");
        assert!(matches!(
            flatten("Just prose, no fence."),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[test]
    fn a_bare_leading_rule_with_no_close_is_not_frontmatter() {
        let body = "---\nThis never closes.";
        assert_eq!(block(body), None);
        let (pairs, rest) = split(body);
        assert!(pairs.is_empty(), "{pairs:?}");
        assert_eq!(rest, body);
        assert_eq!(flatten(body), body);
    }

    #[test]
    fn a_real_fence_splits_into_pairs_and_rest() {
        let body = "---\ndescription: How an SDK release is cut\n---\nCut the branch.";
        let (pairs, rest) = split(body);
        assert_eq!(
            pairs,
            vec![(Some("description"), "How an SDK release is cut")]
        );
        assert_eq!(rest, "Cut the branch.");
    }

    #[test]
    fn a_quoted_value_loses_its_wrapping_quote() {
        let body = "---\nname: \"vh-mcp\"\n---\nBody.";
        let (pairs, _) = split(body);
        assert_eq!(pairs, vec![(Some("name"), "vh-mcp")]);
    }

    #[test]
    fn flatten_turns_the_fence_into_key_two_spaces_value_lines() {
        let body = "---\ndescription: How an SDK release is cut\n---\nCut the branch.";
        assert_eq!(
            flatten(body),
            "description  How an SDK release is cut\n\nCut the branch."
        );
    }

    #[test]
    fn flatten_keeps_every_pair_in_source_order() {
        let body = "---\nname: deploy\nauthor: infra\n---\nBody.";
        assert_eq!(flatten(body), "name  deploy\nauthor  infra\n\nBody.");
    }

    /// The bug a review finding caught: `tags:\n  - deploy\n  - kubernetes`
    /// used to drop every colonless line, so a query for `kubernetes` — a
    /// word that lives only inside a YAML list — could no longer find the
    /// doc. A colonless line now survives as its own value-only entry.
    #[test]
    fn a_colonless_line_inside_the_fence_is_kept_as_a_value_only_entry() {
        let body = "---\ntags:\n  - deploy\n  - kubernetes\nname: deploy\n---\nBody.";
        let (pairs, _) = split(body);
        assert_eq!(
            pairs,
            vec![
                (Some("tags"), ""),
                (None, "- deploy"),
                (None, "- kubernetes"),
                (Some("name"), "deploy"),
            ]
        );
        assert!(
            flatten(body).contains("kubernetes"),
            "a list item's word must survive flatten: {}",
            flatten(body)
        );
    }

    /// A folded block scalar (`summary: >` then indented continuation lines)
    /// is not a single YAML string this parser resolves — but its words are
    /// still prose worth finding, so they must not be discarded either.
    #[test]
    fn a_folded_block_scalars_continuation_lines_are_kept() {
        let body = "---\nsummary: >\n  folded block text\n---\nBody.";
        assert_eq!(flatten(body), "summary  >\nfolded block text\n\nBody.");
    }

    /// An empty fence is still a fence: `block` finds it, so its delimiters
    /// must not survive into the index text just because it held no lines.
    /// Deciding on "no pairs" instead of "no fence" returned the body whole,
    /// `---\n---\n` and all, and the snippet showed the dashes.
    #[test]
    fn an_empty_fence_leaves_no_delimiters_behind() {
        assert_eq!(flatten("---\n---\nBody."), "Body.");
        assert_eq!(flatten("---\r\n---\r\nBody."), "Body.");
    }

    /// `split` drops blank lines, so a fence holding only blank lines has no
    /// pairs either, and the same rule applies.
    #[test]
    fn a_blank_only_fence_leaves_no_delimiters_behind() {
        assert_eq!(flatten("---\n\n  \n---\nBody."), "Body.");
    }

    /// CRLF regression: a Windows-authored file's fence lines end `\r\n`, and
    /// CI runs on Windows.
    #[test]
    fn a_crlf_fence_is_read_the_same_as_a_unix_one() {
        let body = "---\r\nname: deploy\r\n---\r\nBody.";
        let (pairs, rest) = split(body);
        assert_eq!(pairs, vec![(Some("name"), "deploy")]);
        assert_eq!(rest, "Body.");
        assert_eq!(flatten(body), "name  deploy\n\nBody.");
    }

    #[test]
    fn a_crlf_close_with_no_open_crlf_still_matches() {
        // An opening `---\n` (already bare) whose close happens to carry a
        // trailing `\r` (a file only partly normalized) must still close.
        let body = "---\nname: deploy\n---\r\nBody.";
        let (pairs, rest) = split(body);
        assert_eq!(pairs, vec![(Some("name"), "deploy")]);
        assert_eq!(rest, "Body.");
    }
}
