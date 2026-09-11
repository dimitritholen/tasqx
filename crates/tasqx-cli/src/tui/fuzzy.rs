//! Fuzzy matching for the screens that filter as you type (D100).
//!
//! `pick` found this, and the memory browser uses it too, so a query ranks
//! the same way on both screens. A query is split on whitespace into terms.
//! Every term must match some field as a subsequence, each term takes its
//! best field, and a field's weight says how much a hit there counts.

/// Added when a term occurs in a name as a whole run rather than as letters
/// spread through it: the word the reader typed beats the same letters inside
/// another word (`tui` in "tasqx-tui-restyle" over `tui` in "punctuation").
///
/// Found on the memory browser (D121) and brought to `pick` by D124, so both
/// screens rank a query by one rule.
pub(crate) const WHOLE_BONUS: i64 = 400;

/// One term against one NAME field (a title, a project, an id, a tag list):
/// a subsequence, plus [`WHOLE_BONUS`] when the term occurs whole. `None`
/// when the term is not a subsequence of the field at all.
pub(crate) fn score_name(field: &str, term: &str) -> Option<i64> {
    let whole = if field.contains(term) { WHOLE_BONUS } else { 0 };
    score_subsequence(field, term).map(|s| s + whole)
}

/// Score one row against already-lowercased `terms`, over lowercased name
/// `fields` with a weight each. `None` when some term matches no field at
/// all, so the row is not a candidate; otherwise higher is better.
///
/// Each term picks its OWN best-scoring field independently: the AND is
/// "every term matches something", and only the order among candidates comes
/// from the score. A row where "api" hits the title and "test" only hits the
/// tags is as valid a candidate as one where both hit the title.
pub(crate) fn score_terms(fields: &[String], weights: &[i64], terms: &[&str]) -> Option<i64> {
    let mut total = 0i64;
    for t in terms {
        let best = fields
            .iter()
            .zip(weights)
            .filter_map(|(f, w)| score_name(f, t).map(|s| s + w))
            .max()?;
        total += best;
    }
    Some(total)
}

/// The candidates among `0..len`, best first. A stable sort, so rows that
/// score the same keep the order the caller handed in, which makes that
/// order the tiebreak for free.
pub(crate) fn rank(len: usize, score: impl Fn(usize) -> Option<i64>) -> Vec<usize> {
    let mut scored: Vec<(usize, i64)> = (0..len).filter_map(|i| score(i).map(|s| (i, s))).collect();
    scored.sort_by_key(|(_, s)| std::cmp::Reverse(*s));
    scored.into_iter().map(|(i, _)| i).collect()
}

/// How well does `needle`'s characters fit inside `haystack`, in order but
/// not necessarily adjacent? Both must already be lowercased. `None` when
/// `needle` is not a subsequence of `haystack` at all; `Some(score)`
/// otherwise, higher for a tighter, earlier-starting run (#203).
///
/// A subsequence rather than a substring, which is what makes the query worth
/// having: `wac` finds "Write API conformance tests" without the user
/// remembering where the word boundaries were. Whitespace in the query splits
/// it into independent terms (see [`score_terms`]), so `api test` is an AND of two
/// subsequence matches and not one match against a literal space — a space is
/// in no field, so a literal reading would make the query unusable the moment
/// the user typed one.
///
/// The match itself is found greedily (earliest possible character each
/// step), which is not always the tightest span a subsequence could occupy —
/// only good enough to RANK matches against each other, which is all a
/// typeahead needs. Existence (does it match at all) is unaffected by the
/// greedy choice; only the score of an already-matching row could, in a rare
/// case, be a little pessimistic.
pub(crate) fn score_subsequence(haystack: &str, needle: &str) -> Option<i64> {
    if needle.is_empty() {
        return Some(0);
    }
    let hay: Vec<char> = haystack.chars().collect();
    let mut cursor = 0usize;
    let mut first = None;
    let mut last = 0usize;
    for c in needle.chars() {
        while cursor < hay.len() && hay[cursor] != c {
            cursor += 1;
        }
        if cursor >= hay.len() {
            return None;
        }
        first.get_or_insert(cursor);
        last = cursor;
        cursor += 1;
    }
    let first = first.unwrap_or(0);
    let needle_len = needle.chars().count() as i64;
    let span = (last - first + 1) as i64;
    // Characters inside the match that the needle did NOT ask for: zero for
    // "api" hitting a literal "api" substring, large for the same three
    // letters scattered across an 80-character title.
    let gaps = span - needle_len;
    Some(1_000 - gaps * 10 - first as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tight_early_run_outscores_a_scattered_late_one() {
        let tight = score_subsequence("api tests", "api").unwrap();
        let loose = score_subsequence("a pretty idea", "api").unwrap();
        assert!(tight > loose, "{tight} <= {loose}");
        assert_eq!(score_subsequence("nothing", "xyz"), None);
    }

    #[test]
    fn every_term_must_match_some_field_and_weights_decide_the_order() {
        let title_hit = ["memory browser".to_string(), "tui".to_string()];
        let tag_hit = ["something else".to_string(), "memory".to_string()];
        let w = [250, 50];
        let a = score_terms(&title_hit, &w, &["memory"]).unwrap();
        let b = score_terms(&tag_hit, &w, &["memory"]).unwrap();
        assert!(a > b, "a title hit must outrank a tag hit: {a} vs {b}");
        assert_eq!(score_terms(&title_hit, &w, &["memory", "zzz"]), None);
    }

    /// D124, from D121: the word the reader typed beats the same letters
    /// inside another word, on `pick` as on the memory browser.
    #[test]
    fn a_term_found_whole_outranks_its_letters_scattered() {
        let w = [250, 250, 50, 50];
        let scattered = [
            "1".to_string(),
            "fix punctuation in emails".to_string(),
            "work".to_string(),
            String::new(),
        ];
        let whole = [
            "2".to_string(),
            "house style".to_string(),
            "tasqx-tui-restyle".to_string(),
            String::new(),
        ];
        let a = score_terms(&scattered, &w, &["tui"]).unwrap();
        let b = score_terms(&whole, &w, &["tui"]).unwrap();
        assert!(b > a, "scattered {a} outranked whole {b}");
    }

    #[test]
    fn ties_keep_the_order_they_came_in() {
        assert_eq!(rank(4, |i| (i != 2).then_some(7)), vec![0, 1, 3]);
    }
}
