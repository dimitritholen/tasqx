//! The report page (D212) over a synthetic store: an export document with
//! fixed instants, imported into an in-memory engine and rendered at a fixed
//! `now`, so every number below is the same on any calendar day.

use super::*;
use crate::theme;

/// A Wednesday afternoon. The default period is 1–7 Oct, the prior one
/// 24–30 Sep.
const NOW: &str = "2026-10-07T16:00:00Z";

fn uuid(short: i64) -> String {
    format!("00000000-0000-7000-8000-{short:012}")
}

struct Fx {
    tasks: Vec<Value>,
    /// `(ts, uuid, op, payload)` — ids are assigned in time order on build,
    /// because the engine replays the log by event id.
    events: Vec<(String, String, &'static str, Value)>,
}

impl Fx {
    #[allow(clippy::too_many_arguments)]
    fn task(
        &mut self,
        short: i64,
        title: &str,
        project: &str,
        status: &str,
        created: &str,
        completed: Option<&str>,
        extra: Value,
    ) {
        let mut t = json!({
            "id": uuid(short), "short_id": short, "title": title, "project": project,
            "status": status, "created": created, "modified": completed.unwrap_or(created),
            "completed": completed, "tags": [], "depends_on": [], "annotations": [],
            "checks": [], "priority": null, "due": null, "estimate": null,
        });
        if let (Some(obj), Some(more)) = (t.as_object_mut(), extra.as_object()) {
            for (k, v) in more {
                obj.insert(k.clone(), v.clone());
            }
        }
        self.events.push((
            created.to_string(),
            uuid(short),
            "add",
            json!({ "title": title }),
        ));
        if let Some(c) = completed {
            let op = if status == "cancelled" {
                "cancel"
            } else {
                "done"
            };
            self.events
                .push((c.to_string(), uuid(short), op, json!({ "completed": c })));
        }
        self.tasks.push(t);
    }

    fn event(&mut self, short: i64, op: &'static str, ts: &str, payload: Value) {
        self.events.push((ts.to_string(), uuid(short), op, payload));
    }

