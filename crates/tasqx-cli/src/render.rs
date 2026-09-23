//! Human-readable rendering of API results (DESIGN.md §5, §8).
//!
//! Every function takes a [`Ctx`] (active theme + detected terminal capability)
//! and paints via semantic *role* lookups — `header`, `project`, `overdue`,
//! `urgency.ramp` — never a literal color. The one render pipeline adapts to the
//! terminal: truecolor/256/16 color, `NO_COLOR` emphasis-only, or byte-plain
//! when piped (script-safe). Unicode rules degrade to ASCII on the same signal.

use serde_json::Value;

use crate::theme::Ctx;

mod agenda;
pub use agenda::*;
mod detail;
pub use detail::*;
mod echo;
pub use echo::*;
mod memory;
pub use memory::*;
mod reports;
pub use reports::*;
mod table;
pub use table::*;
mod why;
pub use why::*;

/// Strip terminal-control bytes from untrusted text before it is painted, so an
/// imported or agent-authored task field can't smuggle ANSI/OSC escapes that
/// clear the screen, move the cursor, set the window title, or spoof CLI output.
/// This is the terminal-path analogue of `html::esc`. Every C0 control
/// (tab included), DEL, and every C1 control are dropped; ordinary printable
/// text is untouched.
///
/// Tab is dropped here (#234 item 10 / bundle #228's duplicate — same root
/// cause), where `html::esc` deliberately keeps it (D19): a raw `\t` expands
/// to the next 8-column stop in any real terminal, shifting every column to
/// the right of it on that row — the exact misalignment D51 exists to end —
/// while an HTML `<table>` cell has no fixed-width grid for a tab to break.
/// D19's "one sanitizer standard" is about the RULE (strip control bytes,
/// keep printable text) both surfaces share, not that every exception must be
/// identical when the two surfaces' hazards differ.
///
/// For a field that is expected to hold ONE line (a title, a source, a search
/// snippet), a stray newline is itself part of what this guards against — it
/// is how a hostile field would forge a second line of fake CLI output — so it
/// is neutralised. A field that is legitimately multiple lines wants
/// [`san_multiline`] instead.
///
/// TAB and newline are replaced with a single visible space rather than
/// dropped outright: deleting them welds the text on either side into one
/// word (`"a\tb"` -> `"ab"`, `"line1\nline2"` -> `"line1line2"`), which reads
/// as a single, different value rather than the two the field actually held —
/// actively misleading, not merely cosmetic (audit #229 item 11). Every other
/// control byte (escape, bell, backspace, C1) has no legitimate content
/// reading and is still dropped outright.
pub fn san(s: &str) -> String {
    s.chars()
        .map(|c| if c == '\t' || c == '\n' { ' ' } else { c })
        .filter(|c| !c.is_control())
        .collect()
}

/// Same guard as [`san`], for text that is legitimately more than one line — a
/// stored memory doc's body, not a title or a snippet. `\n` survives alongside
/// `\t`; every other control byte (escape, bell, backspace, carriage return,
/// C1) is still dropped, so paragraph breaks and list items reach the screen
/// without opening the escape-injection door `san` closes (#195: `memory show`
/// ran a whole markdown doc through `san` and every newline in it vanished,
/// even though `memory show --json` and D71's "stored verbatim" promise both
/// carried them).
pub fn san_multiline(s: &str) -> String {
    s.chars()
        .filter(|&c| c == '\t' || c == '\n' || !c.is_control())
        .collect()
}

/// Is this task still open, given the `status` string as it arrived in a JSON
/// payload? The one place the CLI turns a wire status back into a [`Status`].
///
/// The fallback is the whole point. The CLI reads these strings from JSON that
/// may come from a *different build of core* over the daemon socket, so a status
/// this binary has never heard of is a real possibility — and the previous
/// `!matches!(status, "done" | "cancelled")` treated it as open by accident,
/// simply because it was not one of the two names it knew. Guessing "open" is
/// the safe guess: an unknown status shows up in the open counts and on the
/// overdue list, where a human will notice it, instead of vanishing from every
/// total and leaving a report that is quietly short a task. That behaviour is
/// preserved here deliberately rather than inherited, so a future editor changes
/// it on purpose.
pub fn status_is_open(status: &str) -> bool {
    tasqx_core::types::Status::parse(status).is_none_or(tasqx_core::types::Status::is_open)
}

