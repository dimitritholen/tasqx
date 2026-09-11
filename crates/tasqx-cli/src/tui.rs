//! The shared interactive foundation: terminal lifecycle, capability gating and
//! theme→ratatui style mapping (DESIGN.md D26).
//!
//! Everything in this module is deliberately small and boring, because the one
//! thing an interactive layer must never do is hand back a broken terminal. The
//! screens themselves ([`settings`] and [`pick`]) hold no terminal
//! state at all: they are pure state machines that take a key and return an
//! intent, and a `render` function that draws into a ratatui `Frame`. That split
//! is what makes a TUI testable in a repo that fails the build on a warning —
//! the untestable surface here is the handful of lines that actually put a real
//! console into raw mode.

pub mod dashboard;
pub(crate) mod fuzzy;
pub mod memory;
pub mod pick;
pub mod settings;

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use ratatui::style::{Color, Modifier};

use crate::theme::{Caps, ColorDepth};

/// Whether a full-screen alt-screen UI may run at this capability level.
///
/// `Caps::PLAIN` is exactly the set of situations where writing escape codes is
/// wrong: stdout piped or redirected, `TERM=dumb`, and a pre-VT Windows console
/// under `NO_COLOR`. On any of those an alt-screen UI would either dump
/// `\x1b[?1049h` into a file or paint a screen the terminal cannot clear.
///
/// Interactivity is asked of the STREAMS, not of the colour detector.
///
/// This was first written as `*caps != Caps::PLAIN`, reusing `Caps::detect` on
/// the reasoning that one detector beats two. That was wrong, and the bug it
/// caused was worse than escape codes in a pipe: `CLICOLOR_FORCE=1 tasqx config
/// edit | cat` HUNG FOREVER and had to be killed. `Caps` answers "may I emit
/// colour", and `CLICOLOR_FORCE` exists precisely to say "colour even when
/// piped" — the opposite of "a human is at the keyboard". Conflating them let
/// the event loop start against a stdin that never delivers a key.
///
/// Both streams are checked: stdout because the alternate screen is written
/// there, stdin because the loop blocks reading keys from it. Either one being
/// redirected means nobody is driving this.
pub fn is_interactive(caps: &Caps) -> bool {
    use std::io::IsTerminal;
    is_interactive_with(
        caps,
        std::io::stdout().is_terminal(),
        std::io::stdin().is_terminal(),
    )
}

/// The rule itself, with the stream facts injected.
///
/// Split out because the real `is_interactive` can only ever answer `false`
/// under a test harness — cargo pipes stdout — so the policy would otherwise be
/// untestable at exactly the point it just went wrong. Same move `config.rs`
/// makes by taking an explicit directory and `datetime.rs` by taking an
/// explicit `now`.
///
/// The `*caps != Caps::PLAIN` term survives the `CLICOLOR_FORCE` lesson because
/// it is not a second answer to "is a human there": `TERM=dumb` and a pre-VT
/// Windows console under `NO_COLOR` are both real ttys that cannot be painted
/// on, and only `Caps::detect` — which runs the Windows VT probe a bare
/// `is_terminal()` knows nothing about — can tell. It narrows the two stream
/// facts; it never stands in for them.
pub fn is_interactive_with(caps: &Caps, stdout_tty: bool, stdin_tty: bool) -> bool {
    stdout_tty && stdin_tty && *caps != Caps::PLAIN
}

/// The bytes that hand the terminal back: leave the alternate screen and show
/// the cursor again.
///
/// Written through an explicit writer rather than straight to stdout so the
/// restore path is assertable. It has to be — the failure it prevents (a shell
/// left with no echo and no cursor after a panic) cannot be discovered by
/// running the test suite, only by a user hitting it.
pub fn write_restore(w: &mut impl Write) -> io::Result<()> {
    // Written literally rather than through `execute!` so the test can name the
    // exact sequences. These are the standard xterm codes crossterm emits for
    // `LeaveAlternateScreen` and `cursor::Show`.
    w.write_all(b"\x1b[?1049l\x1b[?25h")?;
    w.flush()
}

/// Set when the terminal is in the alternate screen — raw mode or not, since
/// `serve::WatchScreen` arms this same flag for `watch`'s alt-screen-only
/// session (#206) — so the panic hook knows whether it has anything to undo.
/// Swapped to false by whoever restores first, so the guard and the hook
/// cannot both emit the sequence.
pub(crate) static IN_RAW_MODE: AtomicBool = AtomicBool::new(false);

/// Restore the terminal if — and only if — this is the first claim on it.
///
/// This is the body of the panic hook, extracted so it can be tested without
/// installing a process-global hook (`set_hook` is shared state that cargo's
/// parallel test threads would race on). Returns whether it did the work.
///
/// The double-restore it prevents is real: the hook fires, then unwinding runs
/// the guard's `Drop`, and a second `\x1b[?1049l` on a terminal that is no
/// longer in the alt screen scrolls the user's scrollback away.
pub fn restore_once(flag: &AtomicBool, w: &mut impl Write) -> bool {
    if flag.swap(false, Ordering::SeqCst) {
        let _ = write_restore(w);
        true
    } else {
        false
    }
}

/// Restores the terminal when it goes out of scope — including while a panic
/// unwinds, and including on the error paths where an early `return` would
/// otherwise skip a hand-written cleanup call.
///
/// The guard alone is NOT enough, which is why [`install_panic_hook`] exists
/// too: Rust runs the panic hook (which prints the message) *before* unwinding
/// starts, so a guard-only design prints the panic into the alt screen and then
/// wipes it off the user's display. The hook restores first so the message
/// lands on the normal screen; the guard covers every non-panic exit.
pub struct Restore<W: Write> {
    out: W,
    /// Whether a real console needs `disable_raw_mode` as well. False in tests,
    /// which drive the guard with an in-memory writer and no console at all.
    raw: bool,
}

impl<W: Write> Restore<W> {
    pub fn new(out: W, raw: bool) -> Self {
        Restore { out, raw }
    }
}

impl<W: Write> Drop for Restore<W> {
    fn drop(&mut self) {
        // Copied out because the closure and `self.out` both borrow `self`.
        let raw = self.raw;
        restore_terminal(&IN_RAW_MODE, &mut self.out, || {
            if raw {
                let _ = ratatui::crossterm::terminal::disable_raw_mode();
            }
        });
    }
}

