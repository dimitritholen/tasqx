//! The task browser's tests (D55, D100, D123).
//!
//! Several of these were written against the chooser `pick` was before D123
//! and kept their intent through it: the cursor clamps, follows the task
//! through a refilter, never starts a task that is not highlighted, and the
//! screen marks the running task apart from the cursor. What moved is the key
//! that reaches each of those states — the query is behind `/` now, and `s`
//! starts where Enter did — so those tests press the new keys.

use super::*;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
use ratatui::Terminal;
use serde_json::json;

use crate::theme::{self, ColorDepth};

fn caps() -> Caps {
    Caps {
        depth: ColorDepth::Truecolor,
        ansi: true,
        unicode: true,
    }
}

fn ascii() -> Caps {
    Caps {
        depth: ColorDepth::Ansi16,
        ansi: true,
        unicode: false,
    }
}

fn now() -> Timestamp {
    "2026-09-10T12:00:00Z".parse().unwrap()
}

fn task(id: i64, title: &str, project: &str, prio: &str, urgency: f64, tags: &[&str]) -> Value {
    let mut t = json!({
        "short_id": id, "title": title, "project": project,
        "urgency": urgency, "tags": tags, "status": "pending",
    });
    if prio != "-" {
        t["priority"] = json!(prio);
    }
    t
}

fn app_with(tasks: Vec<Value>, caps: Caps, theme: &str) -> App {
    let ctx = Ctx::new(theme::load(theme, None), caps);
    let mut a = App::new(
        tasks.into_iter().map(Row::new).collect(),
        &ctx,
        "@working",
        now(),
    );
    a.observe(100, 24);
    a
}

fn app_of(tasks: Vec<Value>) -> App {
    app_with(tasks, caps(), "nord")
}

/// Four tasks with deliberately overlapping words, so a query that narrows
/// has something to narrow away from.
fn app() -> App {
    app_of(vec![
        task(
            42,
            "Ship the v1 JSON API freeze",
            "work.tasqx",
            "H",
            11.8,
            &["release", "api"],
        ),
        task(43, "Publish API docs", "work.tasqx", "M", 6.0, &["docs"]),
        task(
            47,
            "Write API conformance tests",
            "work.tasqx",
            "M",
            9.4,
            &["api", "test"],
        ),
        task(55, "Draft README quickstart", "home", "L", 4.2, &["docs"]),
    ])
}

fn press(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn typed(app: &mut App, text: &str) {
    for c in text.chars() {
        app.on_key(press(KeyCode::Char(c)));
    }
}

/// `/`, the query, and Enter: a search finished with its filter kept.
fn search(app: &mut App, text: &str) {
    app.on_key(press(KeyCode::Char('/')));
    typed(app, text);
}

fn ids(app: &App) -> Vec<i64> {
    app.matches()
        .iter()
        .map(|i| app.rows()[*i].short_id)
        .collect()
}

fn draw(app: &App, w: u16, h: u16) -> Buffer {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| render(app, f)).unwrap();
    term.backend().buffer().clone()
}

fn line_at(buf: &Buffer, y: u16) -> String {
    (0..buf.area().width)
        .map(|x| buf[(x, y)].symbol())
        .collect::<String>()
        .trim_end()
        .to_string()
}

fn all_text(buf: &Buffer) -> String {
    (0..buf.area().height)
        .map(|y| line_at(buf, y))
        .collect::<Vec<_>>()
        .join("\n")
}

// ---- state machine ----------------------------------------------------------

/// Up/Down must clamp, not wrap and not underflow. `cursor - 1` at the top
/// row is a usize underflow: a panic in debug, an out-of-bounds index in
/// release, both inside a raw-mode alt screen.
#[test]
fn the_cursor_clamps_at_both_ends() {
    let mut a = app();
    assert!(a.on_key(press(KeyCode::Up)).is_none());
    assert_eq!(a.cursor(), 0, "up at the top row must stay put");
    for _ in 0..10 {
        a.on_key(press(KeyCode::Down));
    }
    assert_eq!(a.cursor(), a.matches().len() - 1);
    assert_eq!(a.selected().map(|r| r.short_id), Some(55));
}

/// D123: in the list j/k move and are not letters. Type-to-filter (D100's
/// screen) cost the reader j/k, which is why D121 rejected it for the memory
/// browser; pick now takes the memory browser's shape.
#[test]
fn j_and_k_move_through_the_list() {
    let mut a = app();
    a.on_key(press(KeyCode::Char('j')));
    assert!(a.query.is_empty(), "j typed into a query: {:?}", a.query);
    assert_eq!(a.selected().map(|r| r.short_id), Some(43));
    a.on_key(press(KeyCode::Char('k')));
    assert_eq!(a.selected().map(|r| r.short_id), Some(42));
    a.on_key(press(KeyCode::Char('G')));
    assert_eq!(a.selected().map(|r| r.short_id), Some(55));
    a.on_key(press(KeyCode::Char('g')));
    assert_eq!(a.selected().map(|r| r.short_id), Some(42));
}

/// D123: `/` is the only way into the query, and there every letter is a
/// letter — `j`, `k`, `q` and `s` type themselves rather than moving,
/// quitting or starting a task mid-word.
#[test]
fn slash_opens_the_search_where_every_letter_types() {
    let mut a = app();
    assert_eq!(a.on_key(press(KeyCode::Char('/'))), None);
    assert_eq!(a.mode(), Mode::Search);
    for c in ['j', 'k', 'q', 's'] {
        assert_eq!(a.on_key(press(KeyCode::Char(c))), None, "`{c}` acted");
    }
    assert_eq!(a.query, "jkqs");
    assert_eq!(a.mode(), Mode::Search, "q must not leave the search");
}

/// D123: Enter reads the task and never starts it; `s` starts it.
#[test]
fn enter_reads_the_task_and_s_starts_it() {
    let mut a = app();
    a.on_key(press(KeyCode::Down));
    assert_eq!(
        a.on_key(press(KeyCode::Enter)),
        None,
        "enter acted on the store"
    );
    assert_eq!(a.mode(), Mode::Detail);
    assert_eq!(
        a.wanted(),
        Some(43),
        "the card asks for the task under the cursor"
    );
    assert_eq!(
        a.on_key(press(KeyCode::Char('s'))),
        Some(Action::Start { short_id: 43 }),
        "s on the card starts the task it shows"
    );

    let mut b = app();
    b.on_key(press(KeyCode::Down));
    assert_eq!(
        b.on_key(press(KeyCode::Char('s'))),
        Some(Action::Start { short_id: 43 }),
        "s in the list starts the task under the cursor"
    );
}

