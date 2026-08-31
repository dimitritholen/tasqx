//! Semantic, role-based theming + graceful terminal degradation (DESIGN.md §8).
//!
//! Styles attach to *roles* (`header`, `project`, `overdue`, `urgency.ramp`…),
//! never to per-command literal colors. Five themes ship compiled into the
//! binary (`nord`, `gruvbox`, `dracula`, `solarized`, `mono`); a user file
//! `~/.config/tasqx/themes/NAME.toml` may `extends` a built-in and partially
//! override it.
//!
//! The *same* render pipeline adapts to the terminal's capability: truecolor →
//! 24-bit, 256-color → nearest xterm-256, basic → ANSI 16, `NO_COLOR` → emphasis
//! only (no color), piped/dumb → zero ANSI (script-safe). Unicode box-drawing
//! degrades to ASCII on the same signal.

use std::collections::BTreeMap;
use std::io::IsTerminal;

// ============================================================================
// Color primitives
// ============================================================================

/// A 24-bit color anchor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Rgb { r, g, b }
    }

    /// Parse `#rrggbb` (leading `#` optional). Returns None on anything else.
    pub fn parse_hex(s: &str) -> Option<Rgb> {
        let h = s.trim().trim_start_matches('#');
        // Guard the byte-index slicing below against multibyte input: a 6-*byte*
        // non-ASCII value (e.g. "1€45") would otherwise pass the length check and
        // panic on a UTF-8 char boundary. Untrusted theme files reach here.
        if h.len() != 6 || !h.is_ascii() {
            return None;
        }
        let r = u8::from_str_radix(&h[0..2], 16).ok()?;
        let g = u8::from_str_radix(&h[2..4], 16).ok()?;
        let b = u8::from_str_radix(&h[4..6], 16).ok()?;
        Some(Rgb { r, g, b })
    }

    /// `#rrggbb` — used for the HTML palette so terminal and web match.
    pub fn hex(&self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    /// Linear interpolation toward `other` (`t` in 0..=1).
    fn lerp(&self, other: &Rgb, t: f64) -> Rgb {
        let t = t.clamp(0.0, 1.0);
        let mix = |a: u8, b: u8| -> u8 {
            (a as f64 + (b as f64 - a as f64) * t)
                .round()
                .clamp(0.0, 255.0) as u8
        };
        Rgb {
            r: mix(self.r, other.r),
            g: mix(self.g, other.g),
            b: mix(self.b, other.b),
        }
    }

    /// SGR foreground parameter for a given depth (`None` when depth carries no
    /// color). Quantizes truecolor down to the terminal's real palette.
    fn sgr_fg(&self, depth: ColorDepth) -> Option<String> {
        match depth {
            ColorDepth::Truecolor => Some(format!("38;2;{};{};{}", self.r, self.g, self.b)),
            ColorDepth::Ansi256 => Some(format!("38;5;{}", self.to_xterm256())),
            ColorDepth::Ansi16 => Some(self.to_ansi16_fg()),
            ColorDepth::None => None,
        }
    }

    /// Nearest xterm-256 index (16..=231 color cube, 232..=255 grayscale).
    ///
    /// Public because the TUI backend needs the same index the SGR printer
    /// picks. A second nearest-color search in the TUI module would let the
    /// alt-screen settings screen and the printed table disagree about what
    /// "nord accent" looks like on a 256-color terminal, which is precisely the
    /// question the live theme preview is there to answer.
    pub fn to_xterm256(self) -> u8 {
        const LEVELS: [i32; 6] = [0, 95, 135, 175, 215, 255];
        let nearest = |v: u8| -> (usize, i32) {
            let mut best = 0usize;
            let mut bestd = i32::MAX;
            for (i, l) in LEVELS.iter().enumerate() {
                let d = (v as i32 - l).abs();
                if d < bestd {
                    bestd = d;
                    best = i;
                }
            }
            (best, LEVELS[best])
        };
        let (ri, rv) = nearest(self.r);
        let (gi, gv) = nearest(self.g);
        let (bi, bv) = nearest(self.b);
        let cube_idx = 16 + 36 * ri as i32 + 6 * gi as i32 + bi as i32;
        let cube_dist = sqdist(self.r, self.g, self.b, rv as u8, gv as u8, bv as u8);

        // Grayscale ramp 232..=255 (levels 8,18,…,238).
        let gray = (self.r as i32 + self.g as i32 + self.b as i32) / 3;
        let gi2 = ((gray - 8).clamp(0, 238) as f64 / 10.0).round() as i32;
        let gi2 = gi2.clamp(0, 23);
        let gval = 8 + gi2 * 10;
        let gray_idx = 232 + gi2;
        let gray_dist = sqdist(self.r, self.g, self.b, gval as u8, gval as u8, gval as u8);

        if gray_dist < cube_dist {
            gray_idx as u8
        } else {
            cube_idx as u8
        }
    }

    /// Nearest of the 16 standard ANSI colors, as an SGR fg code (`30..37` /
    /// `90..97`).
    fn to_ansi16_fg(self) -> String {
        let i = self.to_ansi16();
        if i < 8 {
            format!("3{}", i) // 30..37
        } else {
            format!("9{}", i - 8) // 90..97
        }
    }

    /// Index 0..=15 of the nearest standard ANSI color.
    ///
    /// Split out of `to_ansi16_fg` for the same reason `to_xterm256` is public:
    /// the TUI needs the color, not an SGR string, and two independent nearest-
    /// color searches would let the two renderers disagree on a 16-color
    /// terminal.
    pub fn to_ansi16(self) -> u8 {
        // Canonical xterm 16-color palette.
        const PAL: [(u8, u8, u8); 16] = [
            (0, 0, 0),
            (205, 0, 0),
            (0, 205, 0),
            (205, 205, 0),
            (0, 0, 238),
            (205, 0, 205),
            (0, 205, 205),
            (229, 229, 229),
            (127, 127, 127),
            (255, 0, 0),
            (0, 255, 0),
            (255, 255, 0),
            (92, 92, 255),
            (255, 0, 255),
            (0, 255, 255),
            (255, 255, 255),
        ];
        let mut best = 0usize;
        let mut bestd = i32::MAX;
        for (i, (r, g, b)) in PAL.iter().enumerate() {
            let d = sqdist(self.r, self.g, self.b, *r, *g, *b);
            if d < bestd {
                bestd = d;
                best = i;
            }
        }
        best as u8
    }
}

fn sqdist(r1: u8, g1: u8, b1: u8, r2: u8, g2: u8, b2: u8) -> i32 {
    let dr = r1 as i32 - r2 as i32;
    let dg = g1 as i32 - g2 as i32;
    let db = b1 as i32 - b2 as i32;
    dr * dr + dg * dg + db * db
}

// ============================================================================
// Capability detection (graceful degradation)
// ============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorDepth {
    Truecolor,
    Ansi256,
    Ansi16,
    None,
}

/// The resolved terminal capability that the render pipeline adapts to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caps {
    /// Depth of *color* the terminal accepts.
    pub depth: ColorDepth,
    /// May we emit any ANSI/SGR escape at all (bold/underline included)?
    /// False when piped or on a dumb terminal — output is then byte-plain.
    pub ansi: bool,
    /// May we use Unicode box-drawing / block glyphs? Falls to ASCII otherwise.
    pub unicode: bool,
}

impl Caps {
    /// Fully plain: no ANSI, ASCII only. The script-safe / piped baseline.
    pub const PLAIN: Caps = Caps {
        depth: ColorDepth::None,
        ansi: false,
        unicode: false,
    };

