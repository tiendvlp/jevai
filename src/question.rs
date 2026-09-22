//! Request-side question types.
//!
//! On the wire a question is one flat object:
//!
//! ```json
//! { "type": "choice", "instructions": "Which team?", "criteria": { "billing": "..." } }
//! ```
//!
//! `instructions` is shared by all three kinds; `type` and `criteria` vary
//! together. [`Question`] models that split directly — a shared field plus a
//! `#[serde(flatten)]`ed, internally tagged [`QuestionKind`] — so the
//! discriminant lives in the enum where it belongs and never appears as a
//! hand-written `type: String` field.

use serde::{Deserialize, Serialize};

use crate::json::{Json, Map};

/// The largest number of options a single Choice may define.
///
/// Verified against the live API: 255 options succeed, 256 returns 400.
pub const MAX_CHOICE_OPTIONS: usize = 255;
/// The smallest number of levels a Score must define.
///
/// The docs say a Score "should have at least two levels", but the live API
/// accepts one (it answers `0.0` with confidence `1.0`), so this bound is 1.
/// A type should forbid what the server rejects, not what we think is unwise.
pub const MIN_SCORE_LEVELS: usize = 1;
/// The largest number of levels a Score may define.
///
/// Verified against the live API: 10 levels succeed, 11 returns 400.
pub const MAX_SCORE_LEVELS: usize = 10;

/// One typed question, evaluated against the request's `state`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize),
    rkyv(derive(Debug))
)]
pub struct Question {
    /// `type` + `criteria`, flattened into this object.
    ///
    /// Declared first so `type` leads the serialized object, which keeps logged
    /// requests readable.
    #[serde(flatten)]
    pub kind: QuestionKind,

    /// What to evaluate. A plain string, or an object holding the question in
    /// one field and the data it refers to in others.
    pub instructions: Json,
}

impl Question {
    /// Builds a yes/no question.
    #[must_use]
    pub fn noul(instructions: impl Into<Json>) -> Self {
        Self {
            kind: QuestionKind::Noul(NoulQuestion { criteria: None }),
            instructions: instructions.into(),
        }
    }

    /// Builds a single-choice question from an ordered option → rubric map.
    ///
    /// Use [`Json::Null`] as the rubric for an option that needs no detail.
    ///
    /// # Errors
    /// Returns [`InvalidCriteria`] unless the map holds 1..=[`MAX_CHOICE_OPTIONS`] options.
    pub fn choice(
        instructions: impl Into<Json>,
        criteria: impl Into<Map<Json>>,
    ) -> Result<Self, InvalidCriteria> {
        Ok(Self {
            kind: QuestionKind::Choice(ChoiceQuestion {
                criteria: ChoiceCriteria::new(criteria.into())?,
            }),
            instructions: instructions.into(),
        })
    }

    /// Builds a graded question from an ordered list of level descriptions.
    ///
    /// # Errors
    /// Returns [`InvalidCriteria`] unless there are
    /// [`MIN_SCORE_LEVELS`]..=[`MAX_SCORE_LEVELS`] levels.
    pub fn score(
        instructions: impl Into<Json>,
        levels: impl IntoIterator<Item = impl Into<Json>>,
    ) -> Result<Self, InvalidCriteria> {
        Ok(Self {
            kind: QuestionKind::Score(ScoreQuestion {
                criteria: ScoreCriteria::new(levels.into_iter().map(Into::into).collect())?,
            }),
            instructions: instructions.into(),
        })
    }

    /// Attaches descriptions of what yes and no mean. No-op on other kinds.
    #[must_use]
    pub fn with_noul_criteria(mut self, criteria: NoulCriteria) -> Self {
        if let QuestionKind::Noul(noul) = &mut self.kind {
            noul.criteria = Some(criteria);
        }
        self
    }
}

/// The three question types, discriminated by the `type` field they carry.
///
/// `#[serde(tag = "type")]` is what removes the redundancy: the variant *is* the
/// `type` value, so `QuestionKind::Noul` and `"type": "noul"` are the same fact
/// stated once. Each variant wraps a named struct, whose fields serde inlines
/// alongside the tag — no nesting appears on the wire.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize),
    rkyv(derive(Debug))
)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum QuestionKind {
    /// A yes/no question. Answered with a probability in `0.0..=1.0`.
    Noul(NoulQuestion),
    /// Pick one option from a set. Answered with the option and a distribution.
    Choice(ChoiceQuestion),
    /// Rate along an ordered rubric. Answered with a weighted value.
    Score(ScoreQuestion),
}

impl QuestionKind {
    /// The wire value of this kind's `type` field.
    #[must_use]
    pub const fn tag(&self) -> &'static str {
        match self {
            Self::Noul(_) => "noul",
            Self::Choice(_) => "choice",
            Self::Score(_) => "score",
        }
    }
}

/// The `criteria` of a yes/no question. Optional — a Noul is valid without one.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize),
    rkyv(derive(Debug))
)]
pub struct NoulQuestion {
    /// Descriptions of what yes and no mean.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub criteria: Option<NoulCriteria>,
}

/// The `criteria` of a single-choice question.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize),
    rkyv(derive(Debug))
)]
pub struct ChoiceQuestion {
    /// Ordered option → rubric map.
    pub criteria: ChoiceCriteria,
}

