//! Response-side answer types.
//!
//! Answers share no field across the three kinds, so unlike
//! [`Question`](crate::Question) there is nothing to flatten *around*: the enum
//! is internally tagged on `type`, and that alone collapses the wrapper. There
//! is no `Answer { kind, choice: Option<String>, noul: Option<f64>, .. }` whose
//! fields are `None` five-sixths of the time — each variant holds exactly the
//! fields the API sends for it, and nothing else is representable.

use serde::{Deserialize, Serialize};

use crate::json::Map;

/// The answer to one question, keyed in the response by that question's id.
///
/// Each variant wraps a named struct rather than inlining its fields, so the
/// payload can be passed around on its own — `as_choice()` hands back the
/// option, the distribution and the confidence together.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize),
    rkyv(derive(Debug))
)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Answer {
    /// The answer to a [`NoulQuestion`](crate::question::NoulQuestion).
    Noul(NoulAnswer),
    /// The answer to a [`ChoiceQuestion`](crate::question::ChoiceQuestion).
    Choice(ChoiceAnswer),
    /// The answer to a [`ScoreQuestion`](crate::question::ScoreQuestion).
    Score(ScoreAnswer),
}

impl Answer {
    /// The wire value of this answer's `type` field.
    #[must_use]
    pub const fn tag(&self) -> &'static str {
        match self {
            Self::Noul(_) => "noul",
            Self::Choice(_) => "choice",
            Self::Score(_) => "score",
        }
    }

    /// The payload, if this is a Noul answer.
    #[must_use]
    pub const fn as_noul(&self) -> Option<&NoulAnswer> {
        match self {
            Self::Noul(a) => Some(a),
            _ => None,
        }
    }

    /// The payload, if this is a Choice answer.
    #[must_use]
    pub const fn as_choice(&self) -> Option<&ChoiceAnswer> {
        match self {
            Self::Choice(a) => Some(a),
            _ => None,
        }
    }

    /// The payload, if this is a Score answer.
    #[must_use]
    pub const fn as_score(&self) -> Option<&ScoreAnswer> {
        match self {
            Self::Score(a) => Some(a),
            _ => None,
        }
    }

    /// Model certainty, where the API reports one.
    ///
    /// Noul answers carry no `confidence` field — the value *is* the
    /// probability. Use [`NoulAnswer::certainty`] for a comparable figure.
    #[must_use]
    pub const fn confidence(&self) -> Option<f64> {
        match self {
            Self::Noul(_) => None,
            Self::Choice(a) => Some(a.confidence),
            Self::Score(a) => Some(a.confidence),
        }
    }

    /// The probability distribution, where the API reports one.
    #[must_use]
    pub const fn probabilities(&self) -> Option<&Map<f64>> {
        match self {
            Self::Noul(_) => None,
            Self::Choice(a) => Some(&a.probabilities),
            Self::Score(a) => Some(&a.probabilities),
        }
    }
}

/// A yes/no answer.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize),
    rkyv(derive(Debug))
)]
pub struct NoulAnswer {
    /// The answer on a scale from 0 (no) to 1 (yes).
    pub noul: f64,
}

impl NoulAnswer {
    /// `true` when the probability of yes is at or above `threshold`.
    #[must_use]
    pub fn is_yes(self, threshold: f64) -> bool {
        self.noul >= threshold
    }

    /// Distance from maximum uncertainty, rescaled to `0.0..=1.0`.
    ///
    /// A Noul carries no `confidence` field because the value already encodes
    /// it: 0.5 is a coin flip, and both 0.0 and 1.0 are fully certain.
    #[must_use]
    pub fn certainty(self) -> f64 {
        (self.noul - 0.5).abs() * 2.0
    }
}

/// One option selected from the set the question defined.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize),
    rkyv(derive(Debug))
)]
pub struct ChoiceAnswer {
    /// The highest-probability option.
    pub choice: String,
    /// Every option mapped to its probability. Sums to 1.
    pub probabilities: Map<f64>,
    /// Model certainty in `0.0..=1.0`, derived from `probabilities`.
    pub confidence: f64,
}

impl ChoiceAnswer {
    /// The probability assigned to `option`, if it was one of the options.
    #[must_use]
    pub fn probability_of(&self, option: &str) -> Option<f64> {
        self.probabilities.get(option).copied()
    }
}

/// A rating along the rubric the question defined.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize),
    rkyv(derive(Debug))
)]
pub struct ScoreAnswer {
    /// The probability-weighted value across the levels. May land between them.
    pub score: f64,
    /// Level number → the description supplied for it.
    pub legend: Map<String>,
    /// Level number → probability. Sums to 1.
    pub probabilities: Map<f64>,
    /// Model certainty in `0.0..=1.0`, derived from `probabilities`.
    pub confidence: f64,
}

impl ScoreAnswer {
    /// The level the score rounds to.
    ///
    /// The raw [`score`](Self::score) is weighted across levels, so a value of
    /// `1.05` means "level 1, leaning 2". Rounding discards that nuance —
    /// prefer thresholds on `score` itself where the gradation matters.
    #[must_use]
    pub fn nearest_level(&self) -> u32 {
        self.score.round().max(0.0) as u32
    }

    /// The legend description for [`nearest_level`](Self::nearest_level).
    #[must_use]
    pub fn nearest_label(&self) -> Option<&str> {
        self.legend.get(&self.nearest_level().to_string()).map(String::as_str)
    }

    /// The description for `level`, if the legend defines it.
    #[must_use]
    pub fn label(&self, level: u32) -> Option<&str> {
        self.legend.get(&level.to_string()).map(String::as_str)
    }
}
