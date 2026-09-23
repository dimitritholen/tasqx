use super::*;
use crate::render::test_support::*;
use crate::theme::{self, Caps, Ctx};
use serde_json::json;

/// D122: `next` says why this task, and what to type. It printed
/// `#48  (urgency 18.1)  <title>` for a task two days overdue.
#[test]
fn next_says_why_this_task_and_what_to_type() {
    let now: Timestamp = "2026-09-11T11:00:00Z".parse().unwrap();
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let out = next_task(
        &ctx,
        &json!({ "tasks": [{
                "short_id": 48, "title": "Renew the TLS certificate",
                "status": "pending", "priority": "H", "urgency": 18.1,
                "project": "infra", "due": "2026-09-09T00:00:00Z", "tags": ["ops"]
            }] }),
        now,
    );
    for want in [
        "Renew the TLS certificate",
        "H 18.1",
        "infra",
        "due 2d ago",
        "+ops",
        "tasqx start 48",
        "tasqx why 48",
    ] {
        assert!(out.contains(want), "{want:?} missing from next:\n{out}");
    }
    assert!(!out.contains("(urgency"), "{out}");
    let lines: Vec<&str> = out.lines().collect();
    let col = lines[0].find("#48").unwrap();
    assert_eq!(
        lines[1].len() - lines[1].trim_start().len(),
        col,
        "the facts do not start under #48:\n{out}"
    );
}

/// D122: `why` names the task, says what drove each term, and ends on the
/// number every other screen prints. It said `Why #48 has urgency 18.1`
/// over `due_proximity 12.00` and closed on `= total 18.09`.
#[test]
fn why_names_the_task_explains_each_term_and_totals_the_urgency() {
    let now: Timestamp = "2026-09-11T11:00:00Z".parse().unwrap();
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let out = why(
        &ctx,
        &json!({
            "short_id": 48, "title": "Renew the TLS certificate", "priority": "H",
            "urgency": 18.1, "due": "2026-09-09T00:00:00Z",
            "created": "2026-09-02T10:00:00Z", "blocked": false,
            "urgency_breakdown": { "priority": 6.0, "due_proximity": 12.0, "age": 0.09 }
        }),
        now,
    );
    for want in [
        "Renew the TLS certificate",
        "deadline",
        "overdue, 2d ago",
        "9 days",
        "18.1",
    ] {
        assert!(out.contains(want), "{want:?} missing from why:\n{out}");
    }
    for gone in ["due_proximity", "18.09", "Why #"] {
        assert!(!out.contains(gone), "{gone:?} still in why:\n{out}");
    }
}

/// #233.1: `next` on a genuinely fresh store must not say "you're clear"
/// — that phrase reads as "you finished your work", which is a lie about
/// a store that has never held any.
#[test]
fn next_task_on_a_genuinely_empty_store_names_the_onboarding_commands() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let empty_store = json!({ "tasks": [], "store_empty": true });
    let out = next_task(&ctx, &empty_store, crate::clock::now());
    assert!(out.contains("tasqx add"), "{out:?}");

    // A working set that is genuinely clear (real tasks exist, none is
    // actionable right now) keeps the original, true statement.
    let clear = json!({ "tasks": [], "store_empty": false });
    assert_eq!(
        next_task(&ctx, &clear, crate::clock::now()),
        "Nothing actionable — you're clear.\n"
    );
}

/// #346: `next` fits the terminal. Its title is cut to the width, and its
/// facts are taken whole or not at all, by the same rule `add`'s echo
/// follows. Which facts survive goes by what the reader loses without
/// them, not by where they sit on the line: the urgency cell and the
/// deadline say why this task, the project only where. The first cut took
/// facts left to right and stopped at the first that did not fit, so a
/// long project name pushed the deadline off the line (review of #346).
#[test]
fn next_fits_a_narrow_terminal_and_keeps_the_facts_that_say_why() {
    use unicode_width::UnicodeWidthStr;
    let ctx = Ctx::new(theme::default_theme(), card_caps()).with_cols(60);
    let mut t = task_json(
        48,
        "Renew the TLS certificate for api.example.dev before it lapses on the weekend",
        "infrastructure-platform-team",
        "2026-08-01T00:00:00Z",
        &["ops", "security", "certificates"],
    );
    t["priority"] = json!("H");
    t["urgency"] = json!(18.1);
    t["status"] = json!("active");
    let out = next_task(&ctx, &json!({ "tasks": [t] }), anchor());
    for l in out.lines() {
        assert!(l.width() <= 60, "{} cells at 60: {l:?}\n{out}", l.width());
    }
    assert!(
        out.lines().next().is_some_and(|l| l.contains("#48  Renew")),
        "{out}"
    );
    let facts = out.lines().nth(1).expect("a facts line");
    assert!(facts.contains("18.1"), "{out}");
    assert!(facts.contains("due "), "the deadline gave way: {out}");
    assert!(
        !facts.contains("infrastructure"),
        "a fact was cut rather than dropped, or kept over the deadline: {facts:?}"
    );
    // The running state is still on screen when its fact is not: the
    // command line offers `done`, not `start`.
    assert!(out.contains("tasqx done 48"), "{out}");
}

