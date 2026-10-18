//! The verbs. One `#[impress_method]` per capability of `regex`; the macro
//! derives the MCP tool, the CLI subcommand and the agent tool from each.
//! A verb's name is `<trait>_<method>` in kebab case:
//! `regex-lab-service_find-all`.
//!
//! The matching verbs take the pattern one of two ways: `pattern`, the regex
//! itself, or `saved`, the name of a pattern saved with `save-pattern`.
//! `flags` are the letters of `regex`'s inline flags (`imsxUR`, and `a` for
//! ASCII-only classes); for a saved pattern they replace its own.

use std::collections::BTreeMap;

use impress_service_core::async_trait;
use impress_service_macros::{impress_service, impress_service_impl};
// `#[impress_service]` consumes `#[impress_method]`, so the import reads as unused.
#[allow(unused_imports)]
use impress_service_macros::impress_method;

use regex_syntax::hir::Look;

use crate::compile::{self, compile_set, resolve, row_limit, start_offset, Flags, Lines};
use crate::dto::*;
use crate::explain;
use crate::library;

/// RegexLab: the capabilities of `regex`.
#[impress_service]
pub trait RegexLabService: Send + Sync + 'static {
    /// Check a pattern without running it: whether it compiles, its groups,
    /// the shortest and longest match it can make, whether it is a plain
    /// literal or anchored. Refuses `invalid-pattern` with `at` (line and
    /// column in the pattern) and `diagram` (the pattern with the place
    /// marked), and `pattern-too-big` past the compiled-size limit.
    #[impress_method]
    #[impress_example(
        name = "date",
        args = r#"{"pattern": "(?P<year>\\d{4})-(?P<month>\\d{2})"}"#,
        expect = r#"{"ok": true, "group_count": 3, "static_group_count": 3, "min_len": 7, "literal": false}"#
    )]
    // # covers: Regex, Regex::new, Error, Regex::as_str, Regex::capture_names, Regex::captures_len, Regex::static_captures_len, CaptureNames
    async fn check(
        &self,
        pattern: Option<String>,
        saved: Option<String>,
        flags: Option<String>,
    ) -> Checked;

    /// A pattern in words: one row per part of its syntax (a literal, a
    /// class, a group, a repetition, an alternation), each followed by the
    /// parts inside it with a greater `depth`. Refuses as `check` does.
    #[impress_method]
    #[impress_example(
        name = "number",
        args = r#"{"pattern": "^-?\\d+$"}"#,
        expect = r#"{"ok": true}"#
    )]
    async fn explain(
        &self,
        pattern: Option<String>,
        saved: Option<String>,
        flags: Option<String>,
    ) -> Explained;

    /// Whether the pattern matches the text anywhere at or after `start` (a
    /// byte offset, default 0), and where the shortest match ends: the
    /// fastest question to ask. Refuses a bad pattern, and a `start` past
    /// the text or inside a character.
    #[impress_method]
    #[impress_example(
        name = "word",
        args = r#"{"pattern": "\\bcat\\b", "haystack": "a cat sat"}"#,
        expect = r#"{"ok": true, "matched": true, "shortest_end": 5}"#
    )]
    // # covers: Regex::is_match, Regex::is_match_at, Regex::shortest_match, Regex::shortest_match_at
    async fn is_match(
        &self,
        pattern: Option<String>,
        saved: Option<String>,
        flags: Option<String>,
        haystack: String,
        start: Option<u64>,
    ) -> Matched;

    /// The first match at or after `start` (a byte offset, default 0): its
    /// offsets, line and column, and text. `found` is false when there is
    /// none, which is not a refusal.
    #[impress_method]
    #[impress_example(
        name = "number",
        args = r#"{"pattern": "\\d+", "haystack": "ab 42 c 7"}"#,
        expect = r#"{"found": true, "start": 3, "end": 5, "line": 1, "column": 4, "text": "42"}"#
    )]
    // # covers: Regex::find, Regex::find_at, Match, Match::start, Match::end, Match::as_str, Match::range, Match::len, Match::is_empty
    async fn find(
        &self,
        pattern: Option<String>,
        saved: Option<String>,
        flags: Option<String>,
        haystack: String,
        start: Option<u64>,
    ) -> Found;

    /// Every match, in order, without overlaps: offsets, line and column,
    /// text. At most `limit` (default 1000, at most 100 000); `truncated`
    /// says there were more.
    #[impress_method]
    #[impress_example(
        name = "numbers",
        args = r#"{"pattern": "\\d+", "haystack": "1 22\n333"}"#,
        expect = r#"{"count": 3, "truncated": false}"#
    )]
    // # covers: Regex::find_iter, Matches
    async fn find_all(
        &self,
        pattern: Option<String>,
        saved: Option<String>,
        flags: Option<String>,
        haystack: String,
        limit: Option<u64>,
    ) -> Matches;

    /// The capture groups of the first match at or after `start`: one row
    /// per group (a group that took no part has `matched: false`), and the
    /// named groups as a map.
    #[impress_method]
    #[impress_example(
        name = "key-value",
        args = r#"{"pattern": "(?P<key>\\w+)=(?P<value>\\w+)", "haystack": "a=1 b=2"}"#,
        expect = r#"{"found": true, "named": {"key": "a", "value": "1"}}"#
    )]
    // # covers: Regex::captures, Regex::captures_at, Captures, Captures::get, Captures::name, Captures::iter, Captures::len, Captures::get_match, Captures::extract, SubCaptureMatches
    async fn captures(
        &self,
        pattern: Option<String>,
        saved: Option<String>,
        flags: Option<String>,
        haystack: String,
        start: Option<u64>,
    ) -> Captured;

    /// The capture groups of every match, at most `limit` (default 1000):
    /// as `records`, one per match keyed by `columns` (`match`, `text`, then
    /// each group's name or index), the table for known groups; and as
    /// `groups`, one row per group per match, the table for unknown ones.
    #[impress_method]
    #[impress_example(
        name = "key-values",
        args = r#"{"pattern": "(?P<key>\\w+)=(?P<value>\\w+)", "haystack": "a=1 b=2"}"#,
        expect = r#"{"count": 2, "columns": ["match", "text", "key", "value"]}"#
    )]
    // # covers: Regex::captures_iter, CaptureMatches
    async fn captures_all(
        &self,
        pattern: Option<String>,
        saved: Option<String>,
        flags: Option<String>,
        haystack: String,
        limit: Option<u64>,
    ) -> CapturesAll;

    /// Replace matches with `replacement`, where `$1`, `${name}` and `$$`
    /// stand for a group, a named group and a dollar sign, unless `literal`
    /// is true. `limit` replaces only the first n matches (default: all).
    /// Answers the new text and how many matches were replaced.
    #[impress_method]
    #[impress_example(
        name = "reorder",
        args = r#"{"pattern": "(?P<y>\\d{4})-(?P<m>\\d{2})", "haystack": "on 2026-10", "replacement": "$m/$y"}"#,
        expect = r#"{"text": "on 10/2026", "replacements": 1}"#
    )]
    // # covers: Regex::replace, Regex::replace_all, Regex::replacen, Replacer, ReplacerRef, NoExpand, Captures::expand
    async fn replace(
        &self,
        pattern: Option<String>,
        saved: Option<String>,
        flags: Option<String>,
        haystack: String,
        replacement: String,
        limit: Option<u64>,
        literal: Option<bool>,
    ) -> Replaced;

    /// Split the text at every match: the pieces between, with offsets,
    /// empty ones included. `limit` stops at n pieces, the last holding the
    /// rest of the text.
    #[impress_method]
    #[impress_example(
        name = "commas",
        args = r#"{"pattern": ",\\s*", "haystack": "a, b,c"}"#,
        expect = r#"{"count": 3}"#
    )]
    // # covers: Regex::split, Regex::splitn, Split, SplitN
    async fn split(
        &self,
        pattern: Option<String>,
        saved: Option<String>,
        flags: Option<String>,
        haystack: String,
        limit: Option<u64>,
    ) -> Split;

    /// Which of several patterns match the text at or after `start` (a byte
    /// offset, default 0), in one pass over it: each pattern's row, and
    /// whether any or all matched. A pattern that does not compile is refused
    /// by its position.
    #[impress_method(effects())]
    #[impress_example(
        name = "kinds",
        args = r#"{"patterns": ["\\d+", "[a-z]+", "@"], "haystack": "abc 123"}"#,
        expect = r#"{"matched_any": true, "matched_count": 2}"#
    )]
    // # covers: RegexSet, RegexSet::new, RegexSet::is_match, RegexSet::is_match_at, RegexSet::matches, RegexSet::matches_at, RegexSet::len, RegexSet::is_empty, RegexSet::patterns, SetMatches, SetMatches::matched_any, SetMatches::matched_all, SetMatches::matched, SetMatches::len, SetMatches::iter, SetMatchesIter, SetMatchesIntoIter
    async fn set_matches(
        &self,
        patterns: Vec<String>,
        flags: Option<String>,
        haystack: String,
        start: Option<u64>,
    ) -> SetMatched;

    /// Escape every meta character of a text: the pattern that matches
    /// exactly that text.
    #[impress_method(effects())]
    #[impress_example(
        name = "sum",
        args = r#"{"text": "1+1=2?"}"#,
        expect = r#"{"escaped": "1\\+1=2\\?"}"#
    )]
    // # covers: escape
    async fn escape(&self, text: String) -> Escaped;

    /// Save a pattern under a name (lowercase letters, digits, `-`, `_`),
    /// replacing one of the same name; matching verbs then take the name as
    /// `saved`. Refuses a pattern that does not compile, and a `sample` it
    /// does not match (`sample-does-not-match`).
    #[impress_method(
        safety = mutating,
        idempotent = true,
        effects(reads = ["regex-pattern"], writes = ["regex-pattern"], reach = [])
    )]
    #[impress_example(
        name = "iso-date",
        args = r#"{"name": "iso-date", "pattern": "(?P<year>\\d{4})-(?P<month>\\d{2})-(?P<day>\\d{2})", "description": "An ISO 8601 calendar date", "sample": "2026-10-17"}"#,
        expect = r#"{"ok": true, "saved": {"name": "iso-date", "flags": ""}}"#
    )]
    async fn save_pattern(
        &self,
        name: String,
        pattern: String,
        flags: Option<String>,
        description: Option<String>,
        sample: Option<String>,
    ) -> Saved;

    /// The saved patterns, by name; `search` keeps those whose name,
    /// pattern or description holds it.
    #[impress_method(
        returns_kind = "regex-pattern",
        rows(id = "name", title = "name", columns = ["name", "pattern", "flags", "description"])
    )]
    #[impress_example(
        name = "none",
        args = r#"{"search": "no pattern is called this"}"#,
        expect = r#"[]"#
    )]
    async fn patterns(&self, search: Option<String>) -> Vec<SavedPattern>;

    /// Delete a saved pattern. Deleting a name nothing is saved under
    /// answers `deleted: false`, so calling twice is calling once.
    #[impress_method(
        safety = destructive,
        idempotent = true,
        effects(reads = ["regex-pattern"], writes = ["regex-pattern"], reach = [])
    )]
    #[impress_example(
        name = "absent",
        args = r#"{"name": "never-saved"}"#,
        expect = r#"{"ok": true, "deleted": false}"#
    )]
    async fn delete_pattern(&self, name: String) -> Deleted;
}

