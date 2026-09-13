//! A minimal YAML-frontmatter splitter for memory docs.
//!
//! Deliberately not a YAML parser: the only shape a memory doc's frontmatter
//! actually carries is scalar `key: value` lines between a leading `---` fence
//! and its close, and a partial parser that silently mis-read a list or a
//! block scalar would be worse than not trying.
//!
//! Shared by `memory.add`/`memory.update`'s storage-time flattening (D135)
//! and the D121 memory browser's body renderer, so a doc's frontmatter is
//! read one way everywhere it is read, rather than reparsed per call site.

/// Split a leading `---\n...\n---\n` block off `body`.
///
/// Returns `(pairs, rest)`: `pairs` is each `key: value` line inside the
/// block, in source order, with both `key` and `value` trimmed and `value`
/// additionally stripped of one wrapping `"` or `'`; `rest` is everything
/// after the closing fence, byte for byte. A line inside the block with no
/// `:` contributes no pair.
///
/// `pairs` is empty and `rest` is `body` unchanged when `body` does not open
/// with a REAL closing fence: a body that merely starts with a horizontal
/// rule is not frontmatter, and reading it as one would eat the whole body
/// hunting a `---` that never comes. Scanning line by line from the top
/// (rather than a substring search) is what makes the fence found the FIRST
/// real one, not a `\n---\n` that happens to sit inside a fenced code block.
pub fn split(body: &str) -> (Vec<(&str, &str)>, &str) {
    let Some(after_open) = body.strip_prefix("---\n") else {
        return (Vec::new(), body);
    };
    let mut consumed = 0;
    for line in after_open.split_inclusive('\n') {
        let trimmed = line.trim_end_matches('\n');
        if trimmed == "---" {
            let fm = &after_open[..consumed];
            let rest = &after_open[consumed + line.len()..];
            let pairs = fm
                .lines()
                .filter_map(|l| l.split_once(':'))
                .map(|(k, v)| (k.trim(), v.trim().trim_matches(['"', '\''])))
                .collect();
            return (pairs, rest);
        }
        consumed += line.len();
    }
    (Vec::new(), body)
}

/// Flatten a leading frontmatter block into plain `key  value` lines —
/// D121(e)'s convention for how the memory browser already reads one — so a
/// doc that opens with a fence reads as prose everywhere its body reaches:
/// a search snippet, `memory show`, `tasqx_get_memory`, the browser preview.
///
/// This CONVERTS the block rather than cutting it, unlike `memory.import`'s
/// unrelated frontmatter cut (#228.4): that importer's corpus is throwaway
/// agent-session metadata (`originSessionId`, `modified`) nobody reads as
/// prose, while a memory doc's `description` or `author` is deliberately
/// authored content worth keeping findable and readable, just not as raw
/// YAML syntax.
///
/// A body with no leading frontmatter block is returned unchanged (as a
/// borrow, so the common case allocates nothing new).
pub fn flatten(body: &str) -> std::borrow::Cow<'_, str> {
    let (pairs, rest) = split(body);
    if pairs.is_empty() {
        return std::borrow::Cow::Borrowed(body);
    }
    let mut out = String::with_capacity(body.len());
    for (k, v) in pairs {
        out.push_str(k);
        out.push_str("  ");
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
        let (pairs, rest) = split(body);
        assert!(pairs.is_empty(), "{pairs:?}");
        assert_eq!(rest, body);
        assert_eq!(flatten(body), body);
    }

    #[test]
    fn a_real_fence_splits_into_pairs_and_rest() {
        let body = "---\ndescription: How an SDK release is cut\n---\nCut the branch.";
        let (pairs, rest) = split(body);
        assert_eq!(pairs, vec![("description", "How an SDK release is cut")]);
        assert_eq!(rest, "Cut the branch.");
    }

    #[test]
    fn a_quoted_value_loses_its_wrapping_quote() {
        let body = "---\nname: \"vh-mcp\"\n---\nBody.";
        let (pairs, _) = split(body);
        assert_eq!(pairs, vec![("name", "vh-mcp")]);
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

    #[test]
    fn a_colonless_line_inside_the_fence_contributes_no_pair() {
        let body = "---\njust text, no colon\nname: deploy\n---\nBody.";
        let (pairs, _) = split(body);
        assert_eq!(pairs, vec![("name", "deploy")]);
    }
}
