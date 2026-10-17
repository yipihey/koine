//! Space-mode commands for Helix-style editor.
//!
//! Space-mode provides application-level commands accessible via the Space key,
//! organized into logical groups (file, buffer, window, etc.).

use crate::keymap::{KeyEvent, KeyTrie, MappableCommand};

/// Space-mode commands for application-level operations.
///
/// These commands are typically handled by the application (editor, IDE) rather
/// than the text engine, and are organized into the Helix-style space menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpaceCommand {
    // File operations (Space f)
    /// Open file picker.
    FileOpen,
    /// Open recent files picker.
    FileRecent,
    /// Save current file.
    FileSave,
    /// Save file with new name.
    FileSaveAs,
    /// Close current file.
    FileClose,
    /// Close all files.
    FileCloseAll,

    // Buffer operations (Space b)
    /// Open buffer picker.
    BufferPicker,
    /// Switch to next buffer.
    BufferNext,
    /// Switch to previous buffer.
    BufferPrev,
    /// Close current buffer.
    BufferClose,
    /// Close all buffers except current.
    BufferCloseOthers,
    /// Revert buffer to last saved state.
    BufferRevert,

    // Window operations (Space w)
    /// Split window horizontally.
    WindowSplitHorizontal,
    /// Split window vertically.
    WindowSplitVertical,
    /// Close current window.
    WindowClose,
    /// Close all windows except current.
    WindowOnly,
    /// Focus window to the left.
    WindowFocusLeft,
    /// Focus window below.
    WindowFocusDown,
    /// Focus window above.
    WindowFocusUp,
    /// Focus window to the right.
    WindowFocusRight,
    /// Swap with window to the left.
    WindowSwapLeft,
    /// Swap with window below.
    WindowSwapDown,
    /// Swap with window above.
    WindowSwapUp,
    /// Swap with window to the right.
    WindowSwapRight,

    // Search/Symbol operations (Space s)
    /// Document symbol picker.
    SymbolPicker,
    /// Workspace symbol picker.
    WorkspaceSymbolPicker,
    /// Global text search (grep).
    GlobalSearch,
    /// Search in current file.
    SearchInFile,

    // Git operations (Space g)
    /// Git status.
    GitStatus,
    /// Git diff for current file.
    GitDiff,
    /// Git blame for current file.
    GitBlame,
    /// Git log.
    GitLog,
    /// Stage current file.
    GitStage,
    /// Unstage current file.
    GitUnstage,

    // Diagnostics (Space d)
    /// Show diagnostics for current file.
    DiagnosticsList,
    /// Go to next diagnostic.
    DiagnosticNext,
    /// Go to previous diagnostic.
    DiagnosticPrev,

    // Code actions (Space a / Space c)
    /// Show code actions.
    CodeAction,
    /// Rename symbol.
    Rename,
    /// Format document.
    Format,

    // Help (Space ?)
    /// Show all keybindings.
    Help,

    // Misc
    /// Open command palette.
    CommandPalette,
    /// Toggle file tree.
    ToggleFileTree,
    /// Toggle terminal.
    ToggleTerminal,
}