/// The card is read once per task, and Esc goes back to the row it came from.
#[test]
fn the_card_is_asked_for_once_and_esc_returns_to_its_row() {
    let mut a = app();
    a.on_key(press(KeyCode::Down));
    a.on_key(press(KeyCode::Enter));
    assert_eq!(a.wanted(), Some(43));
    a.set_detail(
        43,
        Ok(task(43, "Publish API docs", "work.tasqx", "M", 6.0, &[])),
    );
    assert_eq!(a.wanted(), None);
    a.on_key(press(KeyCode::Char('q')));
    assert_eq!(a.mode(), Mode::List, "q on the card goes back, not out");
    assert_eq!(a.selected().map(|r| r.short_id), Some(43));
    a.on_key(press(KeyCode::Down));
    a.on_key(press(KeyCode::Enter));
    assert_eq!(a.wanted(), Some(47), "a different row wants its own card");
}

/// An empty candidate set survives every key and never starts a task. The
/// caller refuses an empty store before opening the screen, so the way in is a
/// search that empties the list.
///
/// #228.12 made Enter on an empty list leave, because on the chooser it did
/// nothing at all. On the browser Enter reads, and there is nothing to read:
/// it stays, under the sentence that names Esc (D123).
#[test]
fn an_empty_list_navigates_and_starts_nothing() {
    let mut a = app_of(Vec::new());
    assert!(a.selected().is_none());
    for code in [
        KeyCode::Up,
        KeyCode::Down,
        KeyCode::Enter,
        KeyCode::Char('s'),
    ] {
        assert!(a.on_key(press(code)).is_none(), "{code:?}");
    }
    assert_eq!(a.mode(), Mode::List, "there was no card to open");
}

/// `s` on a search that matches nothing must not start anything — above all
/// not whatever sat at index 0 of the unfiltered list.
#[test]
fn s_on_a_search_that_matches_nothing_starts_nothing() {
    let mut a = app();
    search(&mut a, "zzzz");
    a.on_key(press(KeyCode::Enter));
    assert!(ids(&a).is_empty(), "the fixture query must match nothing");
    assert_eq!(a.on_key(press(KeyCode::Char('s'))), None);
    assert_eq!(a.on_key(press(KeyCode::Enter)), None);
    assert!(a.selected().is_none());
}

/// The query narrows by SUBSEQUENCE, and whitespace splits it into terms that
/// must all match.
#[test]
fn the_query_narrows_by_subsequence_and_ands_its_terms() {
    let mut a = app();
    assert_eq!(ids(&a), vec![42, 43, 47, 55], "an empty query matches all");
    search(&mut a, "wac");
    assert_eq!(
        ids(&a),
        vec![47],
        "a subsequence of `Write API conformance`"
    );

    // The terms run in the OPPOSITE order to the text, which is what makes
    // this falsifiable: read as one literal subsequence, `test api` matches
    // nothing. Only an intersection of two terms finds #47.
    let mut b = app();
    search(&mut b, "test api");
    assert_eq!(ids(&b), vec![47]);

    let mut c = app();
    search(&mut c, "API");
    let mut got = ids(&c);
    got.sort_unstable();
    assert_eq!(got, vec![42, 43, 47], "matching is case-insensitive");

    let mut d = app();
    search(&mut d, "55");
    assert_eq!(ids(&d), vec![55], "the short_id is searchable");
    let mut e = app();
    search(&mut e, "home");
    assert_eq!(ids(&e), vec![55], "the project is searchable");
}

/// #203: the best match goes on top whatever order the store handed in.
#[test]
fn ranking_promotes_the_contiguous_match_over_a_scattered_one_regardless_of_input_order() {
    let mut a = app_of(vec![
        task(
            10,
            "Random unrelated meeting notes for email marketing",
            "work",
            "H",
            9.9,
            &["misc"],
        ),
        task(
            90,
            "Fix the memory leak in the parser",
            "work",
            "L",
            2.0,
            &["bugfix"],
        ),
    ]);
    search(&mut a, "mem");
    assert_eq!(ids(&a), vec![90, 10]);
}

/// D123, D121's finding brought to pick: the word the reader typed outranks
/// the same letters scattered through another word, and it outranks them from
/// the project too.
#[test]
fn a_query_found_whole_outranks_its_letters_scattered() {
    let mut a = app_of(vec![
        task(1, "Fix punctuation in emails", "work", "H", 9.0, &[]),
        task(2, "House style", "tasqx-tui-restyle", "L", 1.0, &[]),
    ]);
    search(&mut a, "tui");
    assert_eq!(ids(&a), vec![2, 1], "scattered letters outranked the word");
}

/// Narrowing keeps the highlight on the task it was already on (D55).
///
/// The fixture's query moves the highlighted task OFF the top of the ranking
/// and asserts that it did: a cursor that simply reset to 0 would pass a
/// fixture where the task also ranks first, which is how the memory browser's
/// version of this test once went silent (D121).
#[test]
fn narrowing_keeps_the_highlight_on_the_same_task() {
    let mut a = app();
    a.on_key(press(KeyCode::Down));
    a.on_key(press(KeyCode::Down));
    assert_eq!(a.selected().map(|r| r.short_id), Some(47));
    search(&mut a, "s");
    assert_ne!(
        ids(&a)[0],
        47,
        "the fixture no longer moves #47 off the top"
    );
    assert_eq!(
        a.selected().map(|r| r.short_id),
        Some(47),
        "the cursor followed the index"
    );

    // And when the highlighted task falls out of the set, the cursor lands
    // inside the new one rather than past its end.
    typed(&mut a, " home");
    assert_eq!(ids(&a), vec![55]);
    assert_eq!(a.selected().map(|r| r.short_id), Some(55));
}

