//! Captured terminal output as HTML: SGR in, `<pre class="term">` out.
//!
//! The documentation site shows real screens (`crates/tasqx-cli/docs-fixtures`,
//! captured by `scripts/docs-capture.sh`) as TEXT, not as pictures — DESIGN.md
//! D149(b). A picture of a terminal cannot be searched, selected, copied, read
//! by a screen reader or re-flowed on a phone, it weighs twenty times the
//! bytes, and it needs a second toolchain (`freeze` + headless Chrome) that has
//! already cost this project two defects:
//!
//! * `freeze` ignores SGR 39 ("default foreground"), so a cell that resets to
//!   the terminal's own colour kept whatever colour came before it — the
//!   dashboard's titles rendered in the ramp colour of the figure beside them,
//!   which no real terminal draws. `snap-tui.sh` still carries a `sed` that
//!   rewrites the byte away.
//! * `freeze` draws SGR 1 at normal weight (its SVG says `font-weight: normal`),
//!   so nothing is bold in its pictures — and bold is the whole emphasis of a
//!   write echo, and the ONLY emphasis left under `NO_COLOR`.
//!
//! Both are cases this renderer has a unit test for, because both are cases the
//! previous path got wrong silently: the screen still looked like a screen.
//!
//! What is supported is what tasqx's own screens and a `tmux capture-pane -e`
//! of them emit: reset, bold, dim, italic, underline and their offs, the 16
//! basic colours in both fg and bg, 256-colour and truecolor in both, and the
//! 39/49 defaults. Everything else a stream can carry — cursor addressing,
//! erase, scroll regions, OSC title sets, DCS — is DROPPED rather than
//! rendered: a captured pane is a picture of a finished screen, and the
//! sequences that moved the cursor to draw it have no meaning in a `<pre>`.
//!
//! Colours are inline `color:#rrggbb`, not classes, so a rendered block is
//! self-contained: it can be dropped into any page (`tasqx docs`, a README
//! rasterisation, an issue) without carrying a stylesheet with it. The default
//! foreground deliberately emits NO colour at all, so the page's own
//! `--term-fg` shows through and the block follows the site's light/dark
//! switch instead of freezing one theme's grey into the markup.

use crate::html::esc;

/// The 16 basic ANSI colours, in nord — the default theme (`theme::builtin`)
/// and the palette the site's `--term-bg`/`--term-fg` tokens are drawn from,
/// so a `30`–`37` cell lands in the same family as the truecolor cells beside
/// it rather than in a browser's idea of "red".
///
/// Indices 0..=7 are the normal set, 8..=15 the bright one. Nord defines only
/// one red, green, yellow, blue and magenta, so those repeat across the two
/// halves; that is the palette, not an oversight.
const PALETTE_16: [&str; 16] = [
    "#3b4252", // black
    "#bf616a", // red
    "#a3be8c", // green
    "#ebcb8b", // yellow
    "#81a1c1", // blue
    "#b48ead", // magenta
    "#88c0d0", // cyan
    "#e5e9f0", // white
    "#4c566a", // bright black
    "#bf616a", // bright red
    "#a3be8c", // bright green
    "#ebcb8b", // bright yellow
    "#81a1c1", // bright blue
    "#b48ead", // bright magenta
    "#8fbcbb", // bright cyan
    "#eceff4", // bright white
];

/// The six levels of the xterm 256-colour cube (indices 16..=231).
const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// One run's appearance. `None` for a colour means "the terminal's default",
/// which renders as no declaration at all.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Style {
    fg: Option<(u8, u8, u8)>,
    bg: Option<(u8, u8, u8)>,
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
}

impl Style {
    fn is_default(&self) -> bool {
        *self == Style::default()
    }

    /// The `style="…"` body, in a fixed order so two runs that look the same
    /// produce the same bytes — the fixtures are compared byte for byte, and a
    /// declaration order that varied would make every diff unreadable.
    fn css(&self) -> String {
        let mut css = String::new();
        if let Some((r, g, b)) = self.fg {
            css.push_str(&format!("color:#{r:02x}{g:02x}{b:02x};"));
        }
        if let Some((r, g, b)) = self.bg {
            css.push_str(&format!("background:#{r:02x}{g:02x}{b:02x};"));
        }
        if self.bold {
            css.push_str("font-weight:700;");
        }
        if self.dim {
            // Not a dimmer grey: the cell's colour is whatever the stream said,
            // and opacity is the only rendering of "faint" that keeps working
            // when the page is light, dark, or the reader's own high-contrast.
            css.push_str("opacity:.6;");
        }
        if self.italic {
            css.push_str("font-style:italic;");
        }
        if self.underline {
            css.push_str("text-decoration:underline;");
        }
        css.pop(); // the trailing `;`
        css
    }
}