    fn build(mut self) -> Value {
        self.events.sort_by(|a, b| a.0.cmp(&b.0));
        let events: Vec<Value> = self
            .events
            .into_iter()
            .enumerate()
            .map(|(i, (ts, id, op, payload))| {
                json!({
                    "id": format!("00000001-0000-7000-8000-{:012}", i + 1),
                    "entity": "task", "entity_id": id, "op": op, "ts": ts,
                    "actor": "user", "payload": payload,
                })
            })
            .collect();
        json!({
            "projects": [
                { "id": "00000002-0000-7000-8000-000000000001", "name": "alpha",
                  "created": "2026-09-01T00:00:00Z", "archived": false, "description": null },
                { "id": "00000002-0000-7000-8000-000000000002", "name": "beta",
                  "created": "2026-09-01T00:00:00Z", "archived": false, "description": null },
            ],
            "default_project": "alpha",
            "tasks": self.tasks,
            "events": events,
        })
    }
}

/// The store every test below reads. Counts the tests rely on, worked out by
/// hand from these rows:
///
/// * done 1–7 Oct: #1 (6 Oct), #6 (4 Oct) = 2; 24–30 Sep: #5 = 1
/// * added 1–7 Oct: #8 = 1; 24–30 Sep: #2, #3, #4 = 3 (#7 is cancelled)
/// * open at the end of 7 Oct: #2 #3 #4 #8 = 4; of 30 Sep: #1 #2 #3 #4 #6 #7 = 6
/// * blocked now: #4 (on #3) and #8 (on #3, across projects); overdue: #3
fn fixture() -> Value {
    let mut fx = Fx {
        tasks: Vec::new(),
        events: Vec::new(),
    };
    fx.task(
        1,
        "Ship the alpha export",
        "alpha",
        "done",
        "2026-09-20T09:00:00Z",
        Some("2026-10-06T10:00:00Z"),
        json!({ "estimate": "PT1H", "tracked_seconds": 5400,
                "checks": [{ "id": "00000003-0000-7000-8000-000000000001", "body": "exports open",
                             "state": "open", "position": 0,
                             "created": "2026-09-20T09:00:00Z", "modified": "2026-09-20T09:00:00Z" }] }),
    );
    fx.event(1, "start", "2026-10-06T08:00:00Z", json!({}));
    fx.task(
        2,
        "Alpha in flight",
        "alpha",
        "active",
        "2026-09-25T09:00:00Z",
        None,
        json!({}),
    );
    fx.event(2, "start", "2026-09-28T09:00:00Z", json!({}));
    fx.task(
        3,
        "Alpha blocker task",
        "alpha",
        "pending",
        "2026-09-26T09:00:00Z",
        None,
        json!({ "priority": "H", "due": "2026-10-03T00:00:00Z" }),
    );
    fx.task(
        4,
        "Alpha waits on blocker",
        "alpha",
        "pending",
        "2026-09-27T09:00:00Z",
        None,
        json!({ "depends_on": [uuid(3)] }),
    );
    fx.task(
        5,
        "Beta done last week",
        "beta",
        "done",
        "2026-09-22T09:00:00Z",
        Some("2026-09-25T09:00:00Z"),
        json!({ "estimate": "PT2H", "tracked_seconds": 3600 }),
    );
    fx.task(
        6,
        "Beta reopened done",
        "beta",
        "done",
        "2026-09-23T09:00:00Z",
        Some("2026-10-04T09:00:00Z"),
        json!({ "estimate": "PT30M", "tracked_seconds": 1800 }),
    );
    fx.event(6, "done", "2026-10-02T09:00:00Z", json!({}));
    fx.event(
        6,
        "reopen",
        "2026-10-03T09:00:00Z",
        json!({ "from": "done" }),
    );
    fx.task(
        7,
        "Beta cancelled thing",
        "beta",
        "cancelled",
        "2026-09-29T09:00:00Z",
        Some("2026-10-05T09:00:00Z"),
        json!({ "completed": null }),
    );
    fx.event(7, "start", "2026-09-30T09:00:00Z", json!({}));
    fx.task(
        8,
        "Beta waits across projects",
        "beta",
        "pending",
        "2026-10-02T09:00:00Z",
        None,
        json!({ "depends_on": [uuid(3)],
                "annotations": [{ "id": "00000004-0000-7000-8000-000000000001",
                                  "created": "2026-10-02T10:00:00Z",
                                  "body": "SECRET-ANNOTATION-BODY for a client" }] }),
    );
    // Old, finished filler, so the store is bigger than the 160 panels the
    // page used to be capped at. None of it lands in either period.
    for n in 9..=208 {
        let title = if n == 200 {
            "Needle in the haystack".to_string()
        } else {
            format!("Filler task {n}")
        };
        fx.task(
            n,
            &title,
            "beta",
            "done",
            "2026-09-21T09:00:00Z",
            Some("2026-09-22T09:00:00Z"),
            json!({}),
        );
    }
    fx.build()
}

fn engine() -> Engine {
    let e = Engine::open_in_memory().unwrap();
    dispatch(&e, "store.import", &fixture()).expect("the fixture imports");
    e
}

fn now() -> jiff::Timestamp {
    NOW.parse().unwrap()
}

fn page_with(params: Value, opts: Options) -> String {
    let th = theme::builtin("nord").unwrap();
    generate_at(&engine(), &th, &params, opts, now()).expect("the page renders")
}

fn page() -> String {
    page_with(json!({ "group_by": "project" }), Options::default())
}

/// The markup of one scope's block inside one section: from its
/// `data-sc="…"` to the next scope block or the end of the section.
fn block<'a>(doc: &'a str, section: &str, scope: &str) -> &'a str {
    let at = doc
        .find(&format!("id=\"{section}\""))
        .unwrap_or_else(|| panic!("no section {section}"));
    let sec = &doc[at..];
    let sec = &sec[..sec.find("</section>").unwrap_or(sec.len())];
    let i = sec
        .find(&format!("data-sc=\"{scope}\""))
        .unwrap_or_else(|| panic!("no scope {scope} in {section}"));
    let rest = &sec[i + 1..];
    let j = rest.find("data-sc=\"").unwrap_or(rest.len());
    &sec[i..=i + j]
}

