//! Recurrence rules (DESIGN.md §3, §10, §12-D2).
//!
//! The v1 subset (D2) — *not* full RRULE:
//!  * `every N days|weeks|months` (and the singular `every day`/`every week`/…);
//!  * `weekly on <days>` — e.g. `weekly on mon,wed,fri`;
//!  * `monthly on day <D>` — e.g. `monthly on day 15`;
//!  * `monthly on the <Nth> <weekday>` — e.g. `monthly on the 2nd tuesday`,
//!    `monthly on the last friday`.
//!
//! A recurring task is a **template**: completing an instance spawns the next
//! instance with its date advanced by the rule. Per D2, **missed occurrences
//! collapse to a single catch-up** — [`next_after`] advances at least once and
//! then skips every slot at or before `now`, so a machine that was off for a
//! week yields exactly one future instance, never a backfill storm.
//!
//! Parsing takes an explicit reference only where it needs one (it does not);
//! [`next_after`] takes both the current anchor instant and `now` explicitly, so
//! it is deterministic and unit-testable.
//!
//! **Month-end semantics differ by rule kind, by design:**
//!  * `monthly on day D` re-clamps against the stored target day `D` every step,
//!    so a day-31 rule recovers the 31st in long months (Jan31 → Feb28 → Mar31).
//!  * interval `every N months` advances from the *previous* (possibly clamped)
//!    date via calendar arithmetic, so it drifts once clamped and stays there
//!    (Jan31 → Feb28 → Mar28 → …). This matches "every month, same slot" interval
//!    semantics; pick `monthly on day 31` when you want the month-end to stick.

use jiff::civil::{Date, DateTime, Time, Weekday};
use jiff::tz::TimeZone;
use jiff::{Span, Timestamp};

use crate::error::ApiError;

/// A parsed recurrence rule (the D2 subset).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recur {
    /// `every N days` (N ≥ 1).
    EveryDays(i64),
    /// `every N weeks`.
    EveryWeeks(i64),
    /// `every N months`.
    EveryMonths(i64),
    /// `weekly on mon,wed,fri` — one or more weekdays, sorted & de-duplicated.
    WeeklyOn(Vec<Weekday>),
    /// `monthly on day D` (1..=31, clamped to the month's length when applied).
    MonthlyOnDay(i8),
    /// `monthly on the Nth weekday` — nth is 1..=5, or -1 for "last".
    MonthlyNthWeekday(i8, Weekday),
}

/// Parse a rule string. Case-insensitive; extra whitespace tolerated. Returns a
/// `bad_request` with a helpful message on anything outside the D2 subset.
pub fn parse_rule(input: &str) -> Result<Recur, ApiError> {
    let s = input.trim().to_ascii_lowercase();
    if s.is_empty() {
        return Err(ApiError::bad_request("empty recurrence rule"));
    }
    let tokens: Vec<&str> = s.split_whitespace().collect();

    let err = || {
        ApiError::bad_request(format!(
            "unrecognized recurrence rule: {input:?} (supported: \
             \"every N days|weeks|months\", \"weekly on mon,wed,fri\", \
             \"monthly on day D\", \"monthly on the Nth weekday\")"
        ))
    };

    match tokens.as_slice() {
        // every N <unit>  /  every <unit>
        ["every", rest @ ..] => {
            let (n, unit) = match rest {
                [unit] => (1i64, *unit),
                [n, unit] => (n.parse::<i64>().map_err(|_| err())?, *unit),
                // also accept a glued short form like `every 3d`
                _ => return Err(err()),
            };
            if n < 1 {
                return Err(ApiError::bad_request("recurrence interval must be >= 1"));
            }
            match unit {
                "d" | "day" | "days" => Ok(Recur::EveryDays(n)),
                "w" | "wk" | "week" | "weeks" => Ok(Recur::EveryWeeks(n)),
                "mo" | "month" | "months" => Ok(Recur::EveryMonths(n)),
                other => {
                    // `every 3d` style: unit glued to the count in a single token.
                    if let Some((gn, gu)) = split_glued(other) {
                        let n = gn;
                        return match gu {
                            "d" | "day" | "days" => Ok(Recur::EveryDays(n)),
                            "w" | "wk" | "week" | "weeks" => Ok(Recur::EveryWeeks(n)),
                            "mo" | "month" | "months" => Ok(Recur::EveryMonths(n)),
                            _ => Err(err()),
                        };
                    }
                    Err(err())
                }
            }
        }
        // weekly on mon,wed,fri
        ["weekly", "on", days @ ..] => {
            let joined = days.join("");
            let mut wds = Vec::new();
            for part in joined.split(',') {
                let part = part.trim();
                if part.is_empty() {
                    continue;
                }
                wds.push(weekday(part).ok_or_else(err)?);
            }
            if wds.is_empty() {
                return Err(err());
            }
            wds.sort_by_key(|w| w.to_monday_one_offset());
            wds.dedup();
            Ok(Recur::WeeklyOn(wds))
        }
        // monthly on day D
        ["monthly", "on", "day", d] => {
            let d: i8 = d.parse().map_err(|_| err())?;
            if !(1..=31).contains(&d) {
                return Err(ApiError::bad_request("monthly day must be 1..=31"));
            }
            Ok(Recur::MonthlyOnDay(d))
        }
        // monthly on the Nth weekday
        ["monthly", "on", "the", nth, wd] => {
            let n = parse_nth(nth).ok_or_else(err)?;
            let w = weekday(wd).ok_or_else(err)?;
            Ok(Recur::MonthlyNthWeekday(n, w))
        }
        _ => Err(err()),
    }
}

