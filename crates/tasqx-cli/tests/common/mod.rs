//! Helpers shared by the unit-test modules (through `#[path]` in `lib.rs`)
//! and the integration tests (through `mod common;`).

/// `s` without its SGR escapes.
///
/// SGR only: everything from `ESC` up to the next `m` is dropped. Any other
/// escape (a cursor move, an OSC hyperlink) is swallowed up to the next `m`
/// as well, so a test against a renderer that starts emitting one would pass
/// for the wrong reason. Widen this, in this one place, when that happens.
#[allow(dead_code)] // each including crate uses it; none is guaranteed to
pub fn strip_sgr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
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