    /// Detect from the live process environment, enabling Windows VT if needed.
    pub fn detect() -> Caps {
        // Honor the de-facto CLICOLOR_FORCE (and a tasqx-specific alias) so color
        // can be forced through a pipe — standard for tools that feed `less -R`
        // or CI logs. NO_COLOR still wins (checked in `detect_from`).
        let force = std::env::var_os("TASQX_FORCE_COLOR").is_some()
            || std::env::var("CLICOLOR_FORCE")
                .map(|v| v != "0")
                .unwrap_or(false);
        let is_tty = std::io::stdout().is_terminal() || force;
        let env = EnvCaps::from_env();
        // On Windows the console needs VT explicitly enabled for ANSI to work.
        let vt_ok = enable_vt();
        detect_from(&env, is_tty, vt_ok)
    }
}

/// The live width of the output surface, in cells.
///
/// Three sources, in falling order of authority: an explicit `COLUMNS` (the
/// de-facto override, and the only handle a user has when tasqx runs inside
/// something that lies about its size), the terminal itself, and — when the
/// stream is a pipe or the terminal will not answer — [`Ctx::DEFAULT_COLS`].
///
/// It is read ONCE, at context construction, rather than per table: a resize
/// mid-command would otherwise draw a header at one width and its rows at
/// another, and every row of one `list` must agree with every other.
pub fn detect_cols() -> usize {
    if let Some(n) = std::env::var("COLUMNS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
    {
        return n;
    }
    // A redirected stdout has no width — asking the *terminal* would answer with
    // the size of a window the bytes are not going to, so don't ask.
    if !std::io::stdout().is_terminal() {
        return Ctx::DEFAULT_COLS;
    }
    ratatui::crossterm::terminal::size()
        .map(|(w, _)| w as usize)
        .unwrap_or(Ctx::DEFAULT_COLS)
}

/// The environment inputs that drive detection — split out so the decision is a
/// pure function we can unit-test at every capability level.
#[derive(Clone, Debug, Default)]
pub struct EnvCaps {
    pub no_color: bool,
    pub term: String,
    pub colorterm: String,
}

impl EnvCaps {
    fn from_env() -> Self {
        EnvCaps {
            no_color: std::env::var_os("NO_COLOR").is_some(),
            term: std::env::var("TERM").unwrap_or_default(),
            colorterm: std::env::var("COLORTERM").unwrap_or_default(),
        }
    }
}

/// Pure capability decision (DESIGN.md §8 degradation table).
///
/// Precedence: dumb → plain; not-a-TTY → plain; NO_COLOR → emphasis only;
/// else pick the richest color depth the terminal advertises.
pub fn detect_from(env: &EnvCaps, is_tty: bool, vt_ok: bool) -> Caps {
    // TERM=dumb: pure ASCII, no cursor control, no escapes.
    if env.term == "dumb" {
        return Caps::PLAIN;
    }
    // Piped / redirected: plain columns, no ANSI — script-safe by default.
    if !is_tty {
        return Caps::PLAIN;
    }
    // NO_COLOR: zero SGR color; bold/underline + layout carry meaning. But on a
    // legacy Windows console with no VT support, SGR/Unicode can't render at all,
    // so degrade to fully plain (ASCII, no escapes) rather than emitting `←[1m`
    // and mojibake. NO_COLOR must never *add* color, so we can't fall through to
    // the 16-color legacy branch below — we drop to plain here instead.
    if env.no_color {
        return if vt_ok {
            Caps {
                depth: ColorDepth::None,
                ansi: true,
                unicode: true,
            }
        } else {
            Caps::PLAIN
        };
    }

    // Windows legacy console with no VT support: fall back to 16-color + ASCII.
    if !vt_ok {
        return Caps {
            depth: ColorDepth::Ansi16,
            ansi: true,
            unicode: false,
        };
    }

    let ct = env.colorterm.to_ascii_lowercase();
    let depth = if ct.contains("truecolor") || ct.contains("24bit") {
        ColorDepth::Truecolor
    } else if env.term.contains("256color") {
        ColorDepth::Ansi256
    } else if env.term.is_empty() {
        // No hints at all but a TTY with VT (e.g. modern Windows Terminal sets
        // COLORTERM; a bare TTY is conservatively basic).
        ColorDepth::Ansi16
    } else {
        ColorDepth::Ansi16
    };
    Caps {
        depth,
        ansi: true,
        unicode: true,
    }
}

// --- Windows VT enabling -----------------------------------------------------

#[cfg(windows)]
fn enable_vt() -> bool {
    winvt::enable()
}

#[cfg(not(windows))]
fn enable_vt() -> bool {
    true
}

#[cfg(windows)]
mod winvt {
    use core::ffi::c_void;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetStdHandle(n: u32) -> *mut c_void;
        fn GetConsoleMode(h: *mut c_void, mode: *mut u32) -> i32;
        fn SetConsoleMode(h: *mut c_void, mode: u32) -> i32;
    }

    const STD_OUTPUT_HANDLE: u32 = 0xFFFF_FFF5; // (DWORD)-11
    const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;
    const INVALID_HANDLE: isize = -1;

    /// Try to turn on VT processing for stdout. Returns true if VT is (now) on,
    /// false on a legacy console that refuses it.
    pub fn enable() -> bool {
        unsafe {
            let h = GetStdHandle(STD_OUTPUT_HANDLE);
            if h.is_null() || h as isize == INVALID_HANDLE {
                return false;
            }
            let mut mode: u32 = 0;
            if GetConsoleMode(h, &mut mode) == 0 {
                // Not a real console (e.g. redirected) — caller's is_tty guards this.
                return false;
            }
            if mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING != 0 {
                return true;
            }
            SetConsoleMode(h, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) != 0
        }
    }
}

// ============================================================================
// Styles & the theme
// ============================================================================

/// A concrete, palette-resolved style for a role.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub fg: Option<Rgb>,
    pub bold: bool,
    pub dim: bool,
    pub underline: bool,
}

impl Style {
    pub fn fg(rgb: Rgb) -> Self {
        Style {
            fg: Some(rgb),
            ..Default::default()
        }
    }
    pub fn bold(mut self) -> Self {
        self.bold = true;
        self
    }
    // Part of the Style builder API; exercised by the degradation tests.
    #[allow(dead_code)]
    pub fn dim(mut self) -> Self {
        self.dim = true;
        self
    }
    #[allow(dead_code)]
    pub fn underline(mut self) -> Self {
        self.underline = true;
        self
    }

    /// Wrap `text` in the SGR escapes this style + capability level imply.
    /// - `ansi=false` → returns `text` untouched (zero escapes).
    /// - `depth=None` → color dropped, bold/underline kept (NO_COLOR).
    pub fn paint(&self, text: &str, caps: &Caps) -> String {
        if !caps.ansi {
            return text.to_string();
        }
        let mut codes: Vec<String> = Vec::new();
        if self.bold {
            codes.push("1".into());
        }
        if self.dim {
            codes.push("2".into());
        }
        if self.underline {
            codes.push("4".into());
        }
        if let Some(rgb) = self.fg {
            if let Some(code) = rgb.sgr_fg(caps.depth) {
                codes.push(code);
            }
        }
        if codes.is_empty() {
            return text.to_string();
        }
        format!("\x1b[{}m{}\x1b[0m", codes.join(";"), text)
    }
}

/// An unresolved role spec (fg may reference a palette anchor by name).
///
/// The emphasis flags are `Option`: `None` means "not mentioned" so it inherits
/// from the base role on merge, while `Some(false)` explicitly clears it. This is
/// what makes a user override *attribute*-partial — recoloring a role via `fg`
/// alone keeps the base role's bold/dim/underline (DESIGN.md §8 partial override).
#[derive(Clone, Debug, Default)]
struct StyleSpec {
    fg: Option<String>,
    bold: Option<bool>,
    dim: Option<bool>,
    underline: Option<bool>,
}

