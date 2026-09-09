//! Natural-language date parsing (DESIGN.md §5: "forgiving by default",
//! `jiff`-parsed dates). One pure function turns a human date expression plus an
//! explicit reference instant into an RFC3339 timestamp, or a `bad_request`.
//!
//! Determinism is a hard requirement: [`parse_when`] never reads the wall clock
//! — the caller passes `now` (the CLI passes the real `util::now`, tests pass a
//! fixed anchor). The one environmental read left is the zone an explicit CLOCK
//! TIME with no offset of its own is interpreted in (DESIGN.md:458): `due:17:00`
//! on an Amsterdam box means 17:00 Amsterdam time, converted to UTC for storage
//! — not 17:00 UTC, which is what it silently stored before (#138). A DATE with
//! no time carries nothing to localize and stays at midnight **UTC** regardless
//! of the machine's zone: D-1511's "days are UTC days" is unchanged, and
//! grouping by a local day would file `--due 2026-08-05` under the 4th for
//! anyone west of Greenwich. [`parse_when`] reads [`TimeZone::system`] exactly
//! once, at its own boundary; the crate-private `parse_when_zoned` underneath
//! it takes the zone as a parameter instead, so this module's own suite (and
//! [`crate::remind`]'s) can pin it and never depend on the machine they happen
//! to run on.
//!
//! Accepted forms (all case-insensitive):
//!  * absolute — `2026-07-20`, `2026-07-20T17:00`, `2026-07-20 17:00`, and any
//!    full RFC3339 (`2026-07-20T17:00:00+02:00`). A bare date is UTC midnight; a
//!    date **with** a time resolves in the machine's zone (see above);
//!  * relative words — `today`, `tomorrow`, `yesterday` (calendar days, always
//!    UTC-anchored); `now`, which unlike the other three is a full INSTANT, not
//!    a day — `due.before:now` is the filter DSL's only overdue query, and
//!    aliasing it to midnight (as it used to) meant it could never see a task
//!    due earlier the same day (#144);
//!  * weekdays — `monday`..`sunday` / `mon`..`sun` (the next such weekday; today
//!    resolves to +7). `this`/`last` are refused, and so is a leading `next`
//!    (#137): it used to be a silent synonym for the bare weekday — tested, but
//!    documented nowhere a user would read it — which is worse than refusing,
//!    because "next friday" reads to most people as a DIFFERENT day than
//!    "friday", not the same one;
//!  * offsets — `in 3 days`, `in 2 weeks`, `in 1 month`, and the short `3d`,
//!    `2w`, `1mo`, `1y`, each optionally signed (`+3d`, `-1d` = yesterday);
//!  * `eom` / `end of month`, `eow` / `end of week` (ISO week ends Sunday);
//!  * an optional trailing time on any of the above — `friday 17:00`,
//!    `tomorrow 9am`, `monday 5pm` — resolved in the machine's zone, INCLUDING
//!    which calendar day `friday`/`tomorrow`/a bare time itself means: near a
//!    UTC-day boundary, "today" in Auckland and "today" in UTC can already be
//!    different days at the same instant;
//!  * an optional *leading* filler word — `at 6pm`, `on friday`, `by monday
//!    5pm`. Only leading fillers are ignored, so `at fridya` stays an error
//!    rather than quietly resolving to a date nobody asked for.

use jiff::civil::{Date, DateTime, Time, Weekday};
use jiff::tz::TimeZone;
use jiff::{Span, Timestamp};

use crate::error::ApiError;

/// Leading words a human types that carry no date meaning: `--due "at 6pm"` and
/// `--due "on friday"` mean exactly what `6pm` and `friday` mean.
const FILLERS: &[&str] = &["at", "on", "by", "@", "due"];

/// Parse `input` relative to `now`, returning an RFC3339 (UTC, `…Z`) string.
///
/// `now` is explicit so the function is deterministic and unit-testable; it is
/// never read from the system clock here. The one thing this entry point DOES
/// read from the environment is [`TimeZone::system`] — consulted only when
/// `input` carries an explicit clock time with no offset of its own (see the
/// module docs and `parse_when_zoned`).
pub fn parse_when(input: &str, now: Timestamp) -> Result<String, ApiError> {
    parse_when_zoned(input, now, &TimeZone::system())
}