/// The whole restore: the escape sequences AND the console mode, behind the one
/// latch, so the two callers cannot each do half of it.
///
/// They used to. `Drop` read `restore_once(..) && self.raw` and the hook threw
/// the answer away, so whichever ran second had already lost the latch and
/// `disable_raw_mode` never fired at all — the hook always runs first, because
/// Rust prints the panic message before it unwinds. The escape codes hid it:
/// the alt screen was left and the cursor came back, so the terminal *looked*
/// restored while still swallowing every keystroke (DESIGN.md D26).
///
/// `disable_raw` is injected rather than called directly because a test process
/// has no console to take out of raw mode — the same seam `is_interactive_with`
/// uses for the stream facts, and the only way to prove the hook side performs
/// this step instead of silently dropping it again.
fn restore_terminal(flag: &AtomicBool, w: &mut impl Write, disable_raw: impl FnOnce()) -> bool {
    if restore_once(flag, w) {
        disable_raw();
        true
    } else {
        // Lost the latch: somebody else already handed the terminal back, and
        // this process may no longer own a console to reconfigure.
        false
    }
}

/// Chain a terminal restore in front of the existing panic hook.
///
/// Untestable by construction — `set_hook` is process-global — so the logic it
/// installs lives in [`panic_restore`], which is tested directly. The closure
/// here is the plumbing only.
///
/// `pub(crate)`: `serve::WatchScreen::enter` installs the same hook rather
/// than growing a second, less-tested copy of it for `watch`'s alt screen.
pub(crate) fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        panic_restore(&mut io::stdout(), || {
            let _ = ratatui::crossterm::terminal::disable_raw_mode();
        });
        prev(info);
    }));
}

/// The body of the panic hook, extracted so it can be driven with an in-memory
/// writer and a console step the test can count.
///
/// Unconditionally asking to leave raw mode is safe: `IN_RAW_MODE` is armed at
/// exactly one place, after both `enable_raw_mode` and `EnterAlternateScreen`
/// have succeeded, so winning the latch means raw mode really is on. A panic
/// raised outside `with_terminal` loses the latch and touches nothing.
fn panic_restore(w: &mut impl Write, disable_raw: impl FnOnce()) -> bool {
    restore_terminal(&IN_RAW_MODE, w, disable_raw)
}

/// Best-effort install of a background thread that restores the terminal
/// before the process dies of SIGTERM, SIGHUP or SIGQUIT — the paths a daily
/// tmux user actually hits from *outside* the app: `timeout 30 tasqx
/// dashboard` in a hotkey wrapper, `pkill tasqx` after an agent run, a
/// systemd/supervisor stop, `kill %1` from the shell. Installed once per
/// process the first time `with_terminal` runs, not once per call: the
/// dashboard's outer loop re-enters `with_terminal` on every return from
/// `pick` (`dashboard_screen.rs`), and a fresh listener thread on every
/// iteration would leak.
///
/// This closes the gap D26's "On terminal safety" note left open on purpose:
/// the panic hook and the `Restore` guard cover a panic and the three
/// key-driven exits (`q`, `esc`, `ctrl-c` — raw mode disables `ISIG`, so a
/// terminal-generated Ctrl-C reaches the app as an ordinary `KeyEvent`, never
/// as a signal). Neither one runs for a signal delivered from outside the
/// process: with no handler installed, the default disposition for these
/// three is to terminate immediately, before any Rust code — `Drop` included
/// — gets a chance to run.
///
/// Unix only. SIGTERM/SIGHUP/SIGQUIT are POSIX signals with no Windows
/// equivalent; the nearest analogues there (console close/logoff/shutdown,
/// via `SetConsoleCtrlHandler`) are a different mechanism this module does
/// not wire up.
#[cfg(unix)]
fn install_signal_teardown() {
    use signal_hook::consts::{SIGHUP, SIGQUIT, SIGTERM};

    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        spawn_signal_listener(&[SIGTERM, SIGHUP, SIGQUIT], |sig| {
            restore_terminal(&IN_RAW_MODE, &mut io::stdout(), || {
                let _ = ratatui::crossterm::terminal::disable_raw_mode();
            });
            // Reset to the default disposition and re-raise, so the process
            // still dies of the exact signal it was sent — a shell or
            // supervisor reading the exit status (`$?`, `WIFSIGNALED`) sees
            // the same thing it would have without any handler installed.
            // `signal_hook`'s own `Signals::forever` docs use this identical
            // pattern to let SIGTSTP take effect after cleanup.
            let _ = signal_hook::low_level::emulate_default_handler(sig);
        });
    });
}

#[cfg(not(unix))]
fn install_signal_teardown() {}

/// Spawn a background thread that blocks for the first of `sigs` to arrive,
/// then runs `on_signal` with the signal number that fired.
///
/// Split out of [`install_signal_teardown`] so the real `signal_hook` wiring —
/// register, block, dispatch — can be driven by a test with a signal that is
/// safe to raise against the test binary itself (`SIGUSR1`), instead of one
/// whose whole point is to end the process. `emulate_default_handler` really
/// does that, which is why it lives in the caller and not here — the same
/// reason `set_hook` stays out of the tested `panic_restore`.
///
/// Returns whether registration succeeded, so a caller that could not get a
/// listener at least knows it is back to the pre-fix behaviour rather than
/// silently believing it is covered.
#[cfg(unix)]
fn spawn_signal_listener(sigs: &[i32], on_signal: impl FnOnce(i32) + Send + 'static) -> bool {
    let mut signals = match signal_hook::iterator::Signals::new(sigs) {
        Ok(s) => s,
        Err(_) => return false,
    };
    std::thread::spawn(move || {
        if let Some(sig) = signals.forever().next() {
            on_signal(sig);
        }
    });
    true
}