/// A named, resolved theme: palette anchors, role styles, and the urgency ramp.
#[derive(Clone, Debug)]
pub struct Theme {
    pub name: String,
    palette: BTreeMap<String, Rgb>,
    roles: BTreeMap<String, Style>,
    ramp: Vec<Rgb>,
}

impl Theme {
    /// Look up a role's style; unknown roles render as plain text.
    pub fn role(&self, name: &str) -> Style {
        self.roles.get(name).copied().unwrap_or_default()
    }

    /// Paint `text` with the named role at the given capability.
    pub fn paint(&self, role: &str, text: &str, caps: &Caps) -> String {
        self.role(role).paint(text, caps)
    }

    /// A named palette anchor (for the HTML palette export).
    pub fn palette_color(&self, name: &str) -> Option<Rgb> {
        self.palette.get(name).copied()
    }

    /// The urgency ramp anchors (cold → hot).
    pub fn ramp(&self) -> &[Rgb] {
        &self.ramp
    }

    /// Ramp color for a normalized position `t` in 0..=1 (cold→hot), interpolated
    /// across the anchors. None when the theme defines no ramp (e.g. `mono`).
    pub fn ramp_rgb(&self, t: f64) -> Option<Rgb> {
        match self.ramp.len() {
            0 => None,
            1 => Some(self.ramp[0]),
            n => {
                let t = t.clamp(0.0, 1.0);
                let span = (n - 1) as f64;
                let pos = t * span;
                let i = pos.floor() as usize;
                if i >= n - 1 {
                    Some(self.ramp[n - 1])
                } else {
                    Some(self.ramp[i].lerp(&self.ramp[i + 1], pos - i as f64))
                }
            }
        }
    }

    /// The urgency ramp as a `Style` for a normalized value (used in the table).
    pub fn ramp_style(&self, t: f64) -> Style {
        match self.ramp_rgb(t) {
            Some(rgb) => Style::fg(rgb),
            // No ramp (mono): let the hot end read as bold so meaning survives.
            None => {
                if t >= 0.66 {
                    Style::default().bold()
                } else {
                    Style::default()
                }
            }
        }
    }

    /// Names of the roles this theme defines (for `theme show`).
    pub fn role_names(&self) -> Vec<String> {
        self.roles.keys().cloned().collect()
    }
}

// ============================================================================
// Built-in themes
// ============================================================================

/// The canonical role set every built-in defines. Palette anchors: `bg`, `fg`,
/// `accent`, `warn`, `danger`, `muted` plus the role-specific hexes.
///
/// The `card.*` roles are deliberately the same achromatic grays in every
/// colored built-in (D76): the task card's design is "structure recedes into
/// gray, only what you act on is emphasized", and a card that turned blue under
/// nord would put the frame back in competition with the content. The grays are
/// mid-tone on purpose — readable on light and dark grounds alike — and
/// `card.strong` carries no color at all, so emphasis is always the terminal's
/// own strongest foreground. A user theme file can still override all three.
fn build(
    name: &str,
    palette: &[(&str, &str)],
    roles: &[(&str, StyleSpec)],
    ramp: &[&str],
) -> Theme {
    let palette: BTreeMap<String, Rgb> = palette
        .iter()
        .filter_map(|(k, v)| Rgb::parse_hex(v).map(|c| (k.to_string(), c)))
        .collect();
    let resolve = |fg: &Option<String>| -> Option<Rgb> {
        let s = fg.as_ref()?;
        if let Some(c) = Rgb::parse_hex(s) {
            Some(c)
        } else {
            palette.get(s).copied()
        }
    };
    let roles: BTreeMap<String, Style> = roles
        .iter()
        .map(|(k, spec)| {
            (
                k.to_string(),
                Style {
                    fg: resolve(&spec.fg),
                    bold: spec.bold.unwrap_or(false),
                    dim: spec.dim.unwrap_or(false),
                    underline: spec.underline.unwrap_or(false),
                },
            )
        })
        .collect();
    let ramp: Vec<Rgb> = ramp.iter().filter_map(|h| Rgb::parse_hex(h)).collect();
    Theme {
        name: name.to_string(),
        palette,
        roles,
        ramp,
    }
}

fn spec(fg: &str) -> StyleSpec {
    StyleSpec {
        fg: Some(fg.to_string()),
        ..Default::default()
    }
}
fn spec_b(fg: &str) -> StyleSpec {
    StyleSpec {
        fg: Some(fg.to_string()),
        bold: Some(true),
        ..Default::default()
    }
}
fn spec_d(fg: &str) -> StyleSpec {
    StyleSpec {
        fg: Some(fg.to_string()),
        dim: Some(true),
        ..Default::default()
    }
}