fn s(v: &Value, key: &str) -> String {
    san(v.get(key).and_then(Value::as_str).unwrap_or(""))
}

/// Right-align `s` in `w` CELLS. The `{:>w$}` this replaces pads by char count.
fn rpad(s: &str, w: usize) -> String {
    format!("{}{}", " ".repeat(w.saturating_sub(width(s))), s)
}

/// The summary line every table opens with: what was asked (`label`), then the
/// facts, each `(role, text)`, in falling order of what a reader loses by not
/// seeing them. `list`, `agenda`, `memory list` and `memory search` all go
/// through this one function, so a summary fits, drops and paints the same way
/// on each of them (#346).
///
/// The line has to FIT. It sits above the table, where a wrap would put a
/// second line between the header and the rows it labels — the old `N task(s)`
/// trailer could overflow harmlessly, and this cannot. Facts are dropped from
/// the RIGHT until it does, which is why callers push them in falling order:
/// for `list` the count, what is late, what is due today, what is running,
/// what is stuck. Dropping says less; truncating mid-word would say something
/// else.
fn summary_line(ctx: &Ctx, label: Option<&str>, parts: Vec<(&str, String)>) -> String {
    let head = label.map_or(0, |l| {
        width(&truncate(l, ctx.cols / 2, ctx.caps.unicode)) + 3
    });
    let widths: Vec<usize> = parts.iter().map(|(_, t)| width(t)).collect();
    let ranks: Vec<u8> = (0..parts.len()).map(|i| i as u8).collect();
    let keep = keep_ranked(
        &widths,
        &ranks,
        width(ctx.mid()) + 2,
        ctx.cols.saturating_sub(head),
    );
    let parts: Vec<(&str, String)> = parts
        .into_iter()
        .zip(keep)
        .filter_map(|(p, k)| k.then_some(p))
        .collect();

    let sep = format!(" {} ", ctx.paint("muted", ctx.mid()));
    let facts = parts
        .iter()
        .map(|(role, text)| ctx.paint(role, text))
        .collect::<Vec<_>>()
        .join(&sep);
    match label {
        // What was ASKED, echoed on every run rather than only on an empty
        // result. For `list` that is the filter — it defaults to `@working`
        // (`verbs::list`), and a reader who cannot see which question was
        // asked cannot tell a short answer from a narrow one. For `agenda` it
        // is the horizon, which its own trailer already stated on every run
        // and for the same reason: "N tasks" cannot be read as "and that is
        // all there is" unless the window it is all there is WITHIN is on the
        // same line.
        Some(l) => format!(
            "{}   {facts}",
            ctx.paint("muted", &truncate(l, ctx.cols / 2, ctx.caps.unicode))
        ),
        None => facts,
    }
}

/// How many terminal CELLS this text occupies — the only unit a column can be
/// measured in.
///
/// A column is a grid position, and a grid is made of cells, not of `char`s.
/// The two disagree in every direction: a CJK ideograph is one char in two
/// cells, a combining mark is a char in none, and an emoji ZWJ sequence is five
/// chars forming one two-cell cluster. Every padded column in this module goes
/// through here (or through [`pad`]/[`fit`], which do) so there is one answer to
/// "how wide is this" rather than one per call site.
pub fn width(s: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(s)
}

