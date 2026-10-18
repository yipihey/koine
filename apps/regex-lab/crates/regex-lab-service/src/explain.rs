//! A pattern in words: its syntax tree (`regex_syntax::ast`), one row per
//! part, each followed by the parts inside it.
//!
//! A run of plain characters is one row (`the text "abc"`), not a row per
//! character: that is how a person reads it.

use regex_syntax::ast::{self, Ast};

use crate::compile::Flags;
use crate::dto::{Outcome, Part};

/// The parts of `pattern`. The pattern is checked by `compile` first, so a
/// parse error here is reported as `regex` would.
pub fn explain(pattern: &str, flags: Flags) -> Result<Vec<Part>, Outcome> {
    let ast = ast::parse::ParserBuilder::new()
        .ignore_whitespace(flags.ignore_whitespace)
        .build()
        .parse(pattern)
        .map_err(|e| Outcome::refused("invalid-pattern", e.kind().to_string()))?;
    let mut parts = Vec::new();
    let mut groups = 0u32;
    walk(&ast, pattern, 0, &mut groups, &mut parts);
    Ok(parts)
}

fn push(
    parts: &mut Vec<Part>,
    pattern: &str,
    depth: u64,
    span: &ast::Span,
    kind: &str,
    meaning: String,
) {
    parts.push(Part {
        depth,
        kind: kind.to_string(),
        source: pattern
            .get(span.start.offset..span.end.offset)
            .unwrap_or("")
            .to_string(),
        meaning,
        start: span.start.offset as u64,
        end: span.end.offset as u64,
    });
}

fn walk(ast: &Ast, pattern: &str, depth: u64, groups: &mut u32, parts: &mut Vec<Part>) {
    match ast {
        Ast::Empty(_) => {}
        Ast::Flags(flags) => push(
            parts,
            pattern,
            depth,
            &flags.span,
            "flags",
            format!(
                "turns flags on or off from here: {}",
                flag_words(&flags.flags)
            ),
        ),
        Ast::Literal(literal) => push(
            parts,
            pattern,
            depth,
            &literal.span,
            "literal",
            format!("the character {}", char_words(literal.c)),
        ),
        Ast::Dot(span) => push(
            parts,
            pattern,
            depth,
            span,
            "any",
            "any character except a newline (any at all with the s flag)".into(),
        ),
        Ast::Assertion(assertion) => push(
            parts,
            pattern,
            depth,
            &assertion.span,
            "assertion",
            assertion_words(&assertion.kind).into(),
        ),
        Ast::ClassUnicode(class) => {
            let name = match &class.kind {
                ast::ClassUnicodeKind::OneLetter(c) => c.to_string(),
                ast::ClassUnicodeKind::Named(name) => name.clone(),
                ast::ClassUnicodeKind::NamedValue { name, value, .. } => {
                    format!("{name}={value}")
                }
            };
            push(
                parts,
                pattern,
                depth,
                &class.span,
                "class",
                format!(
                    "a character {} the Unicode class {name}",
                    if class.negated { "not in" } else { "in" }
                ),
            )
        }
        Ast::ClassPerl(class) => {
            let what = match class.kind {
                ast::ClassPerlKind::Digit => "digit",
                ast::ClassPerlKind::Space => "whitespace character",
                ast::ClassPerlKind::Word => "word character (letter, digit or _)",
            };
            push(
                parts,
                pattern,
                depth,
                &class.span,
                "class",
                format!(
                    "{} {what}",
                    if class.negated { "anything but a" } else { "a" }
                ),
            )
        }
        Ast::ClassBracketed(class) => push(
            parts,
            pattern,
            depth,
            &class.span,
            "class",
            format!(
                "one character {} the set {}",
                if class.negated { "not in" } else { "in" },
                pattern
                    .get(class.span.start.offset..class.span.end.offset)
                    .unwrap_or("")
            ),
        ),
        Ast::Repetition(repetition) => {
            let times = match &repetition.op.kind {
                ast::RepetitionKind::ZeroOrOne => "optionally (0 or 1 times)".to_string(),
                ast::RepetitionKind::ZeroOrMore => "0 or more times".to_string(),
                ast::RepetitionKind::OneOrMore => "1 or more times".to_string(),
                ast::RepetitionKind::Range(ast::RepetitionRange::Exactly(n)) => {
                    format!("exactly {n} times")
                }
                ast::RepetitionKind::Range(ast::RepetitionRange::AtLeast(n)) => {
                    format!("{n} or more times")
                }
                ast::RepetitionKind::Range(ast::RepetitionRange::Bounded(m, n)) => {
                    format!("{m} to {n} times")
                }
            };
            let greed = if repetition.greedy {
                "as many as it can"
            } else {
                "as few as it can"
            };
            push(
                parts,
                pattern,
                depth,
                &repetition.span,
                "repetition",
                format!("what follows, {times}, {greed}"),
            );
            walk(&repetition.ast, pattern, depth + 1, groups, parts);
        }
        Ast::Group(group) => {
            let meaning = match &group.kind {
                ast::GroupKind::CaptureIndex(index) => {
                    *groups += 1;
                    format!("capture group {index}")
                }
                ast::GroupKind::CaptureName { name, .. } => {
                    *groups += 1;
                    format!("capture group {} named {}", name.index, name.name)
                }
                ast::GroupKind::NonCapturing(flags) if flags.items.is_empty() => {
                    "a group that captures nothing".to_string()
                }
                ast::GroupKind::NonCapturing(flags) => format!(
                    "a group that captures nothing, with flags {}",
                    flag_words(flags)
                ),
            };
            push(parts, pattern, depth, &group.span, "group", meaning);
            walk(&group.ast, pattern, depth + 1, groups, parts);
        }
        Ast::Alternation(alternation) => {
            push(
                parts,
                pattern,
                depth,
                &alternation.span,
                "alternation",
                format!(
                    "one of {} alternatives, tried in order",
                    alternation.asts.len()
                ),
            );
            for branch in &alternation.asts {
                walk(branch, pattern, depth + 1, groups, parts);
            }
        }
        Ast::Concat(concat) => {
            // Runs of plain characters read as one piece of text.
            let mut run: Vec<&ast::Literal> = Vec::new();
            for piece in &concat.asts {
                match piece {
                    Ast::Literal(literal) => run.push(literal),
                    other => {
                        flush(&mut run, pattern, depth, parts);
                        walk(other, pattern, depth, groups, parts);
                    }
                }
            }
            flush(&mut run, pattern, depth, parts);
        }
    }
}

