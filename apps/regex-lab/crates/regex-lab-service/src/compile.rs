//! From a pattern and its flags to a compiled `Regex`, or a refusal that
//! says where the pattern is wrong.
//!
//! A compiled pattern is kept in a bounded process-wide cache keyed by the
//! pattern and its flags, so a surface that re-runs five verbs over the same
//! pattern on every keystroke compiles it once. A `Regex` is cheap to clone
//! (it shares its compiled program), so a hit is a clone.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use regex::{Regex, RegexBuilder, RegexSet, RegexSetBuilder};

use crate::dto::{Outcome, PatternSpan};
use crate::library;

/// The flags a pattern is compiled with: the letters of `regex`'s inline
/// flags (`(?imsxUR)`), plus `a` for ASCII-only classes (Unicode off).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Flags {
    pub case_insensitive: bool,
    pub multi_line: bool,
    pub dot_matches_new_line: bool,
    pub ignore_whitespace: bool,
    pub swap_greed: bool,
    pub crlf: bool,
    pub ascii: bool,
}

/// The letters `flags` takes, in the order [`Flags::letters`] writes them.
pub const FLAG_LETTERS: &str = "imsxURa";

impl Flags {
    /// Parse flag letters (`"im"`). Refuses a letter that is not one of
    /// [`FLAG_LETTERS`].
    pub fn parse(letters: &str) -> Result<Self, Outcome> {
        let mut flags = Flags::default();
        for letter in letters.chars().filter(|c| !c.is_whitespace()) {
            match letter {
                'i' => flags.case_insensitive = true,
                'm' => flags.multi_line = true,
                's' => flags.dot_matches_new_line = true,
                'x' => flags.ignore_whitespace = true,
                'U' => flags.swap_greed = true,
                'R' => flags.crlf = true,
                'a' => flags.ascii = true,
                other => {
                    return Err(Outcome::refused(
                        "invalid-argument",
                        format!(
                            "flag '{other}' is not one of {FLAG_LETTERS}: i case-insensitive, \
                             m multi-line, s dot matches newline, x ignore whitespace, \
                             U swap greed, R CRLF line ends, a ASCII-only classes"
                        ),
                    ))
                }
            }
        }
        Ok(flags)
    }

    /// The flags as letters, in a fixed order: the spelling stored and
    /// answered.
    pub fn letters(&self) -> String {
        [
            (self.case_insensitive, 'i'),
            (self.multi_line, 'm'),
            (self.dot_matches_new_line, 's'),
            (self.ignore_whitespace, 'x'),
            (self.swap_greed, 'U'),
            (self.crlf, 'R'),
            (self.ascii, 'a'),
        ]
        .iter()
        .filter(|(on, _)| *on)
        .map(|(_, letter)| *letter)
        .collect()
    }

    fn builder(&self, pattern: &str) -> RegexBuilder {
        let mut builder = RegexBuilder::new(pattern);
        builder
            .case_insensitive(self.case_insensitive)
            .multi_line(self.multi_line)
            .dot_matches_new_line(self.dot_matches_new_line)
            .ignore_whitespace(self.ignore_whitespace)
            .swap_greed(self.swap_greed)
            .crlf(self.crlf)
            .unicode(!self.ascii);
        builder
    }

    /// A `regex-syntax` parser configured as [`Flags::builder`] configures
    /// `regex`, so its errors and properties are of the same pattern.
    pub fn syntax_parser(&self) -> regex_syntax::Parser {
        regex_syntax::ParserBuilder::new()
            .case_insensitive(self.case_insensitive)
            .multi_line(self.multi_line)
            .dot_matches_new_line(self.dot_matches_new_line)
            .ignore_whitespace(self.ignore_whitespace)
            .swap_greed(self.swap_greed)
            .crlf(self.crlf)
            .unicode(!self.ascii)
            .utf8(true)
            .build()
    }
}

/// A pattern ready to run: its text, its flags and the compiled `Regex`.
#[derive(Debug, Clone)]
pub struct Compiled {
    pub pattern: String,
    pub flags: Flags,
    pub regex: Regex,
}

const CACHE_CAPACITY: usize = 256;

fn cache() -> &'static Mutex<HashMap<(String, Flags), Regex>> {
    static CACHE: OnceLock<Mutex<HashMap<(String, Flags), Regex>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The refusal for a pattern that does not compile: `invalid-pattern` with