/// Return a built-in theme by name, or None.
pub fn builtin(name: &str) -> Option<Theme> {
    let t = match name {
        "nord" => build(
            "nord",
            &[
                ("bg", "#2e3440"),
                ("fg", "#d8dee9"),
                ("accent", "#88c0d0"),
                ("warn", "#ebcb8b"),
                ("danger", "#bf616a"),
                ("muted", "#4c566a"),
            ],
            &[
                ("header", spec_b("accent")),
                ("project", spec_d("#81a1c1")),
                ("tag", spec("#b48ead")),
                ("priority.H", spec_b("danger")),
                ("priority.M", spec("warn")),
                ("priority.L", spec_d("muted")),
                (
                    "overdue",
                    StyleSpec {
                        fg: Some("danger".into()),
                        bold: Some(true),
                        ..Default::default()
                    },
                ),
                ("timer.active", spec("#a3be8c")),
                ("muted", spec_d("muted")),
                ("danger", spec_b("danger")),
                ("warn", spec("warn")),
                ("accent", spec("accent")),
                ("card.frame", spec("#585858")),
                ("card.label", spec("#8a8a8a")),
                (
                    "card.strong",
                    StyleSpec {
                        bold: Some(true),
                        ..Default::default()
                    },
                ),
            ],
            &["#a3be8c", "#ebcb8b", "#bf616a"],
        ),
        "gruvbox" => build(
            "gruvbox",
            &[
                ("bg", "#282828"),
                ("fg", "#ebdbb2"),
                ("accent", "#83a598"),
                ("warn", "#fabd2f"),
                ("danger", "#fb4934"),
                ("muted", "#928374"),
            ],
            &[
                ("header", spec_b("accent")),
                ("project", spec_d("#83a598")),
                ("tag", spec("#d3869b")),
                ("priority.H", spec_b("danger")),
                ("priority.M", spec("warn")),
                ("priority.L", spec_d("muted")),
                (
                    "overdue",
                    StyleSpec {
                        fg: Some("danger".into()),
                        bold: Some(true),
                        ..Default::default()
                    },
                ),
                ("timer.active", spec("#b8bb26")),
                ("muted", spec_d("muted")),
                ("danger", spec_b("danger")),
                ("warn", spec("warn")),
                ("accent", spec("accent")),
                ("card.frame", spec("#585858")),
                ("card.label", spec("#8a8a8a")),
                (
                    "card.strong",
                    StyleSpec {
                        bold: Some(true),
                        ..Default::default()
                    },
                ),
            ],
            &["#b8bb26", "#fabd2f", "#fb4934"],
        ),
        "dracula" => build(
            "dracula",
            &[
                ("bg", "#282a36"),
                ("fg", "#f8f8f2"),
                ("accent", "#8be9fd"),
                ("warn", "#f1fa8c"),
                ("danger", "#ff5555"),
                ("muted", "#6272a4"),
            ],
            &[
                ("header", spec_b("accent")),
                ("project", spec_d("#bd93f9")),
                ("tag", spec("#ff79c6")),
                ("priority.H", spec_b("danger")),
                ("priority.M", spec("warn")),
                ("priority.L", spec_d("muted")),
                (
                    "overdue",
                    StyleSpec {
                        fg: Some("danger".into()),
                        bold: Some(true),
                        ..Default::default()
                    },
                ),
                ("timer.active", spec("#50fa7b")),
                ("muted", spec_d("muted")),
                ("danger", spec_b("danger")),
                ("warn", spec("warn")),
                ("accent", spec("accent")),
                ("card.frame", spec("#585858")),
                ("card.label", spec("#8a8a8a")),
                (
                    "card.strong",
                    StyleSpec {
                        bold: Some(true),
                        ..Default::default()
                    },
                ),
            ],
            &["#50fa7b", "#f1fa8c", "#ff5555"],
        ),
        "solarized" => build(
            "solarized",
            &[
                ("bg", "#002b36"),
                ("fg", "#839496"),
                ("accent", "#268bd2"),
                ("warn", "#b58900"),
                ("danger", "#dc322f"),
                ("muted", "#586e75"),
            ],
            &[
                ("header", spec_b("accent")),
                ("project", spec_d("#268bd2")),
                ("tag", spec("#6c71c4")),
                ("priority.H", spec_b("danger")),
                ("priority.M", spec("warn")),
                ("priority.L", spec_d("muted")),
                (
                    "overdue",
                    StyleSpec {
                        fg: Some("danger".into()),
                        bold: Some(true),
                        ..Default::default()
                    },
                ),
                ("timer.active", spec("#859900")),
                ("muted", spec_d("muted")),
                ("danger", spec_b("danger")),
                ("warn", spec("warn")),
                ("accent", spec("accent")),
                ("card.frame", spec("#585858")),
                ("card.label", spec("#8a8a8a")),
                (
                    "card.strong",
                    StyleSpec {
                        bold: Some(true),
                        ..Default::default()
                    },
                ),
            ],
            &["#859900", "#b58900", "#dc322f"],
        ),
        // mono: no color anywhere — meaning is carried by bold/dim/underline
        // only, so it is correct even on a NO_COLOR or 16-color terminal.
        "mono" => build(
            "mono",
            &[
                ("bg", "#000000"),
                ("fg", "#ffffff"),
                ("accent", "#ffffff"),
                ("warn", "#ffffff"),
                ("danger", "#ffffff"),
                ("muted", "#808080"),
            ],
            &[
                (
                    "header",
                    StyleSpec {
                        bold: Some(true),
                        ..Default::default()
                    },
                ),
                (
                    "project",
                    StyleSpec {
                        dim: Some(true),
                        ..Default::default()
                    },
                ),
                ("tag", StyleSpec::default()),
                (
                    "priority.H",
                    StyleSpec {
                        bold: Some(true),
                        ..Default::default()
                    },
                ),
                ("priority.M", StyleSpec::default()),
                (
                    "priority.L",
                    StyleSpec {
                        dim: Some(true),
                        ..Default::default()
                    },
                ),
                (
                    "overdue",
                    StyleSpec {
                        bold: Some(true),
                        underline: Some(true),
                        ..Default::default()
                    },
                ),
                (
                    "timer.active",
                    StyleSpec {
                        bold: Some(true),
                        ..Default::default()
                    },
                ),
                (
                    "muted",
                    StyleSpec {
                        dim: Some(true),
                        ..Default::default()
                    },
                ),
                (
                    "danger",
                    StyleSpec {
                        bold: Some(true),
                        ..Default::default()
                    },
                ),
                ("warn", StyleSpec::default()),
                (
                    "accent",
                    StyleSpec {
                        bold: Some(true),
                        ..Default::default()
                    },
                ),
                (
                    "card.frame",
                    StyleSpec {
                        dim: Some(true),
                        ..Default::default()
                    },
                ),
                (
                    "card.label",
                    StyleSpec {
                        dim: Some(true),
                        ..Default::default()
                    },
                ),
                (
                    "card.strong",
                    StyleSpec {
                        bold: Some(true),
                        ..Default::default()
                    },
                ),
            ],
            &[],
        ),
        _ => return None,
    };
    Some(t)
}

/// Every built-in theme name, in a stable order (for `theme list`).
pub const BUILTINS: [&str; 5] = ["nord", "gruvbox", "dracula", "solarized", "mono"];

/// The default when nothing selects a theme.
pub const DEFAULT_THEME: &str = "nord";

// ============================================================================
// Resolution + user-file loading
// ============================================================================

/// Resolve which theme *name* wins, by DESIGN.md §8 precedence:
/// `--theme` flag → `$TASQX_THEME` → `[theme] name` in config → default (nord).
pub fn resolve_name(flag: Option<&str>, env: Option<&str>, config: Option<&str>) -> String {
    fn pick(v: Option<&str>) -> Option<&str> {
        v.map(str::trim).filter(|s| !s.is_empty())
    }
    pick(flag)
        .or_else(|| pick(env))
        .or_else(|| pick(config))
        .unwrap_or(DEFAULT_THEME)
        .to_string()
}

/// A parsed user theme file (before merging onto its base).
#[derive(Clone, Debug, Default)]
pub struct UserTheme {
    pub name: Option<String>,
    pub extends: Option<String>,
    pub palette: BTreeMap<String, Rgb>,
    roles: BTreeMap<String, StyleSpec>,
    ramp: Option<Vec<String>>,
    /// Values the parser had to throw away, as ready-to-print fragments (no file
    /// path — `load_reporting` prefixes that). Recorded here because this is the
    /// last point that still holds the raw text that failed to parse; by the time
    /// the overlay reaches `merge`, a mistyped `#gggggg` is indistinguishable
    /// from an anchor the user never wrote.
    pub dropped: Vec<String>,
}

/// Parse a user theme TOML string into an overlay (DESIGN.md §8 shape). Dotted
/// role keys (`priority.H`, `urgency.ramp`) arrive nested under `[roles]`; we
/// flatten them back to literal role names.
pub fn parse_user_theme(src: &str) -> Result<UserTheme, String> {
    let val: toml::Table = src
        .parse()
        .map_err(|e| format!("invalid theme TOML: {e}"))?;
    let mut ut = UserTheme {
        name: val.get("name").and_then(|v| v.as_str()).map(str::to_string),
        extends: val
            .get("extends")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        ..Default::default()
    };

    if let Some(pal) = val.get("palette").and_then(|v| v.as_table()) {
        for (k, v) in pal {
            match v.as_str().and_then(Rgb::parse_hex) {
                Some(hex) => {
                    ut.palette.insert(k.clone(), hex);
                }
                // A `filter_map` here left `#gggggg` as a parse-clean file whose
                // color simply never applies — the quietest failure in the whole
                // theme path, with nothing on stderr to connect the missing color
                // to the typo that caused it.
                None => ut
                    .dropped
                    .push(format!("palette.{k} = {v} is not #rrggbb; ignored")),
            }
        }
    }

    if let Some(roles) = val.get("roles").and_then(|v| v.as_table()) {
        flatten_roles("", roles, &mut ut);
    }

    Ok(ut)
}

/// Recursively flatten a `[roles]` table into literal `a.b` role names,
/// separating style tables from the `urgency.ramp` array.
fn flatten_roles(prefix: &str, table: &toml::Table, ut: &mut UserTheme) {
    for (k, v) in table {
        let name = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        match v {
            toml::Value::Array(arr) => {
                let ramp: Vec<String> = arr
                    .iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect();
                // Any array role is treated as the (single) urgency ramp.
                ut.ramp = Some(ramp);
            }
            toml::Value::Table(t) => {
                if is_style_table(t) {
                    ut.roles.insert(name, style_from_table(t));
                } else {
                    flatten_roles(&name, t, ut);
                }
            }
            _ => {}
        }
    }
}

/// A style table holds only scalar style keys; a namespace table holds sub-tables.
fn is_style_table(t: &toml::Table) -> bool {
    t.values()
        .all(|v| v.is_str() || v.as_bool().is_some() || v.is_integer())
        && t.keys()
            .any(|k| matches!(k.as_str(), "fg" | "bold" | "dim" | "underline"))
}

