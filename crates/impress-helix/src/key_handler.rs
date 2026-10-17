//! Key-to-command translation for Helix modal editing.

use crate::motion::Motion;
use crate::text_object::{TextObject, TextObjectModifier};
use crate::{HelixCommand, HelixMode};

/// Result of handling a key press.
#[derive(Debug, Clone, PartialEq)]
pub enum HelixKeyResult {
    /// A command was produced.
    Command(HelixCommand),
    /// Multiple commands were produced.
    Commands(Vec<HelixCommand>),
    /// The key should be passed through to the text engine.
    PassThrough,
    /// The key was consumed but produced no command (e.g., building count prefix).
    Pending,
    /// The key was consumed but no action needed.
    Consumed,
    /// Enter search mode.
    EnterSearch { backward: bool },
    /// Awaiting a character for f/t/r operations.
    AwaitingCharacter,
    /// Enter space-mode (application command menu).
    EnterSpaceMode,
}

/// A pending operator waiting for a motion or text object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingOperator {
    /// Delete operator (d).
    Delete,
    /// Change operator (c).
    Change,
    /// Yank operator (y).
    Yank,
    /// Indent operator (>).
    Indent,
    /// Dedent operator (<).
    Dedent,
}

/// Type of pending character operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingCharacterOperation {
    /// Find character forward (f).
    FindForward,
    /// Find character backward (F).
    FindBackward,
    /// Till character forward (t).
    TillForward,
    /// Till character backward (T).
    TillBackward,
    /// Replace character (r).
    Replace,
}

/// Key modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KeyModifiers {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
}

impl KeyModifiers {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn shift() -> Self {
        Self {
            shift: true,
            ..Self::default()
        }
    }

    pub fn control() -> Self {
        Self {
            control: true,
            ..Self::default()
        }
    }
}

/// Handles key-to-command translation for Helix modal editing.
pub struct HelixKeyHandler {
    /// Pending key for multi-key sequences (e.g., "g" in "gg").
    pending_key: Option<char>,
    /// Count prefix for commands (e.g., "3" in "3j").
    count_prefix: Option<usize>,
    /// Pending character operation (f, t, r awaiting a character).
    pending_char_op: Option<PendingCharacterOperation>,
    /// Last find operation for ; and , repeats.
    last_find_op: Option<(char, PendingCharacterOperation)>,
    /// Pending operator awaiting motion or text object (d, c, y).
    pending_operator: Option<PendingOperator>,
    /// Pending text object modifier (i, a) after an operator.
    pending_text_object_modifier: Option<TextObjectModifier>,
}

impl HelixKeyHandler {
    /// Create a new key handler.
    pub fn new() -> Self {
        Self {
            pending_key: None,
            count_prefix: None,
            pending_char_op: None,
            last_find_op: None,
            pending_operator: None,
            pending_text_object_modifier: None,
        }
    }

    /// Reset pending state.
    pub fn reset(&mut self) {
        self.pending_key = None;
        self.count_prefix = None;
        self.pending_char_op = None;
        self.pending_operator = None;
        self.pending_text_object_modifier = None;
    }

    /// Get the current pending operator (for UI display).
    pub fn pending_operator(&self) -> Option<PendingOperator> {
        self.pending_operator
    }

    /// Check if awaiting a motion or text object.
    pub fn is_awaiting_motion(&self) -> bool {
        self.pending_operator.is_some()
    }

    /// Get the current pending key (for UI display).
    pub fn pending_key(&self) -> Option<char> {
        self.pending_key
    }

    /// Get the current count prefix (for UI display).
    pub fn count_prefix(&self) -> Option<usize> {
        self.count_prefix
    }

    /// Get the last find operation (for ; and , repeats).
    pub fn last_find_op(&self) -> Option<(char, PendingCharacterOperation)> {
        self.last_find_op
    }

    /// Check if awaiting a character input.
    pub fn is_awaiting_character(&self) -> bool {
        self.pending_char_op.is_some()
    }

