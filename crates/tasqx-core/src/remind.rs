//! Reminder specs (DESIGN.md §9).
//!
//! A task's `remind` field is one canonical string in exactly one of two forms:
//!  * a **signed offset anchored to `due`** — `-1h`, `-30m`, `-2d`, `+15m`.
//!    Negative is *before* due (the overwhelmingly common case); positive is
//!    after. The offset stays symbolic in the store, so moving `due` moves the
//!    reminder with it — that is the whole reason it isn't resolved at set time.
//!  * an **absolute instant** — any [`crate::datetime`] expression (`friday 9am`,
//!    `2026-07-20T17:00`, `tomorrow`) resolved once, at set time, to RFC3339.
//!
//! The sign is what disambiguates the two: a leading `-`/`+` means offset,
//! anything else goes to the one natural-language date parser. Without that rule
//! `3d` would be ambiguous ("3 days before due" vs. `datetime`'s "in 3 days").
//!
//! Determinism, exactly as in `datetime.rs` / `recur.rs`: [`parse_remind`] takes
//! the reference instant explicitly (it needs one only for the absolute branch)
//! and [`parse_spec`] / [`resolve`] need **no clock at all** — the stored form is
//! already canonical, so the scheduler's hot path can never read a hidden clock.

use jiff::Timestamp;

use crate::datetime;
use crate::error::ApiError;

/// A parsed reminder spec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Remind {
    /// Whole seconds relative to the task's `due` (negative = before due).
    Offset(i64),
    /// An absolute RFC3339 (UTC) instant.
    At(String),
}

/// Parse a user-supplied reminder expression relative to `now`.
///
/// `now` is only consulted for the absolute branch (it is handed straight to
/// [`datetime::parse_when`]); offsets are clock-free.
pub fn parse_remind(input: &str, now: Timestamp) -> Result<Remind, ApiError> {
    let raw = input.trim();
    if raw.is_empty() {
        return Err(ApiError::bad_request("empty reminder expression"));
    }
    if raw.starts_with('-') || raw.starts_with('+') {
        return parse_offset(raw).map(Remind::Offset).ok_or_else(|| {
            ApiError::bad_request(format!(
                "could not parse reminder offset: {raw:?} (try e.g. -1h, -30m, \
                 -2d, -1w, or +15m — a signed count plus s/m/h/d/w)"
            ))
        });
    }
    // No sign: an absolute date expression, resolved once through the single
    // natural-language date parser so `remind:` and `due:` accept the same forms.
    Ok(Remind::At(datetime::parse_when(raw, now)?))
}

/// Parse a **stored canonical** spec (as produced by [`spec_to_string`]) with no
/// reference instant. The scheduler runs this on every rebuild, so it must stay
/// clock-free: a canonical offset carries its sign and a canonical absolute is a
/// full RFC3339 instant — neither needs a "now" to be understood.
pub fn parse_spec(stored: &str) -> Option<Remind> {
    let s = stored.trim();
    if s.starts_with('-') || s.starts_with('+') {
        return parse_offset(s).map(Remind::Offset);
    }
    s.parse::<Timestamp>()
        .ok()
        .map(|t| Remind::At(t.to_string()))
}

/// The canonical string form (what actually lands in the `remind` column).
/// Offsets normalize to the largest unit that divides exactly, so `-60m` and
/// `-1h` converge on one representation — mirroring `recur::rule_to_string`.
pub fn spec_to_string(r: &Remind) -> String {
    match r {
        Remind::At(ts) => ts.clone(),
        Remind::Offset(secs) => {
            let sign = if *secs < 0 { '-' } else { '+' };
            let n = secs.abs();
            if n == 0 {
                return "+0s".to_string();
            }
            for (unit_secs, suffix) in [(604_800, 'w'), (86_400, 'd'), (3_600, 'h'), (60, 'm')] {
                if n % unit_secs == 0 {
                    return format!("{sign}{}{suffix}", n / unit_secs);
                }
            }
            format!("{sign}{n}s")
        }
    }
}

/// The instant a reminder should fire, or `None` when it cannot be anchored.
///
/// A relative offset needs a `due` to hang off: a task with `remind:"-1h"` and
/// no `due` has no computable fire time and is simply never scheduled (rather
/// than being an error — clearing `due` must not retroactively break the task).
/// Pure: no clock read, so the scheduler's ripeness check stays deterministic.
pub fn resolve(stored: &str, due: Option<&str>) -> Option<Timestamp> {
    match parse_spec(stored)? {
        Remind::At(ts) => ts.parse::<Timestamp>().ok(),
        Remind::Offset(secs) => {
            let d = due?.parse::<Timestamp>().ok()?;
            Timestamp::from_second(d.as_second().checked_add(secs)?).ok()
        }
    }
}

