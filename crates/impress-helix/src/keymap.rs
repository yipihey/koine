//! Trie-based keymap for Helix-style editor.
//!
//! This module provides a trie (prefix tree) structure for efficient multi-key
//! sequence handling, which is the foundation for space-mode and other nested
//! command menus (which-key style).

use std::collections::HashMap;

use crate::command::HelixCommand;
use crate::space::SpaceCommand;

/// A key event that can be matched in the keymap trie.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyEvent {
    /// The key character (or special key code).
    pub key: char,
    /// Whether shift modifier is held.
    pub shift: bool,
    /// Whether control modifier is held.
    pub ctrl: bool,
    /// Whether alt/option modifier is held.
    pub alt: bool,
}

impl KeyEvent {
    /// Create a simple key event with no modifiers.
    pub fn new(key: char) -> Self {
        Self {
            key,
            shift: false,
            ctrl: false,
            alt: false,
        }
    }

    /// Create a key event with shift modifier.
    pub fn shift(key: char) -> Self {
        Self {
            key,
            shift: true,
            ctrl: false,
            alt: false,
        }
    }

    /// Create a key event with control modifier.
    pub fn ctrl(key: char) -> Self {
        Self {
            key,
            shift: false,
            ctrl: true,
            alt: false,
        }
    }

    /// Create a key event with alt modifier.
    pub fn alt(key: char) -> Self {
        Self {
            key,
            shift: false,
            ctrl: false,
            alt: true,
        }
    }

    /// Returns a display string for this key (e.g., "C-x", "S-a", "Space").
    pub fn display(&self) -> String {
        let mut result = String::new();
        if self.ctrl {
            result.push_str("C-");
        }
        if self.alt {
            result.push_str("A-");
        }
        if self.shift && !self.key.is_uppercase() {
            result.push_str("S-");
        }

        match self.key {
            ' ' => result.push_str("Space"),
            '\t' => result.push_str("Tab"),
            '\n' | '\r' => result.push_str("Enter"),
            '\x1b' => result.push_str("Esc"),
            '\x7f' => result.push_str("Backspace"),
            c => result.push(c),
        }
        result
    }
}

/// A mappable command that can be bound to a key sequence.
#[derive(Debug, Clone, PartialEq)]
pub enum MappableCommand {
    /// A standard Helix editing command.
    Helix(HelixCommand),
    /// A space-mode command (application-level operation).
    Space(SpaceCommand),
    /// A typed command (e.g., `:w`, `:q`).
    Typed(String),
    /// A macro (sequence of other commands).
    Macro(Vec<MappableCommand>),
}

impl MappableCommand {
    /// Returns a description for which-key display.
    pub fn description(&self) -> String {
        match self {
            MappableCommand::Helix(cmd) => cmd.description().to_string(),
            MappableCommand::Space(cmd) => cmd.description().to_string(),
            MappableCommand::Typed(s) => format!(":{}", s),
            MappableCommand::Macro(_) => "macro".to_string(),
        }
    }
}

/// A node in the keymap trie.
#[derive(Debug, Clone)]
pub enum KeyTrieNode {
    /// A leaf node containing a command to execute.
    Leaf(MappableCommand),
    /// An internal node containing a sub-trie (menu).
    Node(KeyTrie),
}

impl KeyTrieNode {
    /// Returns true if this is a leaf node.
    pub fn is_leaf(&self) -> bool {
        matches!(self, KeyTrieNode::Leaf(_))
    }

    /// Returns the command if this is a leaf, None otherwise.
    pub fn command(&self) -> Option<&MappableCommand> {
        match self {
            KeyTrieNode::Leaf(cmd) => Some(cmd),
            KeyTrieNode::Node(_) => None,
        }
    }

    /// Returns the sub-trie if this is a node, None otherwise.
    pub fn sub_trie(&self) -> Option<&KeyTrie> {
        match self {
            KeyTrieNode::Leaf(_) => None,
            KeyTrieNode::Node(trie) => Some(trie),
        }
    }
}

/// A trie (prefix tree) for efficient key sequence matching.
///
/// The trie enables:
/// - Multi-key sequences (e.g., Space → f → o = file open)
/// - Auto-generated which-key content
/// - Sticky modes that stay in a menu until Escape
#[derive(Debug, Clone)]
pub struct KeyTrie {
    /// Map from key events to child nodes.
    map: HashMap<KeyEvent, KeyTrieNode>,
    /// Whether this trie is "sticky" (stay in menu after command execution).
    sticky: bool,
    /// Name of this trie node (for which-key display).
    name: Option<String>,
    /// Order hint for displaying keys (preserves insertion order for which-key).
    order: Vec<KeyEvent>,
}

