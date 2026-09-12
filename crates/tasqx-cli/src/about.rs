//! `tasqx about` — who made it, where to find it, and what build this is.
//!
//! The house style, on a screen whose every value is a thing to copy:
//! no rules drawn (rule 7); `header` for the title and `table.label` for the
//! labels (rule 12); and the author, the links and the paths at the terminal's
//! own foreground, because they are what the row is read for (rule 1).
//!
//! **A value here is copied text.** A URL, a store path, a build id pasted into
//! an issue: none is ever wrapped, cut or decorated. A terminal too narrow for
//! one overflows instead — the manual's `verbatim` rule (D123), on the screen
//! where every row is verbatim.

use crate::columns::{self, Column, GAP};
use crate::render::{pad, width};
use crate::theme::Ctx;

const AUTHOR: &str = "Dimitri Tholen";
const LINKEDIN: &str = "https://www.linkedin.com/in/dimitri-tholen-436825231/";
const GITHUB: &str = "https://github.com/dimitritholen/tasqx";
/// Where a row starts, as on every other page of this terminal.
const INDENT: usize = 2;

/// The facts the screen states, resolved once.
pub(crate) struct Facts {
    /// What `--version` prints: crate version plus the commit it was built
    /// from. The same constant, never a second source — a credits screen that
    /// disagreed with `--version` about the build would be worse than silent.
    pub version: &'static str,
    /// The store this build WOULD open, resolved read-only: `db_path` creates
    /// the platform data directory on its way, and a screen that only says
    /// where things are must not author one.
    pub store: String,
}

impl Facts {
    pub(crate) fn gather() -> Facts {
        Facts {
            version: crate::VERSION,
            store: crate::backend::db_path_read_only()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|e| e),
        }
    }
}