/// Parse a signed offset token (`-1h`, `+30m`, `-90s`) into whole seconds.
fn parse_offset(tok: &str) -> Option<i64> {
    let (neg, rest) = match tok.as_bytes().first()? {
        b'-' => (true, &tok[1..]),
        b'+' => (false, &tok[1..]),
        _ => return None,
    };
    let rest = rest.trim();
    let split = rest.find(|c: char| !c.is_ascii_digit())?;
    if split == 0 {
        return None;
    }
    let (num, unit) = rest.split_at(split);
    let n: i64 = num.parse().ok()?;
    let mult = match unit.trim().to_ascii_lowercase().as_str() {
        "s" | "sec" | "secs" | "second" | "seconds" => 1,
        "m" | "min" | "mins" | "minute" | "minutes" => 60,
        "h" | "hr" | "hrs" | "hour" | "hours" => 3_600,
        "d" | "day" | "days" => 86_400,
        "w" | "wk" | "week" | "weeks" => 604_800,
        _ => return None,
    };
    let secs = n.checked_mul(mult)?;
    Some(if neg { -secs } else { secs })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixed reference: Wednesday, 2026-07-15T12:00:00Z (same anchor datetime.rs uses).
    fn now() -> Timestamp {
        "2026-07-15T12:00:00Z".parse().unwrap()
    }

    fn p(s: &str) -> Remind {
        parse_remind(s, now()).unwrap()
    }

    #[test]
    fn signed_offsets_parse_to_seconds() {
        assert_eq!(p("-1h"), Remind::Offset(-3_600));
        assert_eq!(p("-30m"), Remind::Offset(-1_800));
        assert_eq!(p("-2d"), Remind::Offset(-172_800));
        assert_eq!(p("-1w"), Remind::Offset(-604_800));
        assert_eq!(p("+15m"), Remind::Offset(900));
        assert_eq!(p("-90s"), Remind::Offset(-90));
        // Long unit spellings and case are tolerated.
        assert_eq!(p("-2 hours"), Remind::Offset(-7_200));
        assert_eq!(p("-1D"), Remind::Offset(-86_400));
    }

    #[test]
    fn unsigned_input_is_an_absolute_date_via_datetime() {
        // Delegates to the one NL date parser, anchored at `now`.
        assert_eq!(
            p("2026-07-20T17:00"),
            Remind::At("2026-07-20T17:00:00Z".into())
        );
        assert_eq!(p("tomorrow"), Remind::At("2026-07-16T00:00:00Z".into()));
        assert_eq!(p("friday 9am"), Remind::At("2026-07-17T09:00:00Z".into()));
    }

    #[test]
    fn canonical_form_normalizes_to_the_largest_exact_unit() {
        assert_eq!(spec_to_string(&Remind::Offset(-3_600)), "-1h");
        assert_eq!(spec_to_string(&Remind::Offset(-1_800)), "-30m");
        assert_eq!(spec_to_string(&Remind::Offset(-172_800)), "-2d");
        assert_eq!(spec_to_string(&Remind::Offset(-604_800)), "-1w");
        assert_eq!(spec_to_string(&Remind::Offset(900)), "+15m");
        // 90s is not a whole minute -> stays in seconds.
        assert_eq!(spec_to_string(&Remind::Offset(-90)), "-90s");
        // `-60m` and `-1h` converge on one stored form.
        assert_eq!(spec_to_string(&p("-60m")), "-1h");
        assert_eq!(spec_to_string(&p("-1h")), "-1h");
    }

    #[test]
    fn canonical_form_round_trips_through_parse_spec() {
        for input in ["-1h", "-30m", "-2d", "-1w", "+15m", "-90s"] {
            let canon = spec_to_string(&p(input));
            assert_eq!(parse_spec(&canon), Some(p(input)), "round trip: {input}");
        }
        let abs = spec_to_string(&p("2026-07-20T17:00"));
        assert_eq!(
            parse_spec(&abs),
            Some(Remind::At("2026-07-20T17:00:00Z".into()))
        );
    }

    #[test]
    fn resolve_offsets_against_due_without_a_clock() {
        let due = Some("2026-07-20T17:00:00Z");
        assert_eq!(
            resolve("-1h", due).unwrap().to_string(),
            "2026-07-20T16:00:00Z"
        );
        assert_eq!(
            resolve("-30m", due).unwrap().to_string(),
            "2026-07-20T16:30:00Z"
        );
        assert_eq!(
            resolve("-2d", due).unwrap().to_string(),
            "2026-07-18T17:00:00Z"
        );
        assert_eq!(
            resolve("+15m", due).unwrap().to_string(),
            "2026-07-20T17:15:00Z"
        );
    }

    #[test]
    fn resolve_absolute_ignores_due() {
        let at = "2026-07-19T08:00:00Z";
        assert_eq!(
            resolve(at, Some("2026-07-20T17:00:00Z"))
                .unwrap()
                .to_string(),
            at
        );
        // An absolute reminder needs no anchor at all.
        assert_eq!(resolve(at, None).unwrap().to_string(), at);
    }

    #[test]
    fn relative_reminder_without_due_is_unanchored_not_an_error() {
        assert_eq!(resolve("-1h", None), None);
    }

    #[test]
    fn bad_input_is_an_error() {
        assert!(parse_remind("", now()).is_err());
        assert!(parse_remind("-1x", now()).is_err()); // unknown unit
        assert!(parse_remind("-h", now()).is_err()); // no count
        assert!(parse_remind("not a date", now()).is_err()); // unsigned -> datetime, rejected
    }

    /// `remind:` is a second entry point into the natural-language date parser
    /// (the unsigned branch routes to `datetime::parse_when`), so it inherits any
    /// panic that lives there. An absurd count must surface as `bad_request` —
    /// this used to panic inside jiff and exit the CLI 101.
    #[test]
    fn an_absurd_unsigned_count_is_rejected_rather_than_panicking() {
        assert!(parse_remind("99999999d", now()).is_err());
        assert!(parse_remind("in 99999999 days", now()).is_err());
        assert!(parse_remind("99999999y", now()).is_err());
    }
}
