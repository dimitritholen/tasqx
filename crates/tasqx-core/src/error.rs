//! Typed error domain for the core API.
//!
//! Every failure the engine can surface maps 1:1 onto one of the five stable
//! API error codes from DESIGN.md §4. `ApiError` is what handlers return; it
//! serializes into the `error` object of the response envelope, and its
//! `exit_code()` drives the CLI's stable exit codes.

use serde::Serialize;
use serde_json::Value;

/// Stable, machine-first error codes (DESIGN.md §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// Malformed params / failed validation (also permission denial).
    BadRequest,
    /// Referenced entity does not exist.
    NotFound,
    /// Optimistic-concurrency / dependency-cycle / duplicate.
    Conflict,
    /// Client major version not served.
    UnsupportedVersion,
    /// Internal bug; safe to retry-report.
    Internal,
}

impl ErrorCode {
    /// CLI exit code mapping (DESIGN.md §4: 0 ok, 2 bad_request, 4 not_found,
    /// 5 conflict). Version/internal get their own non-zero codes.
    pub fn exit_code(self) -> i32 {
        match self {
            ErrorCode::BadRequest => 2,
            ErrorCode::NotFound => 4,
            ErrorCode::Conflict => 5,
            ErrorCode::UnsupportedVersion => 6,
            ErrorCode::Internal => 1,
        }
    }

    /// The stable wire name, mirroring [`crate::Status::as_str`].
    ///
    /// These strings are **the same contract** as the `Serialize` derive above:
    /// they are what `ErrorBody.code` puts on the wire (DESIGN.md §4), so this
    /// match must stay character-identical to the `snake_case` rename. It is
    /// not a display convenience that may drift — a test compares the two.
    ///
    /// Exists so a caller that only wants the name can have it for free.
    /// Without it the CLI round-trips the code through `serde_json::to_value`
    /// and allocates a `String` just to print five constant words.
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::BadRequest => "bad_request",
            ErrorCode::NotFound => "not_found",
            ErrorCode::Conflict => "conflict",
            ErrorCode::UnsupportedVersion => "unsupported_version",
            ErrorCode::Internal => "internal",
        }
    }
}

/// An error carrying a stable code, a human message, and optional structured
/// `data` (e.g. the field errors, or the `short_id` that was not found).
#[derive(Debug)]
pub struct ApiError {
    /// The stable, machine-first classification. This is the part a caller may
    /// branch on; `message` is not.
    pub code: ErrorCode,
    /// Human-readable explanation. Free text by design — it names the offending
    /// value and, where there is one, the accepted set.
    pub message: String,
    /// Optional machine-readable detail (the `short_id` that was not found, the
    /// per-field errors). Absent from the serialized envelope when `None`, so a
    /// client sees no key rather than a null.
    pub data: Option<Value>,
}

impl ApiError {
    /// The general constructor. The four helpers below are the ones handlers
    /// normally reach for; use this when the `data` payload is the point.
    pub fn new(code: ErrorCode, message: impl Into<String>, data: Option<Value>) -> Self {
        ApiError {
            code,
            message: message.into(),
            data,
        }
    }

    /// Malformed or invalid params (exit 2). The catch-all for "the caller asked
    /// for something that is not a question this API accepts".
    pub fn bad_request(message: impl Into<String>) -> Self {
        ApiError::new(ErrorCode::BadRequest, message, None)
    }

    /// A named entity does not exist (exit 4). Takes `data` because the caller
    /// usually needs the ref back — `{"short_id": 12}` — to report it usefully.
    pub fn not_found(message: impl Into<String>, data: Option<Value>) -> Self {
        ApiError::new(ErrorCode::NotFound, message, data)
    }

    /// The request is well-formed but collides with the store's current state
    /// (exit 5): a stale `_rev`, a dependency cycle, a duplicate name.
    pub fn conflict(message: impl Into<String>) -> Self {
        ApiError::new(ErrorCode::Conflict, message, None)
    }

    /// An engine bug or a storage failure (exit 1). Never used for anything the
    /// caller could have done differently.
    pub fn internal(message: impl Into<String>) -> Self {
        ApiError::new(ErrorCode::Internal, message, None)
    }

    /// This error's CLI exit code — [`ErrorCode::exit_code`] on `self.code`,
    /// so the process status and the JSON `code` can never disagree.
    pub fn exit_code(&self) -> i32 {
        self.code.exit_code()
    }
}

/// Human-facing rendering: `not_found: no task matches 3f2`. The code is part
/// of it deliberately — it is the stable half of the sentence, so a line in a
/// log or an `anyhow` chain is still greppable when the prose is reworded.
///
/// `data` is left out. It is machine payload for the JSON envelope
/// (see [`ErrorBody`]), can be arbitrarily large, and `{:?}` already shows it.
impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

/// `ApiError` is the public error type of a library crate (re-exported at the
/// crate root), so it must *be* an error: without this impl a downstream caller
/// cannot `?` it into `Box<dyn Error>` or `anyhow::Error` at all — a compile
/// error on their side, not a matter of taste. DESIGN.md §"Core = a library
/// first" makes that consumer a documented scenario.
///
/// No `source()`: the low-level cause is intentionally *not* retained. The
/// `From` impls below collapse storage and JSON failures into `internal` /
/// `bad_request` (see the comment there) so handlers can use `?` without
/// hand-mapping; the cause is folded into `message` instead of a chain.
/// Reversing that is a DESIGN decision, not something to slip in here.
impl std::error::Error for ApiError {}

