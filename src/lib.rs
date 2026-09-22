//! Typed messages for the TypeSafe System One (Jev) API.
//!
//! Models `POST https://api.typesafe.ai/v1/systemone` — a [`Request`] carrying
//! one `state` and a map of typed [`Question`]s, answered by a [`Response`]
//! carrying one [`Answer`] per question under the same ids.
//!
//! # Two serializers, one type
//!
//! Every type here derives both [`serde`] and [`rkyv`]. They do not interfere:
//! serde attributes (`tag`, `flatten`, `rename`) describe the *JSON wire form*,
//! while rkyv derives a separate zero-copy `Archived*` type from the *Rust*
//! shape and ignores serde attributes entirely. Use serde to talk to the API
//! and rkyv to cache, memory-map, or IPC the same values with no parse step.
//!
//! # Sending requests
//!
//! The `client` feature (on by default) adds [`JevClient`], an async client
//! built on reqwest that retries `429`/`529` with backoff. It compiles for
//! native targets and for `wasm32-unknown-unknown`. Turn the feature off to get
//! the types alone with no HTTP stack.
//!
//! The one thing this costs: [`serde_json::Value`] has no `rkyv::Archive` impl,
//! so the polymorphic `string | object | array` fields use [`Json`] instead.
//! Convert at the edges with [`Json::to_serde`] and [`Json::from_serde`].
//!
//! # Example
//!
//! ```
//! use jevai::{Map, NoulCriteria, Question, Request};
//!
//! let request = Request::new("Help! My payouts have been failing for 3 days.")
//!     .ask(
//!         "is_urgent",
//!         Question::noul("Does this convey urgency?")
//!             .with_noul_criteria(NoulCriteria::new(
//!                 "Explicitly time-sensitive",
//!                 "No urgency expressed",
//!             )),
//!     )
//!     .ask(
//!         "department",
//!         Question::choice(
//!             "Which team should handle this?",
//!             Map::new()
//!                 .with("billing", "Payments, invoicing, refunds")
//!                 .with("technical", "Bugs, outages, integrations"),
//!         )?,
//!     );
//!
//! let body = serde_json::to_string(&request)?;
//! assert!(body.contains(r#""type":"noul""#));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![forbid(unsafe_code)]

pub mod answer;
#[cfg(feature = "client")]
pub mod client;
pub mod error;
pub mod json;
pub mod message;
pub mod question;

pub use answer::Answer;
#[cfg(feature = "client")]
pub use client::{JevClient, JevClientBuilder, RetryPolicy};
pub use error::{ApiError, ApiStatus, ErrorDetail, ErrorType, StructuredError, ValidationError};
pub use json::{Json, Map};
pub use message::{Model, Request, Response, Usage};
pub use answer::{ChoiceAnswer, NoulAnswer, ScoreAnswer};
pub use question::{
    ChoiceCriteria, ChoiceQuestion, InvalidCriteria, NoulCriteria, NoulQuestion, Question,
    QuestionKind, ScoreCriteria, ScoreQuestion, MAX_CHOICE_OPTIONS, MAX_SCORE_LEVELS,
    MIN_SCORE_LEVELS,
};

/// Re-exported so downstream crates archive with exactly this version.
///
/// rkyv's archived layouts are only compatible within a version, so depending
/// on `jevai::rkyv` rather than a separately-pinned `rkyv` removes a whole
/// class of silent mismatch.
#[cfg(feature = "rkyv")]
pub use ::rkyv;

/// The production evaluation endpoint.
pub const ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";