/// The `criteria` of a graded question.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize),
    rkyv(derive(Debug))
)]
pub struct ScoreQuestion {
    /// Ordered level descriptions, lowest first.
    pub criteria: ScoreCriteria,
}

/// Optional clarification of what a yes and a no mean for a Noul.
///
/// The wire keys are the bare words `true` and `false`, which are Rust
/// keywords; `#[serde(rename)]` keeps the Rust field names idiomatic without
/// raw identifiers.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize),
    rkyv(derive(Debug))
)]
pub struct NoulCriteria {
    /// What a value near 1 means.
    #[serde(rename = "true", default, skip_serializing_if = "Option::is_none")]
    pub yes: Option<Json>,
    /// What a value near 0 means.
    #[serde(rename = "false", default, skip_serializing_if = "Option::is_none")]
    pub no: Option<Json>,
}

impl NoulCriteria {
    /// Builds criteria from a yes and a no description.
    #[must_use]
    pub fn new(yes: impl Into<Json>, no: impl Into<Json>) -> Self {
        Self {
            yes: Some(yes.into()),
            no: Some(no.into()),
        }
    }
}

/// A validated set of Choice options: 1..=[`MAX_CHOICE_OPTIONS`] entries.
///
/// Values are plain [`Json`], not `Option<Json>`. The API documents an option's
/// rubric as `string | object | array | null`, and `null` is already one of
/// [`Json`]'s untagged variants — wrapping it in `Option` would encode absence
/// twice and make `Some(Json::Null)` and `None` two spellings of the same
/// `null` on the wire. Use [`Json::Null`] for an option that needs no rubric.
///
/// The inner map is private so the bound cannot be broken after construction,
/// and `#[serde(try_from)]` routes deserialization through the same check —
/// there is no way to obtain an oversized `ChoiceCriteria`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize),
    rkyv(derive(Debug))
)]
#[serde(try_from = "Map<Json>")]
pub struct ChoiceCriteria(Map<Json>);

impl ChoiceCriteria {
    /// Validates and wraps an ordered option map.
    ///
    /// # Errors
    /// Returns [`InvalidCriteria`] if empty or over [`MAX_CHOICE_OPTIONS`].
    pub fn new(options: Map<Json>) -> Result<Self, InvalidCriteria> {
        match options.len() {
            0 => Err(InvalidCriteria::NoChoiceOptions),
            n if n > MAX_CHOICE_OPTIONS => Err(InvalidCriteria::TooManyChoiceOptions(n)),
            _ => Ok(Self(options)),
        }
    }

    /// The options, in the order they will be presented to the model.
    #[must_use]
    pub const fn options(&self) -> &Map<Json> {
        &self.0
    }
}

// Written by hand rather than via `#[serde(into)]`, which would clone the map on
// every request.
impl Serialize for ChoiceCriteria {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl TryFrom<Map<Json>> for ChoiceCriteria {
    type Error = InvalidCriteria;

    fn try_from(value: Map<Json>) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

/// A validated ladder of Score levels: [`MIN_SCORE_LEVELS`]..=[`MAX_SCORE_LEVELS`], lowest first.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize),
    rkyv(derive(Debug))
)]
#[serde(try_from = "Vec<Json>")]
pub struct ScoreCriteria(Vec<Json>);

impl ScoreCriteria {
    /// Validates and wraps an ordered list of level descriptions.
    ///
    /// # Errors
    /// Returns [`InvalidCriteria`] outside [`MIN_SCORE_LEVELS`]..=[`MAX_SCORE_LEVELS`].
    pub fn new(levels: Vec<Json>) -> Result<Self, InvalidCriteria> {
        match levels.len() {
            n if n < MIN_SCORE_LEVELS => Err(InvalidCriteria::TooFewScoreLevels(n)),
            n if n > MAX_SCORE_LEVELS => Err(InvalidCriteria::TooManyScoreLevels(n)),
            _ => Ok(Self(levels)),
        }
    }

    /// The levels, lowest first. Index is the level number the answer reports.
    #[must_use]
    pub fn levels(&self) -> &[Json] {
        &self.0
    }
}

impl Serialize for ScoreCriteria {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl TryFrom<Vec<Json>> for ScoreCriteria {
    type Error = InvalidCriteria;

    fn try_from(value: Vec<Json>) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

/// A criteria set that the API would reject.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvalidCriteria {
    /// A Choice needs at least one option.
    NoChoiceOptions,
    /// A Choice may define at most [`MAX_CHOICE_OPTIONS`] options.
    TooManyChoiceOptions(usize),
    /// A Score needs at least [`MIN_SCORE_LEVELS`] levels.
    TooFewScoreLevels(usize),
    /// A Score may define at most [`MAX_SCORE_LEVELS`] levels.
    TooManyScoreLevels(usize),
}

impl std::fmt::Display for InvalidCriteria {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoChoiceOptions => write!(f, "a choice needs at least one option"),
            Self::TooManyChoiceOptions(n) => {
                write!(f, "{n} choice options exceeds the maximum of {MAX_CHOICE_OPTIONS}")
            }
            Self::TooFewScoreLevels(n) => {
                write!(f, "{n} score levels is below the minimum of {MIN_SCORE_LEVELS}")
            }
            Self::TooManyScoreLevels(n) => {
                write!(f, "{n} score levels exceeds the maximum of {MAX_SCORE_LEVELS}")
            }
        }
    }
}

impl std::error::Error for InvalidCriteria {}