/// Round 2 review of #346: `next` and `add`'s echo fit their facts by the
/// one rule `summary_line` follows: taken in rank order, and the first that
/// does not fit ends the line, so nothing less important survives a fact
/// that was dropped. `next` kept the tags after dropping the project, and
/// `add` ranked the project above the deadline where `next` ranked it
/// below.
#[test]
fn next_and_add_drop_facts_from_the_least_important_up() {
    let ctx = Ctx::new(theme::default_theme(), card_caps()).with_cols(60);
    let mut t = task_json(
        48,
        "Renew the certificate",
        "infrastructure-platform-team-west",
        "2026-08-05T00:00:00Z",
        &["ops"],
    );
    t["priority"] = json!("H");
    t["urgency"] = json!(18.1);
    let next = next_task(&ctx, &json!({ "tasks": [t.clone()] }), anchor());
    let facts = next.lines().nth(1).expect("a facts line");
    assert!(facts.contains("due "), "{next}");
    assert!(!facts.contains("infrastructure"), "{next}");
    assert!(
        !facts.contains("+ops"),
        "a fact outlived a more important one that was dropped: {facts:?}"
    );

    let ctx = ctx.with_cols(40);
    let added = echo::added(&ctx, &t, anchor());
    let facts = added.lines().nth(1).expect("a facts line");
    assert!(
        facts.contains("due "),
        "add ranked the project above the deadline: {added}"
    );
    assert!(!facts.contains("infrastructure"), "{added}");
}

/// `tasqx why` printed `age             -0.00`.
///
/// The age term is `(-age_days).max(0.0)`, and when `created` falls in the
/// very second the clock is read, `age_days` is `0.0`, so the negation is
/// `-0.0` — a value that compares EQUAL to zero while keeping its sign bit,
/// which `{:.2}` then faithfully renders with a minus in front. A reader
/// cannot act on "minus zero": it says a term subtracted urgency when it
/// contributed none.
///
/// Both spellings are covered because they are separate format calls with
/// separate precisions — the component rows at 2 decimals and the total
/// (which appears TWICE, in the heading and in the `= total` row) at 1. A
/// fix applied to one of them leaves the other printing `-0`.
#[test]
fn why_never_renders_a_component_or_a_total_as_negative_zero() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let part = |name: &'static str, v: f64| (name, String::new(), v);
    let out = why_table(
        &ctx,
        &[
            part("priority", 0.0),
            part("deadline", 0.0),
            part("age", -0.0),
        ],
    );
    assert!(
        !out.contains("-0"),
        "a component rendered as negative zero: {out:?}"
    );
    assert!(
        out.contains("0.0"),
        "the zero itself must still be shown: {out:?}"
    );

    // Every part negative-zero makes the SUM negative zero too, which is the
    // total row — the twin the component fix does not reach.
    let all_neg = why_table(&ctx, &[part("priority", -0.0), part("age", -0.0)]);
    assert!(
        !all_neg.contains("-0"),
        "the total rendered as negative zero: {all_neg:?}"
    );

    // The rule is about a sign that survived ROUNDING, not about the value
    // being exactly zero: -0.004 is genuinely negative and still prints as a
    // row of zeros, so `v == 0.0` would not have caught it.
    let tiny = why_table(&ctx, &[part("age", -0.004)]);
    assert!(
        !tiny.contains("-0"),
        "a rounded-to-zero negative kept its sign: {tiny:?}"
    );
}

