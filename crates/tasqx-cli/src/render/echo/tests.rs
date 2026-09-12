//! Unit guards for the write-echo card (D126). The end-to-end half, which needs
//! the real binary and a terminal, is `tests/write_echoes.rs`.

use super::*;
use crate::theme::{self, Caps};
use serde_json::json;

fn now() -> Timestamp {
    "2026-09-11T12:00:00Z".parse().unwrap()
}

/// A terminal with Unicode and no escapes, so widths are plain widths.
fn unicode(cols: usize) -> Ctx {
    Ctx::new(
        theme::default_theme(),
        Caps {
            depth: theme::ColorDepth::None,
            ansi: false,
            unicode: true,
        },
    )
    .with_cols(cols)
}

/// NO_COLOR on a terminal: escapes, but bold is the only one left.
fn no_color(cols: usize) -> Ctx {
    Ctx::new(
        theme::default_theme(),
        Caps {
            depth: theme::ColorDepth::None,
            ansi: true,
            unicode: true,
        },
    )
    .with_cols(cols)
}

fn plain(cols: usize) -> Ctx {
    Ctx::new(theme::default_theme(), Caps::PLAIN).with_cols(cols)
}

fn task() -> Value {
    json!({
        "short_id": 50, "title": "Fix token refresh race on Android",
        "status": "pending", "priority": "H", "urgency": 17.2,
        "project": "mobile", "due": "2026-09-12T17:00:00Z", "estimate": "PT4H",
        "tags": ["bug", "urgent"], "_rev": 3, "blocked": false,
        "unmet_blockers": [],
    })
}

fn set(pairs: &[(&str, Value)]) -> serde_json::Map<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

/// D126, amendment 6: the change leads and never drops; `list`'s context goes
/// whole, from the right, and a line never runs past the terminal.
#[test]
fn context_drops_whole_from_the_right_and_the_change_never_does() {
    let t = task();
    let wide = modified(&unicode(120), &t, &set(&[("due", json!("x"))]), &[], now());
    let narrow = modified(&unicode(40), &t, &set(&[("due", json!("x"))]), &[], now());
    let line = |out: &str| out.lines().nth(1).unwrap().to_string();
    assert!(line(&wide).contains("rev 3"), "{wide}");
    assert!(
        line(&narrow).contains("due tomorrow 17:00"),
        "the change went: {narrow}"
    );
    // `rev` stays: `--expected-rev` needs it (#188), so `list`'s context
    // gives way instead (review round 1; it used to be the first to go).
    assert!(line(&narrow).contains("rev 3"), "rev went: {narrow}");
    for l in narrow.lines() {
        assert!(width(l) <= 40, "{} cells: {l:?}", width(l));
    }
    // A change wider than the terminal: it continues on the rail and none of
    // it goes. (The one-field change above fits once the context has gone,
    // so it cannot tell a droppable change from a fixed one.)
    let all = set(&[
        ("priority", json!("H")),
        ("project", json!("mobile")),
        ("due", json!("x")),
        ("estimate", json!("PT4H")),
    ]);
    let wide_change = modified(&unicode(40), &t, &all, &[], now());
    for piece in ["H ▄▄▄▄ 17.2", "mobile", "due tomorrow 17:00", "est 4h"] {
        assert!(wide_change.contains(piece), "{piece} went: {wide_change}");
    }
    for l in wide_change.lines() {
        assert!(width(l) <= 40, "{} cells: {l:?}", width(l));
    }
    // Whole or not at all: nothing is cut to fit.
    for piece in ["mobile", "est 4h", "+bug +urgent"] {
        if line(&narrow).contains(&piece[..3]) {
            assert!(line(&narrow).contains(piece), "{piece} was cut: {narrow}");
        }
    }
    // The order `list` gives way in: est, then tags, project, urgency.
    let at = |cols: usize| {
        line(&modified(
            &unicode(cols),
            &t,
            &set(&[("due", json!("x"))]),
            &[],
            now(),
        ))
    };
    let mut seen_gone = Vec::new();
    for cols in (30..=120).rev() {
        let l = at(cols);
        for f in ["est 4h", "+bug", "mobile", "H ▄"] {
            if !l.contains(f) && !seen_gone.contains(&f) {
                seen_gone.push(f);
            }
        }
    }
    assert_eq!(seen_gone, ["est 4h", "+bug", "mobile", "H ▄"]);
}