impl SpaceCommand {
    /// Returns a description for which-key display.
    pub fn description(&self) -> &'static str {
        match self {
            // File
            SpaceCommand::FileOpen => "File picker",
            SpaceCommand::FileRecent => "Recent files",
            SpaceCommand::FileSave => "Save",
            SpaceCommand::FileSaveAs => "Save as",
            SpaceCommand::FileClose => "Close file",
            SpaceCommand::FileCloseAll => "Close all files",

            // Buffer
            SpaceCommand::BufferPicker => "Buffer picker",
            SpaceCommand::BufferNext => "Next buffer",
            SpaceCommand::BufferPrev => "Previous buffer",
            SpaceCommand::BufferClose => "Close buffer",
            SpaceCommand::BufferCloseOthers => "Close other buffers",
            SpaceCommand::BufferRevert => "Revert buffer",

            // Window
            SpaceCommand::WindowSplitHorizontal => "Split horizontal",
            SpaceCommand::WindowSplitVertical => "Split vertical",
            SpaceCommand::WindowClose => "Close window",
            SpaceCommand::WindowOnly => "Close other windows",
            SpaceCommand::WindowFocusLeft => "Focus left",
            SpaceCommand::WindowFocusDown => "Focus down",
            SpaceCommand::WindowFocusUp => "Focus up",
            SpaceCommand::WindowFocusRight => "Focus right",
            SpaceCommand::WindowSwapLeft => "Swap left",
            SpaceCommand::WindowSwapDown => "Swap down",
            SpaceCommand::WindowSwapUp => "Swap up",
            SpaceCommand::WindowSwapRight => "Swap right",

            // Search/Symbol
            SpaceCommand::SymbolPicker => "Symbol picker",
            SpaceCommand::WorkspaceSymbolPicker => "Workspace symbols",
            SpaceCommand::GlobalSearch => "Global search",
            SpaceCommand::SearchInFile => "Search in file",

            // Git
            SpaceCommand::GitStatus => "Git status",
            SpaceCommand::GitDiff => "Git diff",
            SpaceCommand::GitBlame => "Git blame",
            SpaceCommand::GitLog => "Git log",
            SpaceCommand::GitStage => "Stage file",
            SpaceCommand::GitUnstage => "Unstage file",

            // Diagnostics
            SpaceCommand::DiagnosticsList => "Diagnostics list",
            SpaceCommand::DiagnosticNext => "Next diagnostic",
            SpaceCommand::DiagnosticPrev => "Previous diagnostic",

            // Code
            SpaceCommand::CodeAction => "Code actions",
            SpaceCommand::Rename => "Rename symbol",
            SpaceCommand::Format => "Format document",

            // Help
            SpaceCommand::Help => "Show help",

            // Misc
            SpaceCommand::CommandPalette => "Command palette",
            SpaceCommand::ToggleFileTree => "Toggle file tree",
            SpaceCommand::ToggleTerminal => "Toggle terminal",
        }
    }

    /// Returns the menu category for this command.
    pub fn category(&self) -> &'static str {
        match self {
            SpaceCommand::FileOpen
            | SpaceCommand::FileRecent
            | SpaceCommand::FileSave
            | SpaceCommand::FileSaveAs
            | SpaceCommand::FileClose
            | SpaceCommand::FileCloseAll => "file",

            SpaceCommand::BufferPicker
            | SpaceCommand::BufferNext
            | SpaceCommand::BufferPrev
            | SpaceCommand::BufferClose
            | SpaceCommand::BufferCloseOthers
            | SpaceCommand::BufferRevert => "buffer",

            SpaceCommand::WindowSplitHorizontal
            | SpaceCommand::WindowSplitVertical
            | SpaceCommand::WindowClose
            | SpaceCommand::WindowOnly
            | SpaceCommand::WindowFocusLeft
            | SpaceCommand::WindowFocusDown
            | SpaceCommand::WindowFocusUp
            | SpaceCommand::WindowFocusRight
            | SpaceCommand::WindowSwapLeft
            | SpaceCommand::WindowSwapDown
            | SpaceCommand::WindowSwapUp
            | SpaceCommand::WindowSwapRight => "window",

            SpaceCommand::SymbolPicker
            | SpaceCommand::WorkspaceSymbolPicker
            | SpaceCommand::GlobalSearch
            | SpaceCommand::SearchInFile => "search",

            SpaceCommand::GitStatus
            | SpaceCommand::GitDiff
            | SpaceCommand::GitBlame
            | SpaceCommand::GitLog
            | SpaceCommand::GitStage
            | SpaceCommand::GitUnstage => "git",

            SpaceCommand::DiagnosticsList
            | SpaceCommand::DiagnosticNext
            | SpaceCommand::DiagnosticPrev => "diagnostics",

            SpaceCommand::CodeAction | SpaceCommand::Rename | SpaceCommand::Format => "code",

            SpaceCommand::Help => "help",

            SpaceCommand::CommandPalette
            | SpaceCommand::ToggleFileTree
            | SpaceCommand::ToggleTerminal => "misc",
        }
    }
}