pub(crate) fn render(ctx: &Ctx, f: &Facts) -> String {
    let rows: [(&str, &str); 5] = [
        ("made by", AUTHOR),
        ("linkedin", LINKEDIN),
        ("github", GITHUB),
        ("build", f.version),
        ("store", &f.store),
    ];
    // `columns::fit` sizes the label column and the gap, as it does for every
    // other table here. Both columns are FIXED, for the same reason a number
    // is: a cut label is not a label, and a cut URL is a different URL. When
    // the row does not fit, it overflows rather than lose either.
    let w = columns::fit(
        &[
            Column::fixed(rows.iter().map(|(l, _)| width(l)).max().unwrap_or(0)),
            Column::fixed(rows.iter().map(|(_, v)| width(v)).max().unwrap_or(0)),
        ],
        ctx.cols.saturating_sub(INDENT),
    );
    let mut s = ctx.paint("header", "tasqx");
    s.push_str("\n\n");
    for (label, value) in rows {
        s.push_str(&format!(
            "{}{}{}{value}\n",
            " ".repeat(INDENT),
            ctx.paint("table.label", &pad(label, w[0])),
            " ".repeat(GAP),
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{builtin, default_theme, Caps, ColorDepth};

    fn facts() -> Facts {
        Facts {
            version: crate::VERSION,
            store: "/home/x/.local/share/tasqx/tasqx.db".to_string(),
        }
    }

    fn at(cols: usize) -> Ctx {
        let caps = Caps {
            depth: ColorDepth::None,
            ansi: false,
            unicode: true,
        };
        Ctx::new(default_theme(), caps).with_cols(cols)
    }

    fn strip(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for c in chars.by_ref() {
                    if c == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    /// A URL is copied text: whole on one line, undecorated, at every width a
    /// terminal can have. It may run PAST the terminal — that is the only
    /// honest answer, since a cut URL is a different URL and a wrapped one
    /// pastes as two — and nothing else on the screen may.
    #[test]
    fn a_url_prints_whole_and_only_a_url_may_overflow() {
        let f = facts();
        for cols in Ctx::MIN_COLS..=Ctx::MAX_COLS {
            let screen = render(&at(cols), &f);
            for url in [LINKEDIN, GITHUB] {
                assert!(
                    screen.lines().any(|l| l.ends_with(url)),
                    "{url} is broken or decorated at {cols}:\n{screen}"
                );
            }
            for line in screen.lines() {
                if crate::render::width(line) <= cols {
                    continue;
                }
                let copied = [LINKEDIN, GITHUB, f.store.as_str(), f.version];
                assert!(
                    copied.iter().any(|c| line.ends_with(c)),
                    "a line that is not copied text runs past {cols}: {line:?}"
                );
            }
        }
    }

    /// A label is never cut: `made b…` is not a label. Checked at every width
    /// a terminal can have.
    ///
    /// What can break this is a `truncate` on the label, and that is the drift
    /// this guard answers. A narrower COLUMN cannot: `render::pad` only ever
    /// adds spaces, so a shrunken label column pads less and cuts nothing, and
    /// the overflow guard above cannot see it either, since a narrower column
    /// makes rows shorter. The column is `fixed` for the alignment and the
    /// gap, not as the thing that keeps a label whole.
    #[test]
    fn a_label_is_never_cut() {
        let f = facts();
        for cols in Ctx::MIN_COLS..=Ctx::MAX_COLS {
            let screen = render(&at(cols), &f);
            for label in ["made by", "linkedin", "github", "build", "store"] {
                assert!(
                    screen.lines().any(|l| l.starts_with(&format!("  {label}"))),
                    "the label {label:?} is cut or moved at {cols}:\n{screen}"
                );
            }
        }
    }

    /// The screen credits its author, by name — the literal name, spelled
    /// here as the second copy that catches an edit to the first.
    ///
    /// The first cut asserted `ends_with(AUTHOR)`, which every line satisfies
    /// the moment `AUTHOR` is emptied: a guard that could not tell a credit
    /// from a blank.
    #[test]
    fn the_author_is_named() {
        let screen = render(&at(100), &facts());
        let row = screen
            .lines()
            .find(|l| l.starts_with("  made by"))
            .expect("a made by row");
        assert!(
            row.ends_with("Dimitri Tholen"),
            "the author is not credited: {row:?}"
        );
    }

    /// The build row says what `--version` says, from the one constant both
    /// read. A second source would let a credits screen disagree with the flag
    /// about which commit this is — and on a tarball build the id is `unknown`,
    /// so this is pinned to VERSION itself, never to a sha-shaped pattern.
    ///
    /// Checked at the SOURCE, on `Facts::gather`, which is the path the binary
    /// takes. The first cut asserted only against facts the test had built
    /// itself, so it proved the renderer prints what it is handed and stayed
    /// green while `gather` grew a second source.
    #[test]
    fn the_build_row_is_what_version_prints() {
        assert_eq!(
            Facts::gather().version,
            crate::VERSION,
            "the screen reads a different build string from `--version`"
        );
        let screen = render(&at(100), &facts());
        let row = screen
            .lines()
            .find(|l| l.trim_start().starts_with("build"))
            .expect("a build row");
        assert!(
            row.ends_with(crate::VERSION),
            "the build row is not `--version`'s own string: {row:?}"
        );
    }

    /// House style rule 7: weight and whitespace separate, nothing is drawn.
    #[test]
    fn the_screen_draws_no_rule() {
        for cols in [40, 80, 140] {
            for line in render(&at(cols), &facts()).lines() {
                let t = line.trim();
                assert!(
                    !(t.chars().count() >= 3 && t.chars().all(|c| c == '─' || c == '-')),
                    "a rule is drawn at {cols}: {line:?}"
                );
            }
        }
    }

    /// Rule 12: the title takes `header`, a label takes `table.label`, and the
    /// two differ in the BYTES drawn in every built-in theme and under
    /// `NO_COLOR` — `mono` paints `accent` and `header` alike, so naming a role
    /// is not enough on its own.
    ///
    /// Compared against the label's OWN paint. The first cut of this test
    /// compared the label row's leading escapes with the title's, which cannot
    /// fail while the row starts with a two-cell indent: it stayed green with
    /// the labels painted `header`.
    #[test]
    fn the_title_and_the_labels_are_told_apart_in_every_theme() {
        let truecolor = Caps {
            depth: ColorDepth::Truecolor,
            ansi: true,
            unicode: true,
        };
        let no_color = Caps {
            depth: ColorDepth::None,
            ansi: true,
            unicode: true,
        };
        let mut ctxs: Vec<(String, Ctx)> = crate::theme::BUILTINS
            .iter()
            .map(|n| (n.to_string(), Ctx::new(builtin(n).unwrap(), truecolor)))
            .collect();
        ctxs.push(("NO_COLOR".to_string(), Ctx::new(default_theme(), no_color)));
        // The escapes a role opens with, taken with a sentinel the text never
        // holds. Empty where a role paints nothing at this capability.
        let opener = |ctx: &Ctx, role: &str| -> String {
            let painted = ctx.paint(role, "\u{0}");
            painted[..painted.find('\u{0}').expect("the sentinel survives")].to_string()
        };
        for (theme, ctx) in &ctxs {
            let screen = render(ctx, &facts());
            let lines: Vec<&str> = screen.lines().collect();
            let title = opener(ctx, "header");
            let label = opener(ctx, "table.label");
            assert_ne!(
                title, label,
                "{theme}: the title and a label are drawn with the same bytes"
            );
            assert_eq!(
                lines[0],
                ctx.paint("header", "tasqx"),
                "{theme}: the title is not painted as one"
            );
            let row = lines
                .iter()
                .find(|l| strip(l).trim_start().starts_with("made by"))
                .expect("the made by row");
            assert!(
                row.contains(&format!("{label}made by")),
                "{theme}: the label does not carry `table.label`'s own paint: {row:?}"
            );
            assert!(
                row.ends_with(AUTHOR),
                "{theme}: the value a label names carries paint of its own: {row:?}"
            );
        }
    }

    /// `about` is a declared `--json` carve-out, with its reason written down:
    /// a credits screen is not data, and a method would freeze its shape in the
    /// conformance suite (D56).
    #[test]
    fn about_is_a_declared_json_carve_out() {
        let reason = crate::JSON_CARVE_OUTS
            .iter()
            .find(|(n, _)| *n == "about")
            .map(|(_, why)| *why)
            .expect("`about` is not a declared carve-out");
        assert!(
            !reason.trim().is_empty(),
            "the carve-out must say why, not merely exist"
        );
    }
}
