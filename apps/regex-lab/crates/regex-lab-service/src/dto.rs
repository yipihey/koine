//! The types that cross the wire: a verb's arguments and result.
//!
//! Offsets are byte offsets into the UTF-8 text, the way `regex` reports
//! them, so `text[start..end]` is the match. Lines and columns are 1-based,
//! the column counted in characters, for a person reading the text.
//!
//! A result that can refuse carries an [`Outcome`] flattened into its top
//! level: `ok`, and when refused `code`, `message`, and for a pattern that
//! does not parse `at` (where in the pattern) and `diagram` (the pattern
//! with the place marked, as `regex` prints it).

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Where a refusal points in the pattern.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PatternSpan {
    /// Byte offset of the start.
    pub start: u64,
    /// Byte offset just past the end.
    pub end: u64,
    /// 1-based line of the start (an `x`-mode pattern can have several).
    pub line: u64,
    /// 1-based column of the start, in characters.
    pub column: u64,
}

/// Whether a call was answered, and why not when it was refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Outcome {
    /// False when the call was refused; then `code` and `message` say why.
    pub ok: bool,
    /// A stable kebab-case reason, when refused: `invalid-pattern`,
    /// `pattern-too-big`, `invalid-argument`, `not-found`, `store-error`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// The reason in a sentence, when refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// For `invalid-pattern`: where in the pattern.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<PatternSpan>,
    /// For `invalid-pattern`: the pattern with the place marked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagram: Option<String>,
}

impl Default for Outcome {
    fn default() -> Self {
        Self::answered()
    }
}

impl Outcome {
    pub fn answered() -> Self {
        Self {
            ok: true,
            code: None,
            message: None,
            at: None,
            diagram: None,
        }
    }

    pub fn refused(code: &str, message: impl Into<String>) -> Self {
        Self {
            ok: false,
            code: Some(code.into()),
            message: Some(message.into()),
            at: None,
            diagram: None,
        }
    }
}

/// Every refusable result is its [`Outcome`] plus empty fields.
macro_rules! refusable {
    ($($result:ty),* $(,)?) => {$(
        impl From<Outcome> for $result {
            fn from(outcome: Outcome) -> Self {
                Self { outcome, ..Default::default() }
            }
        }
    )*};
}

/// A capture group of a pattern.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Group {
    /// The group's index; 0 is the whole match.
    pub index: u64,
    /// The group's name, for `(?P<name>…)` or `(?<name>…)`.
    pub name: Option<String>,
}

/// What `check` answers about a pattern.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Checked {
    #[serde(flatten)]
    pub outcome: Outcome,
    /// The pattern checked (a saved one's text, when `saved` named it).
    pub pattern: String,
    /// The flags it was compiled with, as letters.
    pub flags: String,
    /// How many groups a match has, the whole match included.
    pub group_count: u64,
    /// The group count when every match has every group, absent when some
    /// group is optional (`(a)?`, `a|(b)`).
    pub static_group_count: Option<u64>,
    /// Every group, the whole match (0) first.
    pub groups: Vec<Group>,
    /// The shortest match in bytes; absent when it cannot match at all.
    pub min_len: Option<u64>,
    /// The longest match in bytes; absent when there is no bound.
    pub max_len: Option<u64>,
    /// True when the pattern only ever matches one literal string.
    pub literal: bool,
    /// True when every match starts at the start of the text (`^` without
    /// `m`, or `\A`).
    pub anchored_start: bool,
    /// True when every match ends at the end of the text.
    pub anchored_end: bool,
}

/// One part of a pattern's syntax, as `explain` lists them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Part {
    /// Depth in the syntax tree; 0 is the top.
    pub depth: u64,
    /// What it is: `literal`, `class`, `group`, `repetition`, ...
    pub kind: String,
    /// The pattern text it spans.
    pub source: String,
    /// What it matches, in words.
    pub meaning: String,
    /// Byte offset of its start in the pattern.
    pub start: u64,
    /// Byte offset just past its end.
    pub end: u64,
}

/// What `explain` answers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Explained {
    #[serde(flatten)]
    pub outcome: Outcome,
    /// The pattern's parts, in order, each followed by the parts inside it.
    pub parts: Vec<Part>,
}

/// What `is-match` answers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Matched {
    #[serde(flatten)]
    pub outcome: Outcome,
    /// True when the pattern matches somewhere at or after `start`.
    pub matched: bool,
    /// Where the shortest match ends (byte offset), when it matched: the
    /// earliest point at which a match is known.
    pub shortest_end: Option<u64>,
}

/// One match in the text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Match {
    /// The match's number, from 0.
    pub index: u64,
    /// Byte offset of its start.
    pub start: u64,
    /// Byte offset just past its end.
    pub end: u64,
    /// 1-based line of the start.
    pub line: u64,
    /// 1-based column of the start, in characters.
    pub column: u64,
    /// The matched text.
    pub text: String,
}