/// The serialized `error` object inside a response envelope.
#[derive(Debug, Serialize)]
pub struct ErrorBody {
    /// The stable code, serialized `snake_case` (`bad_request`, `not_found`, …).
    pub code: ErrorCode,
    /// The human-readable explanation, verbatim from the [`ApiError`].
    pub message: String,
    /// The structured detail, omitted from the JSON entirely when there is none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl From<&ApiError> for ErrorBody {
    fn from(e: &ApiError) -> Self {
        ErrorBody {
            code: e.code,
            message: e.message.clone(),
            data: e.data.clone(),
        }
    }
}

// Storage and (de)serialization failures collapse to `internal` / `bad_request`
// so a handler can use `?` freely without hand-mapping every low-level error.
impl From<rusqlite::Error> for ApiError {
    fn from(e: rusqlite::Error) -> Self {
        ApiError::internal(format!("storage error: {e}"))
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(e: serde_json::Error) -> Self {
        ApiError::bad_request(format!("invalid JSON params: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every variant, restated with the wire name it *must* serialize to. The
    /// match is exhaustive, so a sixth `ErrorCode` is a compile error right
    /// here — which is the only mechanism that drags the author of a new code
    /// into this file to pick its name deliberately. Having done so they must
    /// also extend `ALL` below, or the new code escapes the serde comparison.
    fn wire_name(code: ErrorCode) -> &'static str {
        match code {
            ErrorCode::BadRequest => "bad_request",
            ErrorCode::NotFound => "not_found",
            ErrorCode::Conflict => "conflict",
            ErrorCode::UnsupportedVersion => "unsupported_version",
            ErrorCode::Internal => "internal",
        }
    }

    /// Hand-written like `Status::ALL` (types.rs) and carrying the same failure
    /// mode: the declared length makes a *missing* entry a compile error, but a
    /// duplicate would satisfy the length while dropping a code from every
    /// assertion below. Kept test-local on purpose — nothing in the workspace
    /// needs to enumerate error codes at runtime, so a `pub ALL` would be API
    /// surface with no consumer.
    const ALL: [ErrorCode; 5] = [
        ErrorCode::BadRequest,
        ErrorCode::NotFound,
        ErrorCode::Conflict,
        ErrorCode::UnsupportedVersion,
        ErrorCode::Internal,
    ];

    #[test]
    fn all_lists_every_code_exactly_once() {
        let mut seen: Vec<&str> = ALL.iter().map(|c| c.as_str()).collect();
        let before = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), before, "ALL contains a duplicate");
        assert_eq!(before, 5, "ALL must list every variant");
    }

    /// `as_str` is a second hand-written match over the same variants as the
    /// `Serialize` derive, and the JSON envelope (`ErrorBody`, DESIGN.md §4) is
    /// the wire contract. A typo like `"notfound"` compiles, type-checks, and
    /// silently makes the printed code disagree with the transmitted one — so
    /// the two are compared against each other, not just against a literal.
    #[test]
    fn as_str_matches_the_serialized_wire_name() {
        for code in ALL {
            let json = serde_json::to_value(code).expect("fieldless enum always serializes");
            assert_eq!(
                json.as_str(),
                Some(code.as_str()),
                "as_str diverged from serde for {code:?}"
            );
            assert_eq!(code.as_str(), wire_name(code), "wrong name for {code:?}");
        }
    }

    /// The whole point of the `Error` impl: a downstream crate must be able to
    /// `?` an `ApiError` into a boxed error, which needs `Error + Send + Sync`.
    /// Written as a real fallible function because a trait-bound assertion
    /// alone would not prove the `?` conversion resolves.
    #[test]
    fn propagates_into_a_boxed_error_via_question_mark() {
        fn fallible() -> Result<(), ApiError> {
            Err(ApiError::not_found("no such task", None))
        }
        fn caller() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            fallible()?;
            Ok(())
        }
        let boxed = caller().expect_err("fallible() always fails");
        assert_eq!(boxed.to_string(), "not_found: no such task");
        // The cause deliberately does not survive `From<rusqlite::Error>` (see
        // the comment above that impl), so the chain ends here. Pinned so that
        // adding a `source` field later is a visible decision, not a drive-by.
        assert!(boxed.source().is_none(), "no cause is retained today");
    }

    /// `Display` is the human-facing rendering (`{}`); `Debug` stays the
    /// structural dump (`{:?}`). Asserting they differ stops the classic
    /// mistake of implementing one by delegating to the other.
    #[test]
    fn display_carries_the_code_and_message_without_debug_noise() {
        let e = ApiError::new(
            ErrorCode::Conflict,
            "version mismatch",
            Some(serde_json::json!({"expected": 3})),
        );
        assert_eq!(e.to_string(), "conflict: version mismatch");
        assert!(
            format!("{e:?}").contains("ApiError"),
            "Debug must stay structural"
        );
    }
}
