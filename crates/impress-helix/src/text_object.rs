//! Text object definitions for Helix-style editor.
//!
//! Text objects are used with operators (d, c, y) and modifiers (i = inner, a = around)
//! to select and manipulate structured text regions like words, quotes, and brackets.

/// A text object describing a structured region of text.
///
/// Text objects are used with the "inner" (i) and "around" (a) modifiers:
/// - `diw` - delete inner word (the word itself)
/// - `daw` - delete around word (word plus surrounding whitespace)
/// - `ci"` - change inner quote (content between quotes)
/// - `da(` - delete around parens (including the parentheses)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextObject {
    // Word objects
    /// A word (sequence of alphanumeric/underscore characters).
    Word,
    /// A WORD (sequence of non-whitespace characters).
    WORD,

    // Quote objects
    /// Double-quoted string `"..."`.
    DoubleQuote,
    /// Single-quoted string `'...'`.
    SingleQuote,
    /// Backtick-quoted string `` `...` ``.
    BacktickQuote,

    // Bracket/delimiter objects
    /// Parentheses `(...)`.
    Parentheses,
    /// Square brackets `[...]`.
    SquareBrackets,
    /// Curly braces `{...}`.
    CurlyBraces,
    /// Angle brackets `<...>`.
    AngleBrackets,

    // Block objects
    /// A paragraph (separated by blank lines).
    Paragraph,
    /// A sentence (ending with `.`, `!`, or `?`).
    Sentence,

    // Code-specific objects
    /// A function/method (language-aware).
    Function,
    /// A class definition (language-aware).
    Class,
    /// A comment block.
    Comment,
    /// An argument in a function call or definition.
    Argument,
}

/// Modifier for text object selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextObjectModifier {
    /// Select the inner content (excluding delimiters/whitespace).
    /// Used with `i` key: `diw`, `ci"`, etc.
    Inner,
    /// Select around (including delimiters and/or surrounding whitespace).
    /// Used with `a` key: `daw`, `da(`, etc.
    Around,
}

impl TextObject {
    /// Returns the opening delimiter for paired text objects, if applicable.
    pub fn opening_delimiter(&self) -> Option<char> {
        match self {
            TextObject::DoubleQuote => Some('"'),
            TextObject::SingleQuote => Some('\''),
            TextObject::BacktickQuote => Some('`'),
            TextObject::Parentheses => Some('('),
            TextObject::SquareBrackets => Some('['),
            TextObject::CurlyBraces => Some('{'),
            TextObject::AngleBrackets => Some('<'),
            _ => None,
        }
    }

    /// Returns the closing delimiter for paired text objects, if applicable.
    pub fn closing_delimiter(&self) -> Option<char> {
        match self {
            TextObject::DoubleQuote => Some('"'),
            TextObject::SingleQuote => Some('\''),
            TextObject::BacktickQuote => Some('`'),
            TextObject::Parentheses => Some(')'),
            TextObject::SquareBrackets => Some(']'),
            TextObject::CurlyBraces => Some('}'),
            TextObject::AngleBrackets => Some('>'),
            _ => None,
        }
    }

    /// Returns true if this text object has paired delimiters.
    pub fn is_paired(&self) -> bool {
        self.opening_delimiter().is_some()
    }

    /// Returns true if this text object uses the same character for open and close.
    pub fn is_symmetric(&self) -> bool {
        matches!(
            self,
            TextObject::DoubleQuote | TextObject::SingleQuote | TextObject::BacktickQuote
        )
    }