fn style_from_table(t: &toml::Table) -> StyleSpec {
    // `None` for an absent key so merge() inherits that attribute from the base
    // role; only keys the user actually wrote override.
    StyleSpec {
        fg: t.get("fg").and_then(|v| v.as_str()).map(str::to_string),
        bold: t.get("bold").and_then(|v| v.as_bool()),
        dim: t.get("dim").and_then(|v| v.as_bool()),
        underline: t.get("underline").and_then(|v| v.as_bool()),
    }
}

/// Merge a `UserTheme` overlay onto a base theme: only keys present in the
/// overlay override; everything else falls through to `base` (partial override).
pub fn merge(base: &Theme, user: &UserTheme) -> Theme {
    let mut palette = base.palette.clone();
    for (k, v) in &user.palette {
        palette.insert(k.clone(), *v);
    }

    // Resolve an overlay role spec against the *merged* palette.
    let resolve = |fg: &Option<String>| -> Option<Rgb> {
        let s = fg.as_ref()?;
        if let Some(c) = Rgb::parse_hex(s) {
            Some(c)
        } else {
            palette.get(s).copied()
        }
    };

    // Attribute-partial override: start from the base role's resolved style and
    // apply only the attributes the overlay actually specifies. A color-only
    // override (`fg` alone) therefore keeps the base role's bold/dim/underline,
    // which matters most under NO_COLOR where emphasis is the only signal left.
    let mut roles = base.roles.clone();
    for (k, spec) in &user.roles {
        let mut style = roles.get(k).copied().unwrap_or_default();
        if spec.fg.is_some() {
            style.fg = resolve(&spec.fg);
        }
        if let Some(b) = spec.bold {
            style.bold = b;
        }
        if let Some(d) = spec.dim {
            style.dim = d;
        }
        if let Some(u) = spec.underline {
            style.underline = u;
        }
        roles.insert(k.clone(), style);
    }

    let ramp = match &user.ramp {
        Some(list) => list.iter().filter_map(|h| Rgb::parse_hex(h)).collect(),
        None => base.ramp.clone(),
    };

    Theme {
        name: user.name.clone().unwrap_or_else(|| base.name.clone()),
        palette,
        roles,
        ramp,
    }
}

/// What the user's `<name>.toml` contributed to a load, and what it cost.
///
/// The two call paths need different answers to the same load, which is why the
/// severity is in the type rather than in a bare string: the render path warns
/// and renders anyway (a broken theme must never fail a task capture), while
/// `theme show` must refuse a `Rejected` outright — printing nord when the user
/// asked for `mine` is exactly the failure
/// `theme_show_rejects_an_unknown_name` was written to stop, reached through a
/// file instead of a name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileOutcome {
    /// No `<name>.toml` (or no themes dir at all): a built-in, or the default.
    /// The overwhelmingly common case — it must stay silent, or every run of
    /// every built-in theme prints a warning.
    Untouched,
    /// The file was read and merged. Each string names a piece of it that had to
    /// be dropped anyway: an `extends` naming a non-built-in, a hex value that
    /// does not parse. Empty means a clean file.
    Merged(Vec<String>),
    /// The file exists but yielded no theme at all — unreadable, or invalid
    /// TOML. The theme handed back is a fallback the user never asked for.
    Rejected(String),
}

// Both readers live in `lib.rs`, which this change does not touch: the render
// path (`build_ctx`) warns and continues, `theme show` refuses a rejection the
// way it already refuses an unknown *name*. Until those two lines land, nothing
// outside the tests reads either method, and `mod theme` is private so
// `pub` alone does not keep dead_code quiet. Drop these two attributes when the
// call sites are wired up. Better still, make them `#[expect(dead_code)]`:
// `lint_reasons` is stable since 1.81, well under the 1.95 workspace floor
// (Cargo.toml), so the loud option IS available here — an `expect` that stops
// firing becomes a warning, and `-D warnings` turns that into the reminder,
// instead of this paragraph having to be found and remembered.
#[allow(dead_code)]
impl FileOutcome {
    /// The complaint that means "this is not the theme you asked for". `None`
    /// for `Merged`, however many pieces it dropped: the user's name, palette
    /// and roles did apply, so `theme show` is still showing their theme.
    pub fn rejection(&self) -> Option<&str> {
        match self {
            FileOutcome::Rejected(m) => Some(m),
            FileOutcome::Untouched | FileOutcome::Merged(_) => None,
        }
    }

    /// Every complaint, in file order — what the render path prints, which does
    /// not care about the severity because it continues either way.
    pub fn messages(&self) -> &[String] {
        match self {
            FileOutcome::Untouched => &[],
            FileOutcome::Merged(v) => v,
            // One rejection is still one message; `from_ref` avoids storing the
            // single-element case as a Vec that could then hold two.
            FileOutcome::Rejected(m) => std::slice::from_ref(m),
        }
    }
}

/// A theme plus what loading it had to say. `load` throws the second half away.
pub struct Loaded {
    pub theme: Theme,
    #[allow(dead_code)] // see FileOutcome above: read by lib.rs once wired up.
    pub file: FileOutcome,
}

/// Load a theme by name: a user file `<themes_dir>/<name>.toml` wins (extending
/// its `extends` base or `nord`); otherwise a built-in; otherwise the default.
///
/// This is `load` with the diagnostics kept instead of discarded. Every failure
/// here used to be an `if let Ok` or a `filter_map` that fell through to nord at
/// exit 0 with an empty stderr: a mistyped bracket in `themes/mine.toml`
/// rendered every subsequent command in nord and never said why, even though
/// `parse_user_theme` had already formatted the line and column.
pub fn load_reporting(name: &str, themes_dir: Option<&std::path::Path>) -> Loaded {
    // The fallback used by every early return below: the built-in of the same
    // name if there is one, else the default. Unchanged from before.
    let fallback = || Loaded {
        theme: builtin(name).unwrap_or_else(default_theme),
        file: FileOutcome::Untouched,
    };
    let Some(dir) = themes_dir else {
        return fallback();
    };
    let path = dir.join(format!("{name}.toml"));
    // `at` prefixes every message: a user with several theme files needs to know
    // WHICH one is being skipped, and the path is the only thing that says so.
    let at = format!("theme file {}", path.display());

    let src = match std::fs::read_to_string(&path) {
        Ok(src) => src,
        // "No such file" is how a built-in name resolves, not a problem.
        // Anything else — a directory, a permission bit, a bad symlink — is a
        // file the user meant to be read and that they will never see used.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return fallback(),
        Err(e) => {
            return Loaded {
                theme: builtin(name).unwrap_or_else(default_theme),
                file: FileOutcome::Rejected(format!("{at}: {e}")),
            };
        }
    };

    let user = match parse_user_theme(&src) {
        Ok(user) => user,
        // The toml error carries the line and column; that string was built and
        // then dropped on the floor by the old `if let Ok`.
        Err(e) => {
            return Loaded {
                theme: builtin(name).unwrap_or_else(default_theme),
                file: FileOutcome::Rejected(format!("{at}: {e}")),
            };
        }
    };

    // Everything below is a file that DID load, with pieces of it dropped.
    let mut dropped: Vec<String> = user
        .dropped
        .iter()
        .map(|what| format!("{at}: {what}"))
        .collect();

    let base_name = user
        .extends
        .clone()
        .unwrap_or_else(|| DEFAULT_THEME.to_string());
    let base = match builtin(&base_name) {
        Some(base) => base,
        None => {
            // Say "built-in", not just "unknown": `builtin()` is the only lookup
            // here, so `extends = "myothertheme"` naming another *user* file is
            // dropped exactly the same way, and a message that merely listed the
            // known names would leave that user re-reading their own file.
            dropped.push(format!(
                "{at}: extends {base_name:?}, which is not a built-in theme (extends must name one of: {}); extended {DEFAULT_THEME} instead",
                BUILTINS.join(", ")
            ));
            default_theme()
        }
    };
    let mut merged = merge(&base, &user);

    // `merge` drops unparsable ramp entries and unresolvable role colors through
    // a filter_map and an `Option`, and neither keeps the raw string, so the
    // complaints are reconstructed here from the overlay against the *merged*
    // palette — the same palette `merge` resolved against, so this cannot
    // disagree with what actually happened.
    if let Some(list) = &user.ramp {
        for h in list.iter().filter(|h| Rgb::parse_hex(h).is_none()) {
            dropped.push(format!(
                "{at}: urgency.ramp entry {h:?} is not #rrggbb; ignored"
            ));
        }
    }
    for (role, spec) in &user.roles {
        let Some(fg) = spec.fg.as_deref() else {
            continue;
        };
        if Rgb::parse_hex(fg).is_none() && !merged.palette.contains_key(fg) {
            // Note this is worse than "ignored": `merge` writes the unresolved
            // `None` over the base role's color, so the role ends up with no
            // color at all rather than the one it inherited.
            dropped.push(format!(
                "{at}: roles.{role} fg {fg:?} is neither #rrggbb nor a [palette] key; {role} rendered without color"
            ));
        }
    }

    if user.name.is_none() {
        merged.name = name.to_string();
    }
    Loaded {
        theme: merged,
        file: FileOutcome::Merged(dropped),
    }
}