/// The big number of band metric `key` in `scope`.
fn metric(doc: &str, scope: &str, key: &str) -> String {
    let b = block(doc, "band", scope);
    let at = b
        .find(&format!("data-k=\"{key}\""))
        .unwrap_or_else(|| panic!("no metric {key}"));
    let m = &b[at..];
    let v = &m[m.find("class=\"mval\">").unwrap() + "class=\"mval\">".len()..];
    v[..v.find('<').unwrap()].to_string()
}

fn metric_delta(doc: &str, scope: &str, key: &str) -> String {
    let b = block(doc, "band", scope);
    let m = &b[b.find(&format!("data-k=\"{key}\"")).unwrap()..];
    let d = &m[m.find("class=\"delta").unwrap()..];
    let d = &d[d.find('>').unwrap() + 1..];
    d[..d.find('<').unwrap()].to_string()
}

#[test]
fn the_page_leads_with_change_then_standup_grid_flow_outcomes_and_search() {
    let doc = page();
    let at = |id: &str| {
        doc.find(&format!("id=\"{id}\""))
            .unwrap_or_else(|| panic!("no #{id}"))
    };
    let order = [
        at("lede"),
        at("band"),
        at("standup"),
        at("projects"),
        at("flow"),
        at("outcomes"),
        at("search"),
    ];
    assert!(
        order.windows(2).all(|w| w[0] < w[1]),
        "sections out of order: {order:?}"
    );
}

#[test]
fn the_band_counts_the_period_against_the_prior_one() {
    let doc = page();
    assert_eq!(metric(&doc, "*", "done"), "2");
    assert_eq!(metric_delta(&doc, "*", "done"), "+1");
    assert_eq!(metric(&doc, "*", "added"), "1");
    assert_eq!(metric_delta(&doc, "*", "added"), "−2");
    assert_eq!(metric(&doc, "*", "net"), "−2");
    assert_eq!(metric(&doc, "*", "blocked"), "2");
    assert_eq!(metric(&doc, "*", "overdue"), "1");
    let band = block(&doc, "band", "*");
    assert!(band.contains("open 4"), "the net cell names the open count");
    assert!(
        doc.contains("Store started 20 Sep 2026"),
        "the band states the store's start"
    );
}

#[test]
fn the_window_follows_since_and_until() {
    let doc = page_with(
        json!({ "group_by": "project", "since": "2026-09-24T00:00:00Z",
                "until": "2026-10-01T00:00:00Z" }),
        Options::default(),
    );
    assert_eq!(metric(&doc, "*", "done"), "1", "only #5 closed 24–30 Sep");
    assert_eq!(metric(&doc, "*", "added"), "3");
    assert!(
        doc.contains("24 Sep – 30 Sep 2026"),
        "the header names the window"
    );
}

#[test]
fn an_until_before_since_is_refused() {
    let th = theme::builtin("nord").unwrap();
    let err = generate_at(
        &engine(),
        &th,
        &json!({ "since": "2026-10-05T00:00:00Z", "until": "2026-10-01T00:00:00Z" }),
        Options::default(),
        now(),
    )
    .unwrap_err();
    assert!(err.message.contains("until"), "{}", err.message);
}

#[test]
fn the_lede_is_one_computed_sentence() {
    let doc = page();
    let lede = block(&doc, "lede", "*");
    assert!(lede.contains("against the prior 7 days"), "{lede}");
}

#[test]
fn standup_answers_done_yesterday_in_progress_blocked_and_next() {
    let doc = page();
    let su = block(&doc, "standup", "*");
    let part = |h: &str| {
        let i = su.find(h).unwrap_or_else(|| panic!("no {h} list"));
        let rest = &su[i..];
        &rest[..rest[1..].find("<h3").map_or(rest.len(), |j| j + 1)]
    };
    let done = part("Done yesterday");
    assert!(done.contains("Ship the alpha export"), "{done}");
    assert!(done.contains("1.5h tracked of 1h"), "{done}");
    assert!(done.contains("unproven"), "{done}");
    let wip = part("In progress");
    assert!(
        wip.contains("Alpha in flight") && wip.contains("stale"),
        "{wip}"
    );
    let blocked = part("Blocked");
    assert!(
        blocked.contains("Alpha waits on blocker") && blocked.contains("Alpha blocker task"),
        "the blocked row names its blocker by title: {blocked}"
    );
    let next = part("Next");
    assert!(
        next.contains("Alpha blocker task") && next.contains("overdue by"),
        "{next}"
    );
    assert!(
        !next.contains("Alpha waits on blocker"),
        "a blocked task is not next"
    );
}