/// Split a glued offset token such as `3d` / `2wk` / `1mo` into (count, unit).
fn split_glued(tok: &str) -> Option<(i64, &str)> {
    let split = tok.find(|c: char| !c.is_ascii_digit())?;
    if split == 0 {
        return None;
    }
    let (num, unit) = tok.split_at(split);
    Some((num.parse().ok()?, unit))
}

/// Canonical, normalized string form (what gets stored & displayed).
pub fn rule_to_string(r: &Recur) -> String {
    match r {
        Recur::EveryDays(1) => "every day".to_string(),
        Recur::EveryWeeks(1) => "every week".to_string(),
        Recur::EveryMonths(1) => "every month".to_string(),
        Recur::EveryDays(n) => format!("every {n} days"),
        Recur::EveryWeeks(n) => format!("every {n} weeks"),
        Recur::EveryMonths(n) => format!("every {n} months"),
        Recur::WeeklyOn(days) => {
            let names: Vec<&str> = days.iter().map(|w| weekday_abbr(*w)).collect();
            format!("weekly on {}", names.join(","))
        }
        Recur::MonthlyOnDay(d) => format!("monthly on day {d}"),
        Recur::MonthlyNthWeekday(n, w) => {
            let nth = if *n == -1 {
                "last".to_string()
            } else {
                nth_label(*n)
            };
            format!("monthly on the {nth} {}", weekday_name(*w))
        }
    }
}

/// The next occurrence strictly after the current anchor, collapsing any missed
/// slots at or before `now` (D2). `anchor` is the current instance's date; `now`
/// is the reference instant. Time-of-day of the anchor is preserved.
pub fn next_after(rule: &Recur, anchor: Timestamp, now: Timestamp) -> Result<Timestamp, ApiError> {
    let z = anchor.to_zoned(TimeZone::UTC);
    let time = z.time();
    let mut date = z.date();

    // Advance at least once past the current anchor.
    date = advance_once(rule, date)?;
    let mut ts = to_ts(date, time)?;

    // Collapse missed occurrences: skip any slot at or before `now`.
    let mut guard = 0;
    while ts <= now {
        date = advance_once(rule, date)?;
        ts = to_ts(date, time)?;
        guard += 1;
        if guard > 100_000 {
            return Err(ApiError::internal("recurrence advance did not converge"));
        }
    }
    Ok(ts)
}