/// Map a tasqx role style onto a ratatui style at the terminal's real depth.
///
/// Quantization goes through `Rgb::to_xterm256` / `Rgb::to_ansi16`, the same
/// functions the SGR printer uses. A private nearest-color search here would let
/// the settings screen and `tasqx list` render "nord accent" as two different
/// colors on a 256-color terminal — and the live theme preview exists precisely
/// to show the user the colors they are about to commit to.
/// The index of the first list entry that fits on screen — the scroll window,
/// shared by every screen here that draws a marked list taller than its area.
///
/// A pure function of the cursor, the length of the list and the number of body
/// rows, which is what lets each screen's `App` stay terminal-free while still
/// scrolling. An `App` cannot hold a scroll offset without being told the
/// terminal's height, and the moment it is told that it stops being a state
/// machine a test can drive with nothing but key presses.
///
/// # The failure this closes
///
/// `pick` pushed every match into one `Paragraph` starting at index 0 while its
/// `step` clamped the cursor to the number of MATCHES, not to what fits. On a
/// 24-row terminal that is about 20 visible rows, and `@working` routinely holds
/// more: pressing Down past row 20 moved a cursor nobody could see, no row on
/// screen was marked at all, and `⏎` started a task that had never been drawn.
///
/// `settings` had the identical bug and did not get the identical fix, which is
/// why this function now lives here instead of in `pick`: one rule in one place
/// cannot be applied to one screen and forgotten on the other. There the units
/// are drawn LINES rather than list entries, because an open picker inserts a
/// line per candidate under one row — see `settings::render`.
///
/// # Why centred, and not "scroll only when the cursor would fall off"
///
/// The minimal rule (`start = cursor + 1 - height` once the cursor passes the
/// bottom) is also pure, and it pins the highlight to the last visible row for
/// the whole rest of the list: moving UP then scrolls the list under a cursor
/// that never moves, which reads as the screen ignoring the key. Centring keeps
/// the highlight in the middle of the window while scrolling in either
/// direction, with context above and below it, and degrades to "no scrolling at
/// all" whenever the list fits — which is the common case and the one the
/// existing render tests pin.
///
/// The invariant, asserted exhaustively by `the_cursor_is_always_inside_the_
/// viewport`: for `height >= 1` and a non-empty list, `start <= cursor` and
/// `cursor - start < height`. The marker is therefore on screen for every
/// (cursor, length, height) any of these screens can be in.
pub fn first_visible(cursor: usize, len: usize, height: usize) -> usize {
    // `min` against the tail: without it the last screenful would scroll past
    // the end of the list and draw blank rows below the final entry.
    len.saturating_sub(height)
        .min(cursor.saturating_sub(height / 2))
}

pub fn rt_style(s: crate::theme::Style, caps: &Caps) -> ratatui::style::Style {
    let mut out = ratatui::style::Style::default();
    if !caps.ansi {
        return out;
    }
    if let Some(rgb) = s.fg {
        let color = match caps.depth {
            ColorDepth::Truecolor => Some(Color::Rgb(rgb.r, rgb.g, rgb.b)),
            ColorDepth::Ansi256 => Some(Color::Indexed(rgb.to_xterm256())),
            ColorDepth::Ansi16 => Some(Color::Indexed(rgb.to_ansi16())),
            // NO_COLOR: emphasis survives, color does not.
            ColorDepth::None => None,
        };
        if let Some(c) = color {
            out = out.fg(c);
        }
    }
    if s.bold {
        out = out.add_modifier(Modifier::BOLD);
    }
    // Dim is a colour substitute, so `NO_COLOR` drops it with the colour, as
    // `Style::paint` does (§8's degradation table, #234 item 7). It used to
    // survive here, so under `NO_COLOR` a `mono` screen dimmed what `list`
    // printed plain (D123).
    if s.dim && caps.depth != ColorDepth::None {
        out = out.add_modifier(Modifier::DIM);
    }
    if s.underline {
        out = out.add_modifier(Modifier::UNDERLINED);
    }
    out
}

/// Enter the alt screen, run `body` against a real terminal, and restore on
/// every exit path.
///
/// This is the untestable part of the module, and it is kept to exactly this: no
/// decisions, no state, no rendering. Everything it calls — the gate, the state
/// machine, the renderer, the restore sequence — is tested elsewhere.
pub fn with_terminal<T>(
    body: impl FnOnce(
        &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<io::Stdout>>,
    ) -> io::Result<T>,
) -> io::Result<T> {
    use ratatui::crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen};

    install_panic_hook();
    install_signal_teardown();
    enable_raw_mode()?;
    // Arming happens only once the alt screen is actually entered, so a failure
    // here cannot leave a guard that emits a leave-alt-screen for a screen we
    // never entered — that sequence on a normal screen eats scrollback.
    if let Err(e) = ratatui::crossterm::execute!(
        io::stdout(),
        EnterAlternateScreen,
        ratatui::crossterm::cursor::Hide
    ) {
        let _ = disable_raw_mode();
        return Err(e);
    }
    IN_RAW_MODE.store(true, Ordering::SeqCst);
    // From here every exit — `?`, an early return inside `body`, or a panic
    // unwinding through this frame — runs the guard's Drop.
    let guard = Restore::new(io::stdout(), true);
    let backend = ratatui::backend::CrosstermBackend::new(io::stdout());
    let out = ratatui::Terminal::new(backend).and_then(|mut t| body(&mut t));
    drop(guard);
    out
}

// ============================================================================
// The printed renderers, drawn inside a screen (D123)
// ============================================================================