    /// Handle a key press in the given mode.
    ///
    /// Returns the result of handling the key.
    pub fn handle_key(
        &mut self,
        key: char,
        mode: HelixMode,
        modifiers: &KeyModifiers,
    ) -> HelixKeyResult {
        // Insert mode: only Escape returns to normal mode
        if mode == HelixMode::Insert {
            if key == '\x1b' {
                // Escape
                return HelixKeyResult::Command(HelixCommand::EnterNormalMode);
            }
            return HelixKeyResult::PassThrough;
        }

        // Escape cancels pending state
        if key == '\x1b' {
            self.reset();
            return HelixKeyResult::Command(HelixCommand::EnterNormalMode);
        }

        // Handle pending character operation (f, t, r)
        if let Some(op) = self.pending_char_op.take() {
            return self.handle_character_operation(op, key);
        }

        // Handle pending text object modifier (i, a) after operator
        if let Some(modifier) = self.pending_text_object_modifier.take() {
            if let Some(text_object) = TextObject::from_char(key) {
                let operator = self.pending_operator.take().unwrap();
                return self.create_text_object_command(operator, text_object, modifier);
            }
            // Invalid text object key, reset
            self.pending_operator = None;
            return HelixKeyResult::Consumed;
        }

        // Handle pending operator (d, c, y) waiting for motion or text object
        if let Some(operator) = self.pending_operator {
            let count = self.count_prefix.take().unwrap_or(1);
            return self.handle_pending_operator(operator, key, count, modifiers);
        }

        // Handle count prefix (digits)
        if key.is_ascii_digit() {
            let digit = key.to_digit(10).unwrap() as usize;
            if let Some(count) = self.count_prefix {
                self.count_prefix = Some(count * 10 + digit);
            } else if digit != 0 {
                // 0 is special (line start), don't start count with 0
                self.count_prefix = Some(digit);
                return HelixKeyResult::Pending;
            }
            if self.count_prefix.is_some() {
                return HelixKeyResult::Pending;
            }
        }

        // Get count and reset
        let count = self.count_prefix.take().unwrap_or(1);

        // Handle pending key sequences (e.g., "gg")
        if let Some(pending) = self.pending_key.take() {
            return self.handle_pending_key(pending, key, count, modifiers);
        }

        // Normal/Select mode key handling
        self.handle_normal_key(key, count, modifiers)
    }

    /// Handle a key after an operator is pending (d, c, y waiting for motion/text-object).
    fn handle_pending_operator(
        &mut self,
        operator: PendingOperator,
        key: char,
        count: usize,
        _modifiers: &KeyModifiers,
    ) -> HelixKeyResult {
        // Check for doubled operator (dd, cc, yy)
        let is_same_operator = matches!(
            (operator, key),
            (PendingOperator::Delete, 'd')
                | (PendingOperator::Change, 'c')
                | (PendingOperator::Yank, 'y')
                | (PendingOperator::Indent, '>')
                | (PendingOperator::Dedent, '<')
        );

        if is_same_operator {
            self.pending_operator = None;
            return self.create_motion_command(operator, Motion::Line);
        }

        // Text object modifiers
        match key {
            'i' => {
                self.pending_text_object_modifier = Some(TextObjectModifier::Inner);
                return HelixKeyResult::Pending;
            }
            'a' => {
                self.pending_text_object_modifier = Some(TextObjectModifier::Around);
                return HelixKeyResult::Pending;
            }
            _ => {}
        }

        // Try to parse as motion
        if let Some(motion) = self.key_to_motion(key, count) {
            self.pending_operator = None;
            return self.create_motion_command(operator, motion);
        }

        // Character-finding motions (f, t)
        match key {
            'f' => {
                self.pending_char_op = Some(PendingCharacterOperation::FindForward);
                return HelixKeyResult::AwaitingCharacter;
            }
            'F' => {
                self.pending_char_op = Some(PendingCharacterOperation::FindBackward);
                return HelixKeyResult::AwaitingCharacter;
            }
            't' => {
                self.pending_char_op = Some(PendingCharacterOperation::TillForward);
                return HelixKeyResult::AwaitingCharacter;
            }
            'T' => {
                self.pending_char_op = Some(PendingCharacterOperation::TillBackward);
                return HelixKeyResult::AwaitingCharacter;
            }
            'g' => {
                // Could be gg motion
                self.pending_key = Some('g');
                return HelixKeyResult::Pending;
            }
            _ => {}
        }

        // Unknown key, cancel operator
        self.pending_operator = None;
        HelixKeyResult::Consumed
    }