#[test]
fn the_project_grid_has_a_row_per_project_and_a_total() {
    let doc = page();
    let grid = &doc[doc.find("id=\"projects\"").unwrap()..];
    let grid = &grid[..grid.find("</section>").unwrap()];
    let row = |p: &str| {
        let i = grid
            .find(&format!("<tr data-p=\"{p}\""))
            .unwrap_or_else(|| panic!("no row {p}"));
        let r = &grid[i..];
        &r[..r.find("</tr>").unwrap()]
    };
    let alpha = row("alpha");
    assert!(
        alpha.contains("<td>3</td>"),
        "alpha has three open: {alpha}"
    );
    assert!(
        alpha.contains("1.50×"),
        "alpha's tracked ÷ estimate: {alpha}"
    );
    assert!(row("beta").contains("<td>1</td>"));
    assert!(grid.contains("<tfoot>") && grid.contains("All projects"));
}

#[test]
fn every_section_is_rendered_per_project_scope_and_only_all_shows_without_a_script() {
    let doc = page();
    assert_eq!(metric(&doc, "alpha", "done"), "1");
    assert_eq!(metric(&doc, "beta", "done"), "1");
    for section in ["lede", "band", "standup", "flow", "outcomes"] {
        let b = block(&doc, section, "alpha");
        assert!(
            b.starts_with("data-sc=\"alpha\" hidden"),
            "{section}'s project scope must start hidden: {}",
            &b[..b.len().min(60)]
        );
        let all = block(&doc, section, "*");
        assert!(!all.starts_with("data-sc=\"*\" hidden"), "{section}");
    }
}

#[test]
fn annotation_bodies_stay_out_of_the_file_unless_asked_for() {
    let doc = page();
    assert!(
        !doc.contains("SECRET-ANNOTATION-BODY"),
        "an annotation body was embedded by default"
    );
    let with = page_with(
        json!({ "group_by": "project" }),
        Options {
            with_notes: true,
            all: false,
        },
    );
    assert!(with.contains("SECRET-ANNOTATION-BODY"));
    assert!(
        with.contains("id=\"notes\""),
        "with notes, a toggle shows them"
    );
    assert!(
        !doc.contains("id=\"notes\""),
        "no toggle for bodies that are not there"
    );
}

#[test]
fn search_indexes_every_task_and_the_page_has_no_panels() {
    let doc = page();
    assert!(doc.contains("Needle in the haystack"));
    assert!(!doc.contains("class=\"detail\""), "the 160 panels are gone");
    assert!(
        !doc.contains("Beta cancelled thing\""),
        "cancelled is out by default"
    );
    let all = page_with(
        json!({ "group_by": "project", "all": true }),
        Options {
            with_notes: false,
            all: true,
        },
    );
    assert!(
        all.contains("Beta cancelled thing"),
        "--all puts cancelled work in the index"
    );
}

#[test]
fn outcomes_read_the_engines_numbers() {
    let doc = page();
    let e = engine();
    let o = dispatch(
        &e,
        "report.outcomes",
        &json!({ "group_by": "project", "since": "2026-10-01T00:00:00Z", "until": NOW }),
    )
    .unwrap();
    let sum = |f: &dyn Fn(&Value) -> i64| -> i64 { array_at(&o, "groups").iter().map(f).sum() };
    let rework = sum(&|g| g["rework"]["count"].as_i64().unwrap());
    let cancelled = sum(&|g| g["closed"].as_i64().unwrap() - g["completions"].as_i64().unwrap());
    let unproven = sum(&|g| g["unproven"]["count"].as_i64().unwrap());
    assert_eq!(
        (rework, cancelled, unproven),
        (1, 1, 1),
        "the fixture's own"
    );
    let out = block(&doc, "outcomes", "*");
    for (k, n) in [
        ("reopened", rework),
        ("cancelled", cancelled),
        ("unproven", unproven),
    ] {
        assert!(
            out.contains(&format!("data-k=\"{k}\"><div class=\"mlabel\">")),
            "no {k} stat"
        );
        let at = out.find(&format!("data-k=\"{k}\"")).unwrap();
        let v = &out[at..];
        let v = &v[v.find("class=\"mval\">").unwrap() + 13..];
        assert_eq!(&v[..v.find('<').unwrap()], n.to_string(), "{k}");
    }
    assert!(out.contains("Median 1.25×"), "{out}");
}

