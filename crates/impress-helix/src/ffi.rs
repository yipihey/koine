//! UniFFI bindings for impress-helix.
//!
//! This module provides FFI-safe wrappers for the Helix modal editing types,
//! suitable for use from Swift, Kotlin, and other languages via UniFFI.

use crate::mode::HelixMode as InternalHelixMode;
use crate::space::SpaceCommand as InternalSpaceCommand;
use crate::text_object::{
    TextObject as InternalTextObject, TextObjectModifier as InternalModifier,
};

/// FFI-safe Helix editing mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiHelixMode {
    /// Normal mode for navigation and commands.
    Normal,
    /// Insert mode for text input.
    Insert,
    /// Select mode for extending selections.
    Select,
}

impl From<InternalHelixMode> for FfiHelixMode {
    fn from(mode: InternalHelixMode) -> Self {
        match mode {
            InternalHelixMode::Normal => FfiHelixMode::Normal,
            InternalHelixMode::Insert => FfiHelixMode::Insert,
            InternalHelixMode::Select => FfiHelixMode::Select,
        }
    }
}

impl From<FfiHelixMode> for InternalHelixMode {
    fn from(mode: FfiHelixMode) -> Self {
        match mode {
            FfiHelixMode::Normal => InternalHelixMode::Normal,
            FfiHelixMode::Insert => InternalHelixMode::Insert,
            FfiHelixMode::Select => InternalHelixMode::Select,
        }
    }
}

/// FFI-safe text object modifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiTextObjectModifier {
    /// Inner modifier (e.g., `iw` for inner word).
    Inner,
    /// Around modifier (e.g., `aw` for around word).
    Around,
}

impl From<InternalModifier> for FfiTextObjectModifier {
    fn from(m: InternalModifier) -> Self {
        match m {
            InternalModifier::Inner => FfiTextObjectModifier::Inner,
            InternalModifier::Around => FfiTextObjectModifier::Around,
        }
    }
}

/// FFI-safe text object type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiTextObject {
    Word,
    BigWord,
    DoubleQuote,
    SingleQuote,
    BacktickQuote,
    Parentheses,
    SquareBrackets,
    CurlyBraces,
    AngleBrackets,
    Paragraph,
    Sentence,
    Function,
    Class,
    Comment,
    Argument,
}

impl From<InternalTextObject> for FfiTextObject {
    fn from(t: InternalTextObject) -> Self {
        match t {
            InternalTextObject::Word => FfiTextObject::Word,
            InternalTextObject::WORD => FfiTextObject::BigWord,
            InternalTextObject::DoubleQuote => FfiTextObject::DoubleQuote,
            InternalTextObject::SingleQuote => FfiTextObject::SingleQuote,
            InternalTextObject::BacktickQuote => FfiTextObject::BacktickQuote,
            InternalTextObject::Parentheses => FfiTextObject::Parentheses,
            InternalTextObject::SquareBrackets => FfiTextObject::SquareBrackets,
            InternalTextObject::CurlyBraces => FfiTextObject::CurlyBraces,
            InternalTextObject::AngleBrackets => FfiTextObject::AngleBrackets,
            InternalTextObject::Paragraph => FfiTextObject::Paragraph,
            InternalTextObject::Sentence => FfiTextObject::Sentence,
            InternalTextObject::Function => FfiTextObject::Function,
            InternalTextObject::Class => FfiTextObject::Class,
            InternalTextObject::Comment => FfiTextObject::Comment,
            InternalTextObject::Argument => FfiTextObject::Argument,
        }
    }
}

/// FFI-safe space command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiSpaceCommand {
    // File operations
    FileOpen,
    FileRecent,
    FileSave,
    FileSaveAs,
    FileClose,
    FileCloseAll,

    // Buffer operations
    BufferPicker,
    BufferNext,
    BufferPrev,
    BufferClose,
    BufferCloseOthers,
    BufferRevert,

    // Window operations
    WindowSplitHorizontal,
    WindowSplitVertical,
    WindowClose,
    WindowOnly,
    WindowFocusLeft,
    WindowFocusDown,
    WindowFocusUp,
    WindowFocusRight,
    WindowSwapLeft,
    WindowSwapDown,
    WindowSwapUp,
    WindowSwapRight,

    // Search/Symbol
    SymbolPicker,
    WorkspaceSymbolPicker,
    GlobalSearch,
    SearchInFile,

    // Git
    GitStatus,
    GitDiff,
    GitBlame,
    GitLog,
    GitStage,
    GitUnstage,

    // Diagnostics
    DiagnosticsList,
    DiagnosticNext,
    DiagnosticPrev,

    // Code
    CodeAction,
    Rename,
    Format,

    // Help
    Help,

    // Misc
    CommandPalette,
    ToggleFileTree,
    ToggleTerminal,
}