/// `s` names the highlighted task of the NARROWED list, never the unfiltered
/// row at the same index — which would start an unrelated task.
#[test]
fn s_starts_the_highlighted_task_of_the_narrowed_list() {
    let mut a = app();
    search(&mut a, "s");
    a.on_key(press(KeyCode::Enter));
    a.on_key(press(KeyCode::Down));
    a.on_key(press(KeyCode::Down));
    let expected = ids(&a)[2];
    assert_ne!(expected, 47, "narrowed and unfiltered index 2 must differ");
    assert_eq!(
        a.on_key(press(KeyCode::Char('s'))),
        Some(Action::Start { short_id: expected })
    );
}

/// Enter or Esc leave the search with the filter kept; Esc again clears it,
/// and only then does Esc leave (D121(b)).
#[test]
fn enter_or_esc_leaves_the_search_with_the_filter_kept_and_esc_again_clears_it() {
    for leave in [KeyCode::Enter, KeyCode::Esc] {
        let mut a = app();
        search(&mut a, "api");
        assert_eq!(a.on_key(press(leave)), None);
        assert_eq!(a.mode(), Mode::List);
        assert_eq!(a.query, "api", "{leave:?} dropped the filter");
        assert_eq!(ids(&a).len(), 3);
        assert_eq!(a.on_key(press(KeyCode::Esc)), None, "the first esc clears");
        assert_eq!(ids(&a), vec![42, 43, 47, 55], "clearing restores all");
        assert_eq!(a.on_key(press(KeyCode::Esc)), Some(Action::Cancel));
    }
}

/// Ctrl-N/Ctrl-P move in both modes; Ctrl-C leaves from any state, mid-query
/// included.
#[test]
fn the_control_keys_navigate_and_interrupt() {
    let mut a = app();
    assert!(a.on_key(ctrl('n')).is_none());
    assert_eq!(a.selected().map(|r| r.short_id), Some(43));
    assert!(a.on_key(ctrl('p')).is_none());
    assert_eq!(a.selected().map(|r| r.short_id), Some(42));
    search(&mut a, "api");
    assert_eq!(a.cursor(), 2, "the cursor followed #42 to the bottom");
    a.on_key(ctrl('p'));
    assert_eq!(a.cursor(), 1, "ctrl-p moves while typing");
    a.on_key(ctrl('n'));
    assert_eq!(a.cursor(), 2, "ctrl-n moves while typing");
    assert_eq!(a.query, "api", "a control chord typed its letter");
    assert_eq!(a.on_key(ctrl('c')), Some(Action::Cancel));
}

/// PageUp/PageDown move a screenful — the list's rows at the observed size —
/// and Home/End jump to the ends.
#[test]
fn page_and_home_end_move_by_a_screenful_or_to_the_ends() {
    let mut a = app();
    a.observe(100, 7); // 7 rows less 5 of chrome: 2 list rows
    a.on_key(press(KeyCode::PageDown));
    assert_eq!(a.selected().map(|r| r.short_id), Some(47));
    a.on_key(press(KeyCode::PageDown));
    assert_eq!(a.selected().map(|r| r.short_id), Some(55), "clamped");
    a.on_key(press(KeyCode::PageUp));
    assert_eq!(a.selected().map(|r| r.short_id), Some(43));
    a.on_key(press(KeyCode::Home));
    assert_eq!(a.selected().map(|r| r.short_id), Some(42));
    a.on_key(press(KeyCode::End));
    assert_eq!(a.selected().map(|r| r.short_id), Some(55));
}

/// Ctrl-U and Ctrl-W edit the query the readline way.
#[test]
fn ctrl_u_and_ctrl_w_edit_the_query_the_readline_way() {
    let mut a = app();
    search(&mut a, "publish api docs");
    a.on_key(ctrl('w'));
    assert_eq!(a.query, "publish api ");
    a.on_key(ctrl('w'));
    assert_eq!(a.query, "publish ", "ctrl-w eats the space it exposed");
    a.on_key(ctrl('u'));
    assert!(a.query.is_empty());
    assert_eq!(a.matches().len(), a.rows().len());
}

/// Backspace edits the query and the match list follows it back out.
#[test]
fn backspace_widens_the_match_list_again() {
    let mut a = app();
    search(&mut a, "apix");
    assert!(ids(&a).is_empty());
    a.on_key(press(KeyCode::Backspace));
    assert_eq!(a.query, "api");
    assert_eq!(ids(&a).len(), 3, "the list did not widen back");
}

/// Windows crossterm emits Press AND Release for every keystroke.
#[test]
fn a_key_release_is_not_a_second_key_press() {
    let mut a = app();
    let mut release = press(KeyCode::Char('j'));
    release.kind = KeyEventKind::Release;
    a.on_key(release);
    assert_eq!(a.cursor(), 0, "a Release event moved the cursor");
    search(&mut a, "");
    let mut release = press(KeyCode::Char('a'));
    release.kind = KeyEventKind::Release;
    a.on_key(release);
    assert!(a.query.is_empty(), "a Release event typed a character");
}

/// Task text is untrusted (`store.import`, MCP writes). The search reads only
/// sanitised text, and nothing a title carries reaches the frame as an escape
/// — which matters more than it did, because [`tui::painted_line`] honours
/// SGR: an escape that survived to it would restyle the row.
#[test]
fn a_row_is_searched_and_drawn_only_as_sanitised_text() {
    let a = app_of(vec![task(
        1,
        "quiet\x1b[31mRED title",
        "proj\x1b[2J",
        "H",
        1.0,
        &["t\u{9b}"],
    )]);
    assert!(!a.rows()[0].title.chars().any(char::is_control));
    let mut b = app_of(vec![task(1, "quiet\x1b[31mRED title", "p", "H", 1.0, &[])]);
    search(&mut b, "\u{1b}");
    assert!(
        b.matches().is_empty(),
        "an escape byte must not be searchable"
    );

    let buf = draw(&a, 100, 12);
    let text = all_text(&buf);
    assert!(
        text.contains("quiet[31mRED"),
        "the escape's bytes must arrive as text, not as a style:\n{text}"
    );
    let y = (0..12).find(|y| line_at(&buf, *y).contains("RED")).unwrap();
    let x = (0..100u16)
        .find(|x| buf[(*x, y)].symbol() == "R" && buf[(*x + 1, y)].symbol() == "E")
        .unwrap();
    assert_ne!(buf[(x, y)].fg, Color::Indexed(1), "the title turned red");
}

