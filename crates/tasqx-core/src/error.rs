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
}

/// An error carrying a stable code, a human message, and optional structured
/// `data` (e.g. the field errors, or the `short_id` that was not found).
#[derive(Debug)]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,
    pub data: Option<Value>,
}

impl ApiError {
    pub fn new(code: ErrorCode, message: impl Into<String>, data: Option<Value>) -> Self {
        ApiError {
            code,
            message: message.into(),
            data,
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        ApiError::new(ErrorCode::BadRequest, message, None)
    }

    pub fn not_found(message: impl Into<String>, data: Option<Value>) -> Self {
        ApiError::new(ErrorCode::NotFound, message, data)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        ApiError::new(ErrorCode::Conflict, message, None)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        ApiError::new(ErrorCode::Internal, message, None)
    }

    pub fn exit_code(&self) -> i32 {
        self.code.exit_code()
    }
}

/// The serialized `error` object inside a response envelope.
#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub code: ErrorCode,
    pub message: String,
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
