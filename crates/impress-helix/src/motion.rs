//! Motion definitions for Helix-style editor.
//!
//! Motions describe cursor movements that can be used standalone or combined
//! with operators (d, c, y) to affect text ranges.

/// A motion describing cursor movement.
///
/// Motions can be:
/// - Used standalone to move the cursor
/// - Combined with operators (delete, change, yank) to affect a range of text
/// - Used with counts (e.g., `3w` for 3 words forward)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    // Character movements
    /// Move left by count characters.
    Left(usize),
    /// Move right by count characters.
    Right(usize),
    /// Move up by count lines.
    Up(usize),
    /// Move down by count lines.
    Down(usize),

    // Word movements
    /// Move to the start of the next word, count times.
    WordForward(usize),
    /// Move to the start of the previous word, count times.
    WordBackward(usize),
    /// Move to the end of the current/next word, count times.
    WordEnd(usize),
    /// Move to the start of the next WORD (whitespace-delimited), count times.
    WORDForward(usize),
    /// Move to the start of the previous WORD, count times.
    WORDBackward(usize),
    /// Move to the end of the current/next WORD, count times.
    WORDEnd(usize),

    // Line movements
    /// Move to the start of the line (column 0).
    LineStart,
    /// Move to the end of the line.
    LineEnd,
    /// Move to the first non-blank character of the line.
    LineFirstNonBlank,

    // Document movements
    /// Move to the start of the document.
    DocumentStart,
    /// Move to the end of the document.
    DocumentEnd,
    /// Move to a specific line number.
    GotoLine(usize),

    // Paragraph movements
    /// Move forward by count paragraphs.
    ParagraphForward(usize),
    /// Move backward by count paragraphs.
    ParagraphBackward(usize),

    // Character finding (within line)
    /// Find character forward on line, count times (f command).
    FindChar(char, usize),
    /// Find character backward on line, count times (F command).
    FindCharBackward(char, usize),
    /// Find till (before) character forward on line (t command).
    TillChar(char, usize),
    /// Find till (after) character backward on line (T command).
    TillCharBackward(char, usize),

    // Special
    /// The current line (used for dd, cc, yy operators).
    Line,
    /// Move to matching bracket/delimiter.
    MatchingBracket,
    /// To end of line ($ motion, used for d$, c$, etc.).
    ToLineEnd,
    /// To start of line (0 motion).
    ToLineStart,
}

impl Motion {
    /// Returns true if this motion operates linewise (affects whole lines).
    ///
    /// Linewise motions are handled specially by operators - they operate on
    /// complete lines rather than character ranges.
    pub fn is_linewise(&self) -> bool {
        matches!(
            self,
            Motion::Line
                | Motion::Up(_)
                | Motion::Down(_)
                | Motion::ParagraphForward(_)
                | Motion::ParagraphBackward(_)
                | Motion::GotoLine(_)
        )
    }

    /// Returns true if this motion is inclusive (includes the character at the end).
    ///
    /// Most motions are exclusive (don't include the final character), but some
    /// like `e` (word end) and `f` (find character) are inclusive.
    pub fn is_inclusive(&self) -> bool {
        matches!(
            self,
            Motion::WordEnd(_)
                | Motion::WORDEnd(_)
                | Motion::FindChar(_, _)
                | Motion::FindCharBackward(_, _)
                | Motion::MatchingBracket
                | Motion::LineEnd
                | Motion::ToLineEnd
        )
    }

    /// Get the count for this motion, if applicable.
    pub fn count(&self) -> Option<usize> {
        match self {
            Motion::Left(n)
            | Motion::Right(n)
            | Motion::Up(n)
            | Motion::Down(n)
            | Motion::WordForward(n)
            | Motion::WordBackward(n)
            | Motion::WordEnd(n)
            | Motion::WORDForward(n)
            | Motion::WORDBackward(n)
            | Motion::WORDEnd(n)
            | Motion::ParagraphForward(n)
            | Motion::ParagraphBackward(n)
            | Motion::FindChar(_, n)
            | Motion::FindCharBackward(_, n)
            | Motion::TillChar(_, n)
            | Motion::TillCharBackward(_, n)
            | Motion::GotoLine(n) => Some(*n),
            _ => None,
        }
    }

    /// Create a motion with a count multiplier applied.
    pub fn with_count(self, count: usize) -> Self {
        match self {
            Motion::Left(_) => Motion::Left(count),
            Motion::Right(_) => Motion::Right(count),
            Motion::Up(_) => Motion::Up(count),
            Motion::Down(_) => Motion::Down(count),
            Motion::WordForward(_) => Motion::WordForward(count),
            Motion::WordBackward(_) => Motion::WordBackward(count),
            Motion::WordEnd(_) => Motion::WordEnd(count),
            Motion::WORDForward(_) => Motion::WORDForward(count),
            Motion::WORDBackward(_) => Motion::WORDBackward(count),
            Motion::WORDEnd(_) => Motion::WORDEnd(count),
            Motion::ParagraphForward(_) => Motion::ParagraphForward(count),
            Motion::ParagraphBackward(_) => Motion::ParagraphBackward(count),
            Motion::FindChar(c, _) => Motion::FindChar(c, count),
            Motion::FindCharBackward(c, _) => Motion::FindCharBackward(c, count),
            Motion::TillChar(c, _) => Motion::TillChar(c, count),
            Motion::TillCharBackward(c, _) => Motion::TillCharBackward(c, count),
            Motion::GotoLine(_) => Motion::GotoLine(count),
            // These don't have counts
            other => other,
        }
    }
}

impl Default for Motion {
    fn default() -> Self {
        Motion::Right(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_linewise_motions() {
        assert!(Motion::Line.is_linewise());
        assert!(Motion::Up(1).is_linewise());
        assert!(Motion::Down(5).is_linewise());
        assert!(Motion::ParagraphForward(1).is_linewise());
        assert!(!Motion::WordForward(1).is_linewise());
        assert!(!Motion::FindChar('a', 1).is_linewise());
    }

    #[test]
    fn test_inclusive_motions() {
        assert!(Motion::WordEnd(1).is_inclusive());
        assert!(Motion::FindChar('x', 1).is_inclusive());
        assert!(Motion::LineEnd.is_inclusive());
        assert!(!Motion::WordForward(1).is_inclusive());
        assert!(!Motion::Left(1).is_inclusive());
    }

    #[test]
    fn test_with_count() {
        assert_eq!(Motion::WordForward(1).with_count(5), Motion::WordForward(5));
        assert_eq!(
            Motion::FindChar('a', 1).with_count(3),
            Motion::FindChar('a', 3)
        );
        // Motions without counts remain unchanged
        assert_eq!(Motion::LineStart.with_count(5), Motion::LineStart);
    }

    #[test]
    fn test_count_extraction() {
        assert_eq!(Motion::WordForward(3).count(), Some(3));
        assert_eq!(Motion::Down(10).count(), Some(10));
        assert_eq!(Motion::LineStart.count(), None);
        assert_eq!(Motion::MatchingBracket.count(), None);
    }
}