/// Load a theme by name, discarding what loading it had to say.
///
/// Kept because five of the eight call sites pass `themes_dir = None` and so can
/// never produce a diagnostic, and a sixth renders a live preview inside the
/// TUI's alt screen where an `eprintln!` would corrupt the frame. Only the two
/// call sites that can actually reach a user — the render path and `theme show`
/// — need `load_reporting`.
pub fn load(name: &str, themes_dir: Option<&std::path::Path>) -> Theme {
    load_reporting(name, themes_dir).theme
}

/// The compiled-in default theme (nord), always available with zero files.
pub fn default_theme() -> Theme {
    builtin(DEFAULT_THEME).expect("nord is a built-in")
}

// ============================================================================
// Render context
// ============================================================================

/// The bundle every render function receives: the active theme + the detected
/// terminal capability + how wide the surface is. One lookup surface, so no
/// command hard-codes a color — or a column width.
pub struct Ctx {
    pub theme: Theme,
    pub caps: Caps,
    /// How many CELLS wide a table may lay itself out over. Not a capability
    /// (it changes when the user drags a window edge, and `Caps` is `Copy` state
    /// several tests build by literal), so it rides on the context instead.
    pub cols: usize,
}

impl Ctx {
    /// The width to lay out for when the real one is unknowable — piped,
    /// redirected, or a terminal that will not answer. Deliberately a constant
    /// rather than "unbounded": a pipe has no width, and a table that grows to
    /// its content there would be a different shape for every store, which is
    /// exactly what a script diffing two runs must not see.
    pub const DEFAULT_COLS: usize = 100;
    /// Narrower than this and the table has no room for its own headers, so the
    /// minimum column budgets take over and rows are allowed to overflow.
    pub const MIN_COLS: usize = 40;
    /// An ultrawide terminal is not an invitation to draw a 300-cell row; past
    /// this the eye loses the line it is reading (see `typography`).
    pub const MAX_COLS: usize = 160;

    pub fn new(theme: Theme, caps: Caps) -> Self {
        Ctx {
            theme,
            caps,
            cols: Self::DEFAULT_COLS,
        }
    }

    /// Lay out for `cols` cells, clamped to the range a table can actually use.
    pub fn with_cols(mut self, cols: usize) -> Self {
        self.cols = cols.clamp(Self::MIN_COLS, Self::MAX_COLS);
        self
    }

    pub fn paint(&self, role: &str, text: &str) -> String {
        self.theme.paint(role, text, &self.caps)
    }

    /// A horizontal rule glyph run, Unicode when supported else ASCII.
    pub fn hrule(&self, len: usize) -> String {
        let ch = if self.caps.unicode { '─' } else { '-' };
        ch.to_string().repeat(len)
    }