/// The twin of the above, and the reason it is not spelled `.abs()`: a term
/// that really is negative must keep its minus. Nothing in today's formula
/// produces one, but the formula is D1's to change and a display that
/// silently drops signs would report the change wrong.
#[test]
fn why_keeps_the_sign_of_a_value_that_is_actually_negative() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let out = why_table(
        &ctx,
        &[
            ("penalty", String::new(), -1.5),
            ("priority", String::new(), 6.0),
        ],
    );
    assert!(
        out.contains("-1.5"),
        "a real negative lost its sign: {out:?}"
    );
    assert!(out.contains("4.5"), "the total must still net out: {out:?}");
}

/// Finding #6 (audit-2026-09), kept at one decimal (D122): the shares
/// printed must add up to the total printed. Rounded independently,
/// `3.95 + 11.45` reads `4.0 + 11.5` over a total of `15.4`; apportioned by
/// largest remainder the shares are `3.9 + 11.5`, and the total is still
/// the `15.4` every other screen prints for the task.
#[test]
fn why_shares_add_up_to_the_total_at_one_decimal() {
    for parts in [
        vec![3.95, 11.45],
        vec![6.0, 12.0, 0.09],
        vec![3.9, 11.52, 0.33],
        vec![1.8, 0.05, 0.05, 0.05],
    ] {
        let shares = apportion(&parts);
        let sum: f64 = shares.iter().sum();
        let total: f64 = parts.iter().sum();
        assert!(
            (sum - (total * 10.0).round() / 10.0).abs() < 1e-9,
            "{parts:?} -> {shares:?} sums to {sum}, not {total:.1}"
        );
    }
}

/// Finding #8 (audit-2026-09): `why` explained a blocked task's urgency
/// and never mentioned that `next` will skip it — the least relevant half
/// of the answer, with the fact that decides actionability left unsaid.
#[test]
fn why_names_the_unmet_blockers_and_that_next_skips_the_task() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let out = why(
        &ctx,
        &json!({
            "short_id": 279,
            "priority": "H",
            "due": null,
            "created": "2026-09-09T00:00:00Z",
            "blocked": true,
            "unmet_blockers": [{ "short_id": 280, "title": "the blocker" }],
        }),
        crate::clock::now(),
    );
    assert!(out.contains("#280"), "{out:?}");
    assert!(out.contains("the blocker"), "{out:?}");
    assert!(out.contains("next"), "must say `next` skips it: {out:?}");

    // Not blocked: no such line at all.
    let out = why(
        &ctx,
        &json!({
            "short_id": 1,
            "priority": "H",
            "due": null,
            "created": "2026-09-09T00:00:00Z",
            "blocked": false,
            "unmet_blockers": [],
        }),
        crate::clock::now(),
    );
    assert!(
        !out.contains("blocked by"),
        "an unblocked task must not claim a blocker: {out:?}"
    );
}

/// D131 on the two single-task screens: `why` and the `show` card said
/// `overdue` for a date-only deadline from one second past midnight.
#[test]
fn why_and_the_card_do_not_call_a_date_only_deadline_overdue_on_its_own_day() {
    let now: Timestamp = "2026-09-11T11:00:00Z".parse().unwrap();
    let plain = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let task = |due: &str| {
        json!({
            "short_id": 48, "title": "Renew the TLS certificate", "priority": "H",
            "urgency": 18.0, "due": due, "created": "2026-09-02T10:00:00Z",
            "blocked": false,
            "urgency_breakdown": { "priority": 6.0, "due_proximity": 12.0, "age": 0.0 }
        })
    };
    let date_only = why(&plain, &task("2026-09-11T00:00:00Z"), now);
    assert!(!date_only.contains("overdue"), "{date_only}");
    assert!(date_only.contains("due today"), "{date_only}");
    let missed = why(&plain, &task("2026-09-11T09:00:00Z"), now);
    assert!(missed.contains("overdue, today"), "{missed}");

    let color = Ctx::new(
        theme::default_theme(),
        Caps {
            depth: theme::ColorDepth::Truecolor,
            ansi: true,
            unicode: true,
        },
    );
    let mut t = detail_fixture();
    t["due"] = json!("2026-09-11T00:00:00Z");
    let card = task_detail(&color, &t, now);
    assert!(
        !card.contains(&color.paint("overdue", "today")),
        "a date-only deadline today is not painted overdue: {card:?}"
    );
}