impl From<InternalSpaceCommand> for FfiSpaceCommand {
    fn from(cmd: InternalSpaceCommand) -> Self {
        match cmd {
            InternalSpaceCommand::FileOpen => FfiSpaceCommand::FileOpen,
            InternalSpaceCommand::FileRecent => FfiSpaceCommand::FileRecent,
            InternalSpaceCommand::FileSave => FfiSpaceCommand::FileSave,
            InternalSpaceCommand::FileSaveAs => FfiSpaceCommand::FileSaveAs,
            InternalSpaceCommand::FileClose => FfiSpaceCommand::FileClose,
            InternalSpaceCommand::FileCloseAll => FfiSpaceCommand::FileCloseAll,
            InternalSpaceCommand::BufferPicker => FfiSpaceCommand::BufferPicker,
            InternalSpaceCommand::BufferNext => FfiSpaceCommand::BufferNext,
            InternalSpaceCommand::BufferPrev => FfiSpaceCommand::BufferPrev,
            InternalSpaceCommand::BufferClose => FfiSpaceCommand::BufferClose,
            InternalSpaceCommand::BufferCloseOthers => FfiSpaceCommand::BufferCloseOthers,
            InternalSpaceCommand::BufferRevert => FfiSpaceCommand::BufferRevert,
            InternalSpaceCommand::WindowSplitHorizontal => FfiSpaceCommand::WindowSplitHorizontal,
            InternalSpaceCommand::WindowSplitVertical => FfiSpaceCommand::WindowSplitVertical,
            InternalSpaceCommand::WindowClose => FfiSpaceCommand::WindowClose,
            InternalSpaceCommand::WindowOnly => FfiSpaceCommand::WindowOnly,
            InternalSpaceCommand::WindowFocusLeft => FfiSpaceCommand::WindowFocusLeft,
            InternalSpaceCommand::WindowFocusDown => FfiSpaceCommand::WindowFocusDown,
            InternalSpaceCommand::WindowFocusUp => FfiSpaceCommand::WindowFocusUp,
            InternalSpaceCommand::WindowFocusRight => FfiSpaceCommand::WindowFocusRight,
            InternalSpaceCommand::WindowSwapLeft => FfiSpaceCommand::WindowSwapLeft,
            InternalSpaceCommand::WindowSwapDown => FfiSpaceCommand::WindowSwapDown,
            InternalSpaceCommand::WindowSwapUp => FfiSpaceCommand::WindowSwapUp,
            InternalSpaceCommand::WindowSwapRight => FfiSpaceCommand::WindowSwapRight,
            InternalSpaceCommand::SymbolPicker => FfiSpaceCommand::SymbolPicker,
            InternalSpaceCommand::WorkspaceSymbolPicker => FfiSpaceCommand::WorkspaceSymbolPicker,
            InternalSpaceCommand::GlobalSearch => FfiSpaceCommand::GlobalSearch,
            InternalSpaceCommand::SearchInFile => FfiSpaceCommand::SearchInFile,
            InternalSpaceCommand::GitStatus => FfiSpaceCommand::GitStatus,
            InternalSpaceCommand::GitDiff => FfiSpaceCommand::GitDiff,
            InternalSpaceCommand::GitBlame => FfiSpaceCommand::GitBlame,
            InternalSpaceCommand::GitLog => FfiSpaceCommand::GitLog,
            InternalSpaceCommand::GitStage => FfiSpaceCommand::GitStage,
            InternalSpaceCommand::GitUnstage => FfiSpaceCommand::GitUnstage,
            InternalSpaceCommand::DiagnosticsList => FfiSpaceCommand::DiagnosticsList,
            InternalSpaceCommand::DiagnosticNext => FfiSpaceCommand::DiagnosticNext,
            InternalSpaceCommand::DiagnosticPrev => FfiSpaceCommand::DiagnosticPrev,
            InternalSpaceCommand::CodeAction => FfiSpaceCommand::CodeAction,
            InternalSpaceCommand::Rename => FfiSpaceCommand::Rename,
            InternalSpaceCommand::Format => FfiSpaceCommand::Format,
            InternalSpaceCommand::Help => FfiSpaceCommand::Help,
            InternalSpaceCommand::CommandPalette => FfiSpaceCommand::CommandPalette,
            InternalSpaceCommand::ToggleFileTree => FfiSpaceCommand::ToggleFileTree,
            InternalSpaceCommand::ToggleTerminal => FfiSpaceCommand::ToggleTerminal,
        }
    }
}