/// A line the terminal renderers painted (`render::row_line_at`,
/// `render::task_detail`, `render::table_summary`), as the ratatui spans that
/// draw the same cells in the same styles.
///
/// This is how `pick` shows `list`'s rows and `show`'s card without a second
/// renderer for either: the text, the widths, the glyphs and the roles all come
/// from the function the printed command calls, so the screen and the table
/// cannot drift. `watch` made the same choice for the same reason (D102).
///
/// It reads exactly the vocabulary [`crate::theme::Style::paint`] writes, and
/// nothing else: `ESC[…m` with `0`, `1`, `2`, `4`, `38;2;r;g;b`, `38;5;n` and
/// `30–37`/`90–97`. Any other escape is dropped whole — a CSI to its final
/// byte, an OSC to its terminator, a charset switch with its designator — and
/// so is any other control character. The text reaching this has already been
/// through `render::san`; a sequence that is not one of the painter's own is
/// not one this should honour, nor draw as stray letters. `every_role_crosses_into_the_screen_as_the_style_it_was_painted`
/// holds the two ends together over every role of every built-in theme at every
/// depth.
pub(crate) fn painted_line(s: &str) -> ratatui::text::Line<'static> {
    use ratatui::style::Style as RtStyle;
    use ratatui::text::{Line, Span};

    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut style = RtStyle::default();
    let mut text = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            if !c.is_control() {
                text.push(c);
            }
            continue;
        }
        match chars.next() {
            // CSI: parameter and intermediate bytes, then one final byte in
            // `@`..`~`. Only a final `m` with plain digits is the painter's.
            Some('[') => {}
            // OSC (a window title, a hyperlink): everything up to BEL or ST.
            Some(']') => {
                while let Some(o) = chars.next() {
                    if o == '\x07' || (o == '\x1b' && chars.next_if_eq(&'\\').is_some()) {
                        break;
                    }
                }
                continue;
            }
            // Any other escape: its one following byte, and a charset
            // designation's one more (`ESC ( B`).
            Some('(' | ')' | '*' | '+') => {
                chars.next();
                continue;
            }
            _ => continue,
        }
        let mut params = String::new();
        let mut end = None;
        for p in chars.by_ref() {
            if ('@'..='~').contains(&p) {
                end = Some(p);
                break;
            }
            params.push(p);
        }
        if end != Some('m') || !params.chars().all(|p| p.is_ascii_digit() || p == ';') {
            continue;
        }
        if !text.is_empty() {
            spans.push(Span::styled(std::mem::take(&mut text), style));
        }
        let codes: Vec<u16> = params.split(';').map(|p| p.parse().unwrap_or(0)).collect();
        let mut i = 0;
        while i < codes.len() {
            match codes[i] {
                0 => style = RtStyle::default(),
                1 => style = style.add_modifier(Modifier::BOLD),
                2 => style = style.add_modifier(Modifier::DIM),
                4 => style = style.add_modifier(Modifier::UNDERLINED),
                n @ 30..=37 => style = style.fg(Color::Indexed((n - 30) as u8)),
                n @ 90..=97 => style = style.fg(Color::Indexed((n - 90 + 8) as u8)),
                38 if codes.get(i + 1) == Some(&2) && i + 4 < codes.len() => {
                    let c = |k: usize| codes[k] as u8;
                    style = style.fg(Color::Rgb(c(i + 2), c(i + 3), c(i + 4)));
                    i += 4;
                }
                38 if codes.get(i + 1) == Some(&5) => {
                    style = style.fg(Color::Indexed(codes.get(i + 2).copied().unwrap_or(0) as u8));
                    i += 2;
                }
                _ => {}
            }
            i += 1;
        }
    }
    if !text.is_empty() {
        spans.push(Span::styled(text, style));
    }
    Line::from(spans)
}

/// `spans` cut to `width` cells, the cut marked with an ellipsis in the style of
/// the span it falls in, so a line wider than the frame is never stopped
/// mid-word by the frame's own clip (rule 2). A line that fits is untouched.
pub(crate) fn fit_spans(
    spans: Vec<ratatui::text::Span<'static>>,
    width: usize,
    unicode: bool,
) -> Vec<ratatui::text::Span<'static>> {
    use ratatui::text::Span;
    let total: usize = spans.iter().map(|s| crate::render::width(&s.content)).sum();
    if total <= width {
        return spans;
    }
    let mut out = Vec::new();
    let mut used = 0;
    for s in spans {
        let w = crate::render::width(&s.content);
        if used + w < width {
            used += w;
            out.push(s);
            continue;
        }
        // The span the edge falls in: keep what fits, less a cell for the mark.
        let cut = crate::render::truncate(s.content.trim_end(), width - used, unicode);
        let cut = if cut == s.content.trim_end() {
            // Only padding crossed the edge; the mark still says the row went on.
            let dots = if unicode { "…" } else { "..." };
            let room = (width - used).saturating_sub(crate::render::width(dots));
            let keep: String = cut.chars().take(room).collect();
            format!("{keep}{dots}")
        } else {
            cut
        };
        out.push(Span::styled(cut, s.style));
        break;
    }
    out
}

/// A search on a screen's header line: `filter   / query▏   N of M match`,
/// the same on every screen that has one (`pick`, the memory browser).
pub(crate) struct SearchLine<'a> {
    /// The question the list answers (`pick`'s filter); empty when the
    /// screen has none.
    pub filter: &'a str,
    pub query: &'a str,
    /// Whether the query is being typed, which is what draws the caret.
    pub searching: bool,
    pub kept: usize,
    pub total: usize,
}

/// The spans of a [`SearchLine`] in `room` cells, fitted the way `list`'s
/// summary is: the count is a number and never gives way (rule 2), the
/// filter drops first when the line is too long, and then the query loses
/// its HEAD, keeping the end the reader is typing at. One function for both
/// screens (D123): they had two, and the memory browser's still cut the count.
pub(crate) fn search_spans(
    line: &SearchLine,
    room: usize,
    accent: ratatui::style::Style,
    muted: ratatui::style::Style,
    unicode: bool,
) -> Vec<ratatui::text::Span<'static>> {
    use crate::render::width;
    use ratatui::text::Span;
    let count = format!("{} of {} match", line.kept, line.total);
    let caret = match (line.searching, unicode) {
        (false, _) => "",
        (true, true) => "▏",
        (true, false) => "_",
    };
    let room = room.saturating_sub(3 + width(&count));
    let query_w = 2 + width(line.query) + width(caret);
    let mut spans = Vec::new();
    if !line.filter.is_empty() && width(line.filter) + 3 + query_w <= room {
        spans.push(Span::styled(line.filter.to_string(), muted));
        spans.push(Span::raw("   "));
    }
    spans.push(Span::styled(
        "/ ",
        if line.searching { accent } else { muted },
    ));
    let query_room = room.saturating_sub(2 + width(caret));
    spans.push(Span::raw(tail_fit(line.query, query_room, unicode)));
    if line.searching {
        spans.push(Span::styled(caret, accent));
    }
    spans.push(Span::raw("   "));
    spans.push(Span::styled(count, muted));
    spans
}