/// The implementation: stateless; compiled patterns are cached in
/// `compile`, saved ones are in the store.
#[derive(Clone, Default)]
pub struct DefaultRegexLabService;

impl DefaultRegexLabService {
    pub fn new() -> Self {
        Self
    }
}

/// `?` for verbs: a refusal becomes the verb's answer.
macro_rules! answer {
    ($result:expr) => {
        match $result {
            Ok(value) => value,
            Err(outcome) => return outcome.into(),
        }
    };
}

#[async_trait::async_trait]
impl RegexLabService for DefaultRegexLabService {
    async fn check(
        &self,
        pattern: Option<String>,
        saved: Option<String>,
        flags: Option<String>,
    ) -> Checked {
        let compiled = answer!(resolve(pattern, saved, flags));
        // What the parser knows of the pattern that `regex` does not say:
        // its match lengths, whether it is a literal, its anchors.
        let (min_len, max_len, literal, anchored_start, anchored_end) =
            match compiled.flags.syntax_parser().parse(&compiled.pattern) {
                Ok(hir) => {
                    let p = hir.properties();
                    (
                        p.minimum_len().map(|n| n as u64),
                        p.maximum_len().map(|n| n as u64),
                        p.is_literal(),
                        p.look_set_prefix().contains(Look::Start),
                        p.look_set_suffix().contains(Look::End),
                    )
                }
                Err(_) => (None, None, false, false, false),
            };
        Checked {
            outcome: Outcome::answered(),
            flags: compiled.flags.letters(),
            group_count: compiled.regex.captures_len() as u64,
            static_group_count: compiled.regex.static_captures_len().map(|n| n as u64),
            groups: compiled
                .regex
                .capture_names()
                .enumerate()
                .map(|(index, name)| Group {
                    index: index as u64,
                    name: name.map(str::to_string),
                })
                .collect(),
            min_len,
            max_len,
            literal,
            anchored_start,
            anchored_end,
            pattern: compiled.pattern,
        }
    }