/// [`parse_when`], with the zone a naive clock time resolves in taken as a
/// parameter instead of read from the machine. Kept `pub(crate)` — not part of
/// the public API — purely so [`crate::remind`] and this module's own suite can
/// pin a zone and stay deterministic; every other caller wants the real
/// machine zone and goes through [`parse_when`].
pub(crate) fn parse_when_zoned(
    input: &str,
    now: Timestamp,
    tz: &TimeZone,
) -> Result<String, ApiError> {
    let raw = input.trim();
    if raw.is_empty() {
        return Err(ApiError::bad_request("empty date expression"));
    }

    // 1. A full RFC3339 with offset/Z: the instant is unambiguous — normalize.
    if let Ok(ts) = raw.parse::<Timestamp>() {
        return Ok(ts.to_string());
    }

    // 2. A naive datetime (`2026-07-20T17:00[:SS]`, or with a space) — an
    //    explicit clock time with no offset of its own, so it resolves in
    //    `tz` (DESIGN.md:458), not UTC. Gated on a literal `:` in `raw`: a
    //    bare date has none, but `jiff`'s `DateTime` parser is lenient enough
    //    to accept one anyway (defaulting the time to midnight) — without
    //    this gate a bare `2026-07-20` would be caught HERE and localized,
    //    silently reopening the D-1511 hole branch 3 exists to close.
    if raw.contains(':') {
        let norm = raw.replacen(' ', "T", 1);
        for cand in [norm.clone(), format!("{norm}:00")] {
            if let Ok(dt) = cand.parse::<DateTime>() {
                return finish(dt, raw, tz);
            }
        }
    }
    // 3. A bare ISO date → that day at 00:00 UTC, regardless of `tz`: a date
    //    with no time carries nothing to localize (D-1511).
    if let Ok(d) = raw.parse::<Date>() {
        return finish(DateTime::from_parts(d, midnight()), raw, &TimeZone::UTC);
    }

    // 4. Keyword / relative grammar. Work lowercased and tokenized.
    let lower = raw.to_ascii_lowercase();
    let mut tokens: Vec<&str> = lower.split_whitespace().collect();

    // Peel off a trailing time token (`… 17:00`, `… 9am`).
    let mut time: Option<Time> = None;
    if let Some(last) = tokens.last() {
        if is_time_token(last) {
            if let Some(t) = parse_clock(last) {
                time = Some(t);
                tokens.pop();
            }
        }
    }

    // Peel leading filler words (`at 6pm`, `on friday`, `by monday 5pm`). These
    // carry no meaning, so a grammar that is "forgiving by default" must not
    // trip over them — but the forgiveness is LEADING-only and deliberately so:
    // `at fridya` still has to fail rather than silently resolve to something
    // the user never typed.
    while let Some(first) = tokens.first() {
        if FILLERS.contains(first) {
            tokens.remove(0);
        } else {
            break;
        }
    }

    // `now` is the one keyword that answers with the exact instant rather than
    // a calendar day, so it is handled before `today`/`resolve_date` ever
    // reduce anything to a `Date` — there is no `Date` that would represent it
    // (#144). No trailing time combines with it: `now 17:00` is not a spelling
    // this grammar models, so it falls through to the ordinary keyword path
    // below and is refused like any other unmodelled token.
    if tokens == ["now"] && time.is_none() {
        return Ok(now.to_string());
    }

    // A trailing time anchors the WHOLE expression in `tz`; a bare date/
    // weekday/relative-word with no time of its own stays UTC-anchored
    // (D-1511). These can legitimately be different calendar days at once —
    // near a UTC-day boundary a positive-offset `tz` is already tomorrow.
    let zone = if time.is_some() { tz } else { &TimeZone::UTC };
    let today = now.to_zoned(zone.clone()).date();
    // Empty tokens now has two causes: a clock was peeled (`6pm` — a real bare
    // time), or the input was filler and nothing else (`at`). Only the first is
    // a date; the second is a bad request, not "today at midnight".
    let bare_time = tokens.is_empty();
    if bare_time && time.is_none() {
        return Err(unparseable(raw));
    }

    let date = if bare_time {
        today
    } else {
        resolve_date(&tokens, today).ok_or_else(|| unparseable(raw))?
    };

    let t = time.unwrap_or_else(midnight);
    let out = finish(DateTime::from_parts(date, t), raw, zone)?;

    // A bare time already past today rolls forward to tomorrow, in the same
    // zone the time itself was read in.
    if bare_time {
        let dt = DateTime::from_parts(date, t);
        // The same conversion `finish` makes, through the same helper, so the
        // same failure cannot acquire a second wording here.
        let ts = to_instant(dt, raw, zone)?;
        if ts <= now {
            let tomorrow = date.tomorrow().map_err(|_| out_of_range(raw))?;
            return finish(DateTime::from_parts(tomorrow, t), raw, zone);
        }
    }

    Ok(out)
}

/// The one refusal for a date expression this grammar cannot read. It is a
/// function because it is reachable from two places, and two copies of a message
/// carrying an examples list are two things to keep in step.
fn unparseable(raw: &str) -> ApiError {
    ApiError::bad_request(format!(
        "could not parse date: {raw:?} (try e.g. tomorrow, friday, \
         2026-07-20, \"in 3 days\", eom, or 2026-07-20T17:00)"
    ))
}