    /// Convert a key to a motion, if possible.
    fn key_to_motion(&self, key: char, count: usize) -> Option<Motion> {
        match key {
            // Basic movement
            'h' => Some(Motion::Left(count)),
            'j' => Some(Motion::Down(count)),
            'k' => Some(Motion::Up(count)),
            'l' => Some(Motion::Right(count)),

            // Word movement
            'w' => Some(Motion::WordForward(count)),
            'W' => Some(Motion::WORDForward(count)),
            'b' => Some(Motion::WordBackward(count)),
            'B' => Some(Motion::WORDBackward(count)),
            'e' => Some(Motion::WordEnd(count)),
            'E' => Some(Motion::WORDEnd(count)),

            // Line movement
            '0' => Some(Motion::LineStart),
            '$' => Some(Motion::ToLineEnd),
            '^' => Some(Motion::LineFirstNonBlank),

            // Document movement
            'G' => Some(Motion::DocumentEnd),

            // Paragraph movement
            '{' => Some(Motion::ParagraphBackward(count)),
            '}' => Some(Motion::ParagraphForward(count)),

            // Matching bracket
            '%' => Some(Motion::MatchingBracket),

            _ => None,
        }
    }

    /// Create an operator+motion command.
    fn create_motion_command(&self, operator: PendingOperator, motion: Motion) -> HelixKeyResult {
        let command = match operator {
            PendingOperator::Delete => HelixCommand::DeleteMotion(motion),
            PendingOperator::Change => HelixCommand::ChangeMotion(motion),
            PendingOperator::Yank => HelixCommand::YankMotion(motion),
            PendingOperator::Indent => HelixCommand::IndentMotion(motion),
            PendingOperator::Dedent => HelixCommand::DedentMotion(motion),
        };
        HelixKeyResult::Command(command)
    }

    /// Create an operator+text-object command.
    fn create_text_object_command(
        &self,
        operator: PendingOperator,
        text_object: TextObject,
        modifier: TextObjectModifier,
    ) -> HelixKeyResult {
        let command = match operator {
            PendingOperator::Delete => HelixCommand::DeleteTextObject(text_object, modifier),
            PendingOperator::Change => HelixCommand::ChangeTextObject(text_object, modifier),
            PendingOperator::Yank => HelixCommand::YankTextObject(text_object, modifier),
            // Indent/Dedent with text objects doesn't make as much sense, but handle it
            PendingOperator::Indent => HelixCommand::DeleteTextObject(text_object, modifier),
            PendingOperator::Dedent => HelixCommand::DeleteTextObject(text_object, modifier),
        };
        HelixKeyResult::Command(command)
    }

    fn handle_character_operation(
        &mut self,
        op: PendingCharacterOperation,
        char: char,
    ) -> HelixKeyResult {
        // Store for ; and , repeats (except replace)
        if op != PendingCharacterOperation::Replace {
            self.last_find_op = Some((char, op));
        }

        let command = match op {
            PendingCharacterOperation::FindForward => {
                HelixCommand::FindCharacter { char, count: 1 }
            }
            PendingCharacterOperation::FindBackward => {
                HelixCommand::FindCharacterBackward { char, count: 1 }
            }
            PendingCharacterOperation::TillForward => {
                HelixCommand::TillCharacter { char, count: 1 }
            }
            PendingCharacterOperation::TillBackward => {
                HelixCommand::TillCharacterBackward { char, count: 1 }
            }
            PendingCharacterOperation::Replace => HelixCommand::ReplaceCharacter { char },
        };

        HelixKeyResult::Command(command)
    }

    fn handle_pending_key(
        &mut self,
        pending: char,
        key: char,
        count: usize,
        _modifiers: &KeyModifiers,
    ) -> HelixKeyResult {
        match (pending, key) {
            // gg - go to document start
            ('g', 'g') => HelixKeyResult::Command(HelixCommand::DocumentStart),
            // ge - go to end of previous word
            ('g', 'e') => HelixKeyResult::Command(HelixCommand::WordEnd { count }),
            // Unknown g sequence
            ('g', _) => HelixKeyResult::Consumed,
            _ => HelixKeyResult::Consumed,
        }
    }