/// Pad `s` out to at least `max` cells. Never truncates.
///
/// This is the treatment for a column whose content is DATA the reader came for
/// — a project name, a config value. Overflowing such a cell pushes the columns
/// to its right, which is ugly; silently cutting the value would be worse, so
/// this half of the pair only ever adds spaces.
pub fn pad(s: &str, max: usize) -> String {
    let mut out = s.to_string();
    // `push_str` on a run of spaces rather than `format!("{s:<max$}")`, because
    // that macro pads by char count — the exact bug this function exists to end.
    out.push_str(&" ".repeat(max.saturating_sub(width(s))));
    out
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use crate::theme::{self, Caps};
    use jiff::Timestamp;
    use serde_json::json;

    /// The capability level the card renders at, measurable: `unicode` turns
    /// the card on, `ansi: false` keeps SGR bytes out of the output so lines
    /// can be measured with [`width`] directly. `detect_from` never produces
    /// this exact combination — it is a measuring instrument, not a terminal.
    pub(crate) fn card_caps() -> Caps {
        Caps {
            depth: theme::ColorDepth::None,
            ansi: false,
            unicode: true,
        }
    }

    /// A task exercising every conditional row both detail layouts can draw.
    pub(crate) fn full_task() -> serde_json::Value {
        json!({
            "short_id": 42, "title": "Ship the release notes",
            "status": "pending", "priority": "H", "project": "work",
            "urgency": 11.4, "due": "2026-09-04T17:00:00Z", "remind": "-1h",
            "scheduled": "2026-09-01T09:00:00Z", "wait": "2026-08-30T00:00:00Z",
            "recurrence": "weekly on mon", "estimate": "PT4H",
            "completed": "2026-09-05T10:00:00Z", "tracked": "PT2H",
            "active_since": "2026-08-31T12:00:00Z", "blocked": true,
            "tags": ["docs", "release"], "depends_on": [7, 9],
            "tokens": [{"input_tokens": 10, "output_tokens": 20,
                        "cache_read_tokens": 0, "cache_creation_tokens": 5}],
            "annotations": [{"body": "called the plumber"}],
            "created": "2026-08-20T09:00:00Z", "modified": "2026-09-03T14:00:00Z",
            "_rev": 6
        })
    }

    /// A blocked task from the demo store, with a running timer's worth of
    /// fields around it, as `task.get` returns it.
    pub(crate) fn detail_fixture() -> serde_json::Value {
        json!({
            "short_id": 52, "title": "Write the migration guide for SDK 3.0",
            "status": "pending", "priority": "M", "project": "api",
            "urgency": 12.1, "due": "2026-09-16T00:00:00Z", "estimate": "PT5H",
            "blocked": true, "depends_on": [51],
            "unmet_blockers": [{ "short_id": 51, "title": "Rate-limit the /search endpoint" }],
            "tags": ["docs"], "created": "2026-09-01T10:00:00Z",
            "modified": "2026-09-11T09:36:55.218763722Z", "_rev": 2
        })
    }

    /// Text whose char count and terminal-cell count disagree, one entry per
    /// way they can disagree. Every table guard below runs the whole list, so a
    /// fix that measures CJK correctly but splits an emoji cluster still fails.
    ///
    /// `chars != cells` in four different directions:
    ///  * a CJK ideograph is 1 char, 2 cells;
    ///  * a combining mark is a char with 0 cells;
    ///  * an emoji ZWJ sequence is 5 chars and one 2-cell cluster;
    ///  * a skin-tone modifier is 2 chars and one 2-cell cluster.
    pub(crate) const AWKWARD: &[&str] = &[
        "plain ascii",
        "漢字テスト",
        "e\u{301}accent",                                  // e + COMBINING ACUTE
        "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f466} fam", // family ZWJ sequence
        "\u{1f44d}\u{1f3fd} ok",                           // thumbs up + skin tone modifier
        "中文",
    ];

    pub(crate) fn cells(s: &str) -> usize {
        unicode_width::UnicodeWidthStr::width(s)
    }

    /// One row of table JSON, so a layout test can vary the one field it is about.
    pub(crate) fn task_json(
        id: i64,
        title: &str,
        project: &str,
        due: &str,
        tags: &[&str],
    ) -> Value {
        json!({ "short_id": id, "urgency": 5.0, "priority": "M", "title": title,
                "project": project, "due": due, "tags": tags, "status": "pending" })
    }

    // ---- agenda ------------------------------------------------------------

    /// 2026-08-03 is a Monday. Every agenda test anchors to it, so "Today" and
    /// "Tomorrow" are facts about the fixture rather than about the day the
    /// suite happens to run.
    pub(crate) const ANCHOR: &str = "2026-08-03T09:00:00Z";

    pub(crate) fn anchor() -> Timestamp {
        ANCHOR.parse().expect("the anchor is a real instant")
    }

    /// A task carrying whichever of the two dated fields the case is about.
    /// An empty `due`/`scheduled` means the field is absent, which is what the
    /// engine emits as `null` and what `field_ts` reads the same way.
    pub(crate) fn dated(id: i64, title: &str, due: &str, scheduled: &str) -> Value {
        json!({
            "short_id": id, "urgency": 5.0, "priority": "M", "title": title,
            "project": "p", "tags": [], "status": "pending",
            "due": if due.is_empty() { Value::Null } else { json!(due) },
            "scheduled": if scheduled.is_empty() { Value::Null } else { json!(scheduled) },
        })
    }

    /// The payload half of the old `agenda_of`: the agenda borrows its rows
    /// now, so the `task.list` answer must outlive it — callers bind this
    /// first, exactly as `run_agenda` does.
    pub(crate) fn agenda_payload(tasks: Vec<Value>) -> Value {
        json!({ "tasks": tasks })
    }

    pub(crate) fn agenda_of(payload: &Value, days: usize) -> Agenda<'_> {
        agenda_select(payload, days, anchor())
    }

    pub(crate) fn agenda_out(tasks: Vec<Value>, days: usize) -> String {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let payload = agenda_payload(tasks);
        agenda_text(&ctx, &agenda_of(&payload, days))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The CLI and core are separately deployable — `tasqx` talks to a daemon it
    /// did not necessarily ship with — so a status string this binary cannot
    /// parse can genuinely arrive on the wire. The old `!matches!(status, "done"
    /// | "cancelled")` called such a status open purely as a side effect of not
    /// recognising it; now it is a stated rule, and this test is what stops
    /// someone "tidying" the fallback to `false`. Flip the `map_or` default and
    /// an unknown status disappears from open counts, the overdue list and the
    /// tag roll-up at once — three wrong numbers, no error.
    #[test]
    fn an_unknown_wire_status_is_treated_as_open() {
        for unknown in ["", "snoozed", "DONE", "canceled", "archived"] {
            assert!(
                status_is_open(unknown),
                "{unknown:?} should fall back to open"
            );
        }
        // ...while the statuses core actually defines still answer for themselves.
        for s in tasqx_core::types::Status::ALL {
            assert_eq!(status_is_open(s.as_str()), s.is_open(), "{s:?}");
        }
    }

    #[test]
    fn san_strips_control_and_escape_bytes() {
        // A title carrying a screen-clear + OSC title-set + cursor move: every
        // control byte is removed (tab replaced with a space — see
        // `san_replaces_tab_with_a_space_instead_of_deleting_it` — everything
        // else dropped outright), printable text survives.
        let malicious = "\x1b[2Jpwned\x1b]0;evil\x07\x08 ok\ttab";
        let clean = san(malicious);
        assert!(!clean.contains('\x1b'), "escape byte leaked: {clean:?}");
        assert!(!clean.contains('\x07') && !clean.contains('\x08'));
        assert!(!clean.contains('\t'), "a raw tab expands in any terminal and shifts every column to its right on that row — the misalignment D51 exists to end (D19/#234 item 10)");
        assert_eq!(
            clean, "[2Jpwned]0;evil ok tab",
            "printable kept, tab replaced with a visible space (#229 item 11)"
        );
    }

    /// #229 item 11: a newline in a single-line field (title, source,
    /// snippet) was deleted by `san` rather than replaced, so `"line1\nline2"`
    /// rendered as one welded word `"line1line2"` — actively misleading in
    /// both `list` and `show`. A newline becomes a visible space instead,
    /// same treatment as tab, while `san_multiline` (paragraph text) is left
    /// untouched — it keeps real newlines on purpose.
    #[test]
    fn san_replaces_newline_with_a_space_instead_of_deleting_it() {
        assert_eq!(
            san("line1\nline2"),
            "line1 line2",
            "a deleted newline welds two lines into one misleading word (#229 item 11)"
        );
    }

    /// `san_multiline` is `san` minus the newline drop: a stored doc's
    /// paragraph breaks and list items survive, while a bare escape byte
    /// smuggled inside the same body still does not (#195).
    #[test]
    fn san_multiline_keeps_newlines_and_still_strips_escape_bytes() {
        let doc = "# Runbook\n\nDeploys go\x1bthrough\n\n- step one\n- step two\ttabbed";
        let clean = san_multiline(doc);
        assert_eq!(
            clean, "# Runbook\n\nDeploys gothrough\n\n- step one\n- step two\ttabbed",
            "newlines and tabs kept, the lone escape byte dropped"
        );
        assert!(!clean.contains('\x1b'), "escape byte leaked: {clean:?}");
        assert_eq!(
            clean.matches('\n').count(),
            doc.matches('\n').count(),
            "every newline in the source must survive: {clean:?}"
        );
    }
}