    /// Leading marker for footer hints (`▸` / `>`).
    pub fn arrow(&self) -> &'static str {
        if self.caps.unicode {
            "▸"
        } else {
            ">"
        }
    }

    /// Inline separator (`·` / `-`), ASCII on dumb/legacy terminals.
    pub fn mid(&self) -> &'static str {
        if self.caps.unicode {
            "·"
        } else {
            "-"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- capability degradation --------------------------------------------

    fn env(no_color: bool, term: &str, colorterm: &str) -> EnvCaps {
        EnvCaps {
            no_color,
            term: term.into(),
            colorterm: colorterm.into(),
        }
    }

    #[test]
    fn truecolor_detected() {
        let c = detect_from(&env(false, "xterm-256color", "truecolor"), true, true);
        assert_eq!(c.depth, ColorDepth::Truecolor);
        assert!(c.ansi && c.unicode);
    }

    #[test]
    fn ansi256_detected() {
        let c = detect_from(&env(false, "xterm-256color", ""), true, true);
        assert_eq!(c.depth, ColorDepth::Ansi256);
    }

    #[test]
    fn basic16_detected() {
        let c = detect_from(&env(false, "xterm", ""), true, true);
        assert_eq!(c.depth, ColorDepth::Ansi16);
    }

    #[test]
    fn no_color_is_emphasis_only() {
        let c = detect_from(&env(true, "xterm-256color", "truecolor"), true, true);
        assert_eq!(c.depth, ColorDepth::None);
        assert!(c.ansi, "NO_COLOR keeps bold/underline");
    }

    #[test]
    fn no_color_on_legacy_no_vt_is_plain() {
        // NO_COLOR on a pre-VT Windows console must not emit SGR/Unicode it can't
        // render — it degrades to fully plain rather than color or `←[1m` litter.
        let c = detect_from(&env(true, "", ""), true, false);
        assert_eq!(c, Caps::PLAIN);
        assert!(!c.ansi && !c.unicode);
    }

    #[test]
    fn parse_hex_rejects_non_ascii_six_bytes() {
        // "1€45" is exactly 6 bytes but not 6 ASCII chars — must not panic.
        assert_eq!(Rgb::parse_hex("1€45"), None);
        assert_eq!(Rgb::parse_hex("#88c0d0"), Some(Rgb::new(0x88, 0xc0, 0xd0)));
    }

    #[test]
    fn piped_is_plain() {
        let c = detect_from(&env(false, "xterm-256color", "truecolor"), false, true);
        assert_eq!(c, Caps::PLAIN);
        assert!(!c.ansi && !c.unicode);
    }

    #[test]
    fn dumb_is_plain() {
        let c = detect_from(&env(false, "dumb", ""), true, true);
        assert_eq!(c, Caps::PLAIN);
    }

    #[test]
    fn windows_legacy_no_vt_is_ansi16_ascii() {
        let c = detect_from(&env(false, "", ""), true, false);
        assert_eq!(c.depth, ColorDepth::Ansi16);
        assert!(c.ansi);
        assert!(!c.unicode, "legacy console => ASCII box chars");
    }

    // ---- style rendering at each depth -------------------------------------

    fn caps(depth: ColorDepth, ansi: bool) -> Caps {
        Caps {
            depth,
            ansi,
            unicode: true,
        }
    }

    #[test]
    fn truecolor_emits_24bit_escape() {
        let s = Style::fg(Rgb::new(0x88, 0xc0, 0xd0));
        let out = s.paint("x", &caps(ColorDepth::Truecolor, true));
        assert!(out.contains("\x1b[38;2;136;192;208m"), "{out:?}");
    }

    #[test]
    fn ansi256_emits_indexed_escape() {
        let s = Style::fg(Rgb::new(0x88, 0xc0, 0xd0));
        let out = s.paint("x", &caps(ColorDepth::Ansi256, true));
        assert!(out.contains("\x1b[38;5;"), "{out:?}");
        assert!(!out.contains("38;2;"));
    }

    #[test]
    fn ansi16_emits_basic_escape() {
        let s = Style::fg(Rgb::new(0xbf, 0x61, 0x6a));
        let out = s.paint("x", &caps(ColorDepth::Ansi16, true));
        // basic fg is 3x or 9x, never a 38;2 / 38;5 color.
        assert!(!out.contains("38;2;") && !out.contains("38;5;"), "{out:?}");
        assert!(out.starts_with("\x1b["), "{out:?}");
    }

    #[test]
    fn no_color_emits_zero_color_but_keeps_bold() {
        let s = Style::fg(Rgb::new(0xbf, 0x61, 0x6a)).bold();
        let out = s.paint("x", &caps(ColorDepth::None, true));
        assert!(
            !out.contains("38;2;") && !out.contains("38;5;"),
            "no color: {out:?}"
        );
        assert!(out.contains("\x1b[1m"), "bold kept: {out:?}");
    }

    #[test]
    fn plain_emits_zero_ansi() {
        let s = Style::fg(Rgb::new(0xbf, 0x61, 0x6a)).bold().underline();
        let out = s.paint("hello", &Caps::PLAIN);
        assert_eq!(out, "hello");
        assert!(!out.contains('\x1b'));
    }

    #[test]
    fn hrule_degrades_to_ascii() {
        let uni = Ctx::new(default_theme(), caps(ColorDepth::Truecolor, true));
        assert_eq!(uni.hrule(3), "───");
        let ascii = Ctx::new(default_theme(), Caps::PLAIN);
        assert_eq!(ascii.hrule(3), "---");
    }

    // ---- theme resolution + precedence -------------------------------------

    #[test]
    fn precedence_flag_beats_all() {
        let n = resolve_name(Some("dracula"), Some("gruvbox"), Some("mono"));
        assert_eq!(n, "dracula");
    }

    #[test]
    fn precedence_env_beats_config() {
        let n = resolve_name(None, Some("gruvbox"), Some("mono"));
        assert_eq!(n, "gruvbox");
    }

    #[test]
    fn precedence_config_beats_default() {
        let n = resolve_name(None, None, Some("solarized"));
        assert_eq!(n, "solarized");
    }

    #[test]
    fn precedence_empty_falls_to_default() {
        let n = resolve_name(Some(""), Some(""), None);
        assert_eq!(n, "nord");
    }

    #[test]
    fn all_builtins_load() {
        for name in BUILTINS {
            let t = builtin(name).unwrap_or_else(|| panic!("missing {name}"));
            assert_eq!(t.name, name);
            // Every built-in defines the core roles.
            for role in [
                "header",
                "overdue",
                "priority.H",
                "project",
                "tag",
                "card.frame",
                "card.label",
                "card.strong",
            ] {
                assert!(
                    t.role_names().iter().any(|r| r == role),
                    "{name} missing {role}"
                );
            }
        }
    }

    // ---- 'extends' partial override -----------------------------------------

    #[test]
    fn extends_partial_override_merges() {
        let src = r##"
name    = "mine"
extends = "nord"

[palette]
danger = "#ff0000"

[roles]
tag        = { fg = "#123456" }
priority.H = { fg = "danger", bold = true }
"##;
        let user = parse_user_theme(src).expect("parse");
        assert_eq!(user.extends.as_deref(), Some("nord"));
        let base = builtin("nord").unwrap();
        let merged = merge(&base, &user);

        // Overridden: tag fg is the user hex.
        assert_eq!(merged.role("tag").fg, Some(Rgb::new(0x12, 0x34, 0x56)));
        // Overridden palette anchor flows into priority.H's "danger" reference.
        assert_eq!(
            merged.role("priority.H").fg,
            Some(Rgb::new(0xff, 0x00, 0x00))
        );
        assert!(merged.role("priority.H").bold);
        // NOT overridden: header falls through to nord's accent (bold).
        assert_eq!(merged.role("header"), base.role("header"));
        assert!(merged.role("header").bold);
        // NOT overridden: ramp inherited from base.
        assert_eq!(merged.ramp().len(), base.ramp().len());
    }

    #[test]
    fn color_only_override_keeps_base_emphasis() {
        // Recoloring an emphasized role via `fg` alone must preserve the base
        // role's bold (attribute-partial merge), not silently de-emphasize it.
        let src = r##"
extends = "nord"
[roles]
header = { fg = "#abcdef" }
"##;
        let user = parse_user_theme(src).expect("parse");
        let base = builtin("nord").unwrap();
        let merged = merge(&base, &user);
        assert_eq!(merged.role("header").fg, Some(Rgb::new(0xab, 0xcd, 0xef)));
        assert!(
            merged.role("header").bold,
            "base bold survives a color-only override"
        );
        // An explicit `bold = false` still clears it.
        let src2 = r##"
extends = "nord"
[roles]
header = { bold = false }
"##;
        let user2 = parse_user_theme(src2).expect("parse");
        let merged2 = merge(&base, &user2);
        assert!(
            !merged2.role("header").bold,
            "explicit bold=false clears emphasis"
        );
        // fg untouched by the emphasis-only override.
        assert_eq!(merged2.role("header").fg, base.role("header").fg);
    }

    #[test]
    fn extends_ramp_override() {
        let src = r##"
extends = "nord"
[roles]
urgency.ramp = ["#000000", "#ffffff"]
"##;
        let user = parse_user_theme(src).unwrap();
        let merged = merge(&builtin("nord").unwrap(), &user);
        assert_eq!(merged.ramp(), &[Rgb::new(0, 0, 0), Rgb::new(255, 255, 255)]);
    }

    #[test]
    fn ramp_interpolates_cold_to_hot() {
        let t = builtin("nord").unwrap();
        assert_eq!(t.ramp_rgb(0.0), Some(Rgb::new(0xa3, 0xbe, 0x8c)));
        assert_eq!(t.ramp_rgb(1.0), Some(Rgb::new(0xbf, 0x61, 0x6a)));
        // mid is between anchors, not equal to either end
        let mid = t.ramp_rgb(0.5).unwrap();
        assert_ne!(mid, t.ramp_rgb(0.0).unwrap());
    }

    #[test]
    fn mono_has_no_ramp_color() {
        let t = builtin("mono").unwrap();
        assert_eq!(t.ramp_rgb(0.5), None);
        // hot end still readable via bold under mono
        assert!(t.ramp_style(0.9).bold);
    }

    // ---- user-file diagnostics ----------------------------------------------
    //
    // Every case below used to fall through to nord in silence: the `if let Ok`
    // pair in `load` threw away both the io error and the toml error (which
    // already carries the line and column the user needs), `builtin(&base_name)`
    // turned an unknown `extends` into the default, and both hex parsers are
    // `filter_map`s. A mistyped bracket in `~/.config/tasqx/themes/mine.toml`
    // therefore rendered every subsequent `tasqx list` in nord with nothing on
    // stderr and exit 0.

    /// A scratch themes directory, unique per test so the suite's threads cannot
    /// collide, and never the user's real `~/.config/tasqx/themes`.
    fn scratch_themes_dir(tag: &str) -> std::path::PathBuf {
        let mut d = std::env::temp_dir();
        d.push(format!("tasqx-theme-diag-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("create scratch themes dir");
        d
    }

    fn write_theme(dir: &std::path::Path, name: &str, src: &str) {
        std::fs::write(dir.join(format!("{name}.toml")), src).expect("write theme file");
    }

    #[test]
    fn missing_user_file_reports_nothing() {
        // The common case: every built-in name resolves with no file on disk, so
        // a NotFound must stay silent or the warning fires on every single run.
        let dir = scratch_themes_dir("absent");
        let loaded = load_reporting("gruvbox", Some(&dir));
        assert_eq!(loaded.theme.name, "gruvbox");
        assert_eq!(loaded.file, FileOutcome::Untouched);
    }

    /// The property is that the io error REACHES the user, so they learn why the
    /// file could not be read — not that the operating system phrases it any
    /// particular way.
    ///
    /// # Why this asks the platform instead of naming the message
    ///
    /// The assertion used to be `msg.contains("directory")`, which is Linux's
    /// phrasing of this failure ("Is a directory (os error 21)") and is not
    /// Windows's ("Access is denied. (os error 5)"). The technique below — a
    /// DIRECTORY where a file belongs, so the read fails without depending on
    /// chmod, which root ignores — is genuinely portable; only the assertion on
    /// top of it was not, and it turned `test (windows-latest)` red from
    /// 2026-07-26 until it was fixed. A permanently red platform job is worse
    /// than a missing one: it trains everyone to stop reading the only signal
    /// that would catch a real Windows regression.
    ///
    /// So the expected text is obtained by performing the SAME read here and
    /// asking the resulting `io::Error` what it says. That is stronger than the
    /// old spelling as well as portable: it pins the exact error the user is
    /// shown rather than one word that happened to appear in it.
    #[test]
    fn unreadable_user_file_is_rejected_with_the_io_error() {
        let dir = scratch_themes_dir("unreadable");
        let path = dir.join("mine.toml");
        std::fs::create_dir_all(&path).expect("mkdir mine.toml");

        // What this platform says about reading a directory as a file. Taken
        // from the same call `load_reporting` makes, so the two cannot disagree.
        let expected = std::fs::read_to_string(&path)
            .expect_err("reading a directory as a file must fail on every platform")
            .to_string();

        let loaded = load_reporting("mine", Some(&dir));
        let msg = loaded
            .file
            .rejection()
            .expect("an unreadable theme file must be rejected, not swallowed");
        assert!(msg.contains("mine.toml"), "must name the file: {msg}");
        assert!(
            msg.contains(&expected),
            "must carry the io error this platform reports ({expected:?}), so the \
             user learns why it could not be read: {msg}"
        );
        assert_eq!(loaded.theme.name, DEFAULT_THEME, "falls back, as before");
    }

    #[test]
    fn malformed_toml_is_rejected_with_line_and_column() {
        let dir = scratch_themes_dir("malformed");
        write_theme(&dir, "mine", "[palette\ndanger = \"#ff0000\"\n");
        let loaded = load_reporting("mine", Some(&dir));
        let msg = loaded
            .file
            .rejection()
            .expect("invalid TOML must be rejected, not swallowed");
        assert!(msg.contains("mine.toml"), "must name the file: {msg}");
        assert!(
            msg.contains("invalid theme TOML"),
            "must carry the parser's own complaint: {msg}"
        );
        // parse_user_theme already formats the position; it was being dropped at
        // the `if let Ok`. Line 1 is where the unclosed bracket is.
        assert!(
            msg.contains("line 1"),
            "must carry the position the parser knows: {msg}"
        );
    }

    #[test]
    fn unknown_extends_is_reported_but_the_file_still_applies() {
        let dir = scratch_themes_dir("extends");
        write_theme(
            &dir,
            "mine",
            "extends = \"nosuchbase\"\n[roles]\ntag = { fg = \"#123456\" }\n",
        );
        let loaded = load_reporting("mine", Some(&dir));
        assert!(
            loaded.file.rejection().is_none(),
            "the user's own overrides still applied, so this is not a rejection"
        );
        let msgs = loaded.file.messages();
        assert_eq!(msgs.len(), 1, "one complaint, about extends: {msgs:?}");
        assert!(msgs[0].contains("nosuchbase"), "names the value: {msgs:?}");
        // `builtin()` knows built-ins ONLY, so `extends = "myothertheme"` naming
        // another user file is dropped too — the message has to say so.
        assert!(
            msgs[0].contains("built-in"),
            "must say the base has to be a built-in: {msgs:?}"
        );
        assert_eq!(
            loaded.theme.role("tag").fg,
            Some(Rgb::new(0x12, 0x34, 0x56)),
            "the rest of the file is still the user's"
        );
    }

    #[test]
    fn unparsable_palette_hex_is_reported() {
        // Parse-clean file, color simply never applies: the quietest case of all.
        let dir = scratch_themes_dir("badhex");
        write_theme(
            &dir,
            "mine",
            "extends = \"nord\"\n[palette]\ndanger = \"#gggggg\"\n",
        );
        let msgs = load_reporting("mine", Some(&dir)).file.messages().to_vec();
        assert_eq!(msgs.len(), 1, "expected one complaint: {msgs:?}");
        assert!(msgs[0].contains("danger"), "names the anchor: {msgs:?}");
        assert!(msgs[0].contains("#gggggg"), "names the value: {msgs:?}");
    }

    #[test]
    fn unparsable_ramp_entry_is_reported() {
        let dir = scratch_themes_dir("badramp");
        write_theme(
            &dir,
            "mine",
            "extends = \"nord\"\n[roles]\nurgency.ramp = [\"#000000\", \"nope\"]\n",
        );
        let msgs = load_reporting("mine", Some(&dir)).file.messages().to_vec();
        assert_eq!(msgs.len(), 1, "expected one complaint: {msgs:?}");
        assert!(msgs[0].contains("ramp"), "names the role: {msgs:?}");
        assert!(msgs[0].contains("nope"), "names the value: {msgs:?}");
    }

    #[test]
    fn role_fg_naming_nothing_is_reported() {
        // `merge` resolves an unknown fg to None, which does not fall back to the
        // base color — it strips it. Worse than ignored, and equally silent.
        let dir = scratch_themes_dir("badrole");
        write_theme(
            &dir,
            "mine",
            "extends = \"nord\"\n[roles]\nheader = { fg = \"dangre\" }\n",
        );
        let loaded = load_reporting("mine", Some(&dir));
        let msgs = loaded.file.messages();
        assert_eq!(msgs.len(), 1, "expected one complaint: {msgs:?}");
        assert!(msgs[0].contains("header"), "names the role: {msgs:?}");
        assert!(msgs[0].contains("dangre"), "names the typo: {msgs:?}");
        assert_eq!(loaded.theme.role("header").fg, None, "color really is gone");
    }

    #[test]
    fn a_good_user_file_reports_nothing() {
        // The guard on every message above: none of them may fire on a file that
        // is simply correct, or the render path warns on every run forever.
        let dir = scratch_themes_dir("clean");
        write_theme(
            &dir,
            "mine",
            r##"
name    = "mine"
extends = "gruvbox"

[palette]
danger = "#ff0000"

[roles]
tag          = { fg = "#123456" }
priority.H   = { fg = "danger", bold = true }
urgency.ramp = ["#000000", "#ffffff"]
"##,
        );
        let loaded = load_reporting("mine", Some(&dir));
        assert_eq!(loaded.file, FileOutcome::Merged(Vec::new()));
        assert_eq!(loaded.theme.name, "mine");
    }

    #[test]
    fn load_is_load_reporting_without_the_diagnostic() {
        // The 8 existing call sites keep the old signature; only the two that can
        // print anything need the richer one.
        let dir = scratch_themes_dir("delegates");
        write_theme(
            &dir,
            "mine",
            "extends = \"nord\"\n[roles]\ntag = { fg = \"#123456\" }\n",
        );
        assert_eq!(
            load("mine", Some(&dir)).role("tag").fg,
            load_reporting("mine", Some(&dir)).theme.role("tag").fg
        );
    }
}