    fn handle_normal_key(
        &mut self,
        key: char,
        count: usize,
        modifiers: &KeyModifiers,
    ) -> HelixKeyResult {
        match key {
            // Space-mode
            ' ' => HelixKeyResult::EnterSpaceMode,

            // Mode changes
            'i' => HelixKeyResult::Command(HelixCommand::EnterInsertMode),
            'a' => HelixKeyResult::Command(HelixCommand::AppendAfterCursor),
            'A' => HelixKeyResult::Command(HelixCommand::AppendAtLineEnd),
            'I' => HelixKeyResult::Command(HelixCommand::InsertAtLineStart),
            'o' => HelixKeyResult::Command(HelixCommand::OpenLineBelow),
            'O' => HelixKeyResult::Command(HelixCommand::OpenLineAbove),
            'v' => HelixKeyResult::Command(HelixCommand::EnterSelectMode),
            '\x1b' => HelixKeyResult::Command(HelixCommand::EnterNormalMode), // Escape

            // Basic movement
            'h' => HelixKeyResult::Command(HelixCommand::MoveLeft { count }),
            'j' => HelixKeyResult::Command(HelixCommand::MoveDown { count }),
            'k' => HelixKeyResult::Command(HelixCommand::MoveUp { count }),
            'l' => HelixKeyResult::Command(HelixCommand::MoveRight { count }),

            // Word movement
            'w' => HelixKeyResult::Command(HelixCommand::WordForward { count }),
            'b' => HelixKeyResult::Command(HelixCommand::WordBackward { count }),
            'e' => HelixKeyResult::Command(HelixCommand::WordEnd { count }),

            // Line movement
            '0' => HelixKeyResult::Command(HelixCommand::LineStart),
            '$' => HelixKeyResult::Command(HelixCommand::LineEnd),
            '^' => HelixKeyResult::Command(HelixCommand::LineFirstNonBlank),

            // Document movement
            'G' => HelixKeyResult::Command(HelixCommand::DocumentEnd),
            'g' => {
                self.pending_key = Some('g');
                HelixKeyResult::Pending
            }

            // Character finding
            'f' => {
                self.pending_char_op = Some(PendingCharacterOperation::FindForward);
                HelixKeyResult::AwaitingCharacter
            }
            'F' => {
                self.pending_char_op = Some(PendingCharacterOperation::FindBackward);
                HelixKeyResult::AwaitingCharacter
            }
            't' => {
                self.pending_char_op = Some(PendingCharacterOperation::TillForward);
                HelixKeyResult::AwaitingCharacter
            }
            'T' => {
                self.pending_char_op = Some(PendingCharacterOperation::TillBackward);
                HelixKeyResult::AwaitingCharacter
            }
            ';' => HelixKeyResult::Command(HelixCommand::RepeatFind),
            ',' => HelixKeyResult::Command(HelixCommand::RepeatFindReverse),

            // Search
            '/' => HelixKeyResult::EnterSearch { backward: false },
            '?' => HelixKeyResult::EnterSearch { backward: true },
            'n' => HelixKeyResult::Command(HelixCommand::SearchNext { count }),
            'N' => HelixKeyResult::Command(HelixCommand::SearchPrevious { count }),

            // Selection
            'x' => HelixKeyResult::Command(HelixCommand::SelectLine),
            '%' => HelixKeyResult::Command(HelixCommand::SelectAll),

            // Operators (pending for motion/text-object)
            'd' => {
                self.pending_operator = Some(PendingOperator::Delete);
                self.count_prefix = if count > 1 { Some(count) } else { None };
                HelixKeyResult::Pending
            }
            'c' => {
                self.pending_operator = Some(PendingOperator::Change);
                self.count_prefix = if count > 1 { Some(count) } else { None };
                HelixKeyResult::Pending
            }
            'y' => {
                self.pending_operator = Some(PendingOperator::Yank);
                self.count_prefix = if count > 1 { Some(count) } else { None };
                HelixKeyResult::Pending
            }
            '>' => {
                self.pending_operator = Some(PendingOperator::Indent);
                self.count_prefix = if count > 1 { Some(count) } else { None };
                HelixKeyResult::Pending
            }
            '<' => {
                self.pending_operator = Some(PendingOperator::Dedent);
                self.count_prefix = if count > 1 { Some(count) } else { None };
                HelixKeyResult::Pending
            }

            // Immediate editing commands
            'p' => HelixKeyResult::Command(HelixCommand::PasteAfter),
            'P' => HelixKeyResult::Command(HelixCommand::PasteBefore),
            's' => HelixKeyResult::Command(HelixCommand::Substitute),

            // Line operations
            'J' => HelixKeyResult::Command(HelixCommand::JoinLines),
            '~' => HelixKeyResult::Command(HelixCommand::ToggleCase),
            'r' => {
                self.pending_char_op = Some(PendingCharacterOperation::Replace);
                HelixKeyResult::AwaitingCharacter
            }

            // Repeat and undo
            '.' => HelixKeyResult::Command(HelixCommand::RepeatLastChange),
            'u' => HelixKeyResult::Command(HelixCommand::Undo),
            'U' if modifiers.control => HelixKeyResult::Command(HelixCommand::Redo),

            _ => HelixKeyResult::Consumed,
        }
    }
}