/// One step of the rule: the next date strictly after `date`.
fn advance_once(rule: &Recur, date: Date) -> Result<Date, ApiError> {
    let next = match rule {
        Recur::EveryDays(n) => date.checked_add(Span::new().days(*n)),
        Recur::EveryWeeks(n) => date.checked_add(Span::new().weeks(*n)),
        Recur::EveryMonths(n) => date.checked_add(Span::new().months(*n)),
        Recur::WeeklyOn(days) => Ok(next_in_weekdays(date, days)),
        Recur::MonthlyOnDay(d) => Ok(next_month_day(date, *d)),
        Recur::MonthlyNthWeekday(n, w) => next_month_nth_weekday(date, *n, *w),
    };
    next.map_err(|e| ApiError::bad_request(format!("recurrence date overflow: {e}")))
}

/// The soonest date strictly after `date` whose weekday is in `days`.
fn next_in_weekdays(date: Date, days: &[Weekday]) -> Date {
    let cur = date.weekday().to_monday_one_offset() as i64;
    let mut best = 8i64;
    for w in days {
        let tgt = w.to_monday_one_offset() as i64;
        let mut delta = (tgt - cur).rem_euclid(7);
        if delta == 0 {
            delta = 7; // strictly after
        }
        best = best.min(delta);
    }
    date.checked_add(Span::new().days(best)).unwrap_or(date)
}

/// Day `d` of the next month strictly after `date` (clamped to month length).
fn next_month_day(date: Date, d: i8) -> Date {
    let next_month = date.checked_add(Span::new().months(1)).unwrap_or(date);
    clamp_day(next_month, d)
}

/// Set `date`'s day to `d`, clamped to the last day of its month.
fn clamp_day(date: Date, d: i8) -> Date {
    let last = date.last_of_month().day();
    let day = d.min(last).max(1);
    Date::new(date.year(), date.month(), day).unwrap_or(date)
}

/// The nth `weekday` of the first month strictly after `date` that has one.
///
/// `nth` is 1..=5, or -1 for "last". For `nth == 5` many months have no 5th
/// occurrence of a given weekday (≈8 of 12 months); rather than erroring — which
/// would abort the whole completion (D2 says advancement must always yield the
/// next future slot) — we skip forward to the next month that *does* have a 5th
/// occurrence. `nth` in 1..=4 and -1 exist in every month, so the loop returns
/// on the first iteration for them.
fn next_month_nth_weekday(date: Date, n: i8, w: Weekday) -> Result<Date, jiff::Error> {
    let mut month = date.checked_add(Span::new().months(1))?;
    // A 5th weekday recurs at least a few times a year, so a small bound covers
    // every real case; the guard only prevents an unbounded loop on a surprise.
    for _ in 0..60 {
        if let Ok(d) = month.nth_weekday_of_month(n, w) {
            return Ok(d);
        }
        month = month.checked_add(Span::new().months(1))?;
    }
    // Unreachable for valid n (checked at parse time); surface the real error.
    month.nth_weekday_of_month(n, w)
}

/// Combine a civil date and time into a UTC timestamp.
fn to_ts(date: Date, time: Time) -> Result<Timestamp, ApiError> {
    Ok(DateTime::from_parts(date, time)
        .to_zoned(TimeZone::UTC)
        .map_err(|e| ApiError::internal(format!("recurrence datetime: {e}")))?
        .timestamp())
}

fn weekday(s: &str) -> Option<Weekday> {
    Some(match s {
        "monday" | "mon" => Weekday::Monday,
        "tuesday" | "tue" | "tues" => Weekday::Tuesday,
        "wednesday" | "wed" => Weekday::Wednesday,
        "thursday" | "thu" | "thur" | "thurs" => Weekday::Thursday,
        "friday" | "fri" => Weekday::Friday,
        "saturday" | "sat" => Weekday::Saturday,
        "sunday" | "sun" => Weekday::Sunday,
        _ => return None,
    })
}

fn weekday_name(w: Weekday) -> &'static str {
    match w {
        Weekday::Monday => "monday",
        Weekday::Tuesday => "tuesday",
        Weekday::Wednesday => "wednesday",
        Weekday::Thursday => "thursday",
        Weekday::Friday => "friday",
        Weekday::Saturday => "saturday",
        Weekday::Sunday => "sunday",
    }
}