/// Every key each table advertises changes something, so the bar cannot
/// promise a key the screen ignores (D62's rule, D121(c)).
#[test]
fn every_key_in_the_tables_does_something() {
    let long: Vec<Value> = (0..60)
        .map(|n| json!({ "body": format!("note {n}"), "created": "2026-09-01T00:00:00Z" }))
        .collect();
    let setup = |mode: Mode| {
        let mut a = app();
        a.on_key(press(KeyCode::Char('j')));
        a.on_key(press(KeyCode::Char('j')));
        a.on_key(press(KeyCode::Char('k')));
        match mode {
            Mode::List => {}
            // `a` keeps all four and ranks #43 third, so up and down
            // both have somewhere to go.
            Mode::Search => search(&mut a, "a"),
            Mode::Detail => {
                a.on_key(press(KeyCode::Enter));
                let mut t = task(43, "Publish API docs", "work.tasqx", "M", 6.0, &[]);
                t["annotations"] = json!(long);
                a.set_detail(43, Ok(t));
                a.observe(80, 20);
                for _ in 0..10 {
                    a.on_key(press(KeyCode::Char('j')));
                }
            }
        }
        a
    };
    let key_of = |tok: &str| -> KeyEvent {
        match tok {
            "enter" => press(KeyCode::Enter),
            "esc" => press(KeyCode::Esc),
            "up" => press(KeyCode::Up),
            "down" => press(KeyCode::Down),
            "space" => press(KeyCode::Char(' ')),
            "ctrl-u" => ctrl('u'),
            t if t.chars().count() == 1 => press(KeyCode::Char(t.chars().next().unwrap())),
            t => panic!("the key table names {t:?}, which this test cannot press"),
        }
    };
    let state = |a: &App| (a.mode(), a.cursor(), a.query.clone(), a.scroll);
    // The empty states: a search that matched nothing, kept and still open.
    let empty = |open: bool| {
        let mut a = app();
        search(&mut a, "zzqx");
        if !open {
            a.on_key(press(KeyCode::Enter));
        }
        a
    };
    type State<'a> = (&'a str, &'a [Key], &'a dyn Fn() -> App);
    let tables: [State; 5] = [
        ("list", LIST_KEYS, &|| setup(Mode::List)),
        ("search", SEARCH_KEYS, &|| setup(Mode::Search)),
        ("card", DETAIL_KEYS, &|| setup(Mode::Detail)),
        ("empty list", LIST_EMPTY_KEYS, &|| empty(false)),
        ("empty search", SEARCH_EMPTY_KEYS, &|| empty(true)),
    ];
    for (mode, table, make) in tables {
        for k in table {
            for tok in k.keys.split(" / ") {
                let mut a = make();
                let before = state(&a);
                let action = a.on_key(key_of(tok));
                assert!(
                    action.is_some() || state(&a) != before,
                    "{mode:?}: `{tok}` ({}) did nothing",
                    k.help
                );
            }
        }
    }
}

/// The bar that says what a key does must agree with what it does: the one
/// key with an effect on the store is `s` in both tables that carry it, and
/// Enter is never described as starting anything.
#[test]
fn the_key_tables_say_s_starts_and_enter_reads() {
    for table in [LIST_KEYS, DETAIL_KEYS] {
        let s = table
            .iter()
            .find(|k| k.keys == "s")
            .expect("s is in the table");
        assert!(s.help.contains("start"), "{}", s.help);
    }
    let enter = LIST_KEYS.iter().find(|k| k.keys == "enter").unwrap();
    assert!(!enter.help.contains("start"), "{}", enter.help);
}

// ---- rendering --------------------------------------------------------------

/// D123: a row is `list`'s row, cell for cell. The screen draws what
/// `render::task_table` prints for the same tasks at the same width, behind
/// the three cells of cursor lead — one renderer, so the two cannot drift.
#[test]
fn the_rows_are_lists_rows_cell_for_cell() {
    let mut blocked = task(61, "Waiting on legal", "work", "H", 7.0, &["legal"]);
    blocked["blocked"] = json!(true);
    let mut running = task(62, "Write the release notes", "work", "M", 13.5, &["docs"]);
    running["status"] = json!("active");
    running["due"] = json!("2026-09-11T17:00:00Z");
    let mut late = task(63, "Renew the certificate", "infra", "H", 18.1, &["ops"]);
    late["due"] = json!("2026-09-08T00:00:00Z");
    let tasks = vec![
        late,
        running,
        blocked,
        task(64, "Tidy", "home", "-", 0.2, &[]),
    ];
    for w in [60u16, 80, 100, 140] {
        let a = app_of(tasks.clone());
        let buf = draw(&a, w, 16);
        let ctx = Ctx::new(theme::load("nord", None), caps()).with_cols(w as usize - LEAD);
        let printed = render::task_table(&ctx, &json!({ "tasks": tasks, "count": 4 }), now());
        let printed: Vec<String> = printed
            .lines()
            .skip(2) // the summary and the blank line under it
            .take(5)
            .map(|l| {
                tui::painted_line(l)
                    .spans
                    .iter()
                    .map(|s| s.content.to_string())
                    .collect()
            })
            .collect();
        for (i, want) in printed.iter().enumerate() {
            let got: String = line_at(&buf, 2 + i as u16).chars().skip(LEAD).collect();
            assert_eq!(got.trim_end(), want.trim_end(), "row {i} at {w} columns");
        }
    }
}

/// What `list` shows a reader, the screen shows too: the calendar day, the
/// rail, the gauge and the priority in the urgency cell (rules 3–6).
#[test]
fn a_row_carries_the_date_the_rail_and_the_gauge() {
    let mut running = task(62, "Write the release notes", "work", "M", 13.5, &[]);
    running["status"] = json!("active");
    running["due"] = json!("2026-09-11T17:00:00Z");
    let mut blocked = task(61, "Waiting on legal", "work", "H", 7.0, &[]);
    blocked["blocked"] = json!(true);
    let text = all_text(&draw(&app_of(vec![running, blocked]), 100, 12));
    let r62 = text.lines().find(|l| l.contains("release notes")).unwrap();
    assert!(r62.contains("▶") && r62.contains("tomorrow 17:00"), "{r62}");
    assert!(r62.contains("M ▄▄▄▄ 13.5"), "{r62}");
    let r61 = text.lines().find(|l| l.contains("legal")).unwrap();
    assert!(r61.contains("⊘"), "{r61}");
    assert!(
        text.contains("DUE") && text.contains("TASK"),
        "no column labels:\n{text}"
    );
}