/// the place for a syntax error, `pattern-too-big` past the size limit.
fn refusal(pattern: &str, flags: Flags, error: regex::Error) -> Outcome {
    match error {
        regex::Error::CompiledTooBig(limit) => Outcome::refused(
            "pattern-too-big",
            format!(
                "the compiled pattern is larger than the {limit}-byte limit; \
                 shorten its repetitions (a{{1000}}) or its classes"
            ),
        ),
        regex::Error::Syntax(diagram) => {
            // `regex` reports a syntax error as text. The same parser, run on
            // its own, says where.
            let (message, at) = match flags.syntax_parser().parse(pattern) {
                Err(regex_syntax::Error::Parse(e)) => (e.kind().to_string(), Some(span(e.span()))),
                Err(regex_syntax::Error::Translate(e)) => {
                    (e.kind().to_string(), Some(span(e.span())))
                }
                _ => (diagram.lines().last().unwrap_or("").to_string(), None),
            };
            let place = at
                .as_ref()
                .map(|s| format!(" at line {}, column {}", s.line, s.column))
                .unwrap_or_default();
            let mut outcome = Outcome::refused("invalid-pattern", format!("{message}{place}"));
            outcome.at = at;
            outcome.diagram = Some(diagram);
            outcome
        }
        other => Outcome::refused("invalid-pattern", other.to_string()),
    }
}

fn span(span: &regex_syntax::ast::Span) -> PatternSpan {
    PatternSpan {
        start: span.start.offset as u64,
        end: span.end.offset as u64,
        line: span.start.line as u64,
        column: span.start.column as u64,
    }
}

/// Compile `pattern` with `flags`, from the cache when it was compiled
/// before.
pub fn compile(pattern: &str, flags: Flags) -> Result<Compiled, Outcome> {
    let key = (pattern.to_string(), flags);
    if let Some(regex) = cache().lock().ok().and_then(|c| c.get(&key).cloned()) {
        return Ok(Compiled {
            pattern: key.0,
            flags,
            regex,
        });
    }
    let regex = flags
        .builder(pattern)
        .build()
        .map_err(|e| refusal(pattern, flags, e))?;
    if let Ok(mut cache) = cache().lock() {
        if cache.len() >= CACHE_CAPACITY {
            cache.clear();
        }
        cache.insert(key.clone(), regex.clone());
    }
    Ok(Compiled {
        pattern: key.0,
        flags,
        regex,
    })
}

/// The pattern a matching verb runs: `pattern` itself, or the saved pattern
/// `saved` names. Exactly one of the two. `flags` overrides a saved
/// pattern's own.
pub fn resolve(
    pattern: Option<String>,
    saved: Option<String>,
    flags: Option<String>,
) -> Result<Compiled, Outcome> {
    let pattern = pattern.filter(|p| !p.is_empty());
    let saved = saved
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let (text, stored_flags) = match (pattern, saved) {
        (Some(_), Some(name)) => {
            return Err(Outcome::refused(
                "invalid-argument",
                format!("give `pattern` or `saved`, not both (saved: {name})"),
            ))
        }
        (None, None) => {
            return Err(Outcome::refused(
                "invalid-argument",
                "give `pattern` (the regex) or `saved` (a saved pattern's name)",
            ))
        }
        (Some(pattern), None) => (pattern, String::new()),
        (None, Some(name)) => {
            let saved = library::get(&name)?;
            (saved.pattern, saved.flags)
        }
    };
    let flags = Flags::parse(flags.as_deref().unwrap_or(&stored_flags))?;
    compile(&text, flags)
}

/// A set of patterns, compiled together. A pattern that does not compile is
/// refused by its position in the list.
pub fn compile_set(patterns: &[String], flags: Flags) -> Result<RegexSet, Outcome> {
    if patterns.is_empty() {
        return Err(Outcome::refused(
            "invalid-argument",
            "`patterns` is empty: give at least one",
        ));
    }
    for (index, pattern) in patterns.iter().enumerate() {
        if let Err(mut outcome) = compile(pattern, flags) {
            outcome.message = outcome
                .message
                .map(|m| format!("pattern {index} ({pattern:?}): {m}"));
            return Err(outcome);
        }
    }
    RegexSetBuilder::new(patterns)
        .case_insensitive(flags.case_insensitive)
        .multi_line(flags.multi_line)
        .dot_matches_new_line(flags.dot_matches_new_line)
        .ignore_whitespace(flags.ignore_whitespace)
        .swap_greed(flags.swap_greed)
        .crlf(flags.crlf)
        .unicode(!flags.ascii)
        .build()
        .map_err(|e| match e {
            regex::Error::CompiledTooBig(limit) => Outcome::refused(
                "pattern-too-big",
                format!("the set compiles larger than the {limit}-byte limit"),
            ),
            other => Outcome::refused("invalid-pattern", other.to_string()),
        })
}