fn weekday_abbr(w: Weekday) -> &'static str {
    match w {
        Weekday::Monday => "mon",
        Weekday::Tuesday => "tue",
        Weekday::Wednesday => "wed",
        Weekday::Thursday => "thu",
        Weekday::Friday => "fri",
        Weekday::Saturday => "sat",
        Weekday::Sunday => "sun",
    }
}

/// Parse an ordinal: `1st`/`first`..`5th`/`fifth`, or `last`.
fn parse_nth(s: &str) -> Option<i8> {
    Some(match s {
        "1st" | "first" | "1" => 1,
        "2nd" | "second" | "2" => 2,
        "3rd" | "third" | "3" => 3,
        "4th" | "fourth" | "4" => 4,
        "5th" | "fifth" | "5" => 5,
        "last" => -1,
        _ => return None,
    })
}

fn nth_label(n: i8) -> String {
    match n {
        1 => "1st",
        2 => "2nd",
        3 => "3rd",
        4 => "4th",
        5 => "5th",
        _ => "1st",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    #[test]
    fn parses_every_forms() {
        assert_eq!(parse_rule("every 3 days").unwrap(), Recur::EveryDays(3));
        assert_eq!(parse_rule("every 2 weeks").unwrap(), Recur::EveryWeeks(2));
        assert_eq!(parse_rule("every 1 months").unwrap(), Recur::EveryMonths(1));
        assert_eq!(parse_rule("every day").unwrap(), Recur::EveryDays(1));
        assert_eq!(parse_rule("every week").unwrap(), Recur::EveryWeeks(1));
        assert_eq!(parse_rule("EVERY 3 DAYS").unwrap(), Recur::EveryDays(3));
        assert_eq!(parse_rule("every 3d").unwrap(), Recur::EveryDays(3));
    }

    #[test]
    fn parses_weekly_and_monthly() {
        assert_eq!(
            parse_rule("weekly on mon,wed,fri").unwrap(),
            Recur::WeeklyOn(vec![Weekday::Monday, Weekday::Wednesday, Weekday::Friday])
        );
        assert_eq!(
            parse_rule("monthly on day 15").unwrap(),
            Recur::MonthlyOnDay(15)
        );
        assert_eq!(
            parse_rule("monthly on the 2nd tuesday").unwrap(),
            Recur::MonthlyNthWeekday(2, Weekday::Tuesday)
        );
        assert_eq!(
            parse_rule("monthly on the last friday").unwrap(),
            Recur::MonthlyNthWeekday(-1, Weekday::Friday)
        );
    }

    #[test]
    fn rejects_bad_rules() {
        assert!(parse_rule("").is_err());
        assert!(parse_rule("every 0 days").is_err());
        assert!(parse_rule("every 3 fortnights").is_err());
        assert!(parse_rule("weekly on blursday").is_err());
        assert!(parse_rule("monthly on day 40").is_err());
        assert!(parse_rule("hourly").is_err());
    }

    #[test]
    fn every_n_days_advances_one_step_on_time() {
        // anchor Jul 1, now Jul 1 (on-time completion) -> Jul 4.
        let r = Recur::EveryDays(3);
        let next = next_after(&r, ts("2026-07-01T09:00:00Z"), ts("2026-07-01T09:00:00Z")).unwrap();
        assert_eq!(next.to_string(), "2026-07-04T09:00:00Z");
    }

    #[test]
    fn missed_occurrences_collapse_to_one() {
        // anchor Jul 1, now Jul 20 (machine was off). Sequence 1,4,7,10,13,16,19,22
        // -> first slot strictly after Jul 20 is Jul 22. Exactly one instance.
        let r = Recur::EveryDays(3);
        let next = next_after(&r, ts("2026-07-01T09:00:00Z"), ts("2026-07-20T12:00:00Z")).unwrap();
        assert_eq!(next.to_string(), "2026-07-22T09:00:00Z");
    }

    #[test]
    fn weekly_on_days_picks_right_next_day() {
        // anchor is Mon 2026-07-13 (weekly on mon,wed,fri), on-time -> Wed 15th.
        let r = parse_rule("weekly on mon,wed,fri").unwrap();
        let next = next_after(&r, ts("2026-07-13T08:00:00Z"), ts("2026-07-13T08:00:00Z")).unwrap();
        assert_eq!(next.to_string(), "2026-07-15T08:00:00Z");
        // From Fri 17th -> Mon 20th.
        let next = next_after(&r, ts("2026-07-17T08:00:00Z"), ts("2026-07-17T08:00:00Z")).unwrap();
        assert_eq!(next.to_string(), "2026-07-20T08:00:00Z");
    }

    #[test]
    fn monthly_on_day_advances() {
        let r = Recur::MonthlyOnDay(15);
        let next = next_after(&r, ts("2026-07-15T09:00:00Z"), ts("2026-07-15T09:00:00Z")).unwrap();
        assert_eq!(next.to_string(), "2026-08-15T09:00:00Z");
        // Day clamps to month length: Jan 31 monthly-on-day-31 -> Feb 28.
        let r = Recur::MonthlyOnDay(31);
        let next = next_after(&r, ts("2026-01-31T09:00:00Z"), ts("2026-01-31T09:00:00Z")).unwrap();
        assert_eq!(next.to_string(), "2026-02-28T09:00:00Z");
    }

    #[test]
    fn monthly_nth_weekday_advances() {
        // 2nd Tuesday. Anchor 2026-07-14 (2nd Tue of Jul) -> 2026-08-11.
        let r = Recur::MonthlyNthWeekday(2, Weekday::Tuesday);
        let next = next_after(&r, ts("2026-07-14T09:00:00Z"), ts("2026-07-14T09:00:00Z")).unwrap();
        assert_eq!(next.to_string(), "2026-08-11T09:00:00Z");
    }

    #[test]
    fn monthly_5th_weekday_skips_months_without_one() {
        // "monthly on the 5th friday". Anchor is the 5th Friday of Jan 2027
        // (2027-01-29). Feb, Mar, Apr 2027 have only 4 Fridays; the next month
        // with a 5th Friday is Apr 2027? Check: the next 5th Friday after Jan is
        // 2027-04-30 (Apr 2027 Fridays: 2,9,16,23,30). It must NOT error.
        let r = Recur::MonthlyNthWeekday(5, Weekday::Friday);
        let next = next_after(&r, ts("2027-01-29T09:00:00Z"), ts("2027-01-29T09:00:00Z")).unwrap();
        assert_eq!(next.to_string(), "2027-04-30T09:00:00Z");
        // And it is genuinely a Friday.
        let wd = next.to_zoned(TimeZone::UTC).date().weekday();
        assert_eq!(wd, Weekday::Friday);
    }

    #[test]
    fn every_months_drifts_after_short_month_clamp() {
        // Pinned decision: interval `every month` drifts to the 28th once clamped
        // and does not recover (contrast monthly_on_day_advances' day-31 case).
        let r = Recur::EveryMonths(1);
        // Jan 31 -> Feb 28 (clamped).
        let feb = next_after(&r, ts("2027-01-31T09:00:00Z"), ts("2027-01-31T09:00:00Z")).unwrap();
        assert_eq!(feb.to_string(), "2027-02-28T09:00:00Z");
        // Feb 28 -> Mar 28 (stays on the 28th; does NOT jump back to the 31st).
        let mar = next_after(&r, feb, feb).unwrap();
        assert_eq!(mar.to_string(), "2027-03-28T09:00:00Z");
    }

    #[test]
    fn round_trips_through_string() {
        for s in [
            "every 3 days",
            "every week",
            "weekly on mon,wed,fri",
            "monthly on day 15",
            "monthly on the 2nd tuesday",
            "monthly on the last friday",
        ] {
            let r = parse_rule(s).unwrap();
            let round = parse_rule(&rule_to_string(&r)).unwrap();
            assert_eq!(r, round, "round-trip failed for {s}");
        }
    }
}
