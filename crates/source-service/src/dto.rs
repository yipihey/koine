//! The types that cross the wire for the six verbs (S-D3). A result that can
//! refuse carries `ok`, and when refused only `ok`, `code` and `message`
//! (docs/capability.md § Refusing); the answer fields are a flattened
//! `Option` beside the outcome.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use koine_sources::{Diagnostic, ExplainRecord, Work};

use crate::adapter::ExportFormat;

/// `ok`, and when refused `code` and `message`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Outcome {
    /// False when the call was refused; then `code` and `message` say why.
    pub ok: bool,
    /// A stable kebab-case reason (`invalid-argument`, `not-found`), when refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// The reason in a sentence, when refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl Outcome {
    pub fn ok() -> Self {
        Outcome {
            ok: true,
            code: None,
            message: None,
        }
    }

    pub fn refused(code: &str, message: impl Into<String>) -> Self {
        Outcome {
            ok: false,
            code: Some(code.into()),
            message: Some(message.into()),
        }
    }
}

macro_rules! refusable {
    ($(#[$doc:meta])* $name:ident, $answer:ty) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
        pub struct $name {
            #[serde(flatten)]
            pub outcome: Outcome,
            #[serde(flatten)]
            pub answer: Option<$answer>,
        }

        impl $name {
            pub fn answer(answer: $answer) -> Self {
                $name {
                    outcome: Outcome::ok(),
                    answer: Some(answer),
                }
            }

            pub fn refused(code: &str, message: impl Into<String>) -> Self {
                $name {
                    outcome: Outcome::refused(code, message),
                    answer: None,
                }
            }
        }
    };
}

/// The inventory (S-D9): every linked adapter's explain record.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExplainAnswer {
    /// The contact the adapters send (polite pools); null when the host set
    /// none and `KOINE_SOURCE_CONTACT` is unset.
    #[serde(default)]
    pub contact: Option<String>,
    pub sources: Vec<ExplainRecord>,
}

refusable!(
    /// Result of `explain`.
    ExplainResult,
    ExplainAnswer
);

/// The L-D5 envelope with diagnostics (S-D3, S-D7).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RowsAnswer {
    /// Canonical records (`bibliographic-work@1.0.0`), each with its `ids`
    /// and a `provenance` naming the source and `source_id` it came from.
    pub rows: Vec<Work>,
    /// Pass back as `cursor` for the next page; null at the end.
    #[serde(default)]
    pub next: Option<String>,
    /// Every source's outcome: `done`, `refused`, `timed_out`, `unsupported`
    /// (with the index in `detail`), `rate_limited`, `not_found` (the id in
    /// `detail`), `error` (the failure in `detail`).
    #[serde(default)]
    pub diagnostics: Vec<Diagnostic>,
}

refusable!(
    /// Result of `search`.
    SearchResult,
    RowsAnswer
);

refusable!(
    /// Result of `related`.
    RelatedResult,
    RowsAnswer
);

/// The merged record (S-D7) with every source's outcome.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FetchAnswer {
    pub work: Work,
    #[serde(default)]
    pub diagnostics: Vec<Diagnostic>,
}

refusable!(
    /// Result of `fetch`.
    FetchResult,
    FetchAnswer
);

/// What `match` is given: a free string, or the structured fields of a
/// reference (S-D3). At least one of `text`, `title`, `doi`, `arxiv` is
/// needed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MatchInput {
    /// The reference as written (`Abel, Bryan & Norman 2002, Science 295, 93`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Family names, first author first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub authors: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<i32>,
    /// Journal, venue or bibstem.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doi: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arxiv: Option<String>,
}

/// One candidate of `match` (S-D6): the record, its score and the evidence
/// that produced it. `score` is null for an unscored candidate (the scorer
/// is a later package; an identifier hit scores 1).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Candidate {
    pub work: Work,
    #[serde(default)]
    pub score: Option<f64>,
    /// Per-field contributions (`{"identifier": 1.0}`).
    #[serde(default)]
    pub evidence: BTreeMap<String, f64>,
    /// How it was found: `identifier`, `local`, `search`.
    pub via: String,
}

/// The candidates, the best one when a rule accepted it, and which rule.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MatchAnswer {
    pub candidates: Vec<Candidate>,
    /// Index into `candidates` of the accepted one; null below threshold.
    #[serde(default)]
    pub best: Option<u32>,
    /// True when `best` cleared a rule; false means a person or an agent chooses.
    pub accepted: bool,
    /// The acceptance rule that fired (`identifier`), when one did.
    #[serde(default)]
    pub rule: Option<String>,
    #[serde(default)]
    pub diagnostics: Vec<Diagnostic>,
}

refusable!(
    /// Result of `match`.
    MatchResult,
    MatchAnswer
);

/// The exported text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExportAnswer {
    pub format: ExportFormat,
    /// BibTeX or RIS entries separated by a blank line; a JSON array for `csl-json`.
    pub text: String,
    /// How many of `ids` the text covers.
    pub count: u32,
    #[serde(default)]
    pub diagnostics: Vec<Diagnostic>,
}

impl Default for ExportAnswer {
    fn default() -> Self {
        ExportAnswer {
            format: ExportFormat::Bibtex,
            text: String::new(),
            count: 0,
            diagnostics: Vec::new(),
        }
    }
}

refusable!(
    /// Result of `export`.
    ExportResult,
    ExportAnswer
);
