//! Highlighted spans for the `code` node (plan-koine-tree-stream T-D5).
//!
//! [`spans`] turns source text into one list of `(class, text)` per line. The
//! class is a short name from a fixed vocabulary ([`CLASSES`]), or empty for
//! plain text, so a stylesheet colours nine classes and no more. With the
//! `highlight` feature the grammar is `syntect`'s default pack, parsed with
//! its pure-Rust regex engine; without it [`spans`] answers no lines and the
//! node reads as it always did. A language syntect does not know, a text too
//! large to be worth it ([`MAX_LINES`], [`MAX_BYTES`]) and a parse error all
//! answer no lines too: the node falls back to its plain text.

/// One line of source: `(class, text)` pairs whose texts concatenate to the
/// line without its line ending. The class is empty for plain text.
pub type Line = Vec<(String, String)>;

/// The classes a span can carry, so a stylesheet knows the whole set.
pub const CLASSES: &[&str] = &[
    "keyword",
    "string",
    "comment",
    "constant",
    "function",
    "type",
    "variable",
    "punctuation",
];

/// A text with more lines than this is left plain.
pub const MAX_LINES: usize = 2_000;
/// A text with more bytes than this is left plain.
pub const MAX_BYTES: usize = 200_000;

/// The class of one scope name (`keyword.control.rust` is `keyword`), or
/// `None` when the scope is not one of the classes. Looks at the scope's
/// first two or three dotted parts only.
pub fn class_of_scope(scope: &str) -> Option<&'static str> {
    let mut parts = scope.split('.');
    let first = parts.next()?;
    let second = parts.next().unwrap_or("");
    let third = parts.next().unwrap_or("");
    Some(match (first, second) {
        ("keyword", _) => "keyword",
        ("string", _) => "string",
        ("comment", _) => "comment",
        ("constant", _) => "constant",
        ("entity", "name") => match third {
            "function" => "function",
            "type" | "class" | "struct" | "enum" | "trait" | "interface" | "namespace"
            | "module" | "union" => "type",
            _ => return None,
        },
        // `fn`, `def` and `class` are keywords whatever the grammar files
        // them under; the rest of `storage.type` names a type.
        ("storage", "type") => match third {
            "function" | "class" | "struct" | "enum" | "trait" | "impl" | "interface"
            | "namespace" | "module" | "union" | "const" | "let" | "var" => "keyword",
            _ => "type",
        },
        ("storage", _) => "keyword",
        ("support", "function") => "function",
        ("support", "type" | "class") => "type",
        ("support", "constant") => "constant",
        ("variable", "function") => "function",
        ("variable", _) => "variable",
        ("punctuation", _) => "punctuation",
        _ => return None,
    })
}

/// The class of a whole scope stack (outermost first): the outermost scope
/// with a class that is not punctuation wins, so the quotes of a string and
/// the `//` of a comment take the class of what they delimit; a stack with
/// only punctuation is punctuation.
#[cfg_attr(not(feature = "highlight"), allow(dead_code))]
fn class_of_stack<'a>(scopes: impl IntoIterator<Item = &'a str>) -> &'static str {
    let mut punctuation = false;
    for scope in scopes {
        match class_of_scope(scope) {
            Some("punctuation") => punctuation = true,
            Some(class) => return class,
            None => {}
        }
    }
    if punctuation {
        "punctuation"
    } else {
        ""
    }
}

/// Highlight `text` as `language` (a syntax name or file extension token,
/// case-insensitive: `rust`, `rs`, `python`, `json`, `sh`). One [`Line`] per
/// line of `text`; empty when the feature is off, the language is unknown,
/// the text is too large or the grammar failed on it.
pub fn spans(language: &str, text: &str) -> Vec<Line> {
    imp::spans(language, text)
}

#[cfg(not(feature = "highlight"))]
mod imp {
    use super::Line;

    pub fn spans(_language: &str, _text: &str) -> Vec<Line> {
        Vec::new()
    }
}

#[cfg(feature = "highlight")]
mod imp {
    use std::sync::OnceLock;