#[test]
fn a_one_project_filter_is_a_single_project_page_that_names_no_outside_task() {
    let doc = page_with(
        json!({ "group_by": "project", "filter": "project:beta" }),
        Options::default(),
    );
    assert!(doc.contains("Beta waits across projects"));
    for leak in ["Alpha blocker task", "Ship the alpha export", "alpha"] {
        assert!(!doc.contains(leak), "{leak:?} leaked into beta's page");
    }
    assert!(doc.contains("a task outside this report"));
    assert!(
        !doc.contains("data-sc=\"beta\""),
        "one project needs no scopes"
    );
}

#[test]
fn the_page_passes_the_self_containment_guard() {
    let doc = page();
    assert_eq!(guard::violations(&doc), Vec::<String>::new());
    assert_eq!(doc.matches("<script>").count(), 1);
}

#[test]
fn the_chart_is_clipped_to_the_store_age() {
    let doc = page();
    let flow = block(&doc, "flow", "*");
    // 20 Sep – 7 Oct is 18 days: three 7-day bins, not twelve empty weeks.
    assert_eq!(flow.matches("class=\"done\"").count() / 2, 3, "{flow}");
}

fn page_in(theme_name: &str) -> String {
    let th = theme::builtin(theme_name).unwrap();
    generate_at(
        &engine(),
        &th,
        &json!({ "group_by": "project" }),
        Options::default(),
        now(),
    )
    .unwrap()
}

/// One document, and nothing in it points off the file.
#[test]
fn report_is_one_self_contained_document() {
    let doc = page();
    assert!(doc.starts_with("<!doctype html>"));
    assert!(doc.trim_end().ends_with("</html>"));
    assert_eq!(doc.matches("<html").count(), 1);
    assert!(!doc.contains("http://") && !doc.contains("https://"));
}

/// The guard's own contract: silent on prose, and it bites on every drift
/// class D48b names — each one injected here, so a rule that stops firing
/// is a red test rather than a quiet gap.
#[test]
fn self_containment_guard_judges_markup_not_prose() {
    let prose = "<p>https://x.example @import url(x.png) pushState fetch( &lt;script&gt; src=</p>\
                 <style>.a { fill: url(#ramp); }</style>\
                 <svg><defs><linearGradient id=\"ramp\"/></defs><rect fill=\"url(#ramp)\"/></svg>\
                 <a href=\"#task-1\">t</a>\
                 <script>window.addEventListener('hashchange', () => { if (a < b) {} });</script>";
    assert_eq!(guard::violations(prose), Vec::<String>::new());

    let drifts = [
        ("<link rel=\"stylesheet\" href=\"x.css\">", "a <link>"),
        ("<a href=\"https://x.example\">x</a>", "an external href"),
        ("<a href=\"#\">x</a>", "an empty anchor"),
        ("<img src=\"x.png\">", "a src="),
        ("<style>@import url(x.css);</style>", "a CSS @import"),
        (
            "<style>.a { background: url(x.png); }</style>",
            "a CSS url()",
        ),
        (
            "<div style=\"background: url('x.png')\"></div>",
            "a url() in a style attribute",
        ),
        ("<script>fetch('x')</script>", "fetch in the script"),
        (
            "<script>history.pushState({}, '')</script>",
            "pushState in the script",
        ),
        ("<script>1</script><script>2</script>", "a second script"),
        (
            "<button onclick=\"go()\">x</button>",
            "an inline event handler",
        ),
        ("<iframe></iframe>", "an <iframe>"),
    ];
    for (markup, what) in drifts {
        assert!(
            !guard::violations(markup).is_empty(),
            "the guard missed {what}: {markup}"
        );
    }
}