/// Build the default space-mode keymap trie.
///
/// This creates the full Helix-style space menu structure:
/// ```text
/// Space
/// ├── f - File
/// │   ├── f - File picker
/// │   ├── r - Recent files
/// │   ├── s - Save
/// │   ├── S - Save as
/// │   └── c - Close
/// ├── b - Buffer
/// │   ├── b - Buffer picker
/// │   ├── n - Next buffer
/// │   ├── p - Previous buffer
/// │   └── d - Delete buffer
/// ├── w - Window
/// │   ├── s - Split horizontal
/// │   ├── v - Split vertical
/// │   ├── h/j/k/l - Focus window
/// │   └── q - Close window
/// ├── s - Search/Symbol
/// │   ├── s - Symbol picker
/// │   ├── S - Workspace symbol
/// │   └── / - Global search
/// ├── g - Git
/// │   ├── g - Status
/// │   ├── d - Diff
/// │   └── b - Blame
/// └── ? - Help
/// ```
pub fn build_space_mode_keymap() -> KeyTrie {
    let mut space = KeyTrie::with_name("space");

    // File menu (Space f)
    let mut file = KeyTrie::with_name("file");
    file.insert_command(
        KeyEvent::new('f'),
        MappableCommand::Space(SpaceCommand::FileOpen),
    );
    file.insert_command(
        KeyEvent::new('r'),
        MappableCommand::Space(SpaceCommand::FileRecent),
    );
    file.insert_command(
        KeyEvent::new('s'),
        MappableCommand::Space(SpaceCommand::FileSave),
    );
    file.insert_command(
        KeyEvent::shift('S'),
        MappableCommand::Space(SpaceCommand::FileSaveAs),
    );
    file.insert_command(
        KeyEvent::new('c'),
        MappableCommand::Space(SpaceCommand::FileClose),
    );
    file.insert_command(
        KeyEvent::shift('C'),
        MappableCommand::Space(SpaceCommand::FileCloseAll),
    );
    space.insert_trie(KeyEvent::new('f'), file);

    // Buffer menu (Space b)
    let mut buffer = KeyTrie::with_name("buffer");
    buffer.insert_command(
        KeyEvent::new('b'),
        MappableCommand::Space(SpaceCommand::BufferPicker),
    );
    buffer.insert_command(
        KeyEvent::new('n'),
        MappableCommand::Space(SpaceCommand::BufferNext),
    );
    buffer.insert_command(
        KeyEvent::new('p'),
        MappableCommand::Space(SpaceCommand::BufferPrev),
    );
    buffer.insert_command(
        KeyEvent::new('d'),
        MappableCommand::Space(SpaceCommand::BufferClose),
    );
    buffer.insert_command(
        KeyEvent::shift('D'),
        MappableCommand::Space(SpaceCommand::BufferCloseOthers),
    );
    buffer.insert_command(
        KeyEvent::new('r'),
        MappableCommand::Space(SpaceCommand::BufferRevert),
    );
    space.insert_trie(KeyEvent::new('b'), buffer);

    // Window menu (Space w)
    let mut window = KeyTrie::with_name("window");
    window.insert_command(
        KeyEvent::new('s'),
        MappableCommand::Space(SpaceCommand::WindowSplitHorizontal),
    );
    window.insert_command(
        KeyEvent::new('v'),
        MappableCommand::Space(SpaceCommand::WindowSplitVertical),
    );
    window.insert_command(
        KeyEvent::new('q'),
        MappableCommand::Space(SpaceCommand::WindowClose),
    );
    window.insert_command(
        KeyEvent::new('o'),
        MappableCommand::Space(SpaceCommand::WindowOnly),
    );
    window.insert_command(
        KeyEvent::new('h'),
        MappableCommand::Space(SpaceCommand::WindowFocusLeft),
    );
    window.insert_command(
        KeyEvent::new('j'),
        MappableCommand::Space(SpaceCommand::WindowFocusDown),
    );
    window.insert_command(
        KeyEvent::new('k'),
        MappableCommand::Space(SpaceCommand::WindowFocusUp),
    );
    window.insert_command(
        KeyEvent::new('l'),
        MappableCommand::Space(SpaceCommand::WindowFocusRight),
    );
    window.insert_command(
        KeyEvent::shift('H'),
        MappableCommand::Space(SpaceCommand::WindowSwapLeft),
    );
    window.insert_command(
        KeyEvent::shift('J'),
        MappableCommand::Space(SpaceCommand::WindowSwapDown),
    );
    window.insert_command(
        KeyEvent::shift('K'),
        MappableCommand::Space(SpaceCommand::WindowSwapUp),
    );
    window.insert_command(
        KeyEvent::shift('L'),
        MappableCommand::Space(SpaceCommand::WindowSwapRight),
    );
    space.insert_trie(KeyEvent::new('w'), window);

    // Search/Symbol menu (Space s)
    let mut search = KeyTrie::with_name("search");
    search.insert_command(
        KeyEvent::new('s'),
        MappableCommand::Space(SpaceCommand::SymbolPicker),
    );
    search.insert_command(
        KeyEvent::shift('S'),
        MappableCommand::Space(SpaceCommand::WorkspaceSymbolPicker),
    );
    search.insert_command(
        KeyEvent::new('/'),
        MappableCommand::Space(SpaceCommand::GlobalSearch),
    );
    search.insert_command(
        KeyEvent::new('f'),
        MappableCommand::Space(SpaceCommand::SearchInFile),
    );
    space.insert_trie(KeyEvent::new('s'), search);

    // Git menu (Space g)
    let mut git = KeyTrie::with_name("git");
    git.insert_command(
        KeyEvent::new('g'),
        MappableCommand::Space(SpaceCommand::GitStatus),
    );
    git.insert_command(
        KeyEvent::new('d'),
        MappableCommand::Space(SpaceCommand::GitDiff),
    );
    git.insert_command(
        KeyEvent::new('b'),
        MappableCommand::Space(SpaceCommand::GitBlame),
    );
    git.insert_command(
        KeyEvent::new('l'),
        MappableCommand::Space(SpaceCommand::GitLog),
    );
    git.insert_command(
        KeyEvent::new('s'),
        MappableCommand::Space(SpaceCommand::GitStage),
    );
    git.insert_command(
        KeyEvent::new('u'),
        MappableCommand::Space(SpaceCommand::GitUnstage),
    );
    space.insert_trie(KeyEvent::new('g'), git);

    // Diagnostics menu (Space d)
    let mut diagnostics = KeyTrie::with_name("diagnostics");
    diagnostics.insert_command(
        KeyEvent::new('d'),
        MappableCommand::Space(SpaceCommand::DiagnosticsList),
    );
    diagnostics.insert_command(
        KeyEvent::new('n'),
        MappableCommand::Space(SpaceCommand::DiagnosticNext),
    );
    diagnostics.insert_command(
        KeyEvent::new('p'),
        MappableCommand::Space(SpaceCommand::DiagnosticPrev),
    );
    space.insert_trie(KeyEvent::new('d'), diagnostics);

    // Code menu (Space c)
    let mut code = KeyTrie::with_name("code");
    code.insert_command(
        KeyEvent::new('a'),
        MappableCommand::Space(SpaceCommand::CodeAction),
    );
    code.insert_command(
        KeyEvent::new('r'),
        MappableCommand::Space(SpaceCommand::Rename),
    );
    code.insert_command(
        KeyEvent::new('f'),
        MappableCommand::Space(SpaceCommand::Format),
    );
    space.insert_trie(KeyEvent::new('c'), code);

    // Top-level shortcuts
    space.insert_command(
        KeyEvent::new('?'),
        MappableCommand::Space(SpaceCommand::Help),
    );
    space.insert_command(
        KeyEvent::new(':'),
        MappableCommand::Space(SpaceCommand::CommandPalette),
    );
    space.insert_command(
        KeyEvent::new('e'),
        MappableCommand::Space(SpaceCommand::ToggleFileTree),
    );
    space.insert_command(
        KeyEvent::new('t'),
        MappableCommand::Space(SpaceCommand::ToggleTerminal),
    );

    space
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::KeyTrieNode;

    #[test]
    fn test_space_command_description() {
        assert_eq!(SpaceCommand::FileOpen.description(), "File picker");
        assert_eq!(SpaceCommand::GitStatus.description(), "Git status");
        assert_eq!(SpaceCommand::Help.description(), "Show help");
    }

    #[test]
    fn test_space_command_category() {
        assert_eq!(SpaceCommand::FileOpen.category(), "file");
        assert_eq!(SpaceCommand::BufferNext.category(), "buffer");
        assert_eq!(SpaceCommand::GitDiff.category(), "git");
    }

    #[test]
    fn test_build_space_mode_keymap() {
        let space = build_space_mode_keymap();

        // Check file submenu exists
        let file_node = space.get(&KeyEvent::new('f'));
        assert!(file_node.is_some());
        assert!(matches!(file_node.unwrap(), KeyTrieNode::Node(_)));

        // Check traversal works
        let keys = [KeyEvent::new('f'), KeyEvent::new('s')];
        let result = space.traverse(&keys);
        assert!(result.is_some());
        assert!(result.unwrap().is_leaf());
    }

    #[test]
    fn test_available_keys_in_submenu() {
        let space = build_space_mode_keymap();

        if let Some(KeyTrieNode::Node(file_trie)) = space.get(&KeyEvent::new('f')) {
            let available = file_trie.available_keys();
            assert!(!available.is_empty());

            // Check that 's' for save is available
            let has_save = available.iter().any(|(k, _)| k.key == 's');
            assert!(has_save);
        } else {
            panic!("File submenu should exist");
        }
    }
}
