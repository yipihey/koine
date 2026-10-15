//! What every page shares: the token held, the last refusal, the one style
//! host, and which tabs the host can serve (docs/plan-koine-affordances.md
//! A-D4).

use dioxus::prelude::*;
use koine::style::StyleHost;

use crate::api::{self, ApiError, LayoutTree};
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
    /// Whether that refusal came from a background read (a render, the wait
    /// loop) rather than something the person did. A read that succeeds
    /// clears only a read's refusal, so the reason a click was refused stays
    /// on screen until the next action, not until the next poll.
    pub refusal_is_read: Signal<bool>,
    /// The sheet this page applies (`web`). The style page edits it and the
    /// surface page reads it, so an edit is visible on the next paint. Nothing
    /// persists it (plan D12).
    pub style: Signal<StyleHost>,
    /// Whether the nav shows the layout tab ([`layout_tab`]). Hidden until
    /// the host has answered; the root checks once at startup
    /// ([`Session::check_layout`]) and the layout page's poll re-checks
    /// ([`Session::saw_layout`]).
    pub layout_tab: Signal<bool>,
    /// Whether the nav shows the ask tab: the host links
    /// `agent-service_ask` ([`Session::check_ask`]).
    pub ask_tab: Signal<bool>,
}

/// What the host answered about its layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutAnswer {
    /// `GET /api/layout/tree` answered, so the host serves the layout verbs;
    /// `true` when at least one tile is one the page can draw
    /// (`layout_page::drawable`).
    Tree(bool),
    /// The tree did not answer and the host says the layout verb is not
    /// linked (`api::verb_linked` answered `false`).
    NotLinked,
    /// Nothing either way: the transport failed, or the answer was another
    /// refusal.
    Failed,
}

impl LayoutAnswer {
    /// The answer one tree fetch gives on its own: a failure is
    /// [`LayoutAnswer::Failed`], since only the probe can say "not linked".
    pub fn of_tree(answer: &Result<LayoutTree, ApiError>) -> Self {
        match answer {
            Ok(tree) => Self::Tree(crate::layout_page::drawable(&tree.layout)),
            Err(_) => Self::Failed,
        }
    }
}

/// Whether the layout tab is shown, given whether it is shown now and the
/// host's latest answer (A-D4): shown when the host serves the layout verbs
/// and the layout has a tile the page can draw; hidden when it does not or
/// has nothing drawable; and as it was when a fetch fails, so a shown tab does
/// not blink out on a dropped request.
pub fn layout_tab(shown: bool, answer: LayoutAnswer) -> bool {
    match answer {
        LayoutAnswer::Tree(drawable) => drawable,
        LayoutAnswer::NotLinked => false,
        LayoutAnswer::Failed => shown,
    }
}

impl Session {
    pub fn new() -> Self {
        Self {
            token: Signal::new(None),
            refusal: Signal::new(None),
            refusal_is_read: Signal::new(false),
            style: Signal::new(StyleHost::new("web").expect("web is a koine frontend")),
            layout_tab: Signal::new(false),
            ask_tab: Signal::new(false),
        }
    }

    /// Fold one answer into [`Session::layout_tab`].
    pub fn saw_layout(mut self, answer: LayoutAnswer) {
        let next = layout_tab(*self.layout_tab.peek(), answer);
        if *self.layout_tab.peek() != next {
            self.layout_tab.set(next);
        }
    }

    /// The startup check: the tree, and when it does not answer, whether the
    /// host links the layout verb at all. On a host that answers the tree
    /// (koine-host, the Swift app) the tree is the proof; the probe only tells
    /// "not linked" from "not reachable".
    pub async fn check_layout(self) {
        let answer = match api::layout_tree().await {
            Ok(tree) => LayoutAnswer::Tree(crate::layout_page::drawable(&tree.layout)),
            Err(_) => match api::verb_linked(api::LAYOUT_VERB).await {
                Ok(false) => LayoutAnswer::NotLinked,
                Ok(true) | Err(_) => LayoutAnswer::Failed,
            },
        };
        self.saw_layout(answer);
    }

    /// Whether the host links the desk's ask verb: the ask tab is shown when
    /// it does, and stays hidden when the probe fails.
    pub async fn check_ask(mut self) {
        let linked = matches!(api::verb_linked(crate::ask_page::ASK_VERB).await, Ok(true));
        if *self.ask_tab.peek() != linked {
            self.ask_tab.set(linked);
        }
    }

    /// Record a refusal of something the person did.
    pub fn refuse(mut self, error: ApiError) {
        if *self.refusal_is_read.peek() {
            self.refusal_is_read.set(false);
        }
        if self.refusal.peek().as_ref() != Some(&error) {
            self.refusal.set(Some(error));
        }
    }

    /// Record a refusal of a background read, unless an action's refusal is
    /// showing (it says more).
    pub fn refuse_read(mut self, error: ApiError) {
        if self.refusal.peek().is_some() && !*self.refusal_is_read.peek() {
            return;
        }
        if !*self.refusal_is_read.peek() {
            self.refusal_is_read.set(true);
        }
        if self.refusal.peek().as_ref() != Some(&error) {
            self.refusal.set(Some(error));
        }
    }

    /// A background read answered: a read's refusal no longer holds; an
    /// action's stays.
    pub fn clear_read(self) {
        if *self.refusal_is_read.peek() {
            self.clear();
        }
    }

    /// An answer came back ok: the last refusal no longer describes the page.
    pub fn clear(mut self) {
        if self.refusal.peek().is_some() {
            self.refusal.set(None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_layout_tab_follows_the_hosts_answer_and_survives_a_failed_fetch() {
        use LayoutAnswer::*;
        assert!(layout_tab(false, Tree(true)));
        assert!(!layout_tab(true, Tree(false)), "only placeholders: no page");
        assert!(!layout_tab(true, NotLinked));
        assert!(layout_tab(true, Failed), "a shown tab stays shown");
        assert!(!layout_tab(false, Failed), "a hidden tab stays hidden");
        assert_eq!(
            LayoutAnswer::of_tree(&Err(ApiError::new("transport", "down"))),
            Failed
        );
    }
}