/// FFI-safe key modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, uniffi::Record)]
pub struct FfiKeyModifiers {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
}

/// An available key in the space-mode menu.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiWhichKeyItem {
    /// The key character.
    pub key: String,
    /// Description of what the key does.
    pub description: String,
}

/// Result of handling a key in the FFI wrapper.
#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiKeyResult {
    /// Key was handled, editor state updated.
    Handled,
    /// Key should be passed through to the text input.
    PassThrough,
    /// A space-mode command was triggered.
    SpaceCommand { command: FfiSpaceCommand },
    /// Space-mode menu is showing, here are the available keys.
    SpaceModePending {
        available_keys: Vec<FfiWhichKeyItem>,
    },
}

/// FFI wrapper for the Helix editor state.
#[derive(uniffi::Object)]
pub struct FfiHelixEditor {
    state: std::sync::Mutex<crate::HelixState>,
}

#[uniffi::export]
impl FfiHelixEditor {
    /// Create a new Helix editor instance.
    #[uniffi::constructor]
    pub fn new() -> Self {
        Self {
            state: std::sync::Mutex::new(crate::HelixState::new()),
        }
    }

    /// Get the current editing mode.
    pub fn mode(&self) -> FfiHelixMode {
        let state = self.state.lock().unwrap();
        state.mode().into()
    }

    /// Check if search mode is active.
    pub fn is_searching(&self) -> bool {
        let state = self.state.lock().unwrap();
        state.is_searching()
    }

    /// Get the current search query.
    pub fn search_query(&self) -> String {
        let state = self.state.lock().unwrap();
        state.search_query().to_string()
    }

    /// Check if space-mode is active.
    pub fn is_space_mode(&self) -> bool {
        let state = self.state.lock().unwrap();
        state.is_space_mode()
    }

    /// Get available keys in the current space-mode menu.
    pub fn space_mode_available_keys(&self) -> Vec<FfiWhichKeyItem> {
        let state = self.state.lock().unwrap();
        state
            .space_mode_available_keys()
            .iter()
            .map(|(key, desc)| FfiWhichKeyItem {
                key: key.display(),
                description: desc.clone(),
            })
            .collect()
    }

    /// Check if awaiting a character input (f/t/r operations).
    pub fn is_awaiting_character(&self) -> bool {
        let state = self.state.lock().unwrap();
        state.is_awaiting_character()
    }

    /// Check if awaiting a motion (operator pending, e.g., after pressing 'd').
    pub fn is_awaiting_motion(&self) -> bool {
        let state = self.state.lock().unwrap();
        state.is_awaiting_motion()
    }

    /// Get the register (clipboard) content.
    pub fn register_content(&self) -> String {
        let state = self.state.lock().unwrap();
        state.register_content().to_string()
    }

