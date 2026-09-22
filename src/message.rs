//! The two top-level messages exchanged with `POST /v1/systemone`.

use serde::{Deserialize, Serialize};

use crate::answer::Answer;
use crate::json::{Json, Map};
use crate::question::Question;

/// A request body: one `state`, evaluated against every question in parallel.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize),
    rkyv(derive(Debug))
)]
pub struct Request {
    /// The content to evaluate: a string for text, or structured data for chat
    /// logs, records, or application state.
    pub state: Json,
    /// Which model handles the request.
    pub model: Model,
    /// Questions keyed by ids you choose. Answers come back under the same ids.
    ///
    /// The key is not sent to the model and does not affect inference.
    pub questions: Map<Question>,
}

impl Request {
    /// Starts a request against `state`, using [`Model::JevLatest`].
    #[must_use]
    pub fn new(state: impl Into<Json>) -> Self {
        Self {
            state: state.into(),
            model: Model::JevLatest,
            questions: Map::new(),
        }
    }

    /// Pins the request to a specific model or alias.
    #[must_use]
    pub fn with_model(mut self, model: Model) -> Self {
        self.model = model;
        self
    }

    /// Adds a question under `id`.
    #[must_use]
    pub fn ask(mut self, id: impl Into<String>, question: Question) -> Self {
        self.questions.insert(id, question);
        self
    }
}

/// A model name accepted by the `model` field.
///
/// Kept open: an unrecognized name deserializes into [`Model::Version`] rather
/// than failing, so a model released after this crate was built still parses.
/// The `#[serde(untagged)]` variant is what makes that fallback work while the
/// named aliases stay as plain strings on the wire.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize),
    rkyv(derive(Debug))
)]
#[serde(rename_all = "kebab-case")]
pub enum Model {
    /// `jev-latest` — the most recent stable release. Moves without notice.
    #[default]
    JevLatest,
    /// `jev-preview` — the most recent release, preview or not.
    JevPreview,
    /// A pinned version such as `jev-1.13.0`, or any name this crate predates.
    #[serde(untagged)]
    Version(String),
}

impl Model {
    /// The name as sent in the `model` field.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::JevLatest => "jev-latest",
            Self::JevPreview => "jev-preview",
            Self::Version(name) => name,
        }
    }

    /// `true` for names that resolve to a different model over time.
    ///
    /// Answers behind an alias can change without a change on your side; pin a
    /// version if you have tuned confidence thresholds.
    #[must_use]
    pub const fn is_alias(&self) -> bool {
        matches!(self, Self::JevLatest | Self::JevPreview)
    }
}

impl std::fmt::Display for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Read access to a [`Model`] borrowed out of an rkyv buffer.
#[cfg(feature = "rkyv")]
impl ArchivedModel {
    /// The name as sent in the `model` field, without deserializing.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::JevLatest => "jev-latest",
            Self::JevPreview => "jev-preview",
            Self::Version(name) => name.as_ref(),
        }
    }
}

/// A response body: one answer per question, plus usage.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize),
    rkyv(derive(Debug))
)]
pub struct Response {
    /// The versioned model that actually answered, even when an alias was sent.
    ///
    /// Typed as [`Model`] rather than `String` so it compares directly against
    /// the model you requested. In practice it is always a concrete version, so
    /// it lands in [`Model::Version`] via the untagged fallback — check with
    /// [`Model::is_alias`] if you need to be sure.
    pub model: Model,
    /// Answers keyed by the ids from the request.
    pub answers: Map<Answer>,
    /// Token usage for the request.
    pub usage: Usage,
}

impl Response {
    /// The answer stored under `id`.
    #[must_use]
    pub fn answer(&self, id: &str) -> Option<&Answer> {
        self.answers.get(id)
    }
}

/// Token usage. Only input tokens are billed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize),
    rkyv(derive(Debug))
)]
pub struct Usage {
    /// Tokens consumed by `state` plus all questions.
    pub input_tokens: u64,
    /// Tokens produced. Free.
    pub output_tokens: u64,
}