    async fn explain(
        &self,
        pattern: Option<String>,
        saved: Option<String>,
        flags: Option<String>,
    ) -> Explained {
        let compiled = answer!(resolve(pattern, saved, flags));
        Explained {
            outcome: Outcome::answered(),
            parts: answer!(explain::explain(&compiled.pattern, compiled.flags)),
        }
    }

    async fn is_match(
        &self,
        pattern: Option<String>,
        saved: Option<String>,
        flags: Option<String>,
        haystack: String,
        start: Option<u64>,
    ) -> Matched {
        let compiled = answer!(resolve(pattern, saved, flags));
        let start = answer!(start_offset(&haystack, start));
        let shortest = compiled.regex.shortest_match_at(&haystack, start);
        Matched {
            outcome: Outcome::answered(),
            matched: shortest.is_some(),
            shortest_end: shortest.map(|end| end as u64),
        }
    }

    async fn find(
        &self,
        pattern: Option<String>,
        saved: Option<String>,
        flags: Option<String>,
        haystack: String,
        start: Option<u64>,
    ) -> Found {
        let compiled = answer!(resolve(pattern, saved, flags));
        let start = answer!(start_offset(&haystack, start));
        match compiled.regex.find_at(&haystack, start) {
            Some(m) => {
                let (line, column) = Lines::new(&haystack).at(m.start());
                Found {
                    outcome: Outcome::answered(),
                    found: true,
                    start: Some(m.start() as u64),
                    end: Some(m.end() as u64),
                    line: Some(line),
                    column: Some(column),
                    text: Some(m.as_str().to_string()),
                }
            }
            None => Found::default(),
        }
    }