    /// Handle a key press without a text engine.
    ///
    /// Returns the result indicating what action was taken.
    pub fn handle_key(&self, key: String, modifiers: FfiKeyModifiers) -> FfiKeyResult {
        let mut state = self.state.lock().unwrap();

        let key_char = key.chars().next().unwrap_or(' ');
        let mods = crate::KeyModifiers {
            shift: modifiers.shift,
            control: modifiers.control,
            alt: modifiers.alt,
        };

        let result = state
            .handle_key_with_result::<crate::text_engine::NullTextEngine>(key_char, &mods, None);

        match result {
            crate::KeyHandleResult::Handled => FfiKeyResult::Handled,
            crate::KeyHandleResult::PassThrough => FfiKeyResult::PassThrough,
            crate::KeyHandleResult::SpaceCommand(cmd) => FfiKeyResult::SpaceCommand {
                command: cmd.into(),
            },
            crate::KeyHandleResult::SpaceModePending => FfiKeyResult::SpaceModePending {
                available_keys: state
                    .space_mode_available_keys()
                    .iter()
                    .map(|(key, desc)| FfiWhichKeyItem {
                        key: key.display(),
                        description: desc.clone(),
                    })
                    .collect(),
            },
        }
    }

    /// Reset the editor to normal mode.
    pub fn reset(&self) {
        let mut state = self.state.lock().unwrap();
        state.reset();
    }

    /// Exit space-mode.
    pub fn exit_space_mode(&self) {
        let mut state = self.state.lock().unwrap();
        state.exit_space_mode();
    }
}

impl Default for FfiHelixEditor {
    fn default() -> Self {
        Self::new()
    }
}

// MARK: - Motion FFI

/// FFI-safe motion type.
#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiMotion {
    Left { count: u32 },
    Right { count: u32 },
    Up { count: u32 },
    Down { count: u32 },
    WordForward { count: u32 },
    WordBackward { count: u32 },
    WordEnd { count: u32 },
    BigWordForward { count: u32 },
    BigWordBackward { count: u32 },
    BigWordEnd { count: u32 },
    LineStart,
    LineEnd,
    LineFirstNonBlank,
    DocumentStart,
    DocumentEnd,
    GotoLine { line: u32 },
    ParagraphForward { count: u32 },
    ParagraphBackward { count: u32 },
    FindChar { char: String, count: u32 },
    FindCharBackward { char: String, count: u32 },
    TillChar { char: String, count: u32 },
    TillCharBackward { char: String, count: u32 },
    Line,
    MatchingBracket,
    ToLineEnd,
    ToLineStart,
}

impl From<FfiMotion> for crate::motion::Motion {
    fn from(m: FfiMotion) -> Self {
        match m {
            FfiMotion::Left { count } => crate::motion::Motion::Left(count as usize),
            FfiMotion::Right { count } => crate::motion::Motion::Right(count as usize),
            FfiMotion::Up { count } => crate::motion::Motion::Up(count as usize),
            FfiMotion::Down { count } => crate::motion::Motion::Down(count as usize),
            FfiMotion::WordForward { count } => crate::motion::Motion::WordForward(count as usize),
            FfiMotion::WordBackward { count } => {
                crate::motion::Motion::WordBackward(count as usize)
            }
            FfiMotion::WordEnd { count } => crate::motion::Motion::WordEnd(count as usize),
            FfiMotion::BigWordForward { count } => {
                crate::motion::Motion::WORDForward(count as usize)
            }
            FfiMotion::BigWordBackward { count } => {
                crate::motion::Motion::WORDBackward(count as usize)
            }
            FfiMotion::BigWordEnd { count } => crate::motion::Motion::WORDEnd(count as usize),
            FfiMotion::LineStart => crate::motion::Motion::LineStart,
            FfiMotion::LineEnd => crate::motion::Motion::LineEnd,
            FfiMotion::LineFirstNonBlank => crate::motion::Motion::LineFirstNonBlank,
            FfiMotion::DocumentStart => crate::motion::Motion::DocumentStart,
            FfiMotion::DocumentEnd => crate::motion::Motion::DocumentEnd,
            FfiMotion::GotoLine { line } => crate::motion::Motion::GotoLine(line as usize),
            FfiMotion::ParagraphForward { count } => {
                crate::motion::Motion::ParagraphForward(count as usize)
            }
            FfiMotion::ParagraphBackward { count } => {
                crate::motion::Motion::ParagraphBackward(count as usize)
            }
            FfiMotion::FindChar { char, count } => {
                let c = char.chars().next().unwrap_or(' ');
                crate::motion::Motion::FindChar(c, count as usize)
            }
            FfiMotion::FindCharBackward { char, count } => {
                let c = char.chars().next().unwrap_or(' ');
                crate::motion::Motion::FindCharBackward(c, count as usize)
            }
            FfiMotion::TillChar { char, count } => {
                let c = char.chars().next().unwrap_or(' ');
                crate::motion::Motion::TillChar(c, count as usize)
            }
            FfiMotion::TillCharBackward { char, count } => {
                let c = char.chars().next().unwrap_or(' ');
                crate::motion::Motion::TillCharBackward(c, count as usize)
            }
            FfiMotion::Line => crate::motion::Motion::Line,
            FfiMotion::MatchingBracket => crate::motion::Motion::MatchingBracket,
            FfiMotion::ToLineEnd => crate::motion::Motion::ToLineEnd,
            FfiMotion::ToLineStart => crate::motion::Motion::ToLineStart,
        }
    }
}