impl Default for KeyTrie {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyTrie {
    /// Create a new empty trie.
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
            sticky: false,
            name: None,
            order: Vec::new(),
        }
    }

    /// Create a new trie with a name.
    pub fn with_name(name: impl Into<String>) -> Self {
        Self {
            map: HashMap::new(),
            sticky: false,
            name: Some(name.into()),
            order: Vec::new(),
        }
    }

    /// Set whether this trie is sticky.
    pub fn set_sticky(&mut self, sticky: bool) {
        self.sticky = sticky;
    }

    /// Returns whether this trie is sticky.
    pub fn is_sticky(&self) -> bool {
        self.sticky
    }

    /// Returns the name of this trie.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Insert a binding at a key.
    pub fn insert(&mut self, key: KeyEvent, node: KeyTrieNode) {
        if !self.map.contains_key(&key) {
            self.order.push(key);
        }
        self.map.insert(key, node);
    }

    /// Insert a leaf command at a key.
    pub fn insert_command(&mut self, key: KeyEvent, command: MappableCommand) {
        self.insert(key, KeyTrieNode::Leaf(command));
    }

    /// Insert a sub-trie at a key.
    pub fn insert_trie(&mut self, key: KeyEvent, trie: KeyTrie) {
        self.insert(key, KeyTrieNode::Node(trie));
    }

    /// Get the node at a key, if it exists.
    pub fn get(&self, key: &KeyEvent) -> Option<&KeyTrieNode> {
        self.map.get(key)
    }

    /// Traverse the trie with a sequence of keys.
    ///
    /// Returns the final node reached after consuming all keys, or None if the
    /// sequence doesn't match any path in the trie.
    pub fn traverse(&self, keys: &[KeyEvent]) -> Option<&KeyTrieNode> {
        if keys.is_empty() {
            return None;
        }

        let first = &keys[0];
        let rest = &keys[1..];

        match self.map.get(first) {
            Some(KeyTrieNode::Leaf(cmd)) if rest.is_empty() => Some(&self.map[first]),
            Some(KeyTrieNode::Node(sub_trie)) if rest.is_empty() => Some(&self.map[first]),
            Some(KeyTrieNode::Node(sub_trie)) => sub_trie.traverse(rest),
            _ => None,
        }
    }

    /// Returns available keys and their descriptions for which-key display.
    ///
    /// This is the primary API for building which-key popups - it returns all
    /// keys that are valid at the current position along with human-readable
    /// descriptions of what they do.
    pub fn available_keys(&self) -> Vec<(KeyEvent, String)> {
        self.order
            .iter()
            .filter_map(|key| {
                self.map.get(key).map(|node| {
                    let desc = match node {
                        KeyTrieNode::Leaf(cmd) => cmd.description(),
                        KeyTrieNode::Node(trie) => {
                            trie.name.clone().unwrap_or_else(|| "+menu".to_string())
                        }
                    };
                    (*key, desc)
                })
            })
            .collect()
    }

    /// Returns true if this trie contains the given key.
    pub fn contains_key(&self, key: &KeyEvent) -> bool {
        self.map.contains_key(key)
    }

    /// Returns the number of direct children.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Returns true if this trie is empty.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// Result of looking up a key sequence in the keymap.
#[derive(Debug, Clone, PartialEq)]
pub enum KeymapResult {
    /// The sequence matched a command.
    Matched(MappableCommand),
    /// The sequence is a valid prefix (more keys needed).
    Pending(Vec<(KeyEvent, String)>),
    /// The sequence doesn't match anything.
    NotFound,
    /// Cancel/escape was pressed.
    Cancelled,
}

/// A complete keymap containing tries for each mode.
#[derive(Debug, Clone)]
pub struct Keymap {
    /// Normal mode keymap.
    pub normal: KeyTrie,
    /// Insert mode keymap.
    pub insert: KeyTrie,
    /// Select mode keymap.
    pub select: KeyTrie,
    /// Current key sequence being built.
    pending_keys: Vec<KeyEvent>,
    /// Current position in the trie (for multi-key sequences).
    current_trie: Option<KeyTrie>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self::new()
    }
}

impl Keymap {
    /// Create a new empty keymap.
    pub fn new() -> Self {
        Self {
            normal: KeyTrie::new(),
            insert: KeyTrie::new(),
            select: KeyTrie::new(),
            pending_keys: Vec::new(),
            current_trie: None,
        }
    }

    /// Reset pending state.
    pub fn reset(&mut self) {
        self.pending_keys.clear();
        self.current_trie = None;
    }

    /// Returns the pending key sequence.
    pub fn pending_keys(&self) -> &[KeyEvent] {
        &self.pending_keys
    }