/// The END of `text` in at most `cells` cells, an ellipsis standing for what
/// was cut from the front: the part of a query the reader is typing at.
pub(crate) fn tail_fit(text: &str, cells: usize, unicode: bool) -> String {
    use crate::render::width;
    if width(text) <= cells {
        return text.to_string();
    }
    let dots = if unicode { "…" } else { "..." };
    let keep = cells.saturating_sub(width(dots));
    let mut tail: Vec<char> = Vec::new();
    let mut used = 0;
    for c in text.chars().rev() {
        let cw = width(&c.to_string());
        if used + cw > keep {
            break;
        }
        used += cw;
        tail.push(c);
    }
    tail.reverse();
    format!("{dots}{}", tail.into_iter().collect::<String>())
}

/// A screen's bottom row: a cell of margin, the hints `keys` can fit, and —
/// when `position` (first line shown, lines shown, lines in all) says there is
/// more than one screenful — where the reader is, right-aligned. The position
/// is a number, so it is measured FIRST and the hints give way to it; the two
/// screens that scroll a body (`pick`'s card, the memory browser's doc) share
/// this rather than each appending it after the hints had taken the row.
pub(crate) fn key_bar(
    keys: &[Key],
    width: u16,
    position: Option<(usize, usize, usize)>,
    accent: ratatui::style::Style,
    muted: ratatui::style::Style,
    unicode: bool,
) -> Vec<ratatui::text::Span<'static>> {
    use ratatui::text::Span;
    let pos = match position {
        Some((scroll, rows, total)) if total > rows => {
            let dash = if unicode { "–" } else { "-" };
            let to = (scroll + rows).min(total);
            format!("{}{dash}{to} of {total} ", scroll + 1)
        }
        _ => String::new(),
    };
    let pos_w = crate::render::width(&pos);
    let room = (width as usize).saturating_sub(1 + pos_w);
    let mut spans = vec![Span::raw(" ")];
    spans.extend(footer_spans(keys, room as u16, accent, muted, unicode));
    if !pos.is_empty() {
        let used: usize = spans.iter().map(|s| crate::render::width(&s.content)).sum();
        let gap = (width as usize).saturating_sub(used + pos_w);
        spans.push(Span::raw(" ".repeat(gap)));
        spans.push(Span::styled(pos, muted));
    }
    spans
}

// ============================================================================
// The key bar: one table drives the footer and the help (D62)
// ============================================================================

/// One binding, in the ONE table a screen's help and its footer both read.
///
/// They used to be two. The overlay iterated `KEYS`; the footer built from a
/// `hints` literal of its own, under a doc comment asserting the two "cannot
/// drift". They had: `w`, `R`, `g`/`G` and `S-tab` were advertised nowhere but
/// behind `?`, so the burndown window was a control the screen never mentioned
/// and a reader who did not open the help had no way to learn it was there
/// (D62). A second list of strings is a second path to the data, and this is
/// that rule from `CLAUDE.md` applied to the key vocabulary.
pub struct Key {
    /// The binding as a reader types it, e.g. `"j / k"`. Parsed by the tests
    /// that bind this table to `on_key`, so the shape is load-bearing: tokens
    /// split on `/`, and anything that is not a single character or one of the
    /// named non-`Char` keys makes them fail loudly rather than skip it.
    pub keys: &'static str,
    /// The overlay's line — a sentence, because the overlay has the room.
    pub help: &'static str,
    /// How the footer spells this binding, or `None` for one it deliberately
    /// withholds.
    ///
    /// `ctrl-c` is the only `None`, because `q` already answers "how do I
    /// leave" — an omission this table states rather than one the footer
    /// arrives at by drifting.
    pub footer: Option<Hint>,
}

/// A binding as the footer draws it: shorter than the overlay's spelling, and
/// ranked, because a 56-column footer cannot hold eleven of them.
pub struct Hint {
    /// The keys the footer prints, which is not always the overlay's spelling.
    /// `tab / S-tab` prints as `tab` and `q / esc` as `q` — the second half of
    /// each is a variant of the first. `j / k` prints as `j/k` and `g / G` as
    /// `g/G`, because there the second half is the other direction and a
    /// footer naming one of a pair is a footer that has hidden the other.
    pub keys: &'static str,
    /// The word beside the keys. One word: the footer is a reminder, and the
    /// sentence lives in `Key::help`.
    pub word: &'static str,
    /// Which hints survive a narrow footer — low numbers first. `?`, `q` and
    /// `p` are ranked ahead of everything because they are the three a reader
    /// cannot do without, which is the judgement the deleted `Rung::Xs` arm
    /// used to carry as a literal.
    pub rank: u8,
}

