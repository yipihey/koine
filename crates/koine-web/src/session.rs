//! What every page shares: the token held, the last refusal, and the one
//! style host.

use dioxus::prelude::*;
use koine::style::StyleHost;

use crate::api::ApiError;
use crate::token::Token;

/// Provided once by the root as context. Every field is a signal, so the
/// struct is `Copy` and can be moved into any handler or task.
#[derive(Clone, Copy)]
pub struct Session {
    /// The loopback token, when one is held (plan D8). Without it the page is
    /// read-only.
    pub token: Signal<Option<Token>>,
    /// The last refusal, drawn in the status line as `code: message`.
    pub refusal: Signal<Option<ApiError>>,
    /// The sheet this page applies (`web`). The style page edits it and the
    /// surface page reads it, so an edit is visible on the next paint. Nothing
    /// persists it (plan D12).
    pub style: Signal<StyleHost>,
}

impl Session {
    pub fn new() -> Self {
        Self {
            token: Signal::new(None),
            refusal: Signal::new(None),
            style: Signal::new(StyleHost::new("web").expect("web is a koine frontend")),
        }
    }

    /// Record a refusal.
    pub fn refuse(mut self, error: ApiError) {
        if self.refusal.peek().as_ref() != Some(&error) {
            self.refusal.set(Some(error));
        }
    }

    /// An answer came back ok: the last refusal no longer describes the page.
    pub fn clear(mut self) {
        if self.refusal.peek().is_some() {
            self.refusal.set(None);
        }
    }
}