    /// Try to create a text object from a character typed after 'i' or 'a'.
    ///
    /// This maps the keys typically used in Vim/Helix:
    /// - `w` -> Word
    /// - `W` -> WORD
    /// - `"` -> DoubleQuote
    /// - `'` -> SingleQuote
    /// - `` ` `` -> BacktickQuote
    /// - `(`, `)`, `b` -> Parentheses
    /// - `[`, `]` -> SquareBrackets
    /// - `{`, `}`, `B` -> CurlyBraces
    /// - `<`, `>` -> AngleBrackets
    /// - `p` -> Paragraph
    /// - `s` -> Sentence
    /// - `f` -> Function
    /// - `c` -> Class
    /// - `a` -> Argument
    pub fn from_char(c: char) -> Option<Self> {
        match c {
            'w' => Some(TextObject::Word),
            'W' => Some(TextObject::WORD),
            '"' => Some(TextObject::DoubleQuote),
            '\'' => Some(TextObject::SingleQuote),
            '`' => Some(TextObject::BacktickQuote),
            '(' | ')' | 'b' => Some(TextObject::Parentheses),
            '[' | ']' => Some(TextObject::SquareBrackets),
            '{' | '}' | 'B' => Some(TextObject::CurlyBraces),
            '<' | '>' => Some(TextObject::AngleBrackets),
            'p' => Some(TextObject::Paragraph),
            's' => Some(TextObject::Sentence),
            'f' => Some(TextObject::Function),
            'c' => Some(TextObject::Class),
            'a' => Some(TextObject::Argument),
            _ => None,
        }
    }

    /// Returns a description of this text object for which-key display.
    pub fn description(&self) -> &'static str {
        match self {
            TextObject::Word => "word",
            TextObject::WORD => "WORD",
            TextObject::DoubleQuote => "double quotes",
            TextObject::SingleQuote => "single quotes",
            TextObject::BacktickQuote => "backticks",
            TextObject::Parentheses => "parentheses",
            TextObject::SquareBrackets => "brackets",
            TextObject::CurlyBraces => "braces",
            TextObject::AngleBrackets => "angle brackets",
            TextObject::Paragraph => "paragraph",
            TextObject::Sentence => "sentence",
            TextObject::Function => "function",
            TextObject::Class => "class",
            TextObject::Comment => "comment",
            TextObject::Argument => "argument",
        }
    }
}

impl TextObjectModifier {
    /// Returns a description for which-key display.
    pub fn description(&self) -> &'static str {
        match self {
            TextObjectModifier::Inner => "inner",
            TextObjectModifier::Around => "around",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_char() {
        assert_eq!(TextObject::from_char('w'), Some(TextObject::Word));
        assert_eq!(TextObject::from_char('W'), Some(TextObject::WORD));
        assert_eq!(TextObject::from_char('"'), Some(TextObject::DoubleQuote));
        assert_eq!(TextObject::from_char('('), Some(TextObject::Parentheses));
        assert_eq!(TextObject::from_char(')'), Some(TextObject::Parentheses));
        assert_eq!(TextObject::from_char('b'), Some(TextObject::Parentheses));
        assert_eq!(TextObject::from_char('{'), Some(TextObject::CurlyBraces));
        assert_eq!(TextObject::from_char('B'), Some(TextObject::CurlyBraces));
        assert_eq!(TextObject::from_char('z'), None);
    }

    #[test]
    fn test_delimiters() {
        assert_eq!(TextObject::Parentheses.opening_delimiter(), Some('('));
        assert_eq!(TextObject::Parentheses.closing_delimiter(), Some(')'));
        assert_eq!(TextObject::DoubleQuote.opening_delimiter(), Some('"'));
        assert_eq!(TextObject::DoubleQuote.closing_delimiter(), Some('"'));
        assert_eq!(TextObject::Word.opening_delimiter(), None);
    }

    #[test]
    fn test_is_symmetric() {
        assert!(TextObject::DoubleQuote.is_symmetric());
        assert!(TextObject::SingleQuote.is_symmetric());
        assert!(!TextObject::Parentheses.is_symmetric());
        assert!(!TextObject::CurlyBraces.is_symmetric());
    }

    #[test]
    fn test_is_paired() {
        assert!(TextObject::Parentheses.is_paired());
        assert!(TextObject::DoubleQuote.is_paired());
        assert!(!TextObject::Word.is_paired());
        assert!(!TextObject::Paragraph.is_paired());
    }
}
