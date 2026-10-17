//! Helix-style modal editing for terminal text interfaces.
//!
//! This crate provides a Rust implementation of Helix-style modal editing,
//! designed for use in terminal applications and via UniFFI bindings in
//! native Swift apps.
//!
//! # Features
//!
//! - **Modal editing**: Normal, Insert, and Select modes
//! - **Operator + Motion**: Vim-style compositions like `dw`, `c$`, `y2j`
//! - **Text Objects**: Inner/Around text objects like `diw`, `ci"`, `da(`
//! - **Space-Mode**: Helix-style application command menu
//! - **Trie-based Keymap**: Efficient multi-key sequences with which-key support
//!
//! # UniFFI Bindings
//!
//! Enable the `ffi` feature to generate Swift/Kotlin bindings:
//!
//! ```toml
//! impress-helix = { version = "0.1", features = ["ffi"] }
//! ```
//!
//! # Example
//!
//! ```
//! use impress_helix::{HelixState, HelixMode, HelixKeyHandler, HelixKeyResult, KeyModifiers};
//!
//! let mut state = HelixState::new();
//! let mut handler = HelixKeyHandler::new();
//! let modifiers = KeyModifiers::default();
//!
//! // Handle a key press
//! match handler.handle_key('j', state.mode(), &modifiers) {
//!     HelixKeyResult::Command(command) => {
//!         println!("Got command: {:?}", command);
//!     }
//!     HelixKeyResult::Pending => {
//!         println!("Waiting for more input...");
//!     }
//!     _ => {}
//! }
//! ```

#[cfg(feature = "ffi")]
uniffi::setup_scaffolding!();

mod command;
mod key_handler;
pub mod keymap;
mod mode;
pub mod motion;
pub mod space;
mod state;
pub mod text_engine;
pub mod text_object;

// Re-exports for FFI
#[cfg(feature = "ffi")]
pub mod ffi;

pub use command::HelixCommand;
pub use key_handler::{
    HelixKeyHandler, HelixKeyResult, KeyModifiers, PendingCharacterOperation, PendingOperator,
};
pub use keymap::{KeyEvent, KeyTrie, KeyTrieNode, Keymap, KeymapResult, MappableCommand};
pub use mode::HelixMode;
pub use motion::Motion;
pub use space::{build_space_mode_keymap, SpaceCommand};
pub use state::{HelixState, KeyHandleResult};
pub use text_engine::HelixTextEngine;
pub use text_object::{TextObject, TextObjectModifier};