/// The refusal for a date this grammar read PERFECTLY WELL and cannot store.
///
/// It used to be `jiff`'s own words — "parameter 'Unix timestamp seconds' is not
/// in the required range of -377705023201..=253402207200" — which named neither
/// the value the user typed nor anything they could type instead. Every other
/// rejection in this tool names the offending value and the way out, so this one
/// does too, in the shape [`unparseable`] uses.
///
/// The bounds are DERIVED from `jiff`'s own limits rather than typed in, so the
/// number in the message cannot drift away from the number being enforced when
/// the crate moves. They print as civil UTC datetimes because that is the
/// spelling the user typed in, not the epoch seconds the library counts in.
fn out_of_range(raw: &str) -> ApiError {
    let lo = Timestamp::MIN.to_zoned(TimeZone::UTC).datetime();
    // Truncated to the second: `…T22:00:00.999999999` is noise in a hint, and
    // dropping the fraction moves the stated ceiling DOWN, so everything the
    // message says is storable really is. (The floor carries no fraction, and
    // truncating it would move it the wrong way, so it is left alone.)
    let hi = to_the_second(Timestamp::MAX.to_zoned(TimeZone::UTC).datetime());
    ApiError::bad_request(format!(
        "date out of range: {raw:?} (tasqx stores instants from {lo} to {hi} UTC; \
         try e.g. 2026-07-20 or 2026-07-20T17:00)"
    ))
}

/// `dt` with its sub-second part dropped.
fn to_the_second(dt: DateTime) -> DateTime {
    let t = Time::new(dt.hour(), dt.minute(), dt.second(), 0)
        .expect("the components came from a Time that already validated");
    DateTime::from_parts(dt.date(), t)
}

/// The one civil-to-instant conversion, so its one failure has one message.
///
/// `to_zoned` uses "compatible" disambiguation regardless of `tz` — a spring-
/// forward gap shifts forward, a fall-back fold picks the earlier instant —
/// which is what makes the error worth trusting without inspecting it: it
/// never fires for an ordinary DST transition, only "outside the representable
/// instant range". Reading the library's error text to find that out would be
/// the same leak this function exists to close, pointed the other way.
fn to_instant(dt: DateTime, raw: &str, tz: &TimeZone) -> Result<Timestamp, ApiError> {
    Ok(dt
        .to_zoned(tz.clone())
        .map_err(|_| out_of_range(raw))?
        .timestamp())
}

/// Convert a naive civil datetime, read in `tz`, to an RFC3339 (UTC) string.
fn finish(dt: DateTime, raw: &str, tz: &TimeZone) -> Result<String, ApiError> {
    Ok(to_instant(dt, raw, tz)?.to_string())
}

fn midnight() -> Time {
    Time::new(0, 0, 0, 0).expect("midnight is a valid time")
}

/// Resolve the date portion (no time) from the keyword tokens.
///
/// `now` is deliberately absent: it is a full instant, not a `Date`, and is
/// handled by its own early return in [`parse_when_zoned`] before this
/// function is ever called (#144). `next <weekday>` is absent too — it used to
/// alias the bare weekday here and is refused now, on the same terms as
/// `this`/`last` below (#137).
fn resolve_date(tokens: &[&str], today: Date) -> Option<Date> {
    match tokens {
        ["today"] => Some(today),
        ["tomorrow"] | ["tmr"] => today.tomorrow().ok(),
        ["yesterday"] => today.yesterday().ok(),
        ["eom"] => Some(today.last_of_month()),
        ["end", "of", "month"] => Some(today.last_of_month()),
        ["eow"] => Some(end_of_week(today)),
        ["end", "of", "week"] => Some(end_of_week(today)),
        // `in N <unit>`
        ["in", n, unit] => {
            let n: i64 = n.parse().ok()?;
            add_units(today, n, unit)
        }
        // A single token: a weekday, a short offset (`3d`), or an ISO date.
        [one] => {
            if let Some(w) = weekday(one) {
                return Some(next_weekday(today, w));
            }
            if let Some(d) = short_offset(one, today) {
                return Some(d);
            }
            one.parse::<Date>().ok()
        }
        _ => None,
    }
}

/// Add `n` of a calendar `unit` to `date` (days/weeks/months/years, sing/plur).
///
/// The **fallible** `try_*` span builders are load-bearing: the plain `days`/
/// `weeks`/`months`/`years` builders PANIC on an out-of-range count, so
/// `1e8 days` would abort the process instead of reaching `checked_add`. A
/// parser that is "forgiving by default" must return `None` here (which
/// `resolve_date` turns into a clean `bad_request`) for every input a user can
/// type — an absurd count is a bad request, not a crash.
fn add_units(date: Date, n: i64, unit: &str) -> Option<Date> {
    let span = match unit {
        "d" | "day" | "days" => Span::new().try_days(n).ok()?,
        "w" | "wk" | "week" | "weeks" => Span::new().try_weeks(n).ok()?,
        "mo" | "mon" | "month" | "months" => Span::new().try_months(n).ok()?,
        "y" | "yr" | "year" | "years" => Span::new().try_years(n).ok()?,
        _ => return None,
    };
    date.checked_add(span).ok()
}