/// The footer's spans: as many of `keys` as the width affords, lowest rank
/// first, drawn in table order.
///
/// The width decides, not the rung. A rung match here would be the second list
/// again in a different costume — and it made the wide footer a fixed eight
/// even on a 200-column terminal with room for every one of them. What the rank
/// buys is that the *narrow* end keeps `?`, `q` and `p`: the three a reader
/// cannot do without, chosen by the table rather than by a literal beside it.
///
/// A hint is taken whole or not at all. A footer cut mid-word reads as a broken
/// screen, which is why this measures before it pushes rather than truncating
/// afterwards — the idiom `build_bar` established for the status line.
pub(crate) fn footer_spans(
    keys: &[Key],
    width: u16,
    accent: ratatui::style::Style,
    muted: ratatui::style::Style,
    unicode: bool,
) -> Vec<ratatui::text::Span<'static>> {
    use ratatui::text::Span;

    // A hint's keys may be arrows; a terminal without Unicode gets words, as
    // every other glyph on these screens degrades.
    let spell = |k: &str| -> String {
        if unicode {
            k.to_string()
        } else {
            k.replace("↑↓", "up/dn")
                .replace('↑', "up")
                .replace('↓', "dn")
        }
    };

    let mut ranked: Vec<(usize, &Hint)> = keys
        .iter()
        .enumerate()
        .filter_map(|(i, k)| k.footer.as_ref().map(|h| (i, h)))
        .collect();
    ranked.sort_by_key(|(_, h)| h.rank);

    let mut budget = width as usize;
    let mut taken: Vec<(usize, &Hint)> = Vec::new();
    for (i, h) in ranked {
        // The keys, a space, the word, and the three cells of gutter that
        // separate this hint from the next one.
        let need = crate::render::width(&spell(h.keys)) + 1 + crate::render::width(h.word) + 3;
        if need > budget {
            continue;
        }
        budget -= need;
        taken.push((i, h));
    }
    taken.sort_by_key(|(i, _)| *i);

    let mut spans = Vec::new();
    for (_, h) in taken {
        spans.push(Span::styled(spell(h.keys), accent));
        spans.push(Span::styled(format!(" {}   ", h.word), muted));
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{ColorDepth, Rgb, Style};

    /// The viewport invariant, exhaustively: whatever the cursor, the length of
    /// the list and the number of body rows, the highlighted entry is inside the
    /// window that gets drawn.
    ///
    /// Asserted over every combination rather than at a few sizes, because the
    /// bug it replaces was invisible at every size the screens' other render
    /// tests use — each fixture fits its own body — and appeared only when the
    /// list outgrew the area. The two halves are what "on screen" means:
    /// `start <= cursor` (the marker is not above the window) and
    /// `cursor - start < height` (not below it).
    ///
    /// It lives here rather than beside one screen because the function does:
    /// `pick` and `settings` had the same bug, and only one of them got the fix.
    #[test]
    fn the_cursor_is_always_inside_the_viewport() {
        for len in 1..40usize {
            for height in 1..24usize {
                for cursor in 0..len {
                    let start = first_visible(cursor, len, height);
                    assert!(
                        start <= cursor,
                        "len {len} height {height} cursor {cursor}: window starts at {start}, \
                         below the cursor — the marker is off the top"
                    );
                    assert!(
                        cursor - start < height,
                        "len {len} height {height} cursor {cursor}: window starts at {start}, \
                         so the cursor is {} rows past the bottom",
                        cursor - start - height + 1
                    );
                    // And the window may not run off the end of the list, which
                    // would draw blank rows under the last entry.
                    assert!(
                        start + height <= len || start == 0,
                        "len {len} height {height} cursor {cursor}: window {start}..{} overruns",
                        start + height
                    );
                }
            }
        }
    }

    /// `painted_line` is the seam between the printed renderers and the
    /// screen that shows them (D123). It must read back every style the
    /// painter writes, as the style `rt_style` would have drawn directly, or
    /// `pick` would draw `list`'s rows in colours `list` never prints.
    ///
    /// The one deliberate difference is `dim` under `NO_COLOR`: the painter
    /// drops it there (#234 item 7, §8's degradation table), and the seam
    /// follows the painter, because the printed output is the reference.
    #[test]
    fn every_role_crosses_into_the_screen_as_the_style_it_was_painted() {
        use ratatui::text::Span;
        let depths = [
            ColorDepth::Truecolor,
            ColorDepth::Ansi256,
            ColorDepth::Ansi16,
            ColorDepth::None,
        ];
        for name in crate::theme::BUILTINS {
            let th = crate::theme::load(name, None);
            let mut styles: Vec<(String, Style)> = th
                .role_names()
                .into_iter()
                .map(|r| (r.clone(), th.role(&r)))
                .collect();
            for t in [0.0, 0.5, 1.0] {
                styles.push((format!("ramp {t}"), th.ramp_style(t)));
                styles.push((format!("ramp {t} bold"), th.ramp_style(t).bold()));
            }
            for depth in depths {
                let c = caps(depth, true);
                for (role, style) in &styles {
                    let mut painted = *style;
                    if depth == ColorDepth::None {
                        painted.dim = false;
                    }
                    let want = rt_style(painted, &c);
                    let got = painted_line(&style.paint("cell", &c));
                    assert_eq!(
                        got.spans,
                        vec![Span::styled("cell", want)],
                        "{name} {role} at {depth:?}"
                    );
                }
            }
        }
    }

    /// Text is kept and styles end where the painter resets them; a byte that
    /// is not the painter's own is never drawn.
    #[test]
    fn a_painted_line_keeps_its_text_and_drops_foreign_escapes() {
        let c = caps(ColorDepth::Truecolor, true);
        let accent = Style::fg(Rgb::new(1, 2, 3)).bold();
        let line = painted_line(&format!("a {} b\x1b[2Jc\x07", accent.paint("x", &c)));
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "a x bc");
        assert_eq!(line.spans[1].style.fg, Some(Color::Rgb(1, 2, 3)));
        assert_eq!(line.spans[2].style, ratatui::style::Style::default());
    }

    /// Any escape that is not the painter's own SGR is dropped whole, not
    /// just its ESC byte: an OSC title, a private CSI, a charset switch.
    #[test]
    fn a_foreign_escape_is_dropped_whole() {
        let line = painted_line("a\x1b]0;evil\x07b\x1b[?25hc\x1b(Bd\x1b]2;t\x1b\\e");
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "abcde");
    }

    /// `NO_COLOR` keeps bold and underline and drops everything else, dim
    /// included (§8, #234 item 7) — on a screen as on the printed path, so a
    /// `mono` screen does not dim what `list` prints plain.
    #[test]
    fn no_color_drops_dim_on_a_screen_too() {
        let s = Style::fg(Rgb::new(1, 2, 3)).dim().bold();
        let none = rt_style(s, &caps(ColorDepth::None, true));
        assert!(!none.add_modifier.contains(Modifier::DIM), "{none:?}");
        assert!(none.add_modifier.contains(Modifier::BOLD));
    }

    fn caps(depth: ColorDepth, ansi: bool) -> Caps {
        Caps {
            depth,
            ansi,
            unicode: true,
        }
    }

    /// `IN_RAW_MODE` is a process-global, so the two tests that drive it have to
    /// be serialised or cargo's parallel test threads interleave one test's
    /// arming with the other's assertion. Same reason `config.rs` takes an
    /// explicit directory instead of setting `$TASQX_CONFIG_DIR`.
    static RAW_FLAG_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// The refusal that stops `tasqx config edit` writing `\x1b[?1049h` into a
    /// pipe or a redirect. A TUI that starts anyway produces a file full of
    /// escape codes and a command that appears to hang, which is the single most
    /// common way an interactive subcommand breaks a script.
    #[test]
    fn a_plain_capability_level_refuses_the_alt_screen() {
        let real = |c: &Caps| is_interactive_with(c, true, true);
        assert!(
            !real(&Caps::PLAIN),
            "piped/dumb must never open the alt screen"
        );
        let truecolor = caps(ColorDepth::Truecolor, true);
        let ansi16 = caps(ColorDepth::Ansi16, true);
        // NO_COLOR is still a terminal: emphasis-only, but interactive.
        let nocolor = caps(ColorDepth::None, true);
        assert!(real(&truecolor));
        assert!(real(&ansi16));
        assert!(real(&nocolor));
    }

    /// Interactivity is a question about the STREAMS, and asking `Caps` instead
    /// was a real bug, not a theoretical one: `CLICOLOR_FORCE=1 tasqx config
    /// edit | cat` HUNG FOREVER and had to be killed. `Caps::detect` reports a
    /// TTY when colour is forced — that flag exists to say "colour even when
    /// piped", which is the opposite of "a human is at the keyboard" — so the
    /// event loop started against a stdin that never delivered a key.
    ///
    /// Both streams matter: stdout carries the alternate screen, stdin feeds the
    /// loop. Either one redirected means nobody is driving this.
    #[test]
    fn a_redirected_stream_refuses_even_when_colour_is_forced() {
        let forced = caps(ColorDepth::Truecolor, true);
        assert!(
            !is_interactive_with(&forced, false, true),
            "piped stdout must refuse"
        );
        assert!(
            !is_interactive_with(&forced, true, false),
            "redirected stdin must refuse"
        );
        assert!(!is_interactive_with(&forced, false, false));
        assert!(
            is_interactive_with(&forced, true, true),
            "a real terminal still works"
        );
    }

    /// The bytes that give the user their shell back. Pinned literally because
    /// the failure — no cursor, no echo — is invisible to CI and only ever
    /// discovered by a person whose terminal is already ruined.
    #[test]
    fn the_restore_sequence_leaves_the_alt_screen_and_shows_the_cursor() {
        let mut buf: Vec<u8> = Vec::new();
        write_restore(&mut buf).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(
            s.contains("\x1b[?1049l"),
            "must leave the alternate screen: {s:?}"
        );
        assert!(s.contains("\x1b[?25h"), "must show the cursor again: {s:?}");
    }

    /// Exactly one restore. The hook fires first, then unwinding drops the
    /// guard; if both emitted the sequence the second `\x1b[?1049l` would land
    /// on a terminal already back on the normal screen and eat the scrollback.
    #[test]
    fn the_restore_runs_once_even_when_two_paths_claim_it() {
        let flag = AtomicBool::new(true);
        let mut first: Vec<u8> = Vec::new();
        let mut second: Vec<u8> = Vec::new();

        assert!(
            restore_once(&flag, &mut first),
            "the first claim does the work"
        );
        assert!(
            !restore_once(&flag, &mut second),
            "the second claim is a no-op"
        );
        assert!(!first.is_empty());
        assert!(
            second.is_empty(),
            "a second restore must emit nothing: {second:?}"
        );
    }

    /// A guard that never armed must not emit anything. Restoring a terminal
    /// that was never put into the alt screen writes `\x1b[?1049l` to whatever
    /// stdout happens to be — including a pipe.
    #[test]
    fn a_guard_over_an_unarmed_terminal_writes_nothing() {
        let _lock = RAW_FLAG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        IN_RAW_MODE.store(false, Ordering::SeqCst);
        let mut sink: Vec<u8> = Vec::new();
        drop(Restore::new(&mut sink, false));
        assert!(sink.is_empty(), "unarmed guard emitted {sink:?}");
    }

    /// The classic way a TUI ruins someone's day: a panic inside the alt screen
    /// with raw mode on. `Drop` runs while the panic unwinds, so the guard is
    /// what covers the panic path (the hook covers *ordering* against the panic
    /// message, which `set_hook` being process-global keeps out of this test —
    /// its body is `restore_once`, covered above).
    ///
    /// Serialised against the other `IN_RAW_MODE` test by `RAW_FLAG_LOCK`,
    /// because the flag is a process-global that cargo's parallel test threads
    /// would otherwise interleave. `catch_unwind` stops the unwind inside the
    /// closure, so this frame never unwinds and the lock is never poisoned.
    #[test]
    fn a_panic_inside_the_alt_screen_still_restores_the_terminal() {
        let _lock = RAW_FLAG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // A writer the panicking scope can own and the test can read after.
        let sink = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
        struct Shared(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
        impl Write for Shared {
            fn write(&mut self, b: &[u8]) -> io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        IN_RAW_MODE.store(true, Ordering::SeqCst);
        let inner = sink.clone();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _guard = Restore::new(Shared(inner), false);
            panic!("render blew up");
        }));

        assert!(
            result.is_err(),
            "the panic must still propagate, not be swallowed"
        );
        let out = String::from_utf8(sink.lock().unwrap().clone()).unwrap();
        assert!(
            out.contains("\x1b[?1049l") && out.contains("\x1b[?25h"),
            "the terminal was not restored while unwinding: {out:?}"
        );
        assert!(
            !IN_RAW_MODE.load(Ordering::SeqCst),
            "the flag must be cleared by the restore"
        );
    }

    /// The gap D26's "On terminal safety" note left open on purpose: a panic
    /// and the three key-driven exits (`q`, `esc`, `ctrl-c`) are covered, but
    /// nothing reached the restore path when the process was asked to
    /// terminate from *outside* it — a real SIGTERM, SIGHUP or SIGQUIT sent by
    /// `kill`, `timeout`'s deadline, or a supervisor stop.
    ///
    /// Driven with `SIGUSR1` rather than one of the three production signals,
    /// so the test binary survives the assertion instead of dying with it —
    /// `install_signal_teardown` itself, which really does end the process via
    /// `emulate_default_handler`, is exercised only by construction, the same
    /// way `install_panic_hook`'s `set_hook` call is (see its doc comment).
    /// `spawn_signal_listener` is the real `signal_hook` wiring in both cases;
    /// only the signal and the final disposition differ.
    #[cfg(unix)]
    #[test]
    fn a_real_signal_reaches_the_restore_before_the_process_would_die() {
        let _lock = RAW_FLAG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        IN_RAW_MODE.store(true, Ordering::SeqCst);

        let sink: std::sync::Arc<std::sync::Mutex<Vec<u8>>> = Default::default();
        let disabled = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (tx, rx) = std::sync::mpsc::channel();

        let sink2 = sink.clone();
        let disabled2 = disabled.clone();
        let installed = spawn_signal_listener(&[signal_hook::consts::SIGUSR1], move |sig| {
            let mut w = sink2.lock().unwrap_or_else(|e| e.into_inner());
            restore_terminal(&IN_RAW_MODE, &mut *w, || {
                disabled2.fetch_add(1, Ordering::SeqCst);
            });
            let _ = tx.send(sig);
        });
        assert!(
            installed,
            "SIGUSR1 registration must succeed on a real unix process"
        );

        signal_hook::low_level::raise(signal_hook::consts::SIGUSR1)
            .expect("raising a signal against our own process must succeed");

        let seen = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the listener thread must receive the signal and call back");
        assert_eq!(seen, signal_hook::consts::SIGUSR1);

        let out = String::from_utf8(sink.lock().unwrap_or_else(|e| e.into_inner()).clone())
            .expect("restore bytes are valid UTF-8");
        assert!(
            out.contains("\x1b[?1049l") && out.contains("\x1b[?25h"),
            "a signal delivered from outside the process must still restore \
             the terminal: {out:?}"
        );
        assert_eq!(
            disabled.load(Ordering::SeqCst),
            1,
            "the signal path must take the console out of raw mode too, not \
             just emit the escape sequences"
        );
        assert!(
            !IN_RAW_MODE.load(Ordering::SeqCst),
            "the flag must be cleared by the restore"
        );
    }

    /// The half of the restore no escape sequence can do. `\x1b[?1049l` leaves
    /// the alt screen and `\x1b[?25h` shows the cursor, but raw mode is a
    /// termios (or Windows console-mode) setting, and process exit does not put
    /// it back — so the hook has to call `disable_raw_mode` itself.
    ///
    /// It used to not: the hook discarded `restore_once`'s answer, and because
    /// Rust runs the hook *before* unwinding, the hook won the latch and the
    /// guard's `Drop` then short-circuited on `restore_once(..) && self.raw`.
    /// Nothing anywhere called `disable_raw_mode`, and `tasqx config edit`
    /// panicking left the user in the shell DESIGN.md D26 names: no echo, no
    /// line editing, Ctrl-C dead.
    ///
    /// The console step is injected because a test process has no console to put
    /// into raw mode — the same seam `is_interactive_with` uses for the stream
    /// facts, for the same reason.
    #[test]
    fn the_panic_hook_takes_the_console_out_of_raw_mode() {
        let _lock = RAW_FLAG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        IN_RAW_MODE.store(true, Ordering::SeqCst);
        let disabled = std::cell::Cell::new(0usize);
        let mut sink: Vec<u8> = Vec::new();

        assert!(
            panic_restore(&mut sink, || disabled.set(disabled.get() + 1)),
            "the hook is the first claim on an armed terminal"
        );
        assert_eq!(
            disabled.get(),
            1,
            "the hook left the console in raw mode; escape codes cannot undo termios"
        );
        assert!(
            String::from_utf8(sink).unwrap().contains("\x1b[?1049l"),
            "the hook must also leave the alternate screen"
        );
        assert!(
            !IN_RAW_MODE.load(Ordering::SeqCst),
            "the hook must consume the latch so the guard does not restore twice"
        );
    }

    /// The loser of the latch must not touch the console mode. The hook is
    /// process-global: it fires for every panic, including ones raised long
    /// after `with_terminal` returned. Calling `disable_raw_mode` there would
    /// reach for a console this process is no longer driving.
    #[test]
    fn a_lost_restore_latch_leaves_the_console_mode_alone() {
        let flag = AtomicBool::new(false);
        let disabled = std::cell::Cell::new(0usize);
        let mut sink: Vec<u8> = Vec::new();

        let claimed = restore_terminal(&flag, &mut sink, || disabled.set(disabled.get() + 1));
        assert!(!claimed, "an unarmed restore must not claim the terminal");
        assert_eq!(
            disabled.get(),
            0,
            "an unarmed restore must not disable raw mode"
        );
        assert!(sink.is_empty(), "an unarmed restore emitted {sink:?}");
    }

    /// The TUI must render through the theme at the terminal's real depth, not
    /// pick colors of its own. A truecolor style quantized differently here than
    /// in `Style::paint` would make the live preview show colors the printed
    /// output never produces — which would make the preview actively misleading
    /// rather than merely decorative.
    #[test]
    fn styles_quantize_to_the_same_color_the_sgr_printer_picks() {
        let rgb = Rgb::new(0x88, 0xc0, 0xd0);
        let s = Style::fg(rgb).bold();

        let true_ = rt_style(s, &caps(ColorDepth::Truecolor, true));
        assert_eq!(true_.fg, Some(Color::Rgb(0x88, 0xc0, 0xd0)));
        assert!(true_.add_modifier.contains(Modifier::BOLD));

        // The indexed forms must agree with the printer's own quantization.
        let i256 = rt_style(s, &caps(ColorDepth::Ansi256, true));
        assert_eq!(i256.fg, Some(Color::Indexed(rgb.to_xterm256())));
        let i16 = rt_style(s, &caps(ColorDepth::Ansi16, true));
        assert_eq!(i16.fg, Some(Color::Indexed(rgb.to_ansi16())));
        assert!(
            rgb.to_ansi16() < 16,
            "an ANSI16 index must stay in the basic range"
        );

        // NO_COLOR drops color and keeps emphasis, exactly as `Style::paint` does.
        let none = rt_style(s, &caps(ColorDepth::None, true));
        assert_eq!(none.fg, None, "NO_COLOR must not colour the TUI");
        assert!(
            none.add_modifier.contains(Modifier::BOLD),
            "emphasis survives NO_COLOR"
        );
    }
}