/// D48b: one script, measured, so growth is a red test.
#[test]
fn the_script_stays_within_its_budget() {
    assert!(
        SCRIPT.len() <= SCRIPT_BUDGET,
        "the script is {} bytes, the budget {SCRIPT_BUDGET}",
        SCRIPT.len()
    );
}

/// A title is text everywhere it lands: the markup and the index.
#[test]
fn user_content_is_escaped_and_never_carries_an_escape_byte() {
    let mut doc = fixture();
    doc["tasks"][2]["title"] = json!("Alpha <b>blocker</b> & \u{1b}]0;HIJACKED\u{7} task");
    let e = Engine::open_in_memory().unwrap();
    dispatch(&e, "store.import", &doc).unwrap();
    let th = theme::builtin("nord").unwrap();
    let page = generate_at(
        &e,
        &th,
        &json!({ "group_by": "project" }),
        Options::default(),
        now(),
    )
    .unwrap();
    assert!(!page.contains("<b>blocker</b>"), "raw markup from a title");
    assert!(page.contains("Alpha &lt;b&gt;blocker&lt;/b&gt; &amp;"));
    assert!(!page.contains('\u{1b}'), "report --html writes to stdout");
    assert!(!page.contains("\\u001b"), "the index kept the escape byte");
}

#[test]
fn html_escaper_strips_terminal_control_bytes() {
    let out = esc("pwn\u{1b}]0;HIJACKED\u{7}\u{1b}[2Jgone");
    assert!(!out.contains('\u{1b}') && !out.contains('\u{7}'), "{out:?}");
    assert!(out.contains("pwn") && out.contains("gone"), "{out:?}");
    assert_eq!(esc("a\tb\nc"), "a\tb\nc");
    assert_eq!(esc("<a & 'b'>"), "&lt;a &amp; &#39;b&#39;&gt;");
}

/// The hex a token is set to inside one token block.
fn token(css: &str, name: &str) -> Rgb {
    let at = css.find(name).unwrap_or_else(|| panic!("{name} missing"));
    let rest = &css[at + name.len()..];
    let hex = rest[..rest.find(';').unwrap()].trim();
    Rgb::parse_hex(hex).unwrap_or_else(|| panic!("{name} is not a hex: {hex:?}"))
}

/// #163, kept: every built-in's roles clear WCAG AA as text on the light
/// scheme's white, and on the dark scheme against the theme's own ground.
#[test]
fn every_builtin_theme_clears_aa_contrast_in_both_schemes() {
    let white = Rgb::new(0xff, 0xff, 0xff);
    for name in theme::BUILTINS {
        let doc = page_in(name);
        let light = &doc[doc.find(":root{--bg:").unwrap()..];
        let light = &light[..light.find('}').unwrap()];
        let dark = &doc[doc.find("[data-theme=\"dark\"]{").unwrap()..];
        let dark = &dark[..dark.find('}').unwrap()];
        let ground = token(dark, "--surface:");
        for role in ["--accent:", "--warn:", "--danger:", "--good:", "--muted:"] {
            let l = contrast_ratio(token(light, role), white);
            assert!(l >= 4.5, "{name} light {role} {l:.2}:1");
            let d = contrast_ratio(token(dark, role), ground);
            assert!(d >= 4.5, "{name} dark {role} {d:.2}:1");
        }
    }
}

/// Light by default, dark when the OS asks, and the switch overrides both;
/// print stays light because both dark blocks are screen-only.
#[test]
fn dark_follows_the_os_unless_the_switch_says_otherwise() {
    let doc = page();
    assert!(doc.contains(
        "@media screen and (prefers-color-scheme:dark){:root:not([data-theme=\"light\"]){"
    ));
    assert!(doc.contains("@media screen{:root[data-theme=\"dark\"]{"));
    assert!(doc.contains("id=\"theme\""));
}

/// mono collapses every role to grey; the judgement must survive in shape.
#[test]
fn mono_keeps_good_and_bad_apart_without_colour() {
    let doc = page_in("mono");
    assert!(doc.contains(".warn{") && doc.contains("border-style:dashed"));
    assert!(doc.contains(".bad{") && doc.contains("font-weight:800"));
    assert!(
        doc.contains("<span class=\"delta bad\">"),
        "the fixture has a worsening figure"
    );
}
