//! # tasqx-core
//!
//! The headless engine behind tasqx (DESIGN.md §2). A plain Rust library: the
//! CLI links it and calls [`dispatch`] in-process with no serialization tax,
//! while the stdio `api` transport wraps that same dispatch table in the JSON
//! envelope via [`handle_envelope`]. There is exactly one dispatch table, so
//! "call a function" and "send a JSON command" run identical code.
//!
//! Module map:
//!  * [`types`]    — domain types + the request envelope (the serde contract).
//!  * [`error`]    — the five stable API error codes.
//!  * [`storage`]  — SQLite setup (WAL, busy_timeout), schema, row primitives.
//!  * [`engine`]   — [`Engine`] and the per-method mutation/query logic.
//!  * [`dispatch`] — the single dispatch table + envelope handling.
//!  * [`filter`]   — the filter DSL subset used by `task.list`.
//!  * [`urgency`]  — the fixed urgency formula.
//!  * [`remind`]   — reminder specs: `due`-anchored offsets + absolute instants.
//!  * [`scheduler`]— the daemon's reminder min-heap (§9), ripeness by injected now.
//!  * [`notify`]   — the `Notifier` trait; log backend always, OS behind `notify-os`.
//!
//! The full §4 method catalogue is implemented (task add/list/get/start/stop/
//! done/modify/cancel/reopen, project create/list/archive, tag.add,
//! annotation.add, dependency add/remove, report.summary, store export/import,
//! event.list, core.capabilities). Real dependency/blocked logic and the
//! §12-D8 boolean/grouping filter grammar are wired in.
//!
//! Not built yet (clear seams, no build-breaking stubs): the daemon/socket
//! transport, MCP server, hooks/plugins, recurrence, `event.revert`/undo, and
//! configurable urgency weights. Adding them stays additive — new match arms in
//! [`dispatch`] and new engine methods — with no change to the envelope.

pub mod daemon;
pub mod datetime;
pub mod dispatch;
pub mod engine;
pub mod error;
pub mod filter;
pub mod mcp;
pub mod notify;
pub mod recur;
pub mod remind;
pub mod scheduler;
pub mod storage;
pub mod types;
pub mod urgency;
pub mod util;

pub use dispatch::{capabilities, dispatch, handle_envelope, API_VERSION, PARAMS};
pub use mcp::{McpServer, Scope};
pub use engine::Engine;
pub use error::{ApiError, ErrorCode};
pub use notify::{Notification, Notifier};
pub use scheduler::ReminderScheduler;
pub use types::{Entity, Priority, Status, Task};