/// An xterm 256-colour index as RGB: the 16 basic colours, the 6×6×6 cube,
/// then the 24-step grey ramp.
fn xterm256(n: u8) -> (u8, u8, u8) {
    match n {
        0..=15 => {
            let hex = PALETTE_16[n as usize].as_bytes();
            let byte = |i: usize| {
                let hi = (hex[i] as char).to_digit(16).unwrap_or(0) as u8;
                let lo = (hex[i + 1] as char).to_digit(16).unwrap_or(0) as u8;
                hi * 16 + lo
            };
            (byte(1), byte(3), byte(5))
        }
        16..=231 => {
            let i = n - 16;
            (
                CUBE[(i / 36) as usize],
                CUBE[(i / 6 % 6) as usize],
                CUBE[(i % 6) as usize],
            )
        }
        232..=255 => {
            let v = 8 + (n - 232) * 10;
            (v, v, v)
        }
    }
}

/// Apply one SGR sequence's parameters to `style`.
///
/// Unknown parameters are skipped rather than aborting the sequence: a stream
/// carrying `4:3` (curly underline) or `53` (overline) should still get its
/// colours, and an attribute this does not model is better absent than
/// mistaken for a colour index.
fn apply_sgr(params: &[u32], style: &mut Style) {
    let mut i = 0;
    while i < params.len() {
        match params[i] {
            0 => *style = Style::default(),
            1 => style.bold = true,
            2 => style.dim = true,
            3 => style.italic = true,
            4 => style.underline = true,
            // 22 is "normal intensity", which turns off BOTH bold and dim —
            // there is no separate off-code for either, and a renderer that
            // cleared only bold left every `\x1b[2m…\x1b[22m` run faint to the
            // end of the screen.
            22 => {
                style.bold = false;
                style.dim = false;
            }
            23 => style.italic = false,
            24 => style.underline = false,
            30..=37 => style.fg = Some(xterm256((params[i] - 30) as u8)),
            38 => {
                i += take_color(&params[i + 1..], &mut style.fg);
            }
            39 => style.fg = None,
            40..=47 => style.bg = Some(xterm256((params[i] - 40) as u8)),
            48 => {
                i += take_color(&params[i + 1..], &mut style.bg);
            }
            49 => style.bg = None,
            90..=97 => style.fg = Some(xterm256((params[i] - 90 + 8) as u8)),
            100..=107 => style.bg = Some(xterm256((params[i] - 100 + 8) as u8)),
            _ => {}
        }
        i += 1;
    }
}

/// Read the argument of a `38`/`48`: `5;n` (256) or `2;r;g;b` (truecolor).
/// Returns how many parameters were consumed, so a truncated sequence
/// (`\x1b[38;2;1m`) consumes what is there and cannot loop or panic.
fn take_color(rest: &[u32], slot: &mut Option<(u8, u8, u8)>) -> usize {
    match rest.first() {
        Some(5) => {
            if let Some(&n) = rest.get(1) {
                *slot = Some(xterm256(n.min(255) as u8));
                return 2;
            }
            1
        }
        Some(2) => {
            if let (Some(&r), Some(&g), Some(&b)) = (rest.get(1), rest.get(2), rest.get(3)) {
                *slot = Some((r.min(255) as u8, g.min(255) as u8, b.min(255) as u8));
                return 4;
            }
            rest.len()
        }
        _ => 0,
    }
}

