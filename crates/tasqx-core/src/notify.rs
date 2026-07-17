//! Notification delivery (DESIGN.md §9).
//!
//! One [`Notifier`] abstraction, two backends:
//!  * [`LogNotifier`] — **always compiled**. Writes one line to stderr and
//!    returns. This is the headless/CI-safe path §9 demands: with no
//!    notification transport, delivery degrades to a logged line and exit 0,
//!    never an error.
//!  * [`OsNotifier`] — behind the **off-by-default `notify-os` feature**, using
//!    `notify-rust` (WinRT toast / `NSUserNotification` / D-Bus). It is gated
//!    because `notify-rust` drags WinRT in on Windows and a visual toast is not
//!    headlessly verifiable — neither belongs in the default build.
//!
//! Two rules hold across every backend:
//!  * **The log line is invariant.** `OsNotifier` logs *and then* attempts the
//!    toast, so the verifiable surface never depends on which backend is live
//!    (and a toast that fails is already covered).
//!  * **Delivery never fails.** `notify` returns `()`. A dead D-Bus, a missing
//!    Action Center, an unregistered AppUserModelID — all degrade to the line
//!    that was already written.
//!
//! Quiet by default (§9) is enforced by the *caller*, not here: the scheduler
//! only ever tracks tasks that carry an explicit `remind`, and [`default_notifier`]
//! hands back the log backend unless the user opted in via `[notify] enabled`.

use std::sync::Arc;

/// One reminder to deliver. Deliberately flat and owned — a backend may hand it
/// to an OS API on another thread.
#[derive(Debug, Clone)]
pub struct Notification {
    /// The task's stable `short_id`, so a user can act on it (`tasqx done 12`).
    pub short_id: i64,
    /// The task title.
    pub title: String,
    /// Supporting detail (the due date, when there is one).
    pub body: String,
}

impl Notification {
    /// The single stderr line every backend emits. One format, one place.
    fn log_line(&self) -> String {
        if self.body.is_empty() {
            format!("tasqx reminder: [#{}] {}", self.short_id, self.title)
        } else {
            format!("tasqx reminder: [#{}] {} ({})", self.short_id, self.title, self.body)
        }
    }
}

/// A notification transport. `Send + Sync` so the daemon can share one behind an
/// `Arc` across its reminder thread.
pub trait Notifier: Send + Sync {
    /// Deliver `n`. Infallible by contract: a backend that cannot deliver must
    /// degrade (§9), never propagate an error.
    fn notify(&self, n: &Notification);
}

/// The always-available backend: one stderr line, no dependencies, no failure
/// mode. Also the default for `daemon::serve` so CI never grows a toast habit.
pub struct LogNotifier;

impl Notifier for LogNotifier {
    fn notify(&self, n: &Notification) {
        eprintln!("{}", n.log_line());
    }
}

/// The OS backend (`notify-os` feature): logs the line, then attempts a native
/// notification. A delivery failure is swallowed — the line already landed.
#[cfg(feature = "notify-os")]
pub struct OsNotifier;

#[cfg(feature = "notify-os")]
impl Notifier for OsNotifier {
    fn notify(&self, n: &Notification) {
        // The log line first, unconditionally: it is the verifiable surface and
        // must not depend on the toast succeeding.
        eprintln!("{}", n.log_line());
        let summary = format!("tasqx #{}", n.short_id);
        let body = if n.body.is_empty() {
            n.title.clone()
        } else {
            format!("{}\n{}", n.title, n.body)
        };
        // `show()` can fail on a headless box, a dead session bus, or an
        // unregistered AppUserModelID. All of those degrade to the logged line.
        if let Err(e) = notify_rust::Notification::new().summary(&summary).body(&body).show() {
            eprintln!("tasqx reminder: OS notification unavailable ({e}); logged only");
        }
    }
}

/// Pick a backend. `os_enabled` is the user's `[notify] enabled` opt-in (§9
/// "quiet by default" — Tasqx never surprises you on first run).
///
/// Without the `notify-os` feature this always yields [`LogNotifier`], so the
/// default build has no OS notification surface at all and `os_enabled` is
/// simply inert.
pub fn default_notifier(os_enabled: bool) -> Arc<dyn Notifier> {
    #[cfg(feature = "notify-os")]
    {
        if os_enabled {
            return Arc::new(OsNotifier);
        }
    }
    #[cfg(not(feature = "notify-os"))]
    {
        let _ = os_enabled; // inert without the feature; keeps one signature.
    }
    Arc::new(LogNotifier)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A test backend that records what it was handed — the same shape the
    /// scheduler tests use to assert delivery without any OS involvement.
    struct Collecting(Mutex<Vec<Notification>>);

    impl Notifier for Collecting {
        fn notify(&self, n: &Notification) {
            self.0.lock().unwrap().push(n.clone());
        }
    }

    #[test]
    fn log_line_includes_short_id_title_and_body() {
        let n = Notification {
            short_id: 12,
            title: "Ship the thing".into(),
            body: "due 2026-07-20T17:00:00Z".into(),
        };
        assert_eq!(
            n.log_line(),
            "tasqx reminder: [#12] Ship the thing (due 2026-07-20T17:00:00Z)"
        );
    }

    #[test]
    fn log_line_omits_empty_body() {
        let n = Notification { short_id: 3, title: "No due date".into(), body: String::new() };
        assert_eq!(n.log_line(), "tasqx reminder: [#3] No due date");
    }

    #[test]
    fn notifier_is_object_safe_and_delivers() {
        let c = Collecting(Mutex::new(Vec::new()));
        let dynamic: &dyn Notifier = &c;
        dynamic.notify(&Notification { short_id: 1, title: "t".into(), body: String::new() });
        assert_eq!(c.0.lock().unwrap().len(), 1);
    }

    /// Not opting in yields the log backend in **either** build configuration —
    /// the "quiet by default" guarantee (§9). Deliberately does not exercise
    /// `default_notifier(true)`: with `notify-os` compiled in that would hand
    /// back `OsNotifier` and fire a real toast from a test run.
    #[test]
    fn default_notifier_is_log_only_when_not_opted_in() {
        let n = default_notifier(false);
        // Never panics and never needs a transport — the CI-safe guarantee.
        n.notify(&Notification { short_id: 1, title: "quiet".into(), body: String::new() });
    }

    /// Without the feature, the opt-in can't resurrect a backend that isn't in
    /// the binary — it is inert, not an error.
    #[cfg(not(feature = "notify-os"))]
    #[test]
    fn opting_in_is_inert_without_the_feature() {
        let n = default_notifier(true);
        n.notify(&Notification { short_id: 2, title: "still fine".into(), body: String::new() });
    }
}