    async fn find_all(
        &self,
        pattern: Option<String>,
        saved: Option<String>,
        flags: Option<String>,
        haystack: String,
        limit: Option<u64>,
    ) -> Matches {
        let compiled = answer!(resolve(pattern, saved, flags));
        let limit = row_limit(limit);
        let mut lines = Lines::new(&haystack);
        let mut matches = Vec::new();
        let mut truncated = false;
        for (index, m) in compiled.regex.find_iter(&haystack).enumerate() {
            if index == limit {
                truncated = true;
                break;
            }
            let (line, column) = lines.at(m.start());
            matches.push(Match {
                index: index as u64,
                start: m.start() as u64,
                end: m.end() as u64,
                line,
                column,
                text: m.as_str().to_string(),
            });
        }
        Matches {
            outcome: Outcome::answered(),
            count: matches.len() as u64,
            truncated,
            matches,
        }
    }

    async fn captures(
        &self,
        pattern: Option<String>,
        saved: Option<String>,
        flags: Option<String>,
        haystack: String,
        start: Option<u64>,
    ) -> Captured {
        let compiled = answer!(resolve(pattern, saved, flags));
        let start = answer!(start_offset(&haystack, start));
        let names: Vec<Option<&str>> = compiled.regex.capture_names().collect();
        match compiled.regex.captures_at(&haystack, start) {
            Some(caps) => {
                let groups = capture_rows(0, &caps, &names);
                let named = names
                    .iter()
                    .enumerate()
                    .filter_map(|(i, name)| {
                        Some(((*name)?.to_string(), caps.get(i)?.as_str().to_string()))
                    })
                    .collect();
                Captured {
                    outcome: Outcome::answered(),
                    found: true,
                    groups,
                    named,
                }
            }
            None => Captured::default(),
        }
    }