    /// Look up a key in the specified mode's keymap.
    ///
    /// This handles multi-key sequences by tracking pending state. Returns:
    /// - `Matched(cmd)` when a full sequence is matched
    /// - `Pending(keys)` when more keys are needed (includes available keys for which-key)
    /// - `NotFound` when the sequence doesn't match
    /// - `Cancelled` when Escape was pressed
    pub fn lookup(&mut self, key: KeyEvent, mode_trie: &KeyTrie) -> KeymapResult {
        // Handle escape to cancel pending sequence
        if key.key == '\x1b' {
            self.reset();
            return KeymapResult::Cancelled;
        }

        // Determine which trie to search
        let trie = self.current_trie.as_ref().unwrap_or(mode_trie);

        match trie.get(&key) {
            Some(KeyTrieNode::Leaf(cmd)) => {
                let result = cmd.clone();
                // If current trie is sticky, don't reset
                if !trie.is_sticky() {
                    self.reset();
                }
                KeymapResult::Matched(result)
            }
            Some(KeyTrieNode::Node(sub_trie)) => {
                self.pending_keys.push(key);
                let available = sub_trie.available_keys();
                self.current_trie = Some(sub_trie.clone());
                KeymapResult::Pending(available)
            }
            None => {
                self.reset();
                KeymapResult::NotFound
            }
        }
    }

    /// Returns available keys at the current position for which-key display.
    pub fn available_keys(&self, mode_trie: &KeyTrie) -> Vec<(KeyEvent, String)> {
        let trie = self.current_trie.as_ref().unwrap_or(mode_trie);
        trie.available_keys()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_key_event_display() {
        assert_eq!(KeyEvent::new('a').display(), "a");
        assert_eq!(KeyEvent::new(' ').display(), "Space");
        assert_eq!(KeyEvent::ctrl('x').display(), "C-x");
        assert_eq!(KeyEvent::alt('a').display(), "A-a");
    }

    #[test]
    fn test_trie_insert_and_lookup() {
        let mut trie = KeyTrie::new();
        trie.insert_command(
            KeyEvent::new('d'),
            MappableCommand::Helix(HelixCommand::Delete),
        );

        assert!(trie.contains_key(&KeyEvent::new('d')));
        assert!(!trie.contains_key(&KeyEvent::new('x')));

        let node = trie.get(&KeyEvent::new('d')).unwrap();
        assert!(node.is_leaf());
    }

    #[test]
    fn test_nested_trie() {
        let mut space_trie = KeyTrie::with_name("space");

        let mut file_trie = KeyTrie::with_name("file");
        file_trie.insert_command(
            KeyEvent::new('s'),
            MappableCommand::Space(SpaceCommand::FileSave),
        );
        file_trie.insert_command(
            KeyEvent::new('o'),
            MappableCommand::Space(SpaceCommand::FileOpen),
        );

        space_trie.insert_trie(KeyEvent::new('f'), file_trie);

        // Traverse Space -> f -> s
        let keys = [KeyEvent::new('f'), KeyEvent::new('s')];
        let result = space_trie.traverse(&keys);
        assert!(result.is_some());
        assert!(result.unwrap().is_leaf());
    }

    #[test]
    fn test_available_keys() {
        let mut trie = KeyTrie::new();
        trie.insert_command(
            KeyEvent::new('d'),
            MappableCommand::Helix(HelixCommand::Delete),
        );
        trie.insert_command(
            KeyEvent::new('y'),
            MappableCommand::Helix(HelixCommand::Yank),
        );

        let available = trie.available_keys();
        assert_eq!(available.len(), 2);
        assert_eq!(available[0].0, KeyEvent::new('d'));
        assert_eq!(available[1].0, KeyEvent::new('y'));
    }

    #[test]
    fn test_keymap_lookup() {
        let mut keymap = Keymap::new();

        let mut file_trie = KeyTrie::with_name("file");
        file_trie.insert_command(
            KeyEvent::new('s'),
            MappableCommand::Space(SpaceCommand::FileSave),
        );

        keymap
            .normal
            .insert_trie(KeyEvent::new(' '), file_trie.clone());
        keymap.normal.insert_command(
            KeyEvent::new('d'),
            MappableCommand::Helix(HelixCommand::Delete),
        );

        // Single key
        let result = keymap.lookup(KeyEvent::new('d'), &keymap.normal.clone());
        assert!(matches!(result, KeymapResult::Matched(_)));

        // Multi-key sequence
        keymap.reset();
        let result = keymap.lookup(KeyEvent::new(' '), &keymap.normal.clone());
        assert!(matches!(result, KeymapResult::Pending(_)));

        let result = keymap.lookup(KeyEvent::new('s'), &keymap.normal.clone());
        assert!(matches!(result, KeymapResult::Matched(_)));
    }
}
