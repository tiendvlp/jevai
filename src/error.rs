//! Error responses.
//!
//! The endpoint returns `{"detail": …}` on every failure, but `detail` is one
//! of three unrelated JSON shapes depending on which layer rejected the call:
//!
//! | Shape | Layer | Example |
//! |---|---|---|
//! | array | request-schema validation (422) | `[{"type":"missing","loc":["body","state"],…}]` |
//! | object | application validation (400, 401) | `{"error_type":"api_usage_error","message":"…"}` |
//! | string | application validation (400) | `"Too many choices. Must have at most 255 choices."` |
//!
//! This is what `#[serde(untagged)]` is for. There is no discriminant field to
//! match on — the *shape itself* is the discriminant — so serde tries each
//! variant and keeps the one that fits. Callers that only want text can ignore
//! the distinction entirely and use [`Display`](std::fmt::Display).
//!
//! All three shapes were captured from the live API on 2026-09-22.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::json::{Json, Map};

/// The body of any non-2xx response.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize),
    rkyv(derive(Debug))
)]
pub struct ApiError {
    /// What went wrong, in whichever of the three shapes the server chose.
    pub detail: ErrorDetail,
}

impl ApiError {
    /// The machine-readable error type, when the server sent the object shape.
    #[must_use]
    pub const fn error_type(&self) -> Option<&ErrorType> {
        match &self.detail {
            ErrorDetail::Structured(e) => Some(&e.error_type),
            _ => None,
        }
    }

    /// `true` when the failure is an authentication problem.
    #[must_use]
    pub fn is_auth(&self) -> bool {
        matches!(self.error_type(), Some(ErrorType::AuthenticationError))
    }

    /// The field paths the request-schema layer rejected, if it was that layer.
    ///
    /// Each path is a `loc` array such as `["body", "state"]`. Note the entries
    /// are mixed — a JSON syntax error reports a byte offset, `["body", 9]` —
    /// which is why they are [`Json`] rather than strings.
    pub fn invalid_fields(&self) -> impl Iterator<Item = &[Json]> {
        match &self.detail {
            ErrorDetail::Validation(errors) => errors.as_slice(),
            _ => &[],
        }
        .iter()
        .map(|e| e.loc.as_slice())
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.detail.fmt(f)
    }
}

impl std::error::Error for ApiError {}

/// The three shapes `detail` arrives in.
///
/// Variants are ordered most-specific first. They are disjoint by JSON type
/// (array / object / string), so no input can match two of them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize),
    rkyv(derive(Debug))
)]
#[serde(untagged)]
pub enum ErrorDetail {
    /// Request-schema validation, one entry per offending field. Status 422.
    Validation(Vec<ValidationError>),
    /// Application-level rejection with a machine-readable type. Status 400/401.
    Structured(StructuredError),
    /// Application-level rejection carrying only prose. Status 400.
    Message(String),
}

impl fmt::Display for ErrorDetail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Message(msg) => f.write_str(msg),
            Self::Structured(e) => write!(f, "{}: {}", e.error_type, e.message),
            Self::Validation(errors) => {
                for (i, error) in errors.iter().enumerate() {
                    if i > 0 {
                        f.write_str("; ")?;
                    }
                    write!(f, "{error}")?;
                }
                Ok(())
            }
        }
    }
}

/// An application-level rejection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize),
    rkyv(derive(Debug))
)]
pub struct StructuredError {
    /// The machine-readable category.
    pub error_type: ErrorType,
    /// Human-readable explanation.
    pub message: String,
}

/// A machine-readable error category.
///
/// Open, like [`Model`](crate::Model): a category this crate predates lands in
/// [`ErrorType::Other`] instead of failing the parse, because failing to parse
/// an error response is the worst possible moment to start failing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize),
    rkyv(derive(Debug))
)]
#[serde(rename_all = "snake_case")]
pub enum ErrorType {
    /// The API key is missing or invalid.
    AuthenticationError,
    /// The request was well-formed JSON but asked for something invalid.
    ApiUsageError,
    /// Any category introduced after this crate was built.
    #[serde(untagged)]
    Other(String),
}

impl ErrorType {
    /// The category as sent on the wire.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::AuthenticationError => "authentication_error",
            Self::ApiUsageError => "api_usage_error",
            Self::Other(name) => name,
        }
    }
}

impl fmt::Display for ErrorType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One entry from the request-schema validation layer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize),
    rkyv(derive(Debug))
)]
pub struct ValidationError {
    /// The validator that fired, e.g. `missing`, `too_short`, `json_invalid`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Path to the offending input. Mixed strings and byte offsets.
    pub loc: Vec<Json>,
    /// Human-readable explanation.
    pub msg: String,
    /// The input that failed, echoed back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Json>,
    /// Validator-specific context, such as `min_length`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctx: Option<Map<Json>>,
}

impl ValidationError {
    /// The `loc` path rendered as a dotted string, e.g. `body.state`.
    #[must_use]
    pub fn path(&self) -> String {
        self.loc
            .iter()
            .map(|part| match part {
                Json::Str(s) => s.clone(),
                other => format!("{other:?}"),
            })
            .collect::<Vec<_>>()
            .join(".")
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({}): {}", self.path(), self.kind, self.msg)
    }
}

/// The HTTP status of a failed call, interpreted.
///
/// The status is transport metadata, not part of the body, so this is
/// constructed from the code your HTTP client reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApiStatus {
    /// 400 — application-level validation rejected the request.
    BadRequest,
    /// 401 — missing or invalid API key.
    Unauthorized,
    /// 422 — the body failed request-schema validation.
    UnprocessableEntity,
    /// 429 — rate limit exceeded. Retry with backoff.
    RateLimited,
    /// 529 — TypeSafe is overloaded. Retry with backoff.
    Overloaded,
    /// Anything else.
    Other(u16),
}

impl ApiStatus {
    /// Interprets an HTTP status code.
    #[must_use]
    pub const fn from_code(code: u16) -> Self {
        match code {
            400 => Self::BadRequest,
            401 => Self::Unauthorized,
            422 => Self::UnprocessableEntity,
            429 => Self::RateLimited,
            529 => Self::Overloaded,
            other => Self::Other(other),
        }
    }

    /// The numeric code.
    #[must_use]
    pub const fn code(self) -> u16 {
        match self {
            Self::BadRequest => 400,
            Self::Unauthorized => 401,
            Self::UnprocessableEntity => 422,
            Self::RateLimited => 429,
            Self::Overloaded => 529,
            Self::Other(code) => code,
        }
    }

    /// `true` for failures worth retrying with exponential backoff.
    ///
    /// Retrying anything else just burns quota — the request itself is wrong.
    #[must_use]
    pub const fn is_retryable(self) -> bool {
        matches!(self, Self::RateLimited | Self::Overloaded)
    }
}

impl fmt::Display for ApiStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code())
    }
}