fn flush(run: &mut Vec<&ast::Literal>, pattern: &str, depth: u64, parts: &mut Vec<Part>) {
    match run.len() {
        0 => {}
        1 => push(
            parts,
            pattern,
            depth,
            &run[0].span,
            "literal",
            format!("the character {}", char_words(run[0].c)),
        ),
        _ => {
            let span = ast::Span::new(run[0].span.start, run[run.len() - 1].span.end);
            let text: String = run.iter().map(|l| l.c).collect();
            push(
                parts,
                pattern,
                depth,
                &span,
                "literal",
                format!("the text {text:?}"),
            );
        }
    }
    run.clear();
}

fn char_words(c: char) -> String {
    match c {
        '\n' => "newline".into(),
        '\t' => "tab".into(),
        '\r' => "carriage return".into(),
        ' ' => "space".into(),
        c => format!("{c:?}"),
    }
}

fn assertion_words(kind: &ast::AssertionKind) -> &'static str {
    match kind {
        ast::AssertionKind::StartLine => "the start of the text (of a line, with the m flag)",
        ast::AssertionKind::EndLine => "the end of the text (of a line, with the m flag)",
        ast::AssertionKind::StartText => "the start of the text",
        ast::AssertionKind::EndText => "the end of the text",
        ast::AssertionKind::WordBoundary => "a word boundary",
        ast::AssertionKind::NotWordBoundary => "not a word boundary",
        ast::AssertionKind::WordBoundaryStart | ast::AssertionKind::WordBoundaryStartAngle => {
            "the start of a word"
        }
        ast::AssertionKind::WordBoundaryEnd | ast::AssertionKind::WordBoundaryEndAngle => {
            "the end of a word"
        }
        ast::AssertionKind::WordBoundaryStartHalf => "a place a word could start",
        ast::AssertionKind::WordBoundaryEndHalf => "a place a word could end",
    }
}

fn flag_words(flags: &ast::Flags) -> String {
    let mut words = Vec::new();
    let mut negated = false;
    for item in &flags.items {
        let word = match &item.kind {
            ast::FlagsItemKind::Negation => {
                negated = true;
                continue;
            }
            ast::FlagsItemKind::Flag(flag) => match flag {
                ast::Flag::CaseInsensitive => "case-insensitive",
                ast::Flag::MultiLine => "multi-line",
                ast::Flag::DotMatchesNewLine => "dot matches newline",
                ast::Flag::SwapGreed => "swap greed",
                ast::Flag::Unicode => "Unicode",
                ast::Flag::CRLF => "CRLF line ends",
                ast::Flag::IgnoreWhitespace => "ignore whitespace",
            },
        };
        words.push(if negated {
            format!("{word} off")
        } else {
            word.to_string()
        });
    }
    words.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(pattern: &str) -> Vec<(u64, String, String)> {
        explain(pattern, Flags::default())
            .unwrap()
            .into_iter()
            .map(|p| (p.depth, p.kind, p.source))
            .collect()
    }

    #[test]
    fn a_run_of_characters_is_one_literal() {
        assert_eq!(
            kinds(r"ab\d+"),
            vec![
                (0, "literal".into(), "ab".into()),
                (0, "repetition".into(), r"\d+".into()),
                (1, "class".into(), r"\d".into()),
            ]
        );
    }

    #[test]
    fn a_named_group_is_named_and_its_body_is_inside() {
        let parts = explain(r"(?P<year>\d{4})-x", Flags::default()).unwrap();
        assert_eq!(parts[0].kind, "group");
        assert_eq!(parts[0].meaning, "capture group 1 named year");
        assert_eq!(parts[1].kind, "repetition");
        assert_eq!(parts[1].depth, 1);
        assert!(parts[1].meaning.contains("exactly 4 times"));
        assert_eq!(parts.last().unwrap().source, "-x");
    }

    #[test]
    fn alternatives_are_children() {
        let parts = kinds("^(cat|dog)$");
        assert_eq!(parts[0], (0, "assertion".into(), "^".into()));
        assert_eq!(parts[2].1, "alternation");
        assert_eq!(parts[3], (2, "literal".into(), "cat".into()));
        assert_eq!(parts[4], (2, "literal".into(), "dog".into()));
    }
}