/// Rule 8 on the header: the filter that was asked, and the facts the reader
/// cannot count at a glance — not `4/4`.
#[test]
fn the_header_names_the_filter_and_what_the_set_holds() {
    let mut late = task(63, "Renew the certificate", "infra", "H", 18.1, &[]);
    late["due"] = json!("2026-09-08T00:00:00Z");
    let mut running = task(62, "Write notes", "work", "M", 3.0, &[]);
    running["status"] = json!("active");
    let head = line_at(&draw(&app_of(vec![late, running]), 100, 12), 0);
    for needle in ["pick", "@working", "2 tasks", "1 overdue", "#62 running"] {
        assert!(head.contains(needle), "{needle:?} missing: {head}");
    }
    assert!(!head.contains("2/2"), "{head}");
}

/// The query sits on the header line with how much it kept, so the list does
/// not move when a search opens, and an empty result says so in words.
#[test]
fn the_query_and_the_match_count_are_drawn_on_the_header() {
    let mut a = app();
    search(&mut a, "api");
    let buf = draw(&a, 100, 12);
    let head = line_at(&buf, 0);
    assert!(
        head.contains("/ api") && head.contains("3 of 4 match"),
        "{head}"
    );
    assert!(line_at(&buf, 3).contains("42") || all_text(&buf).contains("47"));
    typed(&mut a, "zz");
    let text = all_text(&draw(&a, 100, 12));
    assert!(text.contains("0 of 4 match"), "{text}");
    assert!(text.contains("Nothing matches"), "{text}");
}

/// Every candidate is drawn with the fields that tell tasks apart, and the key
/// bar says what Enter and `s` do.
#[test]
fn every_candidate_is_drawn_with_the_fields_that_tell_them_apart() {
    let text = all_text(&draw(&app(), 100, 12));
    for needle in [
        "42",
        "Ship the v1 JSON API freeze",
        "work.tasqx",
        "+release +api",
        "11.8",
        "55",
        "Draft README quickstart",
        "home",
        "enter open",
        "s start",
    ] {
        assert!(text.contains(needle), "{needle:?} missing from:\n{text}");
    }
}

/// The key bar is the bottom row in every mode, drawn from that mode's table.
#[test]
fn the_key_bar_is_the_bottom_row_in_every_mode() {
    let mut a = app();
    let last = |a: &App| line_at(&draw(a, 100, 24), 23);
    assert!(
        last(&a).contains("leave") && last(&a).contains("start"),
        "{}",
        last(&a)
    );
    a.on_key(press(KeyCode::Char('/')));
    assert!(
        last(&a).contains("done") && !last(&a).contains("leave"),
        "{}",
        last(&a)
    );
    a.on_key(press(KeyCode::Enter));
    a.on_key(press(KeyCode::Enter));
    a.set_detail(42, Ok(task(42, "Ship", "w", "H", 11.8, &[])));
    assert!(
        last(&a).contains("back") && last(&a).contains("start"),
        "{}",
        last(&a)
    );
}