/// A change longer than the terminal continues under itself on the rail; it
/// is not cut, and nothing runs past the edge.
#[test]
fn a_change_too_long_for_one_line_continues_on_the_rail() {
    let mut t = task();
    t["tags"] = json!(["alpha", "bravo", "charlie", "delta", "echo", "foxtrot"]);
    let asked: Vec<String> = ["alpha", "bravo", "charlie", "delta", "echo", "foxtrot"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let out = modified(
        &unicode(40),
        &t,
        &set(&[("due", json!("x")), ("estimate", json!("PT4H"))]),
        &asked,
        now(),
    );
    for l in out.lines() {
        assert!(width(l) <= 40, "{} cells: {l:?}", width(l));
        assert!(l.starts_with('▌'), "off the rail: {l:?}");
    }
    for tag in &asked {
        assert!(out.contains(&format!("+{tag}")), "+{tag} went: {out}");
    }
    assert!(out.contains("est 4h"), "{out}");
}

/// The user's own words wrap and are never cut, and a pipe gets them whole.
#[test]
fn an_annotation_wraps_under_itself_and_keeps_every_word() {
    let note = "word ".repeat(30).trim().to_string();
    let result = json!({ "short_id": 50, "annotation": { "body": note } });
    let out = annotated(&unicode(40), &result, &task(), now());
    assert_eq!(out.matches("word").count(), 30, "{out}");
    for l in out.lines() {
        assert!(width(l) <= 40, "{} cells: {l:?}", width(l));
        assert!(l.starts_with('▌'), "off the rail: {l:?}");
    }
    let piped = annotated(&plain(40), &result, &task(), now());
    assert_eq!(piped.lines().count(), 2, "a pipe has no width: {piped}");
}

/// The plain path is never fitted: a pipe has no width, and a script diffing
/// two runs must not see a fact vanish because a terminal was narrow.
#[test]
fn the_plain_card_is_never_fitted() {
    let out = modified(
        &plain(20),
        &task(),
        &set(&[("due", json!("x"))]),
        &[],
        now(),
    );
    let l: Vec<&str> = out.lines().collect();
    assert_eq!(l[0], "#50  Fix token refresh race on Android");
    assert!(l[1].starts_with("modified"), "{out}");
    for fact in ["H 17.2", "mobile", "+bug +urgent", "est 4h", "rev 3"] {
        assert!(
            l[1].contains(fact),
            "{fact} dropped on the plain path: {out}"
        );
    }
    assert!(out.is_ascii(), "{out}");
}

/// D126, amendment 1: `▶`/`⊘` live in the rail column, at the start of a
/// line, and nowhere else; everything else starts on `▌`.
#[test]
fn only_the_rail_column_carries_a_state_glyph() {
    let ctx = unicode(100);
    let mut running = task();
    running["status"] = json!("active");
    let mut blocked = task();
    blocked["blocked"] = json!(true);
    blocked["unmet_blockers"] = json!([{ "short_id": 7, "title": "the blocker" }]);
    let titles: Titles = [(52, "waits".to_string()), (49, "stopped one".to_string())]
        .into_iter()
        .collect();
    let outs = [
        started(
            &ctx,
            &json!({ "short_id": 50, "auto_stopped": [{ "short_id": 49, "tracked": "PT5M" }] }),
            &running,
            &titles,
            now(),
        ),
        status_changed(
            &ctx,
            "reopened",
            &json!({ "blocked": [52] }),
            &task(),
            &titles,
            now(),
        ),
        dep_changed(
            &ctx,
            &json!({ "inserted": true }),
            &blocked,
            true,
            "7",
            now(),
        ),
        stopped(
            &ctx,
            &json!({ "interval": "PT5M", "tracked": "PT5M" }),
            &task(),
            now(),
        ),
    ];
    assert!(
        outs[0].lines().nth(1).unwrap().starts_with('▶'),
        "{}",
        outs[0]
    );
    assert!(
        outs[1].lines().any(|l| l.starts_with("⊘ #52")),
        "{}",
        outs[1]
    );
    assert!(
        outs[2].lines().nth(1).unwrap().starts_with('⊘'),
        "{}",
        outs[2]
    );
    assert!(
        outs[3].lines().nth(1).unwrap().starts_with('▌'),
        "{}",
        outs[3]
    );
    for out in &outs {
        for l in out.lines() {
            for g in ['▶', '⊘'] {
                if let Some(i) = l.find(g) {
                    assert_eq!(i, 0, "{g} away from the rail: {l:?}");
                }
            }
            assert!(
                l.starts_with('▌')
                    || l.starts_with('▶')
                    || l.starts_with('⊘')
                    || l.starts_with("  #"),
                "a line off the rail: {l:?}"
            );
        }
    }
}

/// Every stretch of bold on a painted line.
fn bold_runs(line: &str) -> Vec<String> {
    let mut runs = Vec::new();
    let mut rest = line;
    while let Some(i) = rest.find("\u{1b}[1") {
        let after = &rest[i..];
        let start = after.find('m').unwrap() + 1;
        let end = after[start..]
            .find('\u{1b}')
            .map_or(after.len(), |e| start + e);
        runs.push(after[start..end].to_string());
        rest = &after[end..];
    }
    runs
}

/// D126, amendment 2: on the second line, bold is the outcome and what the
/// write changed. `list`'s context never is, bar a late deadline and the
/// top urgency band, which carry their own weight.
#[test]
fn bold_on_the_second_line_is_the_outcome_and_the_change() {
    let ctx = no_color(120);
    let mut t = task();
    t["urgency"] = json!(4.0);
    t["due"] = json!("2026-09-20T00:00:00Z");
    let line2 = |out: String| out.lines().nth(1).unwrap().to_string();

    let a = line2(added(&ctx, &t, now()));
    assert_eq!(bold_runs(&a), ["added"], "{a:?}");

    let m = line2(modified(&ctx, &t, &set(&[("due", json!("x"))]), &[], now()));
    assert_eq!(bold_runs(&m), ["modified", "due 20 Sep"], "{m:?}");

    // The tags the command named are bold inside the whole set, so `tag` says
    // which tags it added rather than redrawing the set unmarked. A tag the
    // task already had and the command named again is bold on a write that
    // changed nothing — the same residue `modify` carries for a field set to
    // the value it already had (D126 b), and the reason `tag.add`'s own answer
    // (the whole set) is still not what picks the bold.
    let tg = line2(tag_changed(
        &ctx,
        &json!({}),
        &t,
        true,
        &["urgent".into()],
        now(),
    ));
    assert_eq!(bold_runs(&tg), ["tagged", "+urgent"], "{tg:?}");

    let st = line2(stopped(
        &ctx,
        &json!({ "interval": "PT5M", "tracked": "PT2H" }),
        &t,
        now(),
    ));
    assert_eq!(
        bold_runs(&st),
        ["stopped after 5m", "tracked 2h of 4h"],
        "{st:?}"
    );

    let mut running = t.clone();
    running["status"] = json!("active");
    let again = line2(started(
        &ctx,
        &json!({ "already_running": true, "interval_started": "2026-09-11T09:00:00Z" }),
        &running,
        &Titles::new(),
        now(),
    ));
    assert!(bold_runs(&again).is_empty(), "nothing changed: {again:?}");
}

/// D126, amendment 3: a closed task draws no urgency, and a zero is not a fact.
#[test]
fn a_closed_task_draws_no_urgency_and_no_zero() {
    let ctx = unicode(120);
    let mut t = task();
    t["status"] = json!("done");
    let out = done(
        &ctx,
        &json!({ "tracked": "PT0S", "estimate": "PT4H" }),
        &t,
        &Titles::new(),
        now(),
    );
    let l = out.lines().nth(1).unwrap();
    assert!(!l.contains("17.2") && !l.contains('▄'), "{out}");
    assert!(!l.contains("0s"), "{out}");
    assert!(l.contains("est 4h"), "{out}");
    let worked = done(
        &ctx,
        &json!({ "tracked": "PT3H", "estimate": "PT4H" }),
        &t,
        &Titles::new(),
        now(),
    );
    let l = worked.lines().nth(1).unwrap();
    assert!(
        l.contains("tracked 3h of 4h") && !l.contains("est 4h"),
        "rule 11: {worked}"
    );

    t["status"] = json!("cancelled");
    let out = status_changed(&ctx, "cancelled", &json!({}), &t, &Titles::new(), now());
    assert!(!out.contains("17.2"), "{out}");
}

/// D126, amendment 8: an undep that leaves a blocker names it, in the rail
/// and on the line, and never calls the task free.
#[test]
fn an_undep_that_leaves_a_blocker_names_it() {
    let ctx = unicode(100);
    let mut t = task();
    t["blocked"] = json!(true);
    t["unmet_blockers"] = json!([{ "short_id": 2, "title": "Review the draft" }]);
    let out = dep_changed(&ctx, &json!({ "blocked": true }), &t, false, "1", now());
    let l = out.lines().nth(1).unwrap();
    assert!(l.starts_with('⊘'), "{out}");
    assert!(l.contains("no longer waits on #1"), "{out}");
    assert!(
        l.contains("still blocked by #2 · Review the draft"),
        "{out}"
    );
    assert!(!out.contains("unblocked") && !out.contains("free"), "{out}");
}

/// D126, amendment 7: a project is not a task, so no rail; its name leads.
#[test]
fn a_project_echo_draws_no_rail() {
    let ctx = unicode(80);
    for out in [
        project_created(
            &ctx,
            &json!({ "name": "research", "default": false, "current_default": "work" }),
        ),
        default_switched(&ctx, &json!({ "name": "research", "previous": "work" })),
        project_archived(
            &ctx,
            &json!({ "name": "research", "default_cleared": false, "open_tasks": 0 }),
        ),
        imported(
            &ctx,
            &json!({ "imported": 3, "projects_imported": 1, "docs_imported": 0 }),
        ),
    ] {
        assert!(!out.contains('▌'), "{out}");
    }
    assert!(imported(&ctx, &json!({ "imported": 1 })).contains("1 task · no memory docs"));
}

/// The terminal's sentence for core's `tokens_hint` keys on how core words
/// the variant that needs nothing from the reader. If core rewords it, this
/// goes red here rather than the terminal nagging a reader who already
/// reported.
#[test]
fn the_token_note_tracks_cores_hint_wording() {
    let e = tasqx_core::Engine::open_in_memory().unwrap();
    let call = |m: &str, p: Value| tasqx_core::dispatch(&e, m, &p).unwrap();
    call("project.create", json!({ "name": "p" }));
    call("task.add", json!({ "title": "a", "project": "p" }));
    call("task.add", json!({ "title": "b", "project": "p" }));

    let bare = call("task.done", json!({ "ref": "1" }));
    let hint = bare["tokens_hint"]
        .as_str()
        .expect("a bare done carries a hint");
    let note = tokens_note(hint, 200, true).expect("and the reader can act on it");
    assert!(note.starts_with("note: no token counts"), "{note}");

    call(
        "token.add",
        json!({ "ref": "2", "input_tokens": 10, "output_tokens": 5, "source": "self-report", "confidence": "medium", "tool": "claude-code" }),
    );
    let covered = call("task.done", json!({ "ref": "2" }));
    let hint = covered["tokens_hint"]
        .as_str()
        .expect("a covered done still hints");
    assert!(
        hint.starts_with(ALREADY_COVERED),
        "core reworded the covered hint: {hint}"
    );
    assert_eq!(tokens_note(hint, 200, true), None);
}

/// Rule 11: the tags an undo brought back are drawn once, inside the task's
/// set, not once as the change and again as `list`'s tags fact.
#[test]
fn undo_names_the_restored_tags_once() {
    let ctx = unicode(120);
    let result = json!({
        "reverted": { "op": "tag.remove" }, "short_id": 50,
        "restored": { "tags": ["blocking"] },
    });
    let mut t = task();
    t["tags"] = json!(["api", "blocking"]);
    let out = undone(&ctx, &result, &t, &Titles::new(), now());
    assert_eq!(out.matches("+blocking").count(), 1, "{out}");
    assert_eq!(out.matches("+api").count(), 1, "{out}");
}

// ---- review round 1 ------------------------------------------------------

/// Every echo, painted with `ctx`, from a fixture that is neither late nor in
/// the top urgency band, so the only bold a line may carry is the write's.
/// Each entry names the bold runs its lines after the first may carry.
fn every_echo(ctx: &Ctx) -> Vec<(&'static str, String, Vec<&'static str>)> {
    let n = now();
    let mut t = task();
    t["urgency"] = json!(4.0);
    t["due"] = json!("2026-09-20T00:00:00Z");
    t["tags"] = json!(["bug"]);
    let mut running = t.clone();
    running["status"] = json!("active");
    let mut blocked = t.clone();
    blocked["blocked"] = json!(true);
    blocked["unmet_blockers"] = json!([{ "short_id": 7, "title": "the blocker" }]);
    let mut closed = t.clone();
    closed["status"] = json!("done");
    let titles: Titles = [(49, "stopped one".to_string()), (52, "waits".to_string())]
        .into_iter()
        .collect();
    let undo = |op: &str, restored: Value| json!({ "reverted": { "op": op }, "short_id": 50, "restored": restored });
    vec![
        ("add", added(ctx, &t, n), vec!["added"]),
        (
            "start",
            started(
                ctx,
                &json!({ "auto_stopped": [{ "short_id": 49, "tracked": "PT5M" }] }),
                &running,
                &titles,
                n,
            ),
            vec!["started"],
        ),
        (
            "start again",
            started(
                ctx,
                &json!({ "already_running": true, "interval_started": "2026-09-11T09:00:00Z" }),
                &running,
                &titles,
                n,
            ),
            vec![],
        ),
        (
            "stop",
            stopped(
                ctx,
                &json!({ "interval": "PT5M", "tracked": "PT2H" }),
                &t,
                n,
            ),
            vec!["stopped after 5m", "tracked 2h of 4h"],
        ),
        (
            "done",
            done(
                ctx,
                &json!({ "tracked": "PT1H", "estimate": "PT4H", "unblocked": [52],
                         "completed": "2026-09-11T11:30:00Z" }),
                &closed,
                &titles,
                n,
            ),
            vec!["done today"],
        ),
        (
            "cancel",
            status_changed(
                ctx,
                "cancelled",
                &json!({ "unblocked": [52] }),
                &closed,
                &titles,
                n,
            ),
            vec!["cancelled"],
        ),
        (
            "reopen",
            status_changed(ctx, "reopened", &json!({ "blocked": [52] }), &t, &titles, n),
            vec!["reopened"],
        ),
        (
            "modify",
            modified(ctx, &t, &set(&[("due", json!("x"))]), &[], n),
            vec!["modified", "due 20 Sep"],
        ),
        (
            "tag (the tags the command named)",
            tag_changed(ctx, &json!({}), &t, true, &["bug".into()], n),
            vec!["tagged", "+bug"],
        ),
        (
            "untag",
            tag_changed(ctx, &json!({ "removed": ["bug"] }), &t, false, &[], n),
            vec!["untagged", "+bug"],
        ),
        (
            "dep",
            dep_changed(ctx, &json!({ "inserted": true }), &blocked, true, "7", n),
            vec!["blocked by #7"],
        ),
        (
            "dep again",
            dep_changed(ctx, &json!({ "inserted": false }), &blocked, true, "7", n),
            vec![],
        ),
        (
            "undep, still blocked",
            dep_changed(ctx, &json!({ "blocked": true }), &blocked, false, "1", n),
            vec!["no longer waits on #1"],
        ),
        (
            "annotate",
            annotated(ctx, &json!({ "annotation": { "body": "a note" } }), &t, n),
            vec!["annotated"],
        ),
        (
            "unannotate",
            annotation_removed(ctx, &t, n),
            vec!["note removed"],
        ),
        (
            "undo stop",
            undone(
                ctx,
                &undo(
                    "stop",
                    json!({ "interval_started": "2026-09-11T09:00:00Z" }),
                ),
                &running,
                &Titles::new(),
                n,
            ),
            vec!["undid stop"],
        ),
        (
            "undo untag",
            undone(
                ctx,
                &undo("tag.remove", json!({ "tags": ["bug"] })),
                &t,
                &Titles::new(),
                n,
            ),
            vec!["undid untag", "+bug"],
        ),
        (
            "init",
            project_created(
                ctx,
                &json!({ "name": "r", "default": false, "current_default": "w" }),
            ),
            vec!["created"],
        ),
        (
            "use",
            default_switched(ctx, &json!({ "name": "r", "previous": "w" })),
            vec!["now the default"],
        ),
        (
            "archive",
            project_archived(
                ctx,
                &json!({ "name": "r", "default_cleared": false, "open_tasks": 2 }),
            ),
            vec!["archived"],
        ),
        (
            "archive, default cleared",
            project_archived(ctx, &json!({ "name": "r", "default_cleared": true })),
            vec!["archived", "it was your default project"],
        ),
        (
            "import",
            imported(
                ctx,
                &json!({ "imported": 3, "projects_imported": 1, "docs_imported": 0 }),
            ),
            vec!["imported"],
        ),
    ]
}

/// D126 (b), review round 1: on every line but the title, bold is the outcome
/// and what the write changed, under NO_COLOR and in `mono`, where roles carry
/// bold of their own (`danger`, `accent`, `timer.active`). `still blocked by`
/// was bold because `danger` is; `blocked again`, `unblocked` and a moved
/// task's `stopped after` were bold in mono; `tag` bolded a tag the task
/// already had; `done` bolded a total it could not know it had changed.
#[test]
fn bold_is_only_the_outcome_and_the_change_on_every_echo() {
    let mono = Ctx::new(
        theme::builtin("mono").expect("mono is built in"),
        Caps {
            depth: theme::ColorDepth::Truecolor,
            ansi: true,
            unicode: true,
        },
    )
    .with_cols(160);
    let mut wrong = Vec::new();
    for (label, ctx) in [("NO_COLOR", no_color(160)), ("mono", mono)] {
        for (name, out, want) in every_echo(&ctx) {
            // The first line is the title (a project's name); `import` has none.
            let skip = usize::from(name != "import");
            let got: Vec<String> = out.lines().skip(skip).flat_map(bold_runs).collect();
            if got != want {
                wrong.push(format!("{label} {name}: bold {got:?}, want {want:?}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// D126 (h), review round 3: an undone `undep` puts a blocker back and said
/// `waits on #7 again`, leaving the reader to remember what #7 was. Every
/// other line that names another task names it; this one now does too, as a
/// detail, so a narrow terminal cuts the title rather than dropping the id
/// with it.
#[test]
fn an_undone_undep_names_the_blocker_it_put_back() {
    let t = task();
    let result = json!({
        "reverted": { "event": "e1", "op": "dependency.remove", "ts": "t" },
        "short_id": 50,
        "restored": { "depends_on": 7 },
    });
    let titles: Titles = [(7, "Review the migration plan".to_string())]
        .into_iter()
        .collect();
    let out = undone(&unicode(100), &result, &t, &titles, now());
    let line = out.lines().nth(1).expect("a second line");
    assert!(
        line.contains("waits on #7 again") && line.contains("Review the migration plan"),
        "{out}"
    );
    // Narrow: the id survives and the title is cut, not dropped with it.
    let narrow = undone(&unicode(46), &result, &t, &titles, now());
    let line = narrow.lines().nth(1).expect("a second line");
    assert!(line.contains("#7"), "the blocker's id went: {narrow}");
    assert!(
        !line.contains("Review the migration plan"),
        "nothing was cut at 46 columns: {narrow}"
    );
}

/// D126, review round 3: a `modify` that changed only the title said
/// `modified` and nothing else on a terminal. The new title is line 1, but
/// line 1's title is bold on EVERY echo — its own role — so nothing on the
/// card marked which field had moved. `renamed` is that mark; the title
/// itself is not repeated (rule 11).
#[test]
fn a_title_only_modify_says_which_field_moved() {
    let mut t = task();
    t["title"] = json!("Renew the TLS certificate");
    let out = modified(
        &unicode(100),
        &t,
        &set(&[("title", json!("Renew the TLS certificate"))]),
        &[],
        now(),
    );
    let line = out.lines().nth(1).expect("a second line");
    assert!(line.contains("renamed"), "{out}");
    // Once, not twice: the title is line 1 and is not repeated below it.
    assert_eq!(
        out.matches("Renew the TLS certificate").count(),
        1,
        "the title was said twice: {out}"
    );
    // And it is the bold, so the mark survives NO_COLOR.
    let no = modified(
        &no_color(100),
        &t,
        &set(&[("title", json!("Renew the TLS certificate"))]),
        &[],
        now(),
    );
    let line2 = no.lines().nth(1).expect("a second line");
    assert_eq!(bold_runs(line2), ["modified", "renamed"], "{no:?}");
}

/// D126 (b), review round 3: the case the every-echo guard above could not
/// see. Its fixture is neither overdue nor in the top urgency band, so the two
/// roles that are bold by default and paint facts a write never touched —
/// `overdue` on a late deadline, the ramp's top band on the figure — were
/// never exercised. Under `NO_COLOR` a `tag` on an overdue, 18.1-urgency task
/// bolded `▄▄▄▄ 18.1` and `2d ago` and NOT the tag it had just added, which is
/// the one thing it changed.
#[test]
fn bold_is_the_write_on_an_overdue_task_in_the_danger_band() {
    let t = json!({
        "short_id": 48, "title": "Renew the TLS certificate",
        "status": "pending", "priority": "H", "urgency": 18.1,
        "project": "work", "due": "2026-09-09T09:00:00Z",
        "tags": ["ops"], "_rev": 2, "blocked": false, "unmet_blockers": [],
    });
    let mono = Ctx::new(
        theme::builtin("mono").expect("mono is built in"),
        Caps {
            depth: theme::ColorDepth::Truecolor,
            ansi: true,
            unicode: true,
        },
    )
    .with_cols(120);
    let line2 = |out: String| out.lines().nth(1).unwrap().to_string();
    for (label, ctx) in [("NO_COLOR", no_color(120)), ("mono", mono)] {
        // The deadline is two days past and the score is in the top band, and
        // this write touched neither.
        let tagged = line2(tag_changed(
            &ctx,
            &json!({}),
            &t,
            true,
            &["urgent".into()],
            now(),
        ));
        assert_eq!(
            bold_runs(&tagged),
            ["tagged", "+urgent"],
            "{label}: {tagged:?}"
        );
        assert!(tagged.contains("2d ago"), "{label}: {tagged:?}");
        assert!(tagged.contains("18.1"), "{label}: {tagged:?}");

        // And a start, which changes nothing a fact on the line carries.
        let started = line2(started(&ctx, &json!({}), &t, &Titles::new(), now()));
        assert_eq!(bold_runs(&started), ["started"], "{label}: {started:?}");
    }
}

/// D126: the path without Unicode prints the program's own glyphs in ASCII.
/// It printed `·` and `—`, and only `modify` was checked.
#[test]
fn without_unicode_every_echo_is_ascii() {
    let legacy = Ctx::new(
        theme::default_theme(),
        Caps {
            depth: theme::ColorDepth::Ansi16,
            ansi: true,
            unicode: false,
        },
    )
    .with_cols(160);
    for ctx in [plain(160), legacy] {
        for (name, out, _) in every_echo(&ctx) {
            let text: String = out
                .split('\u{1b}')
                .enumerate()
                .map(|(i, s)| {
                    if i == 0 {
                        s
                    } else {
                        s.split_once('m').map_or(s, |(_, r)| r)
                    }
                })
                .collect();
            assert!(text.is_ascii(), "{name} is not ASCII:\n{text}");
        }
    }
    assert!(tokens_note("no counts; x", 200, false).unwrap().is_ascii());
    assert!(export_note(2, 200, false).is_ascii());
}

/// A terminal that cannot draw Unicode (a legacy Windows console, colour and
/// no VT glyphs) is still a terminal with a width: the card fits it. The path
/// used to be chosen by "no Unicode", which sent it the unfitted pipe layout.
#[test]
fn a_terminal_without_unicode_still_fits_the_card() {
    let legacy = Ctx::new(
        theme::default_theme(),
        Caps {
            depth: theme::ColorDepth::Ansi16,
            ansi: true,
            unicode: false,
        },
    )
    .with_cols(40);
    for (name, out, _) in every_echo(&legacy) {
        for l in out.lines() {
            let shown: String = l
                .split('\u{1b}')
                .enumerate()
                .map(|(i, s)| {
                    if i == 0 {
                        s
                    } else {
                        s.split_once('m').map_or(s, |(_, r)| r)
                    }
                })
                .collect();
            assert!(
                width(&shown) <= 40,
                "{name}: {} cells: {shown:?}",
                width(&shown)
            );
        }
    }
}

/// Plain has no bold, so it has to say in words what `modify` set; the old
/// echo listed `due <- … / priority <- M` and the first D126 plain line did
/// not say which of its facts were the change.
#[test]
fn plain_modify_names_what_it_set() {
    let out = modified(
        &plain(100),
        &task(),
        &set(&[("due", json!("x")), ("priority", json!("H"))]),
        &["urgent".to_string()],
        now(),
    );
    let l = out.lines().nth(1).unwrap();
    assert!(
        l.starts_with("modified   set priority H, due tomorrow 17:00, tags +bug +urgent"),
        "{out}"
    );
}

/// A total is not rounded into a false equality: 3h41m against a 4h estimate
/// read `tracked 4h of 4h`, which says the estimate is used up.
#[test]
fn a_tracked_total_is_not_rounded_into_its_estimate() {
    let out = stopped(
        &unicode(120),
        &json!({ "interval": "PT52M", "tracked": "PT3H41M" }),
        &task(),
        now(),
    );
    assert!(
        out.contains("stopped after 52m   tracked 3h41 of 4h"),
        "{out}"
    );
}

/// `rev` is what `--expected-rev` needs (#188), so a narrow terminal keeps it
/// and lets `list`'s context go first.
#[test]
fn modify_keeps_rev_on_a_narrow_terminal() {
    let out = modified(
        &unicode(40),
        &task(),
        &set(&[("due", json!("x"))]),
        &[],
        now(),
    );
    assert!(out.lines().nth(1).unwrap().contains("rev 3"), "{out}");
    for l in out.lines() {
        assert!(width(l) <= 40, "{} cells: {l:?}", width(l));
    }
}

/// `pack` lays a record's facts too (a project is not a task, so no rail): a
/// line too long continues under the facts, cut nowhere.
#[test]
fn a_record_too_long_for_one_line_continues_under_itself() {
    let out = project_archived(
        &unicode(40),
        &json!({ "name": "prive.klussen", "default_cleared": true, "open_tasks": 12,
                 "open_overdue": 3 }),
    );
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines.len() >= 4, "{out}");
    for l in &lines {
        assert!(width(l) <= 40, "{} cells: {l:?}", width(l));
    }
    // Wrapped at words where a clause is longer than the room, never cut.
    let flat = out.split_whitespace().collect::<Vec<_>>().join(" ");
    for piece in [
        "12 open tasks left in it, 3 overdue",
        "it was your default project",
        "tasqx use <project> sets another",
    ] {
        assert!(flat.contains(piece), "{piece} went: {out}");
    }
    let indent = " ".repeat(width("archived") + 3);
    assert!(lines[2..].iter().all(|l| l.starts_with(&indent)), "{out}");
}

// ---- review round 2 ------------------------------------------------------

/// D126 (d): both sides of `of` at the same precision. The estimate was
/// rounded to one unit beside an exact total: 3h41 against a 3h30 estimate
/// read `tracked 3h41 of 4h`, under the estimate when it was 11m over.
#[test]
fn a_total_and_its_estimate_are_spelled_alike() {
    let ctx = unicode(120);
    let line = |tracked: &str, est: &str| {
        let mut t = task();
        t["estimate"] = json!(est);
        stopped(
            &ctx,
            &json!({ "interval": "PT5M", "tracked": tracked }),
            &t,
            now(),
        )
    };
    let over = line("PT3H41M", "PT3H30M");
    assert!(over.contains("tracked 3h41 of 3h30"), "{over}");
    let over = line("PT1H40M", "PT1H30M");
    assert!(over.contains("tracked 1h40 of 1h30"), "{over}");
    let under = line("PT3H41M", "PT4H");
    assert!(under.contains("tracked 3h41 of 4h"), "{under}");
}

/// Rule 11: a total that reads the same as the interval beside it is the
/// same fact twice, and is left off.
#[test]
fn a_total_that_reads_like_its_interval_is_not_repeated() {
    let out = stopped(
        &unicode(120),
        &json!({ "interval": "PT2H0M10S", "tracked": "PT2H0M50S" }),
        &task(),
        now(),
    );
    assert!(out.contains("stopped after 2h"), "{out}");
    assert!(!out.contains("tracked"), "{out}");
}

/// D126 (h): `⊘ still blocked by #N · <title>`, at 60 columns with a long
/// title: the title is cut with an ellipsis, the way a moved task's title is,
/// and not dropped whole.
#[test]
fn a_blockers_title_is_cut_not_dropped() {
    let long = "Write the migration guide for the SDK and every language binding it ships";
    let mut t = task();
    t["blocked"] = json!(true);
    t["unmet_blockers"] = json!([{ "short_id": 53, "title": long }]);
    let undep = dep_changed(
        &unicode(60),
        &json!({ "blocked": true }),
        &t,
        false,
        "51",
        now(),
    );
    let dep = dep_changed(
        &unicode(60),
        &json!({ "inserted": true }),
        &t,
        true,
        "53",
        now(),
    );
    for out in [&undep, &dep] {
        let l = out.lines().nth(1).unwrap();
        assert!(l.contains("blocked by #53 · Write the"), "{out}");
        assert!(l.contains('…'), "cut, with an ellipsis: {out}");
        for l in out.lines() {
            assert!(width(l) <= 60, "{} cells: {l:?}", width(l));
        }
    }
}

/// D21 survives a long name: `init` keeps the command that moves the default.
#[test]
fn init_keeps_the_command_that_moves_the_default() {
    let name = "a-project-with-a-name-long-enough-to-crowd-the-line";
    let out = project_created(
        &unicode(40),
        &json!({ "name": name, "default": false, "current_default": "work" }),
    );
    let flat = out.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(flat.contains(&format!("tasqx use \"{name}\"")), "{out}");
}

/// A changed fact is bold and never dim: `mono` paints `project` dim, and
/// bold on top of dim rendered dim, so a changed project was invisible.
#[test]
fn a_changed_fact_is_never_dim() {
    let mono = Ctx::new(
        theme::builtin("mono").expect("mono is built in"),
        Caps {
            depth: theme::ColorDepth::Truecolor,
            ansi: true,
            unicode: true,
        },
    )
    .with_cols(160);
    let out = modified(
        &mono,
        &task(),
        &set(&[("project", json!("mobile"))]),
        &[],
        now(),
    );
    let line = out.lines().nth(1).unwrap();
    for sgr in line.split('\u{1b}').skip(1) {
        let codes: Vec<&str> = sgr
            .trim_start_matches('[')
            .split('m')
            .next()
            .unwrap_or("")
            .split(';')
            .collect();
        assert!(
            !(codes.contains(&"1") && codes.contains(&"2")),
            "bold and dim at once: {line:?}"
        );
    }
}

/// Off a terminal `modify` names every change in words, a new title and a
/// cleared field included: it printed `modified   H 17.2 …` for a new title,
/// and `set remind cleared` for a clear.
#[test]
fn plain_modify_names_a_new_title_and_a_cleared_field() {
    let retitled = modified(
        &plain(100),
        &task(),
        &set(&[("title", json!("x"))]),
        &[],
        now(),
    );
    assert!(
        retitled
            .lines()
            .nth(1)
            .unwrap()
            .starts_with("modified   set title"),
        "{retitled}"
    );
    let cleared = modified(
        &plain(100),
        &task(),
        &set(&[("remind", Value::Null)]),
        &[],
        now(),
    );
    let l = cleared.lines().nth(1).unwrap();
    assert!(l.starts_with("modified   cleared remind"), "{cleared}");
    assert!(!l.contains("set"), "{cleared}");
}

/// Off a terminal an undone untag says which tag came back.
#[test]
fn plain_undo_of_an_untag_says_which_tag_came_back() {
    let mut t = task();
    t["tags"] = json!(["docs", "urgent"]);
    let result = json!({
        "reverted": { "op": "tag.remove" }, "short_id": 50,
        "restored": { "tags": ["urgent"] },
    });
    let out = undone(&plain(100), &result, &t, &Titles::new(), now());
    assert!(
        out.lines()
            .nth(1)
            .unwrap()
            .starts_with("undid untag   +urgent back"),
        "{out}"
    );
}

/// A zero is not a fact (D126 c): a fresh task with no priority and no
/// deadline has urgency 0, and every echo drew `- ▁▁▁▁ 0.0` for it.
#[test]
fn a_zero_urgency_is_not_a_fact() {
    let mut t = task();
    t["urgency"] = json!(0.0);
    t["priority"] = Value::Null;
    for out in [
        added(&unicode(120), &t, now()),
        added(&plain(120), &t, now()),
        status_changed(
            &unicode(120),
            "reopened",
            &json!({}),
            &t,
            &Titles::new(),
            now(),
        ),
    ] {
        let l = out.lines().nth(1).unwrap();
        assert!(!l.contains("0.0") && !l.contains('▁'), "{out}");
    }
}

/// Rule 3 on `modify`'s reminder: an absolute reminder is a day, not the
/// stored instant (`remind 2026-09-12T00:00:00Z`).
#[test]
fn modify_spells_an_absolute_reminder_as_a_day() {
    let mut t = task();
    t["remind"] = json!("2026-09-12T09:00:00Z");
    for ctx in [unicode(120), plain(120)] {
        let out = modified(&ctx, &t, &set(&[("remind", json!("x"))]), &[], now());
        assert!(out.contains("remind tomorrow 09:00"), "{out}");
        assert!(!out.contains("2026-"), "{out}");
    }
    t["remind"] = json!("-1h");
    let out = modified(
        &unicode(120),
        &t,
        &set(&[("remind", json!("x"))]),
        &[],
        now(),
    );
    assert!(
        out.contains("remind -1h"),
        "an offset stays an offset: {out}"
    );
}
