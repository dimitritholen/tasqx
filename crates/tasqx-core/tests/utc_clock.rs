//! A clock time typed with no offset of its own is UTC, whatever zone the
//! machine is in (D132).
//!
//! This is its own test binary because it sets `TZ` for the whole process: the
//! zone a parser could read is process state, and the only honest way to prove
//! it is NOT read is to run under a zone that is not UTC. `JST-9` is a POSIX
//! zone string (UTC+9), so it needs no tz database on any CI platform. Every
//! test here sets the same value before touching a date, so parallel tests in
//! this binary cannot disagree about it.

use jiff::Timestamp;
use serde_json::json;
use tasqx_core::datetime::parse_when;
use tasqx_core::remind::{parse_remind, Remind};
use tasqx_core::Engine;

fn in_tokyo() {
    std::env::set_var("TZ", "JST-9");
}

/// Wednesday, 2026-07-15T12:00:00Z — 21:00 in the zone above.
fn now() -> Timestamp {
    "2026-07-15T12:00:00Z".parse().unwrap()
}

/// Every spelling that carries a clock and no offset reads that clock as UTC.
/// Before D132 each of these came back nine hours early under this zone
/// (`2026-07-20T08:00:00Z` for `2026-07-20T17:00`).
#[test]
fn a_clock_time_with_no_offset_is_utc_under_a_non_utc_zone() {
    in_tokyo();
    for (typed, want) in [
        ("2026-07-20T17:00", "2026-07-20T17:00:00Z"),
        ("2026-07-20 17:00", "2026-07-20T17:00:00Z"),
        ("2026-07-20T17:00:30", "2026-07-20T17:00:30Z"),
        ("monday 17:00", "2026-07-20T17:00:00Z"),
        ("tomorrow 9am", "2026-07-16T09:00:00Z"),
        ("today 23:00", "2026-07-15T23:00:00Z"),
        // A bare time is today's UTC day, and rolls on UTC's clock: 13:00 UTC
        // is still ahead of 12:00 UTC, though 13:00 in Tokyo has passed.
        ("13:00", "2026-07-15T13:00:00Z"),
        ("at 11am", "2026-07-16T11:00:00Z"),
    ] {
        assert_eq!(
            parse_when(typed, now()).unwrap(),
            want,
            "{typed:?} was read in the machine's zone"
        );
    }
}

/// An explicit offset keeps its meaning, and a bare date and `now` are
/// unaffected — the ruling moves only the naive clock.
#[test]
fn an_explicit_offset_a_bare_date_and_now_keep_their_meaning() {
    in_tokyo();
    assert_eq!(
        parse_when("2026-07-20T17:00:00+02:00", now()).unwrap(),
        "2026-07-20T15:00:00Z"
    );
    assert_eq!(
        parse_when("2026-07-20T17:00:00Z", now()).unwrap(),
        "2026-07-20T17:00:00Z"
    );
    assert_eq!(
        parse_when("2026-07-20", now()).unwrap(),
        "2026-07-20T00:00:00Z"
    );
    assert_eq!(parse_when("now", now()).unwrap(), now().to_string());
}

/// `remind:` is a second entry point into the same grammar.
#[test]
fn an_absolute_reminder_clock_is_utc_under_a_non_utc_zone() {
    in_tokyo();
    assert_eq!(
        parse_remind("2026-07-20T17:00", now()).unwrap(),
        Remind::At("2026-07-20T17:00:00Z".into())
    );
    assert_eq!(
        parse_remind("friday 9am", now()).unwrap(),
        Remind::At("2026-07-17T09:00:00Z".into())
    );
}

/// The JSON API path — what MCP and every client send — resolves the same way.
#[test]
fn the_json_api_stores_a_naive_clock_as_utc_under_a_non_utc_zone() {
    in_tokyo();
    let e = Engine::open_in_memory().expect("open in-memory store");
    let added = e
        .task_add(&json!({
            "title": "C",
            "due": "2026-09-13T09:00",
            "scheduled": "2026-09-12 08:30",
            "wait": "2026-09-11T07:00",
            "remind": "2026-09-13T08:00",
        }))
        .unwrap();
    let got = e
        .task_get(&json!({ "ref": added["short_id"].clone() }))
        .unwrap();
    assert_eq!(got["due"], "2026-09-13T09:00:00Z", "{got}");
    assert_eq!(got["scheduled"], "2026-09-12T08:30:00Z", "{got}");
    assert_eq!(got["wait"], "2026-09-11T07:00:00Z", "{got}");
    assert_eq!(got["remind"], "2026-09-13T08:00:00Z", "{got}");

    let modified = e
        .task_modify(
            &json!({ "ref": added["short_id"].clone(), "set": { "due": "2026-09-14T10:15" } }),
        )
        .unwrap();
    let got = e
        .task_get(&json!({ "ref": added["short_id"].clone() }))
        .unwrap();
    assert_eq!(got["due"], "2026-09-14T10:15:00Z", "{modified}");
}