/// A single glued offset token like `3d`, `2w`, `1mo`, `1y`, with an optional
/// sign: `+3d` is the same as `3d`, and `-1d` reaches **into the past**.
///
/// The negative branch is not decoration. `tasqx modify 42 --due -1d` ("this was
/// actually due yesterday") is a real capture move, and `remind` already accepts
/// a leading `-`; a date grammar that rejected `-1d` while the reminder grammar
/// took `-1h` would be an arbitrary split down the middle of one mental model.
fn short_offset(tok: &str, today: Date) -> Option<Date> {
    let (sign, rest) = match tok.strip_prefix('-') {
        Some(r) => (-1i64, r),
        None => (1i64, tok.strip_prefix('+').unwrap_or(tok)),
    };
    let split = rest.find(|c: char| !c.is_ascii_digit())?;
    if split == 0 {
        return None;
    }
    let (num, unit) = rest.split_at(split);
    let n: i64 = num.parse().ok()?;
    add_units(today, sign.checked_mul(n)?, unit)
}

/// Parse a human duration (`4h`, `90m`, `2d`, `1h30m`, `1w`) into the ISO-8601
/// form the store keeps (`PT4H`, `PT90M`, `P2D`). A value that is already ISO
/// (`PT4H`) passes through validated.
///
/// This lives here, next to [`parse_when`], for the same reason the date grammar
/// does: it is the ONE place that turns what a human types into what the column
/// holds. `estimate` is stored as an opaque string, so an unvalidated `4h` would
/// be accepted by the API and then silently ignored by every consumer that reads
/// it back as a duration (`report.summary`'s `est_total`) — a bad value that
/// looks fine until a total is quietly wrong. Parsing at the edge makes it a
/// clean `bad_request` instead.
pub fn parse_duration(input: &str) -> Result<String, ApiError> {
    let raw = input.trim();
    if raw.is_empty() {
        return Err(ApiError::bad_request("empty duration"));
    }
    let bad = || {
        ApiError::bad_request(format!(
            "could not parse duration: {raw:?} (try e.g. 4h, 90m, 1h30m, 2d, 1w, or ISO PT4H)"
        ))
    };

    // Already ISO-8601: validate via the same reader every consumer uses.
    if raw.starts_with('P') || raw.starts_with('p') {
        let up = raw.to_ascii_uppercase();
        return match crate::util::duration_secs(&up) {
            Some(_) => Ok(up),
            None => Err(bad()),
        };
    }

    // Human form: a run of `<count><unit>` pairs, e.g. `1h30m`.
    let lower = raw.to_ascii_lowercase().replace(' ', "");
    let (mut weeks, mut days, mut hours, mut mins, mut secs) = (0i64, 0i64, 0i64, 0i64, 0i64);
    let mut num = String::new();
    let mut unit = String::new();
    let mut saw = false;

    // Flush one `<num><unit>` pair into its accumulator.
    let mut flush = |num: &mut String, unit: &mut String| -> Result<(), ApiError> {
        if num.is_empty() || unit.is_empty() {
            return Err(bad());
        }
        let n: i64 = num.parse().map_err(|_| bad())?;
        // `checked_add`, not `+`: one pair's count always fits, because the
        // parse above refuses anything wider than an `i64`, but enough repeated
        // pairs (`…w…w…w…`) sum past it right here, before the fold below is
        // reached. Note the suite's `1000000000000000000w1000000000000000000w`
        // is NOT that case — it sums to 2e18, which fits — and is refused one
        // step later by the `weeks * 7` fold instead.
        let acc = |slot: &mut i64| -> Result<(), ApiError> {
            *slot = slot.checked_add(n).ok_or_else(bad)?;
            Ok(())
        };
        match unit.as_str() {
            "w" | "wk" | "wks" | "week" | "weeks" => acc(&mut weeks)?,
            "d" | "day" | "days" => acc(&mut days)?,
            "h" | "hr" | "hrs" | "hour" | "hours" => acc(&mut hours)?,
            "m" | "min" | "mins" | "minute" | "minutes" => acc(&mut mins)?,
            "s" | "sec" | "secs" | "second" | "seconds" => acc(&mut secs)?,
            _ => return Err(bad()),
        }
        num.clear();
        unit.clear();
        Ok(())
    };

    for c in lower.chars() {
        if c.is_ascii_digit() {
            // A digit after a unit closes the previous pair (`1h30m`).
            if !unit.is_empty() {
                flush(&mut num, &mut unit)?;
            }
            num.push(c);
        } else if c.is_ascii_alphabetic() {
            if num.is_empty() {
                return Err(bad());
            }
            unit.push(c);
            saw = true;
        } else {
            return Err(bad());
        }
    }
    flush(&mut num, &mut unit)?;
    if !saw {
        return Err(bad());
    }

    // Weeks fold into days: `P1W` cannot legally carry other date parts.
    let days = weeks
        .checked_mul(7)
        .and_then(|w| w.checked_add(days))
        .ok_or_else(bad)?;
    let mut out = String::from("P");
    if days > 0 {
        out.push_str(&format!("{days}D"));
    }
    if hours > 0 || mins > 0 || secs > 0 {
        out.push('T');
        if hours > 0 {
            out.push_str(&format!("{hours}H"));
        }
        if mins > 0 {
            out.push_str(&format!("{mins}M"));
        }
        if secs > 0 {
            out.push_str(&format!("{secs}S"));
        }
    }
    // Everything parsed but summed to nothing (`0h`): a zero estimate is not a
    // typo worth guessing at, but it is not a duration either.
    if out == "P" {
        return Err(bad());
    }
    // Read the result back through the reader every consumer (`report`, urgency)
    // uses. The ISO branch above has always done this; the human branch never
    // did, so a value that formatted fine could still overflow on every later
    // read. One rule for both branches: what we store, `duration_secs` can read.
    if crate::util::duration_secs(&out).is_none() {
        return Err(bad());
    }
    Ok(out)
}