impl Default for HelixKeyHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_movement() {
        let mut handler = HelixKeyHandler::new();
        let mods = KeyModifiers::default();

        assert_eq!(
            handler.handle_key('h', HelixMode::Normal, &mods),
            HelixKeyResult::Command(HelixCommand::MoveLeft { count: 1 })
        );
        assert_eq!(
            handler.handle_key('j', HelixMode::Normal, &mods),
            HelixKeyResult::Command(HelixCommand::MoveDown { count: 1 })
        );
        assert_eq!(
            handler.handle_key('k', HelixMode::Normal, &mods),
            HelixKeyResult::Command(HelixCommand::MoveUp { count: 1 })
        );
        assert_eq!(
            handler.handle_key('l', HelixMode::Normal, &mods),
            HelixKeyResult::Command(HelixCommand::MoveRight { count: 1 })
        );
    }

    #[test]
    fn test_count_prefix() {
        let mut handler = HelixKeyHandler::new();
        let mods = KeyModifiers::default();

        assert_eq!(
            handler.handle_key('3', HelixMode::Normal, &mods),
            HelixKeyResult::Pending
        );
        assert_eq!(
            handler.handle_key('j', HelixMode::Normal, &mods),
            HelixKeyResult::Command(HelixCommand::MoveDown { count: 3 })
        );
    }

    #[test]
    fn test_insert_mode_passthrough() {
        let mut handler = HelixKeyHandler::new();
        let mods = KeyModifiers::default();

        assert_eq!(
            handler.handle_key('a', HelixMode::Insert, &mods),
            HelixKeyResult::PassThrough
        );
        assert_eq!(
            handler.handle_key('\x1b', HelixMode::Insert, &mods),
            HelixKeyResult::Command(HelixCommand::EnterNormalMode)
        );
    }

    #[test]
    fn test_gg_sequence() {
        let mut handler = HelixKeyHandler::new();
        let mods = KeyModifiers::default();

        assert_eq!(
            handler.handle_key('g', HelixMode::Normal, &mods),
            HelixKeyResult::Pending
        );
        assert_eq!(
            handler.handle_key('g', HelixMode::Normal, &mods),
            HelixKeyResult::Command(HelixCommand::DocumentStart)
        );
    }

    #[test]
    fn test_find_character() {
        let mut handler = HelixKeyHandler::new();
        let mods = KeyModifiers::default();

        assert_eq!(
            handler.handle_key('f', HelixMode::Normal, &mods),
            HelixKeyResult::AwaitingCharacter
        );
        assert_eq!(
            handler.handle_key('x', HelixMode::Normal, &mods),
            HelixKeyResult::Command(HelixCommand::FindCharacter {
                char: 'x',
                count: 1
            })
        );
    }

    #[test]
    fn test_delete_word() {
        let mut handler = HelixKeyHandler::new();
        let mods = KeyModifiers::default();

        // dw - delete word
        assert_eq!(
            handler.handle_key('d', HelixMode::Normal, &mods),
            HelixKeyResult::Pending
        );
        assert_eq!(
            handler.handle_key('w', HelixMode::Normal, &mods),
            HelixKeyResult::Command(HelixCommand::DeleteMotion(Motion::WordForward(1)))
        );
    }

    #[test]
    fn test_delete_line_dd() {
        let mut handler = HelixKeyHandler::new();
        let mods = KeyModifiers::default();

        // dd - delete line
        assert_eq!(
            handler.handle_key('d', HelixMode::Normal, &mods),
            HelixKeyResult::Pending
        );
        assert_eq!(
            handler.handle_key('d', HelixMode::Normal, &mods),
            HelixKeyResult::Command(HelixCommand::DeleteMotion(Motion::Line))
        );
    }

    #[test]
    fn test_change_to_line_end() {
        let mut handler = HelixKeyHandler::new();
        let mods = KeyModifiers::default();

        // c$ - change to line end
        assert_eq!(
            handler.handle_key('c', HelixMode::Normal, &mods),
            HelixKeyResult::Pending
        );
        assert_eq!(
            handler.handle_key('$', HelixMode::Normal, &mods),
            HelixKeyResult::Command(HelixCommand::ChangeMotion(Motion::ToLineEnd))
        );
    }

    #[test]
    fn test_yank_line_yy() {
        let mut handler = HelixKeyHandler::new();
        let mods = KeyModifiers::default();

        // yy - yank line
        assert_eq!(
            handler.handle_key('y', HelixMode::Normal, &mods),
            HelixKeyResult::Pending
        );
        assert_eq!(
            handler.handle_key('y', HelixMode::Normal, &mods),
            HelixKeyResult::Command(HelixCommand::YankMotion(Motion::Line))
        );
    }

    #[test]
    fn test_delete_inner_word() {
        let mut handler = HelixKeyHandler::new();
        let mods = KeyModifiers::default();

        // diw - delete inner word
        assert_eq!(
            handler.handle_key('d', HelixMode::Normal, &mods),
            HelixKeyResult::Pending
        );
        assert_eq!(
            handler.handle_key('i', HelixMode::Normal, &mods),
            HelixKeyResult::Pending
        );
        assert_eq!(
            handler.handle_key('w', HelixMode::Normal, &mods),
            HelixKeyResult::Command(HelixCommand::DeleteTextObject(
                TextObject::Word,
                TextObjectModifier::Inner
            ))
        );
    }

    #[test]
    fn test_change_around_quotes() {
        let mut handler = HelixKeyHandler::new();
        let mods = KeyModifiers::default();

        // ca" - change around double quotes
        assert_eq!(
            handler.handle_key('c', HelixMode::Normal, &mods),
            HelixKeyResult::Pending
        );
        assert_eq!(
            handler.handle_key('a', HelixMode::Normal, &mods),
            HelixKeyResult::Pending
        );
        assert_eq!(
            handler.handle_key('"', HelixMode::Normal, &mods),
            HelixKeyResult::Command(HelixCommand::ChangeTextObject(
                TextObject::DoubleQuote,
                TextObjectModifier::Around
            ))
        );
    }

    #[test]
    fn test_delete_inner_parens() {
        let mut handler = HelixKeyHandler::new();
        let mods = KeyModifiers::default();

        // di( - delete inner parentheses
        assert_eq!(
            handler.handle_key('d', HelixMode::Normal, &mods),
            HelixKeyResult::Pending
        );
        assert_eq!(
            handler.handle_key('i', HelixMode::Normal, &mods),
            HelixKeyResult::Pending
        );
        assert_eq!(
            handler.handle_key('(', HelixMode::Normal, &mods),
            HelixKeyResult::Command(HelixCommand::DeleteTextObject(
                TextObject::Parentheses,
                TextObjectModifier::Inner
            ))
        );
    }

    #[test]
    fn test_space_enters_space_mode() {
        let mut handler = HelixKeyHandler::new();
        let mods = KeyModifiers::default();

        assert_eq!(
            handler.handle_key(' ', HelixMode::Normal, &mods),
            HelixKeyResult::EnterSpaceMode
        );
    }

    #[test]
    fn test_escape_cancels_pending() {
        let mut handler = HelixKeyHandler::new();
        let mods = KeyModifiers::default();

        // Start a d operator
        assert_eq!(
            handler.handle_key('d', HelixMode::Normal, &mods),
            HelixKeyResult::Pending
        );
        assert!(handler.pending_operator.is_some());

        // Escape cancels it
        handler.handle_key('\x1b', HelixMode::Normal, &mods);
        assert!(handler.pending_operator.is_none());
    }
}