/// Enter shows `show`'s card for the task: the same rail and rows `tasqx show`
/// prints (D122), rendered at the screen's width less its one-cell lead and
/// one-cell margin — to the cell. A card drawn one cell wider or narrower is
/// caught: the fixtures put a fact exactly on the edge where `show` stops
/// pairing two facts on a line, and the loop asserts that edge is really
/// there, so the test cannot go vacuous by the fixture drifting off it.
#[test]
fn the_card_is_shows_card() {
    let card_at = |t: &Value, cols: usize| -> Vec<String> {
        let ctx = Ctx::new(theme::load("nord", None), caps()).with_cols(cols);
        render::task_detail(&ctx, t, now())
            .lines()
            .map(|l| {
                tui::painted_line(l)
                    .spans
                    .iter()
                    .map(|s| s.content.to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    };
    let (mut narrower, mut wider) = (false, false);
    for len in 40..70 {
        let tag = "t".repeat(len);
        let t = task(42, "Ship", "work.tasqx", "H", 11.8, &[tag.as_str()]);
        let mut a = app();
        a.on_key(press(KeyCode::Enter));
        a.set_detail(42, Ok(t.clone()));
        let buf = draw(&a, 100, 24);
        let want = card_at(&t, 98);
        for (i, l) in want.iter().enumerate() {
            let got: String = line_at(&buf, i as u16).chars().skip(1).collect();
            assert_eq!(&got, l, "tag of {len}, line {i}");
        }
        narrower |= card_at(&t, 97) != want;
        wider |= card_at(&t, 99) != want;
    }
    assert!(
        narrower && wider,
        "no fixture sits on the pairing edge, so a one-cell drift would pass"
    );
    assert!(
        all_text(&draw(&app_with_card(), 100, 24)).contains("▌"),
        "no rail"
    );

    // A task that is gone by the time the key arrives says so on the card.
    let mut a = app_with_card();
    a.set_detail(42, Err("#42 is gone".into()));
    assert!(all_text(&draw(&a, 100, 24)).contains("#42 is gone"));
}

fn app_with_card() -> App {
    let mut a = app();
    a.on_key(press(KeyCode::Enter));
    a.set_detail(42, Ok(task(42, "Ship", "w", "H", 11.8, &[])));
    a
}

/// A card longer than the screen scrolls, and the bar says where the reader is.
#[test]
fn a_long_card_scrolls_and_says_where_it_is() {
    let mut a = app();
    a.on_key(press(KeyCode::Enter));
    let mut t = task(42, "Ship", "w", "H", 11.8, &[]);
    t["annotations"] = json!((0..40)
        .map(|n| json!({ "body": format!("note {n}"), "created": "2026-09-01T00:00:00Z" }))
        .collect::<Vec<_>>());
    a.set_detail(42, Ok(t));
    a.observe(100, 12);
    let top = all_text(&draw(&a, 100, 12));
    assert!(top.contains("1–10 of"), "{top}");
    a.on_key(press(KeyCode::Char('j')));
    let next = line_at(&draw(&a, 100, 12), 11);
    assert!(next.contains("2–11 of"), "{next}");
    a.on_key(press(KeyCode::Char('G')));
    a.on_key(press(KeyCode::Char('j')));
    let end = line_at(&draw(&a, 100, 12), 11);
    let total: usize = end.rsplit(' ').next().unwrap().parse().unwrap();
    assert!(
        end.contains(&format!("of {total}")) && end.contains(&format!("–{total}")),
        "{end}"
    );
}

/// The marker tracks the cursor through a search, and marks one row.
#[test]
fn the_marker_sits_on_the_highlighted_task() {
    let mut a = app();
    let before = all_text(&draw(&a, 100, 12));
    assert!(
        before
            .lines()
            .any(|l| l.starts_with(" ▸") && l.contains("Ship")),
        "{before}"
    );
    a.on_key(press(KeyCode::Down));
    search(&mut a, "api");
    let after = all_text(&draw(&a, 100, 12));
    assert!(
        after
            .lines()
            .any(|l| l.starts_with(" ▸") && l.contains("Publish")),
        "{after}"
    );
    assert_eq!(
        after.lines().filter(|l| l.starts_with(" ▸")).count(),
        1,
        "{after}"
    );
}

/// The row under the cursor is emphasised by more than a glyph: its title is
/// bold, which survives `NO_COLOR` and `mono`, where the accent colour does
/// not (rule 4's reader, D76's "only what you act on is emphasized").
#[test]
fn the_cursor_row_title_is_bold_under_no_color_and_mono() {
    let nc = Caps {
        depth: ColorDepth::None,
        ansi: true,
        unicode: true,
    };
    for (caps, theme) in [(nc, "nord"), (caps(), "mono")] {
        let buf = draw(&app_with(app_tasks(), caps, theme), 100, 12);
        let (y, x) = find(&buf, "Ship");
        assert!(
            buf[(x, y)].modifier.contains(Modifier::BOLD),
            "{theme} {caps:?}"
        );
        let (y, x) = find(&buf, "Publish");
        assert!(
            !buf[(x, y)].modifier.contains(Modifier::BOLD),
            "{theme}: every row bold"
        );
    }
}

fn app_tasks() -> Vec<Value> {
    vec![
        task(
            42,
            "Ship the v1 JSON API freeze",
            "work.tasqx",
            "H",
            11.8,
            &[],
        ),
        task(43, "Publish API docs", "work.tasqx", "M", 6.0, &[]),
    ]
}

fn find(buf: &Buffer, needle: &str) -> (u16, u16) {
    let first = needle.chars().next().unwrap().to_string();
    for y in 0..buf.area().height {
        let line = line_at(buf, y);
        if let Some(pos) = line.find(needle) {
            let cells = line[..pos].chars().count() as u16;
            assert_eq!(buf[(cells, y)].symbol(), first);
            return (y, cells);
        }
    }
    panic!("{needle} not drawn");
}

/// #205: the running task is marked apart from the cursor, on whichever row
/// it is — and (rule 4) without Unicode its mark is `*`, never the `>` the
/// cursor draws there, which was two meanings on one glyph on the terminal
/// with no colour left to tell them apart.
#[test]
fn the_running_task_is_marked_independently_of_the_cursor() {
    let mut running = task(43, "Publish API docs", "work.tasqx", "M", 6.0, &[]);
    running["status"] = json!("active");
    let tasks = vec![
        task(42, "Ship the freeze", "work.tasqx", "H", 11.8, &[]),
        running,
    ];
    let text = all_text(&draw(&app_of(tasks.clone()), 100, 12));
    let r42 = text.lines().find(|l| l.contains("Ship")).unwrap();
    let r43 = text.lines().find(|l| l.contains("Publish")).unwrap();
    assert!(r43.contains('▶'), "{r43:?}");
    assert!(!r42.contains('▶'), "{r42:?}");

    let text = all_text(&draw(&app_with(tasks, ascii(), "nord"), 100, 12));
    let r43 = text.lines().find(|l| l.contains("Publish")).unwrap();
    assert!(r43.contains('*') && !r43.contains('>'), "{r43:?}");
}

/// Without Unicode every glyph degrades: no cursor triangle, no rail glyphs,
/// no gauge, no caret — and no rule, because there is no rule at all (rule 7).
#[test]
fn a_non_unicode_terminal_gets_ascii_glyphs_and_no_rule_anywhere() {
    let mut a = app_with(app_tasks(), ascii(), "nord");
    let text = all_text(&draw(&a, 100, 12));
    for glyph in ['▸', '─', '▊', '▏', '▶', '▄', '▁', '·'] {
        assert!(
            !text.contains(glyph),
            "{glyph} leaked into ASCII mode:\n{text}"
        );
    }
    assert!(
        text.lines()
            .any(|l| l.starts_with(" > ") && l.contains("Ship")),
        "{text}"
    );
    a.on_key(press(KeyCode::Char('/')));
    let head = line_at(&draw(&a, 100, 12), 0);
    assert!(head.contains("/ _"), "no ASCII caret: {head}");
    let unicode = all_text(&draw(&app(), 100, 12));
    assert!(!unicode.contains('─'), "a rule is drawn:\n{unicode}");
}

/// #202: an over-wide title is cut with an ellipsis, never glued to PROJECT.
#[test]
fn an_over_wide_title_is_truncated_with_a_visible_gap_before_project() {
    let long = "VH-STD-001 ratificatie: Owner invullen, D39 STS-vraag, effective date (RC 28-7) and then some more words";
    let a = app_of(vec![task(
        9,
        long,
        "qore-architecture",
        "H",
        18.5,
        &["pr-55", "vh-std"],
    )]);
    let text = all_text(&draw(&a, 140, 12));
    let row = text.lines().find(|l| l.contains("VH-STD")).unwrap();
    assert!(row.contains('…'), "{row:?}");
    assert!(!row.contains("more wordsqore"), "{row:?}");
}

/// #202's other half: a long project runs into TAGS with no separator.
#[test]
fn an_over_wide_project_is_truncated_with_a_visible_gap_before_tags() {
    let a = app_of(vec![task(
        9,
        "short title",
        "qore-architecture",
        "H",
        18.5,
        &["pr-55"],
    )]);
    let text = all_text(&draw(&a, 200, 12));
    let row = text.lines().find(|l| l.contains("short title")).unwrap();
    assert!(!row.contains("qore-architecture+pr-55"), "{row:?}");
}

/// #202 at a narrow terminal: PROJECT must not be pushed off the frame by an
/// over-wide title (the fit shrinks the title first, rule 2).
#[test]
fn a_narrow_terminal_still_shows_project_after_an_over_wide_title() {
    let long = "VH-STD-001 ratificatie: Owner invullen, D39 STS-vraag, effective date (RC 28-7)";
    let a = app_of(vec![task(9, long, "qore-arch", "H", 18.5, &["vh-std"])]);
    let text = all_text(&draw(&a, 80, 12));
    let row = text.lines().find(|l| l.contains("VH-STD")).unwrap();
    assert!(row.contains("qore-arch"), "{row:?}");
}

/// At 60 columns (D120) every line is FITTED rather than clipped by the frame:
/// the test backend clips whatever overruns it, so the check is that each line
/// ends on something whole — a row on a whole column, the header on a whole
/// fact, the bar on a whole hint.
#[test]
fn at_sixty_columns_every_line_is_fitted_not_clipped() {
    let mut late = task(
        63,
        "Renew the TLS certificate for api.example.dev",
        "infra",
        "H",
        18.1,
        &["ops"],
    );
    late["due"] = json!("2026-09-08T00:00:00Z");
    let mut soon = task(
        64,
        "Ship the v2 pricing page",
        "website",
        "H",
        18.2,
        &["launch"],
    );
    soon["due"] = json!("2026-09-10T17:00:00Z");
    let mut running = task(65, "Write notes", "work", "M", 3.0, &[]);
    running["status"] = json!("active");
    let mut blocked = task(66, "Waiting", "work", "L", 1.0, &[]);
    blocked["blocked"] = json!(true);
    let mut a = app_of(vec![late, soon, running, blocked]);
    a.observe(60, 20);
    let buf = draw(&a, 60, 20);

    // The row: the title gives way with an ellipsis and the columns on the
    // right drop whole, in `list`'s order (rule 2). The cursor's three cells
    // of lead are what `list` does not spend: at 60 the browser lays out what
    // `list` would at 57, which drops DUE; the header still says `1 overdue`.
    let row = line_at(&buf, 3);
    assert!(row.contains('…') && row.ends_with("infra"), "{row}");
    // A row can be clipped exactly on a column boundary and look whole; the
    // label line is the one a clip cuts mid-word (`PROJEC`), so it is the
    // check that the columns were fitted to the frame rather than past it.
    let labels = line_at(&buf, 2);
    assert!(
        ["TASK", "STATUS", "PROJECT", "DUE", "TAGS"]
            .iter()
            .any(|l| labels.ends_with(l)),
        "the column labels were clipped: {labels}"
    );
    // The header: facts drop from the right until the line fits (rule 9), so
    // it ends on a whole one.
    let head = line_at(&buf, 0);
    let last = head.rsplit(" · ").next().unwrap();
    assert!(
        ["1 overdue", "1 due today", "#65 running", "1 blocked"].contains(&last),
        "the header was clipped mid-fact: {head}"
    );
    assert!(head.contains("1 overdue"), "{head}");
    // The bar: hints are taken whole or not at all.
    let bar = line_at(&buf, 19);
    assert!(bar.ends_with("leave"), "{bar}");
}

/// The same property through the REAL render on a terminal too short for the
/// list: the marker scrolls into view, and `s` names the task it sits on.
#[test]
fn a_list_taller_than_the_terminal_scrolls_the_marker_into_view() {
    for (presses, expected) in [(0, "Ship"), (1, "Publish"), (2, "Write"), (3, "Draft")] {
        let mut b = app();
        b.observe(100, 7);
        for _ in 0..presses {
            b.on_key(press(KeyCode::Down));
        }
        let text = all_text(&draw(&b, 100, 7));
        assert!(
            text.lines()
                .any(|l| l.starts_with(" ▸") && l.contains(expected)),
            "after {presses} Down the marker on {expected} was not drawn:\n{text}"
        );
    }
    let mut a = app();
    a.observe(100, 7);
    for _ in 0..3 {
        a.on_key(press(KeyCode::Down));
    }
    let text = all_text(&draw(&a, 100, 7));
    assert!(text.contains("Draft") && !text.contains("Ship"), "{text}");
    assert_eq!(
        a.on_key(press(KeyCode::Char('s'))),
        Some(Action::Start { short_id: 55 })
    );
}

/// Painted through the theme it is handed, like every other surface.
#[test]
fn the_screen_is_painted_in_the_theme_it_is_handed() {
    let nord = draw(&app_with(app_tasks(), caps(), "nord"), 100, 12);
    let gruvbox = draw(&app_with(app_tasks(), caps(), "gruvbox"), 100, 12);
    let header = |b: &Buffer| b[(1, 0)].fg;
    assert_eq!(
        Some(header(&nord)),
        rt_style(theme::load("nord", None).role("header"), &caps()).fg
    );
    assert_ne!(
        header(&nord),
        header(&gruvbox),
        "the screen ignored the theme"
    );
}

// ---- found by the adversarial pass -------------------------------------------

/// `s` on a task `task.start` would refuse (done, cancelled, backlog) must not
/// leave the screen to fail: from the dashboard that error ended the whole
/// session. It says why on the key bar's row instead and stays.
#[test]
fn s_on_a_task_that_cannot_start_stays_and_says_why() {
    let mut done = task(70, "Shipped already", "work", "M", 1.0, &[]);
    done["status"] = json!("done");
    let mut a = app_of(vec![done, task(71, "Open", "work", "L", 1.0, &[])]);
    assert_eq!(a.on_key(press(KeyCode::Char('s'))), None, "s left to fail");
    let bar = line_at(&draw(&a, 100, 24), 23);
    assert!(bar.contains("#70") && bar.contains("done"), "{bar}");
    a.on_key(press(KeyCode::Char('j')));
    assert!(
        line_at(&draw(&a, 100, 24), 23).contains("start"),
        "the bar came back"
    );
    assert_eq!(
        a.on_key(press(KeyCode::Char('s'))),
        Some(Action::Start { short_id: 71 })
    );
}

/// The count on a search header is a number, and a number never gives way
/// (rule 2): at 40 columns the filter goes first and then the query, but
/// `1 of 4 match` stays whole.
#[test]
fn the_search_header_keeps_its_count_whole_on_a_narrow_terminal() {
    let mut a = app();
    search(&mut a, "draft readme quick");
    let head = line_at(&draw(&a, 40, 12), 0);
    assert!(head.contains("1 of 4 match"), "{head}");
    // And the sentence under an empty result is cut with an ellipsis, not
    // stopped mid-word by the frame.
    typed(&mut a, "zzz");
    let msg = line_at(&draw(&a, 40, 12), 2);
    assert!(msg.ends_with('…'), "{msg}");
}

/// The card's position is a number too: at 44 columns it must not be cut
/// to `1–10 of 1` by the hints beside it.
#[test]
fn the_card_position_stays_whole_on_a_narrow_terminal() {
    let mut a = app();
    a.on_key(press(KeyCode::Enter));
    let mut t = task(42, "Ship", "w", "H", 11.8, &[]);
    t["annotations"] = json!((0..8)
        .map(|n| json!({ "body": format!("note {n}"), "created": "2026-09-01T00:00:00Z" }))
        .collect::<Vec<_>>());
    a.set_detail(42, Ok(t));
    a.observe(44, 12);
    let bar = line_at(&draw(&a, 44, 12), 11);
    let total = a.card().len();
    assert!(bar.ends_with(&format!("1–10 of {total}")), "{bar}");
}

/// In the search, Esc keeps the filter, so the empty result may not tell the
/// reader Esc clears it.
#[test]
fn the_empty_result_names_the_key_that_does_it_in_each_mode() {
    let mut a = app();
    search(&mut a, "zzqx");
    let text = all_text(&draw(&a, 100, 12));
    assert!(!text.contains("Esc clears"), "{text}");
    assert!(text.contains("Backspace"), "{text}");
    a.on_key(press(KeyCode::Enter));
    assert!(all_text(&draw(&a, 100, 12)).contains("Esc clears"));
}

/// A card is read each time it is opened, so one that failed is tried again
/// and one left open does not go stale behind a list the reader moved on in.
#[test]
fn the_card_is_read_again_each_time_it_opens() {
    let mut a = app();
    a.on_key(press(KeyCode::Enter));
    a.set_detail(42, Err("#42 could not be read".into()));
    a.on_key(press(KeyCode::Esc));
    a.on_key(press(KeyCode::Enter));
    assert_eq!(a.wanted(), Some(42), "a failed read is never retried");
}

/// The search bar has no glyph an ASCII terminal cannot draw.
#[test]
fn the_search_bar_degrades_to_ascii() {
    let mut a = app_with(app_tasks(), ascii(), "nord");
    a.on_key(press(KeyCode::Char('/')));
    let bar = line_at(&draw(&a, 100, 12), 11);
    assert!(bar.is_ascii(), "{bar}");
}

// ---- found by review round 1 ------------------------------------------------

/// Every mode's bar names the way out at any width the screen draws, down to
/// 24 columns: at 40 the list's bar used to drop `q` and `esc` whole.
#[test]
fn every_bar_names_the_way_out_at_any_width() {
    for w in 24..=100u16 {
        let mut a = app();
        a.observe(w, 12);
        assert!(line_at(&draw(&a, w, 12), 11).contains("q "), "list at {w}");
        a.on_key(press(KeyCode::Char('/')));
        assert!(
            line_at(&draw(&a, w, 12), 11).contains("enter"),
            "search at {w}"
        );
        a.on_key(press(KeyCode::Enter));
        a.on_key(press(KeyCode::Enter));
        a.set_detail(42, Ok(task(42, "Ship", "w", "H", 11.8, &[])));
        assert!(line_at(&draw(&a, w, 12), 11).contains("esc"), "card at {w}");
    }
}

/// Below `list`'s floors a row overflows; the frame must not cut it mid-word.
/// It is cut to the width with an ellipsis, as every other overlong line is.
#[test]
fn a_row_past_its_floors_is_cut_with_an_ellipsis() {
    let a = app();
    let buf = draw(&a, 40, 12);
    for y in 2..7 {
        let l = line_at(&buf, y);
        assert!(
            l.chars().count() < 40 || l.ends_with('…'),
            "line {y} was clipped by the frame: {l:?}"
        );
    }
}

/// The refusal sentence degrades like every other glyph on the screen.
#[test]
fn the_s_refusal_is_ascii_without_unicode() {
    let mut done = task(70, "Shipped already", "work", "M", 1.0, &[]);
    done["status"] = json!("done");
    let mut a = app_with(vec![done], ascii(), "nord");
    a.on_key(press(KeyCode::Char('s')));
    let bar = line_at(&draw(&a, 100, 12), 11);
    assert!(bar.contains("#70") && bar.is_ascii(), "{bar}");
}

/// With nothing listed, the bar names only keys that still do something
/// (D62): no enter, no `s`, no moving.
#[test]
fn an_empty_list_bar_names_only_live_keys() {
    let mut a = app();
    search(&mut a, "zzqx");
    a.on_key(press(KeyCode::Enter));
    let bar = line_at(&draw(&a, 100, 12), 11);
    for dead in ["enter", "s start", "j/k", "g/G"] {
        assert!(
            !bar.contains(dead),
            "{dead:?} offered with nothing listed: {bar}"
        );
    }
    assert!(bar.contains("esc") && bar.contains("q "), "{bar}");
    // The search line's own bar, with nothing matching, drops its move keys.
    a.on_key(press(KeyCode::Char('/')));
    let bar = line_at(&draw(&a, 100, 12), 11);
    assert!(!bar.contains("move"), "{bar}");
}

/// `q` leaves `pick`: to the shell, or back to the dashboard it was opened
/// from. Its word on the bar must be true in both.
#[test]
fn q_is_called_what_it_does_from_either_door() {
    let q = LIST_KEYS.iter().find(|k| k.keys == "q").unwrap();
    assert_ne!(q.footer.as_ref().unwrap().word, "quit");
}
