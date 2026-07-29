//! The four token buckets, and the one rule both renderers obey (D48a).
//!
//! `engine/reports.rs` keeps `tokens_in`, `tokens_out`, `tokens_cache_read` and
//! `tokens_cache_creation` apart through the entire aggregation — its own comment
//! says why: "cache tokens cost a fraction, so a blended total would lie" — and
//! derives `tokens_total` only at emit. Both presentation layers then picked that
//! derived field up and made it *the* headline number, throwing away the exact
//! care the core took.
//!
//! Measured on this project's own store: `in 136 · out 83 479 · cacheR 13 630 240
//! · cacheW 186 965`. The blend is 13.9 M. Weighted by published relative prices,
//! cache read is 98.1 % of that volume but 67.7 % of the cost, while output is
//! 0.6 % of the volume and 20.7 % of the cost. One number cannot carry a 35×
//! spread in price per token.
//!
//! This module exists so the rule has ONE implementation. The terminal report and
//! the HTML page both need to name a bucket, order the four, and shorten a count;
//! a copy in each is how the two drift into disagreeing about the same store. That
//! is not hypothetical here — `util::duration_secs` was re-forked into `markdown.rs`
//! days ago, narrower and unchecked, and D14's entry already said two copies of a
//! rule is one copy too many.

use serde_json::Value;

/// The four buckets: the `report.summary` key, a short label, and a full one.
///
/// Order is fixed and load-bearing, not alphabetical. It runs cheapest-per-token
/// to dearest, so a reader scanning left to right crosses the price gradient in
/// one direction, and `dominant` breaks ties by it — two buckets at exactly the
/// same count report the cheaper one, which is the reading that cannot flatter.
///
/// Two labels because the surfaces have different budgets, and only opening the
/// page showed it: the terminal has a 12-character cell and needs `cacheR`, while
/// the HTML header had room and rendered `CACHER` — an abbreviation upper-cased
/// into something that reads like a word. A page with room should spell it.
pub const BUCKETS: [(&str, &str, &str); 4] = [
    ("tokens_cache_read", "cacheR", "cache read"),
    ("tokens_cache_creation", "cacheW", "cache write"),
    ("tokens_in", "in", "input"),
    ("tokens_out", "out", "output"),
];

/// The largest bucket in one `report.summary` group, or `None` when the group
/// spent nothing.
///
/// Deliberately NOT "the most expensive bucket": that would need a price list,
/// tasqx has none, and a stale one is worse than none — the same reason D48 bans
/// a currency figure outright. This answers "which bucket is this volume made
/// of", and the label says `cacheR` rather than anything cost-shaped so the
/// reader is never invited to read a price into it.
pub fn dominant(group: &Value) -> Option<(&'static str, i64)> {
    BUCKETS
        .iter()
        .map(|(key, short, _)| (*short, group.get(key).and_then(Value::as_i64).unwrap_or(0)))
        .filter(|(_, n)| *n > 0)
        // `max_by_key` returns the LAST maximum, so iterate reversed to keep the
        // first — which, given BUCKETS' order, is the cheaper of a tied pair.
        .rev()
        .max_by_key(|(_, n)| *n)
}

/// A count shortened to fit a column: `136`, `83.5K`, `13.6M`.
///
/// Truncates rather than rounds. A report is read to decide where time went, and
/// a number that rounds UP across a threshold — 999 999 shown as `1.0M` — reads
/// as more than was spent. Under-stating is the safe direction for a figure
/// nobody can audit from the page itself.
pub fn compact(n: i64) -> String {
    match n {
        n if n < 0 => "-".to_string(),
        n if n < 1_000 => n.to_string(),
        n if n < 1_000_000 => format!("{}.{}K", n / 1_000, (n % 1_000) / 100),
        n if n < 1_000_000_000 => format!("{}.{}M", n / 1_000_000, (n % 1_000_000) / 100_000),
        n => format!("{}.{}B", n / 1_000_000_000, (n % 1_000_000_000) / 100_000_000),
    }
}

/// `dominant` rendered for a fixed-width cell: `cacheR 13.6M`, or `-`.
pub fn dominant_cell(group: &Value) -> String {
    match dominant(group) {
        Some((label, n)) => format!("{label} {}", compact(n)),
        None => "-".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_dominant_bucket_is_the_largest_one_not_the_total() {
        // The real shape from this project's store: the blend would say 13.9M
        // and name nothing. cacheR is 98% of the volume and is what this reports.
        let g = json!({
            "tokens_in": 136, "tokens_out": 83_479,
            "tokens_cache_read": 13_630_240, "tokens_cache_creation": 186_965,
            "tokens_total": 13_900_820
        });
        assert_eq!(dominant(&g), Some(("cacheR", 13_630_240)));
        assert_eq!(dominant_cell(&g), "cacheR 13.6M");
    }

    #[test]
    fn a_group_that_spent_nothing_reports_nothing_rather_than_zero() {
        let g = json!({
            "tokens_in": 0, "tokens_out": 0,
            "tokens_cache_read": 0, "tokens_cache_creation": 0, "tokens_total": 0
        });
        assert_eq!(dominant(&g), None);
        assert_eq!(dominant_cell(&g), "-");
        // A group with no token keys at all — the shape before any attribution
        // ran — must behave the same, not panic and not read as zero-spend.
        assert_eq!(dominant_cell(&json!({ "count": 3 })), "-");
    }

    #[test]
    fn a_tie_reports_the_cheaper_bucket() {
        // Not cosmetic: with the dearer one winning a tie, a page could name
        // `out` on a store whose spend is half cache, which reads as costlier
        // than it was. BUCKETS is ordered cheapest-first and the tie follows it.
        let g = json!({
            "tokens_in": 0, "tokens_out": 500,
            "tokens_cache_read": 500, "tokens_cache_creation": 0
        });
        assert_eq!(dominant(&g), Some(("cacheR", 500)));
    }

    #[test]
    fn compact_truncates_downward_and_never_rounds_across_a_threshold() {
        assert_eq!(compact(0), "0");
        assert_eq!(compact(136), "136");
        assert_eq!(compact(999), "999");
        assert_eq!(compact(1_000), "1.0K");
        assert_eq!(compact(83_479), "83.4K");
        // The one that matters: rounding would print 1.0M for a number that is
        // not a million, overstating spend nobody can audit from the page.
        assert_eq!(compact(999_999), "999.9K");
        assert_eq!(compact(13_630_240), "13.6M");
        // No unit above B, so the ceiling reads long rather than wrong. Ugly is
        // the right trade here: a `T` step would be dead code for any real store,
        // and the number stays honest at any magnitude.
        assert_eq!(compact(i64::MAX), "9223372036.8B");
    }

    #[test]
    fn every_bucket_key_is_one_report_summary_actually_emits() {
        // The labels are ours, but the KEYS are core's wire contract. A typo
        // here would silently read 0 forever, which is exactly the failure the
        // dominant-bucket cell exists to make impossible.
        for (key, _, _) in BUCKETS {
            assert!(
                tasqx_core::engine::SUMMARY_METRICS.contains(&key),
                "{key} is not a metric report.summary emits"
            );
        }
    }
}