// MARK: - Command FFI

/// FFI-safe Helix command.
#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiHelixCommand {
    // Mode changes
    EnterInsertMode,
    EnterNormalMode,
    EnterSelectMode,
    EnterSearchMode {
        backward: bool,
    },

    // Basic movement
    MoveLeft {
        count: u32,
    },
    MoveRight {
        count: u32,
    },
    MoveUp {
        count: u32,
    },
    MoveDown {
        count: u32,
    },

    // Word movement
    WordForward {
        count: u32,
    },
    WordBackward {
        count: u32,
    },
    WordEnd {
        count: u32,
    },

    // Line movement
    LineStart,
    LineEnd,
    LineFirstNonBlank,

    // Document movement
    DocumentStart,
    DocumentEnd,

    // Character finding
    FindCharacter {
        char: String,
        count: u32,
    },
    FindCharacterBackward {
        char: String,
        count: u32,
    },
    TillCharacter {
        char: String,
        count: u32,
    },
    TillCharacterBackward {
        char: String,
        count: u32,
    },
    RepeatFind,
    RepeatFindReverse,

    // Search
    SearchNext {
        count: u32,
    },
    SearchPrevious {
        count: u32,
    },

    // Selection
    SelectLine,
    SelectAll,

    // Insert mode variants
    AppendAfterCursor,
    AppendAtLineEnd,
    InsertAtLineStart,
    OpenLineBelow,
    OpenLineAbove,

    // Editing
    Delete,
    Yank,
    PasteAfter,
    PasteBefore,
    Change,
    Substitute,

    // Line operations
    JoinLines,
    ToggleCase,
    Indent,
    Dedent,
    ReplaceCharacter {
        char: String,
    },

    // Operator + Motion combinations
    DeleteMotion {
        motion: FfiMotion,
    },
    ChangeMotion {
        motion: FfiMotion,
    },
    YankMotion {
        motion: FfiMotion,
    },
    IndentMotion {
        motion: FfiMotion,
    },
    DedentMotion {
        motion: FfiMotion,
    },

    // Operator + Text Object combinations
    DeleteTextObject {
        text_object: FfiTextObject,
        modifier: FfiTextObjectModifier,
    },
    ChangeTextObject {
        text_object: FfiTextObject,
        modifier: FfiTextObjectModifier,
    },
    YankTextObject {
        text_object: FfiTextObject,
        modifier: FfiTextObjectModifier,
    },

    // Repeat and undo
    RepeatLastChange,
    Undo,
    Redo,
}

// MARK: - Range Calculation FFI

/// Result of a range calculation.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiTextRange {
    /// Start byte offset.
    pub start: u64,
    /// End byte offset.
    pub end: u64,
}

/// Calculate the range affected by a motion.
///
/// - `text`: The full text content.
/// - `cursor_position`: Current cursor position (byte offset).
/// - `motion`: The motion to calculate range for.
///
/// Returns the range as (start, end) byte offsets, or None if motion cannot be performed.
#[uniffi::export]
pub fn calculate_motion_range(
    text: String,
    cursor_position: u64,
    motion: FfiMotion,
) -> Option<FfiTextRange> {
    use crate::text_engine::HelixTextEngine;

    struct TextEngineAdapter {
        text: String,
        cursor: usize,
    }

    impl HelixTextEngine for TextEngineAdapter {
        fn text(&self) -> &str {
            &self.text
        }

        fn cursor_position(&self) -> usize {
            self.cursor
        }

        fn set_cursor_position(&mut self, position: usize) {
            self.cursor = position;
        }

        fn selection(&self) -> (usize, usize) {
            (self.cursor, self.cursor)
        }

        fn set_selection(&mut self, _start: usize, _end: usize) {}

        fn insert_text(&mut self, _text: &str) {}

        fn delete(&mut self) {}

        fn replace_selection(&mut self, _text: &str) {}

        fn undo(&mut self) {}

        fn redo(&mut self) {}
    }

    let engine = TextEngineAdapter {
        text,
        cursor: cursor_position as usize,
    };

    let internal_motion: crate::motion::Motion = motion.into();
    engine
        .motion_range(&internal_motion)
        .map(|(start, end)| FfiTextRange {
            start: start as u64,
            end: end as u64,
        })
}