/// Line and column (1-based, the column in characters) of byte offsets into
/// one text, for offsets asked in increasing order: each call continues from
/// the last, so a whole list of matches is one pass over the text.
pub struct Lines<'a> {
    text: &'a str,
    at: usize,
    line: u64,
    line_start: usize,
}

impl<'a> Lines<'a> {
    pub fn new(text: &'a str) -> Self {
        Self {
            text,
            at: 0,
            line: 1,
            line_start: 0,
        }
    }

    /// The line and column of `offset`. An offset before the last one asked
    /// starts again from the top.
    pub fn at(&mut self, offset: usize) -> (u64, u64) {
        let offset = offset.min(self.text.len());
        if offset < self.at {
            *self = Self::new(self.text);
        }
        for (i, byte) in self.text.as_bytes()[self.at..offset].iter().enumerate() {
            if *byte == b'\n' {
                self.line += 1;
                self.line_start = self.at + i + 1;
            }
        }
        self.at = offset;
        let column = self.text[self.line_start..offset].chars().count() as u64 + 1;
        (self.line, column)
    }
}

/// The limit on listed rows: `limit`, else 1000, at most 100 000.
pub fn row_limit(limit: Option<u64>) -> usize {
    limit.unwrap_or(1000).clamp(1, 100_000) as usize
}

/// A start offset inside `haystack`, on a character boundary.
pub fn start_offset(haystack: &str, start: Option<u64>) -> Result<usize, Outcome> {
    let start = start.unwrap_or(0) as usize;
    if start > haystack.len() {
        return Err(Outcome::refused(
            "invalid-argument",
            format!(
                "`start` {start} is past the end of the text ({} bytes)",
                haystack.len()
            ),
        ));
    }
    if !haystack.is_char_boundary(start) {
        return Err(Outcome::refused(
            "invalid-argument",
            format!("`start` {start} is inside a character; give the byte offset of one"),
        ));
    }
    Ok(start)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_round_trip_in_a_fixed_order() {
        let flags = Flags::parse("x i m").unwrap();
        assert_eq!(flags.letters(), "imx");
        assert_eq!(Flags::parse("").unwrap(), Flags::default());
        let refused = Flags::parse("ig").unwrap_err();
        assert_eq!(refused.code.as_deref(), Some("invalid-argument"));
        assert!(refused.message.unwrap().contains("'g'"));
    }

    #[test]
    fn a_syntax_error_says_where() {
        let refused = compile("(ab", Flags::default()).unwrap_err();
        assert_eq!(refused.code.as_deref(), Some("invalid-pattern"));
        let at = refused.at.unwrap();
        assert_eq!((at.start, at.line, at.column), (0, 1, 1));
        assert!(refused.message.unwrap().contains("unclosed group"));
        assert!(refused.diagram.unwrap().contains('^'));
    }

    #[test]
    fn an_error_in_translation_says_where_too() {
        // `\p{Greek}` parses, but not without Unicode.
        let refused = compile(r"x\p{Greek}", Flags::parse("a").unwrap()).unwrap_err();
        let at = refused.at.unwrap();
        assert_eq!((at.start, at.column), (1, 2));
    }

    #[test]
    fn a_compiled_pattern_comes_from_the_cache() {
        let a = compile("cache-me[0-9]+", Flags::default()).unwrap();
        let b = compile("cache-me[0-9]+", Flags::default()).unwrap();
        assert_eq!(a.regex.as_str(), b.regex.as_str());
        let key = ("cache-me[0-9]+".to_string(), Flags::default());
        assert!(cache().lock().unwrap().contains_key(&key));
    }

    #[test]
    fn lines_count_from_one_in_characters() {
        let text = "ab\ncdé\nf";
        let mut lines = Lines::new(text);
        assert_eq!(lines.at(0), (1, 1));
        assert_eq!(lines.at(4), (2, 2));
        assert_eq!(lines.at(text.len() - 1), (3, 1));
        // Backwards starts again.
        assert_eq!(lines.at(1), (1, 2));
    }

    #[test]
    fn a_start_inside_a_character_is_refused() {
        assert!(start_offset("é", Some(1)).is_err());
        assert_eq!(start_offset("é", Some(2)).unwrap(), 2);
        assert!(start_offset("é", Some(3)).is_err());
    }
}