/// What `find` answers: the first match, when there is one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Found {
    #[serde(flatten)]
    pub outcome: Outcome,
    /// True when there is a match at or after `start`.
    pub found: bool,
    pub start: Option<u64>,
    pub end: Option<u64>,
    pub line: Option<u64>,
    pub column: Option<u64>,
    pub text: Option<String>,
}

/// What `find-all` answers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Matches {
    #[serde(flatten)]
    pub outcome: Outcome,
    /// How many matches are listed.
    pub count: u64,
    /// True when there were more than `limit`.
    pub truncated: bool,
    /// The matches, in order.
    pub matches: Vec<Match>,
}

/// One group of one match.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Capture {
    /// Which match, from 0.
    pub match_index: u64,
    /// The group's index; 0 is the whole match.
    pub group: u64,
    /// The group's name, when it has one.
    pub name: Option<String>,
    /// False when the group took no part in this match (`(a)?` without an
    /// `a`); then the offsets and text are absent.
    pub matched: bool,
    pub start: Option<u64>,
    pub end: Option<u64>,
    pub text: Option<String>,
}

/// What `captures` answers: the groups of the first match.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Captured {
    #[serde(flatten)]
    pub outcome: Outcome,
    /// True when there is a match at or after `start`.
    pub found: bool,
    /// Every group of the match, the whole match (0) first.
    pub groups: Vec<Capture>,
    /// The named groups that matched: name to text.
    pub named: BTreeMap<String, String>,
}

/// What `captures-all` answers: every match, two ways.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CapturesAll {
    #[serde(flatten)]
    pub outcome: Outcome,
    /// How many matches are listed.
    pub count: u64,
    /// True when there were more than `limit`.
    pub truncated: bool,
    /// The keys of each record, in group order: `match`, `text`, then each
    /// group's name, or its index when it has none.
    pub columns: Vec<String>,
    /// One record per match: `match` (its number), `text` (the whole
    /// match), and one key per group; a group that took no part is "".
    /// The table to read when the groups are known.
    pub records: Vec<BTreeMap<String, String>>,
    /// One row per group per match: the table to read when they are not.
    pub groups: Vec<Capture>,
}

/// What `replace` answers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Replaced {
    #[serde(flatten)]
    pub outcome: Outcome,
    /// The text after replacing.
    pub text: String,
    /// How many matches were replaced.
    pub replacements: u64,
}

/// A piece of the text between matches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Piece {
    /// The piece's number, from 0.
    pub index: u64,
    /// Byte offset of its start.
    pub start: u64,
    /// Byte offset just past its end.
    pub end: u64,
    pub text: String,
}

/// What `split` answers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Split {
    #[serde(flatten)]
    pub outcome: Outcome,
    /// How many pieces.
    pub count: u64,
    /// The pieces, in order; one more than the matches, empty ones included.
    pub pieces: Vec<Piece>,
}

/// One pattern of a set, and whether it matched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SetRow {
    /// The pattern's position in `patterns`, from 0.
    pub index: u64,
    pub pattern: String,
    pub matched: bool,
}

/// What `set-matches` answers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SetMatched {
    #[serde(flatten)]
    pub outcome: Outcome,
    /// True when any pattern matched.
    pub matched_any: bool,
    /// True when every pattern matched.
    pub matched_all: bool,
    /// How many patterns matched.
    pub matched_count: u64,
    /// Every pattern, in the order given.
    pub patterns: Vec<SetRow>,
}

/// What `escape` answers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Escaped {
    /// The text with every meta character escaped: a pattern matching
    /// exactly the text.
    pub escaped: String,
}

/// A saved pattern: a record of kind `regex-pattern`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SavedPattern {
    /// Its name: lowercase letters, digits, `-` and `_`. Matching verbs take
    /// it as `saved`.
    pub name: String,
    pub pattern: String,
    /// The flags it is compiled with, as letters.
    pub flags: String,
    /// What it is for.
    pub description: String,
    /// A text it should match.
    pub sample: String,
}

/// What `save-pattern` answers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Saved {
    #[serde(flatten)]
    pub outcome: Outcome,
    /// True when no pattern had this name before.
    pub created: bool,
    /// The pattern as stored.
    pub saved: Option<SavedPattern>,
}

/// What `delete-pattern` answers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Deleted {
    #[serde(flatten)]
    pub outcome: Outcome,
    /// The name asked for.
    pub name: String,
    /// False when nothing was saved under it.
    pub deleted: bool,
}

refusable!(
    Checked,
    Explained,
    Matched,
    Found,
    Matches,
    Captured,
    CapturesAll,
    Replaced,
    Split,
    SetMatched,
    Saved,
    Deleted,
);