/// Calculate the range affected by a text object.
///
/// - `text`: The full text content.
/// - `cursor_position`: Current cursor position (byte offset).
/// - `text_object`: The text object type.
/// - `modifier`: Inner or Around modifier.
///
/// Returns the range as (start, end) byte offsets, or None if text object not found.
#[uniffi::export]
pub fn calculate_text_object_range(
    text: String,
    cursor_position: u64,
    text_object: FfiTextObject,
    modifier: FfiTextObjectModifier,
) -> Option<FfiTextRange> {
    use crate::text_engine::HelixTextEngine;
    use crate::text_object::{TextObject, TextObjectModifier};

    struct TextEngineAdapter {
        text: String,
        cursor: usize,
    }

    impl HelixTextEngine for TextEngineAdapter {
        fn text(&self) -> &str {
            &self.text
        }

        fn cursor_position(&self) -> usize {
            self.cursor
        }

        fn set_cursor_position(&mut self, position: usize) {
            self.cursor = position;
        }

        fn selection(&self) -> (usize, usize) {
            (self.cursor, self.cursor)
        }

        fn set_selection(&mut self, _start: usize, _end: usize) {}

        fn insert_text(&mut self, _text: &str) {}

        fn delete(&mut self) {}

        fn replace_selection(&mut self, _text: &str) {}

        fn undo(&mut self) {}

        fn redo(&mut self) {}
    }

    let engine = TextEngineAdapter {
        text,
        cursor: cursor_position as usize,
    };

    let internal_text_object: TextObject = match text_object {
        FfiTextObject::Word => TextObject::Word,
        FfiTextObject::BigWord => TextObject::WORD,
        FfiTextObject::DoubleQuote => TextObject::DoubleQuote,
        FfiTextObject::SingleQuote => TextObject::SingleQuote,
        FfiTextObject::BacktickQuote => TextObject::BacktickQuote,
        FfiTextObject::Parentheses => TextObject::Parentheses,
        FfiTextObject::SquareBrackets => TextObject::SquareBrackets,
        FfiTextObject::CurlyBraces => TextObject::CurlyBraces,
        FfiTextObject::AngleBrackets => TextObject::AngleBrackets,
        FfiTextObject::Paragraph => TextObject::Paragraph,
        FfiTextObject::Sentence => TextObject::Sentence,
        FfiTextObject::Function => TextObject::Function,
        FfiTextObject::Class => TextObject::Class,
        FfiTextObject::Comment => TextObject::Comment,
        FfiTextObject::Argument => TextObject::Argument,
    };

    let internal_modifier: TextObjectModifier = match modifier {
        FfiTextObjectModifier::Inner => TextObjectModifier::Inner,
        FfiTextObjectModifier::Around => TextObjectModifier::Around,
    };

    engine
        .text_object_range(internal_text_object, internal_modifier)
        .map(|(start, end)| FfiTextRange {
            start: start as u64,
            end: end as u64,
        })
}

/// Check if a motion is linewise (affects whole lines).
#[uniffi::export]
pub fn is_motion_linewise(motion: FfiMotion) -> bool {
    let internal_motion: crate::motion::Motion = motion.into();
    internal_motion.is_linewise()
}

/// Check if a motion is inclusive (includes the character at the end).
#[uniffi::export]
pub fn is_motion_inclusive(motion: FfiMotion) -> bool {
    let internal_motion: crate::motion::Motion = motion.into();
    internal_motion.is_inclusive()
}