/// Render a captured terminal stream as one `<pre class="term">` block.
///
/// The text is escaped with `crate::html::esc` — the same escaper the HTML
/// report uses, one notion of "safe" for both surfaces — which also drops any
/// control byte this parser passed through, so nothing a fixture carries can
/// reach a reader's terminal when the page is `curl`ed.
pub fn render(ansi: &str) -> String {
    let mut out = String::with_capacity(ansi.len() * 2);
    out.push_str("<pre class=\"term\">");

    let mut style = Style::default();
    // The run being accumulated, and the style it was opened with. Adjacent
    // runs that resolve to the same declarations merge into one span: tasqx's
    // renderer re-states a colour per cell in places, and a span per cell
    // quadrupled the page for no visible difference.
    let mut run = String::new();
    let mut run_style = Style::default();

    let mut chars = ansi.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            run.push(c);
            continue;
        }
        match chars.next() {
            // CSI: parameters, then a final byte in 0x40..=0x7e. Only `m` says
            // anything a `<pre>` can show; the rest moved a cursor that is not
            // there any more.
            Some('[') => {
                let mut params = String::new();
                let mut final_byte = None;
                for c in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&c) {
                        final_byte = Some(c);
                        break;
                    }
                    params.push(c);
                }
                if final_byte == Some('m') {
                    // An EMPTY field is zero: ECMA-48 gives an omitted
                    // parameter its default, and SGR's default is 0, so
                    // `\x1b[m` and `\x1b[;m` are resets.
                    //
                    // A field that is present and unreadable is SKIPPED, and
                    // the difference is the whole point. Reading it as a zero
                    // too meant the colon sub-parameter form — `4:3` (curly
                    // underline), `58:2:…` (underline colour), which a terminal
                    // that does not implement it simply ignores — RESET every
                    // attribute instead, so one unsupported underline style
                    // stripped the colour and the weight off the rest of the
                    // line. An attribute this renderer cannot model is absent
                    // from the run; it is never a reset of it.
                    let nums: Vec<u32> = if params.is_empty() {
                        vec![0]
                    } else {
                        params
                            .split(';')
                            .filter_map(|p| {
                                if p.is_empty() {
                                    Some(0)
                                } else {
                                    p.parse::<u32>().ok()
                                }
                            })
                            .collect()
                    };
                    let mut next = style;
                    apply_sgr(&nums, &mut next);
                    if next != style {
                        flush(&mut out, &mut run, run_style);
                        run_style = next;
                        style = next;
                    }
                }
            }
            // OSC and the other string sequences: everything up to BEL or ST.
            // A window title (`\x1b]0;…\x07`) is the one tmux and the TUI both
            // emit, and its payload is text that would otherwise print.
            Some(']') | Some('P') | Some('_') | Some('^') | Some('X') => {
                while let Some(c) = chars.next() {
                    if c == '\x07' {
                        break;
                    }
                    if c == '\x1b' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            // Two-byte escapes (`\x1b(B`, `\x1b)0`, `\x1b#8`) take one more
            // byte; every other one is complete already. Dropping one byte too
            // few would print a charset selector as text.
            Some('(') | Some(')') | Some('*') | Some('+') | Some('#') => {
                chars.next();
            }
            // A lone ESC at the end of the stream, or a single-byte escape.
            _ => {}
        }
    }
    flush(&mut out, &mut run, run_style);

    out.push_str("</pre>");
    out
}