    async fn captures_all(
        &self,
        pattern: Option<String>,
        saved: Option<String>,
        flags: Option<String>,
        haystack: String,
        limit: Option<u64>,
    ) -> CapturesAll {
        let compiled = answer!(resolve(pattern, saved, flags));
        let limit = row_limit(limit);
        let names: Vec<Option<&str>> = compiled.regex.capture_names().collect();
        let mut columns = vec!["match".to_string(), "text".to_string()];
        for (index, name) in names.iter().enumerate().skip(1) {
            columns.push(match name {
                Some(name) if !matches!(*name, "match" | "text") => name.to_string(),
                Some(name) => format!("{name}#{index}"),
                None => index.to_string(),
            });
        }
        let mut records = Vec::new();
        let mut groups = Vec::new();
        let mut truncated = false;
        for (index, caps) in compiled.regex.captures_iter(&haystack).enumerate() {
            if index == limit {
                truncated = true;
                break;
            }
            let mut record = BTreeMap::new();
            record.insert("match".to_string(), index.to_string());
            for (group, column) in columns.iter().skip(1).enumerate() {
                let text = caps.get(group).map(|m| m.as_str()).unwrap_or("");
                record.insert(column.clone(), text.to_string());
            }
            records.push(record);
            groups.extend(capture_rows(index as u64, &caps, &names));
        }
        CapturesAll {
            outcome: Outcome::answered(),
            count: records.len() as u64,
            truncated,
            columns,
            records,
            groups,
        }
    }

    async fn replace(
        &self,
        pattern: Option<String>,
        saved: Option<String>,
        flags: Option<String>,
        haystack: String,
        replacement: String,
        limit: Option<u64>,
        literal: Option<bool>,
    ) -> Replaced {
        let compiled = answer!(resolve(pattern, saved, flags));
        let limit = limit.unwrap_or(0) as usize;
        let found = compiled.regex.find_iter(&haystack).count();
        let replacements = if limit == 0 { found } else { found.min(limit) };
        let text = if literal.unwrap_or(false) {
            compiled
                .regex
                .replacen(&haystack, limit, regex::NoExpand(&replacement))
        } else {
            compiled
                .regex
                .replacen(&haystack, limit, replacement.as_str())
        };
        Replaced {
            outcome: Outcome::answered(),
            text: text.into_owned(),
            replacements: replacements as u64,
        }
    }

    async fn split(
        &self,
        pattern: Option<String>,
        saved: Option<String>,
        flags: Option<String>,
        haystack: String,
        limit: Option<u64>,
    ) -> Split {
        let compiled = answer!(resolve(pattern, saved, flags));
        let base = haystack.as_ptr() as usize;
        let pieces: Vec<&str> = match limit {
            Some(n) => compiled
                .regex
                .splitn(&haystack, n.max(1) as usize)
                .collect(),
            None => compiled.regex.split(&haystack).collect(),
        };
        let pieces: Vec<Piece> = pieces
            .into_iter()
            .enumerate()
            .map(|(index, piece)| {
                // Each piece is a slice of `haystack`: its offset is where it
                // starts in it.
                let start = piece.as_ptr() as usize - base;
                Piece {
                    index: index as u64,
                    start: start as u64,
                    end: (start + piece.len()) as u64,
                    text: piece.to_string(),
                }
            })
            .collect();
        Split {
            outcome: Outcome::answered(),
            count: pieces.len() as u64,
            pieces,
        }
    }