    use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxReference, SyntaxSet};

    use super::{class_of_stack, Line, MAX_BYTES, MAX_LINES};

    fn syntaxes() -> &'static SyntaxSet {
        static SET: OnceLock<SyntaxSet> = OnceLock::new();
        SET.get_or_init(SyntaxSet::load_defaults_newlines)
    }

    /// The names people write for a language that are not a syntax name or an
    /// extension of the default pack.
    fn alias(token: &str) -> &str {
        match token {
            "shell" | "bash" | "zsh" | "console" | "shell-session" => "sh",
            "javascript" | "node" => "js",
            "python3" => "py",
            "yml" => "yaml",
            "c++" => "cpp",
            other => other,
        }
    }

    fn find(language: &str) -> Option<&'static SyntaxReference> {
        let set = syntaxes();
        let token = language.trim().to_ascii_lowercase();
        if token.is_empty() || token == "diff" || token == "patch" {
            return None;
        }
        let syntax = set
            .find_syntax_by_token(&token)
            .or_else(|| set.find_syntax_by_token(alias(&token)))?;
        // Plain Text has no scopes worth a colour.
        (syntax.name != "Plain Text").then_some(syntax)
    }

    pub fn spans(language: &str, text: &str) -> Vec<Line> {
        if text.len() > MAX_BYTES || text.lines().count() > MAX_LINES {
            return Vec::new();
        }
        let Some(syntax) = find(language) else {
            return Vec::new();
        };
        let set = syntaxes();
        let mut state = ParseState::new(syntax);
        let mut stack = ScopeStack::new();
        let mut out: Vec<Line> = Vec::new();

        for raw in text.lines() {
            // The grammars are the "newlines" variants: every line, the last
            // included, ends in `\n`.
            let line = format!("{raw}\n");
            let Ok(ops) = state.parse_line(&line, set) else {
                return Vec::new();
            };
            let mut spans: Line = Vec::new();
            let mut at = 0;
            let mut push = |class: &'static str, piece: &str| {
                let piece = piece.trim_end_matches(['\n', '\r']);
                if piece.is_empty() {
                    return;
                }
                match spans.last_mut() {
                    Some((last, text)) if last == class => text.push_str(piece),
                    _ => spans.push((class.to_string(), piece.to_string())),
                }
            };
            for (offset, op) in ops {
                let class = class_of(&stack);
                if offset > at {
                    push(class, &line[at..offset]);
                    at = offset;
                }
                if stack.apply(&op).is_err() {
                    return Vec::new();
                }
            }
            if at < line.len() {
                push(class_of(&stack), &line[at..]);
            }
            out.push(spans);
        }
        out
    }

    fn class_of(stack: &ScopeStack) -> &'static str {
        let names: Vec<String> = stack
            .as_slice()
            .iter()
            .map(|scope: &Scope| scope.build_string())
            .collect();
        class_of_stack(names.iter().map(String::as_str))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_map_to_the_fixed_classes() {
        for (scope, class) in [
            ("keyword.control.rust", Some("keyword")),
            ("string.quoted.double.rust", Some("string")),
            ("comment.line.double-slash.rust", Some("comment")),
            ("constant.numeric.integer.decimal.rust", Some("constant")),
            ("entity.name.function.rust", Some("function")),
            ("entity.name.type.struct.rust", Some("type")),
            ("storage.type.rust", Some("type")),
            ("storage.type.function.rust", Some("keyword")),
            ("storage.modifier.rust", Some("keyword")),
            ("variable.parameter.rust", Some("variable")),
            ("punctuation.definition.string.begin", Some("punctuation")),
            ("meta.function.rust", None),
            ("source.rust", None),
        ] {
            assert_eq!(class_of_scope(scope), class, "{scope}");
        }
    }

    #[test]
    fn a_stack_takes_the_outermost_class_and_punctuation_only_alone() {
        assert_eq!(
            class_of_stack([
                "source.rust",
                "string.quoted.double.rust",
                "punctuation.definition.string.begin.rust"
            ]),
            "string"
        );
        assert_eq!(
            class_of_stack(["source.rust", "punctuation.separator.rust"]),
            "punctuation"
        );
        assert_eq!(class_of_stack(["source.rust"]), "");
    }

    #[cfg(not(feature = "highlight"))]
    #[test]
    fn without_the_feature_there_are_no_spans() {
        assert!(spans("rust", "fn main() {}").is_empty());
    }

    #[cfg(feature = "highlight")]
    #[test]
    fn a_rust_snippet_has_a_keyword_span_and_keeps_every_character() {
        let source = "fn main() {\n    let s = \"hi\"; // greet\n}";
        let lines = spans("rust", source);
        assert_eq!(lines.len(), 3);
        assert!(
            lines[0].iter().any(|(c, t)| c == "keyword" && t == "fn"),
            "{lines:?}"
        );
        assert!(
            lines[1]
                .iter()
                .any(|(c, t)| c == "string" && t.contains("hi")),
            "{lines:?}"
        );
        assert!(
            lines[1]
                .iter()
                .any(|(c, t)| c == "comment" && t.contains("greet")),
            "{lines:?}"
        );
        for (line, source) in lines.iter().zip(source.lines()) {
            let joined: String = line.iter().map(|(_, t)| t.as_str()).collect();
            assert_eq!(joined, source);
        }
    }

    #[cfg(feature = "highlight")]
    #[test]
    fn the_language_is_a_name_or_an_extension_and_unknown_ones_are_plain() {
        for token in ["rust", "Rust", "rs"] {
            assert!(!spans(token, "fn a() {}").is_empty(), "{token}");
        }
        assert!(!spans("python", "def f():\n    pass").is_empty());
        assert!(!spans("json", "{\"a\": 1}").is_empty());
        assert!(!spans("sh", "echo hi").is_empty());
        assert!(spans("not-a-language", "fn a() {}").is_empty());
        assert!(spans("diff", "+ a\n- b").is_empty());
        assert!(spans("", "x").is_empty());
    }

    #[cfg(feature = "highlight")]
    #[test]
    fn a_text_past_the_limit_is_left_plain() {
        let big = "x\n".repeat(MAX_LINES + 1);
        assert!(spans("rust", &big).is_empty());
    }
}