/// The ISO end-of-week (Sunday) on or after `today`.
fn end_of_week(today: Date) -> Date {
    // Monday=1..Sunday=7; days to Sunday.
    let delta = 7 - today.weekday().to_monday_one_offset() as i64;
    today.checked_add(Span::new().days(delta)).unwrap_or(today)
}

/// The next date whose weekday is `target`, strictly after `today` (so a request
/// for today's own weekday resolves to +7 — the *next* such day, per §5).
fn next_weekday(today: Date, target: Weekday) -> Date {
    let cur = today.weekday().to_monday_one_offset() as i64;
    let tgt = target.to_monday_one_offset() as i64;
    let mut delta = (tgt - cur).rem_euclid(7);
    if delta == 0 {
        delta = 7;
    }
    today.checked_add(Span::new().days(delta)).unwrap_or(today)
}

/// Map a weekday name (full or 3-letter) to a `Weekday`.
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

/// A trailing token counts as a clock only if it carries a `:` or an am/pm
/// suffix — so a bare number (a year, an offset count) is never mistaken for a
/// time.
fn is_time_token(tok: &str) -> bool {
    let t = tok.to_ascii_lowercase();
    (t.ends_with("am") || t.ends_with("pm") || t.contains(':')) && parse_clock(&t).is_some()
}