/// Close the accumulated run: escape it, wrap it in a span when the style says
/// anything, and empty the buffer. An empty run emits nothing, so a stream
/// that re-states its colour between two characters does not grow the page.
fn flush(out: &mut String, run: &mut String, style: Style) {
    if run.is_empty() {
        return;
    }
    let text = esc(run);
    run.clear();
    if text.is_empty() {
        return;
    }
    if style.is_default() {
        out.push_str(&text);
    } else {
        out.push_str(&format!("<span style=\"{}\">{text}</span>", style.css()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The runs inside the block, without the `<pre>` wrapper.
    fn inner(ansi: &str) -> String {
        let html = render(ansi);
        let body = html
            .strip_prefix("<pre class=\"term\">")
            .expect("a term block opens with its pre")
            .strip_suffix("</pre>")
            .expect("a term block closes it");
        body.to_string()
    }

    #[test]
    fn plain_text_is_escaped_and_carries_no_span() {
        assert_eq!(
            inner("a <b> & 'c'"),
            "a &lt;b&gt; &amp; &#39;c&#39;",
            "text with no SGR is text, and markup in it is inert"
        );
        assert!(render("x").starts_with("<pre class=\"term\">"));
        assert!(render("x").ends_with("</pre>"));
    }

    /// The second defect the `freeze` path had: SGR 1 drawn at normal weight,
    /// so a write echo lost the emphasis that says "this is what changed" and
    /// `NO_COLOR` output lost its only emphasis.
    #[test]
    fn bold_is_emitted_as_a_weight() {
        assert_eq!(
            inner("\x1b[1madded\x1b[0m"),
            "<span style=\"font-weight:700\">added</span>"
        );
    }

    #[test]
    fn dim_italic_and_underline_each_render_and_each_turn_off() {
        assert_eq!(
            inner("\x1b[2mfaint\x1b[22mplain"),
            "<span style=\"opacity:.6\">faint</span>plain"
        );
        assert_eq!(
            inner("\x1b[3mi\x1b[23mn"),
            "<span style=\"font-style:italic\">i</span>n"
        );
        assert_eq!(
            inner("\x1b[4mu\x1b[24mn"),
            "<span style=\"text-decoration:underline\">u</span>n"
        );
        // 22 is "normal intensity": one code, both attributes.
        assert_eq!(inner("\x1b[1;2mboth\x1b[22mneither"), {
            let open = "<span style=\"font-weight:700;opacity:.6\">both</span>";
            format!("{open}neither")
        });
    }

    #[test]
    fn the_sixteen_basic_colours_come_from_the_nord_palette() {
        assert_eq!(
            inner("\x1b[31mred\x1b[0m"),
            "<span style=\"color:#bf616a\">red</span>"
        );
        assert_eq!(
            inner("\x1b[90mgrey\x1b[0m"),
            "<span style=\"color:#4c566a\">grey</span>"
        );
        assert_eq!(
            inner("\x1b[42mon\x1b[0m"),
            "<span style=\"background:#a3be8c\">on</span>"
        );
        assert_eq!(
            inner("\x1b[106mbright\x1b[0m"),
            "<span style=\"background:#8fbcbb\">bright</span>"
        );
    }

    #[test]
    fn indexed_256_colour_covers_the_cube_and_the_grey_ramp() {
        // 196 = cube (5,0,0) = #ff0000.
        assert_eq!(
            inner("\x1b[38;5;196mx\x1b[0m"),
            "<span style=\"color:#ff0000\">x</span>"
        );
        // 236 = grey 8 + 4*10 = 48 = #303030.
        assert_eq!(
            inner("\x1b[48;5;236mx\x1b[0m"),
            "<span style=\"background:#303030\">x</span>"
        );
        // Below 16 an indexed colour is the basic palette, not a second table.
        assert_eq!(
            inner("\x1b[38;5;4mx\x1b[0m"),
            "<span style=\"color:#81a1c1\">x</span>"
        );
    }

    #[test]
    fn truecolor_is_carried_through_in_both_fg_and_bg() {
        assert_eq!(
            inner("\x1b[38;2;136;192;208maccent\x1b[0m"),
            "<span style=\"color:#88c0d0\">accent</span>"
        );
        assert_eq!(
            inner("\x1b[48;2;46;52;64;38;2;216;222;233mrow\x1b[0m"),
            "<span style=\"color:#d8dee9;background:#2e3440\">row</span>"
        );
    }

    /// The defect that made a `freeze` picture lie: `\x1b[39m` means "back to
    /// the terminal's own foreground", and a renderer that ignores it paints
    /// the rest of the line in the colour that happened to come before.
    #[test]
    fn thirty_nine_really_restores_the_default_foreground() {
        assert_eq!(
            inner("\x1b[38;2;163;190;140mgreen\x1b[39mplain"),
            "<span style=\"color:#a3be8c\">green</span>plain",
            "the text after SGR 39 must carry no colour at all, so the page's \
             --term-fg shows through"
        );
        assert_eq!(
            inner("\x1b[41;1mhot\x1b[49mjust bold"),
            "<span style=\"background:#bf616a;font-weight:700\">hot</span>\
             <span style=\"font-weight:700\">just bold</span>",
            "49 clears the background and leaves every other attribute standing"
        );
    }

    #[test]
    fn a_reset_clears_everything_and_a_bare_m_is_a_reset() {
        assert_eq!(
            inner("\x1b[1;4;38;5;196mloud\x1b[0mquiet"),
            "<span style=\"color:#ff0000;font-weight:700;text-decoration:underline\">loud</span>quiet"
        );
        assert_eq!(inner("\x1b[2mfaint\x1b[mplain"), {
            format!("{}plain", "<span style=\"opacity:.6\">faint</span>")
        });
        // The compound form tmux's capture emits when it re-states a cell.
        assert_eq!(
            inner("\x1b[1mb\x1b[0;2mf"),
            "<span style=\"font-weight:700\">b</span><span style=\"opacity:.6\">f</span>"
        );
    }

    /// A field a terminal would ignore must not behave like `\x1b[0m`.
    ///
    /// The colon sub-parameter form is real and common — `4:3` is a curly
    /// underline, `58:2:…` an underline colour — and parsing it as "unreadable,
    /// therefore zero" reset the run: one sequence tmux can hand over stripped
    /// the colour and the weight off everything after it.
    #[test]
    fn an_unreadable_field_is_skipped_while_an_empty_one_is_a_reset() {
        assert_eq!(
            inner("\x1b[1;31mred\x1b[4:3mstill red\x1b[32mgreen"),
            "<span style=\"color:#bf616a;font-weight:700\">redstill red</span>\
             <span style=\"color:#a3be8c;font-weight:700\">green</span>",
            "an unsupported underline style must leave bold red standing, and a \
             supported colour after it must still apply"
        );
        assert_eq!(
            inner("\x1b[2mfaint\x1b[58:2::1:2:3mstill faint"),
            "<span style=\"opacity:.6\">faintstill faint</span>"
        );
        // An omitted field is a zero, which is what makes `\x1b[;m` a reset.
        assert_eq!(inner("\x1b[1mb\x1b[;mplain"), {
            format!("{}plain", "<span style=\"font-weight:700\">b</span>")
        });
        // A field that reads as a number this renderer does not model is still
        // ignored the way it always was — `53` (overline) changes nothing.
        assert_eq!(
            inner("\x1b[1;53mbold"),
            "<span style=\"font-weight:700\">bold</span>"
        );
    }

    #[test]
    fn every_other_sequence_is_dropped_rather_than_printed() {
        // Cursor addressing, erase, scroll region, cursor hide/show: what a
        // full-screen app emits to paint the screen the capture already holds.
        assert_eq!(inner("\x1b[2J\x1b[H\x1b[?25lhi\x1b[K\x1b[?25h"), "hi");
        // An OSC title, terminated both ways.
        assert_eq!(inner("\x1b]0;tasqx\x07hi"), "hi");
        assert_eq!(inner("\x1b]0;tasqx\x1b\\hi"), "hi");
        // A charset selector eats its second byte rather than printing it.
        assert_eq!(inner("\x1b(Bhi"), "hi");
        // Truncated input cannot panic or loop.
        assert_eq!(inner("hi\x1b"), "hi");
        assert_eq!(inner("hi\x1b["), "hi");
        assert_eq!(inner("\x1b[38;2;1mx"), "x");
    }

    #[test]
    fn adjacent_runs_with_one_style_become_one_span() {
        // The renderer re-states the same colour between cells; the page should
        // not carry a span per cell.
        let html = inner("\x1b[31ma\x1b[31mb\x1b[31mc");
        assert_eq!(html, "<span style=\"color:#bf616a\">abc</span>");
        assert_eq!(html.matches("<span").count(), 1);
    }

    #[test]
    fn a_real_captured_row_renders_as_the_terminal_drew_it() {
        // A `list` row as the binary emits it: dim rail, bold-red priority,
        // then a default-foreground title.
        let html = inner(
            "\x1b[2;38;2;76;86;106m@working\x1b[0m   \x1b[1m14 tasks\x1b[0m \
             \x1b[1;38;2;191;97;106m1 overdue\x1b[0m",
        );
        assert_eq!(
            html,
            "<span style=\"color:#4c566a;opacity:.6\">@working</span>   \
             <span style=\"font-weight:700\">14 tasks</span> \
             <span style=\"color:#bf616a;font-weight:700\">1 overdue</span>"
        );
    }

    #[test]
    fn a_fixture_carries_no_escape_into_the_page() {
        let html = render("\x1b[31mred\x1b[0m\x07\r\n");
        assert!(
            !html.contains('\x1b') && !html.contains('\x07') && !html.contains('\r'),
            "a control byte reached the rendered page: {html:?}"
        );
    }
}
