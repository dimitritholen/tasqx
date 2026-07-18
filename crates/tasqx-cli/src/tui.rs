//! The shared interactive foundation: terminal lifecycle, capability gating and
//! theme→ratatui style mapping (DESIGN.md D26).
//!
//! Everything in this module is deliberately small and boring, because the one
//! thing an interactive layer must never do is hand back a broken terminal. The
//! screens themselves ([`settings`], and `tasqx pick` later) hold no terminal
//! state at all: they are pure state machines that take a key and return an
//! intent, and a `render` function that draws into a ratatui `Frame`. That split
//! is what makes a TUI testable in a repo that fails the build on a warning —
//! the untestable surface here is the handful of lines that actually put a real
//! console into raw mode.

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
/// This reuses `Caps::detect`'s answer rather than asking `IsTerminal` again.
/// A second detection is how the printed output and the interactive output
/// start disagreeing about what the terminal is — and `Caps::detect` already
/// carries the Windows VT probe, which a bare `is_terminal()` does not.
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
    is_interactive_with(caps, std::io::stdout().is_terminal(), std::io::stdin().is_terminal())
}

/// The rule itself, with the stream facts injected.
///
/// Split out because the real `is_interactive` can only ever answer `false`
/// under a test harness — cargo pipes stdout — so the policy would otherwise be
/// untestable at exactly the point it just went wrong. Same move `config.rs`
/// makes by taking an explicit directory and `datetime.rs` by taking an
/// explicit `now`.
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

/// Set when the terminal is in raw mode + the alt screen, so the panic hook
/// knows whether it has anything to undo. Swapped to false by whoever restores
/// first, so the guard and the hook cannot both emit the sequence.
static IN_RAW_MODE: AtomicBool = AtomicBool::new(false);

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
        if restore_once(&IN_RAW_MODE, &mut self.out) && self.raw {
            let _ = ratatui::crossterm::terminal::disable_raw_mode();
        }
    }
}

/// Chain a terminal restore in front of the existing panic hook.
///
/// Untestable by construction — `set_hook` is process-global — so the logic it
/// installs lives in [`restore_once`], which is tested directly. The closure
/// here is the plumbing only.
fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_once(&IN_RAW_MODE, &mut io::stdout());
        prev(info);
    }));
}

/// Map a tasqx role style onto a ratatui style at the terminal's real depth.
///
/// Quantization goes through `Rgb::to_xterm256` / `Rgb::to_ansi16`, the same
/// functions the SGR printer uses. A private nearest-color search here would let
/// the settings screen and `tasqx list` render "nord accent" as two different
/// colors on a 256-color terminal — and the live theme preview exists precisely
/// to show the user the colors they are about to commit to.
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
    if s.dim {
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
    enable_raw_mode()?;
    // Arming happens only once the alt screen is actually entered, so a failure
    // here cannot leave a guard that emits a leave-alt-screen for a screen we
    // never entered — that sequence on a normal screen eats scrollback.
    if let Err(e) =
        ratatui::crossterm::execute!(io::stdout(), EnterAlternateScreen, ratatui::crossterm::cursor::Hide)
    {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{ColorDepth, Rgb, Style};

    fn caps(depth: ColorDepth, ansi: bool) -> Caps {
        Caps { depth, ansi, unicode: true }
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
        assert!(!real(&Caps::PLAIN), "piped/dumb must never open the alt screen");
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
        assert!(!is_interactive_with(&forced, false, true), "piped stdout must refuse");
        assert!(!is_interactive_with(&forced, true, false), "redirected stdin must refuse");
        assert!(!is_interactive_with(&forced, false, false));
        assert!(is_interactive_with(&forced, true, true), "a real terminal still works");
    }

    /// The bytes that give the user their shell back. Pinned literally because
    /// the failure — no cursor, no echo — is invisible to CI and only ever
    /// discovered by a person whose terminal is already ruined.
    #[test]
    fn the_restore_sequence_leaves_the_alt_screen_and_shows_the_cursor() {
        let mut buf: Vec<u8> = Vec::new();
        write_restore(&mut buf).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("\x1b[?1049l"), "must leave the alternate screen: {s:?}");
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

        assert!(restore_once(&flag, &mut first), "the first claim does the work");
        assert!(!restore_once(&flag, &mut second), "the second claim is a no-op");
        assert!(!first.is_empty());
        assert!(second.is_empty(), "a second restore must emit nothing: {second:?}");
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

        assert!(result.is_err(), "the panic must still propagate, not be swallowed");
        let out = String::from_utf8(sink.lock().unwrap().clone()).unwrap();
        assert!(
            out.contains("\x1b[?1049l") && out.contains("\x1b[?25h"),
            "the terminal was not restored while unwinding: {out:?}"
        );
        assert!(!IN_RAW_MODE.load(Ordering::SeqCst), "the flag must be cleared by the restore");
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
        assert!(rgb.to_ansi16() < 16, "an ANSI16 index must stay in the basic range");

        // NO_COLOR drops color and keeps emphasis, exactly as `Style::paint` does.
        let none = rt_style(s, &caps(ColorDepth::None, true));
        assert_eq!(none.fg, None, "NO_COLOR must not colour the TUI");
        assert!(none.add_modifier.contains(Modifier::BOLD), "emphasis survives NO_COLOR");
    }
}