    async fn set_matches(
        &self,
        patterns: Vec<String>,
        flags: Option<String>,
        haystack: String,
        start: Option<u64>,
    ) -> SetMatched {
        let flags = answer!(Flags::parse(flags.as_deref().unwrap_or("")));
        let set = answer!(compile_set(&patterns, flags));
        let start = answer!(start_offset(&haystack, start));
        let matched = set.matches_at(&haystack, start);
        let rows: Vec<SetRow> = patterns
            .iter()
            .enumerate()
            .map(|(index, pattern)| SetRow {
                index: index as u64,
                pattern: pattern.clone(),
                matched: matched.matched(index),
            })
            .collect();
        SetMatched {
            outcome: Outcome::answered(),
            matched_any: matched.matched_any(),
            matched_all: matched.matched_all(),
            matched_count: matched.iter().count() as u64,
            patterns: rows,
        }
    }

    async fn escape(&self, text: String) -> Escaped {
        Escaped {
            escaped: regex::escape(&text),
        }
    }

    async fn save_pattern(
        &self,
        name: String,
        pattern: String,
        flags: Option<String>,
        description: Option<String>,
        sample: Option<String>,
    ) -> Saved {
        let name = answer!(library::check_name(&name));
        let flags = answer!(Flags::parse(flags.as_deref().unwrap_or("")));
        let compiled = answer!(compile::compile(&pattern, flags));
        let sample = sample.unwrap_or_default();
        if !sample.is_empty() && !compiled.regex.is_match(&sample) {
            return Outcome::refused(
                "sample-does-not-match",
                format!("the pattern does not match its sample {sample:?}, so it was not saved"),
            )
            .into();
        }
        let saved = SavedPattern {
            name,
            pattern,
            flags: flags.letters(),
            description: description.unwrap_or_default(),
            sample,
        };
        let created = answer!(library::put(&saved));
        Saved {
            outcome: Outcome::answered(),
            created,
            saved: Some(saved),
        }
    }

    async fn patterns(&self, search: Option<String>) -> Vec<SavedPattern> {
        library::list(search.as_deref()).unwrap_or_default()
    }

    async fn delete_pattern(&self, name: String) -> Deleted {
        let name = answer!(library::check_name(&name));
        let deleted = match library::delete(&name) {
            Ok(()) => true,
            Err(outcome) if outcome.code.as_deref() == Some("not-found") => false,
            Err(outcome) => return outcome.into(),
        };
        Deleted {
            outcome: Outcome::answered(),
            name,
            deleted,
        }
    }
}

/// One row per group of one match, the whole match (0) first.
fn capture_rows(
    match_index: u64,
    caps: &regex::Captures<'_>,
    names: &[Option<&str>],
) -> Vec<Capture> {
    names
        .iter()
        .enumerate()
        .map(|(group, name)| {
            let m = caps.get(group);
            Capture {
                match_index,
                group: group as u64,
                name: name.map(str::to_string),
                matched: m.is_some(),
                start: m.map(|m| m.start() as u64),
                end: m.map(|m| m.end() as u64),
                text: m.map(|m| m.as_str().to_string()),
            }
        })
        .collect()
}

