//! Tests for the dashboard screen: the state machine, and the three assertions
//! that bind `render` to `layout`.
//!
//! The binding tests exist because of a failure `pick.rs` records against
//! itself — a pure geometry function can be right while `render` quietly
//! ignores it, and that is what shipped there once. A `contains` over the whole
//! buffer does not catch it: it is satisfied by a panel drawn one row off, in
//! the wrong column, or by a task whose title happens to read "BLOCKED on
//! legal". Three assertions are needed, and none of them alone is enough.

use super::*;
use crate::theme::{self, ColorDepth};
use jiff::civil::date;
use model::Sources;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use ratatui::Terminal;
use serde_json::json;

// ============================================================================
// Fixtures
// ============================================================================

fn caps() -> Caps {
    Caps {
        depth: ColorDepth::Truecolor,
        ansi: true,
        unicode: true,
    }
}

fn ascii_caps() -> Caps {
    Caps {
        depth: ColorDepth::Ansi16,
        ansi: true,
        unicode: false,
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

/// A store with enough in it that every panel has something to draw — an empty
/// panel would satisfy a containment test trivially.
fn dash() -> Dashboard {
    let task = |id: i64, title: &str| {
        json!({
            "_rev": 1, "active_since": null, "blocked": false, "completed": null,
            "created": "2026-08-01T09:00:00Z", "due": null, "estimate": null,
            "id": format!("019fd213-0000-7000-8000-{id:012}"),
            "modified": "2026-08-01T09:00:00Z", "priority": "M", "project": "work",
            "recurrence": null, "remind": null, "scheduled": null, "short_id": id,
            "status": "pending", "tags": [], "title": title, "tracked": "PT0S",
            "urgency": id as f64, "wait": null
        })
    };
    let mut running = task(1, "the running one");
    running["status"] = json!("active");
    running["active_since"] = json!("2026-08-05T11:00:00Z");
    let mut blocked = task(2, "the blocked one");
    blocked["blocked"] = json!(true);
    let mut overdue = task(3, "the overdue one");
    overdue["due"] = json!("2026-07-01T00:00:00Z");

    model::build(
        Sources {
            tasks: &json!({ "count": 5, "tasks": [
                running, blocked, overdue, task(4, "a plain one"), task(5, "another")
            ]}),
            summary: &json!({ "groups": [{
                "count": 5, "project": "work", "est_total": "PT4H", "tracked_total": "PT1H",
                "tokens_cache_read": 900, "tokens_cache_creation": 80,
                "tokens_in": 7, "tokens_out": 5
            }]}),
            projects: &json!({ "count": 1, "projects": [{
                "archived": false, "default": true, "description": null,
                "id": "019fd213-0001-7000-8000-000000000001", "name": "work"
            }]}),
            events: &json!({ "count": 0, "events": [] }),
            event_limit: 100,
            days: 7,
        },
        "2026-08-05T12:00:00Z".parse().unwrap(),
        date(2026, 8, 5),
    )
}

fn all_panels() -> Vec<PanelId> {
    vec![
        PanelId::Now,
        PanelId::Next,
        PanelId::Due,
        PanelId::Blocked,
        PanelId::Recent,
        PanelId::Projects,
        PanelId::Burndown,
        PanelId::Tokens,
    ]
}

fn app() -> App {
    App::new(dash(), all_panels(), 7, true)
}

fn draw_at(app: &App, w: u16, h: u16, caps: &Caps) -> Buffer {
    let th = theme::load("nord", None);
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| render(app, &th, caps, f)).unwrap();
    term.backend().buffer().clone()
}

fn all_text(buf: &Buffer) -> String {
    (0..buf.area().height)
        .map(|y| {
            (0..buf.area().width)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The text inside one rect, rows joined.
///
/// `pick`'s `line_at` reads a FULL terminal row, which on a three-column
/// dashboard crosses NEXT UP, DUE and BLOCKED at once — every existing
/// assertion idiom in this repo is single-column and does not transfer.
fn cell_text(buf: &Buffer, x: u16, y: u16, w: u16, h: u16) -> String {
    (y..y + h)
        .map(|row| {
            (x..x + w)
                .map(|col| buf[(col, row)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every rung, plus the floor.
const SIZES: [(u16, u16); 6] = [(160, 44), (120, 32), (96, 28), (80, 24), (60, 16), (56, 14)];

// ============================================================================
// Binding render to layout
// ============================================================================

/// (1) Each panel's rule is at the exact position the layout gave it.
///
/// Equality of a slice at a computed position, not a `contains` over the screen:
/// a substring match is satisfied by a rule drawn at the wrong row or in the
/// wrong column, which is precisely the drift being guarded.
#[test]
fn every_panel_draws_its_rule_where_the_layout_put_it() {
    for (w, h) in SIZES {
        let a = app();
        let buf = draw_at(&a, w, h, &caps());
        let screen = model::layout(w, h, a.order()).unwrap();
        let cols = chrome_cols(w, screen.columns);
        for p in &screen.panels {
            let id = if p.id == PanelId::Slot {
                a.slot()
            } else {
                p.id
            };
            let label = rule_label(id, id == a.focus(), true);
            let x0 = left_chrome(&cols, p.x) + 1;
            let got = cell_text(&buf, x0, p.y, render::width(&label) as u16, 1);
            assert_eq!(
                got,
                label,
                "{w}x{h}: {:?}'s rule is not at ({x0}, {}):\n{}",
                p.id,
                p.y,
                all_text(&buf)
            );
        }
    }
}

/// (2) Nothing is drawn that the layout did not place — the half (1) is blind
/// to, because a per-placement check never looks at a panel that has no
/// placement.
#[test]
fn no_panel_is_drawn_that_the_layout_did_not_place() {
    for (w, h) in SIZES {
        let a = app();
        let buf = draw_at(&a, w, h, &caps());
        let screen = model::layout(w, h, a.order()).unwrap();
        for id in all_panels() {
            // Positional, not a free-text census: a hostile task title spelling
            // `──8─ TOKENS ` in a body row would forge a header that a
            // whole-buffer search counts.
            // Both spellings: the focused panel's rule carries the marker, so a
            // needle built only from the unfocused form would miss it and
            // report the focused panel as never drawn.
            let plain = rule_label(id, false, true);
            let marked = rule_label(id, true, true);
            let drawn = screen
                .panels
                .iter()
                .filter(|p| {
                    let row = cell_text(&buf, p.x, p.y, p.w, 1);
                    row.contains(plain.trim_end_matches(['─', ' ']))
                        || row.contains(marked.trim_end_matches(['─', ' ']))
                })
                .count();
            let placed = usize::from(
                screen.placement(id).is_some() || (id == a.slot() && screen.has_slot()),
            );
            assert_eq!(
                drawn,
                placed,
                "{w}x{h}: {id:?} drawn {drawn} times, placed {placed}:\n{}",
                all_text(&buf)
            );
        }
    }
}

/// (3) A panel paints only inside its own rectangle.
///
/// One panel at a time onto a blank buffer. Differential ownership over the
/// finished screen cannot work: a ratatui Buffer records the last writer per
/// cell and not who wrote it, so an overflow is repainted by whatever is drawn
/// next and disappears.
#[test]
fn a_panel_paints_only_inside_its_own_rectangle() {
    let th = theme::load("nord", None);
    for (w, h) in SIZES {
        let a = app();
        let screen = model::layout(w, h, a.order()).unwrap();
        for p in &screen.panels {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|f| draw_panel(p, &a, &th, &caps(), f)).unwrap();
            let buf = term.backend().buffer().clone();
            let inner = interior(p);
            for y in 0..h {
                for x in 0..w {
                    if buf[(x, y)].symbol() == " " {
                        continue;
                    }
                    assert!(
                        x >= inner.x
                            && x < inner.x + inner.width
                            && y >= inner.y
                            && y < inner.y + inner.height,
                        "{w}x{h}: {:?} painted ({x},{y}) outside its interior {inner:?}",
                        p.id
                    );
                }
            }
        }
    }
}

/// (4) The body inside each rectangle is the body of the panel the layout put
/// there — not merely *a* body.
///
/// Added after a bite-check found (2) blind to half the drift it claimed to
/// cover: (2) censuses rule labels, and rules are drawn by the chrome pass, so
/// a stray `draw_panel` overwriting a neighbour's body left every other
/// assertion green. Comparing each rect against a solo render of the panel that
/// belongs there is what closes it.
#[test]
fn the_body_in_each_rect_is_the_panel_the_layout_placed_there() {
    let th = theme::load("nord", None);
    for (w, h) in SIZES {
        let a = app();
        let full = draw_at(&a, w, h, &caps());
        let screen = model::layout(w, h, a.order()).unwrap();
        for p in &screen.panels {
            let mut solo = Terminal::new(TestBackend::new(w, h)).unwrap();
            solo.draw(|f| draw_panel(p, &a, &th, &caps(), f)).unwrap();
            let solo = solo.backend().buffer().clone();
            let inner = interior(p);
            assert_eq!(
                cell_text(&full, inner.x, inner.y, inner.width, inner.height),
                cell_text(&solo, inner.x, inner.y, inner.width, inner.height),
                "{w}x{h}: the body at {:?}'s rect is not what {:?} draws:\n{}",
                p.id,
                p.id,
                all_text(&full)
            );
        }
    }
}

/// The chrome and the layout must agree about where a column ends, or a seam
/// glyph lands in the middle of a task title on every row.
#[test]
fn the_chrome_columns_agree_with_every_placement() {
    for (w, h) in SIZES {
        let a = app();
        let screen = model::layout(w, h, a.order()).unwrap();
        let cols = chrome_cols(w, screen.columns);
        for p in &screen.panels {
            assert!(
                cols.contains(&(p.x + p.w - 1)),
                "{w}x{h}: {:?} ends at {} which is not a chrome column {cols:?}",
                p.id,
                p.x + p.w - 1
            );
        }
    }
}

/// The junction glyph is a function of BOTH neighbours, which is the whole
/// reason chrome is composited at screen level rather than by each panel.
#[test]
fn a_junction_reflects_the_panels_on_both_sides_of_it() {
    let a = app();
    let buf = draw_at(&a, 160, 44, &caps());
    let screen = model::layout(160, 44, a.order()).unwrap();
    let cols = chrome_cols(160, screen.columns);
    let seam = cols[1];

    // The first rule: every column starts a panel there, and there is no seam
    // above row 1 — so it tees DOWN, not across.
    assert_eq!(
        buf[(seam, 1)].symbol(),
        "┬",
        "the first rule must tee downward at the seam:\n{}",
        all_text(&buf)
    );
    // The closing rule tees back up.
    assert_eq!(
        buf[(seam, screen.rule_y)].symbol(),
        "┴",
        "the closing rule must tee upward at the seam"
    );
    // The frame's own corners.
    assert_eq!(buf[(0, 0)].symbol(), "┌");
    assert_eq!(buf[(159, 0)].symbol(), "┐");
    assert_eq!(buf[(0, screen.rule_y)].symbol(), "└");
    assert_eq!(buf[(159, screen.rule_y)].symbol(), "┘");
}

/// A rule that stops mid-screen terminates in a tee rather than running into a
/// neighbouring column's body.
#[test]
fn a_rule_that_ends_mid_screen_terminates_in_a_tee() {
    let a = app();
    let buf = draw_at(&a, 160, 44, &caps());
    let screen = model::layout(160, 44, a.order()).unwrap();
    let cols = chrome_cols(160, screen.columns);
    let seam = cols[1];

    // A row where the LEFT column starts a panel and the one to its right does
    // not — that is where a rule has to stop at the seam.
    let starts_at = |x_min: u16, x_max: u16| -> Vec<u16> {
        screen
            .panels
            .iter()
            .filter(|p| p.x >= x_min && p.x <= x_max)
            .map(|p| p.y)
            .collect()
    };
    let left = starts_at(0, seam);
    let right = starts_at(seam + 1, 159);
    let solo = left.iter().find(|y| !right.contains(y) && **y > 1);
    let y = solo.unwrap_or_else(|| {
        panic!(
            "the fixture must produce a rule that stops at the seam, else this \
             guard proves nothing:\n{}",
            all_text(&buf)
        )
    });
    assert_eq!(
        buf[(seam, *y)].symbol(),
        "┤",
        "a rule ending at the seam must tee, row {y}:\n{}",
        all_text(&buf)
    );
}

// ============================================================================
// Degradation
// ============================================================================

/// A terminal that cannot draw box glyphs gets ASCII — and the substitutes must
/// actually be there, not merely the originals absent.
#[test]
fn a_non_unicode_terminal_gets_ascii_chrome() {
    let a = app();
    let text = all_text(&draw_at(&a, 80, 24, &ascii_caps()));
    for bad in [
        '─', '│', '├', '┤', '┬', '┴', '┼', '┌', '┐', '└', '┘', '▶', '▸', '█', '▇',
    ] {
        assert!(
            !text.contains(bad),
            "Unicode {bad:?} leaked into ASCII mode:\n{text}"
        );
    }
    assert!(text.contains('+'), "ASCII corners must be drawn:\n{text}");
    assert!(text.contains('-'), "ASCII rules must be drawn");
    assert!(text.contains('|'), "ASCII verticals must be drawn");
}

// ============================================================================
// The state machine
// ============================================================================

#[test]
fn q_and_esc_and_ctrl_c_all_quit_but_help_closes_first() {
    let mut a = app();
    assert_eq!(a.on_key(key(KeyCode::Char('q'))), Some(Action::Quit));

    let mut a = app();
    a.on_key(key(KeyCode::Char('?')));
    assert!(a.help_open());
    assert_eq!(a.on_key(key(KeyCode::Esc)), None, "Esc closes help first");
    assert!(!a.help_open());
    assert_eq!(a.on_key(key(KeyCode::Esc)), Some(Action::Quit));

    // Ctrl-C is never "close the overlay".
    let mut a = app();
    a.on_key(key(KeyCode::Char('?')));
    let ctrl_c = KeyEvent {
        code: KeyCode::Char('c'),
        modifiers: KeyModifiers::CONTROL,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    };
    assert_eq!(a.on_key(ctrl_c), Some(Action::Quit));
}

/// Windows crossterm sends a Release for every Press. Invisible on Linux CI,
/// which is exactly why it is pinned — `pick` and `settings` have each shipped
/// this bug once.
#[test]
fn a_key_release_is_not_a_second_key_press() {
    let mut a = app();
    a.observe(&all_panels(), false);
    let release = KeyEvent {
        code: KeyCode::Char('?'),
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Release,
        state: KeyEventState::NONE,
    };
    assert_eq!(a.on_key(release), None);
    assert!(!a.help_open(), "a Release must not toggle help");
    a.on_key(key(KeyCode::Char('?')));
    assert!(a.help_open(), "the following Press still counts");
}

/// D58: on a narrow screen a digit PLACES its panel into the analytics slot
/// rather than merely focusing something that is not drawn.
#[test]
fn a_digit_places_its_panel_into_the_analytics_slot() {
    let mut a = app();
    let screen = model::layout(80, 24, a.order()).unwrap();
    let placed: Vec<PanelId> = screen.panels.iter().map(|p| p.id).collect();
    a.observe(&placed, screen.has_slot());
    assert!(screen.has_slot(), "80x24 uses the slot");

    a.on_key(key(KeyCode::Char('6')));
    assert_eq!(a.slot(), PanelId::Projects);
    let buf = draw_at(&a, 80, 24, &caps());
    assert!(
        all_text(&buf).contains("PROJECTS"),
        "6 must put PROJECTS in the slot:\n{}",
        all_text(&buf)
    );

    a.on_key(key(KeyCode::Char('7')));
    assert_eq!(a.slot(), PanelId::Burndown);
    let buf = draw_at(&a, 80, 24, &caps());
    let text = all_text(&buf);
    assert!(text.contains("BURNDOWN"), "7 must swap the slot:\n{text}");
    assert!(
        !text.contains("PROJECTS"),
        "and the previous occupant must be gone:\n{text}"
    );
}

/// A panel that cannot be reached at this size says so rather than moving focus
/// somewhere invisible.
#[test]
fn a_digit_for_an_unreachable_panel_reports_instead_of_moving_focus() {
    let mut a = app();
    let screen = model::layout(56, 14, a.order()).unwrap();
    let placed: Vec<PanelId> = screen.panels.iter().map(|p| p.id).collect();
    a.observe(&placed, screen.has_slot());
    assert!(!screen.has_slot(), "the XS rung has no slot");

    let before = a.focus();
    assert_eq!(a.on_key(key(KeyCode::Char('7'))), None);
    assert_eq!(a.focus(), before, "focus must not move to a hidden panel");
}

/// Tab only stops where something is drawn.
#[test]
fn tab_visits_only_reachable_panels_and_wraps() {
    let mut a = app();
    a.observe(&[PanelId::Now, PanelId::Next], false);
    a.on_key(key(KeyCode::Tab));
    assert_eq!(a.focus(), PanelId::Next);
    a.on_key(key(KeyCode::Tab));
    assert_eq!(a.focus(), PanelId::Now, "Tab wraps");
    a.on_key(key(KeyCode::BackTab));
    assert_eq!(a.focus(), PanelId::Next, "BackTab walks the other way");
}

/// `k` at the top must not underflow. A panic here happens inside a raw-mode
/// alternate screen, where the message is wiped before it can be read.
#[test]
fn scrolling_clamps_at_both_ends_without_underflowing() {
    let mut a = app();
    a.observe(&all_panels(), false);
    a.on_key(key(KeyCode::Char('2')));
    assert_eq!(a.scroll_of(PanelId::Next), 0);
    a.on_key(key(KeyCode::Char('k')));
    assert_eq!(a.scroll_of(PanelId::Next), 0, "k at the top is a no-op");

    for _ in 0..50 {
        a.on_key(key(KeyCode::Char('j')));
    }
    let rows = a.dash().next.rows.len();
    assert_eq!(
        a.scroll_of(PanelId::Next),
        rows.saturating_sub(1),
        "j clamps to the last row"
    );
    a.on_key(key(KeyCode::Char('g')));
    assert_eq!(a.scroll_of(PanelId::Next), 0);
    a.on_key(key(KeyCode::Char('G')));
    assert_eq!(a.scroll_of(PanelId::Next), rows.saturating_sub(1));
}

/// `w` must re-read, because the window changes which events are fetched
/// (D59's `from`). A `w` that only relabelled the axis would lie.
#[test]
fn cycling_the_window_asks_for_a_refresh() {
    let mut a = app();
    assert_eq!(a.window_days(), 7);
    assert_eq!(a.on_key(key(KeyCode::Char('w'))), Some(Action::Refresh));
    assert_eq!(a.window_days(), 14);
    a.on_key(key(KeyCode::Char('w')));
    assert_eq!(a.window_days(), 30);
    a.on_key(key(KeyCode::Char('w')));
    assert_eq!(a.window_days(), 7, "the window cycles");
}

#[test]
fn the_intents_the_loop_has_to_act_on_are_returned_not_swallowed() {
    let mut a = app();
    assert_eq!(a.on_key(key(KeyCode::Char('p'))), Some(Action::Pick));
    assert_eq!(a.on_key(key(KeyCode::Char('l'))), Some(Action::List));
    assert_eq!(a.on_key(key(KeyCode::Char('r'))), Some(Action::Refresh));
    // R toggles locally and must NOT ask the loop to do anything.
    assert_eq!(a.on_key(key(KeyCode::Char('R'))), None);
    assert!(!a.auto_refresh());
}

/// Scroll and focus survive a refresh; a screen that jumped to the top on every
/// interval would be unreadable with auto-refresh on.
#[test]
fn a_refresh_keeps_focus_and_scroll() {
    let mut a = app();
    a.observe(&all_panels(), false);
    a.on_key(key(KeyCode::Char('2')));
    a.on_key(key(KeyCode::Char('j')));
    let (f, s) = (a.focus(), a.scroll_of(PanelId::Next));
    a.replace(dash());
    assert_eq!(a.focus(), f);
    assert_eq!(a.scroll_of(PanelId::Next), s);
}

/// Every binding the screen answers to appears in the help, and every help line
/// names a binding that works. The docs-drift idiom, inside the TUI.
#[test]
fn every_binding_is_documented_in_the_help_overlay() {
    let documented: String = KEYS.iter().map(|(k, _)| *k).collect::<Vec<_>>().join(" ");
    for probe in [
        "1-8", "tab", "j / k", "g / G", "r", "R", "w", "p", "l", "?", "q", "ctrl-c",
    ] {
        assert!(
            documented.contains(probe),
            "{probe:?} is answered by on_key but missing from KEYS"
        );
    }
    // And the overlay really draws them.
    let mut a = app();
    a.on_key(key(KeyCode::Char('?')));
    let text = all_text(&draw_at(&a, 96, 28, &caps()));
    for (k, _) in KEYS {
        assert!(text.contains(k), "help overlay is missing {k:?}:\n{text}");
    }
}

/// Relative dates are measured against the model's own `today`, not against a
/// default `Date`.
///
/// This caught a real bug rather than confirming a design: the DUE body built
/// its own `jiff::civil::Date::default()` — 1970 — so every deadline rendered
/// as roughly `+20670d`. Nothing else noticed, because no other assertion looked
/// at the when-cell's TEXT. A date the renderer invents is also a date that can
/// disagree with the buckets it is drawing when a redraw straddles midnight.
#[test]
fn the_due_panel_measures_against_the_models_today() {
    let a = app();
    let text = all_text(&draw_at(&a, 96, 28, &caps()));
    // The fixture's overdue task is due 2026-07-01 against a `today` of
    // 2026-08-05: 35 days ago.
    assert!(
        text.contains("-35d"),
        "the overdue task must read as 35 days late:\n{text}"
    );
    assert!(
        !text.contains("-20000d") && !text.contains("+20000d"),
        "a default (1970) date leaked into the when-cell:\n{text}"
    );
}

/// The `w` cycle and the `dashboard.window` vocabulary are one list written
/// twice. `WINDOW_CHOICES`' own comment has promised this assertion since the
/// screen landed; until now it was a promise with nothing behind it.
#[test]
fn the_window_vocabulary_matches_the_registry() {
    let s = crate::config::find("dashboard.window").expect("registered");
    let crate::config::Choices::OneOf(vocab) = s.choices else {
        panic!(
            "dashboard.window must carry a closed vocabulary, got {:?}",
            s.choices
        );
    };
    let cycled: Vec<&str> = WINDOW_CHOICES.iter().map(|(name, _)| *name).collect();
    assert_eq!(
        cycled, vocab,
        "`w` cycles a different set than `config set dashboard.window` accepts"
    );
    assert!(
        vocab.contains(&s.default),
        "the default is not in its own vocabulary"
    );
}

/// `PANEL_NAMES` is hand-written because a `&[&str]` cannot be sliced out of an
/// enum at const time. This is what keeps it honest: every name maps to a panel,
/// every panel but `Slot` has a name, and the two lists are the same length.
#[test]
fn the_panel_vocabulary_round_trips() {
    for name in model::PANEL_NAMES {
        let id = PanelId::from_slug(name)
            .unwrap_or_else(|| panic!("PANEL_NAMES has {name:?}, which names no panel"));
        assert_eq!(id.slug(), Some(*name), "{name:?} does not round-trip");
    }
    for id in all_panels() {
        let slug = id
            .slug()
            .unwrap_or_else(|| panic!("{id:?} is configurable but has no name"));
        assert!(
            model::PANEL_NAMES.contains(&slug),
            "{id:?} is a panel the config vocabulary cannot name"
        );
    }
    assert_eq!(model::PANEL_NAMES.len(), all_panels().len());
    assert_eq!(PanelId::Slot.slug(), None, "the slot is not configurable");
    assert!(PanelId::from_slug("slot").is_none());
}

/// Scrolling stops when the last row is on screen, not at `len - 1`.
///
/// Found by driving the binary, not by a test: with two tasks in a panel with
/// room for four, one `j` pushed the top row off and left a blank line, and `G`
/// parked the final row alone at the top of an empty rect. The bound belongs to
/// the VIEWPORT, so it is clamped where the rows are drawn — `App` is never
/// told the terminal's height.
#[test]
fn a_panel_whose_rows_all_fit_does_not_scroll() {
    let a = app();
    let d = a.dash();
    let rows = d.next.rows.len();
    assert!(rows >= 2, "the fixture must have rows to scroll");

    // A viewport with room to spare: every row is drawn from index 0 whatever
    // the scroll offset says.
    let tall = panels::body(
        PanelId::Next,
        model::Detail::Full,
        d,
        60,
        rows as u16 + 3,
        99,
        &theme::load("nord", None),
        &caps(),
    );
    let text = tall
        .iter()
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    for t in &d.next.rows {
        assert!(
            text.contains(&format!("#{}", t.short_id)),
            "#{} scrolled out of a panel it fits in:\n{text}",
            t.short_id
        );
    }
}

/// A panel that IS scrolling still shows a full screenful at the bottom.
#[test]
fn scrolling_to_the_end_leaves_no_blank_rows() {
    let a = app();
    let d = a.dash();
    let rows = d.recent.rows.len();
    assert!(rows >= 4, "the fixture must overflow a small viewport");

    let visible = 3u16;
    let body = panels::body(
        PanelId::Recent,
        model::Detail::Full,
        d,
        60,
        visible,
        999,
        &theme::load("nord", None),
        &caps(),
    );
    let drawn = body
        .iter()
        .filter(|l| !l.to_string().trim().is_empty())
        .count();
    assert_eq!(
        drawn, visible as usize,
        "scrolled to the end, the panel must still fill its {visible} rows, drew {drawn}"
    );
}

/// The help overlay is a bordered box wide enough for its own longest line.
///
/// It used to be a fixed 46 cells, which cut `focus a panel (or place it in the
/// analytics slot)` mid-word at EVERY terminal size — no window revealed the
/// tail — and it had no border, so the cleared rectangle read as the dashboard
/// having come apart.
#[test]
fn the_help_overlay_is_bordered_and_shows_every_binding_whole() {
    let mut a = app();
    a.on_key(key(KeyCode::Char('?')));
    let text = all_text(&draw_at(&a, 100, 30, &caps()));

    // A corner somewhere INSIDE the frame, not the frame's own. `contains('┌')`
    // proves nothing here — the dashboard draws one at (0,0) either way.
    let buf = draw_at(&a, 100, 30, &caps());
    let inner_corner = (1..29).any(|y| (1..99).any(|x| buf[(x, y)].symbol() == "┌"));
    assert!(
        inner_corner,
        "the overlay must have a border of its own, not just a cleared hole:\n{text}"
    );
    for (k, what) in KEYS {
        assert!(text.contains(k), "help is missing the key {k:?}");
        assert!(
            text.contains(what),
            "help truncates {k:?}'s description — {what:?} is not shown whole:\n{text}"
        );
    }
}

/// A one-row panel shows a task, never just the count of the tasks it is not
/// showing.
///
/// Found by driving the binary: at 80x24 BLOCKED and RECENT each got one body
/// row, spent it on `…3 more` / `…31 more`, and showed no task at ANY scroll
/// position — a panel that had become a counter for its own emptiness.
#[test]
fn a_single_row_panel_spends_its_row_on_data() {
    let a = app();
    let d = a.dash();
    assert!(d.recent.rows.len() > 1, "the fixture must overflow one row");

    for scroll in [0, 1, 99] {
        let body = panels::body(
            PanelId::Recent,
            model::Detail::OneLine,
            d,
            60,
            1,
            scroll,
            &theme::load("nord", None),
            &caps(),
        );
        let text = body
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("");
        assert!(
            text.contains('#'),
            "scroll {scroll}: one row must carry a task, got {text:?}"
        );
        assert!(
            !text.trim_start().starts_with('…'),
            "scroll {scroll}: the only row must not be the overflow marker: {text:?}"
        );
    }
}

/// An empty panel prints a sentence. A blank body reads as a hung screen.
#[test]
fn an_empty_panel_says_so_rather_than_drawing_nothing() {
    let empty = model::build(
        Sources {
            tasks: &json!({ "count": 0, "tasks": [] }),
            summary: &json!({ "groups": [] }),
            projects: &json!({ "count": 0, "projects": [] }),
            events: &json!({ "count": 0, "events": [] }),
            event_limit: 100,
            days: 7,
        },
        "2026-08-05T12:00:00Z".parse().unwrap(),
        date(2026, 8, 5),
    );
    let a = App::new(empty, all_panels(), 7, true);
    let text = all_text(&draw_at(&a, 96, 28, &caps()));
    assert!(
        text.contains("no timer running"),
        "NOW must say it is empty:\n{text}"
    );
    assert!(
        text.contains("nothing actionable"),
        "NEXT UP must say it is empty:\n{text}"
    );
    assert!(
        text.contains("nothing is blocked"),
        "BLOCKED must say it is empty:\n{text}"
    );
}