/// Parse a clock token: `17:00`, `17:00:30`, `9am`, `5pm`, `9:30am`.
fn parse_clock(tok: &str) -> Option<Time> {
    let t = tok.trim().to_ascii_lowercase();
    let (body, pm) = if let Some(x) = t.strip_suffix("am") {
        (x.trim().to_string(), Some(false))
    } else if let Some(x) = t.strip_suffix("pm") {
        (x.trim().to_string(), Some(true))
    } else {
        (t, None)
    };
    let parts: Vec<&str> = body.split(':').collect();
    let mut h: i8 = parts.first()?.parse().ok()?;
    let m: i8 = match parts.get(1) {
        Some(s) => s.parse().ok()?,
        None => 0,
    };
    let sec: i8 = match parts.get(2) {
        Some(s) => s.parse().ok()?,
        None => 0,
    };
    if let Some(is_pm) = pm {
        if !(1..=12).contains(&h) {
            return None;
        }
        if is_pm && h != 12 {
            h += 12;
        }
        if !is_pm && h == 12 {
            h = 0;
        }
    }
    Time::new(h, m, sec, 0).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixed reference: Wednesday, 2026-07-15T12:00:00Z.
    fn now() -> Timestamp {
        "2026-07-15T12:00:00Z".parse().unwrap()
    }

    /// Every existing assertion below predates the local-zone behaviour
    /// (#138) and was written against UTC, so `p` pins UTC explicitly through
    /// [`parse_when_zoned`] rather than going through the public
    /// [`parse_when`] (which reads [`TimeZone::system`]) — otherwise this
    /// whole suite would depend on the zone of whatever machine runs it.
    /// `zoned_forms` below exercises [`parse_when_zoned`] with a real
    /// non-UTC zone to prove the feature itself.
    fn p(s: &str) -> String {
        parse_when_zoned(s, now(), &TimeZone::UTC).unwrap()
    }

    /// `--due -1d` has to mean "yesterday", not "unparseable". The short-offset
    /// token was digits-only, so a signed offset fell through to the ISO-date
    /// branch and errored — while `remind` happily took `-1h`.
    #[test]
    fn signed_short_offsets() {
        assert_eq!(p("-1d"), "2026-07-14T00:00:00Z");
        assert_eq!(p("+3d"), "2026-07-18T00:00:00Z");
        assert_eq!(p("3d"), "2026-07-18T00:00:00Z");
        assert_eq!(p("-2w"), "2026-07-01T00:00:00Z");
        assert_eq!(p("-1mo"), "2026-06-15T00:00:00Z");
        // A trailing clock still applies to a signed offset.
        assert_eq!(p("-1d 9am"), "2026-07-14T09:00:00Z");
        // A bare sign is still a bad request, not a panic.
        assert!(parse_when("-", now()).is_err());
        assert!(parse_when("-d", now()).is_err());
    }

    /// A date past the end of the representable range was refused with `jiff`'s
    /// own words: "converting datetime with time zone offset `+00` to timestamp
    /// overflowed: parameter 'Unix timestamp seconds' is not in the required
    /// range of -377705023201..=253402207200". That names neither the value the
    /// user typed nor anything they could type instead — it is a library's
    /// internals reaching a terminal, and every other rejection in this tool
    /// names the offending value and the way out.
    ///
    /// All three absolute branches are checked because all three reach `finish`
    /// by different routes — a bare ISO date, a naive datetime with `T`, and the
    /// space-separated spelling of the same — so a fix wired into one of them
    /// leaves the other two leaking.
    #[test]
    fn a_date_past_the_representable_range_is_refused_in_this_tools_words() {
        for raw in [
            "9999-12-31",
            "9999-12-31T23:59",
            "9999-12-31 23:00",
            "9999-12-31T22:00:01",
        ] {
            let err = parse_when(raw, now()).expect_err("must be refused");
            let msg = err.message;
            assert!(msg.contains(raw), "the offending value is not named: {msg}");
            for leak in [
                "Unix timestamp",
                "parameter",
                "overflowed",
                "time zone offset",
                "-377705023201",
            ] {
                assert!(
                    !msg.contains(leak),
                    "jiff internals leaked ({leak:?}): {msg}"
                );
            }
            // The way out: a bound the user can aim below, in the same spelling
            // they typed. `9999` alone would be ambiguous, so the assertion is on
            // the full instant.
            assert!(
                msg.contains("9999-12-30"),
                "the last usable instant is not named: {msg}"
            );
        }
        // The near side of the same boundary still WORKS — a guard that only
        // proved the refusal would be satisfied by refusing every date.
        assert_eq!(p("9999-12-30"), "9999-12-30T00:00:00Z");
        assert_eq!(p("9999-12-30 22:00"), "9999-12-30T22:00:00Z");
    }

    #[test]
    fn duration_human_forms() {
        assert_eq!(parse_duration("4h").unwrap(), "PT4H");
        assert_eq!(parse_duration("90m").unwrap(), "PT90M");
        assert_eq!(parse_duration("1h30m").unwrap(), "PT1H30M");
        assert_eq!(parse_duration("2d").unwrap(), "P2D");
        assert_eq!(parse_duration("1w").unwrap(), "P7D");
        assert_eq!(parse_duration("1d4h").unwrap(), "P1DT4H");
        assert_eq!(parse_duration("30s").unwrap(), "PT30S");
        // Case and spacing are forgiving.
        assert_eq!(parse_duration(" 2 Hours ").unwrap(), "PT2H");
        // ISO passes through, uppercased and validated.
        assert_eq!(parse_duration("PT4H").unwrap(), "PT4H");
        assert_eq!(parse_duration("pt4h").unwrap(), "PT4H");
    }

    /// Every emitted form must read back through the same duration reader
    /// `report.summary` uses — otherwise an accepted estimate totals as zero.
    #[test]
    fn duration_output_reads_back_as_seconds() {
        for (input, want) in [
            ("4h", 4 * 3600),
            ("1h30m", 5400),
            ("2d", 2 * 86_400),
            ("1w", 7 * 86_400),
            ("1d4h", 86_400 + 4 * 3600),
        ] {
            let iso = parse_duration(input).unwrap();
            assert_eq!(
                crate::util::duration_secs(&iso),
                Some(want),
                "{input} → {iso} must read back"
            );
        }
    }

    #[test]
    fn duration_rejects_junk() {
        for bad in ["", "soon", "4x", "h4", "4", "-4h", "PT", "P4X"] {
            assert!(parse_duration(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn absolute_forms() {
        assert_eq!(p("2026-07-20"), "2026-07-20T00:00:00Z");
        assert_eq!(p("2026-07-20T17:00"), "2026-07-20T17:00:00Z");
        assert_eq!(p("2026-07-20 17:00"), "2026-07-20T17:00:00Z");
        assert_eq!(p("2026-07-20T17:00:00"), "2026-07-20T17:00:00Z");
        // Full RFC3339 with offset is normalized to the same instant in UTC.
        assert_eq!(p("2026-07-20T19:00:00+02:00"), "2026-07-20T17:00:00Z");
    }

    #[test]
    fn relative_words() {
        assert_eq!(p("today"), "2026-07-15T00:00:00Z");
        assert_eq!(p("tomorrow"), "2026-07-16T00:00:00Z");
        assert_eq!(p("yesterday"), "2026-07-14T00:00:00Z");
        assert_eq!(p("TOMORROW"), "2026-07-16T00:00:00Z"); // case-insensitive
    }

    #[test]
    fn weekdays_pick_next_occurrence() {
        // now is Wednesday 2026-07-15.
        assert_eq!(p("friday"), "2026-07-17T00:00:00Z"); // +2
        assert_eq!(p("monday"), "2026-07-20T00:00:00Z"); // +5
        assert_eq!(p("wed"), "2026-07-22T00:00:00Z"); // same weekday -> +7
        assert_eq!(p("friday 17:00"), "2026-07-17T17:00:00Z");
    }

    /// #137: `next friday` used to be a silent synonym for the bare weekday —
    /// tested (right here, until now) but documented nowhere a user would
    /// read it, and it disagreed with `due:next banana` proving the `next`
    /// token WAS being inspected: only the weekday branch swallowed it. A
    /// wrong date typed with confidence is worse than a refusal, so `next`
    /// now joins `this`/`last` on the refused side, for every weekday.
    #[test]
    fn next_weekday_is_refused_like_this_and_last() {
        for bad in [
            "next friday",
            "next monday",
            "next wed",
            "this friday",
            "last friday",
        ] {
            assert!(
                parse_when(bad, now()).is_err(),
                "{bad:?} must be refused, not silently aliased to the bare weekday"
            );
        }
    }

    #[test]
    fn offsets() {
        assert_eq!(p("in 3 days"), "2026-07-18T00:00:00Z"); // date offset, default midnight
        assert_eq!(p("3d"), "2026-07-18T00:00:00Z");
        assert_eq!(p("2w"), "2026-07-29T00:00:00Z");
        assert_eq!(p("1mo"), "2026-08-15T00:00:00Z");
        assert_eq!(p("in 2 weeks"), "2026-07-29T00:00:00Z");
        assert_eq!(p("in 1 month"), "2026-08-15T00:00:00Z");
    }

    #[test]
    fn end_of_month_and_week() {
        assert_eq!(p("eom"), "2026-07-31T00:00:00Z");
        assert_eq!(p("end of month"), "2026-07-31T00:00:00Z");
        // 2026-07-15 is Wed; ISO end of week (Sunday) is 2026-07-19.
        assert_eq!(p("eow"), "2026-07-19T00:00:00Z");
    }

    #[test]
    fn bare_time_rolls_to_tomorrow_when_past() {
        // now is 12:00Z. 09:00 already passed -> tomorrow.
        assert_eq!(p("9am"), "2026-07-16T09:00:00Z");
        // 17:00 is still ahead -> today.
        assert_eq!(p("17:00"), "2026-07-15T17:00:00Z");
    }

    /// #144: `now` is a full INSTANT, not a calendar day — unlike every other
    /// keyword in this grammar, which all answer in day-or-better granularity.
    /// It used to alias `today` (midnight), so `due.before:now` — the filter
    /// DSL's only overdue query — could never see a task due earlier the same
    /// day, and a task due at exactly midnight fell outside both halves of a
    /// before/after partition on it.
    #[test]
    fn now_resolves_to_the_exact_instant() {
        assert_eq!(p("now"), now().to_string());
        assert_eq!(p("NOW"), now().to_string()); // case-insensitive
                                                 // A sub-second anchor round-trips exactly — proof this is the real
                                                 // instant, not a truncated or reconstructed one.
        let precise: Timestamp = "2026-07-15T10:42:57.123456789Z".parse().unwrap();
        assert_eq!(
            parse_when_zoned("now", precise, &TimeZone::UTC).unwrap(),
            precise.to_string()
        );
        // `today` keeps meaning midnight — the two are not the same keyword
        // wearing two names.
        assert_ne!(p("now"), p("today"));
    }

    /// #138 (DESIGN.md:458): an explicit clock time with no offset of its own
    /// resolves in the zone it is given, not always UTC. `parse_when` itself
    /// reads the real machine zone (untestable here); `parse_when_zoned` is
    /// the same logic with the zone pinned, so this suite proves the feature
    /// without depending on wherever it runs.
    #[test]
    fn naive_time_resolves_against_the_given_zone() {
        // A date-and-time literal with no offset: DESIGN.md's own example,
        // `due:monday 17:00` in Amsterdam's +02:00 summer offset.
        let cest = TimeZone::fixed(jiff::tz::offset(2));
        assert_eq!(
            parse_when_zoned("2026-07-20T17:00", now(), &cest).unwrap(),
            "2026-07-20T15:00:00Z"
        );
        assert_eq!(
            parse_when_zoned("monday 17:00", now(), &cest).unwrap(),
            "2026-07-20T15:00:00Z"
        );
        // A bare date carries no time to localize and stays UTC midnight
        // regardless of the zone (D-1511) — the split #138 asks for.
        assert_eq!(
            parse_when_zoned("2026-07-20", now(), &cest).unwrap(),
            "2026-07-20T00:00:00Z"
        );
        // A bare weekday/relative word with no time of its own stays
        // UTC-anchored too, for the same reason.
        assert_eq!(
            parse_when_zoned("friday", now(), &cest).unwrap(),
            "2026-07-17T00:00:00Z"
        );
    }

    /// The Auckland case from the audit repro: near a UTC-day boundary, a
    /// positive-offset zone's "today" is already a different calendar day
    /// than UTC's — and a bare time must anchor (and roll to tomorrow) in
    /// ITS OWN day, not UTC's. `now` = 2026-07-15T23:00:00Z is already
    /// 2026-07-16T08:00 in a fixed +09:00 zone.
    #[test]
    fn bare_time_anchors_todays_date_in_the_given_zone_not_utc() {
        let jst = TimeZone::fixed(jiff::tz::offset(9));
        let anchor: Timestamp = "2026-07-15T23:00:00Z".parse().unwrap();
        // UTC-anchored "today" is still the 15th, so the old (buggy) answer
        // would roll a passed 17:00 to the 16th at 17:00 UTC. In JST "today"
        // is already the 16th, and 17:00 JST on the 16th has not passed yet
        // (local clock reads 08:00) — a genuinely different calendar day AND
        // a different roll decision from the UTC-anchored one.
        assert_eq!(
            parse_when_zoned("17:00", anchor, &jst).unwrap(),
            "2026-07-16T08:00:00Z"
        );
    }

    #[test]
    fn bad_input_is_an_error() {
        assert!(parse_when("not a date", now()).is_err());
        assert!(parse_when("", now()).is_err());
        assert!(parse_when("bluesday", now()).is_err());
    }

    /// `--due "at 6pm"` is what a human types, and it errored with "could not
    /// parse date" while the *time* had been understood perfectly — the leading
    /// preposition was the only thing left over, and no `resolve_date` arm
    /// matches a bare `["at"]`. "Forgiving by default" (§5) has to cover the
    /// filler words that carry no meaning: they are noise, not input.
    #[test]
    fn leading_filler_words_are_ignored() {
        // now is Wed 2026-07-15T12:00Z; 18:00 is still ahead -> today.
        assert_eq!(p("at 6pm"), "2026-07-15T18:00:00Z");
        assert_eq!(p("@ 6pm"), "2026-07-15T18:00:00Z");
        assert_eq!(p("on friday"), "2026-07-17T00:00:00Z");
        assert_eq!(p("by monday 5pm"), "2026-07-20T17:00:00Z");
        assert_eq!(p("on 2026-07-20"), "2026-07-20T00:00:00Z");
        // Stacked fillers still reduce to the same date.
        assert_eq!(p("by on friday"), "2026-07-17T00:00:00Z");
        // The bare-time roll-forward still applies behind a filler.
        assert_eq!(p("at 9am"), "2026-07-16T09:00:00Z");
    }

    /// The forgiveness is deliberately LEADING-only. A filler in the middle, a
    /// filler with junk after it, or a filler alone must stay a clean error —
    /// otherwise `at fridya` (a typo) silently becomes a date the user never
    /// meant, and a wrong due date you don't see beats an error you do.
    #[test]
    fn filler_stripping_does_not_swallow_junk() {
        for bad in [
            "at",
            "on",
            "by",
            "@",
            "at fridya",
            "on someday",
            "friday at bogus",
        ] {
            assert!(
                parse_when(bad, now()).is_err(),
                "{bad:?} must stay an error"
            );
        }
    }

    /// An absurd unit count is a `bad_request`, NOT a panic. jiff's `Span::days`
    /// (and friends) abort on an out-of-range count, so these inputs used to take
    /// the whole process down — `tasqx add "x" due:99999999d` exited 101 with a
    /// jiff backtrace. Every reachable spelling is covered: the glued short form
    /// and the `in N <unit>` form, across all four calendar units.
    #[test]
    fn an_absurd_unit_count_is_rejected_rather_than_panicking() {
        for bad in [
            "99999999d",
            "999999999w",
            "99999999mo",
            "99999999y",
            "in 99999999 days",
            "in 999999999999 weeks",
            "in 99999999 months",
            "in 99999999 years",
        ] {
            assert!(
                parse_when(bad, now()).is_err(),
                "{bad:?} must be a clean error"
            );
        }
    }

    /// The duration grammar has the same absurd-count hazard the date grammar
    /// was hardened against above, and reintroduced it: these inputs were
    /// ACCEPTED by `add`, then killed `report` on every read (util.rs unchecked
    /// multiply). A duration that cannot be read back by the reader `report`
    /// uses is not a duration — reject it at the edge, like every other field.
    #[test]
    fn an_absurd_duration_is_rejected_rather_than_panicking() {
        for bad in [
            "1000000000000000000w",
            "999999999999999999h",
            "PT999999999999999999H",
            "P7000000000000000000D",
            "9223372036854775807d",
            "1000000000000000000w1000000000000000000w",
        ] {
            assert!(
                parse_duration(bad).is_err(),
                "{bad:?} must be a clean error"
            );
        }
    }

    /// Every accepted duration round-trips through the reader `report` uses —
    /// the property that makes the guard above meaningful rather than a
    /// blocklist of the three spellings someone happened to think of.
    #[test]
    fn every_accepted_duration_reads_back_through_duration_secs() {
        for ok in ["4h", "90m", "1h30m", "2d", "1w", "PT4H", "P1DT2H30M", "52w"] {
            let iso = parse_duration(ok).expect("{ok:?} must parse");
            assert!(
                crate::util::duration_secs(&iso).is_some(),
                "{ok:?} -> {iso:?} must read back"
            );
        }
    }

    /// The boundary still parses: the guard rejects out-of-range counts without
    /// narrowing the range of inputs that legitimately worked before.
    #[test]
    fn a_large_but_in_range_offset_still_parses() {
        assert_eq!(parse_when("3650d", now()).unwrap(), "2036-07-12T00:00:00Z");
    }
}