impress_service_impl! {
    service = RegexLabService,
    impl = DefaultRegexLabService,
    instance = DefaultRegexLabService::new,
    // Matching changes nothing; saving and deleting say so on the trait.
    safety = read_only,
    since = "0.1.0",
    // A matching verb given `saved` reads that saved pattern; nothing leaves
    // the process.
    effects = { reads: ["regex-pattern"], writes: [], reach: [] },
    methods = [
        /// Check a pattern without running it.
        check(
            /// The regex. Give it or `saved`.
            pattern: Option<String>,
            /// The name of a saved pattern, instead of `pattern`.
            saved: Option<String>,
            /// Flag letters: i case-insensitive, m multi-line, s dot matches
            /// newline, x ignore whitespace, U swap greed, R CRLF, a ASCII.
            flags: Option<String>,
        ) -> Checked,
        /// A pattern in words.
        explain(
            /// The regex. Give it or `saved`.
            pattern: Option<String>,
            /// The name of a saved pattern, instead of `pattern`.
            saved: Option<String>,
            /// Flag letters (imsxURa).
            flags: Option<String>,
        ) -> Explained,
        /// Whether the pattern matches.
        is_match(
            /// The regex. Give it or `saved`.
            pattern: Option<String>,
            /// The name of a saved pattern, instead of `pattern`.
            saved: Option<String>,
            /// Flag letters (imsxURa).
            flags: Option<String>,
            /// The text to search.
            haystack: String,
            /// Byte offset to search from. Defaults to 0.
            start: Option<u64>,
        ) -> Matched,
        /// The first match.
        find(
            /// The regex. Give it or `saved`.
            pattern: Option<String>,
            /// The name of a saved pattern, instead of `pattern`.
            saved: Option<String>,
            /// Flag letters (imsxURa).
            flags: Option<String>,
            /// The text to search.
            haystack: String,
            /// Byte offset to search from. Defaults to 0.
            start: Option<u64>,
        ) -> Found,
        /// Every match.
        find_all(
            /// The regex. Give it or `saved`.
            pattern: Option<String>,
            /// The name of a saved pattern, instead of `pattern`.
            saved: Option<String>,
            /// Flag letters (imsxURa).
            flags: Option<String>,
            /// The text to search.
            haystack: String,
            /// The most matches to list. Defaults to 1000.
            limit: Option<u64>,
        ) -> Matches,
        /// The groups of the first match.
        captures(
            /// The regex. Give it or `saved`.
            pattern: Option<String>,
            /// The name of a saved pattern, instead of `pattern`.
            saved: Option<String>,
            /// Flag letters (imsxURa).
            flags: Option<String>,
            /// The text to search.
            haystack: String,
            /// Byte offset to search from. Defaults to 0.
            start: Option<u64>,
        ) -> Captured,
        /// The groups of every match.
        captures_all(
            /// The regex. Give it or `saved`.
            pattern: Option<String>,
            /// The name of a saved pattern, instead of `pattern`.
            saved: Option<String>,
            /// Flag letters (imsxURa).
            flags: Option<String>,
            /// The text to search.
            haystack: String,
            /// The most matches to list. Defaults to 1000.
            limit: Option<u64>,
        ) -> CapturesAll,
        /// Replace matches.
        replace(
            /// The regex. Give it or `saved`.
            pattern: Option<String>,
            /// The name of a saved pattern, instead of `pattern`.
            saved: Option<String>,
            /// Flag letters (imsxURa).
            flags: Option<String>,
            /// The text to change.
            haystack: String,
            /// What a match becomes; `$1`, `${name}`, `$$` unless `literal`.
            replacement: String,
            /// Replace only the first n matches. Defaults to all.
            limit: Option<u64>,
            /// Take `replacement` as it is, `$` included. Defaults to false.
            literal: Option<bool>,
        ) -> Replaced,
        /// Split at every match.
        split(
            /// The regex. Give it or `saved`.
            pattern: Option<String>,
            /// The name of a saved pattern, instead of `pattern`.
            saved: Option<String>,
            /// Flag letters (imsxURa).
            flags: Option<String>,
            /// The text to split.
            haystack: String,
            /// The most pieces; the last holds the rest. Defaults to no limit.
            limit: Option<u64>,
        ) -> Split,
        /// Which of several patterns match.
        set_matches(
            /// The regexes, each on its own.
            patterns: Vec<String>,
            /// Flag letters (imsxURa), for every pattern.
            flags: Option<String>,
            /// The text to search.
            haystack: String,
            /// Byte offset to search from. Defaults to 0.
            start: Option<u64>,
        ) -> SetMatched,
        /// Escape a text into a pattern.
        escape(
            /// The text to match exactly.
            text: String,
        ) -> Escaped,
        /// Save a pattern under a name.
        save_pattern(
            /// The name: lowercase letters, digits, `-`, `_`.
            name: String,
            /// The regex.
            pattern: String,
            /// Flag letters (imsxURa).
            flags: Option<String>,
            /// What it is for.
            description: Option<String>,
            /// A text it must match; checked before saving.
            sample: Option<String>,
        ) -> Saved,
        /// The saved patterns.
        patterns(
            /// Keep those whose name, pattern or description holds this.
            search: Option<String>,
        ) -> Vec<SavedPattern>,
        /// Delete a saved pattern.
        delete_pattern(
            /// The saved pattern's name.
            name: String,
        ) -> Deleted,
    ],
}
